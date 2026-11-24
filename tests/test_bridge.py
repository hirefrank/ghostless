"""Bridge request and persistent stdio protocol coverage."""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.bridge import handle_request  # noqa: E402
from ghostless.paths import version  # noqa: E402

CHECK = 'The opening avoids "Level 9:".'
SECOND_CHECK = 'The opening uses "The correction".'


class BridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.base = Path(temp.name).resolve()
        self.workspace = self.base / "writings"
        self.voice = self.workspace / "voice"
        shutil.copytree(SAMPLE, self.voice)

        env = patch.dict(os.environ, {"GHOSTLESS_HOME": str(self.base / "home")})
        env.start()
        self.addCleanup(env.stop)
        os.environ.pop("GHOSTLESS_PROJECT", None)

    def test_methods_via_handle_request(self) -> None:
        with contextlib.chdir(self.base):
            self.exercise_methods(handle_request)

    def test_methods_via_persistent_stdio(self) -> None:
        env = {
            **os.environ,
            "PYTHONPATH": str(ROOT / "lib"),
            # The bridge must still write valid UTF-8 when launched under a different locale.
            "PYTHONIOENCODING": "ascii",
        }
        env.pop("GHOSTLESS_NO_UPDATE", None)
        with subprocess.Popen(
            [sys.executable, "-m", "ghostless", "bridge"],
            cwd=self.base,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            encoding="utf-8",
        ) as process:
            assert process.stdin is not None
            assert process.stdout is not None
            assert process.stderr is not None
            stdin, stdout = process.stdin, process.stdout

            def round_trip(line: str) -> tuple[str, dict[str, object]]:
                stdin.write(line + "\n")
                stdin.flush()
                reply = stdout.readline()
                self.assertTrue(reply, "Bridge exited before replying.")
                return reply, json.loads(reply)

            def invoke(request: dict[str, object]) -> dict[str, object]:
                return round_trip(json.dumps(request, ensure_ascii=False))[1]

            for invalid in ('{"id":', '[]', '{"method":"ping","id": NaN}'):
                with self.subTest(invalid=invalid):
                    _, response = round_trip(invalid)
                    self.assertIsNone(response["id"])
                    self.assertFalse(response["ok"])
                    self.assertTrue(response["error"])
            raw, ping = round_trip('{"id":"café","method":"ping"}')
            self.assertIn('"id":"café"', raw)
            self.assertTrue(ping["ok"])

            self.exercise_methods(invoke)
            self.exercise_cache(invoke)

            stdin.close()
            self.assertEqual(process.wait(timeout=10), 0)
            self.assertEqual(stdout.read(), "")
            self.assertEqual(process.stderr.read(), "")

    def exercise_cache(self, invoke: Callable[[dict[str, object]], dict[str, object]]) -> None:
        request = {
            "id": "cache",
            "method": "judge_text",
            "params": {"project": str(self.voice), "text": "The correction."},
        }
        first = invoke(request)
        self.assertTrue(first["ok"])

        stats_path = self.voice / "voice-stats.json"
        stamp = stats_path.stat()
        stats_path.write_text("[]", encoding="utf-8")
        os.utime(stats_path, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
        self.assertEqual(invoke(request), first)

        os.utime(stats_path, ns=(stamp.st_atime_ns, stamp.st_mtime_ns + 2_000_000_000))
        changed = invoke(request)
        self.assertFalse(changed["ok"])
        self.assertIn("must contain a JSON object", str(changed["error"]))

    def exercise_methods(self, invoke: Callable[[dict[str, object]], dict[str, object]]) -> None:
        counter = 0

        def ask(method: str, **params: object) -> Any:
            nonlocal counter
            counter += 1
            response = invoke({"id": counter, "method": method, "params": params})
            self.assertEqual(response["id"], counter)
            self.assertIs(response["ok"], True, response.get("error"))
            return response["result"]

        self.assertEqual(ask("ping"), {"version": version()})
        for path in (self.voice, self.workspace):
            self.assertEqual(
                ask("resolve_project", path=str(path)),
                {"project": str(self.voice), "source": "--project"},
            )
        self.assertEqual(ask("resolve_project"), {"project": None, "source": None})

        root = str(self.voice)
        stats_path = self.voice / "voice-stats.json"
        self.assertEqual(
            ask("judge_text", project=root, text="A draft."),
            {"needsBuild": True, "error": "No voice-stats.json yet. Run build first."},
        )
        self.assertFalse(stats_path.exists())

        summary = ask("build", project=root)
        self.assertEqual(set(summary), {"goldPieces", "counts", "bronzeFormatOnly", "warnings", "generatedAt"})

        source = self.workspace / "draft.md"
        source.write_text("Level 9: starts the note. Then we stop.\n", encoding="utf-8")
        started = ask(
            "session_start", project=root, draft=str(source), section="notes", sources=["source.md"],
        )
        session = started["id"]
        self.assertEqual(started["origin"], "ai-first")
        self.assertEqual(started["sources"], ["source.md"])
        self.assertEqual(started["sourceDraftName"], "draft.md")

        [editing] = ask("sessions_list", project=root)
        self.assertEqual(editing["status"], "editing")
        self.assertEqual(editing["section"], "notes")
        self.assertEqual(editing["origin"], "ai-first")
        self.assertNotIn("editRate", editing)

        after = "The correction starts the note. Then we stop.\n"
        snapshot = ask("session_snapshot", project=root, session=session, after=after, notes="Français")
        self.assertEqual(snapshot["id"], session)
        folder = self.voice / started["path"]
        self.assertEqual((folder / "after.md").read_text(encoding="utf-8"), after)
        self.assertEqual((folder / "notes.md").read_text(encoding="utf-8"), "Français")

        # Exercise finishing from an explicit path as well as the snapshot text.
        final = self.workspace / "final.md"
        final.write_text(after, encoding="utf-8")
        receipt = ask(
            "session_finish", project=root, session=session, final=str(final), note="Changed opening",
            checks=[CHECK], rebuild=True, promote=True,
        )
        self.assertTrue(receipt["rebuilt"])
        self.assertEqual(receipt["promotion"]["tier"], "gold")
        self.assertEqual(receipt["checks"][0]["text"], CHECK)
        self.assertEqual((folder / "notes.md").read_text(encoding="utf-8"), "Changed opening")

        [finished] = ask("sessions_list", project=root)
        self.assertEqual(finished["status"], "finished")
        self.assertEqual(finished["editRate"], receipt["changes"]["editRate"])
        self.assertEqual(finished["learnedPath"], receipt["learnedPath"])
        slug = receipt["learnedPath"].removeprefix("gold/")

        first_id = receipt["checks"][0]["id"]
        [pending] = ask("checks_list", project=root)["pending"]
        self.assertEqual(pending["id"], first_id)
        confirmed = ask("checks_decide", project=root, action="confirm", target=first_id, section="notes")
        self.assertEqual(confirmed["status"], "confirmed")
        [added] = ask("checks_decide", project=root, action="add", target=slug, text=SECOND_CHECK)
        second_id = added["id"]
        ask("checks_decide", project=root, action="confirm", target=second_id, section="notes")

        aiish_text = (self.voice / "drafts" / "ai-ish.md").read_text(encoding="utf-8")
        judged_text = "Level 9: The correction starts the note.\n\n" + aiish_text + "\n[WRITER: your take]"
        judged = ask("judge_text", project=root, text=judged_text, section="notes")
        self.assertIsInstance(judged["score"], int)
        self.assertIn({"tell": "delve", "count": 1}, judged["banned"])
        self.assertTrue(judged["structure"])
        self.assertEqual(judged["placeholders"], ["[WRITER: your take]"])
        self.assertIn("notes", judged["availableSections"])
        self.assertEqual(judged["availableSections"], sorted(judged["availableSections"]))
        self.assertTrue(judged["stale"])
        self.assertIn("checks.json", judged["staleSources"])
        self.assertEqual({item["id"]: item["pass"] for item in judged["confirmedChecks"]}, {
            first_id: False,
            second_id: True,
        })
        self.assertFalse(ask("judge_text", project=root, text=judged_text)["confirmedChecks"])
        self.assertEqual(
            ask("checks_decide", project=root, action="reject", target=second_id)["status"], "rejected",
        )

        for kind in ("style", "audience"):
            note = ask("save_note", project=root, kind=kind, text="Précis et court.")
            expected = self.voice / f"{kind.upper()}.md"
            self.assertEqual(note["path"], str(expected))
            self.assertIn("Précis et court.", expected.read_text(encoding="utf-8"))
        ask("build", project=root)
        self.assertFalse(ask("judge_text", project=root, text=after, section="notes")["stale"])

        # Creating enough edited examples should surface a real rule candidate.
        for index in range(3):
            source.write_text(f"I actually keep note {index} short. I stop here.\n", encoding="utf-8")
            edit = ask("session_start", project=root, draft=str(source), origin="writer-first")
            ask(
                "session_snapshot", project=root, session=edit["id"],
                after=f"I keep note {index} short. I stop here.\n", notes="",
            )
            ask("session_finish", project=root, session=edit["id"])

        rule_id = "cut:actually"
        candidates = ask("rules_list", project=root)["hypotheses"]
        self.assertIn(rule_id, {candidate["id"] for candidate in candidates})
        for action, status in (("confirm", "confirmed"), ("retire", "retired"), ("reject", "rejected")):
            self.assertEqual(ask("rules_decide", project=root, action=action, ruleId=rule_id)["status"], status)

        self.assertEqual(ask("promote", project=root, slug=slug, demote=True)["tier"], "edited")
        self.assertEqual(ask("promote", project=root, slug=slug)["tier"], "gold")

        for method, params, error in (
            ("missing", {}, "Unknown method"),
            ("judge_text", {"project": root, "text": 42}, "'text' must be a string"),
            ("build", {"project": str(self.base / "missing")}, "Not a ghostless project"),
            ("session_start", {"project": root, "draft": str(self.base)}, "Draft file not found"),
        ):
            response = invoke({"id": method, "method": method, "params": params})
            self.assertEqual(response["id"], method)
            self.assertIs(response["ok"], False)
            self.assertIn(error, str(response["error"]))


if __name__ == "__main__":
    unittest.main()
