#!/usr/bin/env python3
"""Stats, judge, learn, and project tests. Sample data is invented."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"

sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.judge import judge_text  # noqa: E402
from ghostless.learn import diff_texts, learn_edit  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import (  # noqa: E402
    GITIGNORE_MARKER,
    LEGACY_GITIGNORE_BLOCK,
    LEGACY_GITIGNORE_MARKER,
    discover,
    init_project,
    load_manifest,
    migrate_project,
    new_session_id,
)
from ghostless.stats import Piece, compile_stats, pools_for, summarize, weighted_median  # noqa: E402
from ghostless.text import measure, split_sentences  # noqa: E402
from ghostless.tells import structural_tells  # noqa: E402
from ghostless.updatecheck import plan, updates_disabled, version_gt  # noqa: E402


class TextTests(unittest.TestCase):
    def test_abbreviation_and_decimal_stay_intact(self) -> None:
        sentences = split_sentences("Dr. Moss tried it. 3.14 is enough. Then she stopped.")
        self.assertEqual(
            sentences,
            ["Dr. Moss tried it.", "3.14 is enough.", "Then she stopped."],
        )

    def test_measure_counts(self) -> None:
        text = "Dr. Moss tried it. I don't wait.\n\nAnd I'm done? So we stop."
        measured = measure(text)
        self.assertEqual(measured.sentences, 4)
        self.assertEqual(measured.sentence_lengths, (4, 3, 3, 3))
        self.assertEqual(measured.contractions, 2)
        self.assertEqual(measured.i_count, 2)
        self.assertEqual(measured.questions, 1)
        self.assertEqual(measured.openers, {"and": 1, "but": 0, "so": 1})
        self.assertEqual(measured.paragraph_lengths, (7, 6))
        self.assertEqual(measured.em_dashes, 0)

    def test_em_dash_forms(self) -> None:
        self.assertEqual(measure("Wait — now.").em_dashes, 1)
        self.assertEqual(measure("Wait -- now.").em_dashes, 1)
        self.assertEqual(measure("well-known").em_dashes, 0)


class WeightTests(unittest.TestCase):
    def test_weighted_median_prefers_gold(self) -> None:
        # One short sentence carrying gold's weight beats two long ones.
        self.assertEqual(weighted_median([(10, 3), (100, 1), (100, 1)]), 10)
        self.assertEqual(weighted_median([(10, 1), (100, 1), (100, 1)]), 100)

    def test_spoken_is_rhythm_only_and_bronze_drops_out_of_voice(self) -> None:
        pieces = [
            Piece("g.md", "gold", "prose", "notes", 3, "I write.\n\nI stop."),
            Piece(
                "b.md",
                "bronze",
                "prose",
                "archive",
                0.5,
                "one two three four five six seven eight nine ten.",
            ),
            Piece("s.md", "silver", "spoken", None, 1, "And I go — now.\n\nSo I go."),
        ]
        self.assertEqual(pools_for(pieces[2], bronze_format_only=False), frozenset({"rhythm"}))
        self.assertEqual(
            pools_for(pieces[1], bronze_format_only=True),
            frozenset({"format"}),
        )
        stats = compile_stats(pieces, weights={"gold": 3, "silver": 1, "bronze": 0.5}, gold_voice_minimum=1)
        self.assertTrue(stats["bronzeFormatOnly"])
        weighted = stats["weighted"]["metrics"]
        self.assertEqual(weighted["rhythmSentences"], 8)
        self.assertEqual(weighted["formatParagraphs"], 6.5)
        self.assertEqual(weighted["emDashesPer1k"], 0)
        unweighted = stats["unweighted"]["metrics"]
        self.assertEqual(unweighted["rhythmSentences"], 4)
        # Spoken "now" is not written prose, so it cannot save a tell.
        # "leverage" is not in this tiny set; the empty-diction check is separate.
        self.assertIn("delve", stats["weighted"]["banList"])
        self.assertNotIn("leverage", _diction_text(pieces))

    def test_gold_spoken_does_not_drop_bronze_voice(self) -> None:
        pieces = [
            Piece("s1.md", "gold", "spoken", None, 3, "And I go now. So I stop."),
            Piece("s2.md", "gold", "spoken", None, 3, "But I stay."),
            Piece("b.md", "bronze", "prose", "archive", 0.5, "one two three four five six seven eight nine ten."),
        ]
        stats = compile_stats(pieces, weights={"gold": 3, "silver": 1, "bronze": 0.5}, gold_voice_minimum=2)
        # Two gold transcripts are not written prose; they must not make
        # bronze format-only and flush the only written sample from voice.
        self.assertFalse(stats["bronzeFormatOnly"])
        self.assertIn("delve", stats["weighted"]["banList"])
        self.assertIsNotNone(stats["weighted"]["metrics"]["medianSentenceLength"])


class SampleProjectTests(unittest.TestCase):
    def test_sample_is_invented(self) -> None:
        blob = "\n".join(
            path.read_text(encoding="utf-8")
            for path in SAMPLE.rglob("*")
            if path.is_file()
        )
        self.assertNotIn("Frank Harris", blob)
        self.assertIn("SAMPLE DATA", (SAMPLE / "README.md").read_text(encoding="utf-8"))

    def test_build_tiers_sections_and_bans(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            stats = build_voice(project)
            self.assertFalse(stats["bronzeFormatOnly"])
            ban = stats["weighted"]["banList"]
            used = stats["weighted"]["tellsInUse"]
            self.assertIn("delve", ban)
            self.assertIn("leverage", ban)
            self.assertIn("furthermore", ban)
            self.assertIn("foster", used)
            self.assertNotIn("foster", ban)
            phrases = [item["phrase"] for item in stats["weighted"]["signaturePhrases"]]
            self.assertIn("cold coffee", phrases)
            self.assertEqual(stats["weighted"]["metrics"]["emDashesPer1k"], 0)
            notes = stats["sections"]["notes"]["weighted"]["metrics"]["medianSentenceLength"]
            archive = stats["sections"]["archive"]["weighted"]["metrics"]["medianSentenceLength"]
            self.assertIsNotNone(notes)
            self.assertIsNotNone(archive)
            self.assertGreater(archive, notes)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertNotIn("purple hedgehog", guide)
            self.assertIn("cold coffee", guide)
            self.assertIn("Unweighted", guide)

            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["goldVoiceMinimum"] = 1
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            limited = build_voice(project)
            self.assertTrue(limited["bronzeFormatOnly"])
            self.assertIn("foster", limited["weighted"]["banList"])
            self.assertNotIn("foster", limited["weighted"]["tellsInUse"])
            archive_metrics = limited["sections"]["archive"]["weighted"]["metrics"]
            self.assertIsNone(archive_metrics["medianSentenceLength"])
            self.assertIsNotNone(archive_metrics["medianParagraphLength"])


class JudgeTests(unittest.TestCase):
    def test_ai_draft_fails_and_closer_draft_beats_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            stats = build_voice(project)
            ai = judge_text((project / "drafts" / "ai-ish.md").read_text(encoding="utf-8"), stats)
            closer = judge_text((project / "drafts" / "closer.md").read_text(encoding="utf-8"), stats)
        self.assertFalse(ai["ok"])
        self.assertTrue(any(item["tell"] == "delve" for item in ai["banned"]))
        self.assertTrue(any(item["tell"] == "furthermore" for item in ai["banned"]))
        kinds = {item["kind"] for item in ai["structure"]}
        self.assertIn("not-x-its-y", kinds)
        self.assertIn("triplet", kinds)
        self.assertIn("em-dash", kinds)
        self.assertGreater(closer["score"], ai["score"])
        self.assertFalse(closer["banned"])
        self.assertFalse(closer["structure"])
        self.assertTrue(closer["ok"])

    def test_structural_hit_is_review_not_automatic_failure(self) -> None:
        from ghostless.judge import draft_summary

        draft = "It's not a shortcut, it's a practice."
        # Give the draft its own metrics as targets so only the structural
        # tell can move the score; that keeps this test focused on review-only.
        stats = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertTrue(report["ok"])
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["assessment"], "mechanical-only")

    def test_empty_project_cannot_pass(self) -> None:
        stats = {"weighted": {"metrics": {}, "banList": []}}
        report = judge_text("An ordinary draft with a few sentences.", stats)
        self.assertFalse(report["ok"])
        self.assertEqual(report["score"], 0)
        self.assertIn("No usable voice data", report["error"])

    def test_section_targets_differ(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            stats = build_voice(project)
            draft = (project / "drafts" / "closer.md").read_text(encoding="utf-8")
            notes = judge_text(draft, stats, section="notes")
            archive = judge_text(draft, stats, section="archive")
        self.assertEqual(notes["section"], "notes")
        self.assertEqual(archive["section"], "archive")
        self.assertNotEqual(notes["targets"]["medianSentenceLength"], archive["targets"]["medianSentenceLength"])

    def test_not_x_and_triplet_detectors(self) -> None:
        hits = structural_tells("It's not a shortcut, it's a practice. The line was clear, concise, and compelling.")
        kinds = [hit.kind for hit in hits]
        self.assertIn("not-x-its-y", kinds)
        self.assertIn("triplet", kinds)
        self.assertEqual(structural_tells("I bought milk, oats, and tea from the shop."), [])


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
            self.assertTrue(all(piece.tier == "gold" for piece in learned))
            self.assertTrue(all(piece.section == "notes" for piece in learned))

            code = main(["build", "--project", str(project)])
            self.assertEqual(code, 0)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("What your edits do", guide)
            self.assertIn("delve", guide)
            self.assertIn("Edit rate", guide)
            self.assertIn("Structure:", guide)


class ProjectTests(unittest.TestCase):
    def test_init_writes_data_repo_rules_and_local_git(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            self.assertEqual(main(["init", str(project)]), 0)
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn(GITIGNORE_MARKER, ignore)
            self.assertNotIn(LEGACY_GITIGNORE_MARKER, ignore)
            self.assertTrue((project / ".git").is_dir())
            self.assertEqual(_git(project, "remote").stdout.strip(), "")
            # The writing, finished edit pairs, sessions, and holdouts are tracked.
            for tracked in (
                "manifest.json",
                "corpus/gold/desk.md",
                "corpus/silver/recent.md",
                "transcripts/talk.md",
                "gold/2026-01-02-notes/after.md",
                "sessions/2026-01-02-notes/before.md",
                "holdouts/kept.md",
            ):
                self.assertFalse(_ignored(project, tracked), tracked)
            # Generated output, drafts, temp files, and raw media stay out.
            for ignored in (
                "voice-stats.json",
                "voice-guide.md",
                "voice-guide.paste.md",
                "notes.paste.md",
                "edits-summary.json",
                "edits-summary.md",
                "stats/summary.json",
                "exports/guide.md",
                "drafts/ai.md",
                "raw/take.wav",
                "take.mp3",
                "take.m4a",
                "clip.mp4",
                "notes.md.tmp",
                ".DS_Store",
            ):
                self.assertTrue(_ignored(project, ignored), ignored)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["kind"], "ghostless")
            self.assertEqual(manifest["weights"]["gold"], 3)
            self.assertEqual(manifest["goldVoiceMinimum"], 10)
            self.assertEqual(main(["init", str(project)]), 2)
            # Appending twice must not duplicate the block.
            self.assertEqual(ignore.count(GITIGNORE_MARKER), 1)

    def test_scratch_dirs_are_root_only(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            self.assertEqual(main(["init", str(project)]), 0)
            # A corpus section named like a scratch dir is still measured by
            # build, so it must stay tracked rather than be swallowed by an
            # unanchored ignore pattern.
            for section in ("drafts", "stats", "exports", "raw"):
                self.assertFalse(_ignored(project, f"corpus/gold/{section}/note.md"), section)
            # The top-level scratch dirs themselves are still ignored.
            for scratch in ("drafts", "stats", "exports", "raw"):
                self.assertTrue(_ignored(project, f"{scratch}/note.md"), scratch)

    def test_init_can_skip_git(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            self.assertEqual(main(["init", "--no-git", str(project)]), 0)
            self.assertFalse((project / ".git").exists())
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn(GITIGNORE_MARKER, ignore)

    def test_rebuild_does_not_dirty_git(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project)
            (project / "corpus" / "gold" / "desk.md").write_text(
                "I write at the desk.\n\nI stop when the coffee is cold.\n",
                encoding="utf-8",
            )
            _commit_all(project)
            self.assertEqual(_git(project, "status", "--porcelain").stdout, "")

            self.assertEqual(main(["build", "--project", str(project)]), 0)
            self.assertEqual(main(["export", "--project", str(project)]), 0)
            self.assertTrue((project / "voice-stats.json").is_file())
            self.assertTrue((project / "voice-guide.md").is_file())
            self.assertTrue((project / "voice-guide.paste.md").is_file())
            self.assertEqual(_git(project, "status", "--porcelain").stdout, "")

    def test_migrate_v01_project_keeps_writing_out_of_git(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            before = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn(LEGACY_GITIGNORE_MARKER, before)

            self.assertEqual(main(["migrate", "--project", str(project)]), 0)
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertNotIn(LEGACY_GITIGNORE_MARKER, ignore)
            self.assertIn(GITIGNORE_MARKER, ignore)
            self.assertEqual(ignore.count(GITIGNORE_MARKER), 1)
            # Rules the writer wrote themselves survive.
            self.assertIn("node_modules/", ignore)

            # Nothing was staged, committed, or pushed by the migration.
            self.assertEqual(_git(project, "ls-files").stdout.strip(), "")
            self.assertEqual(_git(project, "diff", "--cached", "--name-only").stdout.strip(), "")
            self.assertNotEqual(_git(project, "rev-parse", "--verify", "HEAD").returncode, 0)
            self.assertEqual(_git(project, "remote").stdout.strip(), "")

            # Writing and finished edit pairs are trackable; generated files,
            # drafts, and raw media are still ignored.
            for tracked in (
                "corpus/gold/desk.md",
                "gold/2026-01-02-notes/before.md",
                "gold/2026-01-02-notes/after.md",
            ):
                self.assertFalse(_ignored(project, tracked), tracked)
            for ignored in ("voice-stats.json", "drafts/ai.md", "raw/take.wav", "notes.md.tmp"):
                self.assertTrue(_ignored(project, ignored), ignored)

            # Missing v0.1 directories were added, and nothing was deleted.
            self.assertTrue((project / "corpus" / "silver" / ".gitkeep").is_file())
            self.assertTrue((project / "transcripts" / ".gitkeep").is_file())
            self.assertTrue((project / "sessions" / ".gitkeep").is_file())
            self.assertTrue((project / "holdouts" / ".gitkeep").is_file())
            self.assertTrue((project / "corpus" / "gold" / "desk.md").is_file())

            # A second run is a no-op.
            self.assertEqual(main(["migrate", "--project", str(project)]), 0)
            self.assertEqual((project / ".gitignore").read_text(encoding="utf-8"), ignore)

    def test_migrate_prints_no_remote_in_parent_repo(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            parent = Path(tmp) / "parent"
            parent.mkdir()
            self.assertEqual(_git(parent, "init", "-q").returncode, 0)
            project = _write_v01_project(parent / "voice")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(main(["migrate", "--project", str(project)]), 0)
            text = stdout.getvalue()
            self.assertNotIn("git remote add origin", text)
            self.assertNotIn("git push", text)
            self.assertIn("parent", text.lower())

    def test_migrate_no_git_prints_no_git_steps(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(main(["migrate", "--project", str(project), "--no-git"]), 0)
            text = stdout.getvalue()
            self.assertNotIn("git remote add origin", text)
            self.assertNotIn("git push", text)
            self.assertNotIn("git commit", text)

    def test_init_refuses_a_v01_ignore_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(["init", str(project), "--force"])
            self.assertEqual(code, 2)
            self.assertIn("ghostless migrate", stderr.getvalue())
            self.assertFalse((project / ".git").exists())

    def test_migrate_refuses_a_hand_edited_v01_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            (project / ".gitignore").write_text(
                LEGACY_GITIGNORE_BLOCK.replace("gold/\n", ""),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ProjectError, "was edited"):
                migrate_project(project)
            # Fail closed: the edited rules are left exactly as they were.
            self.assertNotIn(
                GITIGNORE_MARKER,
                (project / ".gitignore").read_text(encoding="utf-8"),
            )

    def test_sessions_and_holdouts_are_not_measured(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            (project / "corpus" / "gold" / "desk.md").write_text("I write.\n", encoding="utf-8")
            (project / "sessions" / "2026-01-02-notes").mkdir(parents=True)
            (project / "sessions" / "2026-01-02-notes" / "before.md").write_text(
                "An AI draft.\n", encoding="utf-8"
            )
            (project / "holdouts" / "kept.md").write_text("Held out.\n", encoding="utf-8")
            pieces = discover(project, load_manifest(project))
            self.assertEqual([piece.path for piece in pieces], ["corpus/gold/desk.md"])

    def test_session_ids_follow_the_learn_slug(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            self.assertEqual(
                new_session_id(project, date="2026-01-02", section="Field Notes"),
                "2026-01-02-field-notes",
            )
            (project / "sessions" / "2026-01-02-field-notes").mkdir()
            self.assertEqual(
                new_session_id(project, date="2026-01-02", section="Field Notes"),
                "2026-01-02-field-notes-2",
            )
            self.assertEqual(
                new_session_id(project, date="2026-01-03"),
                "2026-01-03",
            )
            with self.assertRaisesRegex(ProjectError, "YYYY-MM-DD"):
                new_session_id(project, date="01/02/2026")

    def test_export_excerpts_are_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            self.assertEqual(main(["build", "--project", str(project)]), 0)
            self.assertEqual(main(["export", "--project", str(project)]), 0)
            paste = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertNotIn("purple hedgehog", paste)
            self.assertIn("Ban list", paste)
            self.assertIn("cold coffee", paste)  # signature phrase, included by default
            self.assertIn("paste", paste.lower())
            self.assertEqual(main(["export", "--project", str(project), "--excerpts"]), 0)
            with_bits = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertIn("purple hedgehog", with_bits)

    def test_judge_command_exit_codes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            build_voice(project)
            self.assertEqual(
                main(["judge", str(project / "drafts" / "ai-ish.md"), "--project", str(project)]),
                1,
            )
            self.assertEqual(
                main(["judge", str(project / "drafts" / "closer.md"), "--project", str(project)]),
                0,
            )
            self.assertEqual(
                main(
                    [
                        "judge",
                        str(project / "drafts" / "closer.md"),
                        "--project",
                        str(project),
                        "--section",
                        "missing",
                    ]
                ),
                2,
            )


class UpdatePlanTests(unittest.TestCase):
    def test_version_order_and_plan(self) -> None:
        self.assertTrue(version_gt("0.2.0", "0.1.0"))
        self.assertTrue(version_gt("v0.1.10", "0.1.9"))
        self.assertFalse(version_gt("0.1.0", "0.1.0"))
        self.assertFalse(version_gt("0.1.0", "0.1.1"))
        self.assertEqual(
            plan(disabled=True, stamp_age_s=None, interval_s=10, remote="9.0.0", local="0.1.0", fetch_ok=True),
            "disabled",
        )
        self.assertEqual(
            plan(disabled=False, stamp_age_s=5, interval_s=10, remote="9.0.0", local="0.1.0", fetch_ok=True),
            "skip",
        )
        self.assertEqual(
            plan(disabled=False, stamp_age_s=50, interval_s=10, remote="0.2.0", local="0.1.0", fetch_ok=True),
            "update 0.2.0",
        )
        self.assertEqual(
            plan(disabled=False, stamp_age_s=None, interval_s=10, remote=None, local="0.1.0", fetch_ok=True),
            "current",
        )
        self.assertEqual(
            plan(disabled=False, stamp_age_s=None, interval_s=10, remote=None, local="0.1.0", fetch_ok=False),
            "current",
        )

    def test_home_config_disables_the_check_without_a_network_call(self) -> None:
        from ghostless.updatecheck import run_once

        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / "config.json").write_text('{"updateCheck": false}\n', encoding="utf-8")
            old_home = os.environ.get("GHOSTLESS_HOME")
            old_no = os.environ.pop("GHOSTLESS_NO_UPDATE", None)
            os.environ["GHOSTLESS_HOME"] = str(home)

            def explode() -> tuple[bool, str | None]:
                raise AssertionError("release check should not run when disabled")

            try:
                self.assertTrue(updates_disabled())
                self.assertEqual(run_once(fetcher=explode), "disabled")
            finally:
                if old_home is None:
                    os.environ.pop("GHOSTLESS_HOME", None)
                else:
                    os.environ["GHOSTLESS_HOME"] = old_home
                if old_no is not None:
                    os.environ["GHOSTLESS_NO_UPDATE"] = old_no


def _diction_text(pieces: list[Piece]) -> str:
    summary = summarize(pieces, bronze_format_only=True, use_weights=True)
    return " ".join(summary["tellsInUse"])


def _require_git(test: unittest.TestCase) -> None:
    if shutil.which("git") is None:
        test.skipTest("git is not installed")


def _git(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=str(cwd),
        check=False,
        capture_output=True,
        text=True,
    )


def _ignored(cwd: Path, relative: str) -> bool:
    """True when .gitignore matches the path, using Git's own matcher."""
    result = _git(cwd, "check-ignore", "--no-index", "-q", relative)
    if result.returncode not in (0, 1):
        raise AssertionError(result.stderr)
    return result.returncode == 0


def _commit_all(root: Path) -> None:
    added = _git(root, "add", "-A")
    if added.returncode != 0:
        raise AssertionError(added.stderr)
    committed = _git(
        root,
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "-q",
        "-m",
        "test: voice data",
    )
    if committed.returncode != 0:
        raise AssertionError(committed.stderr)


def _write_v01_project(root: Path) -> Path:
    """A representative v0.1 project: old ignore block, writing, generated output."""
    for relative in ("corpus/gold", "corpus/silver", "corpus/bronze", "corpus/spoken", "transcripts"):
        (root / relative).mkdir(parents=True)
    pair = root / "gold" / "2026-01-02-notes"
    pair.mkdir(parents=True)
    (root / "manifest.json").write_text(
        json.dumps(
            {
                "kind": "ghostless",
                "version": 1,
                "weights": {"gold": 3, "silver": 1, "bronze": 0.5},
                "goldVoiceMinimum": 10,
                "updateCheck": True,
                "failUnder": 70,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    (root / "corpus" / "gold" / "desk.md").write_text(
        "I write at the desk.\n\nI stop when the coffee is cold.\n",
        encoding="utf-8",
    )
    (pair / "before.md").write_text("I write at my desk.\n", encoding="utf-8")
    (pair / "after.md").write_text("I write at the desk.\n", encoding="utf-8")
    (pair / "meta.json").write_text(
        json.dumps(
            {
                "date": "2026-01-02",
                "section": "notes",
                "origin": "ai-first",
                "kind": "prose",
                "beforeFile": "before.md",
                "afterFile": "after.md",
            }
        )
        + "\n",
        encoding="utf-8",
    )
    (root / "voice-stats.json").write_text("{}\n", encoding="utf-8")
    (root / "voice-guide.md").write_text("# voice guide\n", encoding="utf-8")
    (root / "drafts").mkdir()
    (root / "drafts" / "ai.md").write_text("An AI draft.\n", encoding="utf-8")
    (root / "raw").mkdir()
    (root / "raw" / "take.wav").write_bytes(b"RIFF")
    (root / ".gitignore").write_text(
        "node_modules/\n\n" + LEGACY_GITIGNORE_BLOCK,
        encoding="utf-8",
    )
    return root


class VersionFileTests(unittest.TestCase):
    def test_versions_match(self) -> None:
        package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        pyproject = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
        from ghostless import __version__

        self.assertEqual(package["name"], "ghostless")
        self.assertEqual(package["version"], "0.1.0")
        self.assertEqual(__version__, "0.1.0")
        self.assertIn('version = "0.1.0"', pyproject)
        self.assertIn('name = "ghostless"', pyproject)


if __name__ == "__main__":
    os.environ.setdefault("GHOSTLESS_NO_UPDATE", "1")
    unittest.main()
