#!/usr/bin/env python3
"""Confirmed edit checks: save, confirm, reject, and grade apart from judge."""

from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.checks import (  # noqa: E402
    check_id,
    grade_confirmed,
    passes_check,
)
from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.eval import evaluate  # noqa: E402
from ghostless.learn import learn_edit  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import TRACKED_PATHS, init_project  # noqa: E402
from ghostless.sessions import finish_session, start_session  # noqa: E402

WRITER = (
    "I keep the note short. I don't polish the first line.\n\n"
    "And I stop when the coffee is cold."
)
LABEL = 'The opener does not copy "Level 9:".'
MECHANICAL = "Keep the median sentence length near the target."


class CheckSaveTests(unittest.TestCase):
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
        section: str | None = "notes",
        checks: list[str] | None = None,
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
            checks=checks,
        )
        return str(result["slug"])

    def test_a_check_is_inactive_until_the_writer_confirms_it(self) -> None:
        slug = self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            checks=[LABEL],
        )
        self.assertFalse((self.project / "checks.json").exists())
        meta = json.loads((self.project / "gold" / slug / "meta.json").read_text(encoding="utf-8"))
        stored = meta["checks"][0]
        self.assertEqual(stored["text"], LABEL)
        self.assertEqual(stored["span"], "Level 9:")
        self.assertEqual(stored["polarity"], "forbid")
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            self.assertEqual(main(["checks", "--project", str(self.project)]), 0)
        self.assertIn("not active", stdout.getvalue().lower())
        self.assertIn("none yet", stdout.getvalue())

        with contextlib.redirect_stdout(io.StringIO()):
            code = main(["checks", "confirm", LABEL, "--project", str(self.project), "--section", "notes"])
        self.assertEqual(code, 0)
        decisions = json.loads((self.project / "checks.json").read_text(encoding="utf-8"))
        record = decisions["confirmed"][check_id(LABEL)]
        self.assertEqual(record["text"], LABEL)
        self.assertEqual(record["pair"], slug)
        self.assertEqual(record["section"], "notes")
        self.assertNotIn(check_id(LABEL), decisions["rejected"])

    def test_a_rejected_check_is_not_revived_by_another_edit(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            checks=[LABEL],
        )
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(main(["checks", "reject", LABEL, "--project", str(self.project)]), 0)
        with self.assertRaisesRegex(ProjectError, "stays rejected"):
            self.learn(
                "Level 9: opens the other note. Then we stop.",
                "The correction opens the other note. Then we stop.",
                checks=[LABEL],
                date="2026-01-03",
            )
        # The failed learn does not create a pair, and the rejection is still the decision.
        self.assertEqual(len(list((self.project / "gold").glob("*/meta.json"))), 1)
        summary = io.StringIO()
        with contextlib.redirect_stdout(summary):
            main(["checks", "--project", str(self.project)])
        text = summary.getvalue()
        pending, _, rejected = text.partition("Rejected")
        self.assertNotIn(LABEL, pending)
        self.assertIn(LABEL, rejected)

    def test_explicit_confirm_can_reverse_a_rejection(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            checks=[LABEL],
        )
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "reject", check_id(LABEL), "--project", str(self.project)])
            code = main(["checks", "confirm", check_id(LABEL), "--project", str(self.project)])
        self.assertEqual(code, 0)
        decisions = json.loads((self.project / "checks.json").read_text(encoding="utf-8"))
        self.assertIn(check_id(LABEL), decisions["confirmed"])
        self.assertNotIn(check_id(LABEL), decisions["rejected"])

    def test_mechanical_judgments_and_tells_are_refused(self) -> None:
        with self.assertRaisesRegex(ProjectError, "sentence length"):
            self.learn(
                "Level 9: starts the note. Then we stop.",
                "The correction starts the note. Then we stop.",
                checks=[MECHANICAL],
            )
        self.assertEqual(list((self.project / "gold").glob("*/meta.json")), [])
        with self.assertRaisesRegex(ProjectError, "judge already scores"):
            self.learn(
                "We delve into the work today and then stop.",
                "We go into the work today and then stop.",
                checks=['Cut the word "delve" here.'],
            )

    def test_a_quote_must_be_a_change_in_this_edit(self) -> None:
        with self.assertRaisesRegex(ProjectError, "not in this edit"):
            self.learn(
                "Level 9: starts the note. Then we stop.",
                "The correction starts the note. Then we stop.",
                checks=['The opener names "a missing label".'],
            )
        with self.assertRaisesRegex(ProjectError, "both the before and the after"):
            self.learn(
                "Level 9: starts the note. Then we stop.",
                "Level 9: starts the note. Then we pause.",
                checks=['Keep the label "Level 9:".'],
            )

    def test_several_phrases_need_a_quote_and_one_phrase_does_not(self) -> None:
        with self.assertRaisesRegex(ProjectError, "more than one phrase"):
            self.learn(
                "See Level 9 before the agent jargon starts.",
                "See the correction before the explanation starts.",
                checks=["The opener states the problem."],
            )
        slug = self.learn(
            "Read the Level 9 note today.",
            "Read the note today.",
            checks=["The opener does not copy a raw label."],
            date="2026-01-04",
        )
        meta = json.loads((self.project / "gold" / slug / "meta.json").read_text(encoding="utf-8"))
        self.assertEqual(meta["checks"][0]["span"], "Level 9")
        self.assertEqual(meta["checks"][0]["polarity"], "forbid")
        self.assertTrue(passes_check("Read the note today.", "forbid", "Level 9"))
        self.assertFalse(passes_check("Read the Level 9 note today.", "forbid", "Level 9"))

    def test_an_added_span_is_required_and_the_after_passes(self) -> None:
        slug = self.learn(
            "Read the note today.",
            "Read the correction note today.",
            checks=['The opener names "the correction".'],
        )
        meta = json.loads((self.project / "gold" / slug / "meta.json").read_text(encoding="utf-8"))
        self.assertEqual(meta["checks"][0]["polarity"], "require")
        after = (self.project / "gold" / slug / "after.md").read_text(encoding="utf-8")
        before = (self.project / "gold" / slug / "before.md").read_text(encoding="utf-8")
        self.assertTrue(passes_check(after, "require", meta["checks"][0]["span"]))
        self.assertFalse(passes_check(before, "require", meta["checks"][0]["span"]))

    def test_finish_can_save_the_check_from_the_session(self) -> None:
        draft = self.source / "draft.md"
        draft.write_text("Level 9: starts the note. Then we stop.\n", encoding="utf-8")
        session = start_session(
            self.project,
            draft,
            section="notes",
            origin="writer-first",
            sources=[],
        )
        folder = self.project / str(session["path"])
        (folder / "after.md").write_text("The correction starts the note. Then we stop.\n", encoding="utf-8")
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = main(
                [
                    "finish",
                    "--session",
                    str(session["id"]),
                    "--check",
                    LABEL,
                    "--project",
                    str(self.project),
                ]
            )
        self.assertEqual(code, 0)
        self.assertIn(check_id(LABEL), stdout.getvalue())
        meta = json.loads((self.project / "gold" / str(session["id"]) / "meta.json").read_text(encoding="utf-8"))
        self.assertEqual(meta["checks"][0]["text"], LABEL)

    def test_build_renders_checks_and_does_not_write_decisions(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            checks=[LABEL],
        )
        build_voice(self.project)
        self.assertFalse((self.project / "checks.json").exists())
        guide = (self.project / "voice-guide.md").read_text(encoding="utf-8")
        self.assertIn("## Confirmed checks", guide)
        self.assertIn("not active", guide)
        self.assertIn(LABEL, guide)
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", LABEL, "--project", str(self.project)])
        before = (self.project / "checks.json").read_text(encoding="utf-8")
        build_voice(self.project)
        self.assertEqual((self.project / "checks.json").read_text(encoding="utf-8"), before)
        guide = (self.project / "voice-guide.md").read_text(encoding="utf-8")
        confirmed = guide.split("### Confirmed\n", 1)[1].split("### Not confirmed yet", 1)[0]
        self.assertIn(LABEL, confirmed)
        self.assertIn("the edit cut", confirmed)
        self.assertNotIn("not active", confirmed)
        pending = guide.split("### Not confirmed yet", 1)[1]
        self.assertTrue(pending.lstrip().startswith("None."))

    def test_checks_json_is_tracked_and_a_bad_file_fails_loudly(self) -> None:
        self.assertIn("checks.json", TRACKED_PATHS)
        ignore = (self.project / ".gitignore").read_text(encoding="utf-8")
        self.assertNotIn("checks.json", [line.strip() for line in ignore.splitlines()])
        (self.project / "checks.json").write_text("{not json\n", encoding="utf-8")
        with self.assertRaisesRegex(ProjectError, "checks.json"):
            build_voice(self.project)


class CheckGradeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)
        (self.project / "corpus" / "gold" / "desk.md").write_text(WRITER + "\n", encoding="utf-8")
        self.source = self.base / "src"
        self.source.mkdir()
        self.count = 0

    def learn(
        self,
        before: str,
        after: str,
        *,
        section: str | None,
        checks: list[str] | None = None,
        date: str,
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
            origin="ai-first",
            date=date,
            checks=checks,
        )
        return str(result["slug"])

    def test_eval_reports_passes_over_checks_and_leaves_the_mechanical_score(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            section="notes",
            checks=[LABEL],
            date="2026-01-02",
        )
        self.learn(
            "The essay starts without that label. Then we stop.",
            "The essay starts with the point. Then we stop.",
            section="essay",
            date="2026-01-03",
        )
        before = evaluate(self.project, holdout=2, seed=0)
        self.assertTrue(before["ok"], before)
        self.assertIsNone(before["confirmedChecks"]["label"])
        scores = [item["score"] for item in before["items"]]

        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", LABEL, "--section", "notes", "--project", str(self.project)])
        after = evaluate(self.project, holdout=2, seed=0)
        self.assertEqual([item["score"] for item in after["items"]], scores)
        self.assertEqual(after["correlation"], before["correlation"])
        self.assertEqual(after["verdict"], before["verdict"])
        # Notes before fails, the after passes, the essay before is out of section.
        self.assertEqual(after["confirmedChecks"]["label"], "1/2")
        self.assertTrue(after["confirmedChecks"]["fresh"])
        self.assertEqual(after["confirmedChecks"]["passes"], 1)
        self.assertEqual(after["confirmedChecks"]["total"], 2)
        cases = {(item["slug"], item["case"]): item["pass"] for item in after["confirmedChecks"]["items"]}
        notes = "2026-01-02-notes"
        self.assertFalse(cases[(notes, "before")])
        self.assertTrue(cases[(notes, "after")])
        self.assertNotIn(("2026-01-03-essay", "before"), cases)
        repeated = evaluate(self.project, holdout=2, seed=0)
        self.assertFalse(repeated["confirmedChecks"]["fresh"])
        self.assertEqual(repeated["confirmedChecks"]["session"], after["confirmedChecks"]["session"])
        self.assertEqual([item["score"] for item in repeated["items"]], scores)

    def test_a_guide_or_skill_change_opens_a_fresh_grade_session(self) -> None:
        slug = self.learn(
            "Read the Level 9 note today.",
            "Read the note today.",
            section="notes",
            checks=["The opener does not copy a raw label."],
            date="2026-01-02",
        )
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", "The opener does not copy a raw label.", "--project", str(self.project)])
        held = [
            {
                "slug": slug,
                "section": "notes",
                "before": "Read the Level 9 note today.\n",
            }
        ]
        skill = self.base / "SKILL.md"
        skill.write_text("Draft from the guide.\n", encoding="utf-8")
        guide = self.project / "voice-guide.md"
        guide.write_text("# Voice guide\n\nFirst.\n", encoding="utf-8")
        first = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
        second = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
        self.assertTrue(first["fresh"])
        self.assertFalse(second["fresh"])
        self.assertEqual(first["session"], second["session"])
        self.assertEqual(first["label"], "1/2")
        guide.write_text("# Voice guide\n\nThe guide changed.\n", encoding="utf-8")
        changed_guide = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
        self.assertTrue(changed_guide["fresh"])
        self.assertNotEqual(changed_guide["session"], first["session"])
        skill.write_text("Draft from the guide.\nRead the confirmed checks.\n", encoding="utf-8")
        changed_skill = grade_confirmed(
            self.project, held, seed=0, skill_path=skill, guide_path=guide
        )
        self.assertTrue(changed_skill["fresh"])
        self.assertNotEqual(changed_skill["session"], changed_guide["session"])
        self.assertEqual(changed_skill["label"], "1/2")
        folder = self.project / "stats" / "check-grades"
        self.assertEqual(len(list(folder.glob("*.json"))), 3)

    def test_eval_prints_the_pass_count_without_blending_it(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            section="notes",
            checks=[LABEL],
            date="2026-01-02",
        )
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", LABEL, "--project", str(self.project)])
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = main(["eval", "--project", str(self.project), "--holdout", "1", "--json"])
        self.assertEqual(code, 0)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(payload["confirmedChecks"]["label"], "1/2")
        self.assertNotIn("blended", payload)
        self.assertIn("score", payload["items"][0])
        text = io.StringIO()
        with contextlib.redirect_stdout(text):
            main(["eval", "--project", str(self.project), "--holdout", "1"])
        rendered = text.getvalue()
        self.assertIn("confirmed checks: 1/2", rendered)
        self.assertIn("correlation:", rendered)
        self.assertNotIn("%", rendered.split("confirmed checks:")[1].split("\n", 1)[0])


if __name__ == "__main__":
    os.environ.setdefault("GHOSTLESS_NO_UPDATE", "1")
    unittest.main()
