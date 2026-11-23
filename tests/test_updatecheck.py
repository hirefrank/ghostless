"""Release update check scheduling and manifest override tests."""

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
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import main  # noqa: E402
from ghostless.updatecheck import plan, run_once, updates_disabled, version_gt  # noqa: E402


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

    def test_project_manifest_disables_check_from_unrelated_cwd_and_respects_project_flag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, tempfile.TemporaryDirectory() as unrelated_tmp:
            base = Path(tmp)
            home = base / "home"
            home.mkdir()
            unrelated = Path(unrelated_tmp)
            project = base / "voice"
            shutil.copytree(SAMPLE, project)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["updateCheck"] = False
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

            def explode(*_args: object, **_kwargs: object) -> object:
                raise AssertionError("network check should not run when project updateCheck is false")

            old = Path.cwd()
            try:
                os.chdir(unrelated)
                with patch.dict(
                    os.environ,
                    {"GHOSTLESS_HOME": str(home), "GHOSTLESS_LAUNCHER": "npm"},
                ):
                    for var in (
                        "GHOSTLESS_NO_UPDATE",
                        "GHOSTLESS_UPDATE_CHECK",
                        "GHOSTLESS_JUST_UPDATED",
                        "GHOSTLESS_PROJECT",
                    ):
                        os.environ.pop(var, None)
                    self.assertFalse(updates_disabled())
                    self.assertTrue(updates_disabled(project=project))
                    self.assertEqual(run_once(fetcher=explode, project=project), "disabled")
                    with patch("urllib.request.urlopen", side_effect=explode):
                        out = io.StringIO()
                        with contextlib.redirect_stdout(out):
                            code = main(["build", "--project", str(project)])
                        self.assertEqual(code, 0)
                        self.assertTrue((project / "voice-guide.md").is_file())
            finally:
                os.chdir(old)

    def test_cli_defers_update_check_until_after_project_parse(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            unrelated = base / "unrelated"
            unrelated.mkdir()
            project = base / "voice"
            shutil.copytree(SAMPLE, project)

            old = Path.cwd()
            try:
                os.chdir(unrelated)
                with patch.dict(os.environ, {"GHOSTLESS_LAUNCHER": "npm"}):
                    with patch("ghostless.cli.check_for_update", return_value="disabled") as mock_check:
                        with contextlib.redirect_stdout(io.StringIO()):
                            self.assertEqual(main(["--version"]), 0)
                            self.assertEqual(main(["--help"]), 0)
                            self.assertEqual(main([]), 0)
                            self.assertEqual(main(["build", "--help"]), 0)
                        mock_check.assert_not_called()

                        with contextlib.redirect_stdout(io.StringIO()):
                            code = main(["build", "--project", str(project)])
                        self.assertEqual(code, 0)
                        mock_check.assert_called_once_with(project=project)
            finally:
                os.chdir(old)


if __name__ == "__main__":
    unittest.main()
