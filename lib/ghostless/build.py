"""Compile weighted voice statistics and render the local voice guide."""

from __future__ import annotations

from pathlib import Path

from ghostless.checks import checks_summary
from ghostless.guide import render_guide
from ghostless.io import atomic_write_json, atomic_write_text, utc_now_iso
from ghostless.learn import refresh_edit_summary
from ghostless.project import discover, load_manifest, load_notes
from ghostless.rules import rules_summary
from ghostless.schema import CompiledStats
from ghostless.stats import compile_stats


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
