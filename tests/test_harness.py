#!/usr/bin/env python3
"""Installer and launcher checks. Uses a fake HOME so nothing touches the real one."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class HarnessTests(unittest.TestCase):
    def test_agent_directories_match_the_suite_map(self) -> None:
        script = """
set -euo pipefail
source "$ROOT/lib/agents.sh"
home="$HOME"
printf '%s\\n' \\
  "$(ghostless_agent_dir claude-code)" \\
  "$(ghostless_agent_dir cursor)" \\
  "$(ghostless_agent_dir codex)" \\
  "$(ghostless_agent_dir opencode)" \\
  "$(ghostless_agent_dir pi)" \\
  "$(ghostless_agent_dir gemini-cli)" \\
  "$(ghostless_agent_dir hermes-agent)" \\
  "$(ghostless_agent_dir muse)" \\
  "$(ghostless_agent_dir agents)"
"""
        env = os.environ.copy()
        env["HOME"] = "/tmp/ghostless-home-map"
        env["ROOT"] = str(ROOT)
        completed = subprocess.run(
            ["bash", "-c", script],
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        lines = [line for line in completed.stdout.splitlines() if line]
        home = "/tmp/ghostless-home-map"
        self.assertEqual(
            lines,
            [
                f"{home}/.claude/skills",
                f"{home}/.cursor/skills",
                f"{home}/.codex/skills",
                f"{home}/.config/opencode/skills",
                f"{home}/.pi/agent/skills",
                f"{home}/.gemini/skills",
                f"{home}/.hermes/skills",
                f"{home}/.agents/skills",
                f"{home}/.agents/skills",
            ],
        )

    def test_install_skills_links_into_a_fake_home(self) -> None:
        with tempfile_home() as home:
            env = os.environ.copy()
            env["HOME"] = str(home)
            env["GHOSTLESS_HOME"] = str(home / ".ghostless")
            env["GHOSTLESS_NO_UPDATE"] = "1"
            completed = subprocess.run(
                ["bash", str(ROOT / "setup"), "--agent", "cursor", "--no-launcher"],
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
            self.assertTrue((home / ".ghostless" / "suite-root").is_file())
            config = (home / ".ghostless" / "config.json").read_text(encoding="utf-8")
            self.assertIn("updateCheck", config)

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
        self.assertIn("ghostless 0.1.0", completed.stdout)

    def test_bash_syntax(self) -> None:
        completed = subprocess.run(
            ["bash", "-n", str(ROOT / "bin" / "ghostless"), str(ROOT / "setup"), str(ROOT / "lib" / "agents.sh")],
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
