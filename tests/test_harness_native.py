"""Tests for the portable npm/Python entry path, with isolated skill storage."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Iterator
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import main  # noqa: E402


class NativeHarnessTests(unittest.TestCase):
    def test_install_and_reset_owned_copy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            skills = base / "skills"
            data = base / "data"
            env = {"GHOSTLESS_SKILLS_DIR": str(skills), "GHOSTLESS_HOME": str(data)}
            with patch.dict(os.environ, env), contextlib.redirect_stdout(io.StringIO()):
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
            with patch.dict(os.environ, env), contextlib.redirect_stdout(io.StringIO()):
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
            with contextlib.redirect_stdout(io.StringIO()):
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
        with patch.object(harness, "_is_windows", return_value=True):
            self.assertEqual(
                harness._npm_prefix_command(npm),
                ["cmd", "/c", npm, "config", "get", "prefix"],
            )
        # Pin the platform helper, not os.name. Path() follows os.name, so a
        # process-wide "posix" patch cannot build paths on Windows.
        with patch.object(harness, "_is_windows", return_value=False):
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

    def test_doctor_exits_zero_for_python_wheel_without_suite_root(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            with (
                patch.dict(os.environ, {"PATH": str(base), "GHOSTLESS_HOME": str(base / "data")}),
                patch.object(harness, "suite_root", return_value=None),
            ):
                os.environ.pop("NPM_CONFIG_PREFIX", None)
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    self.assertEqual(main(["doctor"]), 0)
        text = out.getvalue()
        self.assertIn("Suite: none", text)
        self.assertIn("Install: python-wheel", text)


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
                patch.object(harness, "_is_windows", return_value=False),
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
                patch.object(harness, "_is_windows", return_value=True),
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
                        side_effect=lambda *_a, codes=codes, **_kw: subprocess.CompletedProcess([], next(codes)),
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


class InstalledManifestTests(unittest.TestCase):
    def test_load_installed_handles_missing_corrupt_and_malformed_entries(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp) / "data"
            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(data)}):
                self.assertEqual(harness._load_installed(), {})

                data.mkdir(parents=True)
                manifest = data / harness.INSTALLED_MANIFEST
                manifest.write_text("{not json", encoding="utf-8")
                self.assertEqual(harness._load_installed(), {})

                manifest.write_text(json.dumps({"version": 2, "entries": {}}), encoding="utf-8")
                self.assertEqual(harness._load_installed(), {})

                manifest.write_text(
                    json.dumps(
                        {
                            "version": 1,
                            "entries": {
                                "": {"skill": "ghostless-draft", "target": "/a"},
                                "bad-type": "not-a-dict",
                                "bad-skill": {"skill": 123, "target": "/a"},
                                "missing-target": {"skill": "ghostless-draft"},
                                "bad-method": {
                                    "skill": "ghostless-draft",
                                    "target": "/a",
                                    "method": 99,
                                },
                                "/valid/dest": {
                                    "skill": "ghostless-draft",
                                    "target": "/valid/src",
                                    "suiteRoot": "/valid",
                                    "method": "linked",
                                },
                            },
                        }
                    ),
                    encoding="utf-8",
                )
                loaded = harness._load_installed()
                self.assertEqual(
                    loaded,
                    {
                        os.path.normcase("/valid/dest"): {
                            "skill": "ghostless-draft",
                            "target": "/valid/src",
                            "suiteRoot": "/valid",
                            "method": "linked",
                        }
                    },
                )

    def test_is_ghostless_package_skill_recognizes_posix_and_windows_paths(self) -> None:
        from ghostless.harness import _is_ghostless_package_skill

        self.assertTrue(
            _is_ghostless_package_skill(
                "/home/u/.nvm/versions/node/v24.22.2/lib/node_modules/ghostless/skills/ghostless-draft",
                "ghostless-draft",
            )
        )
        self.assertTrue(
            _is_ghostless_package_skill(
                r"C:\Users\me\AppData\Roaming\npm\node_modules\GHOSTLESS\skills\ghostless-edit",
                "ghostless-edit",
            )
        )
        self.assertFalse(
            _is_ghostless_package_skill(
                "/home/u/proj/node_modules/ghostless/skills/ghostless-draft",
                "ghostless-draft",
            )
        )
        self.assertFalse(
            _is_ghostless_package_skill(
                "/home/u/node_modules/ghostless/skills/ghostless-edit",
                "ghostless-draft",
            )
        )
        self.assertFalse(
            _is_ghostless_package_skill(
                "/home/u/node_modules/other-pkg/skills/ghostless-draft",
                "ghostless-draft",
            )
        )

    def test_copy_install_and_reset_updates_installed_manifest(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            skills = base / "skills"
            data = base / "data"
            env = {"GHOSTLESS_SKILLS_DIR": str(skills), "GHOSTLESS_HOME": str(data), "PATH": ""}
            with patch.dict(os.environ, env), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["install-skills", "--copy", "--no-launcher"]), 0)
                manifest = json.loads((data / harness.INSTALLED_MANIFEST).read_text(encoding="utf-8"))
                self.assertEqual(manifest["version"], 1)
                self.assertEqual(len(manifest["entries"]), len(harness.SKILLS))
                for skill in harness.SKILLS:
                    entry = manifest["entries"][harness._norm_path(skills / skill)]
                    self.assertEqual(entry["skill"], skill)
                    self.assertEqual(entry["method"], "copied")

                self.assertEqual(main(["reset", "--yes"]), 0)
                after = json.loads((data / harness.INSTALLED_MANIFEST).read_text(encoding="utf-8"))
                self.assertEqual(after["entries"], {})


@unittest.skipIf(os.name == "nt", "symlink tests require POSIX")
class SkillOwnershipTests(unittest.TestCase):
    def _make_suite(self, root: Path) -> Path:
        root.mkdir(parents=True, exist_ok=True)
        (root / "package.json").write_text('{"name": "ghostless", "version": "0.1.0"}\n', encoding="utf-8")
        (root / "bin").mkdir(exist_ok=True)
        (root / "bin" / "ghostless").write_text("#!/bin/sh\n", encoding="utf-8")
        for skill in ("ghostless-draft", "ghostless-edit", "ghostless-learn"):
            skill_dir = root / "skills" / skill
            skill_dir.mkdir(parents=True, exist_ok=True)
            (skill_dir / "SKILL.md").write_text(f"# {skill}\n", encoding="utf-8")
        return root

    @contextlib.contextmanager
    def _fake_home_env(self, home: Path, suite: Path) -> Iterator[None]:
        env = {
            "HOME": str(home),
            "GHOSTLESS_HOME": str(home / ".ghostless"),
            "GHOSTLESS_SUITE": str(suite),
            "GHOSTLESS_NO_UPDATE": "1",
            "PATH": "",
        }
        with patch.dict(os.environ, env):
            os.environ.pop("GHOSTLESS_SKILLS_DIR", None)
            os.environ.pop("NPM_CONFIG_PREFIX", None)
            yield

    def _run_cli(self, home: Path, suite: Path, argv: list[str]) -> str:
        out = io.StringIO()
        with self._fake_home_env(home, suite), contextlib.redirect_stdout(out):
            code = main(argv)
        self.assertEqual(code, 0, out.getvalue())
        return out.getvalue()

    def test_moved_node_package_root_relinks_without_force(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            (home / ".claude").mkdir(parents=True)
            old_suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.2" / "lib" / "node_modules" / "ghostless"
            )
            new_suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.3" / "lib" / "node_modules" / "ghostless"
            )

            self._run_cli(home, old_suite, ["install-skills"])
            stdout = self._run_cli(home, new_suite, ["install-skills"])

            self.assertIn("ghostless skills installed: 3; existing paths kept: 0", stdout)
            self.assertNotIn("not installed by ghostless", stdout)
            skills_dir = home / ".claude" / "skills"
            for skill in harness.SKILLS:
                dest = skills_dir / skill
                self.assertTrue(dest.is_symlink())
                self.assertEqual(dest.resolve(), (new_suite / "skills" / skill).resolve())

            manifest = json.loads((home / ".ghostless" / harness.INSTALLED_MANIFEST).read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["entries"]), 3)
            for skill in harness.SKILLS:
                entry = manifest["entries"][harness._norm_path(skills_dir / skill)]
                self.assertEqual(entry["suiteRoot"], str(new_suite.resolve()))
                self.assertEqual(entry["target"], str((new_suite / "skills" / skill).resolve()))

    def test_deleted_old_package_root_dangling_links_relink_and_reset(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            (home / ".claude").mkdir(parents=True)
            old_suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.2" / "lib" / "node_modules" / "ghostless"
            )
            new_suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.3" / "lib" / "node_modules" / "ghostless"
            )

            # Dangling links from a deleted package root relink cleanly without --force.
            self._run_cli(home, old_suite, ["install-skills"])
            shutil.rmtree(old_suite)
            skills_dir = home / ".claude" / "skills"
            for skill in harness.SKILLS:
                dest = skills_dir / skill
                self.assertTrue(dest.is_symlink())
                self.assertFalse(dest.exists())

            relink_out = self._run_cli(home, new_suite, ["install-skills"])
            self.assertIn("ghostless skills installed: 3; existing paths kept: 0", relink_out)
            self.assertNotIn("not installed by ghostless", relink_out)
            for skill in harness.SKILLS:
                dest = skills_dir / skill
                self.assertTrue(dest.exists())
                self.assertEqual(dest.resolve(), (new_suite / "skills" / skill).resolve())

            # Dangling links from a deleted checkout/package root are removed by reset --yes.
            old_checkout = self._make_suite(base / "checkouts" / "old-ghostless")
            self._run_cli(home, old_checkout, ["install-skills"])
            shutil.rmtree(old_checkout)
            for skill in harness.SKILLS:
                dest = skills_dir / skill
                self.assertTrue(dest.is_symlink())
                self.assertFalse(dest.exists())

            reset_out = self._run_cli(home, new_suite, ["reset", "--yes"])
            self.assertIn("Reset complete (3 skill paths removed).", reset_out)
            self.assertNotIn("not installed by ghostless", reset_out)
            for skill in harness.SKILLS:
                dest = skills_dir / skill
                self.assertFalse(dest.is_symlink())
                self.assertFalse(dest.exists())
            manifest = json.loads((home / ".ghostless" / harness.INSTALLED_MANIFEST).read_text(encoding="utf-8"))
            self.assertEqual(manifest["entries"], {})

    def test_legacy_pre_manifest_node_module_links_existing_and_deleted(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            skills_dir = home / ".claude" / "skills"
            skills_dir.mkdir(parents=True)
            new_suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.3" / "lib" / "node_modules" / "ghostless"
            )

            for delete_legacy in (False, True):
                legacy_suite = self._make_suite(
                    base / "nvm" / "versions" / "node" / f"v24.22.2-{int(delete_legacy)}"
                    / "lib" / "node_modules" / "ghostless"
                )
                for skill in harness.SKILLS:
                    dest = skills_dir / skill
                    if dest.is_symlink() or dest.exists():
                        dest.unlink()
                    dest.symlink_to(legacy_suite / "skills" / skill, target_is_directory=True)
                if delete_legacy:
                    shutil.rmtree(legacy_suite)

                # Ensure neither installed.json nor suite-root exists before running.
                if (home / ".ghostless").exists():
                    shutil.rmtree(home / ".ghostless")

                install_out = self._run_cli(home, new_suite, ["install-skills"])
                self.assertIn("ghostless skills installed: 3; existing paths kept: 0", install_out)
                self.assertNotIn("not installed by ghostless", install_out)
                for skill in harness.SKILLS:
                    self.assertEqual((skills_dir / skill).resolve(), (new_suite / "skills" / skill).resolve())

                # Recreate the legacy pre-manifest state and verify reset --yes removes all 3.
                if delete_legacy:
                    legacy_suite = self._make_suite(
                        base / "nvm" / "versions" / "node" / "v24.22.2-reset"
                        / "lib" / "node_modules" / "ghostless"
                    )
                for skill in harness.SKILLS:
                    (skills_dir / skill).unlink()
                    (skills_dir / skill).symlink_to(legacy_suite / "skills" / skill, target_is_directory=True)
                if delete_legacy:
                    shutil.rmtree(legacy_suite)
                shutil.rmtree(home / ".ghostless")

                reset_out = self._run_cli(home, new_suite, ["reset", "--yes"])
                self.assertIn("Reset complete (3 skill paths removed).", reset_out)
                self.assertNotIn("not installed by ghostless", reset_out)
                for skill in harness.SKILLS:
                    dest = skills_dir / skill
                    self.assertFalse(dest.is_symlink())
                    self.assertFalse(dest.exists())

    def test_foreign_and_repointed_symlinks_kept_until_force(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            skills_dir = home / ".claude" / "skills"
            skills_dir.mkdir(parents=True)
            suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.3" / "lib" / "node_modules" / "ghostless"
            )

            foreign_a = base / "foreign-a"
            foreign_a.mkdir()
            (foreign_a / "SKILL.md").write_text("# foreign a\n", encoding="utf-8")
            (skills_dir / "ghostless-draft").symlink_to(foreign_a, target_is_directory=True)

            # First install leaves the pre-existing foreign symlink alone.
            out = self._run_cli(home, suite, ["install-skills"])
            self.assertIn("kept ", out)
            self.assertIn("not installed by ghostless", out)
            self.assertIn("ghostless skills installed: 2; existing paths kept: 1", out)
            self.assertEqual((skills_dir / "ghostless-draft").resolve(), foreign_a.resolve())

            # Repoint an owned symlink to a foreign target after ghostless installed it.
            foreign_b = base / "foreign-b"
            foreign_b.mkdir()
            (foreign_b / "SKILL.md").write_text("# foreign b\n", encoding="utf-8")
            (skills_dir / "ghostless-edit").unlink()
            (skills_dir / "ghostless-edit").symlink_to(foreign_b, target_is_directory=True)

            out_repoint = self._run_cli(home, suite, ["install-skills"])
            self.assertIn("ghostless skills installed: 1; existing paths kept: 2", out_repoint)
            self.assertEqual((skills_dir / "ghostless-draft").resolve(), foreign_a.resolve())
            self.assertEqual((skills_dir / "ghostless-edit").resolve(), foreign_b.resolve())

            reset_out = self._run_cli(home, suite, ["reset", "--yes"])
            self.assertIn("Reset complete (1 skill paths removed).", reset_out)
            self.assertIn("not installed by ghostless", reset_out)
            self.assertEqual((skills_dir / "ghostless-draft").resolve(), foreign_a.resolve())
            self.assertEqual((skills_dir / "ghostless-edit").resolve(), foreign_b.resolve())
            self.assertFalse((skills_dir / "ghostless-learn").is_symlink())

            # Only --force replaces foreign symlinks.
            force_out = self._run_cli(home, suite, ["install-skills", "--force"])
            self.assertIn("replacing ", force_out)
            self.assertIn("(--force)", force_out)
            self.assertIn("ghostless skills installed: 3; existing paths kept: 0", force_out)
            for skill in harness.SKILLS:
                self.assertEqual((skills_dir / skill).resolve(), (suite / "skills" / skill).resolve())

    def test_project_local_node_modules_link_is_kept(self) -> None:
        import ghostless.harness as harness

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            skills_dir = home / ".claude" / "skills"
            skills_dir.mkdir(parents=True)
            suite = self._make_suite(
                base / "nvm" / "versions" / "node" / "v24.22.3" / "lib" / "node_modules" / "ghostless"
            )
            local = self._make_suite(base / "proj" / "node_modules" / "ghostless")
            local_skill = local / "skills" / "ghostless-draft"
            (skills_dir / "ghostless-draft").symlink_to(local_skill, target_is_directory=True)

            out = self._run_cli(home, suite, ["install-skills"])
            self.assertIn("kept ", out)
            self.assertEqual((skills_dir / "ghostless-draft").resolve(), local_skill.resolve())

            reset_out = self._run_cli(home, suite, ["reset", "--yes"])
            self.assertIn("not installed by ghostless", reset_out)
            self.assertEqual((skills_dir / "ghostless-draft").resolve(), local_skill.resolve())
            for skill in harness.SKILLS:
                if skill == "ghostless-draft":
                    continue
                self.assertFalse((skills_dir / skill).exists())


if __name__ == "__main__":
    unittest.main()
