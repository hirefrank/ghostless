"""Mechanical text measure and pool weight tests. Sample data is invented."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.stats import Piece, compile_stats, pools_for, summarize, weighted_median  # noqa: E402
from ghostless.text import measure, split_sentences  # noqa: E402


class TextTests(unittest.TestCase):
    def test_abbreviation_and_decimal_stay_intact(self) -> None:
        sentences = split_sentences("Dr. Moss tried it. 3.14 is enough. Then she stopped.")
        self.assertEqual(
            sentences,
            ["Dr. Moss tried it.", "3.14 is enough.", "Then she stopped."],
        )

    def test_measure_counts(self) -> None:
        text = "Dr. Moss tried it. I don't wait.\n\nAnd I'm done? So we stop."
        measured = measure(text)
        self.assertEqual(measured.sentences, 4)
        self.assertEqual(measured.sentence_lengths, (4, 3, 3, 3))
        self.assertEqual(measured.contractions, 2)
        self.assertEqual(measured.i_count, 2)
        self.assertEqual(measured.questions, 1)
        self.assertEqual(measured.openers, {"and": 1, "but": 0, "so": 1})
        self.assertEqual(measured.paragraph_lengths, (7, 6))
        self.assertEqual(measured.em_dashes, 0)

    def test_em_dash_forms(self) -> None:
        self.assertEqual(measure("Wait — now.").em_dashes, 1)
        self.assertEqual(measure("Wait -- now.").em_dashes, 1)
        self.assertEqual(measure("well-known").em_dashes, 0)


class WeightTests(unittest.TestCase):
    def test_weighted_median_prefers_gold(self) -> None:
        # One short sentence carrying gold's weight beats two long ones.
        self.assertEqual(weighted_median([(10, 3), (100, 1), (100, 1)]), 10)
        self.assertEqual(weighted_median([(10, 1), (100, 1), (100, 1)]), 100)

    def test_spoken_is_rhythm_only_and_bronze_drops_out_of_voice(self) -> None:
        pieces = [
            Piece("g.md", "gold", "prose", "notes", 3, "I write.\n\nI stop."),
            Piece(
                "b.md",
                "bronze",
                "prose",
                "archive",
                0.5,
                "one two three four five six seven eight nine ten.",
            ),
            Piece("s.md", "silver", "spoken", None, 1, "And I go — now.\n\nSo I go."),
        ]
        self.assertEqual(pools_for(pieces[2], bronze_format_only=False), frozenset({"rhythm"}))
        self.assertEqual(
            pools_for(pieces[1], bronze_format_only=True),
            frozenset({"format"}),
        )
        stats = compile_stats(pieces, weights={"gold": 3, "silver": 1, "bronze": 0.5}, gold_voice_minimum=1)
        self.assertTrue(stats["bronzeFormatOnly"])
        weighted = stats["weighted"]["metrics"]
        self.assertEqual(weighted["rhythmSentences"], 8)
        self.assertEqual(weighted["formatParagraphs"], 6.5)
        self.assertEqual(weighted["emDashesPer1k"], 0)
        unweighted = stats["unweighted"]["metrics"]
        self.assertEqual(unweighted["rhythmSentences"], 4)
        # Spoken "now" is not written prose, so it cannot save a tell.
        # "leverage" is not in this tiny set; the empty-diction check is separate.
        self.assertIn("delve", stats["weighted"]["banList"])
        self.assertNotIn("leverage", _diction_text(pieces))

    def test_gold_spoken_does_not_drop_bronze_voice(self) -> None:
        pieces = [
            Piece("s1.md", "gold", "spoken", None, 3, "And I go now. So I stop."),
            Piece("s2.md", "gold", "spoken", None, 3, "But I stay."),
            Piece("b.md", "bronze", "prose", "archive", 0.5, "one two three four five six seven eight nine ten."),
        ]
        stats = compile_stats(pieces, weights={"gold": 3, "silver": 1, "bronze": 0.5}, gold_voice_minimum=2)
        # Two gold transcripts are not written prose; they must not make
        # bronze format-only and flush the only written sample from voice.
        self.assertFalse(stats["bronzeFormatOnly"])
        self.assertIn("delve", stats["weighted"]["banList"])
        self.assertIsNotNone(stats["weighted"]["metrics"]["medianSentenceLength"])


def _diction_text(pieces: list[Piece]) -> str:
    summary = summarize(pieces, bronze_format_only=True, use_weights=True)
    return " ".join(summary["tellsInUse"])


if __name__ == "__main__":
    unittest.main()
