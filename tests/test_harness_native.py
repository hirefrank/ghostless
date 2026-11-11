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


class PathWarningTests(unittest.TestCase):
    def _fake_npm(self, base: Path, prefix: Path) -> Path:
        """A stub `npm` that reports `prefix` for `npm config get prefix`."""
        fakebin = base / "fakebin"
        fakebin.mkdir()
        npm = fakebin / "npm"
        npm.write_text(f"#!/bin/sh\necho {prefix}\n", encoding="utf-8")
        npm.chmod(0o755)
        return fakebin

    def test_bin_dir_detected_and_warning_when_missing(self) -> None:
        from ghostless.harness import npm_bin_path_warning, npm_global_bin_dir

        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            prefix = base / "prefix"
            fakebin = self._fake_npm(base, prefix)
            env = {"PATH": str(fakebin), "GHOSTLESS_HOME": str(base / "data")}
            with patch.dict(os.environ, env):
                # PATH holds only the stub, so prefix/bin is missing from it.
                self.assertEqual(npm_global_bin_dir(), prefix / "bin")
                warning = npm_bin_path_warning()
                self.assertIsNotNone(warning)
                self.assertIn(str(prefix / "bin"), warning)
                self.assertIn('export PATH="', warning)
                with patch.dict(
                    os.environ,
                    {"PATH": os.pathsep.join([str(fakebin), str(prefix / "bin")])},
                ):
                    self.assertIsNone(npm_bin_path_warning())

    def test_no_npm_means_no_warning(self) -> None:
        from ghostless.harness import npm_global_bin_dir

        with tempfile.TemporaryDirectory() as tmp:
            with patch.dict(os.environ, {"PATH": tmp}):
                os.environ.pop("NPM_CONFIG_PREFIX", None)
                self.assertIsNone(npm_global_bin_dir())

    def test_doctor_warns_when_bin_dir_missing_from_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            prefix = base / "prefix"
            fakebin = self._fake_npm(base, prefix)
            env = {"PATH": str(fakebin), "GHOSTLESS_HOME": str(base / "data")}
            with patch.dict(os.environ, env):
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    self.assertEqual(main(["doctor"]), 0)
            self.assertIn(str(prefix / "bin"), out.getvalue())
            self.assertIn("export PATH=", out.getvalue())


if __name__ == "__main__":
    unittest.main()
