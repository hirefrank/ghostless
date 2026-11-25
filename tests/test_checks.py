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
from typing import Any
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.checks import (  # noqa: E402
    check_id,
    checks_summary,
    confirm_check,
    grade_confirmed,
    passes_check,
)
from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.eval import evaluate  # noqa: E402
from ghostless.harness import install_skills  # noqa: E402
from ghostless.learn import learn_edit  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import TRACKED_PATHS, init_project  # noqa: E402
from ghostless.sessions import start_session  # noqa: E402

WRITER = "I keep the note short. I don't polish the first line.\n\nAnd I stop when the coffee is cold."
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

    def test_a_wrapped_span_is_the_same_phrase_and_a_heading_counts(self) -> None:
        wrapped = self.learn(
            "Keep the writer’s\nlabel here.",
            "Keep the plain label here.",
            checks=['The edit must forbid "writer’s label".'],
        )
        meta = json.loads((self.project / "gold" / wrapped / "meta.json").read_text(encoding="utf-8"))
        span = meta["checks"][0]["span"]
        self.assertEqual(span, "writer’s label")
        self.assertFalse(passes_check("Keep the writer’s  label here.", "forbid", span))
        self.assertFalse(passes_check("Keep the writer’s\tlabel here.", "forbid", span))
        self.assertTrue(passes_check("Keep the plain label here.", "forbid", span))
        # `writer` is not the possessive `writer's`.
        self.assertTrue(passes_check("The writer's desk is ready.", "forbid", "writer"))
        self.assertFalse(passes_check("The writer left.", "forbid", "writer"))

        heading = self.learn(
            "# Level 9: starts the note.\n\nThen we stop.",
            "# The correction starts the note.\n\nThen we stop.",
            checks=[LABEL],
            date="2026-01-05",
        )
        heading_meta = json.loads((self.project / "gold" / heading / "meta.json").read_text(encoding="utf-8"))
        self.assertEqual(heading_meta["checks"][0]["span"], "Level 9:")
        self.assertEqual(heading_meta["checks"][0]["polarity"], "forbid")

        auto = self.learn(
            "Read the Level\n9 note today.",
            "Read the note today.",
            checks=["The opener does not copy a raw label."],
            date="2026-01-06",
        )
        auto_meta = json.loads((self.project / "gold" / auto / "meta.json").read_text(encoding="utf-8"))
        self.assertEqual(auto_meta["checks"][0]["span"], "Level 9")

    def test_smart_quotes_do_not_revive_a_rejected_check_or_hide_a_tell(self) -> None:
        straight = 'The edit must forbid "writer\'s label".'
        curly = 'The edit must forbid "writer’s label".'
        self.assertEqual(check_id(straight), check_id(curly))
        self.learn(
            "Keep the writer's label here.",
            "Keep the plain label here.",
            checks=[straight],
        )
        with contextlib.redirect_stdout(io.StringIO()):
            code = main(["checks", "confirm", curly, "--project", str(self.project)])
        self.assertEqual(code, 0)
        decisions = json.loads((self.project / "checks.json").read_text(encoding="utf-8"))
        self.assertIn(check_id(straight), decisions["confirmed"])
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "reject", curly, "--project", str(self.project)])
        with self.assertRaisesRegex(ProjectError, "stays rejected"):
            self.learn(
                "Keep the writer's label in the other note.",
                "Keep the plain label in the other note.",
                checks=[curly],
                date="2026-01-07",
            )
        mixed = self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            checks=['The opener does not copy “Level 9:".'],
            date="2026-01-08",
        )
        mixed_meta = json.loads((self.project / "gold" / mixed / "meta.json").read_text(encoding="utf-8"))
        self.assertEqual(mixed_meta["checks"][0]["span"], "Level 9:")
        with self.assertRaisesRegex(ProjectError, "judge already scores"):
            self.learn(
                "It’s worth noting the gap. Then we stop.",
                "The gap is the point. Then we stop.",
                checks=['Drop "it’s worth noting." from the opener.'],
                date="2026-01-09",
            )

    def test_a_second_finish_keeps_checks_already_on_the_receipt(self) -> None:
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
        added = 'The opener names "the correction".'
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                main(["finish", "--session", str(session["id"]), "--check", LABEL, "--project", str(self.project)]),
                0,
            )
            self.assertEqual(
                main(["finish", "--session", str(session["id"]), "--check", added, "--project", str(self.project)]),
                0,
            )
        receipt = json.loads((folder / "receipt.json").read_text(encoding="utf-8"))
        texts = [item["text"] for item in receipt["checks"]]
        self.assertIn(LABEL, texts)
        self.assertIn(added, texts)
        meta = json.loads((self.project / "gold" / str(session["id"]) / "meta.json").read_text(encoding="utf-8"))
        stored = [item["text"] for item in meta["checks"]]
        self.assertIn(LABEL, stored)
        self.assertIn(added, stored)

    def test_quoted_smart_apostrophes_match_the_edit_and_grade_consistently(self) -> None:
        for before, after, polarity in (
            ("Keep the writer’s label here.", "Keep the plain label here.", "forbid"),
            ("Keep the plain label here.", "Keep the writer’s label here.", "require"),
        ):
            with self.subTest(polarity=polarity):
                sentence = f'The edit must {polarity} "writer’s label".'
                slug = self.learn(before, after, checks=[sentence])
                meta = json.loads((self.project / "gold" / slug / "meta.json").read_text())
                check = meta["checks"][0]
                self.assertEqual(check["span"], "writer’s label")
                self.assertEqual(check["polarity"], polarity)
                self.assertTrue(passes_check(after, polarity, check["span"]))
                self.assertFalse(passes_check(before, polarity, check["span"]))
                self.assertEqual(
                    passes_check("Keep the writer's label here.", polarity, check["span"]),
                    passes_check("Keep the writer’s label here.", polarity, check["span"]),
                )

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

    def test_overlapping_confirmed_and_rejected_keeps_only_rejected(self) -> None:
        identifier = check_id(LABEL)
        other = "chk-other"
        record = {
            "at": "2026-01-02T00:00:00Z",
            "text": LABEL,
            "span": "Level 9:",
            "polarity": "forbid",
            "pair": "pair",
            "section": "notes",
        }
        payload = {
            "kind": "ghostless-checks",
            "version": 1,
            "confirmed": {identifier: record, other: {**record, "text": "Other."}},
            "rejected": {identifier: {**record, "at": "2026-01-03T00:00:00Z"}},
        }
        (self.project / "checks.json").write_text(json.dumps(payload) + "\n", encoding="utf-8")
        summary = checks_summary(self.project)
        confirmed_ids = [item["id"] for item in summary["confirmed"]]
        rejected_ids = [item["id"] for item in summary["rejected"]]
        self.assertEqual(confirmed_ids, [other])
        self.assertEqual(rejected_ids, [identifier])
        self.assertNotIn(identifier, [item["id"] for item in summary["pending"]])

    def test_confirm_without_section_returns_a_check_to_global(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            checks=[LABEL],
        )
        identifier = check_id(LABEL)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                main(["checks", "confirm", LABEL, "--section", "notes", "--project", str(self.project)]),
                0,
            )
        scoped = json.loads((self.project / "checks.json").read_text(encoding="utf-8"))
        self.assertEqual(scoped["confirmed"][identifier]["section"], "notes")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(main(["checks", "reject", LABEL, "--project", str(self.project)]), 0)
            self.assertEqual(main(["checks", "confirm", LABEL, "--project", str(self.project)]), 0)
        revived = json.loads((self.project / "checks.json").read_text(encoding="utf-8"))
        self.assertIsNone(revived["confirmed"][identifier]["section"])
        self.assertNotIn(identifier, revived["rejected"])
        again = confirm_check(self.project, identifier, section="essays")
        self.assertEqual(again["section"], "essays")
        with contextlib.redirect_stdout(io.StringIO()):
            code = main(["checks", "confirm", LABEL, "--global", "--project", str(self.project)])
        self.assertEqual(code, 0)
        cleared = json.loads((self.project / "checks.json").read_text(encoding="utf-8"))
        self.assertIsNone(cleared["confirmed"][identifier]["section"])


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
        before: Any = evaluate(self.project, holdout=2, seed=0)
        self.assertTrue(before["ok"], before)
        self.assertIsNone(before["confirmedChecks"]["label"])
        scores = [item["score"] for item in before["items"]]

        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", LABEL, "--section", "notes", "--project", str(self.project)])
        after: Any = evaluate(self.project, holdout=2, seed=0)
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
        repeated: Any = evaluate(self.project, holdout=2, seed=0)
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
        held: list[dict[str, object]] = [
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
        first: Any = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
        second: Any = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
        self.assertTrue(first["fresh"])
        self.assertFalse(second["fresh"])
        self.assertEqual(first["session"], second["session"])
        self.assertEqual(first["label"], "1/2")
        guide.write_text("# Voice guide\n\nThe guide changed.\n", encoding="utf-8")
        changed_guide: Any = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
        self.assertTrue(changed_guide["fresh"])
        self.assertNotEqual(changed_guide["session"], first["session"])
        skill.write_text("Draft from the guide.\nRead the confirmed checks.\n", encoding="utf-8")
        changed_skill: Any = grade_confirmed(self.project, held, seed=0, skill_path=skill, guide_path=guide)
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

    def test_plain_eval_prints_why_a_check_case_was_dropped(self) -> None:
        self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            section="notes",
            checks=[LABEL],
            date="2026-01-02",
        )
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", LABEL, "--project", str(self.project)])
        before: Any = evaluate(self.project, holdout=1, seed=0)
        scores = [item["score"] for item in before["items"]]
        decisions_path = self.project / "checks.json"
        decisions = json.loads(decisions_path.read_text(encoding="utf-8"))
        decisions["confirmed"][check_id(LABEL)]["pair"] = "missing-pair"
        decisions_path.write_text(json.dumps(decisions) + "\n", encoding="utf-8")
        text = io.StringIO()
        with contextlib.redirect_stdout(text):
            code = main(["eval", "--project", str(self.project), "--holdout", "1"])
        self.assertEqual(code, 0)
        rendered = text.getvalue()
        self.assertIn("confirmed checks: 0/1", rendered)
        self.assertIn("no readable after file", rendered)
        self.assertIn("correlation:", rendered)
        self.assertNotIn("%", rendered.split("confirmed checks:")[1].split("\n", 1)[0])
        after: Any = evaluate(self.project, holdout=1, seed=0)
        self.assertEqual([item["score"] for item in after["items"]], scores)
        self.assertEqual(after["correlation"], before["correlation"])
        self.assertEqual(after["verdict"], before["verdict"])

    def confirmed_holdout(self) -> list[dict[str, object]]:
        slug = self.learn(
            "Level 9: starts the note. Then we stop.",
            "The correction starts the note. Then we stop.",
            section="notes",
            checks=[LABEL],
            date="2026-01-02",
        )
        with contextlib.redirect_stdout(io.StringIO()):
            main(["checks", "confirm", LABEL, "--project", str(self.project)])
        return [{"slug": slug, "section": "notes", "before": "Level 9: starts the note."}]

    def test_changed_case_contents_or_scope_preserve_the_previous_session(self) -> None:
        held = self.confirmed_holdout()
        first: Any = grade_confirmed(self.project, held, seed=0)
        original_path = self.project / "stats" / "check-grades" / f"{first['session']}.json"
        original = original_path.read_bytes()
        held[0]["before"] = "The correction starts the note."
        changed_text: Any = grade_confirmed(self.project, held, seed=0)
        self.assertEqual(first["label"], "1/2")
        self.assertEqual(changed_text["label"], "2/2")
        self.assertTrue(changed_text["fresh"])
        self.assertNotEqual(first["session"], changed_text["session"])
        held[0]["section"] = "essay"
        changed_scope: Any = grade_confirmed(self.project, held, seed=0)
        self.assertEqual(changed_scope["label"], "1/1")
        self.assertTrue(changed_scope["fresh"])
        self.assertNotEqual(changed_text["session"], changed_scope["session"])
        (self.project / "gold" / str(held[0]["slug"]) / "after.md").write_text("Level 9: is back.")
        changed_after: Any = grade_confirmed(self.project, held, seed=0)
        self.assertEqual(changed_after["label"], "0/1")
        self.assertTrue(changed_after["fresh"])
        self.assertNotEqual(changed_scope["session"], changed_after["session"])
        self.assertEqual(original_path.read_bytes(), original)
        repeated: Any = grade_confirmed(self.project, held, seed=0)
        self.assertFalse(repeated["fresh"])
        self.assertEqual(repeated["session"], changed_after["session"])

    def test_an_installed_skill_copy_change_opens_a_fresh_session(self) -> None:
        held = self.confirmed_holdout()
        for custom in (False, True):
            with self.subTest(custom=custom):
                home = self.base / ("custom-home" if custom else "default-home")
                parent = home / "custom-skills" if custom else home / ".codex" / "skills"
                env = dict(os.environ, GHOSTLESS_HOME=str(home / "data"))
                env.pop("GHOSTLESS_SKILLS_DIR", None)
                if custom:
                    env["GHOSTLESS_SKILLS_DIR"] = str(parent)
                with mock.patch.dict(os.environ, env, clear=True), mock.patch("pathlib.Path.home", return_value=home):
                    with mock.patch("ghostless.harness._print_path_warning"), contextlib.redirect_stdout(io.StringIO()):
                        install_skills(agent="codex", force=False, copy=True, no_launcher=True, verbose=False)
                    installed = parent / "ghostless-draft" / "SKILL.md"
                    first: Any = grade_confirmed(self.project, held, seed=0)
                    installed.write_text(installed.read_text() + "\nAlways start with the point.\n")
                    changed: Any = grade_confirmed(self.project, held, seed=0)
                    self.assertIn(str(installed.resolve()), changed["skillFiles"])
                    self.assertTrue(changed["fresh"])
                    self.assertNotEqual(first["skillSha256"], changed["skillSha256"])
                    self.assertNotEqual(first["session"], changed["session"])
                    self.assertEqual(first["label"], changed["label"])
                    repeated: Any = grade_confirmed(self.project, held, seed=0)
                    self.assertFalse(repeated["fresh"])
                    self.assertEqual(repeated["session"], changed["session"])

    def test_a_wrapped_holdout_still_fails_and_a_bad_after_does_not_crash(self) -> None:
        held = self.confirmed_holdout()
        held[0]["before"] = "Level\n9: starts the note."
        graded: Any = grade_confirmed(self.project, held, seed=0)
        self.assertEqual(graded["label"], "1/2")
        before_case = next(item for item in graded["items"] if item["case"] == "before")
        self.assertFalse(before_case["pass"])
        after_path = self.project / "gold" / str(held[0]["slug"]) / "after.md"
        after_path.write_bytes(b"\xff\xfe not utf-8")
        again: Any = grade_confirmed(self.project, held, seed=0)
        self.assertEqual(again["label"], "0/1")
        self.assertNotIn("after", {item["case"] for item in again["items"]})
        self.assertIn("readable after", again["note"])
        self.assertTrue(again["fresh"])
        self.assertNotEqual(again["session"], graded["session"])


if __name__ == "__main__":
    os.environ.setdefault("GHOSTLESS_NO_UPDATE", "1")
    unittest.main()
