#!/usr/bin/env python3
"""Stats, judge, learn, and project tests. Sample data is invented."""

from __future__ import annotations

import json
import os
import shutil
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
from ghostless.project import discover, init_project, load_manifest  # noqa: E402
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
        stats = {"weighted": {"metrics": {}, "banList": []}}
        report = judge_text("It's not a shortcut, it's a practice.", stats)
        self.assertTrue(report["ok"])
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["assessment"], "mechanical-only")

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


class ProjectTests(unittest.TestCase):
    def test_init_gitignore_and_refuses_second_time(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            self.assertEqual(main(["init", str(project)]), 0)
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn("corpus/", ignore)
            self.assertIn("transcripts/", ignore)
            self.assertIn("gold/", ignore)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["kind"], "ghostless")
            self.assertEqual(manifest["weights"]["gold"], 3)
            self.assertEqual(manifest["goldVoiceMinimum"], 10)
            self.assertEqual(main(["init", str(project)]), 2)
            # Appending twice must not duplicate the block.
            self.assertEqual(ignore.count("corpus/"), 1)

    def test_export_excerpts_are_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            self.assertEqual(main(["build", "--project", str(project)]), 0)
            self.assertEqual(main(["export", "--project", str(project)]), 0)
            paste = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertNotIn("purple hedgehog", paste)
            self.assertIn("Ban list", paste)
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
