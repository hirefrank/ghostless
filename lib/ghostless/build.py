"""Compile weighted voice statistics and render the local voice guide."""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path

from ghostless.checks import checks_summary
from ghostless.guide import render_guide
from ghostless.io import atomic_write_json, atomic_write_text, utc_now_iso
from ghostless.learn import refresh_edit_summary
from ghostless.project import NOTE_FILES, SKIP_NAMES, TEXT_SUFFIXES, discover, load_manifest, load_notes
from ghostless.rules import rules_summary
from ghostless.schema import CompiledStats
from ghostless.stats import compile_stats

_GOLD_INPUT_SUFFIXES: frozenset[str] = frozenset(TEXT_SUFFIXES) | {".json"}


def build_voice(root: Path) -> CompiledStats:
    """Compile all corpus sources in `root` and write voice-stats.json and voice-guide.md."""
    manifest = load_manifest(root)
    pieces = discover(root, manifest)
    edits = refresh_edit_summary(root)
    stats = compile_stats(
        pieces,
        weights=manifest["weights"],
        gold_voice_minimum=manifest["goldVoiceMinimum"],
        edits=edits,
    )
    stats["generatedAt"] = utc_now_iso()
    stats["failUnder"] = manifest["failUnder"]
    stats["overrides"] = dict(manifest["overrides"])
    stats["placeholder"] = manifest["placeholder"]
    # Derived snapshots. `rules.json` and `checks.json` stay the sources of truth;
    # build never writes them.
    stats["rules"] = rules_summary(root, edits=edits)
    stats["checks"] = checks_summary(root)
    notes = load_notes(root)
    stats["style"] = notes["style"]
    stats["audience"] = notes["audience"]
    atomic_write_json(root / "voice-stats.json", stats, ensure_ascii=False)
    atomic_write_text(root / "voice-guide.md", render_guide(stats))
    return stats


def stale_inputs(root: Path) -> list[str]:
    """Return sorted relative paths of tracked inputs newer than voice-stats.json."""
    stats_path = root / "voice-stats.json"
    if not stats_path.is_file():
        return []
    try:
        stats_mtime = stats_path.stat().st_mtime_ns
    except OSError:
        return []

    candidates: list[Path] = []
    fixed_names = ("manifest.json", "rules.json", "checks.json", *(name for _, name in NOTE_FILES))
    for name in fixed_names:
        candidate = root / name
        if candidate.is_file():
            candidates.append(candidate)

    prose_suffixes = frozenset(TEXT_SUFFIXES)
    candidates.extend(_iter_input_files(root / "corpus", root=root, suffixes=prose_suffixes))
    candidates.extend(_iter_input_files(root / "transcripts", root=root, suffixes=prose_suffixes))
    candidates.extend(_iter_input_files(root / "gold", root=root, suffixes=_GOLD_INPUT_SUFFIXES))

    stale: list[str] = []
    present: set[str] = set()
    for path in candidates:
        rel = path.relative_to(root).as_posix()
        present.add(rel)
        try:
            mtime = path.stat().st_mtime_ns
        except OSError:
            continue
        if mtime > stats_mtime:
            stale.append(rel)
    stale.extend(_deleted_inputs(root, present))
    return sorted(set(stale))


def _deleted_inputs(root: Path, present: set[str]) -> list[str]:
    """Inputs recorded by the last build that are no longer on disk.

    `stale_inputs` otherwise only notices files that are still there and newer
    than voice-stats.json, so a deletion looks clean.
    """
    stats = _read_stats(root / "voice-stats.json")
    if stats is None:
        return []
    missing: list[str] = []
    for rel in _recorded_paths(stats):
        if rel in present or (root / rel).is_file():
            continue
        missing.append(rel)
    for key, name in NOTE_FILES:
        recorded = stats.get(key)
        if isinstance(recorded, str) and recorded.strip() and name not in present:
            missing.append(name)
    if _has_decisions(stats.get("rules")) and "rules.json" not in present:
        missing.append("rules.json")
    if _has_decisions(stats.get("checks")) and "checks.json" not in present:
        missing.append("checks.json")
    return missing


def _read_stats(path: Path) -> dict[str, object] | None:
    try:
        data: object = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    if not isinstance(data, dict):
        return None
    return data


def _recorded_paths(stats: Mapping[str, object]) -> list[str]:
    files = stats.get("files")
    if not isinstance(files, list):
        return []
    paths: list[str] = []
    for row in files:
        if not isinstance(row, dict):
            continue
        rel = row.get("path")
        if isinstance(rel, str) and rel:
            paths.append(rel)
    return paths


def _has_decisions(block: object) -> bool:
    if not isinstance(block, dict):
        return False
    for key in ("confirmed", "rejected", "retired", "pending"):
        value = block.get(key)
        if isinstance(value, list | dict) and len(value) > 0:
            return True
    return False


def _iter_input_files(
    folder: Path,
    *,
    root: Path,
    suffixes: frozenset[str],
) -> list[Path]:
    if not folder.is_dir():
        return []
    found: list[Path] = []
    for path in folder.rglob("*"):
        if not path.is_file():
            continue
        rel_parts = path.relative_to(root).parts
        if any(part.startswith(".") for part in rel_parts):
            continue
        if path.name.lower() in SKIP_NAMES or path.suffix.lower() not in suffixes:
            continue
        found.append(path)
    return found
