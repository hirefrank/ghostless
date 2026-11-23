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
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"

sys.path.insert(0, str(ROOT / "lib"))

from ghostless.cli import build_voice, main  # noqa: E402
from ghostless.eval import (  # noqa: E402
    ACCEPT_EDIT_RATE,
    _holdout_block,
    evaluate,
    format_eval,
    load_edit_pairs,
    load_holdouts,
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
from ghostless.paths import ProjectError  # noqa: E402
from ghostless.project import discover, init_project, load_manifest  # noqa: E402
from ghostless.stats import Piece, compile_stats, summarize  # noqa: E402
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

    def test_four_backtick_fence_wrapping_triple_backticks_is_stripped(self) -> None:
        text = "````markdown\n```python\ndelve()\n```\n````\nReal prose."
        self.assertEqual(strip_non_prose(text).strip(), "Real prose.")
        self.assertEqual(words(text), ["Real", "prose"])

    def test_non_prose_tells_do_not_remove_words_from_ban_list(self) -> None:
        prose_and_scaffolding = Piece(
            path="corpus/gold/mixed.md",
            tier="gold",
            kind="prose",
            section=None,
            weight=3.0,
            text=(
                "---\n"
                "notes: robust\n"
                "---\n"
                "```python\n"
                "leverage = robust()\n"
                "```\n"
                "Use `seamless` in code, check https://example.com/delve, "
                "<!-- tapestry -->\n"
                "Short real prose stays plain.\n"
            ),
        )
        summary = summarize([prose_and_scaffolding], bronze_format_only=False, use_weights=True)
        self.assertFalse(summary["dictionEmpty"])
        for tell in ("leverage", "robust", "seamless", "delve", "tapestry"):
            self.assertIn(tell, summary["banList"])
            self.assertNotIn(tell, summary["tellsInUse"])

        code_only = Piece(
            path="corpus/gold/code-only.md",
            tier="gold",
            kind="prose",
            section=None,
            weight=3.0,
            text="```python\nleverage = robust()\n```\n",
        )
        empty_summary = summarize([code_only], bronze_format_only=False, use_weights=True)
        self.assertTrue(empty_summary["dictionEmpty"])
        self.assertEqual(empty_summary["banList"], [])


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

    def test_holdout_block_pass_rate_uses_judged_denominator(self) -> None:
        mixed = _holdout_block(
            [
                {"score": 90, "ok": True},
                {"score": None, "ok": False, "error": "Draft is empty."},
            ]
        )
        self.assertEqual(mixed["pieces"], 2)
        self.assertEqual(mixed["judged"], 1)
        self.assertEqual(mixed["meanScore"], 90.0)
        self.assertEqual(mixed["passRate"], 1.0)

        with_failure = _holdout_block(
            [
                {"score": 90, "ok": True},
                {"score": 40, "ok": False},
                {"score": None, "ok": False, "error": "No usable voice data yet."},
                {"score": True, "ok": True},
            ]
        )
        self.assertEqual(with_failure["pieces"], 4)
        self.assertEqual(with_failure["judged"], 2)
        self.assertEqual(with_failure["passRate"], 0.5)

        unscored_only = _holdout_block(
            [
                {"score": None, "ok": False, "error": "Draft is empty."},
                {"score": None, "ok": True},
            ]
        )
        self.assertEqual(unscored_only["pieces"], 2)
        self.assertEqual(unscored_only["judged"], 0)
        self.assertIsNone(unscored_only["meanScore"])
        self.assertIsNone(unscored_only["passRate"])

        empty = _holdout_block([])
        self.assertEqual(empty["pieces"], 0)
        self.assertEqual(empty["judged"], 0)
        self.assertIsNone(empty["passRate"])

    def test_load_holdouts_reads_prose_and_skips_ignored_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            holdouts = root / "holdouts"
            (holdouts / "README.md").write_text("Ignored readme.\n", encoding="utf-8")
            (holdouts / ".hidden.md").write_text("Ignored dotfile.\n", encoding="utf-8")
            (holdouts / ".hidden-dir").mkdir()
            (holdouts / ".hidden-dir" / "secret.md").write_text("Ignored.\n", encoding="utf-8")
            (holdouts / "image.png").write_text("not prose\n", encoding="utf-8")
            (holdouts / "empty.md").write_text("   \n", encoding="utf-8")
            (holdouts / "only-frontmatter.md").write_text("---\nsection: notes\n---\n\n", encoding="utf-8")
            (holdouts / "plain.txt").write_text("Top level holdout prose.\n", encoding="utf-8")
            (holdouts / "notes").mkdir()
            (holdouts / "notes" / "first.md").write_text("Subfolder holdout.\n", encoding="utf-8")
            (holdouts / "notes" / "override.markdown").write_text(
                "---\nsection: essays\n---\nFrontmatter section wins.\n",
                encoding="utf-8",
            )
            manifest = load_manifest(root)
            pieces, notes = load_holdouts(root, manifest)

        self.assertEqual(notes, [])

        self.assertEqual(
            [piece.path for piece in pieces],
            [
                "holdouts/notes/first.md",
                "holdouts/notes/override.markdown",
                "holdouts/plain.txt",
            ],
        )
        self.assertEqual([piece.section for piece in pieces], ["notes", "essays", None])
        for piece in pieces:
            self.assertEqual(piece.tier, "gold")
            self.assertEqual(piece.kind, "prose")
            self.assertEqual(piece.weight, float(manifest["weights"]["gold"]))
            self.assertEqual(piece.origin, "unaided")
            self.assertEqual(piece.confirmation, "origin")

    def test_unreadable_holdout_is_skipped_and_eval_continues(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            (root / "corpus" / "gold" / "one.md").write_text(WRITER + "\n", encoding="utf-8")
            holdouts = root / "holdouts"
            (holdouts / "good.md").write_text("I keep the note short.\n", encoding="utf-8")
            (holdouts / "bad.md").write_bytes(b"\xff\xfe not utf-8")

            pieces, notes = load_holdouts(root, load_manifest(root))
            self.assertEqual([piece.path for piece in pieces], ["holdouts/good.md"])
            self.assertEqual(notes, ["Skipped holdouts/bad.md: not valid UTF-8."])

            report = evaluate(root)
        self.assertTrue(report["ok"], report)
        self.assertIn("Skipped holdouts/bad.md: not valid UTF-8.", report["notes"])
        self.assertEqual(report["holdouts"]["pieces"], 1)

    def test_cv_without_edit_pairs_is_an_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            (root / "corpus" / "gold" / "one.md").write_text(WRITER + "\n", encoding="utf-8")
            (root / "corpus" / "gold" / "two.md").write_text(WRITER + "\n\nAnd another.\n", encoding="utf-8")

            with self.assertRaisesRegex(ProjectError, "--cv needs learned edit pairs"):
                evaluate(root, cv=True)

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(["eval", "--project", str(root), "--cv"])
            self.assertEqual(code, 2)
            self.assertIn("--cv needs learned edit pairs", stderr.getvalue())

    def test_holdouts_scored_standalone_without_leaking_into_build(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            (root / "corpus" / "gold" / "only.md").write_text(WRITER + "\n", encoding="utf-8")
            (root / "holdouts" / "a-writer.md").write_text(WRITER + "\n", encoding="utf-8")
            (root / "holdouts" / "b-tell.md").write_text(
                "I keep the delve short. I don't polish the first line.\n",
                encoding="utf-8",
            )
            build_voice(root)
            stats = json.loads((root / "voice-stats.json").read_text(encoding="utf-8"))
            report = evaluate(root)

        self.assertEqual(stats["counts"]["total"], 1)
        self.assertEqual([entry["path"] for entry in stats["files"]], ["corpus/gold/only.md"])
        # If holdouts/b-tell.md had leaked into build, "delve" would drop off banList.
        self.assertIn("delve", stats["weighted"]["banList"])

        self.assertTrue(report["ok"], report)
        self.assertEqual(report["mode"], "holdouts")
        self.assertFalse(report["cv"])
        self.assertEqual(report["totalCount"], 2)
        self.assertEqual(report["holdoutCount"], 2)
        self.assertEqual(report["train"]["pieces"], 1)
        self.assertEqual(report["holdout"]["judged"], 2)
        self.assertEqual(report["holdouts"]["pieces"], 2)
        self.assertEqual(report["correlation"], {"pearson": None, "spearman": None, "n": 0})
        self.assertIsNone(report["acceptance"])
        self.assertEqual(report["confirmedChecks"]["total"], 0)
        rendered = format_eval(report)
        self.assertIn("Holdouts evaluation: 2 piece(s) from holdouts/", rendered)
        self.assertIn("holdouts/a-writer.md", rendered)
        self.assertIn("holdouts/b-tell.md", rendered)

    def test_holdouts_evaluated_alongside_edits_and_prose_modes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_corpus(root)
            (root / "holdouts" / "kept.md").write_text(WRITER + "\n", encoding="utf-8")

            prose_report = evaluate(root, holdout=1, seed=0)
            self.assertTrue(prose_report["ok"], prose_report)
            self.assertEqual(prose_report["mode"], "prose")
            self.assertIsInstance(prose_report["holdouts"], dict)
            self.assertEqual(prose_report["holdouts"]["pieces"], 1)
            self.assertEqual(prose_report["holdouts"]["summary"]["judged"], 1)
            self.assertIn("holdouts/: 1 judged", format_eval(prose_report))

            _make_pair(root, "2026-01-01", WRITER)
            _make_pair(root, "2026-01-02", WRITER)
            _make_pair(root, "2026-01-03", AI_DRAFT)
            _make_pair(root, "2026-01-04", AI_DRAFT)

            edits_report = evaluate(root, holdout=3, seed=0)
            self.assertTrue(edits_report["ok"], edits_report)
            self.assertEqual(edits_report["mode"], "edits")
            self.assertFalse(edits_report["cv"])
            self.assertEqual(edits_report["holdouts"]["pieces"], 1)
            self.assertEqual(edits_report["holdouts"]["summary"]["passRate"], 1.0)
            rendered = format_eval(edits_report)
            self.assertIn("holdouts/: 1 judged, mean score", rendered)
            self.assertIn("pass rate 100%", rendered)

    def test_loocv_automatic_for_small_edit_datasets_and_zero_leakage(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_corpus(root)
            _make_pair(root, "2026-01-01", WRITER)
            _make_pair(root, "2026-01-02", WRITER)
            _make_pair(root, "2026-01-03", AI_DRAFT)

            # Confirm a check so we also verify #37 confirmedChecks stay populated under CV.
            meta_path = root / "gold" / "2026-01-03-notes" / "meta.json"
            (root / "gold" / "2026-01-03-notes" / "before.md").write_text(
                "Furthermore, we must delve into a seamless tapestry.\n",
                encoding="utf-8",
            )
            with contextlib.redirect_stdout(io.StringIO()):
                main(
                    [
                        "checks",
                        "add",
                        "2026-01-03-notes",
                        "--text",
                        'Do not use "seamless tapestry".',
                        "--project",
                        str(root),
                    ]
                )
                main(
                    [
                        "checks",
                        "confirm",
                        'Do not use "seamless tapestry".',
                        "--project",
                        str(root),
                    ]
                )

            pairs, _ = load_edit_pairs(root)
            all_pieces = discover(root, load_manifest(root))
            seen_train_paths: list[set[str]] = []

            def spy_compile(
                train_pieces: list[Piece],
                *,
                weights: dict[str, float],
                gold_voice_minimum: int,
                edits: dict[str, object] | None = None,
            ) -> dict[str, object]:
                seen_train_paths.append({piece.path for piece in train_pieces})
                return compile_stats(
                    train_pieces,
                    weights=weights,
                    gold_voice_minimum=gold_voice_minimum,
                    edits=edits,
                )

            with mock.patch("ghostless.eval.compile_stats", side_effect=spy_compile):
                report = evaluate(root)

        self.assertTrue(report["ok"], report)
        self.assertEqual(report["mode"], "edits")
        self.assertTrue(report["cv"])
        self.assertEqual(report["totalCount"], 3)
        self.assertEqual(report["holdoutCount"], 3)
        self.assertEqual(report["train"]["pieces"], len(all_pieces) - 1)
        self.assertEqual(report["correlation"]["n"], 3)
        self.assertEqual(report["direction"], "negative")
        self.assertLess(report["correlation"]["pearson"], 0)
        self.assertLess(report["correlation"]["spearman"], 0)
        self.assertGreater(report["acceptance"]["scoreGap"], 0)
        self.assertEqual(len(seen_train_paths), 3)
        for pair, fold_paths in zip(pairs, seen_train_paths):
            self.assertNotIn(pair.after_path, fold_paths)
            other_afters = {other.after_path for other in pairs if other.slug != pair.slug}
            self.assertTrue(other_afters.issubset(fold_paths))

        # Confirmed checks were graded across all 3 pairs' befores plus the check's after.
        self.assertEqual(report["confirmedChecks"]["total"], 4)
        self.assertEqual(report["confirmedChecks"]["passes"], 3)
        self.assertIn("Leave-one-out evaluation: 3 edit pairs (out-of-sample)", format_eval(report))

    def test_loocv_excludes_pair_after_tell_from_own_fold_and_supports_explicit_cv(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            # Three writer-first pairs, no extra corpus/. Pair 1 uses "delve" in both
            # before and after; pairs 2 and 3 do not. If fold 1 leaked pair 1's after.md
            # into targets, "delve" would leave the banList and fold 1 would not penalize it.
            pair1_before = (
                "I keep the delve short. I don't polish the first line.\n\n"
                "And I stop when the coffee is cold. So the page stays messy."
            )
            pair1_after = (
                "I keep the delve short. I don't polish the first line.\n\n"
                "And I stop when the coffee is cold. So the page stays tidy."
            )
            for slug, before, after in (
                ("2026-01-01-notes", pair1_before, pair1_after),
                ("2026-01-02-notes", WRITER, WRITER),
            ):
                folder = root / "gold" / slug
                folder.mkdir(parents=True)
                (folder / "before.md").write_text(before + "\n", encoding="utf-8")
                (folder / "after.md").write_text(after + "\n", encoding="utf-8")
                (folder / "meta.json").write_text(
                    json.dumps(
                        {
                            "date": slug[:10],
                            "section": "notes",
                            "origin": "writer-first",
                            "kind": "prose",
                            "beforeFile": "before.md",
                            "afterFile": "after.md",
                        }
                    )
                    + "\n",
                    encoding="utf-8",
                )

            # With only 2 pairs, auto-CV is off unless cv=True is requested.
            two_pair_default = evaluate(root)
            self.assertFalse(two_pair_default["cv"])
            self.assertEqual(two_pair_default["holdoutCount"], 1)

            two_pair_cv = evaluate(root, cv=True)
            self.assertTrue(two_pair_cv["ok"], two_pair_cv)
            self.assertTrue(two_pair_cv["cv"])
            self.assertEqual(two_pair_cv["holdoutCount"], 2)
            by_slug = {item["slug"]: item for item in two_pair_cv["items"]}
            self.assertLessEqual(by_slug["2026-01-01-notes"]["score"], 90)
            self.assertEqual(by_slug["2026-01-02-notes"]["score"], 100)

    def test_cli_eval_cv_and_rejects_cv_with_holdout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_corpus(root)
            _make_pair(root, "2026-01-01", WRITER)
            _make_pair(root, "2026-01-02", AI_DRAFT)

            with self.assertRaisesRegex(ProjectError, "either --cv or --holdout"):
                evaluate(root, cv=True, holdout=1)

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(["eval", "--project", str(root), "--cv", "--holdout", "1"])
            self.assertEqual(code, 2)
            self.assertIn("either --cv or --holdout", stderr.getvalue())

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = main(["eval", "--project", str(root), "--cv"])
            self.assertEqual(code, 0)
            self.assertIn("Leave-one-out evaluation: 2 edit pairs (out-of-sample)", stdout.getvalue())

            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = main(["eval", "--project", str(root), "--cv", "--json"])
            self.assertEqual(code, 0)
            payload = json.loads(stdout.getvalue())
            self.assertTrue(payload["cv"])
            self.assertEqual(payload["holdoutCount"], 2)

    def test_loocv_reports_failure_when_all_folds_have_no_training_corpus(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "voice"
            init_project(root, git=False)
            _make_pair(root, "2026-01-01", WRITER)
            report = evaluate(root, cv=True)
        self.assertFalse(report["ok"])
        self.assertTrue(report["cv"])
        self.assertIn("no corpus to build targets from", str(report["error"]))



if __name__ == "__main__":
    os.environ.setdefault("GHOSTLESS_NO_UPDATE", "1")
    unittest.main()
