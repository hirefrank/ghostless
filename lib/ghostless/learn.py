"""Store a before/after edit as gold and summarize what changed."""

from __future__ import annotations

import json
import re
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

from ghostless.paths import ProjectError
from ghostless.project import parse_origin
from ghostless.stats import weighted_median
from ghostless.text import measure, track_token, word_tokens

SLUG_RE = re.compile(r"[^a-z0-9]+")
DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}")


def diff_texts(before: str, after: str) -> dict[str, object]:
    before_measured = measure(before)
    after_measured = measure(after)
    before_counts = Counter(word_tokens(before))
    after_counts = Counter(word_tokens(after))
    removed_tokens = 0
    added_tokens = 0
    removed_words: list[dict[str, object]] = []
    added_words: list[dict[str, object]] = []
    for token in set(before_counts) | set(after_counts):
        delta = before_counts[token] - after_counts[token]
        if delta > 0:
            removed_tokens += delta
            if track_token(token):
                removed_words.append({"word": token, "count": delta})
        elif delta < 0:
            added_tokens += -delta
            if track_token(token):
                added_words.append({"word": token, "count": -delta})
    removed_words.sort(key=lambda item: (-int(item["count"]), str(item["word"])))
    added_words.sort(key=lambda item: (-int(item["count"]), str(item["word"])))
    return {
        "wordsBefore": before_measured.words,
        "wordsAfter": after_measured.words,
        "wordsRemoved": removed_tokens,
        "wordsAdded": added_tokens,
        "medianSentenceLengthBefore": _median(before_measured.sentence_lengths),
        "medianSentenceLengthAfter": _median(after_measured.sentence_lengths),
        "contractionsPer1kBefore": _rate(before_measured.contractions, before_measured.words),
        "contractionsPer1kAfter": _rate(after_measured.contractions, after_measured.words),
        "removedWords": removed_words,
        "addedWords": added_words,
    }


def learn_edit(
    root: Path,
    *,
    before: Path,
    after: Path,
    section: str | None,
    origin: str,
    date: str | None = None,
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
    slug = _slug(date_text, section)
    destination = _fresh_dir(root / "gold", slug)
    before_text = before.read_text(encoding="utf-8")
    after_text = after.read_text(encoding="utf-8")
    destination.mkdir(parents=True, exist_ok=False)
    (destination / "before.md").write_text(_trailing_newline(before_text), encoding="utf-8")
    (destination / "after.md").write_text(_trailing_newline(after_text), encoding="utf-8")
    summary = diff_texts(before_text, after_text)
    meta: dict[str, object] = {
        "date": date_text,
        "section": section,
        "origin": checked_origin,
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
    return {"slug": destination.name, "path": destination.relative_to(root).as_posix(), "meta": meta, "edits": edits}


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
            learns.append(
                {
                    "slug": meta_path.parent.name,
                    "date": meta.get("date"),
                    "section": meta.get("section"),
                    "origin": meta.get("origin"),
                    **summary,
                }
            )
    removals: Counter[str] = Counter()
    additions: Counter[str] = Counter()
    for item in learns:
        for entry in _word_entries(item.get("removedWords")):
            removals[entry[0]] += entry[1]
        for entry in _word_entries(item.get("addedWords")):
            additions[entry[0]] += entry[1]
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
        lines.append(f"- Section: {item.get('section') or 'unsectioned'}")
        lines.append(f"- Date: {item.get('date')}")
        lines.append(
            f"- Words: {item.get('wordsBefore')} → {item.get('wordsAfter')} "
            f"(removed {item.get('wordsRemoved')} tokens, added {item.get('wordsAdded')})"
        )
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


def _slug(date_text: str, section: str | None) -> str:
    slug = date_text
    if section:
        extra = SLUG_RE.sub("-", section.casefold()).strip("-")
        if extra:
            slug = f"{date_text}-{extra}"
    return slug


def _fresh_dir(parent: Path, slug: str) -> Path:
    candidate = parent / slug
    if not candidate.exists():
        return candidate
    number = 2
    while (parent / f"{slug}-{number}").exists():
        number += 1
    return parent / f"{slug}-{number}"


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


def _format_repeated(value: object) -> str:
    if not isinstance(value, dict) or not value:
        return "none yet"
    items = [(str(word), count) for word, count in value.items() if isinstance(count, int)]
    items.sort(key=lambda item: (-item[1], item[0]))
    return ", ".join(f"{word} ({count})" for word, count in items)
