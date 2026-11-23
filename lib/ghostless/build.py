"""Compile weighted voice statistics and render the local voice guide."""

from __future__ import annotations

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
    for path in candidates:
        try:
            mtime = path.stat().st_mtime_ns
        except OSError:
            continue
        if mtime > stats_mtime:
            stale.append(path.relative_to(root).as_posix())
    return sorted(stale)


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
