"""Edit-session contract shared by command-line and desktop clients."""

from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import main  # noqa: E402
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import init_project  # noqa: E402
from ghostless.sessions import (  # noqa: E402
    finish_session,
    list_sessions,
    snapshot_session,
    start_session,
)


class SessionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)
        self.draft = self.base / "draft.md"
        self.draft.write_text("A first thought.\n", encoding="utf-8")

    def start(self) -> dict[str, object]:
        return start_session(
            self.project,
            self.draft,
            section="notes",
            origin="writer-first",
            sources=["https://example.org/reference"],
        )

    def test_start_uses_the_learn_slug_and_preserves_the_original(self) -> None:
        session = self.start()
        folder = self.project / str(session["path"])
        self.assertRegex(session["id"], r"^\d{4}-\d{2}-\d{2}-notes$")
        self.assertEqual((folder / "before.md").read_text(), "A first thought.\n")
        self.assertEqual((folder / "after.md").read_text(), "A first thought.\n")
        self.draft.write_text("Changed outside the session.\n")
        self.assertEqual((folder / "before.md").read_text(), "A first thought.\n")

    def test_start_and_finish_learn_once_and_are_idempotent(self) -> None:
        session = self.start()
        folder = self.project / str(session["path"])
        snapshot_session(self.project, session=str(session["id"]), after="A better thought.\n", notes="More direct.")
        receipt = finish_session(self.project, None, session=str(session["id"]))
        self.assertEqual(receipt["sources"], ["https://example.org/reference"])
        self.assertEqual(receipt["origin"], "writer-first")
        self.assertEqual(receipt["tier"], "gold")
        # The learned pair shares the session's id (the #8 invariant).
        self.assertEqual(receipt["learnedPath"], f"gold/{session['id']}")
        self.assertEqual((folder / "after.md").read_text(), "A better thought.\n")
        self.assertEqual((folder / "notes.md").read_text(), "More direct.")
        self.assertEqual(len(list((self.project / "gold").glob("*/meta.json"))), 1)
        # Finishing again with the same final returns the same receipt and does
        # not learn a second pair.
        self.assertEqual(finish_session(self.project, None, session=str(session["id"])), receipt)
        self.assertEqual(len(list((self.project / "gold").glob("*/meta.json"))), 1)
        self.assertEqual(list_sessions(self.project)[0]["status"], "finished")
        with self.assertRaisesRegex(ProjectError, "already finished"):
            snapshot_session(self.project, session=str(session["id"]), after="New", notes="")

    def test_missing_empty_identical_and_tampered_baseline(self) -> None:
        with self.assertRaisesRegex(ProjectError, "not found"):
            start_session(self.project, self.base / "missing.md", section=None, origin="ai-first", sources=[])
        with self.assertRaisesRegex(ProjectError, "portable reference"):
            start_session(self.project, self.draft, section=None, origin="ai-first", sources=[str(self.draft)])
        for absolute in ("/home/user/ref", r"C:\Users\me\ref", "C:/Users/me/ref"):
            with self.assertRaisesRegex(ProjectError, "portable reference"):
                start_session(
                    self.project, self.draft, section=None, origin="ai-first", sources=[absolute]
                )
        session = self.start()
        folder = self.project / str(session["path"])
        with self.assertRaisesRegex(ProjectError, "identical"):
            finish_session(self.project, None, session=str(session["id"]))
        (folder / "after.md").write_text("", encoding="utf-8")
        with self.assertRaisesRegex(ProjectError, "empty"):
            finish_session(self.project, None, session=str(session["id"]))
        (folder / "after.md").write_text("Changed.\n", encoding="utf-8")
        (folder / "before.md").write_text("Tampered.\n", encoding="utf-8")
        with self.assertRaisesRegex(ProjectError, "immutable baseline"):
            finish_session(self.project, None, session=str(session["id"]))

    def test_finish_reuses_existing_learn_and_rejects_changed_final(self) -> None:
        first = self.start()
        first_folder = self.project / str(first["path"])
        (first_folder / "after.md").write_text("A better thought.\n", encoding="utf-8")
        finish_session(self.project, None, session=str(first["id"]))
        second = self.start()
        second_folder = self.project / str(second["path"])
        (second_folder / "after.md").write_text("A better thought.\n", encoding="utf-8")
        receipt = finish_session(self.project, None, session=str(second["id"]))
        self.assertEqual(len(list((self.project / "gold").glob("*/meta.json"))), 1)
        self.assertEqual(
            receipt["learnedPath"],
            json.loads((first_folder / "receipt.json").read_text())["learnedPath"],
        )
        (second_folder / "after.md").write_text("Yet another thought.\n", encoding="utf-8")
        with self.assertRaisesRegex(ProjectError, "already finished"):
            finish_session(self.project, None, session=str(second["id"]))

    def test_finish_accepts_an_explicit_final_file(self) -> None:
        session = self.start()
        final = self.base / "final.md"
        final.write_text("From another editor.\n", encoding="utf-8")
        receipt = finish_session(self.project, final, session=str(session["id"]), note="Typed elsewhere.")
        folder = self.project / str(session["path"])
        self.assertEqual((folder / "after.md").read_text(), "From another editor.\n")
        self.assertEqual((folder / "notes.md").read_text(), "Typed elsewhere.")
        self.assertEqual(receipt["learnedPath"].split("/", 1)[0], "gold")

    def test_start_help_does_not_claim_to_open_the_editor(self) -> None:
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            code = main(["--help"])
        text = stdout.getvalue().lower()
        self.assertEqual(code, 0)
        self.assertNotIn("open an edit session", text)
        self.assertIn("working copy", text)
        self.assertIn("desktop", text)
        self.assertIn("editor", text)

    def test_edit_starts_a_session_and_invokes_the_editor(self) -> None:
        log = self.base / "editor-args"
        script = _write_fake_editor(self.base / "fake-editor.py")
        env = {
            "GHOSTLESS_EDITOR": str(script),
            "GHOSTLESS_EDITOR_LOG": str(log),
            "GHOSTLESS_NO_UPDATE": "1",
            "GHOSTLESS_HOME": str(self.base / "home"),
        }
        with (
            patch.dict(os.environ, env),
            contextlib.chdir(self.project),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            os.environ.pop("GHOSTLESS_PROJECT", None)
            code = main(["edit", str(self.draft), "--json"])
        self.assertEqual(code, 0, log.read_text() if log.exists() else "editor was not invoked")
        recorded = log.read_text(encoding="utf-8").splitlines()
        self.assertEqual(recorded[0], "--project")
        self.assertEqual(Path(recorded[1]), self.project.resolve())
        self.assertEqual(recorded[2], "--session")
        session = self.project / "sessions" / recorded[3]
        self.assertTrue((session / "before.md").is_file())
        self.assertEqual((session / "before.md").read_text(encoding="utf-8"), "A first thought.\n")

    def test_edit_keeps_the_session_when_the_editor_is_missing(self) -> None:
        env = {
            "GHOSTLESS_EDITOR": str(self.base / "missing-editor"),
            "GHOSTLESS_NO_UPDATE": "1",
            "GHOSTLESS_HOME": str(self.base / "home"),
        }
        with (
            patch.dict(os.environ, env),
            contextlib.chdir(self.project),
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()) as stderr,
        ):
            os.environ.pop("GHOSTLESS_PROJECT", None)
            code = main(["edit", str(self.draft)])
        self.assertEqual(code, 2)
        message = stderr.getvalue()
        self.assertIn("GHOSTLESS_EDITOR does not exist", message)
        self.assertIn("Finish later with: ghostless finish --session", message)
        sessions = list((self.project / "sessions").glob("*/session.json"))
        self.assertEqual(len(sessions), 1)

    def test_sessions_open_invokes_the_editor_for_that_id(self) -> None:
        log = self.base / "open-args"
        script = _write_fake_editor(self.base / "open-editor.py")
        session = self.start()
        env = {
            "GHOSTLESS_EDITOR": str(script),
            "GHOSTLESS_EDITOR_LOG": str(log),
            "GHOSTLESS_NO_UPDATE": "1",
            "GHOSTLESS_HOME": str(self.base / "home"),
        }
        with (
            patch.dict(os.environ, env),
            contextlib.chdir(self.project),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            os.environ.pop("GHOSTLESS_PROJECT", None)
            code = main(["sessions", "--open", str(session["id"])])
        self.assertEqual(code, 0)
        recorded = log.read_text(encoding="utf-8").splitlines()
        self.assertEqual(recorded[2:], ["--session", str(session["id"])])

    def test_invalid_check_leaves_session_files_unchanged(self) -> None:
        session = self.start()
        folder = self.project / str(session["path"])
        snapshot_session(
            self.project,
            session=str(session["id"]),
            after="A better thought.\n",
            notes="Keep this note.",
        )
        after_text = (folder / "after.md").read_text(encoding="utf-8")
        notes_text = (folder / "notes.md").read_text(encoding="utf-8")
        with self.assertRaisesRegex(ProjectError, "sentence length"):
            finish_session(
                self.project,
                None,
                session=str(session["id"]),
                note="Should not land.",
                checks=["Keep the median sentence length near the target."],
            )
        self.assertEqual((folder / "after.md").read_text(encoding="utf-8"), after_text)
        self.assertEqual((folder / "notes.md").read_text(encoding="utf-8"), notes_text)
        self.assertFalse((folder / "receipt.json").exists())
        metadata = json.loads((folder / "session.json").read_text(encoding="utf-8"))
        self.assertEqual(metadata["status"], "open")
        self.assertEqual(list((self.project / "gold").glob("*/meta.json")), [])

    def test_retry_repairs_open_status_when_receipt_exists(self) -> None:
        session = self.start()
        folder = self.project / str(session["path"])
        (folder / "after.md").write_text("A better thought.\n", encoding="utf-8")
        finish_session(self.project, None, session=str(session["id"]))
        metadata = json.loads((folder / "session.json").read_text(encoding="utf-8"))
        self.assertEqual(metadata["status"], "final")
        metadata["status"] = "open"
        (folder / "session.json").write_text(json.dumps(metadata) + "\n", encoding="utf-8")
        finish_session(self.project, None, session=str(session["id"]))
        healed = json.loads((folder / "session.json").read_text(encoding="utf-8"))
        self.assertEqual(healed["status"], "final")


def _write_fake_editor(path: Path) -> Path:
    """A Python editor double. Windows starts ``.py`` files through the interpreter."""
    path.write_text(
        "#!/usr/bin/env python3\n"
        "import os\n"
        "import sys\n"
        "from pathlib import Path\n"
        "Path(os.environ['GHOSTLESS_EDITOR_LOG']).write_text(\n"
        "    '\\n'.join(sys.argv[1:]) + '\\n', encoding='utf-8')\n",
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


if __name__ == "__main__":
    unittest.main()
