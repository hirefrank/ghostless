#!/usr/bin/env python3
"""Enforcement of writer-confirmed cut rules in ghostless judge."""

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

from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.judge import draft_summary, format_report, judge_text  # noqa: E402
from ghostless.learn import learn_edit  # noqa: E402
from ghostless.rules import confirm_rule, reject_rule, retire_rule  # noqa: E402


class ConfirmedRulesJudgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = self.base / "voice"
        shutil.copytree(SAMPLE, self.project)
        self.source = self.base / "src"
        self.source.mkdir()
        self.count = 0

    def _learn(
        self,
        before: str,
        after: str,
        *,
        section: str | None = None,
        date: str = "2026-01-02",
    ) -> str:
        self.count += 1
        before_path = self.source / f"before-{self.count}.md"
        after_path = self.source / f"after-{self.count}.md"
        before_path.write_text(before + "\n", encoding="utf-8")
        after_path.write_text(after + "\n", encoding="utf-8")
        result = learn_edit(
            self.project,
            before=before_path,
            after=after_path,
            section=section,
            origin="writer-first",
            date=date,
        )
        return str(result["slug"])

    def _learn_three_cuts(self, word: str, *, section: str | None = None) -> None:
        for index in range(3):
            self._learn(
                f"I {word} keep note {index} short. I stop here.",
                f"I keep note {index} short. I stop here.",
                section=section,
            )

    def _learn_three_adds(self, word: str, *, section: str | None = None) -> None:
        for index in range(3):
            self._learn(
                f"I keep note {index} short. I stop here.",
                f"I {word} keep note {index} short. I stop here.",
                section=section,
            )

    def _closer_text(self) -> str:
        return (self.project / "drafts" / "closer.md").read_text(encoding="utf-8").strip()

    def test_confirmed_cut_rule_flags_review_lowers_score_and_formats_report(self) -> None:
        self._learn_three_cuts("actually")
        baseline_stats = build_voice(self.project)
        draft = self._closer_text() + "\n\nI actually keep the note short."

        # Still a hypothesis before confirmation: must not fire or penalize.
        baseline = judge_text(draft, baseline_stats)
        self.assertEqual(baseline["confirmedRules"], [])
        self.assertFalse(baseline["reviewRequired"])
        self.assertNotIn("Confirmed rules", format_report(baseline))

        confirm_rule(self.project, "cut:actually")
        stats = build_voice(self.project)
        report = judge_text(draft, stats)

        self.assertEqual(
            report["confirmedRules"],
            [{"id": "cut:actually", "word": "actually", "scope": None, "count": 1}],
        )
        self.assertTrue(report["reviewRequired"])
        self.assertEqual(report["score"], baseline["score"] - 8)

        rendered = format_report(report)
        self.assertIn("Confirmed rules", rendered)
        self.assertIn("cut:actually (1)", rendered)
        self.assertIn("Review flagged patterns and placeholders before publishing.", rendered)
        self.assertLess(rendered.index("Banned words"), rendered.index("Confirmed rules"))
        self.assertLess(rendered.index("Confirmed rules"), rendered.index("Structure"))

    def test_section_scoped_cut_rule_enforces_only_matching_section(self) -> None:
        self._learn_three_cuts("actually", section="notes")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = main(
                [
                    "rules",
                    "confirm",
                    "cut:actually",
                    "--section",
                    "notes",
                    "--project",
                    str(self.project),
                ]
            )
        self.assertEqual(code, 0)
        stats = build_voice(self.project)

        draft = self._closer_text() + "\n\nActually, I stop before it gets tidy."
        notes_report = judge_text(draft, stats, section="notes")
        archive_report = judge_text(draft, stats, section="archive")
        all_report = judge_text(draft, stats, section=None)

        self.assertEqual(
            notes_report["confirmedRules"],
            [{"id": "cut:actually", "word": "actually", "scope": "notes", "count": 1}],
        )
        self.assertTrue(notes_report["reviewRequired"])
        notes_rendered = format_report(notes_report)
        self.assertIn("Confirmed rules", notes_rendered)
        self.assertIn("cut:actually (1) [notes]", notes_rendered)

        self.assertEqual(archive_report["confirmedRules"], [])
        self.assertNotIn("Confirmed rules", format_report(archive_report))

        self.assertEqual(all_report["confirmedRules"], [])
        self.assertNotIn("Confirmed rules", format_report(all_report))

        draft_file = self.base / "draft.md"
        draft_file.write_text(draft + "\n", encoding="utf-8")
        cli_notes = io.StringIO()
        with contextlib.redirect_stdout(cli_notes):
            main(["judge", str(draft_file), "--section", "notes", "--project", str(self.project)])
        self.assertIn("Confirmed rules", cli_notes.getvalue())
        self.assertIn("cut:actually (1) [notes]", cli_notes.getvalue())

        cli_archive = io.StringIO()
        with contextlib.redirect_stdout(cli_archive):
            main(["judge", str(draft_file), "--section", "archive", "--project", str(self.project)])
        self.assertNotIn("Confirmed rules", cli_archive.getvalue())

    def test_hypotheses_rejected_retired_and_confirmed_add_rules_never_fire(self) -> None:
        self._learn_three_cuts("actually")
        self._learn_three_adds("honestly")

        draft_with_both = (
            self._closer_text() + "\n\nI actually keep it short. I honestly keep it plain."
        )
        draft_without_add = self._closer_text() + "\n\nI actually keep it short."

        # Baseline with no rules confirmed (both are hypotheses).
        stats_hypotheses = build_voice(self.project)
        hyp_report = judge_text(draft_with_both, stats_hypotheses)
        self.assertEqual(hyp_report["confirmedRules"], [])
        hyp_score_without_add = judge_text(draft_without_add, stats_hypotheses)["score"]

        # Confirming add:honestly never fires whether the word is present or absent.
        confirm_rule(self.project, "add:honestly")
        stats_add = build_voice(self.project)
        add_present = judge_text(draft_with_both, stats_add)
        add_absent = judge_text(draft_without_add, stats_add)
        self.assertEqual(add_present["confirmedRules"], [])
        self.assertEqual(add_absent["confirmedRules"], [])
        self.assertEqual(add_present["score"], hyp_report["score"])
        self.assertEqual(add_absent["score"], hyp_score_without_add)

        # Rejecting cut:actually never fires.
        reject_rule(self.project, "cut:actually")
        stats_rejected = build_voice(self.project)
        rej_report = judge_text(draft_with_both, stats_rejected)
        self.assertEqual(rej_report["confirmedRules"], [])
        self.assertEqual(rej_report["score"], hyp_report["score"])

        # Confirming then retiring cut:actually never fires.
        confirm_rule(self.project, "cut:actually")
        retire_rule(self.project, "cut:actually")
        stats_retired = build_voice(self.project)
        ret_report = judge_text(draft_with_both, stats_retired)
        self.assertEqual(ret_report["confirmedRules"], [])
        self.assertEqual(ret_report["score"], hyp_report["score"])

    def test_double_penalty_guard_when_cut_word_is_already_banned(self) -> None:
        self._learn_three_cuts("delve")
        baseline_stats = build_voice(self.project)
        self.assertIn("delve", baseline_stats["weighted"]["banList"])

        draft = self._closer_text() + "\n\nWe delve into one small detail."
        baseline = judge_text(draft, baseline_stats)
        self.assertEqual(baseline["banned"], [{"tell": "delve", "count": 1}])
        self.assertEqual(baseline["confirmedRules"], [])

        confirm_rule(self.project, "cut:delve")
        stats = build_voice(self.project)
        self.assertIn("delve", stats["weighted"]["banList"])
        self.assertEqual(
            [item["id"] for item in stats["rules"]["confirmed"]],
            ["cut:delve"],
        )

        report = judge_text(draft, stats)
        self.assertEqual(report["banned"], [{"tell": "delve", "count": 1}])
        self.assertEqual(report["confirmedRules"], [])
        self.assertEqual(report["score"], baseline["score"])
        self.assertNotIn("Confirmed rules", format_report(report))

    def test_penalty_cap_non_prose_skip_dedup_and_early_returns(self) -> None:
        draft = (
            "I keep lines short. Actually, I pause. "
            "Actually, I check again. Actually, I wait. Actually, I stop.\n\n"
            "```python\nactually = 1\n```\n"
            "<!-- actually -->\n"
            "See [link](https://example.com/actually).\n"
        )
        metrics = draft_summary(draft)["metrics"]
        stats: dict[str, object] = {
            "weighted": {"metrics": metrics, "banList": ["delve"]},
            "sections": {"notes": {"weighted": {"metrics": metrics, "banList": ["delve"]}}},
            "rules": {
                "confirmed": [
                    {"id": "cut:actually", "kind": "cut", "word": "actually", "scope": "notes"},
                    {"id": "cut:Actually", "kind": "cut", "word": " Actually ", "scope": "  "},
                    {"id": "cut:delve", "kind": "cut", "word": "Delve", "scope": None},
                    {"id": "cut:blank", "kind": "cut", "word": "   ", "scope": None},
                ]
            },
        }
        report = judge_text("Delve into this. " + draft, stats, section="notes")
        self.assertEqual(report["banned"], [{"tell": "delve", "count": 1}])
        # Unscoped entry wins over scoped duplicate, and 4 prose hits cap penalty at 24.
        self.assertEqual(
            report["confirmedRules"],
            [{"id": "cut:Actually", "word": "Actually", "scope": None, "count": 4}],
        )
        # 10 (1 banned delve) + 24 (capped rule penalty) = 34 penalty -> 66 if all checks ok;
        # verify exact 24-point difference vs stats without confirmed cut rules.
        stats_no_cuts = json.loads(json.dumps(stats))
        stats_no_cuts["rules"] = {"confirmed": []}
        without_cuts = judge_text("Delve into this. " + draft, stats_no_cuts, section="notes")
        self.assertEqual(report["score"], without_cuts["score"] - 24)

        empty = judge_text("", stats)
        self.assertEqual(empty["confirmedRules"], [])
        no_targets = judge_text(
            "Actually, this has no targets.",
            {"weighted": {"metrics": {}, "banList": []}, "rules": stats["rules"]},
        )
        self.assertEqual(no_targets["confirmedRules"], [])


if __name__ == "__main__":
    unittest.main()
