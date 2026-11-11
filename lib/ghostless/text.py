"""Mechanical text measures. No model, no network."""

from __future__ import annotations

import re
from dataclasses import dataclass

WORD_RE = re.compile(r"[A-Za-z]+(?:'[A-Za-z]+)?|\d+(?:\.\d+)?")
SENTENCE_END = re.compile(r"(?<=[.!?])\s+")
CODE_FENCE = re.compile(r"```.*?```", re.DOTALL)
FRONTMATTER = re.compile(r"\A---\n(.*?)\n---\n?", re.DOTALL)
ABBREV_RE = re.compile(
    r"(?i)\b(?:mr|mrs|ms|dr|prof|sr|jr|st|vs|etc|e\.g|i\.e|u\.s|u\.k)\."
)
DECIMAL_RE = re.compile(r"(?<=\d)\.(?=\d)")
CONTRACTION_RE = re.compile(
    r"(?i)^(?:[a-z]+n't|[a-z]+'(?:re|ve|ll|d|m)|(?:it|that|there|here|what|who|let)'s)$"
)
I_TOKENS = frozenset({"i", "i'm", "i've", "i'd", "i'll"})
OPENERS = ("and", "but", "so")

# Function words dropped from signature phrases and from edit-word lists.
# Contractions and the pronoun "I" are intentionally absent: those are voice.
STOPWORDS = frozenset(
    {
        "a",
        "an",
        "the",
        "and",
        "but",
        "so",
        "or",
        "nor",
        "of",
        "to",
        "in",
        "on",
        "for",
        "with",
        "at",
        "by",
        "from",
        "as",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "it",
        "its",
        "this",
        "that",
        "these",
        "those",
        "you",
        "we",
        "they",
        "he",
        "she",
        "them",
        "his",
        "her",
        "my",
        "our",
        "your",
        "their",
        "me",
        "him",
        "us",
        "not",
        "no",
        "yes",
        "do",
        "did",
        "does",
        "have",
        "has",
        "had",
        "if",
        "then",
        "than",
        "too",
        "very",
        "just",
        "about",
        "into",
        "over",
        "after",
        "before",
        "out",
        "up",
        "down",
        "what",
        "when",
        "where",
        "who",
        "how",
        "why",
        "which",
        "while",
        "because",
        "can",
        "could",
        "would",
        "should",
        "will",
        "of",
        "that",
        "this",
        "there",
        "here",
        "also",
        "only",
        "still",
        "such",
        "own",
        "same",
        "other",
    }
)

_PLACEHOLDER = "\u0000"


@dataclass(frozen=True)
class Measured:
    words: int
    sentences: int
    sentence_lengths: tuple[int, ...]
    sentence_tokens: tuple[tuple[str, ...], ...]
    contractions: int
    i_count: int
    em_dashes: int
    questions: int
    paragraph_lengths: tuple[int, ...]
    openers: dict[str, int]


def normalize(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = text.replace("\u2019", "'").replace("\u2018", "'")
    text = text.replace("\u201c", '"').replace("\u201d", '"')
    text = CODE_FENCE.sub("", text)
    return text


def split_frontmatter(text: str) -> tuple[dict[str, str], str]:
    """Return simple `key: value` frontmatter and the body.

    A missing or unclosed fence leaves the whole text as the body. Values
    stay strings; callers validate the ones they understand.
    """
    normalized = text.replace("\r\n", "\n").replace("\r", "\n")
    match = FRONTMATTER.match(normalized)
    if not match:
        return {}, normalized
    meta: dict[str, str] = {}
    for line in match.group(1).splitlines():
        if not line.strip() or ":" not in line:
            continue
        key, value = line.split(":", 1)
        key = key.strip().lower()
        value = value.strip().strip('"').strip("'")
        if key:
            meta[key] = value
    return meta, normalized[match.end() :]


def words(text: str) -> list[str]:
    return WORD_RE.findall(normalize(text))


def word_tokens(text: str) -> list[str]:
    return [token.casefold() for token in words(text)]


def is_contraction(token: str) -> bool:
    return CONTRACTION_RE.match(token.casefold()) is not None


def count_em_dashes(text: str) -> int:
    normalized = normalize(text)
    spaced_double = len(re.findall(r"\s--\s", normalized))
    spaced_en = len(re.findall(r"\s\u2013\s", normalized))
    return normalized.count("\u2014") + spaced_double + spaced_en


def paragraphs(text: str) -> list[str]:
    blocks: list[str] = []
    for block in re.split(r"\n\s*\n", normalize(text)):
        stripped = block.strip()
        if not stripped:
            continue
        lines = [line.strip() for line in stripped.splitlines() if line.strip()]
        if lines and all(line.startswith("#") for line in lines):
            continue
        blocks.append(stripped)
    return blocks


def split_sentences(text: str) -> list[str]:
    protected = ABBREV_RE.sub(lambda match: match.group(0).replace(".", _PLACEHOLDER), text)
    protected = DECIMAL_RE.sub(_PLACEHOLDER, protected)
    parts = SENTENCE_END.split(protected.strip())
    sentences: list[str] = []
    for part in parts:
        sentence = part.replace(_PLACEHOLDER, ".").strip()
        if sentence:
            sentences.append(sentence)
    return sentences


def first_word(sentence: str) -> str:
    stripped = sentence.lstrip(" \t\"'`“”‘’([{<-–—*• ")
    match = re.match(r"[A-Za-z]+", stripped)
    if not match:
        return ""
    return match.group(0).casefold()


def track_token(token: str) -> bool:
    """Words worth remembering when a writer adds or cuts them."""
    folded = token.casefold()
    if folded == "i" or is_contraction(folded):
        return True
    if folded in STOPWORDS or len(folded) < 3:
        return False
    return folded.isalpha()


def measure(text: str) -> Measured:
    body = normalize(text).strip()
    if not body:
        return Measured(0, 0, (), (), 0, 0, 0, 0, (), {name: 0 for name in OPENERS})

    sentence_lengths: list[int] = []
    sentence_tokens: list[tuple[str, ...]] = []
    contractions = 0
    i_count = 0
    questions = 0
    openers = {name: 0 for name in OPENERS}
    word_total = 0
    paragraph_lengths: list[int] = []

    blocks = paragraphs(body)
    if not blocks:
        blocks = [body]

    for block in blocks:
        block_words = words(block)
        paragraph_lengths.append(len(block_words))
        for sentence in split_sentences(block):
            tokens = words(sentence)
            folded = [token.casefold() for token in tokens]
            sentence_lengths.append(len(tokens))
            sentence_tokens.append(tuple(folded))
            word_total += len(tokens)
            contractions += sum(1 for token in folded if is_contraction(token))
            i_count += sum(1 for token in folded if token in I_TOKENS)
            if sentence.rstrip().endswith("?"):
                questions += 1
            opener = first_word(sentence)
            if opener in openers:
                openers[opener] += 1

    return Measured(
        words=word_total,
        sentences=len(sentence_lengths),
        sentence_lengths=tuple(sentence_lengths),
        sentence_tokens=tuple(sentence_tokens),
        contractions=contractions,
        i_count=i_count,
        em_dashes=count_em_dashes(body),
        questions=questions,
        paragraph_lengths=tuple(paragraph_lengths),
        openers=openers,
    )
