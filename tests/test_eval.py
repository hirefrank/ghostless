#!/usr/bin/env python3
"""Corpus cleanup, judge confidence, and held-out evaluation tests."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"

sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.eval import (  # noqa: E402
    ACCEPT_EDIT_RATE,
    evaluate,
    format_eval,
    load_edit_pairs,
    split_items,
)
from ghostless.judge import (  # noqa: E402
    CONFIDENCE_HIGH_WORDS,
    CONFIDENCE_MIN_WORDS,
    corpus_confidence,
    format_report,
    judge_text,
)
from ghostless.learn import diff_texts  # noqa: E402
from ghostless.project import discover, init_project, load_manifest  # noqa: E402
from ghostless.text import measure, normalize, strip_non_prose, words  # noqa: E402

WRITER = (
    "I keep the note short. I don't polish the first line.\n\n"
    "And I stop when the coffee is cold. So the page stays messy."
)
AI_DRAFT = (
    "Furthermore, we must delve into a seamless tapestry that elevates the work "
    "and fosters alignment across every layer of the organization."
)


def _confidence_stats(
    *,
    words: int,
    written: int,
    sentences: int,
    pieces: int,
) -> dict[str, object]:
    return {
        "weighted": {"metrics": {"rhythmWords": words, "dictionWords": written}},
        "unweighted": {
            "metrics": {
                "rhythmWords": words,
                "dictionWords": written,
                "rhythmSentences": sentences,
            }
        },
        "counts": {"total": pieces},
    }


def _make_pair(root: Path, date: str, before: str) -> None:
    folder = root / "gold" / f"{date}-notes"
    folder.mkdir(parents=True)
    (folder / "before.md").write_text(before, encoding="utf-8")
    (folder / "after.md").write_text(WRITER, encoding="utf-8")
    (folder / "meta.json").write_text(
        json.dumps(
            {
                "date": date,
                "section": "notes",
                "origin": "ai-first",
                "kind": "prose",
                "beforeFile": "before.md",
                "afterFile": "after.md",
            }
        )
        + "\n",
        encoding="utf-8",
    )


def _make_corpus(root: Path) -> None:
    """Writer-authored gold so the held-out eval has voice to train on."""
    (root / "corpus" / "gold" / "desk.md").write_text(WRITER, encoding="utf-8")
    (root / "corpus" / "gold" / "walk.md").write_text(
        "I take the long way home. So the walk clears my head.\n\n"
        "And I leave the phone behind when I go.",
        encoding="utf-8",
    )


class CorpusCleanupTests(unittest.TestCase):
    def test_prose_only_text_is_unchanged(self) -> None:
        prose = "I don't wait. And I'm done.\n\nSo we stop."
        self.assertEqual(strip_non_prose(prose), normalize(prose))
        measured = measure(prose)
        self.assertEqual(measured.words, 9)
        self.assertEqual(measured.sentence_lengths, (3, 3, 3))
        self.assertEqual(measured.paragraph_lengths, (6, 3))

    def test_markdown_scaffolding_is_not_measured(self) -> None:
        text = """---
section: notes
sample: invented
---

# Title

See [the notes](https://example.com/notes?x=1) and ![a photo](img.png).

```python
print("code here")
```

Some `inline code` stays out.

- bullet one
- bullet two

> A quoted line.

| a | b |
| --- | --- |
| one | two |

---
"""
        self.assertEqual(
            words(text),
            [
                "See",
                "the",
                "notes",
                "and",
                "Some",
                "stays",
                "out",
                "bullet",
                "one",
                "bullet",
                "two",
                "A",
                "quoted",
                "line",
                "a",
                "b",
                "one",
                "two",
            ],
        )
        measured = measure(text)
        self.assertEqual(measured.words, 18)
        # Frontmatter, code, table rules, and rules contribute no paragraphs.
        self.assertEqual(measured.paragraph_lengths, (4, 3, 4, 3, 4))

    def test_unclosed_and_tilde_fences_are_stripped(self) -> None:
        self.assertEqual(words("Before.\n\n~~~\ncode words here"), ["Before"])
        self.assertEqual(words("Before.\n\n```\ncode words here"), ["Before"])

    def test_link_text_is_kept_and_url_words_are_dropped(self) -> None:
        measured = measure("Read [this essay](https://example.com/a/b) now.")
        self.assertEqual(measured.words, 4)
        self.assertEqual(measured.sentence_lengths, (4,))

    def test_frontmatter_is_not_prose(self) -> None:
        self.assertEqual(words("---\nsection: notes\n---\nOnly these words."), ["Only", "these", "words"])


class ConfidenceTests(unittest.TestCase):
    def test_thresholds_are_reported(self) -> None:
        low = corpus_confidence(_confidence_stats(words=1400, written=1400, sentences=90, pieces=6))
        self.assertEqual(low["level"], "low")
        self.assertFalse(low["trustworthy"])
        self.assertEqual(low["words"], 1400)
        self.assertEqual(low["pieces"], 6)
        self.assertIn(str(CONFIDENCE_MIN_WORDS), str(low["note"]))

        few_pieces = corpus_confidence(
            _confidence_stats(
                words=CONFIDENCE_HIGH_WORDS + 500,
                written=CONFIDENCE_HIGH_WORDS,
                sentences=400,
                pieces=2,
            )
        )
        self.assertEqual(few_pieces["level"], "low")

        medium = corpus_confidence(_confidence_stats(words=2000, written=1800, sentences=120, pieces=6))
        self.assertEqual(medium["level"], "medium")
        self.assertTrue(medium["trustworthy"])

        high = corpus_confidence(
            _confidence_stats(
                words=CONFIDENCE_HIGH_WORDS + 1,
                written=CONFIDENCE_HIGH_WORDS,
                sentences=300,
                pieces=12,
            )
        )
        self.assertEqual(high["level"], "high")
        self.assertEqual(high["note"], "")

        spoken_only = corpus_confidence(
            _confidence_stats(words=6000, written=0, sentences=300, pieces=12)
        )
        self.assertEqual(spoken_only["level"], "low")
        self.assertIn("No written prose", spoken_only["note"])

    def test_section_confidence_uses_the_section_sample(self) -> None:
        stats = _confidence_stats(words=6000, written=6000, sentences=300, pieces=12)
        stats["sections"] = {
            "notes": {
                "pieces": 2,
                "unweighted": {
                    "metrics": {"rhythmWords": 120, "dictionWords": 120, "rhythmSentences": 9}
                },
            }
        }
        self.assertEqual(corpus_confidence(stats)["level"], "high")
        section = corpus_confidence(stats, "notes")
        self.assertEqual(section["level"], "low")
        self.assertEqual(section["pieces"], 2)
        self.assertEqual(section["words"], 120)

    def test_judge_report_and_text_carry_confidence(self) -> None:
        stats = _confidence_stats(words=8, written=8, sentences=3, pieces=1)
        stats["weighted"] = {
            "metrics": {"medianSentenceLength": 4, "rhythmWords": 8, "dictionWords": 8},
            "banList": [],
        }
        report = judge_text("I keep the note short. I don't wait.", stats)
        self.assertTrue(report["lowConfidence"])
        self.assertEqual(report["confidence"]["level"], "low")
        rendered = format_report(report)
        self.assertIn("corpus: 1 pieces, 8 measured words", rendered)
        self.assertIn("confidence: low", rendered)

        empty = judge_text("", stats)
        self.assertIn("confidence", empty)
        self.assertEqual(empty["error"], "Draft is empty.")

    def test_guide_reports_sample_size(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            build_voice(project)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
        self.assertIn("## Sample size", guide)
        self.assertIn("Pieces:", guide)
        self.assertIn("Words:", guide)
        self.assertIn("Confidence: low.", guide)


class SplitTests(unittest.TestCase):
    def test_split_is_deterministic_and_disjoint(self) -> None:
        items = list(range(10))
        train, held = split_items(items, None, 7)
        train_again, held_again = split_items(items, None, 7)
        self.assertEqual((train, held), (train_again, held_again))
        self.assertEqual(len(held), 3)  # about a third
        self.assertEqual(sorted(train + held), items)
        self.assertFalse(set(train) & set(held))

    def test_split_can_hold_out_every_item(self) -> None:
        train, held = split_items([1, 2], 2, 0)
        self.assertEqual(train, [])
        self.assertEqual(sorted(held), [1, 2])


class EvalTests(unittest.TestCase):
    def test_pairs_use_the_10_edit_rate_even_when_meta_is_stale(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_pair(root, "2026-01-01", WRITER)
            meta_path = root / "gold" / "2026-01-01-notes" / "meta.json"
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            meta["editRate"] = 0.99
            meta_path.write_text(json.dumps(meta) + "\n", encoding="utf-8")
            pairs, notes = load_edit_pairs(root)
        self.assertEqual(notes, [])
        self.assertEqual(len(pairs), 1)
        self.assertEqual(pairs[0].edit_rate, diff_texts(WRITER, WRITER)["editRate"])
        self.assertEqual(pairs[0].edit_rate, 0.0)

    def test_held_out_edits_correlate_with_the_score(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_corpus(root)
            # Two accepted drafts and two heavily revised ones. Holding out
            # three of four always leaves one of each in the holdout.
            _make_pair(root, "2026-01-01", WRITER)
            _make_pair(root, "2026-01-02", WRITER)
            _make_pair(root, "2026-01-03", AI_DRAFT)
            _make_pair(root, "2026-01-04", AI_DRAFT)
            report = evaluate(root, holdout=3, seed=0)
            pieces = discover(root, load_manifest(root))

        self.assertTrue(report["ok"], report)
        self.assertEqual(report["mode"], "edits")
        self.assertEqual(report["holdoutCount"], 3)
        # The held-out pairs' finals never enter the training targets.
        self.assertEqual(report["train"]["pieces"], len(pieces) - 3)
        self.assertEqual(report["correlation"]["n"], 3)
        self.assertEqual(report["direction"], "negative")
        self.assertLess(report["correlation"]["pearson"], 0)
        self.assertLess(report["correlation"]["spearman"], 0)

        accepted = [item for item in report["items"] if item["accepted"]]
        revised = [item for item in report["items"] if not item["accepted"]]
        self.assertEqual(len(accepted), 2)
        self.assertEqual(len(revised), 1)
        self.assertTrue(all(item["editRate"] <= ACCEPT_EDIT_RATE for item in accepted))
        self.assertTrue(all(item["score"] >= 90 for item in accepted))
        self.assertTrue(all(item["score"] < 50 for item in revised))
        self.assertGreater(report["acceptance"]["scoreGap"], 0)

        rendered = format_eval(report)
        self.assertIn("Held-out evaluation: 3 of 4 edit pairs held out", rendered)
        self.assertIn("correlation:", rendered)
        self.assertIn("Higher scores came with lighter edits", rendered)

    def test_prose_mode_reports_generalization_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            variants = [
                "I keep the note short. I don't polish.",
                "I walk the long way home. So I skip the shortcut.",
                "I drink cold coffee. And I leave the page messy.",
                "I stop before the line gets tidy. I don't add a story.",
            ]
            for index, text in enumerate(variants):
                (root / "corpus" / "gold" / f"note-{index}.md").write_text(
                    text + "\n", encoding="utf-8"
                )
            report = evaluate(root, holdout=2, seed=1)

        self.assertTrue(report["ok"], report)
        self.assertEqual(report["mode"], "prose")
        self.assertEqual(report["holdoutCount"], 2)
        self.assertEqual(report["correlation"]["n"], 0)
        self.assertIsNone(report["acceptance"])
        self.assertEqual(len(report["items"]), 2)
        self.assertTrue(all(item["score"] is not None for item in report["items"]))
        self.assertTrue(any("No edit pairs captured yet" in note for note in report["notes"]))

    def test_too_little_gold_is_reported_not_raised(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            (root / "corpus" / "gold" / "only.md").write_text(WRITER + "\n", encoding="utf-8")
            report = evaluate(root)
            self.assertFalse(report["ok"])
            self.assertIn("at least two gold prose pieces", str(report["error"]))
            self.assertIn("ghostless eval:", format_eval(report))

    def test_cli_eval_prints_a_report_and_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_pair(root, "2026-01-01", WRITER)
            _make_pair(root, "2026-01-02", WRITER)
            _make_pair(root, "2026-01-03", AI_DRAFT)
            _make_pair(root, "2026-01-04", AI_DRAFT)
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = main(["eval", "--project", str(root), "--holdout", "3", "--seed", "0"])
            self.assertEqual(code, 0)
            self.assertIn("Held-out evaluation:", stdout.getvalue())

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = main(["eval", "--project", str(root), "--holdout", "3", "--json"])
            self.assertEqual(code, 0)
            payload = json.loads(stdout.getvalue())
        self.assertEqual(payload["mode"], "edits")
        self.assertEqual(payload["correlation"]["n"], 3)


if __name__ == "__main__":
    os.environ.setdefault("GHOSTLESS_NO_UPDATE", "1")
    unittest.main()
