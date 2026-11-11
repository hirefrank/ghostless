"""Store a before/after edit and summarize what changed.

A pair is always kept as edit evidence. Where the final is measured depends on
provenance: an AI-first final lands in the `edited` tier (model prose the
writer revised) unless the writer promotes it to gold. A writer-first pair is
gold from the start. See `placement_for` in `project.py`.
"""

from __future__ import annotations

import difflib
import json
import re
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

from ghostless.paths import ProjectError
from ghostless.project import (
    fresh_directory,
    parse_confirmation,
    parse_origin,
    placement_for,
    provenance_json,
    read_provenance,
    slug_for,
)
from ghostless.stats import weighted_median
from ghostless.text import measure, normalize, paragraphs, track_token, word_tokens

DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}")

# Below this edit rate a final is mostly the model's prose. Promotion still
# works, but the CLI says so before the writer counts it as gold.
MOSTLY_MODEL_RATE = 0.25

# Characters worth reporting when an edit changes structure but not words.
# Apostrophes matter for contractions ("dont" -> "don't"); markdown marks
# matter because a writer may add or drop emphasis in a revision.
PUNCTUATION = ".,;:!?…—–-()[]{}\"'*_#/"
PUNCTUATION_RE = re.compile("[" + re.escape(PUNCTUATION) + "]")


def diff_texts(before: str, after: str) -> dict[str, object]:
    before_measured = measure(before)
    after_measured = measure(after)
    before_counts = Counter(word_tokens(before))
    after_counts = Counter(word_tokens(after))
    removed_words: list[dict[str, object]] = []
    added_words: list[dict[str, object]] = []
    for token in set(before_counts) | set(after_counts):
        delta = before_counts[token] - after_counts[token]
        if delta > 0:
            if track_token(token):
                removed_words.append({"word": token, "count": delta})
        elif delta < 0:
            if track_token(token):
                added_words.append({"word": token, "count": -delta})
    removed_words.sort(key=lambda item: (-int(item["count"]), str(item["word"])))
    added_words.sort(key=lambda item: (-int(item["count"]), str(item["word"])))

    sequence = _align_tokens(before, after)
    structural_changes, paragraphs_before, paragraphs_after = _structural_changes(before, after)
    token_edits = (
        int(sequence["deletions"])
        + int(sequence["insertions"])
        + int(sequence["replacements"])
        + int(sequence["movedTokens"])
    )
    unchanged_tokens = int(sequence["unchangedTokens"])
    edit_denominator = token_edits + unchanged_tokens
    edit_rate = round(token_edits / edit_denominator, 4) if edit_denominator else 0.0

    return {
        "wordsBefore": before_measured.words,
        "wordsAfter": after_measured.words,
        # Sequence-aware change counts. A replacement removes one token and
        # adds another, so it appears once on each side.
        "wordsRemoved": int(sequence["deletions"]) + int(sequence["replacements"]),
        "wordsAdded": int(sequence["insertions"]) + int(sequence["replacements"]),
        "medianSentenceLengthBefore": _median(before_measured.sentence_lengths),
        "medianSentenceLengthAfter": _median(after_measured.sentence_lengths),
        "contractionsPer1kBefore": _rate(before_measured.contractions, before_measured.words),
        "contractionsPer1kAfter": _rate(after_measured.contractions, after_measured.words),
        # Vocabulary cut and added, counted as a bag. This is what repeated
        # cut/add rollups use; a pure reorder leaves these empty.
        "removedWords": removed_words,
        "addedWords": added_words,
        # Sequence layer.
        "tokenEdits": token_edits,
        "unchangedTokens": unchanged_tokens,
        "deletions": sequence["deletions"],
        "insertions": sequence["insertions"],
        "replacements": sequence["replacements"],
        "movedTokens": sequence["movedTokens"],
        "movedSpans": sequence["movedSpans"],
        # Edit rate: edited token positions / all aligned positions
        # (tokenEdits + unchangedTokens). Moved tokens count as edits, so a
        # pure reorder is not a no-op. The share is bounded 0..1; see
        # SCHEMA.md.
        "editRate": edit_rate,
        "editRateDenominator": edit_denominator,
        # Structural layer. Punctuation and paragraph edits survive even when
        # the word sequence is identical.
        "paragraphsBefore": paragraphs_before,
        "paragraphsAfter": paragraphs_after,
        "sentencesBefore": before_measured.sentences,
        "sentencesAfter": after_measured.sentences,
        "structuralChanges": structural_changes,
        "structuralChangeCount": len(structural_changes),
    }


def _align_tokens(before: str, after: str) -> dict[str, object]:
    """Align both token streams and separate moves from edits.

    A token deleted in one place and inserted in another is a move, not a
    deletion plus an insertion. That keeps a reorder visible without inflating
    removed/added counts.
    """
    before_tokens = word_tokens(before)
    after_tokens = word_tokens(after)
    matcher = difflib.SequenceMatcher(None, before_tokens, after_tokens, autojunk=False)

    delete_blocks: list[tuple[int, list[str]]] = []
    insert_blocks: list[tuple[int, list[str]]] = []
    deletions = 0
    insertions = 0
    replacements = 0
    unchanged = 0

    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            unchanged += i2 - i1
            continue
        if tag == "delete":
            deletions += i2 - i1
            delete_blocks.append((i1, before_tokens[i1:i2]))
        elif tag == "insert":
            insertions += j2 - j1
            insert_blocks.append((j1, after_tokens[j1:j2]))
        else:  # replace
            before_run = before_tokens[i1:i2]
            after_run = after_tokens[j1:j2]
            paired = min(len(before_run), len(after_run))
            replacements += paired
            if len(before_run) > paired:
                deletions += len(before_run) - paired
                delete_blocks.append((i1 + paired, before_run[paired:]))
            elif len(after_run) > paired:
                insertions += len(after_run) - paired
                insert_blocks.append((j1 + paired, after_run[paired:]))

    # Match deleted runs against inserted runs. Anything that appears on both
    # sides is the same word moved, not a word written or cut. Use a multiset
    # overlap rather than an ordered alignment: a swapped pair reverses order,
    # so an ordered LCS would miss half of a clean reorder. Greedy difflib
    # tie-breaks would also anchor one half of the swap and call the rest an
    # insert plus a delete.
    deleted_flat = [
        (start + offset, token)
        for start, tokens in delete_blocks
        for offset, token in enumerate(tokens)
    ]
    inserted_flat = [
        (start + offset, token)
        for start, tokens in insert_blocks
        for offset, token in enumerate(tokens)
    ]
    moved_counts = Counter(token for _, token in deleted_flat) & Counter(
        token for _, token in inserted_flat
    )
    moved_tokens = sum(moved_counts.values())
    moved_spans = _moved_spans(deleted_flat, inserted_flat, moved_counts) if moved_tokens else []

    return {
        "deletions": deletions - moved_tokens,
        "insertions": insertions - moved_tokens,
        "replacements": replacements,
        "movedTokens": moved_tokens,
        "movedSpans": moved_spans,
        "unchangedTokens": unchanged,
    }


def _moved_spans(
    deleted_flat: list[tuple[int, str]],
    inserted_flat: list[tuple[int, str]],
    moved_counts: Counter[str],
) -> list[dict[str, object]]:
    """Group moved tokens into contiguous runs on the before side."""
    remaining = Counter(moved_counts)
    available = Counter(moved_counts)
    positions: dict[str, list[int]] = {}
    for position, (_, token) in enumerate(inserted_flat):
        positions.setdefault(token, []).append(position)
    cursors: dict[str, int] = {token: 0 for token in positions}
    spans: list[dict[str, object]] = []
    run: list[int] = []

    def flush(indexes: list[int]) -> None:
        if not indexes:
            return
        tokens = [deleted_flat[index][1] for index in indexes]
        targets: list[int] = []
        for token in tokens:
            if available[token] <= 0:
                continue
            candidates = positions.get(token, [])
            cursor = cursors.get(token, 0)
            if cursor >= len(candidates):
                continue
            chosen = candidates[cursor]
            cursors[token] = cursor + 1
            available[token] -= 1
            targets.append(inserted_flat[chosen][0])
        if not targets:
            return
        spans.append(
            {
                "tokens": len(indexes),
                "text": " ".join(tokens),
                "from": deleted_flat[indexes[0]][0],
                "to": min(targets),
            }
        )

    for index, (position, token) in enumerate(deleted_flat):
        if remaining[token] <= 0:
            if run:
                flush(run)
                run = []
            continue
        remaining[token] -= 1
        if run and deleted_flat[index - 1][0] + 1 != position:
            flush(run)
            run = []
        run.append(index)
    flush(run)
    return spans


def _structural_changes(before: str, after: str) -> tuple[list[dict[str, object]], int, int]:
    before_text = normalize(before)
    after_text = normalize(after)
    before_sequence = PUNCTUATION_RE.findall(before_text)
    after_sequence = PUNCTUATION_RE.findall(after_text)
    before_marks = Counter(before_sequence)
    after_marks = Counter(after_sequence)
    changes: list[dict[str, object]] = []
    for mark in sorted(set(before_marks) | set(after_marks)):
        delta = after_marks[mark] - before_marks[mark]
        if delta:
            changes.append(
                {
                    "kind": "punctuation",
                    "mark": mark,
                    "before": before_marks[mark],
                    "after": after_marks[mark],
                    "delta": delta,
                }
            )
    if before_marks == after_marks and before_sequence != after_sequence:
        # Same marks, new positions: a punctuation-only edit that count
        # deltas alone would miss.
        changes.append(
            {
                "kind": "punctuation",
                "change": "order",
                "before": len(before_sequence),
                "after": len(after_sequence),
            }
        )

    before_paragraphs = paragraphs(before)
    after_paragraphs = paragraphs(after)
    if len(before_paragraphs) != len(after_paragraphs):
        changes.append(
            {
                "kind": "paragraphs",
                "before": len(before_paragraphs),
                "after": len(after_paragraphs),
                "delta": len(after_paragraphs) - len(before_paragraphs),
            }
        )
    changes.extend(_paragraph_moves(before_paragraphs, after_paragraphs))
    return changes, len(before_paragraphs), len(after_paragraphs)


def _paragraph_moves(before_paragraphs: list[str], after_paragraphs: list[str]) -> list[dict[str, object]]:
    """Report paragraphs that appear on both sides but in a new position."""
    before_keys = [" ".join(block.split()).casefold() for block in before_paragraphs]
    after_keys = [" ".join(block.split()).casefold() for block in after_paragraphs]
    matcher = difflib.SequenceMatcher(None, before_keys, after_keys, autojunk=False)
    matched_before: set[int] = set()
    matched_after: set[int] = set()
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            matched_before.update(range(i1, i2))
            matched_after.update(range(j1, j2))
    deleted = [(index, before_keys[index]) for index in range(len(before_keys)) if index not in matched_before]
    inserted = [(index, after_keys[index]) for index in range(len(after_keys)) if index not in matched_after]
    used: set[int] = set()
    moves: list[dict[str, object]] = []
    for from_index, key in deleted:
        for to_index, candidate in inserted:
            if to_index in used or candidate != key:
                continue
            used.add(to_index)
            moves.append(
                {
                    "kind": "paragraph",
                    "change": "moved",
                    "from": from_index,
                    "to": to_index,
                    "preview": _preview(before_paragraphs[from_index]),
                }
            )
            break
    return moves


def _preview(text: str, limit: int = 80) -> str:
    collapsed = " ".join(text.split())
    if len(collapsed) <= limit:
        return collapsed
    return collapsed[: limit - 1].rstrip() + "…"


def learn_edit(
    root: Path,
    *,
    before: Path,
    after: Path,
    section: str | None,
    origin: str,
    date: str | None = None,
    model: str | None = None,
    tool: str | None = None,
    promote: bool = False,
) -> dict[str, object]:
    if not before.is_file():
        raise ProjectError(f"Before file not found: {before}")
    if not after.is_file():
        raise ProjectError(f"After file not found: {after}")
    if before.resolve() == after.resolve():
        raise ProjectError("Before and after are the same file.")
    checked_origin = parse_origin(origin, path="--origin")
    date_text = date or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    if not DATE_RE.fullmatch(date_text):
        raise ProjectError("--date must be YYYY-MM-DD.")
    slug = slug_for(date_text, section)
    before_text = before.read_text(encoding="utf-8")
    after_text = after.read_text(encoding="utf-8")
    for existing in sorted((root / "gold").glob("*/meta.json")):
        prior_before = existing.parent / "before.md"
        prior_after = existing.parent / "after.md"
        if prior_before.is_file() and prior_after.is_file():
            if (prior_before.read_text(encoding="utf-8").rstrip("\n") == before_text.rstrip("\n")
                    and prior_after.read_text(encoding="utf-8").rstrip("\n") == after_text.rstrip("\n")):
                raise ProjectError(f"This edit pair was already learned in {existing.parent.name}.")
    destination = fresh_directory(root / "gold", slug)
    destination.mkdir(parents=True, exist_ok=False)
    (destination / "before.md").write_text(_trailing_newline(before_text), encoding="utf-8")
    (destination / "after.md").write_text(_trailing_newline(after_text), encoding="utf-8")
    summary = diff_texts(before_text, after_text)
    if promote:
        confirmation = "promoted"
    elif checked_origin == "writer-first":
        # The first draft was the writer's, so the origin itself settles it.
        confirmation = "origin"
    else:
        confirmation = "none"
    provenance = provenance_json(
        checked_origin,
        confirmation=confirmation,
        model=model,
        tool=tool,
    )
    placement = placement_for(read_provenance({"provenance": provenance}, path=str(destination)))
    meta: dict[str, object] = {
        "date": date_text,
        "section": section,
        # v0.1 mirror. `provenance` is canonical; this stays for old readers.
        "origin": checked_origin,
        "provenance": provenance,
        "kind": "prose",
        "beforeFile": "before.md",
        "afterFile": "after.md",
    }
    meta.update(summary)
    (destination / "meta.json").write_text(
        json.dumps(meta, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    edits = refresh_edit_summary(root)
    return {
        "slug": destination.name,
        "path": destination.relative_to(root).as_posix(),
        "meta": meta,
        "edits": edits,
        "origin": checked_origin,
        "confirmation": confirmation,
        "tier": placement,
        "model": model,
        "tool": tool,
        "warning": _mostly_model_warning(
            summary if placement == "gold" and checked_origin == "ai-first" else None
        ),
    }


def promote_final(root: Path, slug: str, *, gold: bool = True) -> dict[str, object]:
    """Deliberately move a learned final into (or back out of) gold.

    Only provenance changes: `before.md`, `after.md`, and the edit receipt are
    untouched, because the writer's changes are evidence either way.
    """
    meta_path = root / "gold" / slug / "meta.json"
    if not meta_path.is_file():
        raise ProjectError(f"No learned pair at gold/{slug}.")
    try:
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ProjectError(f"{meta_path}: {exc}") from exc
    if not isinstance(meta, dict):
        raise ProjectError(f"{meta_path} must be an object.")
    provenance = read_provenance(meta, path=str(meta_path))
    previous = placement_for(provenance)
    if not gold and provenance.origin == "writer-first":
        raise ProjectError(
            f"gold/{slug} is writer-first, so it is gold through its origin. "
            "Demote only an AI-first final."
        )
    if gold:
        confirmation = "promoted"
    else:
        confirmation = "none"
    checked = parse_confirmation(confirmation, path=str(meta_path))
    block = provenance_json(
        provenance.origin,
        confirmation=checked,
        model=provenance.model,
        tool=provenance.tool,
    )
    meta["provenance"] = block
    if provenance.origin is not None:
        meta["origin"] = provenance.origin
    meta_path.write_text(
        json.dumps(meta, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    placement = placement_for(read_provenance(meta, path=str(meta_path)))
    edits = refresh_edit_summary(root)
    summary = _receipt_summary(meta_path.parent / "before.md", meta_path.parent / "after.md")
    return {
        "slug": slug,
        "previous": previous,
        "tier": placement,
        "changed": placement != previous,
        "origin": provenance.origin,
        "confirmation": checked,
        "model": provenance.model,
        "tool": provenance.tool,
        "meta": meta,
        "edits": edits,
        "warning": _mostly_model_warning(
            summary
            if placement == "gold" and provenance.origin in (None, "ai-first")
            else None
        ),
    }


def _receipt_summary(before: Path, after: Path) -> dict[str, object] | None:
    if not before.is_file() or not after.is_file():
        return None
    return diff_texts(
        before.read_text(encoding="utf-8"),
        after.read_text(encoding="utf-8"),
    )


def _mostly_model_warning(summary: dict[str, object] | None) -> str | None:
    if not summary:
        return None
    rate = summary.get("editRate")
    if isinstance(rate, bool) or not isinstance(rate, (int, float)):
        return None
    if float(rate) >= MOSTLY_MODEL_RATE:
        return None
    unchanged = summary.get("unchangedTokens")
    total = summary.get("editRateDenominator")
    return (
        f"edit rate {rate} is below {MOSTLY_MODEL_RATE}: "
        f"{unchanged} of {total} aligned tokens are still as the model wrote them, "
        "so this final is mostly model prose. Gold weight will teach the guide "
        "that voice. Promote only a final you rewrote in your own words."
    )


def refresh_edit_summary(root: Path) -> dict[str, object]:
    learns: list[dict[str, object]] = []
    folder = root / "gold"
    if folder.is_dir():
        for meta_path in sorted(folder.glob("*/meta.json")):
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            if not isinstance(meta, dict):
                continue
            before_name = meta.get("beforeFile") if isinstance(meta.get("beforeFile"), str) else "before.md"
            after_name = meta.get("afterFile") if isinstance(meta.get("afterFile"), str) else "after.md"
            before_path = meta_path.parent / str(before_name)
            after_path = meta_path.parent / str(after_name)
            if not before_path.is_file() or not after_path.is_file():
                continue
            summary = diff_texts(
                before_path.read_text(encoding="utf-8"),
                after_path.read_text(encoding="utf-8"),
            )
            provenance = read_provenance(meta, path=str(meta_path))
            learns.append(
                {
                    "slug": meta_path.parent.name,
                    "date": meta.get("date"),
                    "section": meta.get("section"),
                    "origin": provenance.origin,
                    "confirmation": provenance.confirmation,
                    "model": provenance.model,
                    "tool": provenance.tool,
                    "tier": placement_for(provenance),
                    **summary,
                }
            )
    removals: Counter[str] = Counter()
    additions: Counter[str] = Counter()
    for item in learns:
        removals.update({word for word, _ in _word_entries(item.get("removedWords"))})
        additions.update({word for word, _ in _word_entries(item.get("addedWords"))})
    payload: dict[str, object] = {
        "learns": learns,
        "repeatedRemovals": _repeated(removals),
        "repeatedAdditions": _repeated(additions),
    }
    summary_json = root / "edits-summary.json"
    summary_md = root / "edits-summary.md"
    if learns:
        summary_json.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        summary_md.write_text(render_edits_markdown(payload), encoding="utf-8")
    else:
        if summary_json.exists():
            summary_json.unlink()
        if summary_md.exists():
            summary_md.unlink()
    return payload


def load_edit_summary(root: Path) -> dict[str, object]:
    path = root / "edits-summary.json"
    if not path.is_file():
        return {"learns": [], "repeatedRemovals": {}, "repeatedAdditions": {}}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {"learns": [], "repeatedRemovals": {}, "repeatedAdditions": {}}
    if not isinstance(data, dict):
        return {"learns": [], "repeatedRemovals": {}, "repeatedAdditions": {}}
    return data


def render_edits_markdown(payload: dict[str, object]) -> str:
    lines = [
        "# Edit summary",
        "",
        "Local only. Each entry is an AI draft (or earlier draft) next to the writer's final.",
        "",
    ]
    learns = payload.get("learns")
    if not isinstance(learns, list) or not learns:
        lines.append("No edits captured yet.")
        lines.append("")
        return "\n".join(lines)
    for item in learns:
        if not isinstance(item, dict):
            continue
        lines.append(f"## {item.get('slug')}")
        lines.append("")
        lines.append(f"- Origin: {item.get('origin') or 'unspecified'}")
        lines.append(f"- Measured as: {item.get('tier') or 'unknown'} ({_confirmation_note(item.get('confirmation'))})")
        if item.get("model") or item.get("tool"):
            lines.append(
                f"- Model/tool: {item.get('model') or 'unnamed model'}"
                f"{' via ' + str(item.get('tool')) if item.get('tool') else ''}"
            )
        lines.append(f"- Section: {item.get('section') or 'unsectioned'}")
        lines.append(f"- Date: {item.get('date')}")
        lines.append(
            f"- Words: {item.get('wordsBefore')} → {item.get('wordsAfter')} "
            f"(removed {item.get('wordsRemoved')} tokens, added {item.get('wordsAdded')})"
        )
        lines.append(
            f"- Edit rate: {_format_number(item.get('editRate'))} "
            f"({item.get('tokenEdits', 0)} token edits / "
            f"{item.get('editRateDenominator', 0)} aligned positions)"
        )
        lines.append(f"- Moved: {_format_moves(item.get('movedSpans'))}")
        lines.append(f"- Structure: {_format_structure(item.get('structuralChanges'))}")
        lines.append(
            f"- Median sentence length: {item.get('medianSentenceLengthBefore')} → "
            f"{item.get('medianSentenceLengthAfter')}"
        )
        lines.append(
            f"- Contractions per 1k: {item.get('contractionsPer1kBefore')} → "
            f"{item.get('contractionsPer1kAfter')}"
        )
        lines.append(f"- Words removed: {_format_words(item.get('removedWords'))}")
        lines.append(f"- Words added: {_format_words(item.get('addedWords'))}")
        lines.append("")
    lines.append("## Removed more than once")
    lines.append("")
    lines.append(_format_repeated(payload.get("repeatedRemovals")))
    lines.append("")
    lines.append("## Added more than once")
    lines.append("")
    lines.append(_format_repeated(payload.get("repeatedAdditions")))
    lines.append("")
    return "\n".join(lines)


def _median(lengths: tuple[int, ...]) -> float | int | None:
    value = weighted_median([(length, 1.0) for length in lengths])
    if value is None:
        return None
    if float(value).is_integer():
        return int(value)
    return round(float(value), 4)


def _rate(count: int, words: int) -> float | None:
    if words <= 0:
        return None
    return round(1000.0 * count / words, 4)


def _trailing_newline(text: str) -> str:
    if text.endswith("\n"):
        return text
    return text + "\n"


def _word_entries(value: object) -> list[tuple[str, int]]:
    if not isinstance(value, list):
        return []
    entries: list[tuple[str, int]] = []
    for item in value:
        if not isinstance(item, dict):
            continue
        word = item.get("word")
        count = item.get("count")
        if isinstance(word, str) and isinstance(count, int):
            entries.append((word, count))
    return entries


def _repeated(counts: Counter[str]) -> dict[str, int]:
    items = [(word, count) for word, count in counts.items() if count >= 2]
    items.sort(key=lambda item: (-item[1], item[0]))
    return {word: count for word, count in items}


def _format_words(value: object) -> str:
    entries = _word_entries(value)
    if not entries:
        return "none"
    return ", ".join(f"{word} ({count})" for word, count in entries[:16])


def _format_number(value: object) -> str:
    if isinstance(value, bool) or value is None:
        return "n/a"
    if isinstance(value, float):
        return f"{value:.4f}".rstrip("0").rstrip(".")
    return str(value)


def _confirmation_note(value: object) -> str:
    match value:
        case "origin":
            return "the origin settles it"
        case "promoted":
            return "you promoted it"
        case _:
            return "not confirmed as yours"


def _format_moves(value: object) -> str:
    if not isinstance(value, list) or not value:
        return "none"
    spans = [item for item in value if isinstance(item, dict)]
    tokens = sum(int(item.get("tokens", 0)) for item in spans)
    previews = [f"\"{item.get('text')}\"" for item in spans[:3] if item.get("text")]
    detail = f": {', '.join(previews)}" if previews else ""
    return f"{tokens} tokens in {len(spans)} span(s){detail}"


def _format_structure(value: object) -> str:
    if not isinstance(value, list) or not value:
        return "none"
    parts: list[str] = []
    for item in value:
        if not isinstance(item, dict):
            continue
        kind = item.get("kind")
        if kind == "punctuation":
            if item.get("change") == "order":
                parts.append("punctuation order")
            else:
                parts.append(f"{item.get('mark')} {item.get('before')} → {item.get('after')}")
        elif kind == "paragraphs":
            parts.append(f"paragraphs {item.get('before')} → {item.get('after')}")
        elif kind == "paragraph" and item.get("change") == "moved":
            parts.append(
                f"paragraph {item.get('from')} → {item.get('to')}: \"{item.get('preview')}\""
            )
    return "; ".join(parts) if parts else "none"


def _format_repeated(value: object) -> str:
    if not isinstance(value, dict) or not value:
        return "none yet"
    items = [(str(word), count) for word, count in value.items() if isinstance(count, int)]
    items.sort(key=lambda item: (-item[1], item[0]))
    return ", ".join(f"{word} ({count})" for word, count in items)
