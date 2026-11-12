"""Score a draft against voice targets."""

from __future__ import annotations

import re

from ghostless.paths import ProjectError
from ghostless.stats import Piece, summarize
from ghostless.tells import ban_hits, structural_tells

# (relative slack, absolute slack). A miss is outside target ± max(abs, |target| * rel).
TOLERANCES: dict[str, tuple[float, float]] = {
    "medianSentenceLength": (0.35, 4.0),
    "contractionsPer1k": (0.50, 8.0),
    "iPer1k": (0.50, 8.0),
    "questionsPer1k": (0.60, 3.0),
    "openerShare": (0.50, 0.12),
    "medianParagraphLength": (0.45, 20.0),
}
BOTH_DIRECTIONS = (
    "medianSentenceLength",
    "contractionsPer1k",
    "iPer1k",
    "questionsPer1k",
    "openerShare",
    "medianParagraphLength",
)
DISTRIBUTION_ABS = 0.25
DISTRIBUTION_MIN_SENTENCES = 8
EM_DASH_ABS = 1.5
# Metrics a manifest `overrides` entry may pin: the two-direction metrics
# plus em dashes, which has its own comparison.
OVERRIDABLE_METRICS = BOTH_DIRECTIONS + ("emDashesPer1k",)


def apply_overrides(
    targets: dict[str, object], overrides: dict[str, object] | None
) -> dict[str, object]:
    """Return `targets` with manifest override values pinned on top.

    An override replaces the measured target (e.g. `emDashesPer1k: 0` bans
    em dashes outright). Unknown keys and non-numbers are ignored here;
    `load_manifest` is the place that rejects them.
    """
    if not overrides:
        return dict(targets)
    pinned = dict(targets)
    for key, value in overrides.items():
        if key in OVERRIDABLE_METRICS and isinstance(value, (int, float)) and not isinstance(value, bool):
            pinned[key] = value
    return pinned
PLACEHOLDER_RE = re.compile(r"\[WRITER:[^\]]*\]", re.IGNORECASE)

# Sample-size thresholds for the confidence block. 1,500 words matches the
# provisional-ban-list warning build already writes. Below it, a score is a
# smoke test; below the high-water marks it is usable but noisy.
CONFIDENCE_MIN_WORDS = 1500
CONFIDENCE_MIN_PIECES = 3
CONFIDENCE_HIGH_WORDS = 5000
CONFIDENCE_HIGH_PIECES = 10

METRIC_LABELS = {
    "medianSentenceLength": "median sentence length",
    "contractionsPer1k": "contractions per 1k",
    "iPer1k": "first-person I per 1k",
    "questionsPer1k": "questions per 1k",
    "openerShare": "And/But/So opener share",
    "medianParagraphLength": "median paragraph length",
    "emDashesPer1k": "em dashes per 1k",
}


def draft_summary(text: str) -> dict[str, object]:
    piece = Piece(
        path="draft",
        tier="gold",
        kind="prose",
        section=None,
        weight=1.0,
        text=text,
    )
    return summarize([piece], bronze_format_only=False, use_weights=False)


def judge_text(
    text: str,
    stats: dict[str, object],
    *,
    section: str | None = None,
    fail_under: int = 70,
    overrides: dict[str, object] | None = None,
) -> dict[str, object]:
    targets, target_label = _targets(stats, section)
    target_metrics = apply_overrides(_metrics(targets), overrides)
    confidence = corpus_confidence(stats, section)
    if not text.strip():
        return {
            "ok": False,
            "score": 0,
            "failUnder": fail_under,
            "section": target_label,
            "checks": [],
            "banned": [],
            "structure": [],
            "placeholders": [],
            "draft": {},
            "targets": target_metrics,
            "confidence": confidence,
            "lowConfidence": not confidence["trustworthy"],
            "error": "Draft is empty.",
        }
    if not _has_comparable_targets(target_metrics):
        return {
            "ok": False,
            "score": 0,
            "failUnder": fail_under,
            "section": target_label,
            "checks": [],
            "banned": [],
            "structure": [],
            "placeholders": [],
            "draft": {},
            "targets": target_metrics,
            "confidence": confidence,
            "lowConfidence": not confidence["trustworthy"],
            "error": (
                "No usable voice data yet. Add writing to corpus/ (or transcripts/) "
                "and run `ghostless build`, then judge again."
            ),
        }
    draft = draft_summary(text)
    draft_metrics = _metrics(draft)
    ban_list = _string_list(_block(stats.get("weighted")).get("banList"))
    banned = [{"tell": tell, "count": count} for tell, count in ban_hits(text, ban_list)]
    structure = [{"kind": hit.kind, "excerpt": hit.excerpt} for hit in structural_tells(text)]
    placeholders = [match.group(0) for match in PLACEHOLDER_RE.finditer(text)]

    checks: list[dict[str, object]] = []
    penalty = 0
    for key in BOTH_DIRECTIONS:
        result = _compare(key, target_metrics.get(key), draft_metrics.get(key))
        if result is None:
            continue
        checks.append(result)
        penalty += int(result["penalty"])

    distribution_checks, distribution_penalty = _compare_distribution(
        target_metrics.get("sentenceLengthDistribution"),
        draft_metrics.get("sentenceLengthDistribution"),
        draft_metrics.get("rhythmSentences"),
    )
    checks.extend(distribution_checks)
    penalty += distribution_penalty

    em_check = _compare_em_dash(target_metrics.get("emDashesPer1k"), draft_metrics.get("emDashesPer1k"))
    if em_check is not None:
        checks.append(em_check)
        if em_check["status"] in ("high", "low"):
            structure.append(
                {
                    "kind": "em-dash",
                    "excerpt": f"{em_check['actual']} per 1k vs target {em_check['target']}",
                }
            )
            penalty += int(em_check["penalty"])

    ban_penalty = min(40, 10 * sum(int(item["count"]) for item in banned))
    structure_penalty = min(24, 8 * sum(1 for item in structure if item["kind"] != "em-dash"))
    penalty += ban_penalty + structure_penalty
    score = max(0, 100 - penalty)
    failed = score < fail_under
    return {
        "ok": not failed,
        "score": score,
        "failUnder": fail_under,
        "reviewRequired": bool(banned or structure or placeholders),
        "assessment": "mechanical-only",
        "section": target_label,
        "checks": checks,
        "banned": banned,
        "structure": structure,
        "placeholders": placeholders,
        "draft": draft_metrics,
        "targets": target_metrics,
        "confidence": confidence,
        "lowConfidence": not confidence["trustworthy"],
    }


def format_report(report: dict[str, object]) -> str:
    status = "pass" if report.get("ok") else "fail"
    lines = [
        f"ghostless mechanical check: {report.get('score')}/100 ({status})",
        f"section: {report.get('section')}",
    ]
    error = report.get("error")
    if isinstance(error, str) and error:
        lines.append(error)
        lines.append("")
        return "\n".join(lines)
    confidence = report.get("confidence")
    if isinstance(confidence, dict):
        lines.append(
            f"corpus: {confidence.get('pieces')} pieces, "
            f"{confidence.get('words')} measured words "
            f"({confidence.get('writtenWords')} written), "
            f"{confidence.get('sentences')} sentences"
        )
        note = confidence.get("note")
        level = confidence.get("level")
        if isinstance(note, str) and note:
            lines.append(f"confidence: {level} — {note}")
        else:
            lines.append(f"confidence: {level}")
    lines.append("This checks measured patterns; it does not verify facts or whether the draft sounds like the writer.")
    if report.get("reviewRequired"):
        lines.append("Review flagged patterns and placeholders before publishing.")
    lines.extend(["", "Targets"])
    checks = report.get("checks")
    if isinstance(checks, list) and checks:
        for check in checks:
            if not isinstance(check, dict):
                continue
            label = check.get("label")
            actual = check.get("actual")
            target = check.get("target")
            mark = check.get("status")
            lines.append(f"  {label}: {actual}  target {target}  {mark}")
    else:
        lines.append("  (no comparable targets)")
    lines.append("")
    lines.append("Banned words")
    banned = report.get("banned")
    if isinstance(banned, list) and banned:
        for item in banned:
            if isinstance(item, dict):
                lines.append(f"  {item.get('tell')} ({item.get('count')})")
    else:
        lines.append("  none")
    lines.append("")
    lines.append("Structure")
    structure = report.get("structure")
    if isinstance(structure, list) and structure:
        for item in structure:
            if isinstance(item, dict):
                lines.append(f"  {item.get('kind')}: {item.get('excerpt')}")
    else:
        lines.append("  none")
    placeholders = report.get("placeholders")
    if isinstance(placeholders, list) and placeholders:
        lines.append("")
        lines.append(f"Placeholders left for the writer: {len(placeholders)}")
    lines.append("")
    return "\n".join(lines)


def corpus_confidence(stats: dict[str, object], section: str | None = None) -> dict[str, object]:
    """How much corpus is behind a score, and whether it is enough to trust.

    Counts are unweighted so they describe the real sample rather than the
    weighted target. `words` is everything measured for rhythm (written prose
    plus spoken lines); `writtenWords` is the prose pool that sets the ban list
    and diction targets. A section reports that section's pieces and metrics.
    """
    block = _unweighted_block(stats, section)
    metrics = _metrics(block)
    words = _count(metrics.get("rhythmWords"))
    written = _count(metrics.get("dictionWords"))
    sentences = _count(metrics.get("rhythmSentences"))
    pieces = _piece_count(stats, section)
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


def _unweighted_block(stats: dict[str, object], section: str | None) -> dict[str, object]:
    if section:
        sections = stats.get("sections")
        if isinstance(sections, dict):
            entry = sections.get(section)
            if isinstance(entry, dict):
                block = entry.get("unweighted")
                if isinstance(block, dict) and _metrics(block):
                    return block
                weighted = entry.get("weighted")
                if isinstance(weighted, dict) and _metrics(weighted):
                    return weighted
    block = stats.get("unweighted")
    if isinstance(block, dict) and _metrics(block):
        return block
    return _block(stats.get("weighted"))


def _piece_count(stats: dict[str, object], section: str | None) -> int:
    if section:
        sections = stats.get("sections")
        if isinstance(sections, dict):
            entry = sections.get(section)
            if isinstance(entry, dict):
                value = entry.get("pieces")
                if not isinstance(value, bool) and isinstance(value, (int, float)):
                    return int(value)
    counts = stats.get("counts")
    if isinstance(counts, dict):
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
    return int(round(float(value)))


def _targets(stats: dict[str, object], section: str | None) -> tuple[dict[str, object], str]:
    if not section:
        weighted = stats.get("weighted")
        if not isinstance(weighted, dict):
            return {}, "all"
        return weighted, "all"
    sections = stats.get("sections")
    if not isinstance(sections, dict) or section not in sections:
        known = ", ".join(sorted(sections)) if isinstance(sections, dict) else ""
        extra = f" Known sections: {known}." if known else ""
        raise ProjectError(f"No targets for section {section!r}.{extra}")
    block = sections[section]
    if not isinstance(block, dict):
        raise ProjectError(f"No targets for section {section!r}.")
    weighted = block.get("weighted")
    if not isinstance(weighted, dict):
        raise ProjectError(f"No targets for section {section!r}.")
    return weighted, section


def _compare(key: str, target: object, actual: object) -> dict[str, object] | None:
    if isinstance(target, bool) or isinstance(actual, bool):
        return None
    if not isinstance(target, (int, float)) or not isinstance(actual, (int, float)):
        return None
    rel, abs_slack = TOLERANCES[key]
    slack = max(abs_slack, abs(float(target)) * rel)
    low = max(0.0, float(target) - slack)
    high = float(target) + slack
    if key == "openerShare":
        high = min(1.0, high)
    status = "ok"
    if actual < low:
        status = "low"
    elif actual > high:
        status = "high"
    penalty = 0 if status == "ok" else _distance_penalty(float(actual), low, high)
    return {
        "metric": key,
        "label": METRIC_LABELS[key],
        "target": _clean(float(target)),
        "actual": _clean(float(actual)),
        "low": round(low, 4),
        "high": round(high, 4),
        "status": status,
        "penalty": penalty,
    }


def _compare_em_dash(target: object, actual: object) -> dict[str, object] | None:
    if isinstance(target, bool) or isinstance(actual, bool):
        return None
    if not isinstance(target, (int, float)) or not isinstance(actual, (int, float)):
        return None
    slack = max(EM_DASH_ABS, abs(float(target)) * 0.5)
    if float(target) == 0:
        # A target of 0 bans em dashes outright (e.g. an override pinning
        # emDashesPer1k to 0): no slack, so any use reads high.
        slack = 0.0
    low = max(0.0, float(target) - slack)
    high = float(target) + slack
    status = "ok"
    if float(actual) < low:
        status = "low"
    elif float(actual) > high:
        status = "high"
    penalty = 0 if status == "ok" else _distance_penalty(float(actual), low, high)
    return {
        "metric": "emDashesPer1k",
        "label": METRIC_LABELS["emDashesPer1k"],
        "target": _clean(float(target)),
        "actual": _clean(float(actual)),
        "low": round(low, 4),
        "high": round(high, 4),
        "status": status,
        "penalty": penalty,
    }


def _compare_distribution(
    target: object,
    actual: object,
    sentences: object,
) -> tuple[list[dict[str, object]], int]:
    if isinstance(sentences, bool) or not isinstance(sentences, (int, float)):
        return [], 0
    if sentences < DISTRIBUTION_MIN_SENTENCES:
        return [], 0
    if not isinstance(target, dict) or not isinstance(actual, dict):
        return [], 0
    checks: list[dict[str, object]] = []
    penalty = 0
    for name in ("short", "medium", "long"):
        target_value = target.get(name)
        actual_value = actual.get(name)
        if isinstance(target_value, bool) or isinstance(actual_value, bool):
            continue
        if not isinstance(target_value, (int, float)) or not isinstance(actual_value, (int, float)):
            continue
        delta = abs(float(actual_value) - float(target_value))
        status = "ok" if delta <= DISTRIBUTION_ABS else "off"
        point_penalty = 0 if status == "ok" else 8
        penalty += point_penalty
        checks.append(
            {
                "metric": f"distribution.{name}",
                "label": f"sentence share {name}",
                "target": _clean(float(target_value)),
                "actual": _clean(float(actual_value)),
                "low": round(max(0.0, float(target_value) - DISTRIBUTION_ABS), 4),
                "high": round(min(1.0, float(target_value) + DISTRIBUTION_ABS), 4),
                "status": status,
                "penalty": point_penalty,
            }
        )
    return checks, penalty


def _distance_penalty(actual: float, low: float, high: float) -> int:
    if actual < low:
        gap = low - actual
        span = max(low, 1.0)
    else:
        gap = actual - high
        span = max(high, 1.0)
    return min(12, round(4 + (gap / span) * 8))


def _clean(value: float) -> float | int:
    if value.is_integer():
        return int(value)
    return round(value, 4)


def _block(value: object) -> dict[str, object]:
    return value if isinstance(value, dict) else {}


def _metrics(block: dict[str, object]) -> dict[str, object]:
    metrics = block.get("metrics")
    return metrics if isinstance(metrics, dict) else {}


def _has_comparable_targets(target_metrics: dict[str, object]) -> bool:
    return any(
        isinstance(target_metrics.get(key), (int, float))
        and not isinstance(target_metrics.get(key), bool)
        for key in BOTH_DIRECTIONS
    )


def _string_list(value: object) -> list[str]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str)]
