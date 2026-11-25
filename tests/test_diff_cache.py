"""Tests for contiguous move-span detection and per-pair diff caching (#44)."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

import ghostless.learn as learn_mod  # noqa: E402
import ghostless.sessions as sessions_mod  # noqa: E402
from ghostless.learn import (  # noqa: E402
    DIFF_SUMMARY_KEYS,
    DIFF_VERSION,
    diff_texts,
    learn_edit,
    refresh_edit_summary,
)
from ghostless.project import init_project  # noqa: E402
from ghostless.schema import DiffSummary  # noqa: E402
from ghostless.sessions import finish_session, snapshot_session, start_session  # noqa: E402


class MoveSpanTests(unittest.TestCase):
    def test_diff_summary_keys_match_diff_texts_output(self) -> None:
        summary = diff_texts("Cold coffee waits here.", "Hot tea waits there.")
        self.assertEqual(tuple(summary.keys()), DIFF_SUMMARY_KEYS)

    def test_unrelated_paragraphs_sharing_stopwords_report_no_moves(self) -> None:
        before = (
            "The old brown fox jumps over a fence in autumn.\n\nA bright blue bird flies through the tall pine trees."
        )
        after = (
            "Several grey wolves trot beside a quiet river in winter.\n\n"
            "Two quick deer leap past the mossy stone walls."
        )
        diff = diff_texts(before, after)
        self.assertEqual(diff["movedTokens"], 0)
        self.assertEqual(diff["movedSpans"], [])

        # Even a contiguous two-stopword overlap ("in the") is not a move.
        before_bigram = "Dogs bark in the yard.\n\nAnchor sentence stays right here."
        after_bigram = "Anchor sentence stays right here.\n\nCats purr in the barn."
        bigram_diff = diff_texts(before_bigram, after_bigram)
        self.assertEqual(bigram_diff["movedTokens"], 0)
        self.assertEqual(bigram_diff["movedSpans"], [])

    def test_reordering_clause_across_paragraphs_detects_moves(self) -> None:
        before = "First paragraph opens cleanly. Cold rain falls.\n\nSecond paragraph has many more words than before."
        after = "First paragraph opens cleanly.\n\nSecond paragraph has many more words than before. Cold rain falls."
        diff = diff_texts(before, after)
        self.assertEqual(diff["deletions"], 0)
        self.assertEqual(diff["insertions"], 0)
        self.assertEqual(diff["replacements"], 0)
        self.assertEqual(diff["movedTokens"], 3)
        self.assertEqual(len(diff["movedSpans"]), 1)
        self.assertEqual(diff["movedSpans"][0]["text"], "cold rain falls")
        self.assertEqual(diff["movedSpans"][0]["tokens"], 3)


class DiffCacheTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)

    def _learn_pair(
        self,
        before: str,
        after: str,
        *,
        date: str = "2026-01-05",
        section: str = "notes",
    ) -> dict[str, object]:
        before_path = self.base / "before.md"
        after_path = self.base / "after.md"
        before_path.write_text(before, encoding="utf-8")
        after_path.write_text(after, encoding="utf-8")
        return learn_edit(
            self.project,
            before=before_path,
            after=after_path,
            section=section,
            origin="writer-first",
            date=date,
        )

    def test_refresh_edit_summary_cache_hit_skips_diff_texts(self) -> None:
        # Deliberately omit trailing newline to ensure stored normalization matches cache hashes.
        result = self._learn_pair(
            "We should delve deeper into the old draft",
            "I cut the drift and kept one clean line",
        )
        meta_path = self.project / "gold" / str(result["slug"]) / "meta.json"
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        self.assertEqual(meta["diffVersion"], DIFF_VERSION)
        self.assertIsInstance(meta["beforeSha256"], str)
        self.assertIsInstance(meta["afterSha256"], str)

        expected = result["edits"]
        with patch.object(
            learn_mod,
            "diff_texts",
            side_effect=AssertionError("diff_texts should not run on cache hit"),
        ):
            cached = refresh_edit_summary(self.project)
        self.assertEqual(cached, expected)

    def test_refresh_edit_summary_cache_miss_never_rewrites_meta(self) -> None:
        result = self._learn_pair(
            "We should delve deeper into the draft.\n",
            "I cut the filler.\n",
        )
        pair_dir = self.project / "gold" / str(result["slug"])
        meta_path = pair_dir / "meta.json"
        after_path = pair_dir / "after.md"

        # 1. Modified after.md triggers diff_texts without touching meta.json.
        original_meta_text = meta_path.read_text(encoding="utf-8")
        original_mtime_ns = meta_path.stat().st_mtime_ns
        after_path.write_text("I rewrote the ending completely now.\n", encoding="utf-8")

        real_diff = learn_mod.diff_texts
        calls = 0

        def counting_diff(before: str, after: str) -> DiffSummary:
            nonlocal calls
            calls += 1
            return real_diff(before, after)

        with patch.object(learn_mod, "diff_texts", side_effect=counting_diff):
            edits = refresh_edit_summary(self.project)

        self.assertEqual(calls, 1)
        self.assertEqual(meta_path.read_text(encoding="utf-8"), original_meta_text)
        self.assertEqual(meta_path.stat().st_mtime_ns, original_mtime_ns)
        self.assertEqual(
            edits["learns"][0]["wordsAfter"],
            real_diff("", "I rewrote the ending completely now.\n")["wordsAfter"],
        )

        # 2. Stale diffVersion or missing diffVersion also recomputes without writing meta.json.
        for bad_version in (1, None):
            stale_meta = json.loads(original_meta_text)
            if bad_version is None:
                stale_meta.pop("diffVersion", None)
            else:
                stale_meta["diffVersion"] = bad_version
            meta_path.write_text(
                json.dumps(stale_meta, indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )
            after_path.write_text("I cut the filler.\n", encoding="utf-8")
            stale_meta_text = meta_path.read_text(encoding="utf-8")
            stale_mtime_ns = meta_path.stat().st_mtime_ns
            calls = 0

            with patch.object(learn_mod, "diff_texts", side_effect=counting_diff):
                recomputed = refresh_edit_summary(self.project)

            self.assertEqual(calls, 1)
            self.assertEqual(meta_path.read_text(encoding="utf-8"), stale_meta_text)
            self.assertEqual(meta_path.stat().st_mtime_ns, stale_mtime_ns)
            self.assertEqual(
                recomputed["learns"][0]["wordsAfter"],
                real_diff("We should delve deeper into the draft.\n", "I cut the filler.\n")["wordsAfter"],
            )

    def test_finish_session_calls_diff_texts_at_most_once_for_new_session(self) -> None:
        draft = self.base / "draft.md"
        draft.write_text("First rough line without trailing newline", encoding="utf-8")
        session = start_session(
            self.project,
            draft,
            section="notes",
            origin="writer-first",
            sources=[],
        )
        session_id = str(session["id"])
        snapshot_session(
            self.project,
            session=session_id,
            after="Cleaner revised line without trailing newline",
            notes="Tightened.",
        )

        real_diff = learn_mod.diff_texts
        learn_calls = 0
        session_calls = 0

        def count_learn_diff(before: str, after: str) -> DiffSummary:
            nonlocal learn_calls
            learn_calls += 1
            return real_diff(before, after)

        def count_session_diff(before: str, after: str) -> DiffSummary:
            nonlocal session_calls
            session_calls += 1
            return real_diff(before, after)

        with (
            patch.object(learn_mod, "diff_texts", side_effect=count_learn_diff),
            patch.object(sessions_mod, "diff_texts", side_effect=count_session_diff),
        ):
            receipt = finish_session(self.project, None, session=session_id)

        self.assertLessEqual(learn_calls + session_calls, 1)
        self.assertEqual(learn_calls, 1)
        self.assertEqual(session_calls, 0)
        self.assertEqual(
            receipt["changes"],
            real_diff(
                "First rough line without trailing newline\n",
                "Cleaner revised line without trailing newline\n",
            ),
        )


if __name__ == "__main__":
    unittest.main()
