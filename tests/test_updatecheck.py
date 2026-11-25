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
import urllib.error
import urllib.request
from email.message import Message
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import main  # noqa: E402
from ghostless.updatecheck import (  # noqa: E402
    DEFAULT_INTERVAL_S,
    RELEASE_URL,
    _interval,
    fetch_latest_release,
    parse_version,
    plan,
    run_once,
    updates_disabled,
    version_gt,
)
from ghostless.updatecheck import main as update_main  # noqa: E402


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

            def explode() -> tuple[bool, str | None]:
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
                with (
                    patch.dict(os.environ, {"GHOSTLESS_LAUNCHER": "npm"}),
                    patch("ghostless.cli.check_for_update", return_value="disabled") as mock_check,
                ):
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

    def test_failed_fetch_does_not_start_the_cooldown(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            calls = {"n": 0}

            def fetch() -> tuple[bool, str | None]:
                calls["n"] += 1
                if calls["n"] == 1:
                    return False, None
                return True, "9.9.9"

            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                for var in (
                    "GHOSTLESS_NO_UPDATE",
                    "GHOSTLESS_UPDATE_CHECK",
                    "GHOSTLESS_JUST_UPDATED",
                    "GHOSTLESS_PROJECT",
                ):
                    os.environ.pop(var, None)
                self.assertEqual(run_once(fetcher=fetch), "current")
                self.assertFalse((home / "last-update-check").exists())
                self.assertEqual(run_once(fetcher=fetch), "update 9.9.9")
                self.assertTrue((home / "last-update-check").is_file())

                def explode() -> tuple[bool, str | None]:
                    raise AssertionError("a fresh stamp should skip the next fetch")

                self.assertEqual(run_once(fetcher=explode), "skip")


class _Body:
    def __init__(self, payload: bytes) -> None:
        self.payload = payload

    def read(self) -> bytes:
        return self.payload

    def __enter__(self) -> _Body:
        return self

    def __exit__(self, *_args: object) -> None:
        return None


class UpdateFetchTests(unittest.TestCase):
    def test_unparseable_remote_version_stays_current(self) -> None:
        self.assertEqual(
            plan(disabled=False, stamp_age_s=None, interval_s=10, remote="latest", local="0.1.0", fetch_ok=True),
            "current",
        )
        with self.assertRaises(ValueError):
            parse_version("v")

    def test_env_switches_skip_the_network(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)

            def explode() -> tuple[bool, str | None]:
                raise AssertionError("a disabled check must not fetch")

            switches = {
                "GHOSTLESS_NO_UPDATE": "1",
                "GHOSTLESS_UPDATE_CHECK": "0",
                "GHOSTLESS_JUST_UPDATED": "1",
            }
            for key, value in switches.items():
                with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home), key: value}):
                    for other in switches:
                        if other != key:
                            os.environ.pop(other, None)
                    self.assertTrue(updates_disabled())
                    self.assertEqual(run_once(fetcher=explode), "disabled")

    def test_missing_project_and_broken_files_do_not_disable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            home.mkdir()
            (home / "config.json").write_text("{", encoding="utf-8")
            project = Path(tmp) / "voice"
            project.mkdir()
            (project / "manifest.json").write_text("{", encoding="utf-8")
            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                for var in (
                    "GHOSTLESS_NO_UPDATE",
                    "GHOSTLESS_UPDATE_CHECK",
                    "GHOSTLESS_JUST_UPDATED",
                    "GHOSTLESS_PROJECT",
                ):
                    os.environ.pop(var, None)
                self.assertFalse(updates_disabled(project=project))

    def test_interval_falls_back_on_bad_env_and_honors_a_number(self) -> None:
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("GHOSTLESS_UPDATE_INTERVAL_SECONDS", None)
            self.assertEqual(_interval(), float(DEFAULT_INTERVAL_S))
        with patch.dict(os.environ, {"GHOSTLESS_UPDATE_INTERVAL_SECONDS": "nope"}):
            self.assertEqual(_interval(), float(DEFAULT_INTERVAL_S))
        with patch.dict(os.environ, {"GHOSTLESS_UPDATE_INTERVAL_SECONDS": "-3"}):
            self.assertEqual(_interval(), float(DEFAULT_INTERVAL_S))
        with patch.dict(os.environ, {"GHOSTLESS_UPDATE_INTERVAL_SECONDS": "12"}):
            self.assertEqual(_interval(), 12.0)

    def test_fetch_latest_release_reads_a_tag_and_treats_failures_as_misses(self) -> None:
        with patch.object(urllib.request, "urlopen", return_value=_Body(b'{"tag_name": " v1.2.3 "}')):
            self.assertEqual(fetch_latest_release(), (True, "v1.2.3"))
        with patch.object(urllib.request, "urlopen", return_value=_Body(b"[]")):
            self.assertEqual(fetch_latest_release(), (False, None))
        with patch.object(urllib.request, "urlopen", return_value=_Body(b'{"tag_name": "  "}')):
            self.assertEqual(fetch_latest_release(), (True, None))
        with patch.object(urllib.request, "urlopen", return_value=_Body(b"not-json")):
            self.assertEqual(fetch_latest_release(), (False, None))
        missing = urllib.error.HTTPError(RELEASE_URL, 404, "missing", hdrs=Message(), fp=None)
        server = urllib.error.HTTPError(RELEASE_URL, 500, "down", hdrs=Message(), fp=None)
        with patch.object(urllib.request, "urlopen", side_effect=missing):
            self.assertEqual(fetch_latest_release(), (True, None))
        with patch.object(urllib.request, "urlopen", side_effect=server):
            self.assertEqual(fetch_latest_release(), (False, None))
        with patch.object(urllib.request, "urlopen", side_effect=urllib.error.URLError("offline")):
            self.assertEqual(fetch_latest_release(), (False, None))
        missing.close()
        server.close()

    def test_stamp_write_errors_do_not_fail_the_check(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            home.write_text("not a directory", encoding="utf-8")
            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                for var in (
                    "GHOSTLESS_NO_UPDATE",
                    "GHOSTLESS_UPDATE_CHECK",
                    "GHOSTLESS_JUST_UPDATED",
                    "GHOSTLESS_PROJECT",
                ):
                    os.environ.pop(var, None)
                self.assertEqual(run_once(fetcher=lambda: (True, None)), "current")

    def test_main_prints_the_plan(self) -> None:
        with patch("ghostless.updatecheck.run_once", return_value="current"):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(update_main(), 0)
            self.assertEqual(output.getvalue().strip(), "current")


if __name__ == "__main__":
    unittest.main()
