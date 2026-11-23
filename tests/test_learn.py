"""Edit diffing, move detection, and learn-pair storage tests."""

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

from ghostless.cli import main  # noqa: E402
from ghostless.learn import diff_texts, learn_edit  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import discover, init_project, load_manifest  # noqa: E402


class LearnTests(unittest.TestCase):
    def test_reorder_is_reported_as_moves(self) -> None:
        diff = diff_texts("alpha beta gamma delta", "gamma delta alpha beta")
        self.assertEqual(diff["deletions"], 0)
        self.assertEqual(diff["insertions"], 0)
        self.assertEqual(diff["replacements"], 0)
        self.assertEqual(diff["wordsRemoved"], 0)
        self.assertEqual(diff["wordsAdded"], 0)
        self.assertEqual(diff["movedTokens"], 2)
        self.assertEqual(diff["tokenEdits"], 2)
        self.assertEqual(diff["editRate"], 0.5)
        self.assertTrue(diff["movedSpans"])
        self.assertEqual(diff["movedSpans"][0]["text"], "gamma delta")

        # A swapped pair of phrases is still a reorder, not a cut and a paste.
        sentence = diff_texts(
            "The cold coffee waits on the desk.",
            "The desk waits on the cold coffee.",
        )
        self.assertEqual(sentence["wordsRemoved"], 0)
        self.assertEqual(sentence["wordsAdded"], 0)
        self.assertEqual(sentence["movedTokens"], 4)
        self.assertEqual(sentence["editRate"], round(4 / 7, 4))

    def test_punctuation_only_edit_stays_in_the_receipt(self) -> None:
        before = "I don't wait for the train"
        after = "I don't wait for the train."
        diff = diff_texts(before, after)
        self.assertEqual(diff["wordsBefore"], diff["wordsAfter"])
        self.assertEqual(diff["tokenEdits"], 0)
        self.assertEqual(diff["editRate"], 0.0)
        marks = {
            item["mark"]: item
            for item in diff["structuralChanges"]
            if item["kind"] == "punctuation"
        }
        self.assertEqual(marks["."]["before"], 0)
        self.assertEqual(marks["."]["after"], 1)
        self.assertEqual(diff["structuralChangeCount"], 1)

        # Same marks in a new place still count as a structural edit.
        reordered = diff_texts("Wait. Now,", "Wait, now.")
        self.assertEqual(reordered["tokenEdits"], 0)
        self.assertEqual(reordered["editRate"], 0.0)
        self.assertEqual(reordered["structuralChanges"][0]["change"], "order")

    def test_repeated_words_are_aligned_once(self) -> None:
        diff = diff_texts("delve delve deep", "delve deep")
        self.assertEqual(diff["deletions"], 1)
        self.assertEqual(diff["insertions"], 0)
        self.assertEqual(diff["replacements"], 0)
        self.assertEqual(diff["movedTokens"], 0)
        self.assertEqual(diff["wordsRemoved"], 1)
        removed = {item["word"]: item["count"] for item in diff["removedWords"]}
        self.assertEqual(removed.get("delve"), 1)
        self.assertEqual(diff["editRate"], round(1 / 3, 4))

        same = diff_texts("delve deep", "delve deep")
        self.assertEqual(same["tokenEdits"], 0)
        self.assertEqual(same["editRate"], 0.0)
        self.assertEqual(same["structuralChanges"], [])

        # Repeated tokens must not push the share of edited positions over 1.
        bounded = diff_texts("alpha alpha alpha", "beta beta alpha")
        self.assertEqual(bounded["editRate"], 0.8)
        self.assertLessEqual(bounded["editRate"], 1.0)

    def test_moved_paragraph_is_structural(self) -> None:
        before = "First paragraph stays here.\n\nSecond paragraph moves away."
        after = "Second paragraph moves away.\n\nFirst paragraph stays here."
        diff = diff_texts(before, after)
        self.assertEqual(diff["paragraphsBefore"], 2)
        self.assertEqual(diff["paragraphsAfter"], 2)
        self.assertGreater(diff["movedTokens"], 0)
        self.assertGreater(diff["editRate"], 0.0)
        moves = [
            item for item in diff["structuralChanges"] if item.get("kind") == "paragraph"
        ]
        self.assertEqual(len(moves), 1)
        self.assertEqual({moves[0]["from"], moves[0]["to"]}, {0, 1})

    def test_learn_receipt_keeps_punctuation_change(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project)
            before_path = Path(tmp) / "before.md"
            after_path = Path(tmp) / "after.md"
            before_path.write_text("I don't wait for the train", encoding="utf-8")
            after_path.write_text("I don't wait for the train.", encoding="utf-8")
            result = learn_edit(
                project,
                before=before_path,
                after=after_path,
                section="notes",
                origin="ai-first",
                date="2026-01-03",
            )
            meta = json.loads(
                (project / "gold" / str(result["slug"]) / "meta.json").read_text(encoding="utf-8")
            )
            self.assertEqual(meta["tokenEdits"], 0)
            self.assertEqual(meta["editRate"], 0.0)
            self.assertEqual(meta["structuralChanges"][0]["kind"], "punctuation")
            markdown = (project / "edits-summary.md").read_text(encoding="utf-8")
            self.assertIn("Edit rate", markdown)
            self.assertIn(". 0 → 1", markdown)

    def test_learn_stores_gold_and_repeated_cuts(self) -> None:
        before = "We should delve deeper. Then delve again. The sentence wanders for a while before it stops."
        after = "I'm done. I don't wander."
        diff = diff_texts(before, after)
        removed = {item["word"]: item["count"] for item in diff["removedWords"]}
        self.assertGreaterEqual(removed.get("delve", 0), 2)
        self.assertLess(diff["medianSentenceLengthAfter"], diff["medianSentenceLengthBefore"])
        self.assertGreater(diff["contractionsPer1kAfter"], diff["contractionsPer1kBefore"])

        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project)
            before_path = Path(tmp) / "before.md"
            after_path = Path(tmp) / "after.md"
            before_path.write_text(before, encoding="utf-8")
            after_path.write_text(after, encoding="utf-8")
            first = learn_edit(
                project,
                before=before_path,
                after=after_path,
                section="notes",
                origin="ai-first",
                date="2026-01-02",
            )
            stored = project / "gold" / first["slug"]
            self.assertTrue((stored / "before.md").is_file())
            self.assertTrue((stored / "after.md").is_file())
            meta = json.loads((stored / "meta.json").read_text(encoding="utf-8"))
            self.assertEqual(meta["origin"], "ai-first")
            self.assertEqual(meta["section"], "notes")
            self.assertEqual(meta["date"], "2026-01-02")
            with self.assertRaisesRegex(ProjectError, "already learned"):
                learn_edit(
                    project,
                    before=before_path,
                    after=after_path,
                    section="notes",
                    origin="ai-first",
                    date="2026-01-02",
                )
            after_path.write_text("I'm done. I really don't wander.", encoding="utf-8")
            second = learn_edit(
                project,
                before=before_path,
                after=after_path,
                section="notes",
                origin="writer-first",
                date="2026-01-02",
            )
            self.assertNotEqual(first["slug"], second["slug"])
            summary = json.loads((project / "edits-summary.json").read_text(encoding="utf-8"))
            self.assertGreaterEqual(summary["repeatedRemovals"].get("delve", 0), 2)
            manifest = load_manifest(project)
            pieces = discover(project, manifest)
            learned = [piece for piece in pieces if piece.learned]
            self.assertEqual(len(learned), 2)
            # An AI-first final is model prose the writer revised, so it stays
            # in the edited tier. A writer-first pair is gold through its origin.
            self.assertEqual(sorted(piece.tier for piece in learned), ["edited", "gold"])
            self.assertTrue(all(piece.section == "notes" for piece in learned))
            by_origin = {piece.origin: piece for piece in learned}
            self.assertEqual(by_origin["ai-first"].tier, "edited")
            self.assertEqual(by_origin["writer-first"].tier, "gold")

            with contextlib.redirect_stdout(io.StringIO()):
                code = main(["build", "--project", str(project)])
            self.assertEqual(code, 0)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("What your edits do", guide)
            self.assertIn("delve", guide)
            self.assertIn("Edit rate", guide)
            self.assertIn("Structure:", guide)


if __name__ == "__main__":
    unittest.main()
