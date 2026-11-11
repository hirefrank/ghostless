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


class NotesTests(unittest.TestCase):
    def test_notes_fold_into_guide_and_export(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            (project / "STYLE.md").write_text(
                "# Style\n\n- Every claim needs a source.\n", encoding="utf-8"
            )
            (project / "AUDIENCE.md").write_text(
                "# Audience\n\n- Engineers who skim.\n", encoding="utf-8"
            )
            stats = build_voice(project)
            self.assertEqual(stats["style"], "# Style\n\n- Every claim needs a source.")
            self.assertEqual(stats["audience"], "# Audience\n\n- Engineers who skim.")
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("## Style", guide)
            self.assertIn("Every claim needs a source.", guide)
            self.assertIn("## Audience", guide)
            self.assertIn("Engineers who skim.", guide)
            self.assertEqual(main(["export", "--project", str(project)]), 0)
            paste = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertIn("Every claim needs a source.", paste)
            self.assertIn("Engineers who skim.", paste)

    def test_missing_notes_render_a_pointer(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            stats = build_voice(project)
            self.assertIsNone(stats["style"])
            self.assertIsNone(stats["audience"])
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("No STYLE.md yet", guide)
            self.assertIn("No AUDIENCE.md yet", guide)

    def test_save_appends_dated_rule_and_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            self.assertEqual(
                main(["save", "--style", "Every claim needs a source.", "--project", str(project)]),
                0,
            )
            style = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertIn("# Style", style)
            self.assertIn("Every claim needs a source.", style)
            # The same rule on the same day must not add a duplicate line.
            self.assertEqual(
                main(["save", "--style", "Every claim needs a source.", "--project", str(project)]),
                0,
            )
            second = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertEqual(second.count("Every claim needs a source."), 1)
            # A different kind routes to the other file.
            self.assertEqual(
                main(["save", "--audience", "Engineers who skim.", "--project", str(project)]),
                0,
            )
            audience = (project / "AUDIENCE.md").read_text(encoding="utf-8")
            self.assertIn("# Audience", audience)
            self.assertIn("Engineers who skim.", audience)

    def test_save_does_not_treat_prefix_as_duplicate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            main(["save", "--style", "Every claim needs a source, and a link.", "--project", str(project)])
            # A shorter same-day rule that is only a prefix of the stored line
            # must still append, not be mistaken for the line already present.
            self.assertEqual(
                main(["save", "--style", "Every claim needs a source", "--project", str(project)]),
                0,
            )
            style = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertEqual(style.count("Every claim needs a source"), 2)

    def test_save_collapses_newlines_to_a_single_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            self.assertEqual(
                main(["save", "--style", "Every claim\nneeds a source.", "--project", str(project)]),
                0,
            )
            style = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertIn("Every claim needs a source.", style)
            self.assertNotIn("Every claim\n", style)
            # Same-day idempotency holds once the rule is collapsed.
            self.assertEqual(
                main(["save", "--style", "Every claim\nneeds a source.", "--project", str(project)]),
                0,
            )
            self.assertEqual(
                (project / "STYLE.md").read_text(encoding="utf-8").count("Every claim needs a source."),
                1,
            )

    def test_save_rejects_empty_rule(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(["save", "--style", "   ", "--project", str(project)])
            self.assertEqual(code, 2)
            self.assertFalse((project / "STYLE.md").exists())

    def test_notes_are_tracked_not_measured(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project)
            (project / "STYLE.md").write_text("# Style\n", encoding="utf-8")
            (project / "AUDIENCE.md").write_text("# Audience\n", encoding="utf-8")
            (project / "corpus" / "gold" / "desk.md").write_text("I write.\n", encoding="utf-8")
            # Tracked: the voice-data rules never ignore them.
            self.assertFalse(_ignored(project, "STYLE.md"))
            self.assertFalse(_ignored(project, "AUDIENCE.md"))
            # Not measured: discovery reads corpus/, transcripts/, and gold/ only.
            pieces = discover(project, load_manifest(project))
            self.assertEqual([piece.path for piece in pieces], ["corpus/gold/desk.md"])


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

    def test_any_uppercase_name_placeholder_flags_review(self) -> None:
        from ghostless.judge import draft_summary

        draft = "I write short lines. [FRANK: add the story here]"
        # Give the draft its own metrics as targets so only the placeholder
        # can move reviewRequired; that keeps this test focused.
        stats = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["placeholders"], ["[FRANK: add the story here]"])

    def test_writer_placeholder_still_flags_and_lowercase_brackets_do_not(self) -> None:
        from ghostless.judge import draft_summary, find_placeholders

        self.assertEqual(find_placeholders("[WRITER: your take]"), ["[WRITER: your take]"])
        self.assertEqual(find_placeholders("[TODO: fill this in]"), ["[TODO: fill this in]"])
        self.assertEqual(find_placeholders("[see page 3]"), [])
        self.assertEqual(find_placeholders("[2024: a year]"), [])
        draft = "I write short lines. [see page 3]"
        stats = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertFalse(report["reviewRequired"])
        self.assertEqual(report["placeholders"], [])

    def test_configured_placeholder_token_matches_case_insensitively(self) -> None:
        from ghostless.judge import draft_summary

        draft = "I write short lines. [frank: add the story here]"
        stats = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        self.assertEqual(judge_text(draft, stats)["placeholders"], [])
        report = judge_text(draft, stats, placeholder="Frank")
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["placeholders"], ["[frank: add the story here]"])

    def test_placeholder_token_from_manifest_reaches_judge_and_guide(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["placeholder"] = "FRANK"
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            build_voice(project)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("[FRANK: your take]", guide)
            self.assertNotIn("[WRITER: your take]", guide)
            draft = project / "draft.md"
            draft.write_text(
                (project / "drafts" / "closer.md").read_text(encoding="utf-8")
                + "\n\n[frank: add the story here]\n",
                encoding="utf-8",
            )
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                main(["judge", str(draft), "--json", "--project", str(project)])
            report = json.loads(out.getvalue())
            self.assertTrue(report["reviewRequired"])
            self.assertEqual(report["placeholders"], ["[frank: add the story here]"])

    def test_manifest_rejects_placeholder_with_brackets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["placeholder"] = "[FRANK]"
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaises(ProjectError):
                load_manifest(project)


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

            code = main(["build", "--project", str(project)])
            self.assertEqual(code, 0)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("What your edits do", guide)
            self.assertIn("delve", guide)
            self.assertIn("Edit rate", guide)
            self.assertIn("Structure:", guide)


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

    def test_migrate_reports_the_edited_default_for_v01_pairs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(
                    main(["migrate", "--project", str(project), "--no-git"]), 0
                )
            text = stdout.getvalue()
            self.assertIn("1 AI-first pair(s) are measured as edited", text)
            self.assertIn("ghostless promote <slug>", text)

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
