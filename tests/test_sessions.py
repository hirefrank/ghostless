"""Edit-session contract shared by command-line and desktop clients."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

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


if __name__ == "__main__":
    unittest.main()
