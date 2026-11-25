"""Stable launcher install and editor discovery."""

from __future__ import annotations

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless import launch  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402


class LaunchTests(unittest.TestCase):
    def test_version_scoped_install_requires_a_version_manager_marker(self) -> None:
        scoped = Path("/home/frank/.nvm/versions/node/20/lib/node_modules/ghostless")
        plain = Path("/usr/lib/node_modules/ghostless")
        self.assertTrue(launch.version_scoped_install(scoped))
        self.assertFalse(launch.version_scoped_install(plain))
        self.assertFalse(launch.version_scoped_install(Path("/tmp/ghostless")))

    def test_unreadable_prefix_is_not_owned(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(launch._read_prefix(Path(tmp)), "")
            self.assertFalse(launch.owned_launcher(Path(tmp)))

    def test_install_skip_keep_write_and_remove(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            with patch.object(launch, "stable_launcher_dir", return_value=directory):
                self.assertEqual(
                    launch.install_stable_launcher(enabled=False, transient=False), "launcher skipped (--no-launcher)"
                )
                self.assertEqual(
                    launch.install_stable_launcher(enabled=True, transient=True),
                    "launcher skipped (one-off package)",
                )
                foreign = directory / "ghostless"
                foreign.write_text("echo other\n", encoding="utf-8")
                self.assertEqual(
                    launch.install_stable_launcher(enabled=True, transient=False),
                    f"kept {foreign} (not a ghostless launcher)",
                )
                foreign.unlink()
                with patch.dict(os.environ, {"PATH": "/usr/bin"}):
                    installed = launch.install_stable_launcher(enabled=True, transient=False)
                self.assertIn(str(foreign), installed)
                self.assertIn("add", installed)
                self.assertTrue(launch.owned_launcher(foreign))
                self.assertEqual(launch.launcher_status_line(), f"Launcher: {foreign}")
                with patch.dict(os.environ, {"GHOSTLESS_STABLE_LAUNCHER": "1", "PATH": str(directory)}):
                    self.assertIn("this command", launch.launcher_status_line())
                    on_path = launch.install_stable_launcher(enabled=True, transient=False)
                self.assertNotIn("add", on_path)
                removed = launch.remove_owned_launcher()
                self.assertEqual(removed, foreign)
                self.assertFalse(foreign.exists())
                self.assertIsNone(launch.remove_owned_launcher())
                self.assertEqual(launch.launcher_status_line(), "Launcher: none")

    def test_windows_launcher_is_a_batch_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            with (
                patch.object(launch, "stable_launcher_dir", return_value=directory),
                patch.object(launch, "_is_windows", return_value=True),
                patch.dict(os.environ, {"PATH": str(directory)}),
            ):
                path = directory / "ghostless.cmd"
                self.assertIn(str(path), launch.install_stable_launcher(enabled=True, transient=False))
                self.assertIn("exit /b", path.read_text(encoding="utf-8"))

    def test_install_scope_mentions_the_stable_launcher(self) -> None:
        scoped = Path("/home/frank/.nvm/versions/node/20/lib/node_modules/ghostless")
        self.assertIsNone(launch.install_scope_line(None))
        self.assertIsNone(launch.install_scope_line(Path("/tmp/voice")))
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            with patch.object(launch, "stable_launcher_dir", return_value=directory):
                self.assertIn("install-skills", launch.install_scope_line(scoped) or "")
                launch.install_stable_launcher(enabled=True, transient=False)
                owned = launch.install_scope_line(scoped) or ""
                self.assertIn("stable launcher keeps", owned)
                with patch.dict(os.environ, {"GHOSTLESS_STABLE_LAUNCHER": "1"}):
                    via = launch.install_scope_line(scoped) or ""
                self.assertIn("This doctor run is the stable launcher", via)

    def test_editor_report_and_binary_discovery(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            missing = root / "missing-editor"
            with patch.dict(os.environ, {"GHOSTLESS_EDITOR": str(missing)}):
                self.assertIn("does not exist", launch.editor_report(root))
                self.assertIsNone(launch.find_editor_binary(root))
            binary = root / "editor" / "target" / "release" / "ghostless-editor"
            binary.parent.mkdir(parents=True)
            binary.write_text("", encoding="utf-8")
            os.environ.pop("GHOSTLESS_EDITOR", None)
            self.assertEqual(launch.find_editor_binary(root, honor_override=False), binary)
            self.assertIn(str(binary), launch.editor_report(root))
            cargo = root / "editor" / "Cargo.toml"
            cargo.write_text("[package]\n", encoding="utf-8")
            binary.unlink()
            self.assertIn("not built", launch.editor_report(root))
            self.assertIn("not in this install", launch.editor_report(None))
            absent = root / "no-such-binary"
            with (
                patch.object(launch, "_is_windows", return_value=True),
                patch.object(launch, "_which", return_value=absent),
            ):
                self.assertIsNone(launch.find_editor_binary(None, honor_override=False))

    def test_launch_editor_builds_session_and_draft_commands(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            project.mkdir()
            binary = Path(tmp) / "ghostless-editor"
            binary.write_text("", encoding="utf-8")
            seen: list[list[str]] = []

            def fake_run(command: list[str], check: bool = False) -> subprocess.CompletedProcess[str]:
                del check
                seen.append(command)
                return subprocess.CompletedProcess(command, 0)

            with (
                patch.dict(os.environ, {"GHOSTLESS_EDITOR": str(binary)}),
                patch.object(launch.subprocess, "run", side_effect=fake_run),
            ):
                draft = project / "draft.md"
                linked = project / "linked.md"
                self.assertEqual(launch.launch_editor(project, draft=draft, link_draft=linked), 0)
                self.assertEqual(launch.launch_editor(project, session="2026-01-02-notes", link_draft=linked), 0)
                with self.assertRaises(ProjectError):
                    launch.launch_editor(project)
            self.assertIn("--draft", seen[0])
            self.assertIn("--session", seen[1])
            self.assertIn("--link-draft", seen[0])
            self.assertIn("--link-draft", seen[1])
            with patch.dict(os.environ, {}, clear=False):
                os.environ.pop("GHOSTLESS_EDITOR", None)
                with (
                    patch.object(launch, "find_editor_binary", return_value=None),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    self.assertEqual(launch.launch_editor(project, draft=project / "draft.md"), 2)

    def test_windows_spawn_wraps_scripts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            project.mkdir()
            seen: list[list[str]] = []

            def fake_run(command: list[str], check: bool = False) -> subprocess.CompletedProcess[str]:
                del check
                seen.append(command)
                return subprocess.CompletedProcess(command, 7)

            script = Path(tmp) / "editor.py"
            script.write_text("", encoding="utf-8")
            batch = Path(tmp) / "editor.cmd"
            batch.write_text("", encoding="utf-8")
            with (
                patch.object(launch, "_is_windows", return_value=True),
                patch.object(launch.subprocess, "run", side_effect=fake_run),
            ):
                with patch.dict(os.environ, {"GHOSTLESS_EDITOR": str(script)}):
                    self.assertEqual(launch.launch_editor(project, draft=project / "draft.md"), 7)
                with patch.dict(os.environ, {"GHOSTLESS_EDITOR": str(batch)}):
                    self.assertEqual(launch.launch_editor(project, draft=project / "draft.md"), 7)
            self.assertEqual(seen[0][0], sys.executable)
            self.assertEqual(seen[1][:2], ["cmd", "/c"])
            self.assertEqual(launch._spawn_command(Path(tmp) / "editor", ["editor"]), ["editor"])
