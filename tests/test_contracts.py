"""Validate live Python output and the shared editor JSON fixtures."""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any, cast
from unittest.mock import patch

from ghostless.bridge import handle_request
from ghostless.build import build_voice
from ghostless.checks import checks_summary
from ghostless.project import init_project
from ghostless.rules import rules_summary
from ghostless.schema import BridgeRequest
from ghostless.sessions import SESSION_ID_RE, finish_session, snapshot_session, start_session

try:
    from jsonschema import Draft202012Validator
except ModuleNotFoundError as exc:
    if os.environ.get("GHOSTLESS_REQUIRE_DEV_TESTS") == "1":
        raise
    raise unittest.SkipTest("Install the dev extra to validate JSON contracts") from exc

CONTRACTS = Path(__file__).resolve().parents[1] / "fixtures" / "contracts"
CHECK = 'The opening avoids "Level 9:".'
AFTER = "The correction starts the note. Then we stop.\n"


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def live_contracts(base: Path) -> dict[str, Any]:
    voice = init_project(base / "voice", git=False)
    (voice / "corpus/gold/notes.md").write_text("I dropped the first line. The rest worked.\n", encoding="utf-8")
    draft = base / "draft.md"
    draft.write_text("Level 9: starts the note. Then we stop.\n", encoding="utf-8")
    records: dict[str, Any] = {"Manifest": read_json(voice / "manifest.json")}
    transactions: list[dict[str, Any]] = []

    def call(method: str, *, succeeds: bool = True, **params: object) -> Any:
        request = cast(BridgeRequest, {"id": len(transactions) + 1, "method": method, "params": params})
        response = handle_request(request)
        assert response["ok"] is succeeds, response
        transactions.append({"request": request, "response": response})
        return response.get("result")

    project = str(voice)
    call("ping")
    call("resolve_project", path=str(base))
    call("judge_text", project=project, text=AFTER, section=None)
    records["CompiledStats"] = build_voice(voice)
    call("build", project=project)
    started = start_session(voice, draft, section="notes", origin="writer-first", sources=[])
    session = started["id"]
    snapshot_session(voice, session=session, after=AFTER, notes="Keep this concrete.")
    folder = voice / started["path"]
    records["SessionMeta"] = read_json(folder / "session.json")
    call("sessions_list", project=project)
    receipt = finish_session(voice, None, session=session, checks=[CHECK])
    records["SessionReceipt"] = read_json(folder / "receipt.json")
    records["EditMeta"] = read_json(voice / receipt["learnedPath"] / "meta.json")
    (folder / "session.json").write_text(json.dumps(records["SessionMeta"]), encoding="utf-8")
    call("sessions_list", project=project)
    assert finish_session(voice, None, session=session) == receipt
    assert read_json(folder / "session.json")["status"] == "final"
    return finish_contracts(voice, draft, records, transactions, call, receipt["checks"][0]["id"])


def finish_contracts(
    voice: Path,
    draft: Path,
    records: dict[str, Any],
    transactions: list[dict[str, Any]],
    call: Callable[..., Any],
    check_id: str,
) -> dict[str, Any]:
    project = str(voice)
    started = call("session_start", project=project, draft=str(draft), section="notes", origin="ai-first", sources=[])
    session = started["id"]
    call("session_finish", succeeds=False, project=project, session=session, rebuild=False, promote=False, checks=[])
    edited = "Here is the correction. Then we stop.\n"
    call("session_snapshot", project=project, session=session, after=edited, notes="Keep the last sentence.")
    receipt = call("session_finish", project=project, session=session, note=None, rebuild=True, promote=True, checks=[])
    call("promote", project=project, slug=receipt["learnedPath"].removeprefix("gold/"), demote=False)
    with patch("ghostless.bridge.build.build_voice", side_effect=OSError("fixture rebuild error")):
        warned = call("session_finish", project=project, session=session, rebuild=True)
    assert warned["warnings"] == ["Rebuild failed: fixture rebuild error"]
    assert "rebuilt" not in warned
    records["ChecksSummary"] = checks_summary(voice)
    call("checks_list", project=project)
    call(
        "checks_decide",
        project=project,
        target=check_id,
        action="confirm",
        section="notes",
        text=None,
        **{"global": False},
    )
    call(
        "checks_decide", project=project, target=check_id, action="confirm", section=None, text=None, **{"global": True}
    )
    call("judge_text", project=project, text=AFTER, section="notes")
    rules = voice / "rules.json"
    rules.write_text(
        json.dumps(
            {
                "kind": "ghostless-rules",
                "version": 1,
                "confirmed": {"cut:delve": {"at": "2026-01-02", "receipts": [], "pieces": []}},
                "rejected": {},
                "retired": {},
                "scope": {"cut:delve": "notes"},
            }
        ),
        encoding="utf-8",
    )
    call("rules_decide", project=project, ruleId="cut:delve", action="confirm", section=None, **{"global": True})
    records["RuleSummary"] = rules_summary(voice)
    call("rules_list", project=project)
    call("save_note", project=project, kind="style", text="Keep the opening short.")

    rule_state = read_json(rules)
    rule_state["retired"]["cut:delve"] = {"at": "2026-01-02"}
    rule_state["rejected"]["cut:delve"] = {"at": "2026-01-03"}
    rules.write_text(json.dumps(rule_state), encoding="utf-8")
    records["RulesState"] = read_json(rules)
    rules_view = call("rules_list", project=project)
    assert rules_view["confirmed"] == rules_view["retired"] == []
    assert [rule["id"] for rule in rules_view["rejected"]] == ["cut:delve"]

    checks = voice / "checks.json"
    check_state = read_json(checks)
    check_state["rejected"][check_id] = {"at": "2026-01-03", "text": CHECK}
    checks.write_text(json.dumps(check_state), encoding="utf-8")
    records["ChecksState"] = read_json(checks)
    checks_view = call("checks_list", project=project)
    assert all(check["id"] != check_id for check in checks_view["confirmed"] + checks_view["pending"])
    assert [check["id"] for check in checks_view["rejected"]] == [check_id]
    return {"records": records, "bridge": transactions}


class ContractTests(unittest.TestCase):
    schema: dict[str, Any]

    @classmethod
    def setUpClass(cls) -> None:
        cls.schema = read_json(CONTRACTS / "contracts.schema.json")
        Draft202012Validator.check_schema(cls.schema)

    def validator(self, name: str) -> Draft202012Validator:
        return Draft202012Validator({**self.schema, "$ref": f"#/$defs/{name}"})

    def validate(self, name: str, data: object) -> None:
        with self.subTest(contract=name):
            errors = list(self.validator(name).iter_errors(data))
            self.assertEqual([], [f"{err.json_path}: {err.message}" for err in errors])

    def validate_bundle(self, bundle: dict[str, Any]) -> None:
        for name, value in bundle["records"].items():
            self.validate(name, value)
        for exchange in bundle["bridge"]:
            request, response = exchange["request"], exchange["response"]
            method = request["method"]
            self.validate(f"{method}Request", request)
            self.validate("BridgeRequest", request)
            self.validate("BridgeResponse", response)
            self.assertEqual(request["id"], response["id"])
            if response["ok"]:
                self.validate(f"{method}Result", response["result"])

    def test_live_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.validate_bundle(live_contracts(Path(tmp).resolve()))

    def test_golden_contracts(self) -> None:
        self.validate_bundle(read_json(CONTRACTS / "valid.json"))
        for record in read_json(CONTRACTS / "invalid.json"):
            with self.subTest(contract=record["schema"]):
                self.assertFalse(self.validator(record["schema"]).is_valid(record["payload"]))

    def test_shared_session_ids(self) -> None:
        validator = self.validator("SessionId")
        for case in read_json(CONTRACTS / "session-ids.json"):
            with self.subTest(identifier=case["id"]):
                self.assertEqual(case["valid"], bool(SESSION_ID_RE.fullmatch(case["id"])))
                self.assertEqual(case["valid"], validator.is_valid(case["id"]))
