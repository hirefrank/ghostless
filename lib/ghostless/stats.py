"""Weighted voice statistics.

Three pools, because not every source is allowed to move every number:

- rhythm: sentence length, sentence-length distribution, contractions,
  first-person I, and And/But/So openers. Spoken lines count here.
- diction: em dashes, questions, signature phrases, and the ban list.
  Spoken lines never count here.
- format: paragraph length. Spoken lines never count here. Once the gold
  tier has enough pieces, bronze counts here and nowhere else.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal, Never

from ghostless.text import STOPWORDS, Measured, measure
from ghostless.tells import split_catalog

Tier = Literal["gold", "silver", "bronze"]
Kind = Literal["prose", "spoken"]
Pool = Literal["rhythm", "diction", "format"]

SHORT_MAX = 10
MEDIUM_MAX = 20
PHRASE_LIMIT = 12

DEFAULT_WEIGHTS: dict[Tier, float] = {"gold": 3.0, "silver": 1.0, "bronze": 0.5}


@dataclass(frozen=True)
class Piece:
    path: str
    tier: Tier
    kind: Kind
    section: str | None
    weight: float
    text: str
    origin: str | None = None
    learned: bool = False


def pools_for(piece: Piece, *, bronze_format_only: bool) -> frozenset[Pool]:
    match piece.kind:
        case "spoken":
            return frozenset({"rhythm"})
        case "prose":
            return _prose_pools(piece.tier, bronze_format_only=bronze_format_only)
        case _ as unexpected:
            _never(unexpected)


def _prose_pools(tier: Tier, *, bronze_format_only: bool) -> frozenset[Pool]:
    match tier:
        case "bronze" if bronze_format_only:
            return frozenset({"format"})
        case "gold" | "silver" | "bronze":
            return frozenset({"rhythm", "diction", "format"})
        case _ as unexpected:
            _never(unexpected)


def _never(value: Never) -> Never:
    raise RuntimeError(f"unhandled variant: {value!r}")


def weighted_median(observations: list[tuple[float, float]]) -> float | None:
    """Smallest value at which the cumulative weight reaches half the total."""
    items = [(float(value), float(weight)) for value, weight in observations if weight > 0]
    if not items:
        return None
    items.sort(key=lambda item: item[0])
    total = sum(weight for _, weight in items)
    half = total / 2.0
    covered = 0.0
    for value, weight in items:
        covered += weight
        if covered >= half:
            return value
    return items[-1][0]


def _per_1k(count: float, words: float) -> float | None:
    if words <= 0:
        return None
    return round(1000.0 * count / words, 4)


def _distribution(observations: list[tuple[float, float]]) -> dict[str, float | None]:
    total = sum(weight for _, weight in observations if weight > 0)
    if total <= 0:
        return {"short": None, "medium": None, "long": None}
    buckets = {"short": 0.0, "medium": 0.0, "long": 0.0}
    for length, weight in observations:
        if weight <= 0:
            continue
        if length <= SHORT_MAX:
            buckets["short"] += weight
        elif length <= MEDIUM_MAX:
            buckets["medium"] += weight
        else:
            buckets["long"] += weight
    return {name: round(value / total, 4) for name, value in buckets.items()}


def _signature_phrases(
    sentences: list[tuple[float, tuple[str, ...]]],
) -> list[dict[str, float | int | str]]:
    weighted: dict[str, float] = {}
    raw: dict[str, int] = {}
    for weight, tokens in sentences:
        if weight <= 0:
            continue
        for size in (2, 3):
            if len(tokens) < size:
                continue
            for index in range(len(tokens) - size + 1):
                gram = tokens[index : index + size]
                if not _keep_gram(gram):
                    continue
                phrase = " ".join(gram)
                weighted[phrase] = weighted.get(phrase, 0.0) + weight
                raw[phrase] = raw.get(phrase, 0) + 1
    ranked = [phrase for phrase, count in raw.items() if count >= 2]
    ranked.sort(key=lambda phrase: (-weighted[phrase], -raw[phrase], phrase))
    return [
        {"phrase": phrase, "count": raw[phrase], "weight": round(weighted[phrase], 4)}
        for phrase in ranked[:PHRASE_LIMIT]
    ]


def _keep_gram(gram: tuple[str, ...]) -> bool:
    if gram[0] in STOPWORDS or gram[-1] in STOPWORDS:
        return False
    if all(token in STOPWORDS for token in gram):
        return False
    return all(token.isalpha() and len(token) >= 3 for token in gram)


def _round_median(value: float | None) -> float | None:
    if value is None:
        return None
    if float(value).is_integer():
        return int(value)
    return round(value, 4)


def summarize(pieces: list[Piece], *, bronze_format_only: bool, use_weights: bool) -> dict[str, object]:
    rhythm_words = 0.0
    contractions = 0.0
    i_count = 0.0
    sentence_obs: list[tuple[float, float]] = []
    openers = {"and": 0.0, "but": 0.0, "so": 0.0}
    diction_words = 0.0
    em_dashes = 0.0
    questions = 0.0
    diction_parts: list[str] = []
    phrase_sentences: list[tuple[float, tuple[str, ...]]] = []
    paragraph_obs: list[tuple[float, float]] = []

    for piece in pieces:
        weight = piece.weight if use_weights else 1.0
        if weight <= 0:
            continue
        measured: Measured = measure(piece.text)
        allowed = pools_for(piece, bronze_format_only=bronze_format_only)
        if "rhythm" in allowed:
            rhythm_words += measured.words * weight
            contractions += measured.contractions * weight
            i_count += measured.i_count * weight
            for length in measured.sentence_lengths:
                sentence_obs.append((length, weight))
            for name, count in measured.openers.items():
                openers[name] += count * weight
        if "diction" in allowed:
            diction_words += measured.words * weight
            em_dashes += measured.em_dashes * weight
            questions += measured.questions * weight
            diction_parts.append(piece.text)
            for tokens in measured.sentence_tokens:
                phrase_sentences.append((weight, tokens))
        if "format" in allowed:
            for length in measured.paragraph_lengths:
                paragraph_obs.append((length, weight))

    rhythm_sentences = sum(weight for _, weight in sentence_obs)
    diction_text = "\n".join(diction_parts)
    if diction_text.strip():
        ban_list, tells_in_use = split_catalog(diction_text)
    else:
        ban_list, tells_in_use = [], []

    opener_total = openers["and"] + openers["but"] + openers["so"]
    opener_share = round(opener_total / rhythm_sentences, 4) if rhythm_sentences else None
    opener_rates = {
        name: (round(count / rhythm_sentences, 4) if rhythm_sentences else None)
        for name, count in openers.items()
    }
    metrics = {
        "rhythmWords": round(rhythm_words, 4),
        "rhythmSentences": round(rhythm_sentences, 4),
        "dictionWords": round(diction_words, 4),
        "formatParagraphs": round(sum(weight for _, weight in paragraph_obs), 4),
        "medianSentenceLength": _round_median(weighted_median(sentence_obs)),
        "sentenceLengthDistribution": _distribution(sentence_obs),
        "contractionsPer1k": _per_1k(contractions, rhythm_words),
        "iPer1k": _per_1k(i_count, rhythm_words),
        "emDashesPer1k": _per_1k(em_dashes, diction_words),
        "questionsPer1k": _per_1k(questions, diction_words),
        "medianParagraphLength": _round_median(weighted_median(paragraph_obs)),
        "openerShare": opener_share,
        "openers": opener_rates,
    }
    return {
        "metrics": metrics,
        "signaturePhrases": _signature_phrases(phrase_sentences),
        "banList": ban_list,
        "tellsInUse": tells_in_use,
        "dictionEmpty": not diction_text.strip(),
    }


def _diction_words(summary: dict[str, object]) -> float:
    metrics = summary.get("metrics")
    if not isinstance(metrics, dict):
        return 0.0
    value = metrics.get("dictionWords", 0)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0.0
    return float(value)


def file_rows(pieces: list[Piece]) -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for piece in pieces:
        measured = measure(piece.text)
        rows.append(
            {
                "path": piece.path,
                "tier": piece.tier,
                "kind": piece.kind,
                "section": piece.section,
                "weight": piece.weight,
                "words": measured.words,
                "learned": piece.learned,
                "origin": piece.origin,
            }
        )
    return rows


def count_pieces(pieces: list[Piece]) -> dict[str, object]:
    prose = {"gold": 0, "silver": 0, "bronze": 0}
    spoken = {"gold": 0, "silver": 0, "bronze": 0}
    learned = 0
    for piece in pieces:
        if piece.learned:
            learned += 1
        match piece.kind:
            case "spoken":
                spoken[piece.tier] += 1
            case "prose":
                prose[piece.tier] += 1
            case _ as unexpected:
                _never(unexpected)
    return {"prose": prose, "spoken": spoken, "learned": learned, "total": len(pieces)}


def compile_stats(
    pieces: list[Piece],
    *,
    weights: dict[str, float],
    gold_voice_minimum: int,
    edits: dict[str, object] | None = None,
) -> dict[str, object]:
    gold_pieces = sum(1 for piece in pieces if piece.tier == "gold")
    format_only = gold_pieces >= gold_voice_minimum
    weighted = summarize(pieces, bronze_format_only=format_only, use_weights=True)
    unweighted = summarize(pieces, bronze_format_only=format_only, use_weights=False)

    by_tier: dict[str, object] = {}
    for tier in ("gold", "silver", "bronze"):
        subset = [piece for piece in pieces if piece.tier == tier and piece.kind == "prose"]
        by_tier[tier] = summarize(subset, bronze_format_only=False, use_weights=False)
    spoken = [piece for piece in pieces if piece.kind == "spoken"]
    by_tier["spoken"] = summarize(spoken, bronze_format_only=False, use_weights=False)

    sections: dict[str, object] = {}
    names = sorted({piece.section for piece in pieces if piece.section})
    for name in names:
        subset = [piece for piece in pieces if piece.section == name]
        sections[name] = {
            "pieces": len(subset),
            "weighted": summarize(subset, bronze_format_only=format_only, use_weights=True),
            "unweighted": summarize(subset, bronze_format_only=format_only, use_weights=False),
        }

    warnings: list[str] = []
    if not pieces:
        warnings.append("No sources yet. Add markdown files under corpus/ and run build again.")
    elif weighted["dictionEmpty"]:
        warnings.append(
            "No written prose in the voice pool, so the ban list was left empty. "
            "Spoken lines do not decide bans."
        )
    diction_words = _diction_words(weighted)
    if 0 < diction_words < 200:
        warnings.append(
            "The written sample is under 200 words. The ban list is strict: "
            "any catalog tell you have not used yet is banned."
        )

    return {
        "schemaVersion": 1,
        "weights": weights,
        "goldVoiceMinimum": gold_voice_minimum,
        "goldPieces": gold_pieces,
        "bronzeFormatOnly": format_only,
        "counts": count_pieces(pieces),
        "files": file_rows(pieces),
        "weighted": weighted,
        "unweighted": unweighted,
        "byTier": by_tier,
        "sections": sections,
        "warnings": warnings,
        "edits": edits or {"learns": [], "repeatedRemovals": {}, "repeatedAdditions": {}},
    }
