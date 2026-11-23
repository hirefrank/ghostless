"""Provenance tracking, edited-pool capping, and promotion tests."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.build import build_voice  # noqa: E402
from ghostless.cli import main  # noqa: E402
from ghostless.learn import learn_edit  # noqa: E402
from ghostless.project import discover, init_project, load_manifest  # noqa: E402


class ProvenanceTests(unittest.TestCase):
    def test_ai_first_final_is_edited_not_gold(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            _set_gold_minimum(project, 1)
            result = _learn_pair(
                Path(tmp),
                project,
                "A draft leans on the tapestry of ideas for a while.",
                "A draft leans on the tapestry of ideas for a while, mostly.",
                date="2026-01-02",
                section="notes",
                model="sample-model",
                tool="sample-tool",
            )
            self.assertEqual(result["tier"], "edited")
            self.assertEqual(result["confirmation"], "none")
            self.assertEqual(result["warning"], None)

            slug = str(result["slug"])
            meta = json.loads(
                (project / "gold" / slug / "meta.json").read_text(encoding="utf-8")
            )
            # Provenance is its own block, mirrored top-level for v0.1 readers.
            self.assertEqual(meta["origin"], "ai-first")
            self.assertEqual(
                meta["provenance"],
                {
                    "origin": "ai-first",
                    "confirmation": "none",
                    "model": "sample-model",
                    "tool": "sample-tool",
                },
            )

            pieces = discover(project, load_manifest(project))
            self.assertEqual(len(pieces), 1)
            self.assertEqual(pieces[0].tier, "edited")
            self.assertEqual(pieces[0].origin, "ai-first")
            self.assertEqual(pieces[0].confirmation, "none")
            self.assertEqual(pieces[0].model, "sample-model")

            stats = build_voice(project)
            # One AI-first final cannot reach gold, even at a threshold of 1.
            self.assertEqual(stats["goldPieces"], 0)
            self.assertFalse(stats["bronzeFormatOnly"])
            self.assertEqual(stats["counts"]["prose"]["gold"], 0)
            self.assertEqual(stats["counts"]["prose"]["edited"], 1)
            self.assertEqual(stats["provenance"]["ai-first"], 1)
            self.assertEqual(stats["files"][0]["model"], "sample-model")
            self.assertNotIn("model", stats["weighted"]["metrics"])
            self.assertTrue(
                any("measured as edited" in item for item in stats["warnings"])
            )

    def test_writer_first_final_is_gold(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            result = _learn_pair(
                Path(tmp),
                project,
                "I write at the desk.",
                "I write at the desk, and I stop when the coffee is cold.",
                origin="writer-first",
                date="2026-01-02",
            )
            self.assertEqual(result["tier"], "gold")
            self.assertEqual(result["confirmation"], "origin")
            stats = build_voice(project)
            self.assertEqual(stats["goldPieces"], 1)
            self.assertEqual(stats["counts"]["prose"]["edited"], 0)

    def test_many_lightly_edited_ai_drafts_cannot_overwhelm_an_unaided_corpus(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            _set_gold_minimum(project, 10)
            (project / "corpus" / "gold" / "desk.md").write_text(
                "I write at the desk.\n\nI stop when the coffee is cold.\n\nI walk to the window.\n",
                encoding="utf-8",
            )
            baseline = build_voice(project)
            self.assertEqual(baseline["goldPieces"], 1)

            for index in range(20):
                draft = (
                    f"Delve into the tapestry of ideas in draft number {index}. "
                    "Furthermore it is worth noting that the frame is robust."
                )
                # A light edit: the model's vocabulary and cadence survive.
                edited = draft.replace("robust", "quite robust")
                _learn_pair(
                    Path(tmp),
                    project,
                    draft,
                    edited,
                    date=f"2026-02-{index + 1:02d}",
                    section="notes",
                )

            stats = build_voice(project)
            self.assertEqual(stats["goldPieces"], 1)
            self.assertFalse(stats["bronzeFormatOnly"])
            self.assertEqual(stats["counts"]["prose"]["edited"], 20)
            self.assertEqual(stats["edited"]["pieces"], 20)
            # Twenty drafts at weight 0.5 would be 10; the cap holds them to
            # a quarter of the single gold piece's weight of 3.
            self.assertLessEqual(
                stats["edited"]["countedWeight"],
                stats["edited"]["maxShare"] * 3 + 1e-9,
            )

            # Rhythm, diction, and the ban list are untouched by model prose.
            for key in (
                "medianSentenceLength",
                "contractionsPer1k",
                "iPer1k",
                "emDashesPer1k",
                "questionsPer1k",
                "openerShare",
            ):
                self.assertEqual(
                    stats["weighted"]["metrics"][key],
                    baseline["weighted"]["metrics"][key],
                    key,
                )
            self.assertEqual(stats["weighted"]["banList"], baseline["weighted"]["banList"])
            self.assertIn("delve", stats["weighted"]["banList"])
            self.assertNotIn("delve", stats["weighted"]["tellsInUse"])
            # Paragraph shape is the only thing edited finals inform, and the
            # writer's three paragraphs still set the median.
            self.assertEqual(
                stats["weighted"]["metrics"]["medianParagraphLength"],
                baseline["weighted"]["metrics"]["medianParagraphLength"],
            )
            self.assertTrue(
                any("capped at 25%" in item for item in stats["warnings"])
            )

    def test_promote_moves_a_final_to_gold_with_visible_consequences(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            _set_gold_minimum(project, 1)
            result = _learn_pair(
                Path(tmp),
                project,
                "Delve into the tapestry of ideas for a while.",
                "Delve into the tapestry of ideas for a while!",
                date="2026-01-02",
                section="notes",
            )
            slug = str(result["slug"])
            self.assertEqual(result["tier"], "edited")

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = main(["promote", slug, "--project", str(project)])
            self.assertEqual(code, 0)
            text = stdout.getvalue()
            self.assertIn("edited → gold", text)
            # A 1-punctuation edit is mostly model prose; say so.
            self.assertIn("warning:", text)
            self.assertIn("gold prose pieces: 0 → 1 (threshold 1; bronze is format-only)", text)
            self.assertIn("counts at gold weight", text)

            stats = build_voice(project)
            self.assertEqual(stats["goldPieces"], 1)
            self.assertTrue(stats["bronzeFormatOnly"])
            self.assertEqual(stats["provenance"]["ai-first"], 1)
            learned = [piece for piece in discover(project, load_manifest(project)) if piece.learned]
            self.assertEqual(learned[0].tier, "gold")
            self.assertEqual(learned[0].confirmation, "promoted")

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = main(["promote", slug, "--demote", "--project", str(project)])
            self.assertEqual(code, 0)
            self.assertIn("gold → edited", stdout.getvalue())
            restored = build_voice(project)
            self.assertEqual(restored["goldPieces"], 0)
            self.assertFalse(restored["bronzeFormatOnly"])

    def test_learn_promote_flag_warns_about_model_prose(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            result = _learn_pair(
                Path(tmp),
                project,
                "A model wrote this sentence for the draft.",
                "A model wrote this sentence for the draft!",
                date="2026-01-02",
                promote=True,
            )
            self.assertEqual(result["tier"], "gold")
            self.assertEqual(result["confirmation"], "promoted")
            self.assertIsInstance(result["warning"], str)
            self.assertIn("mostly model prose", str(result["warning"]))

    def test_v01_pairs_get_a_default_placement(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            _write_legacy_pair(project, "2026-01-02-notes", origin="ai-first")
            _write_legacy_pair(project, "2026-01-03-notes", origin="writer-first")
            _write_legacy_pair(project, "2026-01-04-notes", origin=None)

            pieces = discover(project, load_manifest(project))
            tiers = {piece.path: piece.tier for piece in pieces}
            self.assertEqual(tiers["gold/2026-01-02-notes/after.md"], "edited")
            self.assertEqual(tiers["gold/2026-01-03-notes/after.md"], "gold")
            # No origin means unknown, so the conservative default is edited.
            self.assertEqual(tiers["gold/2026-01-04-notes/after.md"], "edited")
            self.assertEqual(
                {piece.path: piece.origin for piece in pieces}[
                    "gold/2026-01-04-notes/after.md"
                ],
                None,
            )

            stats = build_voice(project)
            self.assertEqual(stats["goldPieces"], 1)
            self.assertEqual(stats["counts"]["prose"]["edited"], 2)
            self.assertEqual(stats["provenance"]["unknown"], 1)
            self.assertTrue(
                any("no recorded origin" in item for item in stats["warnings"])
            )

    def test_a_section_of_only_edited_finals_is_not_a_judge_target(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            (project / "corpus" / "gold" / "desk.md").write_text(
                "I write at the desk.\n", encoding="utf-8"
            )
            _learn_pair(
                Path(tmp),
                project,
                "A model draft about notes.",
                "A model draft about notes!",
                date="2026-01-02",
                section="notes",
            )
            stats = build_voice(project)
            self.assertEqual(stats["sections"], {})
            draft = Path(tmp) / "draft.md"
            draft.write_text("An ordinary draft with a few sentences.", encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(
                    ["judge", str(draft), "--section", "notes", "--project", str(project)]
                )
            self.assertEqual(code, 2)
            self.assertIn("No targets for section 'notes'", stderr.getvalue())

    def test_edited_pairs_still_supply_edit_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            _learn_pair(
                Path(tmp),
                project,
                "We should delve deeper into the tapestry.",
                "I stop here.",
                date="2026-01-02",
            )
            stats = build_voice(project)
            learns = stats["edits"]["learns"]
            self.assertEqual(len(learns), 1)
            self.assertEqual(learns[0]["tier"], "edited")
            self.assertEqual(stats["weighted"]["metrics"]["medianSentenceLength"], None)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("What your edits do", guide)
            self.assertIn("delve", guide)
            summary = (project / "edits-summary.md").read_text(encoding="utf-8")
            self.assertIn("Measured as: edited", summary)


def _set_gold_minimum(root: Path, value: int) -> None:
    path = root / "manifest.json"
    manifest = json.loads(path.read_text(encoding="utf-8"))
    manifest["goldVoiceMinimum"] = value
    path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def _learn_pair(
    workdir: Path,
    root: Path,
    before: str,
    after: str,
    *,
    origin: str = "ai-first",
    date: str = "2026-01-02",
    section: str | None = None,
    model: str | None = None,
    tool: str | None = None,
    promote: bool = False,
) -> dict[str, object]:
    source = workdir / "src"
    source.mkdir(exist_ok=True)
    count = len(list(source.glob("before-*.md")))
    before_path = source / f"before-{count}.md"
    after_path = source / f"after-{count}.md"
    before_path.write_text(before, encoding="utf-8")
    after_path.write_text(after, encoding="utf-8")
    return learn_edit(
        root,
        before=before_path,
        after=after_path,
        section=section,
        origin=origin,
        date=date,
        model=model,
        tool=tool,
        promote=promote,
    )


def _write_legacy_pair(root: Path, slug: str, *, origin: str | None) -> None:
    """A v0.1 pair: `origin` only, no `provenance` block, no receipt."""
    pair = root / "gold" / slug
    pair.mkdir(parents=True)
    (pair / "before.md").write_text("I write at my desk.\n", encoding="utf-8")
    (pair / "after.md").write_text("I write at the desk.\n", encoding="utf-8")
    meta: dict[str, object] = {
        "date": slug[:10],
        "section": "notes",
        "kind": "prose",
        "beforeFile": "before.md",
        "afterFile": "after.md",
    }
    if origin is not None:
        meta["origin"] = origin
    (pair / "meta.json").write_text(json.dumps(meta) + "\n", encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
