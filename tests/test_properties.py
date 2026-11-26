"""Reproducible properties over prose, weights, and real project transitions."""

from __future__ import annotations

import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from ghostless import bridge, checks, rules
from ghostless.build import build_voice, stale_inputs
from ghostless.io import atomic_write_json
from ghostless.judge import judge_text
from ghostless.learn import diff_texts
from ghostless.paths import ProjectError
from ghostless.project import init_project, require_contained_path
from ghostless.sessions import finish_session, snapshot_session, start_session
from ghostless.stats import Piece, compile_stats, weighted_median
from ghostless.text import measure, normalize, split_frontmatter, strip_non_prose, word_tokens

try:
    from hypothesis import given, settings
    from hypothesis import strategies as st
    from hypothesis.stateful import RuleBasedStateMachine, invariant, precondition, rule
except ModuleNotFoundError as exc:
    raise unittest.SkipTest("Install the dev extra to run property tests") from exc

QUICK = settings(max_examples=64, deadline=None, derandomize=True, print_blob=True)
FRAGMENTS = (
    "---\r\nsection: essay\r\n---\r\n",
    "~~~\ncode\n~~~~\n",
    "```md\n# Café\n```\n",
    "金曜日 “It’s fine.” ✨",
    "[link](https://example.test/a.b)",
    "| a | b |\n| -- | -- |\n",
    "alpha beta gamma",
    "?!…—’\t\x00",
)
PROSE = st.lists(st.one_of(st.sampled_from(FRAGMENTS), st.text(max_size=40)), max_size=9).map("".join)
BAD_UNICODE = st.sampled_from(("\ud800", "\udfff", "\x00"))
WEIGHT = st.integers(1, 100)
BEFORE = "The opener uses Level 9: as a joke. I keep the rest."
AFTER = "The opener states the correction. I keep the rest."
VALID_CHECK = 'The opener avoids "Level 9:".'


class PropertyTests(unittest.TestCase):
    @QUICK
    @given(PROSE)
    def test_normalization_and_markdown(self, text: str) -> None:
        self.assertEqual(normalize(normalize(text)), normalize(text))
        split_frontmatter(text)
        strip_non_prose(text)
        self.assertEqual(measure(text).words, len(word_tokens(text)))

    @QUICK
    @given(PROSE, PROSE)
    def test_diff_conserves_tokens(self, before: str, after: str) -> None:
        diff = diff_texts(before, after)
        left, right = word_tokens(before), word_tokens(after)
        paired = diff["unchangedTokens"] + diff["replacements"] + diff["movedTokens"]
        self.assertEqual(len(left), paired + diff["deletions"])
        self.assertEqual(len(right), paired + diff["insertions"])
        self.assertLessEqual(0.0, diff["editRate"])
        self.assertLessEqual(diff["editRate"], 1.0)
        self.assertEqual(diff["editRate"] == 0.0, left == right)

    def test_one_changed_token_in_a_long_draft_has_nonzero_rate(self) -> None:
        body = " ".join(str(n) for n in range(20_001))
        self.assertGreater(diff_texts(body, f"{body} appended")["editRate"], 0.0)

    @QUICK
    @given(st.lists(st.tuples(st.integers(-1000, 1000), WEIGHT), min_size=1, max_size=20), WEIGHT)
    def test_weighted_median_is_bounded_and_scale_invariant(self, values: list[tuple[int, int]], scale: int) -> None:
        median = weighted_median([(value, weight) for value, weight in values])
        assert median is not None
        self.assertLessEqual(min(value for value, _ in values), median)
        self.assertLessEqual(median, max(value for value, _ in values))
        self.assertEqual(median, weighted_median([(value, weight * scale) for value, weight in values]))

    @QUICK
    @given(st.lists(WEIGHT, min_size=1, max_size=5), st.lists(WEIGHT, max_size=6))
    def test_edited_weight_cap(self, writer: list[int], edited: list[int]) -> None:
        pieces = [Piece(str(i), "gold", "prose", None, w, "I made the change.") for i, w in enumerate(writer)]
        pieces.extend(Piece(str(i), "edited", "prose", None, w, "Here is the update.") for i, w in enumerate(edited))
        stats = compile_stats(pieces, weights={}, gold_voice_minimum=3)
        self.assertLessEqual(stats["edited"]["countedWeight"] / sum(writer), 0.25)
        self.assertLessEqual(stats["edited"]["countedWeight"], stats["edited"]["configuredWeight"])

    @QUICK
    @given(PROSE, st.integers(1, 7))
    def test_judge_scores_are_bounded_and_tell_penalties_are_monotone(self, prose: str, count: int) -> None:
        # Match the control and marked text on length and isolate the tell
        # penalties from prose-style changes such as sentence distribution.
        text = normalize(prose) + "\n\nA short sentence.\n\n"
        control = text + " ".join(["observe"] * count) + "."
        marked = text + " ".join(["delve"] * count) + "."
        target = {"medianSentenceLength": 8}
        stats = {"weighted": {"metrics": target, "banList": ["delve"]}}
        base = judge_text(control, stats)["score"]
        banned = judge_text(marked, stats)["score"]
        cut = {"id": "cut:delve", "kind": "cut", "word": "delve"}
        rule_stats = {"weighted": {"metrics": target}, "rules": {"confirmed": [cut]}}
        ruled = judge_text(marked, rule_stats)["score"]
        self.assertTrue(all(0 <= value <= 100 for value in (base, banned, ruled)))
        self.assertLessEqual(banned, base)
        self.assertLessEqual(ruled, judge_text(control, rule_stats)["score"])

    @QUICK
    @given(st.lists(st.sampled_from((".", "..", "é", "outside", "root")), min_size=1, max_size=9), st.booleans())
    def test_path_containment(self, segments: list[str], mixed: bool) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve() / "root"
            root.mkdir()
            fragment = "".join(part + ("\\" if mixed and i % 2 else "/") for i, part in enumerate(segments))
            candidate = root / fragment
            expected = candidate.resolve()
            if expected.is_relative_to(root):
                self.assertEqual(require_contained_path(root, candidate, label="generated"), expected)
            else:
                with self.assertRaises(ProjectError):
                    require_contained_path(root, candidate, label="generated")

    @QUICK
    @given(
        st.recursive(st.none() | st.integers() | BAD_UNICODE, lambda inner: st.lists(inner, max_size=3), max_leaves=10)
    )
    def test_bridge_recovers_after_rejected_lines(self, nested: object) -> None:
        malformed = [
            json.dumps({"id": nested, "method": "missing"}),
            json.dumps({"id": 1, "method": "save_note", "params": {"project": "\ud800"}}),
            '{"id":NaN}',
            '{"id":Infinity}',
            '{"id":-Infinity}',
            '{"id":' + "[" * 1100 + "0" + "]" * 1100 + "}",
        ]
        payload = "\n".join([*malformed, '{"id":9,"method":"ping"}', ""]).encode("utf-8")
        output = io.BytesIO()
        with (
            patch.object(bridge.sys, "stdin", io.TextIOWrapper(io.BytesIO(payload), encoding="utf-8")),
            patch.object(bridge.sys, "stdout", io.TextIOWrapper(output, encoding="utf-8")) as stdout,
        ):
            self.assertEqual(bridge.serve_stdio(), 0)
            stdout.flush()
            rows = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(len(rows), len(malformed) + 1)
        self.assertTrue(all(not row["ok"] for row in rows[:-1]))
        self.assertEqual((rows[-1]["id"], rows[-1]["ok"]), (9, True))


class VoiceWorkflow(RuleBasedStateMachine):
    def __init__(self) -> None:
        super().__init__()
        self.directory = tempfile.TemporaryDirectory()
        self.base = Path(self.directory.name)
        self.voice = init_project(self.base / "voice", git=False)
        self.draft = self.base / "draft.md"
        self.draft.write_text(BEFORE, encoding="utf-8")
        self.corpus = self.voice / "corpus/gold/first.md"
        self.corpus.write_text("This was written by hand.", encoding="utf-8")
        self.session = ""
        self.after = BEFORE
        self.finished = False
        self.check_id = ""
        self.check_status = ""
        self.rule_status = "confirmed"
        self.built_with_corpus = False
        atomic_write_json(
            self.voice / "rules.json",
            {
                "kind": "ghostless-rules",
                "version": 1,
                "confirmed": {"cut:delve": {"at": "2026-01-02", "receipts": [], "pieces": []}},
                "rejected": {},
                "retired": {},
                "scope": {"cut:delve": "notes"},
            },
        )

    def teardown(self) -> None:
        self.directory.cleanup()

    @rule()
    @precondition(lambda self: not self.session or self.finished)
    def start(self) -> None:
        self.session = start_session(self.voice, self.draft, section="notes", origin="writer-first", sources=[])["id"]
        self.after = BEFORE
        self.finished = False

    @rule(edit=st.booleans())
    @precondition(lambda self: bool(self.session))
    def snapshot(self, *, edit: bool) -> None:
        next_text = AFTER if edit else BEFORE
        if self.finished:
            with self.assert_project_error():
                snapshot_session(self.voice, session=self.session, after=next_text, notes="A note.")
        else:
            snapshot_session(self.voice, session=self.session, after=next_text, notes="A note.")
            self.after = next_text

    @rule(valid=st.booleans())
    @precondition(lambda self: bool(self.session))
    def finish(self, *, valid: bool) -> None:
        selected = [VALID_CHECK] if valid else ['The text contains "xyxy absent quote xyxy".']
        if self.after == BEFORE or not valid or self.check_status == "rejected":
            with self.assert_project_error():
                finish_session(self.voice, None, session=self.session, note="Finish note.", checks=selected)
        else:
            receipt = finish_session(self.voice, None, session=self.session, note="Finish note.", checks=selected)
            self.finished = True
            self.check_id = receipt["checks"][0]["id"]
            (self.check_status,) = self.check_buckets()

    @rule()
    @precondition(lambda self: self.finished)
    def retry_finished_session(self) -> None:
        path = self.voice / "sessions" / self.session / "receipt.json"
        before = json.loads(path.read_text(encoding="utf-8"))
        assert finish_session(self.voice, None, session=self.session) == before

    @staticmethod
    def assert_project_error() -> unittest.case._AssertRaisesContext[ProjectError]:
        return unittest.TestCase().assertRaises(ProjectError)

    @rule(scope=st.sampled_from(("notes", None)))
    @precondition(lambda self: self.rule_status == "confirmed")
    def confirm_rule(self, *, scope: str | None) -> None:
        assert rules.confirm_rule(self.voice, "cut:delve", section=scope)["scope"] == scope

    @rule(retire=st.booleans())
    @precondition(lambda self: self.rule_status == "confirmed")
    def suppress_rule(self, *, retire: bool) -> None:
        action = rules.retire_rule if retire else rules.reject_rule
        self.rule_status = action(self.voice, "cut:delve")["status"]

    @rule(reject=st.booleans(), clear=st.booleans())
    @precondition(lambda self: bool(self.check_id) and self.check_status != "rejected")
    def decide_check(self, *, reject: bool, clear: bool) -> None:
        if reject:
            checks.reject_check(self.voice, self.check_id)
            self.check_status = "rejected"
        else:
            decision = checks.confirm_check(
                self.voice, self.check_id, section=None if clear else "notes", clear_section=clear
            )
            assert decision.get("section") == (None if clear else "notes")
            self.check_status = "confirmed"

    @rule()
    def build(self) -> None:
        build_voice(self.voice)
        self.built_with_corpus = self.corpus.exists()
        assert stale_inputs(self.voice) == []

    @rule()
    @precondition(lambda self: self.corpus.exists())
    def delete_corpus(self) -> None:
        self.corpus.unlink()
        if self.built_with_corpus:
            assert "corpus/gold/first.md" in stale_inputs(self.voice)

    @invariant()
    def persistence_matches_model(self) -> None:
        if self.session:
            folder = self.voice / "sessions" / self.session
            assert (folder / "before.md").read_text(encoding="utf-8") == BEFORE
            assert (folder / "after.md").read_text(encoding="utf-8") == self.after
            assert (folder / "receipt.json").exists() == self.finished
            metadata = json.loads((folder / "session.json").read_text(encoding="utf-8"))
            assert metadata["status"] == ("final" if self.finished else "open")
        view = rules.rules_summary(self.voice)
        buckets = {"confirmed": view["confirmed"], "rejected": view["rejected"], "retired": view["retired"]}
        assert [status for status, rows in buckets.items() if rows] == [self.rule_status]
        if self.check_id:
            assert self.check_buckets() == [self.check_status]

    def check_buckets(self) -> list[str]:
        view = checks.checks_summary(self.voice)
        buckets = {"pending": view["pending"], "confirmed": view["confirmed"], "rejected": view["rejected"]}
        return [status for status, rows in buckets.items() if any(row["id"] == self.check_id for row in rows)]


TestVoiceWorkflow = VoiceWorkflow.TestCase
TestVoiceWorkflow.settings = settings(
    max_examples=18, stateful_step_count=16, deadline=None, derandomize=True, print_blob=True
)
