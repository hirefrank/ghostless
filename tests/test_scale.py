"""Bound growth on generated writing projects with no checked-in corpus."""

from __future__ import annotations

import json
import tempfile
import time
import unittest
from collections.abc import Callable
from functools import partial
from pathlib import Path
from typing import TypeVar

from ghostless.build import build_voice, stale_inputs
from ghostless.learn import diff_texts
from ghostless.project import init_project

T = TypeVar("T")
TIERS = ("gold", "silver", "bronze")
PARAGRAPH = (
    "The station clock ran slow. I put the ticket in my pocket and crossed the street. "
    "The shopkeeper stacked the empty cups while I waited for the light to change.\n\n"
)


def timed(call: Callable[[], T]) -> tuple[T, float]:
    start = time.thread_time()
    value = call()
    return value, time.thread_time() - start


def populate(root: Path, count: int) -> Path:
    for index in range(count):
        tier = TIERS[index % len(TIERS)]
        text = f"---\nsection: section{index % 4}\n---\n\n# Letter {index}\n\n" + PARAGRAPH * 8
        (root / f"corpus/{tier}/letter-{index}.md").write_text(text, encoding="utf-8")
    for index in range(count // 10):
        pair = root / "gold" / f"2026-01-02-note-{index}"
        pair.mkdir()
        (pair / "before.md").write_text("Please delve into the matter.\n\n" + PARAGRAPH, encoding="utf-8")
        (pair / "after.md").write_text("Here are the details.\n\n" + PARAGRAPH, encoding="utf-8")
        (pair / "meta.json").write_text(
            json.dumps({"origin": "ai-first", "beforeFile": "before.md", "afterFile": "after.md"}),
            encoding="utf-8",
        )
    return root / "corpus/gold/letter-0.md"


class ScaleTests(unittest.TestCase):
    def assert_growth(
        self,
        operation: str,
        small: float,
        large: float,
        *,
        ceiling: float,
        slack: float,
        factor: float = 8.0,
    ) -> None:
        self.assertLess(large, ceiling, f"{operation} large run took {large:.3f}s")
        self.assertLess(
            large,
            small * factor + slack,
            f"{operation} grew from {small:.3f}s to {large:.3f}s at four times the input",
        )

    def test_build_and_staleness_at_writing_project_scale(self) -> None:
        samples: list[tuple[float, float]] = []
        with tempfile.TemporaryDirectory() as tmp:
            for count in (50, 200):
                root = init_project(Path(tmp) / str(count), git=False)
                to_delete = populate(root, count)
                stats, build_time = timed(partial(build_voice, root))
                self.assertEqual(stats["counts"]["total"], count + count // 10)
                self.assertGreater(sum(piece["words"] for piece in stats["files"]), count * 240)
                self.assertEqual(stale_inputs(root), [])
                to_delete.unlink()
                stale_time = 0.0
                for _ in range(5):
                    stale, elapsed = timed(partial(stale_inputs, root))
                    self.assertEqual(stale, ["corpus/gold/letter-0.md"])
                    stale_time += elapsed
                samples.append((build_time, stale_time))
        (build_small, stale_small), (build_large, stale_large) = samples
        self.assert_growth("build_voice", build_small, build_large, ceiling=12.0, slack=0.25)
        self.assert_growth("stale_inputs", stale_small, stale_large, ceiling=3.0, slack=0.10)

    def test_long_draft_diff_with_repeated_phrasing(self) -> None:
        samples: list[float] = []
        for count in (200, 800):
            # Distinct line numbers and recurring prose mimic a draft with
            # reusable phrases and anchor words, while still testing moves.
            lines = [f"Line {i * 8}: " + " ".join(str(i * 8 + j) for j in range(1, 7)) + " ends" for i in range(count)]
            before = "\n".join(lines)
            after = "\n".join([*lines[:5], *lines[7:], *lines[5:7]]) + " Extra."
            runs = [timed(partial(diff_texts, before, after)) for _ in range(2)]
            for diff, _ in runs:
                self.assertEqual(diff["movedTokens"], 17)
                self.assertEqual(diff["insertions"], 2)
            samples.append(min(elapsed for _, elapsed in runs))
        self.assert_growth("diff_texts", *samples, ceiling=4.0, slack=0.25, factor=12.0)
