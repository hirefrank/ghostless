"""Weighted voice statistics and corpus confidence thresholds.

Three pools, because not every source is allowed to move every number:

- rhythm: sentence length, sentence-length distribution, contractions,
  first-person I, and And/But/So openers. Spoken lines count here.
- diction: em dashes, questions, signature phrases, and the ban list.
  Spoken lines never count here.
- format: paragraph length. Spoken lines never count here. Once the gold
  tier has enough pieces, bronze counts here and nowhere else.

AI-first finals get their own `edited` tier. The final is mostly model prose,
so it never votes on rhythm or diction, and its paragraph-length weight is
capped at a share of the writer's own prose. What the writer changed is still
kept as edit evidence in `gold/<slug>/meta.json`; see `learn.py`.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, replace
from typing import Never, cast

from ghostless.schema import (
    OVERRIDABLE_METRICS,
    CompiledStats,
    ConfidenceLevel,
    CorpusConfidence,
    Distribution,
    EditedPoolInfo,
    EditSummary,
    FileRow,
    Kind,
    MetricBlock,
    MetricMap,
    Metrics,
    OpenerRates,
    PieceCounts,
    Pool,
    ProvenanceCounts,
    SectionBlock,
    SignaturePhrase,
    Tier,
    TierCounts,
)
from ghostless.tells import split_catalog
from ghostless.text import STOPWORDS, Measured, is_contraction, measure, strip_non_prose

SHORT_MAX = 10
MEDIUM_MAX = 20
PHRASE_LIMIT = 12

DEFAULT_WEIGHTS: dict[Tier, float] = {
    "gold": 3.0,
    "silver": 1.0,
    "bronze": 0.5,
    "edited": 0.5,
}

# The edited pool can never carry more than this share of the writer-authored
# prose weight. A pile of lightly edited AI drafts then cannot outvote the
# writer's own corpus on paragraph length, the only pool it touches.
EDITED_MAX_SHARE = 0.25

# Sample-size thresholds for the confidence block. 1,500 words matches the
# provisional-ban-list warning build already writes. Below it, a score is a
# smoke test; below the high-water marks it is usable but noisy.
CONFIDENCE_MIN_WORDS = 1500
CONFIDENCE_MIN_PIECES = 3
CONFIDENCE_HIGH_WORDS = 5000
CONFIDENCE_HIGH_PIECES = 10


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
    # Provenance is recorded, not measured. `origin` is who wrote the first
    # draft (unaided, writer-first, ai-first); `confirmation` is the writer's
    # claim about the final (none, origin, promoted). They never enter a metric.
    confirmation: str | None = None
    model: str | None = None
    tool: str | None = None


def as_mapping(value: object) -> Mapping[str, object]:
    """Return `value` when it is a string-keyed mapping, else an empty dict."""
    return value if isinstance(value, Mapping) else {}


def metrics_of(block: object) -> Mapping[str, object]:
    """Return the `metrics` sub-mapping from a metric block, else an empty dict."""
    return as_mapping(as_mapping(block).get("metrics"))


def apply_overrides(
    targets: Mapping[str, object],
    overrides: Mapping[str, object] | None,
) -> MetricMap:
    """Return `targets` with manifest override values pinned on top.

    An override replaces the measured target (e.g. `emDashesPer1k: 0` bans
    em dashes outright). Unknown keys and non-numbers are ignored here;
    `load_manifest` is the place that rejects them.
    """
    if not overrides:
        return cast(MetricMap, dict(targets))
    pinned = dict(targets)
    for key, value in overrides.items():
        if key in OVERRIDABLE_METRICS and isinstance(value, (int, float)) and not isinstance(value, bool):
            pinned[key] = value
    return cast(MetricMap, pinned)


def pools_for(piece: Piece, *, bronze_format_only: bool) -> frozenset[Pool]:
    match piece.kind:
        case "spoken":
            if piece.tier == "edited":
                # An edited pair is written prose evidence, never rhythm.
                return frozenset()
            return frozenset({"rhythm"})
        case "prose":
            return _prose_pools(piece.tier, bronze_format_only=bronze_format_only)
        case _ as unexpected:
            _never(unexpected)


def _prose_pools(tier: Tier, *, bronze_format_only: bool) -> frozenset[Pool]:
    match tier:
        case "bronze" if bronze_format_only:
            return frozenset({"format"})
        case "edited":
            # An unconfirmed AI-first final is mostly the model's prose even
            # after the writer edits it. Paragraph shape is the only thing it
            # is allowed to inform, and compile_stats caps that weight.
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


def _distribution(observations: list[tuple[float, float]]) -> Distribution:
    total = sum(weight for _, weight in observations if weight > 0)
    if total <= 0:
        return {"short": None, "medium": None, "long": None}
    short = 0.0
    medium = 0.0
    long = 0.0
    for length, weight in observations:
        if weight <= 0:
            continue
        if length <= SHORT_MAX:
            short += weight
        elif length <= MEDIUM_MAX:
            medium += weight
        else:
            long += weight
    return {
        "short": round(short / total, 4),
        "medium": round(medium / total, 4),
        "long": round(long / total, 4),
    }


def _signature_phrases(
    sentences: list[tuple[int, float, tuple[str, ...]]],
) -> list[SignaturePhrase]:
    weighted: dict[str, float] = {}
    raw: dict[str, int] = {}
    pieces_seen: dict[str, set[int]] = {}
    total_pieces = len({piece_index for piece_index, weight, _ in sentences if weight > 0})
    for piece_index, weight, tokens in sentences:
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
                pieces_seen.setdefault(phrase, set()).add(piece_index)
    if total_pieces >= 2:
        ranked = [phrase for phrase, seen in pieces_seen.items() if len(seen) >= 2]
    else:
        ranked = [phrase for phrase, count in raw.items() if count >= 2]
    ranked.sort(key=lambda phrase: (-weighted[phrase], -raw[phrase], phrase))
    return [
        {"phrase": phrase, "count": raw[phrase], "weight": round(weighted[phrase], 4)}
        for phrase in ranked[:PHRASE_LIMIT]
    ]


def _keep_gram(gram: tuple[str, ...]) -> bool:
    if gram[0] in STOPWORDS or gram[-1] in STOPWORDS:
        return False
    if all(token in STOPWORDS for token in gram):  # token is a word, not a credential
        return False
    return all(
        (token.isalpha() and len(token) >= 3) or token == "i" or is_contraction(token)  # noqa: S105
        for token in gram
    )


def _round_median(value: float | None) -> float | int | None:
    if value is None:
        return None
    if float(value).is_integer():
        return int(value)
    return round(value, 4)


def summarize(pieces: list[Piece], *, bronze_format_only: bool, use_weights: bool) -> MetricBlock:
    rhythm_words = 0.0
    contractions = 0.0
    i_count = 0.0
    sentence_obs: list[tuple[float, float]] = []
    openers = {"and": 0.0, "but": 0.0, "so": 0.0}
    diction_words = 0.0
    em_dashes = 0.0
    questions = 0.0
    diction_parts: list[str] = []
    phrase_sentences: list[tuple[int, float, tuple[str, ...]]] = []
    paragraph_obs: list[tuple[float, float]] = []

    for piece_index, piece in enumerate(pieces):
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
            cleaned_piece = strip_non_prose(piece.text)
            if cleaned_piece.strip():
                diction_parts.append(cleaned_piece)
            for tokens in measured.sentence_tokens:
                phrase_sentences.append((piece_index, weight, tokens))
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
    opener_rates: OpenerRates = {
        "and": round(openers["and"] / rhythm_sentences, 4) if rhythm_sentences else None,
        "but": round(openers["but"] / rhythm_sentences, 4) if rhythm_sentences else None,
        "so": round(openers["so"] / rhythm_sentences, 4) if rhythm_sentences else None,
    }
    metrics: Metrics = {
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


def file_rows(pieces: list[Piece]) -> list[FileRow]:
    rows: list[FileRow] = []
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
                "confirmation": piece.confirmation,
                "model": piece.model,
                "tool": piece.tool,
            }
        )
    return rows


def count_pieces(pieces: list[Piece]) -> PieceCounts:
    prose: dict[Tier, int] = {"gold": 0, "silver": 0, "bronze": 0, "edited": 0}
    spoken: dict[Tier, int] = {"gold": 0, "silver": 0, "bronze": 0, "edited": 0}
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
    prose_counts: TierCounts = {
        "gold": prose["gold"],
        "silver": prose["silver"],
        "bronze": prose["bronze"],
        "edited": prose["edited"],
    }
    spoken_counts: TierCounts = {
        "gold": spoken["gold"],
        "silver": spoken["silver"],
        "bronze": spoken["bronze"],
        "edited": spoken["edited"],
    }
    return {"prose": prose_counts, "spoken": spoken_counts, "learned": learned, "total": len(pieces)}


def _edited_weights(pieces: list[Piece]) -> tuple[float, float, float]:
    """Return (scale, configured weight, counted weight) for the edited pool.

    Only writer-authored prose can cap the pool. With no writer-authored prose
    there is nothing to protect, so the configured weight is used as written.
    """
    writer = sum(piece.weight for piece in pieces if piece.tier != "edited" and piece.kind == "prose")
    edited = sum(piece.weight for piece in pieces if piece.tier == "edited" and piece.kind == "prose")
    if edited <= 0 or writer <= 0:
        return 1.0, edited, edited
    allowed = EDITED_MAX_SHARE * writer
    if edited <= allowed:
        return 1.0, edited, edited
    return allowed / edited, edited, allowed


def _provenance_counts(pieces: list[Piece]) -> ProvenanceCounts:
    counts = {"unaided": 0, "writer-first": 0, "ai-first": 0, "unknown": 0}
    for piece in pieces:
        match piece.origin:
            case "unaided":
                counts["unaided"] += 1
            case "writer-first":
                counts["writer-first"] += 1
            case "ai-first":
                counts["ai-first"] += 1
            case _:
                counts["unknown"] += 1
    return {
        "unaided": counts["unaided"],
        "writer-first": counts["writer-first"],
        "ai-first": counts["ai-first"],
        "unknown": counts["unknown"],
    }


def compile_stats(
    pieces: list[Piece],
    *,
    weights: Mapping[Tier, float] | Mapping[str, float],
    gold_voice_minimum: int,
    edits: EditSummary | None = None,
) -> CompiledStats:
    # Bronze drops out of voice once enough *written* gold exists. Spoken
    # gold (dictations) contributes no diction or format, so it must not
    # count toward the threshold: ten gold transcripts would otherwise push
    # the only written prose out of the voice pool. Edited AI-first finals do
    # not count either; they are model prose until the writer promotes them.
    gold_pieces = sum(1 for piece in pieces if piece.tier == "gold" and piece.kind == "prose")
    format_only = gold_pieces >= gold_voice_minimum
    scale, edited_configured, edited_counted = _edited_weights(pieces)
    counted = (
        [replace(piece, weight=piece.weight * scale) if piece.tier == "edited" else piece for piece in pieces]
        if scale != 1.0
        else pieces
    )
    weighted = summarize(counted, bronze_format_only=format_only, use_weights=True)
    unweighted = summarize(counted, bronze_format_only=format_only, use_weights=False)

    by_tier: dict[str, MetricBlock] = {}
    tiers: tuple[Tier, ...] = ("gold", "silver", "bronze", "edited")
    for tier in tiers:
        subset = [piece for piece in counted if piece.tier == tier and piece.kind == "prose"]
        by_tier[tier] = summarize(subset, bronze_format_only=False, use_weights=False)
    spoken = [piece for piece in counted if piece.kind == "spoken"]
    by_tier["spoken"] = summarize(spoken, bronze_format_only=False, use_weights=False)

    sections: dict[str, SectionBlock] = {}
    names = sorted({piece.section for piece in pieces if piece.section})
    for name in names:
        subset = [piece for piece in counted if piece.section == name]
        if not any(piece.tier != "edited" for piece in subset):
            # A section made only of AI-first finals has no writer-authored
            # voice data, so it is not offered as a judge target.
            continue
        sections[name] = {
            "pieces": len(subset),
            "weighted": summarize(subset, bronze_format_only=format_only, use_weights=True),
            "unweighted": summarize(subset, bronze_format_only=format_only, use_weights=False),
        }

    edited_pieces = sum(1 for piece in pieces if piece.tier == "edited" and piece.kind == "prose")
    warnings: list[str] = []
    if not pieces:
        warnings.append("No sources yet. Add markdown files under corpus/ and run build again.")
    elif weighted["dictionEmpty"]:
        warnings.append(
            "No written prose in the voice pool, so the ban list was left empty. Spoken lines do not decide bans."
        )
    diction_words = weighted["metrics"]["dictionWords"]
    if 0 < diction_words < 1500:
        warnings.append(
            "The written sample is under 1,500 words. The absent-tell list is provisional; "
            "review hits in context rather than treating absence as a firm rule."
        )
    if edited_pieces:
        warnings.append(
            f"{edited_pieces} AI-first final(s) are measured as edited, not gold. They record "
            "what you changed, but they never set rhythm, phrases, or the ban list, and their "
            "paragraph weight is capped. Run `ghostless promote <slug>` to count one as gold."
        )
    if scale != 1.0:
        warnings.append(
            f"Edited paragraph weight was capped at {int(EDITED_MAX_SHARE * 100)}% of your "
            f"writer-authored prose ({round(edited_configured, 4)} configured, "
            f"{round(edited_counted, 4)} counted)."
        )
    provenance = _provenance_counts(pieces)
    unknown_origin = provenance["unknown"]
    if unknown_origin:
        warnings.append(
            f"{unknown_origin} piece(s) have no recorded origin. A learned pair without one "
            "is measured as edited, not gold. Add `provenance.origin` to its meta.json to "
            "record where the first draft came from."
        )

    edited_info: EditedPoolInfo = {
        "pieces": edited_pieces,
        "configuredWeight": round(edited_configured, 4),
        "countedWeight": round(edited_counted, 4),
        "maxShare": EDITED_MAX_SHARE,
    }
    empty_edits: EditSummary = {"learns": [], "repeatedRemovals": {}, "repeatedAdditions": {}}
    return {
        "schemaVersion": 1,
        "weights": {str(key): float(value) for key, value in weights.items()},
        "goldVoiceMinimum": gold_voice_minimum,
        "goldPieces": gold_pieces,
        "bronzeFormatOnly": format_only,
        "edited": edited_info,
        "counts": count_pieces(counted),
        "provenance": provenance,
        "files": file_rows(counted),
        "weighted": weighted,
        "unweighted": unweighted,
        "byTier": by_tier,
        "sections": sections,
        "warnings": warnings,
        "edits": edits if edits is not None else empty_edits,
    }


def corpus_confidence(stats: Mapping[str, object], section: str | None = None) -> CorpusConfidence:
    """How much corpus is behind a score, and whether it is enough to trust.

    Counts are unweighted so they describe the real sample rather than the
    weighted target. `words` is everything measured for rhythm (written prose
    plus spoken lines); `writtenWords` is the prose pool that sets the ban list
    and diction targets. A section reports that section's pieces and metrics.
    """
    block = _unweighted_block(stats, section)
    metrics = metrics_of(block)
    words = _count(metrics.get("rhythmWords"))
    written = _count(metrics.get("dictionWords"))
    sentences = _count(metrics.get("rhythmSentences"))
    pieces = _piece_count(stats, section)
    level: ConfidenceLevel
    if written <= 0:
        level = "low"
        note = (
            "No written prose in the sample. Only rhythm targets can be compared; "
            "add prose to corpus/ for a full score."
        )
    elif words < CONFIDENCE_MIN_WORDS or pieces < CONFIDENCE_MIN_PIECES:
        level = "low"
        note = (
            f"Small sample: {words} measured words in {pieces} piece(s). "
            f"Below {CONFIDENCE_MIN_WORDS} words or {CONFIDENCE_MIN_PIECES} pieces, "
            "treat the score as a smoke test, not a verdict."
        )
    elif words < CONFIDENCE_HIGH_WORDS or pieces < CONFIDENCE_HIGH_PIECES:
        level = "medium"
        note = (
            f"Moderate sample: {words} measured words in {pieces} pieces. "
            "More gold writing would make the score steadier."
        )
    else:
        level = "high"
        note = ""
    return {
        "level": level,
        "trustworthy": level != "low",
        "pieces": pieces,
        "words": words,
        "writtenWords": written,
        "sentences": sentences,
        "minimums": {"words": CONFIDENCE_MIN_WORDS, "pieces": CONFIDENCE_MIN_PIECES},
        "note": note,
    }


def _unweighted_block(stats: Mapping[str, object], section: str | None) -> Mapping[str, object]:
    if section:
        sections = as_mapping(stats.get("sections"))
        entry = as_mapping(sections.get(section))
        if entry:
            block = as_mapping(entry.get("unweighted"))
            if block and metrics_of(block):
                return block
            weighted = as_mapping(entry.get("weighted"))
            if weighted and metrics_of(weighted):
                return weighted
    block = as_mapping(stats.get("unweighted"))
    if block and metrics_of(block):
        return block
    return as_mapping(stats.get("weighted"))


def _piece_count(stats: Mapping[str, object], section: str | None) -> int:
    if section:
        sections = as_mapping(stats.get("sections"))
        entry = as_mapping(sections.get(section))
        if entry:
            value = entry.get("pieces")
            if not isinstance(value, bool) and isinstance(value, (int, float)):
                return int(value)
    counts = as_mapping(stats.get("counts"))
    if counts:
        total = counts.get("total")
        if not isinstance(total, bool) and isinstance(total, (int, float)):
            return int(total)
    files = stats.get("files")
    if isinstance(files, list):
        return len(files)
    return 0


def _count(value: object) -> int:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0
    return round(float(value))
