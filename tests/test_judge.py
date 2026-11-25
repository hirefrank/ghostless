"""Judge scoring, overrides, placeholders, and structural tell tests. Sample data is invented."""

from __future__ import annotations

import contextlib
import io
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

from ghostless.build import build_voice, stale_inputs  # noqa: E402
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
        stats: dict[str, object] = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertTrue(report["ok"])
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["assessment"], "mechanical-only")

    def test_empty_project_cannot_pass(self) -> None:
        stats: dict[str, object] = {"weighted": {"metrics": {}, "banList": []}}
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
        weighted = stats["weighted"]
        assert isinstance(weighted, dict)
        metrics = weighted["metrics"]
        assert isinstance(metrics, dict)
        self.assertNotEqual(metrics["emDashesPer1k"], 0)
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
                (project / "drafts" / "closer.md").read_text(encoding="utf-8") + "\n\nOne more line — with a dash.\n",
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
                )
                * 3,
                encoding="utf-8",
            )
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["overrides"] = {"emDashesPer1k": 0}
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

            stats = build_voice(project)
            on_disk = json.loads((project / "voice-stats.json").read_text(encoding="utf-8"))
            measured_em = stats["weighted"]["metrics"]["emDashesPer1k"]
            section_measured_em = stats["sections"]["notes"]["weighted"]["metrics"]["emDashesPer1k"]
            assert isinstance(measured_em, int | float) and not isinstance(measured_em, bool)
            assert isinstance(section_measured_em, int | float) and not isinstance(section_measured_em, bool)
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
            draft_text = (project / "drafts" / "closer.md").read_text(
                encoding="utf-8"
            ) + "\n\nOne more line — with a dash.\n"
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
            stats_mtime = (project / "voice-stats.json").stat().st_mtime_ns
            os.utime(project / "manifest.json", ns=(stats_mtime + 2_000_000_000, stats_mtime + 2_000_000_000))
            out = io.StringIO()
            err = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                main(["judge", str(draft), "--json", "--project", str(project)])
            restored = json.loads(out.getvalue())
            restored_check = next(item for item in restored["checks"] if item["metric"] == "emDashesPer1k")
            self.assertEqual(restored_check["target"], measured_em)
            self.assertTrue(restored["stale"])
            self.assertIn("manifest.json", restored["staleSources"])
            self.assertIn("manifest.json", err.getvalue())

            out = io.StringIO()
            err = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
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
            assert isinstance(measured_share, int | float) and not isinstance(measured_share, bool)
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
        stats: dict[str, object] = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["placeholders"], ["[FRANK: add the story here]"])

    def test_writer_placeholder_still_flags_and_lowercase_brackets_do_not(self) -> None:
        self.assertEqual(find_placeholders("[WRITER: your take]"), ["[WRITER: your take]"])
        self.assertEqual(find_placeholders("[TODO: fill this in]"), ["[TODO: fill this in]"])
        self.assertEqual(find_placeholders("[see page 3]"), [])
        self.assertEqual(find_placeholders("[2024: a year]"), [])
        draft = "I write short lines. [see page 3]"
        stats: dict[str, object] = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
        report = judge_text(draft, stats)
        self.assertFalse(report["reviewRequired"])
        self.assertEqual(report["placeholders"], [])

    def test_configured_placeholder_token_matches_case_insensitively(self) -> None:
        draft = "I write short lines. [frank: add the story here]"
        stats: dict[str, object] = {"weighted": {"metrics": draft_summary(draft)["metrics"], "banList": []}}
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
                (project / "drafts" / "closer.md").read_text(encoding="utf-8") + "\n\n[frank: add the story here]\n",
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
        weighted = stats["weighted"]
        assert isinstance(weighted, dict)
        weighted["banList"] = ["it's worth noting"]
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
        weighted = stats["weighted"]
        assert isinstance(weighted, dict)
        weighted["banList"] = ["delve"]
        report = judge_text(draft, stats)
        self.assertEqual(report["banned"], [])
        self.assertEqual([item for item in report["structure"] if item["kind"] != "em-dash"], [])
        self.assertEqual(
            report["placeholders"],
            ["[WRITER: heading gap]", "[WRITER: prose gap]"],
        )


class StaleInputsTests(unittest.TestCase):
    def _bump_mtime(self, path: Path, stats_path: Path) -> int:
        old_ns = path.stat().st_mtime_ns
        newer = stats_path.stat().st_mtime_ns + 2_000_000_000
        os.utime(path, ns=(newer, newer))
        return old_ns

    def test_fresh_build_is_not_stale_and_non_inputs_do_not_trigger(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            build_voice(project)
            stats_path = project / "voice-stats.json"
            self.assertEqual(stale_inputs(project), [])

            draft = project / "drafts" / "closer.md"
            self._bump_mtime(draft, stats_path)
            (project / "corpus" / "gold" / "README.md").write_text("# Ignore me\n", encoding="utf-8")
            self._bump_mtime(project / "corpus" / "gold" / "README.md", stats_path)
            session_file = project / "sessions" / "2026-01-02" / "after.md"
            session_file.parent.mkdir(parents=True, exist_ok=True)
            session_file.write_text("Working draft.\n", encoding="utf-8")
            self._bump_mtime(session_file, stats_path)
            self.assertEqual(stale_inputs(project), [])

            out = io.StringIO()
            err = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = main(["judge", str(draft), "--json", "--project", str(project)])
            self.assertEqual(code, 0)
            self.assertEqual(err.getvalue(), "")
            report = json.loads(out.getvalue())
            self.assertFalse(report["stale"])
            self.assertEqual(report["staleSources"], [])

            out_exp = io.StringIO()
            err_exp = io.StringIO()
            with contextlib.redirect_stdout(out_exp), contextlib.redirect_stderr(err_exp):
                code_exp = main(["export", "--project", str(project)])
            self.assertEqual(code_exp, 0)
            self.assertEqual(err_exp.getvalue(), "")

    def test_judge_and_export_warn_without_rewriting_stats_across_all_input_kinds(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            (project / "STYLE.md").write_text("# Style\n\n- Be direct.\n", encoding="utf-8")
            (project / "AUDIENCE.md").write_text("# Audience\n\n- Builders.\n", encoding="utf-8")
            (project / "rules.json").write_text(
                '{"kind": "ghostless-rules", "version": 1, "confirmed": {}, "rejected": {}, "retired": {}, "scope": {}}\n',
                encoding="utf-8",
            )
            (project / "checks.json").write_text(
                '{"kind": "ghostless-checks", "version": 1, "confirmed": {}, "rejected": {}}\n',
                encoding="utf-8",
            )
            (project / "transcripts").mkdir(parents=True, exist_ok=True)
            (project / "transcripts" / "talk.md").write_text("I keep it brief.\n", encoding="utf-8")
            (project / "gold" / "2026-01-02-notes").mkdir(parents=True, exist_ok=True)
            (project / "gold" / "2026-01-02-notes" / "before.md").write_text("Before.\n", encoding="utf-8")
            (project / "gold" / "2026-01-02-notes" / "after.md").write_text("After.\n", encoding="utf-8")
            (project / "gold" / "2026-01-02-notes" / "meta.json").write_text(
                '{"date": "2026-01-02", "origin": "writer-first", "beforeFile": "before.md", "afterFile": "after.md"}\n',
                encoding="utf-8",
            )
            build_voice(project)

            stats_path = project / "voice-stats.json"
            guide_path = project / "voice-guide.md"
            draft = project / "drafts" / "closer.md"

            rel_targets = (
                "manifest.json",
                "rules.json",
                "checks.json",
                "STYLE.md",
                "AUDIENCE.md",
                "corpus/gold/notes/desk.md",
                "transcripts/talk.md",
                "gold/2026-01-02-notes/after.md",
            )
            for rel in rel_targets:
                with self.subTest(rel=rel):
                    with contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(main(["build", "--project", str(project)]), 0)
                    stats_before = stats_path.read_bytes()
                    guide_before = guide_path.read_bytes()
                    stats_mtime_before = stats_path.stat().st_mtime_ns
                    guide_mtime_before = guide_path.stat().st_mtime_ns

                    old_ns = self._bump_mtime(project / rel, stats_path)
                    try:
                        out_judge = io.StringIO()
                        err_judge = io.StringIO()
                        with contextlib.redirect_stdout(out_judge), contextlib.redirect_stderr(err_judge):
                            judge_rc = main(["judge", str(draft), "--json", "--project", str(project)])
                        self.assertIn(judge_rc, (0, 1))
                        report = json.loads(out_judge.getvalue())
                        self.assertTrue(report["stale"])
                        self.assertEqual(report["staleSources"], [rel])
                        self.assertIn(rel, err_judge.getvalue())
                        self.assertIn("ghostless build", err_judge.getvalue())

                        out_export = io.StringIO()
                        err_export = io.StringIO()
                        with contextlib.redirect_stdout(out_export), contextlib.redirect_stderr(err_export):
                            export_rc = main(["export", "--project", str(project)])
                        self.assertEqual(export_rc, 0)
                        self.assertIn(rel, err_export.getvalue())
                        self.assertIn("ghostless build", err_export.getvalue())

                        self.assertEqual(stats_path.read_bytes(), stats_before)
                        self.assertEqual(guide_path.read_bytes(), guide_before)
                        self.assertEqual(stats_path.stat().st_mtime_ns, stats_mtime_before)
                        self.assertEqual(guide_path.stat().st_mtime_ns, guide_mtime_before)
                    finally:
                        restored = min(old_ns, stats_mtime_before - 1_000_000_000)
                        os.utime(project / rel, ns=(restored, restored))

            # Backdating voice-stats.json simulates every input being modified before rebuild; build clears staleness.
            past = stats_path.stat().st_mtime_ns - 2_000_000_000
            os.utime(stats_path, ns=(past, past))
            self.assertTrue(stale_inputs(project))
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["build", "--project", str(project)]), 0)
            out_clean = io.StringIO()
            err_clean = io.StringIO()
            with contextlib.redirect_stdout(out_clean), contextlib.redirect_stderr(err_clean):
                self.assertIn(main(["judge", str(draft), "--json", "--project", str(project)]), (0, 1))
            self.assertEqual(err_clean.getvalue(), "")
            cleaned = json.loads(out_clean.getvalue())
            self.assertFalse(cleaned["stale"])
            self.assertEqual(cleaned["staleSources"], [])


if __name__ == "__main__":
    unittest.main()
