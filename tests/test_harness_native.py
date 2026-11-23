"""Tests for the portable npm/Python entry path, with isolated skill storage."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]

import sys
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import main  # noqa: E402


class NativeHarnessTests(unittest.TestCase):
    def test_install_and_reset_owned_copy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            skills = base / "skills"
            data = base / "data"
            env = {"GHOSTLESS_SKILLS_DIR": str(skills), "GHOSTLESS_HOME": str(data)}
            with patch.dict(os.environ, env):
                self.assertEqual(main(["install-skills", "--copy", "--no-launcher"]), 0)
                self.assertEqual((skills / "ghostless-draft" / ".ghostless-installed").read_text().strip(),
                                 "ghostless:ghostless-draft")
                self.assertTrue((skills / "ghostless-edit" / "SKILL.md").is_file())
                self.assertEqual(json.loads((data / "config.json").read_text())["updateCheck"], True)
                self.assertEqual(main(["reset", "--yes"]), 0)
                self.assertFalse((skills / "ghostless-draft").exists())

    def test_foreign_skill_directory_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            skills = base / "skills"
            foreign = skills / "ghostless-draft"
            foreign.mkdir(parents=True)
            (foreign / "SKILL.md").write_text("foreign", encoding="utf-8")
            env = {"GHOSTLESS_SKILLS_DIR": str(skills), "GHOSTLESS_HOME": str(base / "data")}
            with patch.dict(os.environ, env):
                self.assertEqual(main(["install-skills", "--copy"]), 0)
            self.assertEqual((foreign / "SKILL.md").read_text(encoding="utf-8"), "foreign")

    @unittest.skipUnless(shutil.which("node"), "Node is needed for the npm launcher")
    def test_node_launcher_uses_python_without_bash(self) -> None:
        env = os.environ.copy()
        env["GHOSTLESS_PYTHON"] = sys.executable
        env["GHOSTLESS_NO_UPDATE"] = "1"
        completed = subprocess.run(
            ["node", str(ROOT / "bin" / "ghostless.cjs"), "--version"],
            cwd=ROOT, env=env, check=False, capture_output=True, text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("ghostless 0.1.0", completed.stdout)

    @unittest.skipUnless(shutil.which("node"), "Node is needed for the npm launcher")
    def test_node_launcher_uses_utf8_for_learn_output(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            project = base / "voice"
            self.assertEqual(main(["init", str(project)]), 0)
            before = base / "before.md"
            after = base / "after.md"
            before.write_text("I write.\n", encoding="utf-8")
            after.write_text("I wrote this.\n", encoding="utf-8")
            env = os.environ.copy()
            env.update({"GHOSTLESS_PYTHON": sys.executable, "GHOSTLESS_NO_UPDATE": "1",
                        "PYTHONIOENCODING": "ascii"})
            completed = subprocess.run(
                ["node", str(ROOT / "bin" / "ghostless.cjs"), "learn", "--before", str(before),
                 "--after", str(after), "--project", str(project)],
                cwd=base, env=env, check=False, capture_output=True, text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_package_entry_is_portable_node_script(self) -> None:
        package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        self.assertEqual(package["bin"]["ghostless"], "bin/ghostless.cjs")
        self.assertTrue((ROOT / "bin" / "ghostless.cjs").is_file())

    def test_maintenance_command_help_and_bad_flag_return_clean_codes(self) -> None:
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(main(["install-skills", "--help"]), 0)
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(main(["install-skills", "--unknown-flag"]), 2)


class PathWarningTests(unittest.TestCase):
    def _bin_dir(self, prefix: Path) -> Path:
        # Windows global installs put the .cmd shims directly in the prefix.
        return prefix if os.name == "nt" else prefix / "bin"

    def _fake_npm(self, base: Path, prefix: Path) -> Path:
        """A stub `npm` that reports `prefix` for `npm config get prefix`."""
        fakebin = base / "fakebin"
        fakebin.mkdir()
        npm = fakebin / "npm"
        npm.write_text(f"#!/bin/sh\necho {prefix}\n", encoding="utf-8")
        npm.chmod(0o755)
        cmd = fakebin / "npm.cmd"
        cmd.write_text(f"@echo {prefix}\r\n", encoding="utf-8")
        return fakebin

    def test_bin_dir_detected_and_warning_when_missing(self) -> None:
        from ghostless.harness import npm_bin_path_warning, npm_global_bin_dir

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            prefix = base / "prefix"
            expected = self._bin_dir(prefix)
            fakebin = self._fake_npm(base, prefix)
            env = {"PATH": str(fakebin), "GHOSTLESS_HOME": str(base / "data")}
            with patch.dict(os.environ, env):
                # PATH holds only the stub, so the bin dir is missing from it.
                self.assertEqual(npm_global_bin_dir(), expected)
                warning = npm_bin_path_warning()
                self.assertIsNotNone(warning)
                self.assertIn(str(expected), warning)
                if os.name != "nt":
                    self.assertIn('export PATH="', warning)
                with patch.dict(
                    os.environ,
                    {"PATH": os.pathsep.join([str(fakebin), str(expected)])},
                ):
                    self.assertIsNone(npm_bin_path_warning())

    def test_no_npm_means_no_warning(self) -> None:
        from ghostless.harness import npm_global_bin_dir

        with tempfile.TemporaryDirectory() as tmp:
            with patch.dict(os.environ, {"PATH": tmp}):
                os.environ.pop("NPM_CONFIG_PREFIX", None)
                self.assertIsNone(npm_global_bin_dir())

    def test_windows_npm_shim_runs_through_cmd(self) -> None:
        # Regression: launching npm.cmd directly fails on Windows
        # (CreateProcess cannot run .cmd shims), the OSError was swallowed,
        # and the warning never fired. Pin the argv on nt.
        import ghostless.harness as harness

        npm = r"C:\Program Files\nodejs\npm.cmd"
        with patch.object(harness.os, "name", "nt"):
            self.assertEqual(
                harness._npm_prefix_command(npm),
                ["cmd", "/c", npm, "config", "get", "prefix"],
            )
        # The POSIX expectation must also pin os.name: on Windows it is
        # genuinely "nt", so without the patch this asserts the cmd form.
        with patch.object(harness.os, "name", "posix"):
            self.assertEqual(
                harness._npm_prefix_command("/usr/local/bin/npm"),
                ["/usr/local/bin/npm", "config", "get", "prefix"],
            )

    def test_doctor_warns_when_bin_dir_missing_from_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            prefix = base / "prefix"
            expected = self._bin_dir(prefix)
            fakebin = self._fake_npm(base, prefix)
            env = {"PATH": str(fakebin), "GHOSTLESS_HOME": str(base / "data")}
            with patch.dict(os.environ, env):
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    self.assertEqual(main(["doctor"]), 0)
            self.assertIn(str(expected), out.getvalue())
            if os.name != "nt":
                self.assertIn("export PATH=", out.getvalue())


class UpdateTests(unittest.TestCase):
    def _make_suite(self, path: Path, *, git_dir: bool = False) -> Path:
        path.mkdir(parents=True, exist_ok=True)
        (path / "package.json").write_text('{"name": "ghostless", "version": "0.1.0"}\n', encoding="utf-8")
        (path / "bin").mkdir(exist_ok=True)
        (path / "bin" / "ghostless").write_text("#!/bin/sh\n", encoding="utf-8")
        (path / "skills" / "ghostless-draft").mkdir(parents=True, exist_ok=True)
        (path / "skills" / "ghostless-draft" / "SKILL.md").write_text("# draft\n", encoding="utf-8")
        if git_dir:
            (path / ".git").mkdir(exist_ok=True)
        return path

    def test_update_npm_global_spawns_newly_installed_cli(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "lib" / "node_modules" / "ghostless")
            env = {"GHOSTLESS_SUITE": str(suite), "GHOSTLESS_LAUNCHER": "npm"}
            which_map = {"npm": "/usr/local/bin/npm", "ghostless": "/usr/local/bin/ghostless"}
            with (
                patch.dict(os.environ, env),
                patch.object(harness.os, "name", "posix"),
                patch("ghostless.harness.shutil.which", side_effect=lambda name: which_map.get(name)),
                patch("ghostless.harness.subprocess.run", return_value=subprocess.CompletedProcess([], 0)) as mock_run,
            ):
                self.assertEqual(harness.update(agent="cursor"), 0)

            self.assertEqual(mock_run.call_count, 2)
            self.assertEqual(
                mock_run.call_args_list[0].args[0],
                ["/usr/local/bin/npm", "install", "-g", "ghostless@latest"],
            )
            self.assertEqual(
                mock_run.call_args_list[1].args[0],
                ["/usr/local/bin/ghostless", "install-skills", "--force", "--agent", "cursor"],
            )
            self.assertEqual(mock_run.call_args_list[1].kwargs["env"]["GHOSTLESS_JUST_UPDATED"], "1")

    def test_update_npm_global_windows_runs_cmd_shim(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "node_modules" / "ghostless")
            env = {"GHOSTLESS_LAUNCHER": "npm"}
            which_map = {"npm.cmd": r"C:\npm\npm.cmd", "ghostless.cmd": r"C:\npm\ghostless.cmd"}
            with (
                patch.dict(os.environ, env),
                patch("ghostless.harness._root", return_value=suite),
                patch.object(harness.os, "name", "nt"),
                patch("ghostless.harness.shutil.which", side_effect=lambda name: which_map.get(name)),
                patch("ghostless.harness.subprocess.run", return_value=subprocess.CompletedProcess([], 0)) as mock_run,
            ):
                self.assertEqual(harness.update(agent="claude-code"), 0)

            self.assertEqual(mock_run.call_count, 2)
            self.assertEqual(
                mock_run.call_args_list[0].args[0],
                ["cmd", "/c", r"C:\npm\npm.cmd", "install", "-g", "ghostless@latest"],
            )
            self.assertEqual(
                mock_run.call_args_list[1].args[0],
                ["cmd", "/c", r"C:\npm\ghostless.cmd", "install-skills", "--force", "--agent", "claude-code"],
            )
            self.assertEqual(mock_run.call_args_list[1].kwargs["env"]["GHOSTLESS_JUST_UPDATED"], "1")

    def test_update_npm_global_falls_back_when_installed_cli_missing_from_path(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "lib" / "node_modules" / "ghostless")
            env = {"GHOSTLESS_SUITE": str(suite), "GHOSTLESS_LAUNCHER": "npm"}
            which_map = {"npm": "/usr/local/bin/npm", "npm.cmd": r"C:\npm\npm.cmd"}
            with (
                patch.dict(os.environ, env),
                patch("ghostless.harness.shutil.which", side_effect=lambda name: which_map.get(name)),
                patch("ghostless.harness.subprocess.run", return_value=subprocess.CompletedProcess([], 0)) as mock_run,
                patch("ghostless.harness.install_skills", return_value=0) as mock_install,
            ):
                self.assertEqual(harness.update(agent="cursor"), 0)

            self.assertEqual(mock_run.call_count, 1)
            mock_install.assert_called_once_with(
                agent="cursor", force=True, copy=False, no_launcher=False, verbose=False
            )

    def test_update_npm_global_stops_when_npm_install_fails(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "lib" / "node_modules" / "ghostless")
            env = {"GHOSTLESS_SUITE": str(suite), "GHOSTLESS_LAUNCHER": "npm"}
            which_map = {
                "npm": "/usr/local/bin/npm",
                "npm.cmd": r"C:\npm\npm.cmd",
                "ghostless": "/usr/local/bin/ghostless",
                "ghostless.cmd": r"C:\npm\ghostless.cmd",
            }
            with (
                patch.dict(os.environ, env),
                patch("ghostless.harness.shutil.which", side_effect=lambda name: which_map.get(name)),
                patch("ghostless.harness.subprocess.run", return_value=subprocess.CompletedProcess([], 1)) as mock_run,
                patch("ghostless.harness.install_skills") as mock_install,
            ):
                self.assertEqual(harness.update(agent="auto"), 1)

            self.assertEqual(mock_run.call_count, 1)
            mock_install.assert_not_called()

    def test_update_git_checkout_clean_pulls_and_refreshes_skills(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "checkout", git_dir=True)
            env = {"GHOSTLESS_SUITE": str(suite)}
            with (
                patch.dict(os.environ, env),
                patch("ghostless.harness.shutil.which", return_value="/usr/bin/git"),
                patch("ghostless.harness.subprocess.run", return_value=subprocess.CompletedProcess([], 0)) as mock_run,
                patch("ghostless.harness.install_skills", return_value=0) as mock_install,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(harness.update(agent="cursor"), 0)

            self.assertEqual(
                [call.args[0] for call in mock_run.call_args_list],
                [
                    ["/usr/bin/git", "-C", str(suite.resolve()), "diff", "--quiet"],
                    ["/usr/bin/git", "-C", str(suite.resolve()), "diff", "--cached", "--quiet"],
                    ["/usr/bin/git", "-C", str(suite.resolve()), "pull", "--ff-only"],
                ],
            )
            mock_install.assert_called_once_with(
                agent="cursor", force=True, copy=False, no_launcher=False, verbose=False
            )

    def test_update_git_checkout_dirty_skips_pull_and_warns(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "checkout", git_dir=True)
            env = {"GHOSTLESS_SUITE": str(suite)}
            for diff_codes in ([1], [0, 1]):
                codes = iter(diff_codes)
                stderr = io.StringIO()
                with (
                    patch.dict(os.environ, env),
                    patch("ghostless.harness.shutil.which", return_value="/usr/bin/git"),
                    patch(
                        "ghostless.harness.subprocess.run",
                        side_effect=lambda *_a, **_kw: subprocess.CompletedProcess([], next(codes)),
                    ) as mock_run,
                    patch("ghostless.harness.install_skills", return_value=0) as mock_install,
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(stderr),
                ):
                    self.assertEqual(harness.update(agent="auto"), 0)

                commands = [call.args[0] for call in mock_run.call_args_list]
                self.assertFalse(any("pull" in cmd for cmd in commands), commands)
                self.assertIn("uncommitted changes", stderr.getvalue())
                mock_install.assert_called_once_with(
                    agent="auto", force=True, copy=False, no_launcher=False, verbose=False
                )

    def test_update_git_checkout_pull_failure_warns_and_still_relinks(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "checkout", git_dir=True)
            env = {"GHOSTLESS_SUITE": str(suite)}
            codes = iter([0, 0, 1])
            stderr = io.StringIO()
            with (
                patch.dict(os.environ, env),
                patch("ghostless.harness.shutil.which", return_value="/usr/bin/git"),
                patch(
                    "ghostless.harness.subprocess.run",
                    side_effect=lambda *_a, **_kw: subprocess.CompletedProcess([], next(codes)),
                ),
                patch("ghostless.harness.install_skills", return_value=0) as mock_install,
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(stderr),
            ):
                self.assertEqual(harness.update(agent="auto"), 0)

            self.assertIn("git pull failed", stderr.getvalue())
            mock_install.assert_called_once()

    def test_update_non_npm_non_git_returns_1(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            suite = self._make_suite(Path(tmp) / "plain", git_dir=False)
            env = {"GHOSTLESS_SUITE": str(suite)}
            out = io.StringIO()
            with patch.dict(os.environ, env):
                os.environ.pop("GHOSTLESS_LAUNCHER", None)
                with contextlib.redirect_stdout(out):
                    self.assertEqual(harness.update(agent="auto"), 1)
            self.assertIn("not launched from npm", out.getvalue())


if __name__ == "__main__":
    unittest.main()
