"""Mechanical text measures. No model, no network."""

from __future__ import annotations

import re
from dataclasses import dataclass

WORD_RE = re.compile(r"[A-Za-z]+(?:'[A-Za-z]+)?|\d+(?:\.\d+)?")
SENTENCE_END = re.compile(r"(?<=[.!?])\s+")
CODE_FENCE = re.compile(r"```.*?```", re.DOTALL)
FRONTMATTER = re.compile(r"\A---\n(.*?)\n---\n?", re.DOTALL)

# Formatting that is not prose. Measurement strips these before counting, so a
# link URL, a code sample, or a table rule cannot move a voice target. Link
# text stays: it is prose the writer wrote. Emphasis marks stay too, because
# the edit receipt treats punctuation and markup changes as structural edits.
FENCE_OPEN = re.compile(r"^[ \t]*(`{3,}|~{3,})")
HTML_COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)
INLINE_CODE = re.compile(r"(?<!`)(`+)(?!`)([^`\n]*?)\1(?!`)")
IMAGE_INLINE = re.compile(r"!\[[^\]]*\]\([^)]*\)")
IMAGE_REF = re.compile(r"!\[[^\]]*\]\[[^\]]*\]")
FOOTNOTE_REF = re.compile(r"\[\^[^\]]*\]")
REF_DEF_LINE = re.compile(r"^[ \t]{0,3}\[[^\]]+\]:[ \t]*\S.*$", re.MULTILINE)
LINK_INLINE = re.compile(r"\[([^\]]+)\]\([^)]*\)")
LINK_REF = re.compile(r"\[([^\]]+)\]\[[^\]]*\]")
AUTOLINK = re.compile(r"<[A-Za-z][A-Za-z0-9+.-]*:[^>]*>")
BARE_URL = re.compile(
    r"\b(?:https?://|mailto:|www\.)[^\s<>\[\]]*[^\s<>\[\].,;:!?]",
    re.IGNORECASE,
)
HTML_TAG = re.compile(r"</?[A-Za-z][^<>]*>")
ATX_HEADING = re.compile(r"^[ \t]{0,3}#{1,6}[ \t]+.*$", re.MULTILINE)
BLOCKQUOTE = re.compile(r"^[ \t]{0,3}>[ \t]?", re.MULTILINE)
LIST_MARKER = re.compile(r"^[ \t]{0,3}(?:[-*+]|\d{1,9}[.)])[ \t]+", re.MULTILINE)
TASK_BOX = re.compile(r"^[ \t]{0,3}\[[ xX]\][ \t]+", re.MULTILINE)
HR_STAR = re.compile(r"^[ \t]*(?:\*[ \t]*){3,}$", re.MULTILINE)
HR_UNDER = re.compile(r"^[ \t]*(?:_[ \t]*){3,}$", re.MULTILINE)
HR_DASH = re.compile(r"^[ \t]*(?:-[ \t]*){3,}$", re.MULTILINE)
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


def strip_non_prose(text: str) -> str:
    """Drop Markdown and HTML scaffolding so measures count prose, not layout.

    Fenced and inline code, link and image syntax, bare URLs, HTML tags,
    frontmatter, heading lines, list markers, blockquote markers, horizontal
    rules, and table rules are removed or unwrapped. Link text is kept because
    the writer wrote it; image alt text is not, because it is a description of
    something outside the prose. Emphasis marks are left alone so `learn` can
    still report an emphasis-only revision as a structural change.

    Idempotent. For prose with no Markdown syntax this is
    `normalize(text)` unchanged, which keeps existing measurements stable.
    """
    text = normalize(text)
    text = FRONTMATTER.sub("", text, count=1)
    text = HTML_COMMENT.sub("", text)
    text = _strip_fences(text)
    text = INLINE_CODE.sub("", text)
    text = IMAGE_INLINE.sub("", text)
    text = IMAGE_REF.sub("", text)
    text = FOOTNOTE_REF.sub("", text)
    text = REF_DEF_LINE.sub("", text)
    # Nested brackets need a second pass: [a [b] c](url).
    for _ in range(2):
        text = LINK_INLINE.sub(r"\1", text)
        text = LINK_REF.sub(r"\1", text)
    text = AUTOLINK.sub("", text)
    text = BARE_URL.sub("", text)
    text = HTML_TAG.sub("", text)
    text = ATX_HEADING.sub("", text)
    text = BLOCKQUOTE.sub("", text)
    text = LIST_MARKER.sub("", text)
    text = TASK_BOX.sub("", text)
    text = HR_STAR.sub("", text)
    text = HR_UNDER.sub("", text)
    text = HR_DASH.sub("", text)
    return _strip_table_lines(text)


def _strip_fences(text: str) -> str:
    """Remove fenced code, including tilde fences and an unclosed last fence."""
    kept: list[str] = []
    fence = ""
    for line in text.split("\n"):
        if fence:
            stripped = line.strip()
            if stripped.startswith(fence) and stripped.strip(fence[0]) == "":
                fence = ""
            continue
        match = FENCE_OPEN.match(line)
        if match:
            fence = match.group(1)
            continue
        kept.append(line)
    return "\n".join(kept)


def _strip_table_lines(text: str) -> str:
    kept: list[str] = []
    for line in text.split("\n"):
        stripped = line.strip()
        if stripped and _is_table_rule(stripped):
            continue
        if stripped.startswith("|") or stripped.endswith("|"):
            line = re.sub(r"[ \t]*\|[ \t]*", "  ", stripped.strip("|"))
        kept.append(line)
    return "\n".join(kept)


def _is_table_rule(stripped: str) -> bool:
    if "|" not in stripped or "-" not in stripped:
        return False
    return all(character in "|-: \t" for character in stripped)


def words(text: str) -> list[str]:
    return WORD_RE.findall(strip_non_prose(text))


def word_tokens(text: str) -> list[str]:
    return [token.casefold() for token in words(text)]


def is_contraction(token: str) -> bool:
    return CONTRACTION_RE.match(token.casefold()) is not None


def count_em_dashes(text: str) -> int:
    normalized = strip_non_prose(text)
    spaced_double = len(re.findall(r"\s--\s", normalized))
    spaced_en = len(re.findall(r"\s\u2013\s", normalized))
    return normalized.count("\u2014") + spaced_double + spaced_en


def paragraphs(text: str) -> list[str]:
    blocks: list[str] = []
    for block in re.split(r"\n\s*\n", strip_non_prose(text)):
        stripped = block.strip()
        if not stripped:
            continue
        lines = [line.strip() for line in stripped.splitlines() if line.strip()]
        if lines and all(line.startswith("#") for line in lines):
            continue
        blocks.append(stripped)
    return blocks


def split_sentences(text: str) -> list[str]:
    text = strip_non_prose(text)
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
    body = strip_non_prose(text).strip()
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
