"""Human-readable CLI reports. The command parser stays in cli.py."""

from __future__ import annotations

from pathlib import Path
from typing import cast

from ghostless.schema import (
    STRUCTURAL_RULE_LABELS,
    CheckItem,
    ChecksSummary,
    CompiledStats,
    ConfirmedRule,
    RuleSummary,
    SessionStartResult,
)


def _print_started(root: Path, result: SessionStartResult) -> None:
    print(f"Started {result['id']}")
    print(f"  original: {root / str(result['path']) / 'before.md'}")
    print(f"  working:  {root / str(result['path']) / 'after.md'}")
    print(f"Finish with: ghostless finish --session {result['id']}")


def _print_saved_checks(checks: object) -> None:
    if not isinstance(checks, list) or not checks:
        return
    for item in checks:
        if not isinstance(item, dict):
            continue
        status = item.get("status") or "pending"
        print(f"  check {item.get('id')}: {status}")
        print(f"    {item.get('text')}")
    print("  A check is inactive until you run `ghostless checks confirm <id>`.")


def _format_checks(summary: ChecksSummary) -> str:
    lines = [
        "Edit checks (yes/no judgments the mechanical score cannot see)",
        "",
        "Confirmed (active)",
    ]
    confirmed = summary["confirmed"]
    if not confirmed:
        lines.append("  none yet")
    for item in confirmed:
        lines.append(_check_heading(item))
    lines.append("")
    lines.append("Pending (saved from an edit, not active)")
    pending = summary["pending"]
    if not pending:
        lines.append("  none")
    for item in pending:
        lines.append(_check_heading(item))
    lines.append("")
    lines.append("Rejected (not brought back by a later edit)")
    rejected = summary["rejected"]
    if not rejected:
        lines.append("  none")
    for item in rejected:
        lines.append(_check_heading(item))
    lines.append("")
    lines.append("Decide one with: ghostless checks confirm|reject <id> [--section NAME]")
    lines.append("")
    return "\n".join(lines).rstrip()


def _check_heading(item: CheckItem) -> str:
    identifier = item.get("id") or "?"
    text = item.get("text") or ""
    section = item.get("section")
    scope = f"section {section}" if isinstance(section, str) and section else "all sections"
    pair = item.get("pair")
    where = f"gold/{pair}" if isinstance(pair, str) and pair else "no pair"
    return f"  {identifier}  ({scope}; {where})\n    {text}"


def _format_rules(summary: RuleSummary) -> str:
    minimum = summary["minimumSupport"]
    lines = [
        f"Voice rules from edit receipts (minimum support: {minimum} distinct pieces)",
        "",
    ]
    confirmed = summary["confirmed"]
    lines.append("Confirmed (active)")
    if not confirmed:
        lines.append("  none yet")
    else:
        for confirmed_rule in confirmed:
            lines.append(_rule_heading(confirmed_rule, confirmed=True))
            receipts = confirmed_rule["receipts"]
            if receipts:
                lines.append("    receipts: " + ", ".join(receipts))
    lines.append("")

    hypotheses = summary["hypotheses"]
    lines.append("Hypotheses (unconfirmed, not active)")
    if not hypotheses:
        lines.append("  none")
    for hypothesis in hypotheses:
        lines.append(_rule_heading(cast(ConfirmedRule, hypothesis), confirmed=False))
        for example in hypothesis["examples"]:
            before = example["before_snippet"].strip()
            after = example["after_snippet"].strip()
            lines.append(f'    {example["slug"]}: "{before}" -> "{after}"')
            if example["receipt"]:
                lines.append(f"      receipt: {example['receipt']}")
        counterexamples = hypothesis["counterexamples"]
        if counterexamples:
            lines.append(f"    counterexamples: {len(counterexamples)}")
            for counterexample in counterexamples[:2]:
                lines.append(f'      {counterexample["slug"]}: "{counterexample["snippet"]}"')
    lines.append("")

    for key, suppressed in (("rejected", summary["rejected"]), ("retired", summary["retired"])):
        label = "never active" if key == "rejected" else "no longer active"
        lines.append(f"{key.capitalize()} ({label})")
        if not suppressed:
            lines.append("  none")
        for suppressed_rule in suppressed:
            lines.append(f"  {suppressed_rule['id']}  {suppressed_rule.get('at') or ''}".rstrip())
        lines.append("")

    if not confirmed and not hypotheses:
        lines.append("No candidates yet. A word has to recur in enough distinct finished edits before it appears here.")
        lines.append("")
    else:
        lines.append("Decide one with: ghostless rules confirm|reject <id> [--section NAME]")
        lines.append("")
    return "\n".join(lines).rstrip()


def _rule_heading(item: ConfirmedRule, *, confirmed: bool) -> str:
    kind = str(item.get("kind") or "cut")
    word = str(item.get("word") or "?")
    detail: list[str] = []
    support = item.get("support")
    if isinstance(support, int) and not isinstance(support, bool):
        detail.append(f"{support} pieces")
    sections = item.get("sections")
    if isinstance(sections, list) and sections:
        detail.append("sections: " + ", ".join(str(name) for name in sections))
    if confirmed:
        scope = item.get("scope")
        detail.append(f"scope: {scope}" if isinstance(scope, str) and scope else "scope: all")
        at = item.get("confirmedAt")
        if isinstance(at, str) and at:
            detail.append(f"confirmed {at[:10]}")
    suffix = f"  ({'; '.join(detail)})" if detail else ""
    if kind == "struct":
        rule_id = str(item.get("id") or f"struct:{word}")
        label = STRUCTURAL_RULE_LABELS.get(word, word)
        return f"  {rule_id}  {label}{suffix}"
    verb = "cut" if kind == "cut" else "add"
    return f"  {verb} {word}{suffix}"


def _format_build(root: Path, stats: CompiledStats) -> str:
    counts = stats["counts"]
    prose = counts["prose"]
    spoken = counts["spoken"]
    spoken_total = spoken["gold"] + spoken["silver"] + spoken["bronze"] + spoken["edited"]
    weighted = stats["weighted"]["metrics"]
    unweighted = stats["unweighted"]["metrics"]
    ban_count = len(stats["weighted"]["banList"])
    lines = [
        f"voice guide: {root / 'voice-guide.md'}",
        (
            "pieces: "
            f"gold {prose['gold']}, "
            f"silver {prose['silver']}, "
            f"bronze {prose['bronze']}, "
            f"edited {prose['edited']}, "
            f"spoken {spoken_total}"
        ),
        f"bronze format-only: {'yes' if stats['bronzeFormatOnly'] else 'no'}",
        (f"gold prose pieces: {stats['goldPieces']} of {stats['goldVoiceMinimum']} to drop bronze from voice"),
        (
            "median sentence: "
            f"{weighted['medianSentenceLength']} words "
            f"(unweighted {unweighted['medianSentenceLength']})"
        ),
        f"ban list: {ban_count}",
    ]
    for warning in stats["warnings"]:
        lines.append(f"note: {warning}")
    return "\n".join(lines)
