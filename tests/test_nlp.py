#!/usr/bin/env python3
"""Tokenizer, sentence splitting, contraction, and signature-phrase tests."""

from __future__ import annotations

import json
import os
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.stats import Piece, summarize  # noqa: E402
from ghostless.tells import ban_hits, structural_tells  # noqa: E402
from ghostless.text import (  # noqa: E402
    STOPWORDS,
    first_word,
    is_contraction,
    measure,
    split_sentences,
    words,
)


class UnicodeTokenizerTests(unittest.TestCase):
    def test_unicode_words_and_first_word(self) -> None:
        self.assertEqual(words("Café naïve résumé"), ["Café", "naïve", "résumé"])
        self.assertEqual(words("foo_bar Café—naïve"), ["foo", "bar", "Café", "naïve"])
        self.assertEqual(first_word("Café helps."), "café")
        self.assertEqual(first_word('"Éclair?"'), "éclair")
        self.assertEqual(first_word("..."), "")


class SentenceSplittingAndQuestionsTests(unittest.TestCase):
    def test_quoted_sentence_splitting(self) -> None:
        self.assertEqual(
            split_sentences('"First one." Second one.'),
            ['"First one."', "Second one."],
        )
        self.assertEqual(
            split_sentences('("First one.") [Second one!] Third.'),
            ['("First one.")', "[Second one!]", "Third."],
        )

    def test_quoted_and_parenthetical_questions(self) -> None:
        measured = measure('"Why now?" (Really?)')
        self.assertEqual(measured.sentences, 2)
        self.assertEqual(measured.questions, 2)

        mixed = measure('She asked, "Why now?" Then [Really?] And yes.')
        self.assertEqual(mixed.sentences, 3)
        self.assertEqual(mixed.questions, 2)

    def test_punctuation_only_text_has_no_sentences(self) -> None:
        for text in ("?", "...", "?!", "—"):
            measured = measure(text)
            self.assertEqual(measured.sentence_lengths, (), text)
            self.assertEqual(measured.sentence_tokens, (), text)
            self.assertEqual(measured.sentences, 0, text)
            self.assertEqual(measured.words, 0, text)
            self.assertEqual(measured.questions, 0, text)

            summary = summarize(
                [Piece("p.md", "gold", "prose", None, 1.0, text)],
                bronze_format_only=False,
                use_weights=True,
            )
            self.assertIsNone(summary["metrics"]["medianSentenceLength"], text)


class ContractionTests(unittest.TestCase):
    def test_expanded_s_contractions_and_possessives(self) -> None:
        for token in ("he's", "she's", "how's", "where's", "when's", "why's", "He's", "Why's"):
            self.assertTrue(is_contraction(token), token)

        for token in ("Frank's", "desk's", "writer's", "café's"):
            self.assertFalse(is_contraction(token), token)

        measured = measure("He's late, and Frank's coffee is cold.")
        self.assertEqual(measured.contractions, 1)

    def test_stopwords_include_expected_words_and_exclude_voice_tokens(self) -> None:
        self.assertIn("of", STOPWORDS)
        self.assertIn("that", STOPWORDS)
        self.assertIn("this", STOPWORDS)
        self.assertNotIn("i", STOPWORDS)
        self.assertNotIn("don't", STOPWORDS)


class SignaturePhraseTests(unittest.TestCase):
    def test_multi_piece_corpus_requires_two_distinct_pieces(self) -> None:
        pieces = [
            Piece(
                "one.md",
                "gold",
                "prose",
                "notes",
                3.0,
                (
                    "Purple hedgehog runs. Purple hedgehog jumps. Purple hedgehog sleeps. "
                    "Purple hedgehog eats. Purple hedgehog waits. "
                    "I think this works. I don't think otherwise. Frank's coffee helps."
                ),
            ),
            Piece(
                "two.md",
                "silver",
                "prose",
                "notes",
                1.0,
                "When I think about it, I don't think twice. Frank's coffee cools.",
            ),
        ]
        summary = summarize(pieces, bronze_format_only=False, use_weights=True)
        phrases = [item["phrase"] for item in summary["signaturePhrases"]]
        self.assertNotIn("purple hedgehog", phrases)
        self.assertNotIn("frank's coffee", phrases)
        self.assertIn("i think", phrases)
        self.assertIn("don't think", phrases)

    def test_single_piece_corpus_keeps_repeated_phrase(self) -> None:
        pieces = [
            Piece(
                "only.md",
                "gold",
                "prose",
                "notes",
                3.0,
                "Cold coffee waits on the desk. Cold coffee helps me start.",
            ),
        ]
        summary = summarize(pieces, bronze_format_only=False, use_weights=True)
        phrases = [item["phrase"] for item in summary["signaturePhrases"]]
        self.assertIn("cold coffee", phrases)


class TellRegressionTests(unittest.TestCase):
    def test_labeled_precision_recall_and_sentence_boundaries(self) -> None:
        cases = json.loads((ROOT / "tests/fixtures/nlp-labels.json").read_text(encoding="utf-8"))
        expected: set[tuple[str, str]] = set()
        observed: set[tuple[str, str]] = set()
        for case in cases:
            identifier, text = case["id"], case["text"]
            expected.update((identifier, kind) for kind in case["tells"])
            observed.update((identifier, hit.kind) for hit in structural_tells(text))
            observed.update((identifier, tell) for tell, _ in ban_hits(text, ["delve", "delve into"]))
            if "sentences" in case:
                self.assertEqual(measure(text).sentences, case["sentences"], identifier)

        # Literal shopping lists and quoted corrections are known heuristic
        # false positives. The floors preserve usefulness without requiring
        # these patterns to infer intent from a short snippet.
        correct = len(observed & expected)
        self.assertGreaterEqual(correct / max(1, len(observed)), 0.80, sorted(observed - expected))
        self.assertGreaterEqual(correct / len(expected), 0.90, sorted(expected - observed))


if __name__ == "__main__":
    os.environ.setdefault("GHOSTLESS_NO_UPDATE", "1")
    unittest.main()
