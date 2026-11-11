#!/usr/bin/env python3
"""Voice rules: support threshold, confirmation, rejection, and scoping.

Sample data is invented.
"""

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

from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.learn import learn_edit, refresh_edit_summary  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import TRACKED_PATHS, init_project  # noqa: E402
from ghostless.rules import (  # noqa: E402
    active_rules,
    confirm_rule,
    default_rules,
    derive_status,
    load_rules,
    mine_candidates,
    reject_rule,
    retire_rule,
    rules_summary,
    scope_rule,
)
from ghostless.sessions import finish_session, snapshot_session, start_session  # noqa: E402


class RuleMiningTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)
        self.source = self.base / "src"
        self.source.mkdir()
        self.count = 0

    def learn(
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

    def candidates(self) -> dict[str, dict[str, object]]:
        edits = refresh_edit_summary(self.project)
        return {
            str(item["id"]): item for item in mine_candidates(self.project, edits)
        }

    def cut_delve(self, index: int, *, section: str | None = None) -> str:
        return self.learn(
            f"We delve into idea {index} here. Then we stop.",
            f"We go into idea {index} here. Then we stop.",
            section=section,
        )

    def test_one_or_two_pieces_are_not_candidates(self) -> None:
        first = self.cut_delve(1)
        second = self.cut_delve(2)
        found = self.candidates()
        self.assertNotIn("cut:delve", found)
        # The same word cut twice is still sparse, even with two receipts.
        self.assertEqual(refresh_edit_summary(self.project)["learns"][0]["removedWords"][0]["word"], "delve")
        self.assertTrue(first and second)

        third = self.cut_delve(3)
        candidate = self.candidates()["cut:delve"]
        self.assertEqual(candidate["support"], 3)
        self.assertEqual(sorted(candidate["pieces"]), sorted([first, second, third]))
        # Sparse input is never a hypothesis, even if a caller asks directly.
        sparse = {"id": "cut:delve", "support": 2, "minimumSupport": 3}
        self.assertEqual(derive_status(sparse, default_rules()), "sparse")

    def test_additions_are_mined_too(self) -> None:
        for index in range(3):
            self.learn(
                f"The result is clear in case {index}.",
                f"The result is honestly clear in case {index}.",
            )
        candidate = self.candidates()["add:honestly"]
        self.assertEqual(candidate["kind"], "add")
        self.assertEqual(candidate["support"], 3)

    def test_counterexamples_are_reported(self) -> None:
        for index in range(3):
            self.cut_delve(index)
        kept = self.learn(
            "We delve into the counter idea here. Then we stop.",
            "We delve into the counter idea here. Then we pause.",
        )
        candidate = self.candidates()["cut:delve"]
        self.assertEqual(candidate["support"], 3)
        self.assertEqual([item["slug"] for item in candidate["counterexamples"]], [kept])

    def test_minimum_support_can_be_lowered_in_the_manifest(self) -> None:
        manifest_path = self.project / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["rules"] = {"minimumSupport": 2}
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        self.cut_delve(1)
        self.cut_delve(2)
        candidate = self.candidates()["cut:delve"]
        self.assertEqual(candidate["minimumSupport"], 2)
        self.assertEqual(candidate["support"], 2)

    def test_examples_show_before_and_after(self) -> None:
        self.cut_delve(1)
        self.cut_delve(2)
        self.cut_delve(3)
        example = self.candidates()["cut:delve"]["examples"][0]
        self.assertIn("delve", example["before_snippet"])
        self.assertNotIn("delve", example["after_snippet"])
        self.assertIn("receipt", example)


class RuleDecisionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)
        self.source = self.base / "src"
        self.source.mkdir()
        self.count = 0

    def learn(
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

    def three_cuts(self, *, sections: tuple[str | None, ...] = ("notes", "essays", "notes")) -> list[str]:
        slugs: list[str] = []
        for index, section in enumerate(sections):
            slugs.append(
                self.learn(
                    f"We delve into idea {index} here. Then we stop.",
                    f"We go into idea {index} here. Then we stop.",
                    section=section,
                )
            )
        return slugs

    def test_confirmation_activates_a_rule_and_keeps_receipts(self) -> None:
        slugs = self.three_cuts()
        summary = rules_summary(self.project)
        self.assertEqual([item["id"] for item in summary["hypotheses"]], ["cut:delve"])
        self.assertEqual(summary["confirmed"], [])

        result = confirm_rule(self.project, "cut:delve")
        self.assertEqual(result["status"], "confirmed")
        self.assertEqual(sorted(result["pieces"]), sorted(slugs))
        self.assertTrue(result["receipts"])
        self.assertEqual(sorted(result["receipts"]), sorted(f"gold/{slug}" for slug in slugs))

        state = load_rules(self.project)
        self.assertIn("cut:delve", state["confirmed"])
        self.assertIn("at", state["confirmed"]["cut:delve"])
        confirmed = active_rules(self.project)
        self.assertEqual([item["id"] for item in confirmed], ["cut:delve"])
        self.assertIsNone(confirmed[0]["scope"])
        # The confirmation is traceable to the receipts it was based on.
        self.assertEqual(
            sorted(confirmed[0]["receipts"]),
            sorted(state["confirmed"]["cut:delve"]["receipts"]),
        )

    def test_confirmed_rule_appears_in_the_guide(self) -> None:
        self.three_cuts()
        confirm_rule(self.project, "cut:delve", section="notes")
        build_voice(self.project)
        guide = (self.project / "voice-guide.md").read_text(encoding="utf-8")
        self.assertIn("## Voice rules", guide)
        self.assertIn("Measured targets live under Targets", guide)
        self.assertLess(guide.index("## Voice rules"), guide.index("## Targets"))
        confirmed = guide.split("### Confirmed rules", 1)[1].split("### Hypotheses", 1)[0]
        self.assertIn("Cut `delve`", confirmed)
        self.assertIn("scope: notes", confirmed)
        stats = json.loads((self.project / "voice-stats.json").read_text(encoding="utf-8"))
        self.assertEqual([item["id"] for item in stats["rules"]["confirmed"]], ["cut:delve"])
        # A rebuild derives the guide but never rewrites the decision.
        self.assertEqual(stats["rules"]["confirmed"][0]["support"], 3)

    def test_guide_lists_hypotheses_apart_from_confirmed_rules(self) -> None:
        self.three_cuts()
        build_voice(self.project)
        guide = (self.project / "voice-guide.md").read_text(encoding="utf-8")
        self.assertIn("Hypotheses are not instructions", guide)
        confirmed = guide.split("### Confirmed rules", 1)[1].split("### Hypotheses", 1)[0]
        hypotheses = guide.split("### Hypotheses", 1)[1].split("## Targets", 1)[0]
        self.assertNotIn("Cut `delve`", confirmed)
        self.assertIn("Cut `delve` (3 piece(s)", hypotheses)
        self.assertIn("→", hypotheses)

    def test_rejection_survives_rebuilds_and_is_not_reintroduced(self) -> None:
        self.three_cuts()
        reject_rule(self.project, "cut:delve")
        decisions = (self.project / "rules.json").read_text(encoding="utf-8")
        self.assertIn("cut:delve", load_rules(self.project)["rejected"])

        build_voice(self.project)
        summary = rules_summary(self.project)
        self.assertEqual(summary["hypotheses"], [])
        self.assertEqual(summary["confirmed"], [])
        self.assertEqual([item["id"] for item in summary["rejected"]], ["cut:delve"])
        guide = (self.project / "voice-guide.md").read_text(encoding="utf-8")
        self.assertIn("Rejected or retired", guide)
        self.assertIn("cut:delve (rejected)", guide)
        stats = json.loads((self.project / "voice-stats.json").read_text(encoding="utf-8"))
        self.assertEqual(stats["rules"]["confirmed"], [])
        self.assertEqual(stats["rules"]["hypotheses"], [])

        # A second rebuild cannot resurrect it either.
        build_voice(self.project)
        self.assertEqual(
            (self.project / "rules.json").read_text(encoding="utf-8"),
            decisions,
        )
        self.assertIn("cut:delve", load_rules(self.project)["rejected"])

    def test_section_scoped_rule_and_cli_decisions(self) -> None:
        self.three_cuts(sections=("notes", "essays", "notes"))
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = main(
                [
                    "rules",
                    "confirm",
                    "cut:delve",
                    "--section",
                    "notes",
                    "--project",
                    str(self.project),
                ]
            )
        self.assertEqual(code, 0)
        self.assertIn("cut:delve: confirmed", stdout.getvalue())
        self.assertEqual(active_rules(self.project)[0]["scope"], "notes")

        self.assertEqual(scope_rule(self.project, "cut:delve", "essays")["scope"], "essays")
        self.assertEqual(active_rules(self.project)[0]["scope"], "essays")
        with self.assertRaisesRegex(ProjectError, "not a confirmed rule"):
            scope_rule(self.project, "cut:missing", "notes")

        listing = io.StringIO()
        with contextlib.redirect_stdout(listing):
            self.assertEqual(main(["rules", "--project", str(self.project)]), 0)
        self.assertIn("Confirmed (active)", listing.getvalue())
        self.assertIn("scope: essays", listing.getvalue())

    def test_retire_keeps_the_decision_and_removes_the_rule(self) -> None:
        self.three_cuts()
        confirm_rule(self.project, "cut:delve")
        retired = retire_rule(self.project, "cut:delve")
        self.assertEqual(retired["status"], "retired")
        self.assertEqual(active_rules(self.project), [])
        summary = rules_summary(self.project)
        self.assertEqual(summary["confirmed"], [])
        self.assertEqual([item["id"] for item in summary["retired"]], ["cut:delve"])
        build_voice(self.project)
        guide = (self.project / "voice-guide.md").read_text(encoding="utf-8")
        self.assertIn("cut:delve (retired)", guide)
        self.assertNotIn("Cut `delve` (", guide.split("### Hypotheses")[0])
        with self.assertRaisesRegex(ProjectError, "nothing to retire"):
            retire_rule(self.project, "cut:delve")

    def test_unknown_rule_ids_are_rejected(self) -> None:
        self.three_cuts()
        with self.assertRaisesRegex(ProjectError, "No voice-rule candidate"):
            confirm_rule(self.project, "cut:unknown")
        with self.assertRaisesRegex(ProjectError, "No voice-rule candidate"):
            reject_rule(self.project, "cut:unknown")
        self.assertFalse((self.project / "rules.json").exists())
        self.assertEqual(main(["rules", "confirm", "--project", str(self.project)]), 2)

    def test_session_receipt_is_the_traceability_source(self) -> None:
        for index in range(2):
            self.learn(
                f"We delve into idea {index} here. Then we stop.",
                f"We go into idea {index} here. Then we stop.",
            )
        draft = self.base / "session-draft.md"
        draft.write_text("We delve into the session piece here. Then we stop.\n", encoding="utf-8")
        session = start_session(
            self.project,
            draft,
            section=None,
            origin="writer-first",
            sources=[],
        )
        snapshot_session(
            self.project,
            session=str(session["id"]),
            after="We go into the session piece here. Then we stop.\n",
            notes="Cut the tell.",
        )
        finish_session(self.project, None, session=str(session["id"]))

        edits = refresh_edit_summary(self.project)
        candidate = next(
            item for item in mine_candidates(self.project, edits) if item["id"] == "cut:delve"
        )
        receipt = f"sessions/{session['id']}/receipt.json"
        self.assertIn(receipt, candidate["receipts"])
        result = confirm_rule(self.project, "cut:delve")
        self.assertIn(receipt, result["receipts"])


class RuleFileTests(unittest.TestCase):
    def test_rules_json_is_tracked_and_build_never_writes_it(self) -> None:
        self.assertIn("rules.json", TRACKED_PATHS)
        with tempfile.TemporaryDirectory() as tmp:
            project = init_project(Path(tmp) / "voice", git=False)
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            # The tracked-file comment mentions rules.json; no ignore rule may.
            self.assertNotIn("rules.json", [line.strip() for line in ignore.splitlines()])
            build_voice(project)
            self.assertFalse((project / "rules.json").exists())
            self.assertEqual(load_rules(project)["confirmed"], {})

    def test_malformed_rules_json_fails_loudly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = init_project(Path(tmp) / "voice", git=False)
            (project / "rules.json").write_text("{not json\n", encoding="utf-8")
            with self.assertRaisesRegex(ProjectError, "rules.json"):
                load_rules(project)


if __name__ == "__main__":
    unittest.main()
