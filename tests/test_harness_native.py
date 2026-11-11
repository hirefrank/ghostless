"""Tests for the portable npm/Python entry path, with isolated skill storage."""

from __future__ import annotations

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

    def test_package_entry_is_portable_node_script(self) -> None:
        package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        self.assertEqual(package["bin"]["ghostless"], "bin/ghostless.cjs")
        self.assertTrue((ROOT / "bin" / "ghostless.cjs").is_file())


if __name__ == "__main__":
    unittest.main()
