"""Judge scoring, overrides, placeholders, and structural tell tests. Sample data is invented."""

from __future__ import annotations

import contextlib
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.build import build_voice  # noqa: E402
from ghostless.cli import main  # noqa: E402
from ghostless.guide import _num  # noqa: E402
from ghostless.judge import apply_overrides, draft_summary, find_placeholders, judge_text  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import init_project, load_manifest  # noqa: E402
from ghostless.tells import ban_hits, structural_tells  # noqa: E402


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

    def _stats_from_draft(self, draft: str, **targets: float) -> dict[str, object]:
        metrics = dict(draft_summary(draft)["metrics"])
        metrics.update(targets)
        return {"weighted": {"metrics": metrics, "banList": []}}

    def test_em_dash_underuse_warns_like_overuse(self) -> None:
        draft = (
            "I write short lines. I keep them plain. I stop when the thought ends. "
            "I start again. I write daily. I cut what I do not need. "
            "I read it aloud. I fix the clumsy line. I ship the piece. I move on."
        )
        stats = self._stats_from_draft(draft, emDashesPer1k=8.1)
        report = judge_text(draft, stats)
        check = next(item for item in report["checks"] if item["metric"] == "emDashesPer1k")
        self.assertEqual(check["status"], "low")
        self.assertEqual(check["low"], 4.05)
        self.assertGreater(check["penalty"], 0)
        self.assertTrue(any(item["kind"] == "em-dash" for item in report["structure"]))
        self.assertTrue(report["reviewRequired"])

    def test_other_mid_range_metrics_already_warn_low(self) -> None:
        # Verified, not assumed: _compare warns both ways for the
        # two-direction metrics; only em dashes lacked a floor.
        draft = "I write short lines. I keep them plain. I stop when the thought ends."
        stats = self._stats_from_draft(draft, iPer1k=600.0)
        report = judge_text(draft, stats)
        check = next(item for item in report["checks"] if item["metric"] == "iPer1k")
        self.assertEqual(check["status"], "low")
        self.assertGreater(check["penalty"], 0)

    def test_override_zero_bans_em_dashes(self) -> None:
        draft = (
            "I write with a pause — and then I stop — and then I start again. "
            "Short lines. Clear lines. I keep going. I stop here. I start again. "
            "I write plainly. I write daily."
        )
        stats = self._stats_from_draft(draft)
        report = judge_text(draft, stats, overrides={"emDashesPer1k": 0})
        check = next(item for item in report["checks"] if item["metric"] == "emDashesPer1k")
        self.assertEqual(check["target"], 0)
        self.assertEqual(check["status"], "high")
        self.assertGreater(check["penalty"], 0)
        # The override never rewrites the caller's stats.
        self.assertNotEqual(stats["weighted"]["metrics"]["emDashesPer1k"], 0)
        self.assertEqual(
            apply_overrides({"emDashesPer1k": 8.1}, {"emDashesPer1k": 0}),
            {"emDashesPer1k": 0},
        )

        plain = "I write short lines. I keep them plain. I stop here. I start again."
        calm = judge_text(plain, self._stats_from_draft(plain), overrides={"emDashesPer1k": 0})
        check = next(item for item in calm["checks"] if item["metric"] == "emDashesPer1k")
        self.assertEqual(check["status"], "ok")

    def test_override_zero_bans_a_single_em_dash(self) -> None:
        # Regression: the old 1.5-per-1k slack let a single dash in a
        # ~1000-word draft score ok even with the target pinned to 0.
        body = "I write short lines. " * 250
        draft = body + "One line — with a dash."
        stats = self._stats_from_draft(draft)
        report = judge_text(draft, stats, overrides={"emDashesPer1k": 0})
        check = next(item for item in report["checks"] if item["metric"] == "emDashesPer1k")
        self.assertEqual(check["target"], 0)
        # Inside the old slack band, so this only passes without the slack.
        self.assertGreater(check["actual"], 0)
        self.assertLess(check["actual"], 1.5)
        self.assertEqual(check["high"], 0)
        self.assertEqual(check["status"], "high")
        self.assertGreater(check["penalty"], 0)
        self.assertTrue(any(item["kind"] == "em-dash" for item in report["structure"]))

    def test_override_pins_target_in_guide_and_judge(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["overrides"] = {"emDashesPer1k": 0}
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            stats = build_voice(project)
            # sample-voice itself has 0 em dashes; measured stats stay untouched.
            self.assertEqual(stats["weighted"]["metrics"]["emDashesPer1k"], 0)
            self.assertEqual(stats["overrides"], {"emDashesPer1k": 0})
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("target pinned to 0", guide)
            self.assertIn("measured", guide)
            draft = project / "draft.md"
            draft.write_text(
                (project / "drafts" / "closer.md").read_text(encoding="utf-8")
                + "\n\nOne more line — with a dash.\n",
                encoding="utf-8",
            )
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                main(["judge", str(draft), "--json", "--project", str(project)])
            report = json.loads(out.getvalue())
            check = next(item for item in report["checks"] if item["metric"] == "emDashesPer1k")
            self.assertEqual(check["target"], 0)
            self.assertEqual(check["status"], "high")

    def test_override_preserves_measured_targets_and_unpins_without_rebuild(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            (project / "corpus" / "gold" / "notes" / "dashes.md").write_text(
                (
                    "I write short lines — then I pause — and then I write again.\n"
                    "We keep the cadence clear — never ornate — and stop on time.\n"
                ) * 3,
                encoding="utf-8",
            )
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["overrides"] = {"emDashesPer1k": 0}
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

            stats = build_voice(project)
            on_disk = json.loads((project / "voice-stats.json").read_text(encoding="utf-8"))
            measured_em = stats["weighted"]["metrics"]["emDashesPer1k"]
            section_measured_em = stats["sections"]["notes"]["weighted"]["metrics"]["emDashesPer1k"]
            self.assertGreater(measured_em, 0)
            self.assertGreater(section_measured_em, 0)
            self.assertEqual(on_disk["weighted"]["metrics"]["emDashesPer1k"], measured_em)
            self.assertEqual(
                on_disk["sections"]["notes"]["weighted"]["metrics"]["emDashesPer1k"],
                section_measured_em,
            )
            self.assertEqual(stats["overrides"], {"emDashesPer1k": 0})

            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("| Em dashes per 1k words | 0 |", guide)
            self.assertIn(f"target pinned to 0 (measured {_num(measured_em)})", guide)
            notes_section = guide.split("### notes", 1)[1].split("### ", 1)[0]
            self.assertIn("- Em dashes per 1k: 0\n", notes_section)

            draft = project / "draft.md"
            draft_text = (
                (project / "drafts" / "closer.md").read_text(encoding="utf-8")
                + "\n\nOne more line — with a dash.\n"
            )
            draft.write_text(draft_text, encoding="utf-8")

            direct = judge_text(draft_text, stats)
            direct_check = next(item for item in direct["checks"] if item["metric"] == "emDashesPer1k")
            self.assertEqual(direct_check["target"], 0)
            self.assertEqual(direct_check["status"], "high")

            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                main(["judge", str(draft), "--json", "--project", str(project)])
            report = json.loads(out.getvalue())
            check = next(item for item in report["checks"] if item["metric"] == "emDashesPer1k")
            self.assertEqual(check["target"], 0)
            self.assertEqual(check["status"], "high")

            del manifest["overrides"]
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                main(["judge", str(draft), "--json", "--project", str(project)])
            restored = json.loads(out.getvalue())
            restored_check = next(item for item in restored["checks"] if item["metric"] == "emDashesPer1k")
            self.assertEqual(restored_check["target"], measured_em)

            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                main(["judge", str(draft), "--section", "notes", "--json", "--project", str(project)])
            restored_section = json.loads(out.getvalue())
            restored_section_check = next(
                item for item in restored_section["checks"] if item["metric"] == "emDashesPer1k"
            )
            self.assertEqual(restored_section_check["target"], section_measured_em)

    def test_override_opener_share_formats_as_percentage_in_guide(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["overrides"] = {"openerShare": 0.2}
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

            stats = build_voice(project)
            measured_share = stats["weighted"]["metrics"]["openerShare"]
            measured_pct = f"{measured_share * 100:.0f}%"
            self.assertNotEqual(measured_pct, "20%")

            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("| Sentences opening with And/But/So | 20% |", guide)
            self.assertIn(
                f"- And/But/So opener share: target pinned to 20% (measured {measured_pct})",
                guide,
            )
            notes_section = guide.split("### notes", 1)[1].split("### ", 1)[0]
            self.assertIn("- And/But/So openers: 20%\n", notes_section)

    def test_any_uppercase_name_placeholder_flags_review(self) -> None:
        draft = "I write short lines. [FRANK: add the story here]"
        # Give the draft its own metrics as targets so only the placeholder
        # can move reviewRequired; that keeps this test focused.
        stats = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["placeholders"], ["[FRANK: add the story here]"])

    def test_writer_placeholder_still_flags_and_lowercase_brackets_do_not(self) -> None:
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

    def test_default_placeholder_token_flags_mixed_case_writer(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            # The sample manifest has no `placeholder` key: the documented
            # default WRITER must apply, matched case-insensitively.
            self.assertEqual(load_manifest(project)["placeholder"], "WRITER")
            build_voice(project)
            draft = project / "draft.md"
            draft.write_text(
                (project / "drafts" / "closer.md").read_text(encoding="utf-8")
                + "\n\n[Writer: add the story here]\n\n[writer: and the quote]\n",
                encoding="utf-8",
            )
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                main(["judge", str(draft), "--json", "--project", str(project)])
            report = json.loads(out.getvalue())
            self.assertTrue(report["reviewRequired"])
            self.assertEqual(
                report["placeholders"],
                ["[Writer: add the story here]", "[writer: and the quote]"],
            )

    def test_manifest_rejects_placeholder_with_brackets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["placeholder"] = "[FRANK]"
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaises(ProjectError):
                load_manifest(project)

    def test_manifest_rejects_bad_overrides(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            for bad in (
                {"noSuchMetric": 1},
                {"emDashesPer1k": -1},
                {"openerShare": 1.5},
                {"emDashesPer1k": "none"},
            ):
                manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
                manifest["overrides"] = bad
                (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
                with self.assertRaises(ProjectError, msg=str(bad)):
                    load_manifest(project)

    def test_curly_apostrophes_match_tells_and_structural_frames(self) -> None:
        draft = "It\u2019s worth noting. It\u2019s not a note, it\u2019s a framework."
        self.assertEqual(ban_hits(draft, ["it's worth noting"]), [("it's worth noting", 1)])
        hits = structural_tells(draft)
        self.assertTrue(any(hit.kind == "not-x-its-y" for hit in hits))

        stats = self._stats_from_draft("It's worth noting. It's not a note, it's a framework.")
        stats["weighted"]["banList"] = ["it's worth noting"]
        report = judge_text(draft, stats)
        self.assertEqual(report["banned"], [{"tell": "it's worth noting", "count": 1}])
        self.assertTrue(any(item["kind"] == "not-x-its-y" for item in report["structure"]))

    def test_code_blocks_urls_and_comments_do_not_trigger_bans_or_placeholders(self) -> None:
        draft = (
            "---\n"
            "title: delve\n"
            "---\n"
            "## [WRITER: heading gap]\n\n"
            "I write short lines. I keep them plain. [WRITER: prose gap]\n\n"
            "```python\n"
            "# It's not a note, it's a framework.\n"
            "delve = '[TODO: inside code]'\n"
            "```\n\n"
            "Inline `delve` and `[TODO: inline]` stay ignored.\n"
            "See [this guide](https://example.com/delve) <!-- delve [TODO: comment] -->.\n"
        )
        stats = self._stats_from_draft("I write short lines. I keep them plain.")
        stats["weighted"]["banList"] = ["delve"]
        report = judge_text(draft, stats)
        self.assertEqual(report["banned"], [])
        self.assertEqual([item for item in report["structure"] if item["kind"] != "em-dash"], [])
        self.assertEqual(
            report["placeholders"],
            ["[WRITER: heading gap]", "[WRITER: prose gap]"],
        )


if __name__ == "__main__":
    unittest.main()
