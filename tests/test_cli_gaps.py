"""CLI branches that the happy-path suites do not reach."""

from __future__ import annotations

import argparse
import contextlib
import io
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import (  # noqa: E402
    _dispatch,
    _report_git,
    _warn_if_stale,
    main,
)


class CliGapTests(unittest.TestCase):
    def test_help_unknown_and_dispatch_errors(self) -> None:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(main([]), 0)
        self.assertIn("usage:", output.getvalue())
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            self.assertEqual(_dispatch(argparse.Namespace(command="nope")), 2)
        self.assertIn("unknown command nope", error.getvalue())
        with patch("ghostless.cli._dispatch", side_effect=OSError("disk")):
            error = io.StringIO()
            with contextlib.redirect_stderr(error):
                self.assertEqual(main(["build"]), 2)
            self.assertIn("disk", error.getvalue())

    def test_maintenance_exit_codes_and_update_notice(self) -> None:
        with patch("ghostless.cli.harness_main", side_effect=SystemExit("boom")):
            self.assertEqual(main(["doctor"]), 2)
        with patch("ghostless.cli.harness_main", side_effect=SystemExit(4)):
            self.assertEqual(main(["doctor"]), 4)
        error = io.StringIO()
        with (
            patch("ghostless.cli.harness_main", side_effect=OSError("disk")),
            contextlib.redirect_stderr(error),
        ):
            self.assertEqual(main(["doctor"]), 2)
        self.assertIn("disk", error.getvalue())
        error = io.StringIO()
        with (
            patch.dict(os.environ, {"GHOSTLESS_LAUNCHER": "npm"}),
            patch("ghostless.cli.check_for_update", return_value="update 9.9.9"),
            patch("ghostless.cli.harness_main", return_value=0),
            contextlib.redirect_stderr(error),
        ):
            self.assertEqual(main(["doctor"]), 0)
        self.assertIn("9.9.9", error.getvalue())

    def test_init_rejects_here_and_a_directory_together(self) -> None:
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            self.assertEqual(main(["init", "--here", "voice"]), 2)
        self.assertIn("either a directory or --here", error.getvalue())

    def test_judge_missing_draft_and_unreadable_stats(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            error = io.StringIO()
            with contextlib.redirect_stderr(error):
                self.assertEqual(main(["judge", str(project / "missing.md"), "--project", str(project)]), 2)
            self.assertIn("draft not found", error.getvalue())
            (project / "voice-stats.json").write_text("[]\n", encoding="utf-8")
            draft = project / "draft.md"
            draft.write_text("A short draft.\n", encoding="utf-8")
            error = io.StringIO()
            with contextlib.redirect_stderr(error):
                self.assertEqual(main(["judge", str(draft), "--project", str(project)]), 2)
            self.assertIn("unreadable", error.getvalue())

    def test_stale_warning_and_git_report(self) -> None:
        silent = io.StringIO()
        with contextlib.redirect_stderr(silent):
            _warn_if_stale([])
        self.assertEqual(silent.getvalue(), "")
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            _warn_if_stale(["a.md", "b.md", "c.md", "d.md"])
        self.assertIn("(+1 more)", error.getvalue())
        root = Path("/tmp/voice")
        skipped = io.StringIO()
        with contextlib.redirect_stdout(skipped):
            self.assertIsNone(_report_git(root, skip=True))
        self.assertIn("skipped", skipped.getvalue())
        cases = {
            "created": "created local history",
            "existing": "existing local repository",
            "unavailable": "unavailable",
            "parent:/repo": "inside the existing repository",
            "weird": "Git: weird",
        }
        for state, snippet in cases.items():
            output = io.StringIO()
            with (
                patch("ghostless.cli.ensure_local_git", return_value=state),
                contextlib.redirect_stdout(output),
            ):
                self.assertEqual(_report_git(root, skip=False), state)
            self.assertIn(snippet, output.getvalue())

    def test_which_reports_no_project(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            old = os.getcwd()
            os.chdir(tmp)
            try:
                with patch.dict(os.environ, {}, clear=False):
                    os.environ.pop("GHOSTLESS_PROJECT", None)
                    output = io.StringIO()
                    with contextlib.redirect_stdout(output):
                        self.assertEqual(main(["which"]), 0)
                self.assertIn("project=none", output.getvalue())
            finally:
                os.chdir(old)
