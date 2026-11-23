#!/usr/bin/env python3
"""Installer and CLI shim checks. Uses a fake HOME so nothing touches the real one."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.harness import AGENTS, _ephemeral_package, agent_dir  # noqa: E402
from ghostless.paths import version  # noqa: E402


class HarnessMapTests(unittest.TestCase):
    def test_agent_directories_match_the_suite_map(self) -> None:
        fake_home = Path(tempfile.gettempdir()) / "ghostless-home-map"
        expected = {
            "claude-code": fake_home / ".claude" / "skills",
            "cursor": fake_home / ".cursor" / "skills",
            "codex": fake_home / ".codex" / "skills",
            "opencode": fake_home / ".config" / "opencode" / "skills",
            "pi": fake_home / ".pi" / "agent" / "skills",
            "gemini-cli": fake_home / ".gemini" / "skills",
            "hermes-agent": fake_home / ".hermes" / "skills",
            "muse": fake_home / ".agents" / "skills",
            "agents": fake_home / ".agents" / "skills",
        }
        self.assertEqual(set(AGENTS), set(expected))
        with patch.object(Path, "home", return_value=fake_home):
            actual = {name: agent_dir(name) for name in AGENTS}
        self.assertEqual(actual, expected)

    def test_npm_package_is_unscoped_ghostless(self) -> None:
        self.assertFalse((ROOT / "lib" / "agents.sh").exists())
        surfaces = [
            ROOT / "package.json",
            ROOT / "bin" / "ghostless",
            ROOT / "setup",
            ROOT / "lib" / "ghostless" / "harness.py",
            ROOT / "README.md",
            ROOT / "docs" / "RELEASE.md",
            ROOT / ".github" / "workflows" / "publish.yml",
            *sorted((ROOT / "skills").rglob("SKILL.md")),
        ]
        for path in surfaces:
            text = path.read_text(encoding="utf-8")
            self.assertNotIn("@hirefrank/ghostless", text, str(path))
        package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        self.assertEqual(package["name"], "ghostless")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertIn("npm i -g ghostless@latest", readme)
        self.assertIn("npx --yes ghostless@latest install-skills", readme)
        self.assertIn("pnpm dlx ghostless@latest install-skills", readme)
        harness_text = (ROOT / "lib" / "ghostless" / "harness.py").read_text(encoding="utf-8")
        self.assertIn("ghostless@latest", harness_text)
        self.assertNotIn("@hirefrank", harness_text)
        self.assertTrue(_ephemeral_package(Path("/home/user/.npm/_npx/abc/node_modules/ghostless")))
        self.assertTrue(_ephemeral_package(Path("/home/user/.cache/pnpm/dlx/abc/node_modules/ghostless")))
        self.assertFalse(_ephemeral_package(ROOT))
        self.assertFalse(_ephemeral_package(Path("/home/user/.local/share/mise/installs/node/22/lib/node_modules/ghostless")))


@unittest.skipIf(os.name == "nt", "Bash shims run on Unix only")
class HarnessTests(unittest.TestCase):
    def _env(self, home: Path, **extra: str) -> dict[str, str]:
        env = os.environ.copy()
        env["HOME"] = str(home)
        env["GHOSTLESS_HOME"] = str(home / ".ghostless")
        env["GHOSTLESS_NO_UPDATE"] = "1"
        env["GHOSTLESS_PYTHON"] = sys.executable
        env.update(extra)
        return env

    def test_install_skills_links_into_a_fake_home(self) -> None:
        with tempfile_home() as home:
            env = self._env(home)
            completed = subprocess.run(
                [str(ROOT / "setup"), "--agent", "cursor"],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                cwd=ROOT,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
            skill = home / ".cursor" / "skills" / "ghostless-draft"
            self.assertTrue(skill.is_symlink())
            self.assertTrue((skill / "SKILL.md").is_file())
            self.assertTrue((home / ".cursor" / "skills" / "ghostless-edit" / "SKILL.md").is_file())
            self.assertTrue((home / ".cursor" / "skills" / "ghostless-learn" / "SKILL.md").is_file())
            suite_file = home / ".ghostless" / "suite-root"
            self.assertTrue(suite_file.is_file())
            self.assertEqual(suite_file.read_text(encoding="utf-8").strip(), str(ROOT.resolve()))
            config = json.loads((home / ".ghostless" / "config.json").read_text(encoding="utf-8"))
            self.assertEqual(config["updateCheck"], True)

    def test_reset_keeps_foreign_skill_directory(self) -> None:
        with tempfile_home() as home:
            env = self._env(home)
            install = subprocess.run(
                [str(ROOT / "setup"), "--agent", "cursor"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(install.returncode, 0, install.stderr)
            skills = home / ".cursor" / "skills"
            foreign = skills / "ghostless-edit"
            foreign.unlink()
            foreign.mkdir()
            (foreign / "SKILL.md").write_text("foreign skill", encoding="utf-8")
            reset = subprocess.run(
                [str(ROOT / "bin" / "ghostless"), "reset", "--agent", "cursor", "--yes"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(reset.returncode, 0, reset.stderr)
            self.assertTrue((foreign / "SKILL.md").is_file())
            self.assertFalse((skills / "ghostless-draft").exists())
            self.assertFalse((skills / "ghostless-learn").exists())
            self.assertIn("kept", reset.stdout)

    def test_install_keeps_foreign_skill_symlink(self) -> None:
        with tempfile_home() as home:
            env = self._env(home)
            skills = home / ".cursor" / "skills"
            skills.mkdir(parents=True, exist_ok=True)
            foreign = home / "my-own-skill"
            foreign.mkdir()
            (skills / "ghostless-draft").symlink_to(foreign)
            completed = subprocess.run(
                [str(ROOT / "setup"), "--agent", "cursor"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            link = skills / "ghostless-draft"
            self.assertTrue(link.is_symlink())
            self.assertEqual(os.readlink(link), str(foreign))
            self.assertTrue((skills / "ghostless-edit" / "SKILL.md").is_file())

    def test_setup_works_when_suite_reached_via_symlink(self) -> None:
        with tempfile_home() as home:
            env = self._env(home)
            link_root = home / "ghostless-link"
            link_root.symlink_to(ROOT)
            completed = subprocess.run(
                [str(link_root / "setup"), "--agent", "cursor"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
            self.assertTrue((home / ".cursor" / "skills" / "ghostless-draft" / "SKILL.md").is_file())
            self.assertEqual(
                (home / ".ghostless" / "suite-root").read_text(encoding="utf-8").strip(),
                str(ROOT.resolve()),
            )

    def test_doctor_reports_project_and_matches_python_module(self) -> None:
        with tempfile_home() as home:
            project = home / "voice"
            project.mkdir()
            (project / "manifest.json").write_text('{"kind": "ghostless"}\n', encoding="utf-8")
            env = self._env(home, GHOSTLESS_PROJECT=str(project))
            py_env = env.copy()
            py_env["PYTHONPATH"] = str(ROOT / "lib")

            bin_doctor = subprocess.run(
                [str(ROOT / "bin" / "ghostless"), "doctor"],
                check=False, capture_output=True, text=True, env=env, cwd=home,
            )
            py_doctor = subprocess.run(
                [sys.executable, "-m", "ghostless", "doctor"],
                check=False, capture_output=True, text=True, env=py_env, cwd=home,
            )
            self.assertEqual(bin_doctor.returncode, 0, bin_doctor.stderr)
            self.assertEqual(py_doctor.returncode, 0, py_doctor.stderr)
            self.assertIn("Project:", bin_doctor.stdout)
            self.assertEqual(bin_doctor.stdout, py_doctor.stdout)

    def test_ghostless_skills_dir_install_and_reset(self) -> None:
        with tempfile_home() as home:
            custom_skills = home / "custom-skills"
            env = self._env(home, GHOSTLESS_SKILLS_DIR=str(custom_skills))
            install = subprocess.run(
                [str(ROOT / "bin" / "ghostless"), "install-skills"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(install.returncode, 0, install.stderr + install.stdout)
            for skill in ("ghostless-draft", "ghostless-edit", "ghostless-learn"):
                self.assertTrue((custom_skills / skill).is_symlink())
                self.assertTrue((custom_skills / skill / "SKILL.md").is_file())
            self.assertFalse((home / ".claude").exists())

            reset = subprocess.run(
                [str(ROOT / "bin" / "ghostless"), "reset", "--yes"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(reset.returncode, 0, reset.stderr + reset.stdout)
            for skill in ("ghostless-draft", "ghostless-edit", "ghostless-learn"):
                self.assertFalse((custom_skills / skill).exists())

    def test_launcher_version_is_offline(self) -> None:
        env = os.environ.copy()
        env["GHOSTLESS_NO_UPDATE"] = "1"
        completed = subprocess.run(
            [str(ROOT / "bin" / "ghostless"), "--version"],
            check=False,
            capture_output=True,
            text=True,
            env=env,
            cwd=ROOT,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn(f"ghostless {version()}", completed.stdout)

    def test_bash_syntax(self) -> None:
        completed = subprocess.run(
            ["bash", "-n", str(ROOT / "bin" / "ghostless"), str(ROOT / "setup")],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)


class tempfile_home:
    def __enter__(self) -> Path:
        self._tmp = tempfile.TemporaryDirectory()
        return Path(self._tmp.name)

    def __exit__(self, *args: object) -> None:
        self._tmp.cleanup()


if __name__ == "__main__":
    unittest.main()
