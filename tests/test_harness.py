#!/usr/bin/env python3
"""Installer and launcher checks. Uses a fake HOME so nothing touches the real one."""

from __future__ import annotations

import json
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

    def test_reset_keeps_foreign_skill_directory(self) -> None:
        with tempfile_home() as home:
            env = os.environ.copy()
            env["HOME"] = str(home)
            env["GHOSTLESS_HOME"] = str(home / ".ghostless")
            env["GHOSTLESS_NO_UPDATE"] = "1"
            install = subprocess.run(
                ["bash", str(ROOT / "setup"), "--agent", "cursor", "--no-launcher"],
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
            self.assertIn("kept", reset.stdout)

    def test_install_keeps_foreign_skill_symlink(self) -> None:
        with tempfile_home() as home:
            env = os.environ.copy()
            env["HOME"] = str(home)
            env["GHOSTLESS_HOME"] = str(home / ".ghostless")
            env["GHOSTLESS_NO_UPDATE"] = "1"
            skills = home / ".cursor" / "skills"
            skills.mkdir(parents=True, exist_ok=True)
            foreign = home / "my-own-skill"
            foreign.mkdir()
            (skills / "ghostless-draft").symlink_to(foreign)
            completed = subprocess.run(
                ["bash", str(ROOT / "setup"), "--agent", "cursor", "--no-launcher"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            link = skills / "ghostless-draft"
            self.assertTrue(link.is_symlink())
            self.assertEqual(os.readlink(link), str(foreign))
            self.assertTrue((skills / "ghostless-edit" / "SKILL.md").is_file())

    def test_install_keeps_foreign_launcher(self) -> None:
        with tempfile_home() as home:
            env = os.environ.copy()
            env["HOME"] = str(home)
            env["GHOSTLESS_HOME"] = str(home / ".ghostless")
            env["GHOSTLESS_NO_UPDATE"] = "1"
            launcher = home / ".local" / "bin" / "ghostless"
            launcher.parent.mkdir(parents=True, exist_ok=True)
            launcher.write_text("#!/usr/bin/env bash\necho foreign-tool\n", encoding="utf-8")
            completed = subprocess.run(
                ["bash", str(ROOT / "setup"), "--agent", "cursor"],
                check=False, capture_output=True, text=True, env=env,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
            self.assertIn("foreign-tool", launcher.read_text(encoding="utf-8"))
            self.assertIn("not a ghostless launcher", completed.stdout)

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

    def test_npm_package_is_unscoped_ghostless(self) -> None:
        surfaces = [
            ROOT / "package.json",
            ROOT / "bin" / "ghostless",
            ROOT / "setup",
            ROOT / "lib" / "agents.sh",
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
        script = r"""
set -euo pipefail
source "$ROOT/lib/agents.sh"
global="$HOME/.local/share/mise/installs/node/22/lib/node_modules/ghostless"
scoped="$HOME/.local/share/mise/installs/node/22/lib/node_modules/@hirefrank/ghostless"
npx_copy="$HOME/.npm/_npx/abc/node_modules/ghostless"
if ghostless_is_npm_global_install "$global"; then echo global_yes; else echo global_no; fi
if ghostless_is_npm_global_install "$scoped"; then echo scoped_yes; else echo scoped_no; fi
if ghostless_is_packaged_install "$npx_copy"; then echo packaged_yes; else echo packaged_no; fi
if ghostless_is_packaged_install "$ROOT"; then echo checkout_yes; else echo checkout_no; fi
ghostless_setup_rerun_command
"""
        env = os.environ.copy()
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
        self.assertEqual(lines[:4], ["global_yes", "scoped_no", "packaged_yes", "checkout_no"])
        self.assertIn("ghostless@latest", lines[4])
        self.assertNotIn("@hirefrank", lines[4])

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
