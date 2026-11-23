"""Score a draft against voice targets."""

from __future__ import annotations

import re
from collections.abc import Mapping
from typing import Literal

from ghostless.paths import ProjectError
from ghostless.schema import (
    BOTH_DIRECTIONS as BOTH_DIRECTIONS,
    METRIC_LABELS as METRIC_LABELS,
    OVERRIDABLE_METRICS as OVERRIDABLE_METRICS,
    BannedHit,
    ConfirmedRuleHit,
    JudgeCheck,
    JudgeReport,
    MetricBlock,
    StructureHitDict,
)
from ghostless.stats import (
    CONFIDENCE_HIGH_PIECES as CONFIDENCE_HIGH_PIECES,
    CONFIDENCE_HIGH_WORDS as CONFIDENCE_HIGH_WORDS,
    CONFIDENCE_MIN_PIECES as CONFIDENCE_MIN_PIECES,
    CONFIDENCE_MIN_WORDS as CONFIDENCE_MIN_WORDS,
    Piece,
    apply_overrides as apply_overrides,
    as_mapping,
    corpus_confidence as corpus_confidence,
    metrics_of,
    summarize,
)
from ghostless.tells import ban_hits, structural_tells
from ghostless.text import strip_code_and_comments, strip_non_prose

# (relative slack, absolute slack). A miss is outside target ± max(abs, |target| * rel).
TOLERANCES: dict[str, tuple[float, float]] = {
    "medianSentenceLength": (0.35, 4.0),
    "contractionsPer1k": (0.50, 8.0),
    "iPer1k": (0.50, 8.0),
    "questionsPer1k": (0.60, 3.0),
    "openerShare": (0.50, 0.12),
    "medianParagraphLength": (0.45, 20.0),
}
DISTRIBUTION_ABS = 0.25
DISTRIBUTION_MIN_SENTENCES = 8
EM_DASH_ABS = 1.5

# Any bracketed uppercase name followed by a colon is an unfilled gap for
# the writer: [WRITER: ...], [FRANK: ...], [TODO: ...]. The token is at
# least two characters and starts with a letter, so a bare citation such as
# [see p. 3] or a year is not a placeholder. A manifest `placeholder` token
# (e.g. "Frank") is matched case-insensitively on top of this, so a writer
# whose token is not all-caps still flags.
PLACEHOLDER_RE = re.compile(r"\[[A-Z][A-Z0-9 _\-]{1,}:[^\]]*\]")


def placeholder_pattern(token: str) -> re.Pattern[str]:
    return re.compile(r"\[" + re.escape(token) + r":[^\]]*\]", re.IGNORECASE)


def find_placeholders(text: str, placeholder: str | None = None) -> list[str]:
    cleaned = strip_code_and_comments(text)
    found = [match.group(0) for match in PLACEHOLDER_RE.finditer(cleaned)]
    if placeholder:
        seen = set(found)
        for match in placeholder_pattern(placeholder).finditer(cleaned):
            if match.group(0) not in seen:
                seen.add(match.group(0))
                found.append(match.group(0))
    return found


def draft_summary(text: str) -> MetricBlock:
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
    stats: Mapping[str, object],
    *,
    section: str | None = None,
    fail_under: int = 70,
    overrides: Mapping[str, object] | None = None,
    placeholder: str | None = None,
) -> JudgeReport:
    targets, target_label = _targets(stats, section)
    raw_overrides = stats.get("overrides")
    effective_overrides = (
        overrides
        if overrides is not None
        else (raw_overrides if isinstance(raw_overrides, Mapping) else None)
    )
    target_metrics = apply_overrides(metrics_of(targets), effective_overrides)
    confidence = corpus_confidence(stats, section)
    if not text.strip():
        return {
            "ok": False,
            "score": 0,
            "failUnder": fail_under,
            "section": target_label,
            "checks": [],
            "banned": [],
            "confirmedRules": [],
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
            "confirmedRules": [],
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
    draft_metrics = metrics_of(draft)
    ban_list = _string_list(as_mapping(stats.get("weighted")).get("banList"))
    cleaned = strip_non_prose(text)
    banned: list[BannedHit] = [
        {"tell": tell, "count": count} for tell, count in ban_hits(cleaned, ban_list)
    ]
    banned_words = {item["tell"].casefold() for item in banned}
    active_cuts = _active_cut_rules(stats, section)
    scope_by_word = {word.casefold(): rule_scope for word, rule_scope in active_cuts}
    remaining_words = [word for word, _ in active_cuts if word.casefold() not in banned_words]
    confirmed_hits: list[ConfirmedRuleHit] = [
        {
            "id": f"cut:{word}",
            "word": word,
            "scope": scope_by_word[word.casefold()],
            "count": count,
        }
        for word, count in ban_hits(cleaned, remaining_words)
    ]
    structure: list[StructureHitDict] = [
        {"kind": hit.kind, "excerpt": hit.excerpt} for hit in structural_tells(cleaned)
    ]
    placeholders = find_placeholders(text, placeholder)

    checks: list[JudgeCheck] = []
    penalty = 0
    for key in BOTH_DIRECTIONS:
        result = _compare(key, target_metrics.get(key), draft_metrics.get(key))
        if result is None:
            continue
        checks.append(result)
        penalty += result["penalty"]

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
            penalty += em_check["penalty"]

    ban_penalty = min(40, 10 * sum(item["count"] for item in banned))
    rule_penalty = min(24, 8 * sum(item["count"] for item in confirmed_hits))
    structure_penalty = min(24, 8 * sum(1 for item in structure if item["kind"] != "em-dash"))
    penalty += ban_penalty + rule_penalty + structure_penalty
    score = max(0, 100 - penalty)
    failed = score < fail_under
    return {
        "ok": not failed,
        "score": score,
        "failUnder": fail_under,
        "reviewRequired": bool(banned or confirmed_hits or structure or placeholders),
        "assessment": "mechanical-only",
        "section": target_label,
        "checks": checks,
        "banned": banned,
        "confirmedRules": confirmed_hits,
        "structure": structure,
        "placeholders": placeholders,
        "draft": dict(draft_metrics),
        "targets": target_metrics,
        "confidence": confidence,
        "lowConfidence": not confidence["trustworthy"],
    }


def format_report(report: Mapping[str, object]) -> str:
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
    confirmed_rules = report.get("confirmedRules")
    if isinstance(confirmed_rules, list) and confirmed_rules:
        lines.append("")
        lines.append("Confirmed rules")
        for item in confirmed_rules:
            if isinstance(item, dict):
                scope_note = f" [{item['scope']}]" if item.get("scope") else ""
                lines.append(f"  {item.get('id')} ({item.get('count')}){scope_note}")
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


def _targets(stats: Mapping[str, object], section: str | None) -> tuple[Mapping[str, object], str]:
    if not section:
        weighted = as_mapping(stats.get("weighted"))
        return weighted, "all"
    sections = stats.get("sections")
    if not isinstance(sections, Mapping) or section not in sections:
        known = ", ".join(sorted(sections)) if isinstance(sections, Mapping) else ""
        extra = f" Known sections: {known}." if known else ""
        raise ProjectError(f"No targets for section {section!r}.{extra}")
    block = sections[section]
    if not isinstance(block, Mapping):
        raise ProjectError(f"No targets for section {section!r}.")
    section_weighted = block.get("weighted")
    if not isinstance(section_weighted, Mapping):
        raise ProjectError(f"No targets for section {section!r}.")
    return section_weighted, section


def _compare(key: str, target: object, actual: object) -> JudgeCheck | None:
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
    status: Literal["ok", "low", "high", "off"] = "ok"
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


def _compare_em_dash(target: object, actual: object) -> JudgeCheck | None:
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
    status: Literal["ok", "low", "high", "off"] = "ok"
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
) -> tuple[list[JudgeCheck], int]:
    if isinstance(sentences, bool) or not isinstance(sentences, (int, float)):
        return [], 0
    if sentences < DISTRIBUTION_MIN_SENTENCES:
        return [], 0
    if not isinstance(target, Mapping) or not isinstance(actual, Mapping):
        return [], 0
    checks: list[JudgeCheck] = []
    penalty = 0
    for name in ("short", "medium", "long"):
        target_value = target.get(name)
        actual_value = actual.get(name)
        if isinstance(target_value, bool) or isinstance(actual_value, bool):
            continue
        if not isinstance(target_value, (int, float)) or not isinstance(actual_value, (int, float)):
            continue
        delta = abs(float(actual_value) - float(target_value))
        status: Literal["ok", "low", "high", "off"] = "ok" if delta <= DISTRIBUTION_ABS else "off"
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


def _has_comparable_targets(target_metrics: Mapping[str, object]) -> bool:
    return any(
        isinstance(target_metrics.get(key), (int, float))
        and not isinstance(target_metrics.get(key), bool)
        for key in BOTH_DIRECTIONS
    )


def _string_list(value: object) -> list[str]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str)]


def _active_cut_rules(
    stats: Mapping[str, object], section: str | None
) -> list[tuple[str, str | None]]:
    """Return deduplicated (word, scope) cut rules confirmed for `section`."""
    rules = as_mapping(stats.get("rules"))
    confirmed = rules.get("confirmed")
    if not isinstance(confirmed, list):
        return []
    by_word: dict[str, tuple[str, str | None]] = {}
    for item in confirmed:
        if not isinstance(item, dict) or item.get("kind") != "cut":
            continue
        raw_word = item.get("word")
        if not isinstance(raw_word, str):
            continue
        word = raw_word.strip()
        if not word:
            continue
        raw_scope = item.get("scope")
        rule_scope = raw_scope.strip() if isinstance(raw_scope, str) and raw_scope.strip() else None
        if rule_scope is not None and (section is None or rule_scope != section):
            continue
        key = word.casefold()
        existing = by_word.get(key)
        if existing is None or (existing[1] is not None and rule_scope is None):
            by_word[key] = (word, rule_scope)
    return list(by_word.values())
