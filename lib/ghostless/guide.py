"""Render voice-guide.md from compiled stats."""

from __future__ import annotations

import json

from ghostless.judge import corpus_confidence


def render_guide(stats: dict[str, object]) -> str:
    return _render(stats, paste=False, excerpts=None)


def render_paste(stats: dict[str, object], excerpts: list[dict[str, str]] | None) -> str:
    return _render(stats, paste=True, excerpts=excerpts)


def _render(
    stats: dict[str, object],
    *,
    paste: bool,
    excerpts: list[dict[str, str]] | None,
) -> str:
    weighted = _block(stats.get("weighted"))
    unweighted = _block(stats.get("unweighted"))
    metrics = _metrics(weighted)
    bare = _metrics(unweighted)
    lines: list[str] = []
    if paste:
        lines.append("# Voice guide (paste into a project)")
        lines.append("")
        lines.append(
            "Self-contained voice target for a writer. Paste this whole file into "
            "a Claude project or a ChatGPT project. It already holds the targets, "
            "the ban list, signature phrases, per-section targets, the words "
            "you cut or added in edits, and your STYLE.md and AUDIENCE.md when "
            "present. Nothing else on disk is required to follow it."
        )
    else:
        lines.append("# Voice guide")
        lines.append("")
        lines.append(
            "Measured from the local corpus. Weighted numbers are the targets. "
            "Unweighted numbers use the same pieces at weight 1, so you can see "
            "what the weights changed. Check a draft with `ghostless judge`."
        )
    lines.append("")
    lines.extend(_rules(stats, paste=paste))
    lines.extend(_voice_rules(stats))
    lines.extend(_style(stats))
    lines.extend(_audience(stats))
    lines.append("## Targets")
    lines.append("")
    lines.append("| Metric | Weighted | Unweighted |")
    lines.append("| --- | --- | --- |")
    rows = (
        ("Median sentence length (words)", "medianSentenceLength", _num),
        ("Contractions per 1k words", "contractionsPer1k", _num),
        ("First-person I per 1k words", "iPer1k", _num),
        ("Em dashes per 1k words", "emDashesPer1k", _num),
        ("Questions per 1k words", "questionsPer1k", _num),
        ("Sentences opening with And/But/So", "openerShare", _pct),
        ("Median paragraph length (words)", "medianParagraphLength", _num),
    )
    for label, key, formatter in rows:
        lines.append(
            f"| {label} | {formatter(metrics.get(key))} | {formatter(bare.get(key))} |"
        )
    lines.append("")
    lines.extend(_sample_size(stats))
    lines.append("## Sentence length")
    lines.append("")
    lines.append("Short is 1–10 words, medium is 11–20, long is 21 or more.")
    lines.append("")
    lines.append(f"- Weighted: {_distribution(metrics.get('sentenceLengthDistribution'))}")
    lines.append(f"- Unweighted: {_distribution(bare.get('sentenceLengthDistribution'))}")
    lines.append("")
    lines.append("## Openers")
    lines.append("")
    lines.append(_openers(metrics.get("openers"), "Weighted"))
    lines.append(_openers(bare.get("openers"), "Unweighted"))
    lines.append("")
    lines.extend(_phrases(weighted))
    lines.extend(_bans(weighted, stats))
    lines.extend(_sections(stats))
    lines.extend(_edits(stats.get("edits")))
    lines.extend(_weights(stats))
    if paste and excerpts:
        lines.append("## Gold excerpts")
        lines.append("")
        lines.append("Short samples from the writer's own gold. Included because export was run with excerpts turned on.")
        lines.append("")
        for index, excerpt in enumerate(excerpts, start=1):
            section = excerpt.get("section") or "unsectioned"
            lines.append(f"### Excerpt {index} ({section})")
            lines.append("")
            lines.append(excerpt.get("text", "").strip())
            lines.append("")
    generated = stats.get("generatedAt")
    if isinstance(generated, str) and generated:
        lines.append(f"Generated {generated}.")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def _rules(stats: dict[str, object], *, paste: bool) -> list[str]:
    lines = ["## Rules", ""]
    lines.append("- Use the targets above as diagnostics. They are counts from the writer's own text, not a measure of writing quality.")
    lines.append("- Review words and phrases on the absent-tell list in context, especially with a small corpus.")
    token = stats.get("placeholder")
    token = token if isinstance(token, str) and token else "WRITER"
    lines.append(
        "- Do not invent quotes, statistics, anecdotes, or opinions. "
        f"Where the piece needs the writer's take, leave `[{token}: your take]` and say what is missing."
    )
    lines.append("- Avoid the tidy frames models default to: \"it's not X, it's Y\", neat triplets, and em dashes stacked past the target.")
    if stats.get("bronzeFormatOnly"):
        minimum = stats.get("goldVoiceMinimum")
        gold = stats.get("goldPieces")
        lines.append(
            f"- Bronze is format-only right now ({gold} gold pieces, threshold {minimum}). "
            "It can shape paragraph length. It does not set voice, phrases, or the ban list."
        )
    else:
        lines.append(
            "- Bronze still counts toward voice. It drops out of voice once gold reaches the threshold in the manifest, and stays on for paragraph length."
        )
    lines.append(
        "- Spoken transcripts count only toward sentence length, contractions, first-person I, and And/But/So openers. They do not set paragraphs, em dashes, questions, phrases, or the ban list."
    )
    if not paste:
        lines.append("- Corpus files stay on this machine. Do not upload them.")
    lines.append("")
    return lines


def _voice_rules(stats: dict[str, object]) -> list[str]:
    block = _block(stats.get("rules"))
    lines = ["## Voice rules", ""]
    if not block:
        lines.append(
            "No rule data in this build. Run `ghostless build` to mine candidates from "
            "your edit receipts."
        )
        lines.append("")
        return lines
    minimum = block.get("minimumSupport")
    if isinstance(minimum, bool) or not isinstance(minimum, int):
        minimum = 3
    lines.append(
        f"Rules come from your edit receipts, not from the measured targets. A word becomes "
        f"a candidate only after it recurs across {minimum} distinct pieces, and it is active "
        "only after you confirm it. Measured targets live under Targets. Hypotheses are not "
        "instructions; they are waiting for your decision."
    )
    lines.append("")

    confirmed = block.get("confirmed")
    confirmed = confirmed if isinstance(confirmed, list) else []
    lines.append("### Confirmed rules")
    lines.append("")
    if confirmed:
        for item in confirmed:
            if isinstance(item, dict):
                lines.append(_rule_line(item, confirmed=True))
    else:
        lines.append("None yet. Review a hypothesis, then run `ghostless rules confirm <id>`.")
    lines.append("")

    hypotheses = block.get("hypotheses")
    hypotheses = hypotheses if isinstance(hypotheses, list) else []
    lines.append("### Hypotheses (unconfirmed, not active)")
    lines.append("")
    if hypotheses:
        for item in hypotheses:
            if not isinstance(item, dict):
                continue
            lines.append(_rule_line(item, confirmed=False))
            examples = item.get("examples")
            if isinstance(examples, list):
                for example in examples[:1]:
                    if not isinstance(example, dict):
                        continue
                    before = str(example.get("before_snippet") or "").strip()
                    after = str(example.get("after_snippet") or "").strip()
                    if before or after:
                        lines.append(
                            f"  - {example.get('slug')}: \"{before}\" → \"{after}\""
                        )
            counterexamples = item.get("counterexamples")
            if isinstance(counterexamples, list) and counterexamples:
                lines.append(
                    f"  - Counterexample: {len(counterexamples)} piece(s) kept the word, "
                    "so read the pattern before trusting it."
                )
    else:
        lines.append("None yet. A pattern needs repeated support before it appears here.")
    lines.append("")

    suppressed = [
        item
        for key in ("rejected", "retired")
        for item in (block.get(key) if isinstance(block.get(key), list) else [])
        if isinstance(item, dict)
    ]
    if suppressed:
        parts = [
            f"{item.get('id')} ({item.get('status')})"
            for item in suppressed
            if item.get("id")
        ]
        lines.append("Rejected or retired (not active): " + ", ".join(parts) + ".")
        lines.append("")
    return lines


def _rule_line(item: dict[str, object], *, confirmed: bool) -> str:
    kind = str(item.get("kind") or "cut")
    verb = "Cut" if kind == "cut" else "Add"
    word = item.get("word") or item.get("id") or "?"
    support = item.get("support")
    scope = item.get("scope")
    detail: list[str] = []
    if isinstance(support, int) and not isinstance(support, bool):
        detail.append(f"{support} piece(s)")
    sections = item.get("sections")
    if isinstance(sections, list) and sections:
        detail.append("sections: " + ", ".join(str(name) for name in sections))
    if confirmed:
        detail.append(f"scope: {scope}" if isinstance(scope, str) and scope else "scope: all sections")
        at = item.get("confirmedAt")
        if isinstance(at, str) and at:
            detail.append(f"confirmed {at[:10]}")
        receipts = item.get("receipts")
        if isinstance(receipts, list) and receipts:
            detail.append("receipts: " + ", ".join(str(receipt) for receipt in receipts[:4]))
    suffix = f" ({'; '.join(detail)})" if detail else ""
    return f"- {verb} `{word}`{suffix}"


def _style(stats: dict[str, object]) -> list[str]:
    text = stats.get("style")
    lines = ["## Style", ""]
    if not isinstance(text, str) or not text:
        lines.append(
            "No STYLE.md yet. This optional file holds what a piece must do, "
            "contain, and prove — argument, evidence, structure, readiness. "
            "Voice stays measured, not described."
        )
        lines.append("")
        return lines
    lines.append(
        "Substantive standards, not voice. Wording and rhythm come from the Targets table."
    )
    lines.append("")
    lines.append(_note_body(text))
    lines.append("")
    return lines


def _audience(stats: dict[str, object]) -> list[str]:
    text = stats.get("audience")
    lines = ["## Audience", ""]
    if not isinstance(text, str) or not text:
        lines.append(
            "No AUDIENCE.md yet. This optional file holds who reads the piece — "
            "situation, knowledge, wants, interests, resistance."
        )
        lines.append("")
        return lines
    lines.append("Who reads the piece. This is reader context, not voice.")
    lines.append("")
    lines.append(_note_body(text))
    lines.append("")
    return lines


def _note_body(text: str) -> str:
    """The note content with a leading top-level heading dropped.

    The guide already supplies the `## Style` / `## Audience` heading, so the
    file's own `# Style` / `# Audience` line would only repeat it.
    """
    lines = text.splitlines()
    while lines and not lines[0].strip():
        lines.pop(0)
    if lines and lines[0].lstrip().startswith("# "):
        lines.pop(0)
        while lines and not lines[0].strip():
            lines.pop(0)
    return "\n".join(lines).strip()


def _sample_size(stats: dict[str, object]) -> list[str]:
    confidence = corpus_confidence(stats)
    counts = _block(stats.get("counts"))
    written_pieces = _sum_counts(_block(counts.get("prose")))
    spoken_pieces = _sum_counts(_block(counts.get("spoken")))
    lines = ["## Sample size", ""]
    lines.append(
        f"- Pieces: {_num(confidence.get('pieces'))} measured "
        f"({written_pieces} written, {spoken_pieces} spoken)"
    )
    lines.append(
        f"- Words: {_num(confidence.get('words'))} measured, "
        f"{_num(confidence.get('writtenWords'))} written prose"
    )
    lines.append(f"- Sentences: {_num(confidence.get('sentences'))}")
    level = confidence.get("level")
    note = confidence.get("note")
    if isinstance(note, str) and note:
        lines.append(f"- Confidence: {level}. {note}")
    else:
        lines.append(f"- Confidence: {level}.")
    lines.append("")
    return lines


def _sum_counts(block: dict[str, object]) -> int:
    return sum(
        int(value)
        for value in block.values()
        if isinstance(value, int) and not isinstance(value, bool)
    )


def _phrases(block: dict[str, object]) -> list[str]:
    raw = block.get("signaturePhrases")
    lines = ["## Signature phrases", ""]
    if not isinstance(raw, list) or not raw:
        lines.append("None yet. A phrase has to show up at least twice in the written voice pool.")
        lines.append("")
        return lines
    lines.append("These show up at least twice in the written voice pool. Use one only when it actually fits.")
    lines.append("")
    for item in raw:
        if not isinstance(item, dict):
            continue
        phrase = item.get("phrase")
        count = item.get("count")
        if isinstance(phrase, str):
            lines.append(f"- {phrase} ({count})")
    lines.append("")
    return lines


def _bans(block: dict[str, object], stats: dict[str, object]) -> list[str]:
    lines = ["## Ban list", ""]
    if block.get("dictionEmpty"):
        lines.append("Not inferred. There is no written prose in the voice pool yet, and spoken lines cannot fill a ban list.")
        lines.append("")
        return lines
    ban = block.get("banList")
    used = block.get("tellsInUse")
    if isinstance(ban, list) and ban:
        lines.append("Catalog AI-tells that do not appear in the written voice pool:")
        lines.append("")
        for tell in ban:
            if isinstance(tell, str):
                lines.append(f"- {tell}")
        lines.append("")
    else:
        lines.append("None. Every catalog tell showed up in the written voice pool, so nothing was banned.")
        lines.append("")
    if isinstance(used, list) and used:
        lines.append("Present in the written voice pool, so not banned:")
        lines.append("")
        for tell in used:
            if isinstance(tell, str):
                lines.append(f"- {tell}")
        lines.append("")
    warnings = stats.get("warnings")
    if isinstance(warnings, list):
        for warning in warnings:
            if isinstance(warning, str) and warning not in "\n".join(lines):
                lines.append(f"Note: {warning}")
                lines.append("")
    return lines


def _sections(stats: dict[str, object]) -> list[str]:
    raw = stats.get("sections")
    lines = ["## Sections", ""]
    if not isinstance(raw, dict) or not raw:
        lines.append("No section tags yet. Add `section:` in frontmatter, or put files in a subfolder of a tier, and rebuild.")
        lines.append("")
        return lines
    lines.append("Pass `--section` to `ghostless judge` to use a section's weighted targets. The ban list stays global.")
    lines.append("")
    for name in sorted(raw):
        block = raw[name]
        if not isinstance(block, dict):
            continue
        weighted = _metrics(_block(block.get("weighted")))
        lines.append(f"### {name}")
        lines.append("")
        lines.append(f"- Pieces: {block.get('pieces')}")
        lines.append(f"- Median sentence length: {_num(weighted.get('medianSentenceLength'))} words")
        lines.append(f"- Contractions per 1k: {_num(weighted.get('contractionsPer1k'))}")
        lines.append(f"- I per 1k: {_num(weighted.get('iPer1k'))}")
        lines.append(f"- And/But/So openers: {_pct(weighted.get('openerShare'))}")
        lines.append(f"- Em dashes per 1k: {_num(weighted.get('emDashesPer1k'))}")
        lines.append(f"- Median paragraph length: {_num(weighted.get('medianParagraphLength'))} words")
        lines.append("")
    return lines


def _edits(edits: object) -> list[str]:
    if not isinstance(edits, dict):
        return []
    learns = edits.get("learns")
    if not isinstance(learns, list) or not learns:
        return []
    lines = ["## What your edits do", ""]
    lines.append("Each pair you kept with `ghostless learn` is edit evidence. A writer-first final is gold; an AI-first final is measured as edited until you `ghostless promote` it. The shifts below are before → after.")
    lines.append("")
    for item in learns:
        if not isinstance(item, dict):
            continue
        slug = item.get("slug")
        origin = item.get("origin") or "unspecified"
        section = item.get("section") or "unsectioned"
        lines.append(f"### {slug}")
        lines.append("")
        lines.append(f"- {origin}, section {section}")
        lines.append(
            f"- Words: {_num(item.get('wordsBefore'))} → {_num(item.get('wordsAfter'))} "
            f"(removed {_num(item.get('wordsRemoved'))} tokens, added {_num(item.get('wordsAdded'))})"
        )
        lines.append(
            f"- Edit rate: {_num(item.get('editRate'))} "
            f"({_num(item.get('tokenEdits'))} token edits / "
            f"{_num(item.get('editRateDenominator'))} aligned positions)"
        )
        lines.append(
            f"- Moved: {_num(item.get('movedTokens'))} tokens in {_span_count(item.get('movedSpans'))} span(s)"
        )
        lines.append(f"- Structure: {_structure(item.get('structuralChanges'))}")
        lines.append(
            f"- Median sentence length: {_num(item.get('medianSentenceLengthBefore'))} → "
            f"{_num(item.get('medianSentenceLengthAfter'))}"
        )
        lines.append(
            f"- Contractions per 1k: {_num(item.get('contractionsPer1kBefore'))} → "
            f"{_num(item.get('contractionsPer1kAfter'))}"
        )
        lines.append(f"- Cut: {_word_list(item.get('removedWords'))}")
        lines.append(f"- Added: {_word_list(item.get('addedWords'))}")
        lines.append("")
    lines.append("Cut more than once: " + _repeat(edits.get("repeatedRemovals")))
    lines.append("")
    lines.append("Added more than once: " + _repeat(edits.get("repeatedAdditions")))
    lines.append("")
    return lines


def _weights(stats: dict[str, object]) -> list[str]:
    weights = stats.get("weights")
    lines = ["## Weights", ""]
    if isinstance(weights, dict):
        lines.append(
            f"- gold {weights.get('gold')}, silver {weights.get('silver')}, bronze {weights.get('bronze')}"
        )
    lines.append(
        f"- Gold pieces: {stats.get('goldPieces')}. "
        f"Bronze becomes format-only at {stats.get('goldVoiceMinimum')}."
    )
    counts = stats.get("counts")
    if isinstance(counts, dict):
        lines.append(f"- Counts: `{json_compact(counts)}`")
    lines.append("")
    return lines


def json_compact(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(", ", ": "))


def _block(value: object) -> dict[str, object]:
    return value if isinstance(value, dict) else {}


def _metrics(block: dict[str, object]) -> dict[str, object]:
    metrics = block.get("metrics")
    return metrics if isinstance(metrics, dict) else {}


def _num(value: object) -> str:
    if isinstance(value, bool) or value is None:
        return "n/a"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if value.is_integer():
            return str(int(value))
        text = f"{value:.2f}".rstrip("0").rstrip(".")
        return text
    return str(value)


def _pct(value: object) -> str:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return "n/a"
    return f"{value * 100:.0f}%"


def _distribution(value: object) -> str:
    if not isinstance(value, dict):
        return "n/a"
    return (
        f"{_pct(value.get('short'))} short, "
        f"{_pct(value.get('medium'))} medium, "
        f"{_pct(value.get('long'))} long"
    )


def _openers(value: object, label: str) -> str:
    if not isinstance(value, dict):
        return f"- {label}: n/a"
    return (
        f"- {label}: And {_pct(value.get('and'))}, "
        f"But {_pct(value.get('but'))}, So {_pct(value.get('so'))}"
    )


def _word_list(value: object) -> str:
    if not isinstance(value, list) or not value:
        return "nothing tracked"
    parts: list[str] = []
    for item in value[:12]:
        if not isinstance(item, dict):
            continue
        word = item.get("word")
        count = item.get("count")
        if isinstance(word, str):
            parts.append(f"{word} ({count})")
    return ", ".join(parts) if parts else "nothing tracked"


def _repeat(value: object) -> str:
    if not isinstance(value, dict) or not value:
        return "none yet"
    items = [(str(word), count) for word, count in value.items() if isinstance(count, int)]
    items.sort(key=lambda item: (-item[1], item[0]))
    return ", ".join(f"{word} ({count})" for word, count in items)


def _span_count(value: object) -> str:
    if not isinstance(value, list):
        return "0"
    return str(sum(1 for item in value if isinstance(item, dict)))


def _structure(value: object) -> str:
    if not isinstance(value, list) or not value:
        return "no structural change"
    parts: list[str] = []
    for item in value:
        if not isinstance(item, dict):
            continue
        if item.get("kind") == "punctuation":
            if item.get("change") == "order":
                parts.append("punctuation order")
            else:
                parts.append(f"{item.get('mark')} {item.get('before')} → {item.get('after')}")
        elif item.get("kind") == "paragraphs":
            parts.append(f"paragraphs {item.get('before')} → {item.get('after')}")
        elif item.get("kind") == "paragraph":
            parts.append(f"paragraph moved {item.get('from')} → {item.get('to')}")
    return "; ".join(parts) if parts else "no structural change"
