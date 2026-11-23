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
from collections.abc import Mapping
from pathlib import Path

from ghostless.checks import new_pair_checks, prepare_checks
from ghostless.io import atomic_write_json, atomic_write_text, sha256_text, text_preview, utc_today
from ghostless.paths import ProjectError
from ghostless.project import (
    fresh_directory,
    pair_file_path,
    parse_confirmation,
    parse_origin,
    placement_for,
    provenance_json,
    read_provenance,
    require_contained_path,
    slug_for,
    validate_date,
)
from ghostless.schema import (
    DiffSummary,
    EditLearnEntry,
    EditSummary,
    MovedSpan,
    TokenAlignment,
    WordCount,
)
from ghostless.stats import weighted_median
from ghostless.text import STOPWORDS, measure, normalize, paragraphs, track_token, word_tokens

DIFF_VERSION = 2
DIFF_SUMMARY_KEYS = (
    "wordsBefore",
    "wordsAfter",
    "wordsRemoved",
    "wordsAdded",
    "medianSentenceLengthBefore",
    "medianSentenceLengthAfter",
    "contractionsPer1kBefore",
    "contractionsPer1kAfter",
    "removedWords",
    "addedWords",
    "tokenEdits",
    "unchangedTokens",
    "deletions",
    "insertions",
    "replacements",
    "movedTokens",
    "movedSpans",
    "editRate",
    "editRateDenominator",
    "paragraphsBefore",
    "paragraphsAfter",
    "sentencesBefore",
    "sentencesAfter",
    "structuralChanges",
    "structuralChangeCount",
)

# Below this edit rate a final is mostly the model's prose. Promotion still
# works, but the CLI says so before the writer counts it as gold.
MOSTLY_MODEL_RATE = 0.25

# Characters worth reporting when an edit changes structure but not words.
# Apostrophes matter for contractions ("dont" -> "don't"); markdown marks
# matter because a writer may add or drop emphasis in a revision.
PUNCTUATION = ".,;:!?…—–-()[]{}\"'*_#/"
PUNCTUATION_RE = re.compile("[" + re.escape(PUNCTUATION) + "]")


def diff_texts(before: str, after: str) -> DiffSummary:
    before_measured = measure(before)
    after_measured = measure(after)
    before_counts = Counter(word_tokens(before))
    after_counts = Counter(word_tokens(after))
    removed_words: list[WordCount] = []
    added_words: list[WordCount] = []
    for token in set(before_counts) | set(after_counts):
        delta = before_counts[token] - after_counts[token]
        if delta > 0:
            if track_token(token):
                removed_words.append({"word": token, "count": delta})
        elif delta < 0:
            if track_token(token):
                added_words.append({"word": token, "count": -delta})
    removed_words.sort(key=lambda item: (-item["count"], item["word"]))
    added_words.sort(key=lambda item: (-item["count"], item["word"]))

    sequence = _align_tokens(before, after)
    structural_changes, paragraphs_before, paragraphs_after = _structural_changes(before, after)
    token_edits = (
        sequence["deletions"]
        + sequence["insertions"]
        + sequence["replacements"]
        + sequence["movedTokens"]
    )
    unchanged_tokens = sequence["unchangedTokens"]
    edit_denominator = token_edits + unchanged_tokens
    edit_rate = round(token_edits / edit_denominator, 4) if edit_denominator else 0.0

    return {
        "wordsBefore": before_measured.words,
        "wordsAfter": after_measured.words,
        # Sequence-aware change counts. A replacement removes one token and
        # adds another, so it appears once on each side.
        "wordsRemoved": sequence["deletions"] + sequence["replacements"],
        "wordsAdded": sequence["insertions"] + sequence["replacements"],
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


def _align_tokens(before: str, after: str) -> TokenAlignment:
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

    # Match longest contiguous multi-token spans between deleted and inserted runs.
    # Requiring multi-token spans prevents isolated words or stopword pairs
    # across unrelated edits from inflating move counts, while greedy splitting
    # still catches swapped clauses that ordered difflib alignment misses.
    moved_spans = _match_moved_spans(delete_blocks, insert_blocks)
    moved_tokens = sum(span["tokens"] for span in moved_spans)

    return {
        "deletions": deletions - moved_tokens,
        "insertions": insertions - moved_tokens,
        "replacements": replacements,
        "movedTokens": moved_tokens,
        "movedSpans": moved_spans,
        "unchangedTokens": unchanged,
    }


def _valid_move_span(span: list[str]) -> bool:
    """Require at least 3 tokens, or 2 tokens containing a non-stopword."""
    return len(span) >= 3 or (len(span) == 2 and any(token not in STOPWORDS for token in span))


def _best_common_span(
    del_slices: list[tuple[int, list[str]]],
    ins_slices: list[tuple[int, list[str]]],
) -> tuple[int, int, int, int, int] | None:
    """Return the longest valid contiguous common slice with deterministic tie-breaking."""
    ins_positions: list[dict[str, list[int]]] = []
    for _, ins_tokens in ins_slices:
        pos_map: dict[str, list[int]] = {}
        for pos, token in enumerate(ins_tokens):
            pos_map.setdefault(token, []).append(pos)
        ins_positions.append(pos_map)

    best: tuple[int, int, int, int, int] | None = None
    best_key: tuple[int, int, int] | None = None

    for del_idx, (del_start, del_tokens) in enumerate(del_slices):
        if len(del_tokens) < 2:
            continue
        for ins_idx, (ins_start, ins_tokens) in enumerate(ins_slices):
            if len(ins_tokens) < 2:
                continue
            if best_key is not None and min(len(del_tokens), len(ins_tokens)) < -best_key[0]:
                continue
            pos_map = ins_positions[ins_idx]
            prev: dict[int, int] = {}
            for i, token in enumerate(del_tokens):
                positions = pos_map.get(token)
                if not positions:
                    prev = {}
                    continue
                curr: dict[int, int] = {}
                for j in positions:
                    length = prev.get(j - 1, 0) + 1
                    curr[j] = length
                    if length >= 2:
                        del_off = i - length + 1
                        ins_off = j - length + 1
                        if _valid_move_span(del_tokens[del_off : i + 1]):
                            key = (-length, del_start + del_off, ins_start + ins_off)
                            if best_key is None or key < best_key:
                                best_key = key
                                best = (del_idx, del_off, ins_idx, ins_off, length)
                prev = curr
    return best


def _split_slice(
    slices: list[tuple[int, list[str]]],
    index: int,
    offset: int,
    length: int,
) -> None:
    """Replace a slice with its non-empty prefix and suffix around a match."""
    start, tokens = slices[index]
    replacement: list[tuple[int, list[str]]] = []
    if offset > 0:
        replacement.append((start, tokens[:offset]))
    tail = offset + length
    if tail < len(tokens):
        replacement.append((start + tail, tokens[tail:]))
    slices[index : index + 1] = replacement


def _match_moved_spans(
    delete_blocks: list[tuple[int, list[str]]],
    insert_blocks: list[tuple[int, list[str]]],
) -> list[MovedSpan]:
    """Match contiguous moved spans iteratively from longest to shortest."""
    del_slices = [(start, list(tokens)) for start, tokens in delete_blocks if tokens]
    ins_slices = [(start, list(tokens)) for start, tokens in insert_blocks if tokens]
    spans: list[MovedSpan] = []

    while del_slices and ins_slices:
        match = _best_common_span(del_slices, ins_slices)
        if match is None:
            break
        del_idx, del_off, ins_idx, ins_off, length = match
        del_start, del_tokens = del_slices[del_idx]
        ins_start, _ = ins_slices[ins_idx]
        span = del_tokens[del_off : del_off + length]
        spans.append(
            {
                "tokens": length,
                "text": " ".join(span),
                "from": del_start + del_off,
                "to": ins_start + ins_off,
            }
        )
        _split_slice(del_slices, del_idx, del_off, length)
        _split_slice(ins_slices, ins_idx, ins_off, length)

    spans.sort(key=lambda item: (item["from"], item["to"]))
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
                    "preview": text_preview(before_paragraphs[from_index]),
                }
            )
            break
    return moves


def find_existing_learn_pair(root: Path, before: str, after: str) -> Path | None:
    """Return an existing learned pair directory whose before/after texts match."""
    folder = root / "gold"
    if not folder.is_dir():
        return None
    before_norm = before.rstrip("\n")
    after_norm = after.rstrip("\n")
    for existing in sorted(folder.glob("*/meta.json")):
        pair_dir = existing.parent
        prior_before = pair_dir / "before.md"
        prior_after = pair_dir / "after.md"
        if prior_before.is_file() and prior_after.is_file():
            if (
                prior_before.read_text(encoding="utf-8").rstrip("\n") == before_norm
                and prior_after.read_text(encoding="utf-8").rstrip("\n") == after_norm
            ):
                return pair_dir
    return None


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
    checks: list[str] | None = None,
) -> dict[str, object]:
    if not before.is_file():
        raise ProjectError(f"Before file not found: {before}")
    if not after.is_file():
        raise ProjectError(f"After file not found: {after}")
    if before.resolve() == after.resolve():
        raise ProjectError("Before and after are the same file.")
    checked_origin = parse_origin(origin, path="--origin")
    date_text = date or utc_today()
    validate_date(date_text, label="--date")
    slug = slug_for(date_text, section)
    before_text = before.read_text(encoding="utf-8")
    after_text = after.read_text(encoding="utf-8")
    duplicate = find_existing_learn_pair(root, before_text, after_text)
    if duplicate is not None:
        raise ProjectError(f"This edit pair was already learned in {duplicate.name}.")
    prepared = prepare_checks(root, before_text, after_text, checks) if checks else []
    destination = fresh_directory(root / "gold", slug)
    destination.mkdir(parents=True, exist_ok=False)
    stored_before = _trailing_newline(before_text)
    stored_after = _trailing_newline(after_text)
    atomic_write_text(destination / "before.md", stored_before)
    atomic_write_text(destination / "after.md", stored_after)
    summary = diff_texts(stored_before, stored_after)
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
        "diffVersion": DIFF_VERSION,
        "beforeSha256": sha256_text(stored_before),
        "afterSha256": sha256_text(stored_after),
    }
    stored_checks = new_pair_checks(prepared)
    if stored_checks:
        meta["checks"] = stored_checks
    meta.update(summary)
    atomic_write_json(destination / "meta.json", meta, ensure_ascii=False)
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
        "checks": prepared,
        "warning": _mostly_model_warning(
            summary if placement == "gold" and checked_origin == "ai-first" else None
        ),
    }


def promote_final(root: Path, slug: str, *, gold: bool = True) -> dict[str, object]:
    """Deliberately move a learned final into (or back out of) gold.

    Only provenance changes: `before.md`, `after.md`, and the edit receipt are
    untouched, because the writer's changes are evidence either way.
    """
    if not isinstance(slug, str) or not slug or slug in {".", ".."} or "/" in slug or "\\" in slug:
        raise ProjectError(f"Invalid learned pair slug: {slug!r}")
    pair_dir = require_contained_path(root / "gold", root / "gold" / slug, label="slug")
    meta_path = pair_dir / "meta.json"
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
    atomic_write_json(meta_path, meta, ensure_ascii=False)
    placement = placement_for(read_provenance(meta, path=str(meta_path)))
    edits = refresh_edit_summary(root)
    summary = _receipt_summary(
        pair_file_path(meta, meta_path, "beforeFile", "before.md", base=pair_dir),
        pair_file_path(meta, meta_path, "afterFile", "after.md", base=pair_dir),
    )
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


def _receipt_summary(before: Path, after: Path) -> DiffSummary | None:
    if not before.is_file() or not after.is_file():
        return None
    return diff_texts(
        before.read_text(encoding="utf-8"),
        after.read_text(encoding="utf-8"),
    )


def _mostly_model_warning(summary: Mapping[str, object] | None) -> str | None:
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


def _cached_int(value: object) -> int | None:
    if type(value) is not int:
        return None
    return value


def _cached_float(value: object) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)


def _cached_opt_num(value: object) -> tuple[bool, float | int | None]:
    if value is None:
        return True, None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return False, None
    return True, value


def _cached_opt_float(value: object) -> tuple[bool, float | None]:
    if value is None:
        return True, None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return False, None
    return True, float(value)


def _cached_word_counts(value: object) -> list[WordCount] | None:
    if not isinstance(value, list):
        return None
    items: list[WordCount] = []
    for entry in value:
        if not isinstance(entry, dict):
            return None
        word = entry.get("word")
        count = entry.get("count")
        if not isinstance(word, str) or type(count) is not int:
            return None
        items.append({"word": word, "count": count})
    return items


def _cached_moved_spans(value: object) -> list[MovedSpan] | None:
    if not isinstance(value, list):
        return None
    spans: list[MovedSpan] = []
    for entry in value:
        if not isinstance(entry, dict):
            return None
        tokens = entry.get("tokens")
        text = entry.get("text")
        src = entry.get("from")
        dst = entry.get("to")
        if type(tokens) is not int or not isinstance(text, str) or type(src) is not int or type(dst) is not int:
            return None
        spans.append({"tokens": tokens, "text": text, "from": src, "to": dst})
    return spans


def _cached_structural_changes(value: object) -> list[dict[str, object]] | None:
    if not isinstance(value, list):
        return None
    items: list[dict[str, object]] = []
    for entry in value:
        if not isinstance(entry, dict):
            return None
        items.append({str(key): item for key, item in entry.items()})
    return items


def _cached_summary(
    meta: Mapping[str, object],
    before_text: str,
    after_text: str,
) -> DiffSummary | None:
    """Return cached diff fields when version, SHA-256 digests, and keys match."""
    version = meta.get("diffVersion")
    if type(version) is not int or version != DIFF_VERSION:
        return None
    if meta.get("beforeSha256") != sha256_text(before_text):
        return None
    if meta.get("afterSha256") != sha256_text(after_text):
        return None
    if not all(key in meta for key in DIFF_SUMMARY_KEYS):
        return None
    words_before = _cached_int(meta["wordsBefore"])
    words_after = _cached_int(meta["wordsAfter"])
    words_removed = _cached_int(meta["wordsRemoved"])
    words_added = _cached_int(meta["wordsAdded"])
    ok_med_before, med_before = _cached_opt_num(meta["medianSentenceLengthBefore"])
    ok_med_after, med_after = _cached_opt_num(meta["medianSentenceLengthAfter"])
    ok_con_before, con_before = _cached_opt_float(meta["contractionsPer1kBefore"])
    ok_con_after, con_after = _cached_opt_float(meta["contractionsPer1kAfter"])
    removed_words = _cached_word_counts(meta["removedWords"])
    added_words = _cached_word_counts(meta["addedWords"])
    token_edits = _cached_int(meta["tokenEdits"])
    unchanged = _cached_int(meta["unchangedTokens"])
    deletions = _cached_int(meta["deletions"])
    insertions = _cached_int(meta["insertions"])
    replacements = _cached_int(meta["replacements"])
    moved_tokens = _cached_int(meta["movedTokens"])
    moved_spans = _cached_moved_spans(meta["movedSpans"])
    edit_rate = _cached_float(meta["editRate"])
    edit_denom = _cached_int(meta["editRateDenominator"])
    paragraphs_before = _cached_int(meta["paragraphsBefore"])
    paragraphs_after = _cached_int(meta["paragraphsAfter"])
    sentences_before = _cached_int(meta["sentencesBefore"])
    sentences_after = _cached_int(meta["sentencesAfter"])
    structural = _cached_structural_changes(meta["structuralChanges"])
    structural_count = _cached_int(meta["structuralChangeCount"])
    if (
        words_before is None
        or words_after is None
        or words_removed is None
        or words_added is None
        or not ok_med_before
        or not ok_med_after
        or not ok_con_before
        or not ok_con_after
        or removed_words is None
        or added_words is None
        or token_edits is None
        or unchanged is None
        or deletions is None
        or insertions is None
        or replacements is None
        or moved_tokens is None
        or moved_spans is None
        or edit_rate is None
        or edit_denom is None
        or paragraphs_before is None
        or paragraphs_after is None
        or sentences_before is None
        or sentences_after is None
        or structural is None
        or structural_count is None
    ):
        return None
    return {
        "wordsBefore": words_before,
        "wordsAfter": words_after,
        "wordsRemoved": words_removed,
        "wordsAdded": words_added,
        "medianSentenceLengthBefore": med_before,
        "medianSentenceLengthAfter": med_after,
        "contractionsPer1kBefore": con_before,
        "contractionsPer1kAfter": con_after,
        "removedWords": removed_words,
        "addedWords": added_words,
        "tokenEdits": token_edits,
        "unchangedTokens": unchanged,
        "deletions": deletions,
        "insertions": insertions,
        "replacements": replacements,
        "movedTokens": moved_tokens,
        "movedSpans": moved_spans,
        "editRate": edit_rate,
        "editRateDenominator": edit_denom,
        "paragraphsBefore": paragraphs_before,
        "paragraphsAfter": paragraphs_after,
        "sentencesBefore": sentences_before,
        "sentencesAfter": sentences_after,
        "structuralChanges": structural,
        "structuralChangeCount": structural_count,
    }


def refresh_edit_summary(root: Path) -> EditSummary:
    learns: list[EditLearnEntry] = []
    folder = root / "gold"
    if folder.is_dir():
        for meta_path in sorted(folder.glob("*/meta.json")):
            pair_dir = require_contained_path(folder, meta_path.parent, label=str(meta_path))
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            if not isinstance(meta, dict):
                continue
            before_path = pair_file_path(meta, meta_path, "beforeFile", "before.md", base=pair_dir)
            after_path = pair_file_path(meta, meta_path, "afterFile", "after.md", base=pair_dir)
            if not before_path.is_file() or not after_path.is_file():
                continue
            before_text = before_path.read_text(encoding="utf-8")
            after_text = after_path.read_text(encoding="utf-8")
            summary = _cached_summary(meta, before_text, after_text)
            if summary is None:
                summary = diff_texts(before_text, after_text)
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
                    "wordsBefore": summary["wordsBefore"],
                    "wordsAfter": summary["wordsAfter"],
                    "wordsRemoved": summary["wordsRemoved"],
                    "wordsAdded": summary["wordsAdded"],
                    "medianSentenceLengthBefore": summary["medianSentenceLengthBefore"],
                    "medianSentenceLengthAfter": summary["medianSentenceLengthAfter"],
                    "contractionsPer1kBefore": summary["contractionsPer1kBefore"],
                    "contractionsPer1kAfter": summary["contractionsPer1kAfter"],
                    "removedWords": summary["removedWords"],
                    "addedWords": summary["addedWords"],
                    "tokenEdits": summary["tokenEdits"],
                    "unchangedTokens": summary["unchangedTokens"],
                    "deletions": summary["deletions"],
                    "insertions": summary["insertions"],
                    "replacements": summary["replacements"],
                    "movedTokens": summary["movedTokens"],
                    "movedSpans": summary["movedSpans"],
                    "editRate": summary["editRate"],
                    "editRateDenominator": summary["editRateDenominator"],
                    "paragraphsBefore": summary["paragraphsBefore"],
                    "paragraphsAfter": summary["paragraphsAfter"],
                    "sentencesBefore": summary["sentencesBefore"],
                    "sentencesAfter": summary["sentencesAfter"],
                    "structuralChanges": summary["structuralChanges"],
                    "structuralChangeCount": summary["structuralChangeCount"],
                }
            )
    removals: Counter[str] = Counter()
    additions: Counter[str] = Counter()
    for item in learns:
        removals.update({entry["word"] for entry in item["removedWords"]})
        additions.update({entry["word"] for entry in item["addedWords"]})
    payload: EditSummary = {
        "learns": learns,
        "repeatedRemovals": _repeated(removals),
        "repeatedAdditions": _repeated(additions),
    }
    summary_json = root / "edits-summary.json"
    summary_md = root / "edits-summary.md"
    if learns:
        atomic_write_json(summary_json, payload, ensure_ascii=False)
        atomic_write_text(summary_md, render_edits_markdown(payload))
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


def render_edits_markdown(payload: Mapping[str, object]) -> str:
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
    tokens = sum(
        item["tokens"]
        for item in spans
        if isinstance(item.get("tokens"), int) and not isinstance(item.get("tokens"), bool)
    )
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
