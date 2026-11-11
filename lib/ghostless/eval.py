"""Held-out evaluation: does the judge score track the writer's edits?

A score is only useful if it predicts acceptance. This module splits the
learned edit pairs into train and holdout, builds voice targets from the train
pieces only, judges each held-out pair's `before` draft, and compares that
score with the writer's own edit rate for the pair. The edit rate is the one
defined by `ghostless.learn.diff_texts` (#10): edited aligned token positions
divided by all aligned positions, bounded 0..1. A writer who accepted a draft
with light edits has a low rate, so a score that predicts acceptance should
correlate *negatively* with it.

With no learned pairs there is no acceptance signal to measure. In that case
the module still holds out gold prose pieces, builds on the rest, and judges
them, which checks that the targets generalize to unseen writing by the same
author. That is reported honestly as a weaker check.
"""

from __future__ import annotations

import json
import math
import random
from dataclasses import dataclass
from pathlib import Path

from ghostless.judge import corpus_confidence, judge_text
from ghostless.learn import diff_texts
from ghostless.paths import ProjectError
from ghostless.project import discover, load_manifest
from ghostless.stats import Piece, compile_stats
from ghostless.text import words

# A pair at or below this edit rate counts as accepted. The rate is bounded
# 0..1; 0.15 means the writer changed about one position in seven.
ACCEPT_EDIT_RATE = 0.15
DEFAULT_HOLDOUT_SHARE = 0.3
MIN_CORRELATION_POINTS = 3
MAX_ITEMS_IN_REPORT = 20


@dataclass(frozen=True)
class EditPair:
    slug: str
    section: str | None
    origin: str | None
    before: str
    after: str
    after_path: str
    edit_rate: float


def load_edit_pairs(root: Path) -> tuple[list[EditPair], list[str]]:
    """Read `gold/<slug>/` pairs and recompute the #10 edit rate from the files.

    Recomputing rather than trusting `meta.json` keeps old pairs on the current
    definition. A broken pair is skipped with a note instead of aborting the
    evaluation; `build` already refuses to run on one.
    """
    pairs: list[EditPair] = []
    notes: list[str] = []
    folder = root / "gold"
    if not folder.is_dir():
        return pairs, notes
    for meta_path in sorted(folder.glob("*/meta.json")):
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            notes.append(f"Skipped {meta_path.parent.name}: unreadable meta.json.")
            continue
        if not isinstance(meta, dict):
            notes.append(f"Skipped {meta_path.parent.name}: meta.json is not an object.")
            continue
        before_name = meta.get("beforeFile") if isinstance(meta.get("beforeFile"), str) else "before.md"
        after_name = meta.get("afterFile") if isinstance(meta.get("afterFile"), str) else "after.md"
        before_path = meta_path.parent / before_name
        after_path = meta_path.parent / after_name
        if not before_path.is_file() or not after_path.is_file():
            notes.append(f"Skipped {meta_path.parent.name}: before/after file is missing.")
            continue
        try:
            before = before_path.read_text(encoding="utf-8")
            after = after_path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            notes.append(f"Skipped {meta_path.parent.name}: pair is not readable UTF-8.")
            continue
        if not before.strip() or not after.strip():
            notes.append(f"Skipped {meta_path.parent.name}: empty before or after.")
            continue
        section = meta.get("section")
        origin = meta.get("origin")
        relative_after = after_path.resolve().relative_to(root.resolve()).as_posix()
        pairs.append(
            EditPair(
                slug=meta_path.parent.name,
                section=section if isinstance(section, str) and section else None,
                origin=origin if isinstance(origin, str) and origin else None,
                before=before,
                after=after,
                after_path=relative_after,
                edit_rate=float(diff_texts(before, after)["editRate"]),
            )
        )
    return pairs, notes


def split_items(items: list[object], holdout: int | None, seed: int) -> tuple[list[object], list[object]]:
    """Seeded, deterministic train/holdout split.

    At least one item is held out. The split can hold everything out when the
    corpus itself still supplies training pieces (for example, a single edit
    pair with a larger corpus around it).
    """
    if not items:
        return [], []
    count = holdout if holdout is not None else max(1, round(len(items) * DEFAULT_HOLDOUT_SHARE))
    count = max(1, min(int(count), len(items)))
    rng = random.Random(seed)
    picked = set(rng.sample(range(len(items)), count))
    train = [item for index, item in enumerate(items) if index not in picked]
    held = [item for index, item in enumerate(items) if index in picked]
    return train, held


def evaluate(root: Path, *, holdout: int | None = None, seed: int = 0) -> dict[str, object]:
    """Run the held-out evaluation for one voice project."""
    manifest = load_manifest(root)
    pieces = discover(root, manifest)
    weights = manifest["weights"]
    if not isinstance(weights, dict):
        raise ProjectError("manifest weights are missing.")
    pairs, notes = load_edit_pairs(root)
    if pairs:
        return _evaluate_edits(
            pieces=pieces,
            pairs=pairs,
            weights={str(key): float(value) for key, value in weights.items()},
            minimum=int(manifest["goldVoiceMinimum"]),
            fail_under=int(manifest["failUnder"]),
            overrides=manifest.get("overrides") if isinstance(manifest.get("overrides"), dict) else None,
            holdout=holdout,
            seed=seed,
            notes=notes,
        )
    return _evaluate_prose(
        pieces=pieces,
        weights={str(key): float(value) for key, value in weights.items()},
        minimum=int(manifest["goldVoiceMinimum"]),
        fail_under=int(manifest["failUnder"]),
        overrides=manifest.get("overrides") if isinstance(manifest.get("overrides"), dict) else None,
        holdout=holdout,
        seed=seed,
        notes=notes,
    )


def _evaluate_edits(
    *,
    pieces: list[Piece],
    pairs: list[EditPair],
    weights: dict[str, float],
    minimum: int,
    fail_under: int,
    overrides: dict[str, object] | None,
    holdout: int | None,
    seed: int,
    notes: list[str],
) -> dict[str, object]:
    _, held = split_items(list(pairs), holdout, seed)
    held = [pair for pair in held if isinstance(pair, EditPair)]
    held_paths = {pair.after_path for pair in held}
    train_pieces = [piece for piece in pieces if piece.path not in held_paths]
    if not train_pieces:
        return _failure(
            "edits",
            "Holding out every edit pair left no corpus to build targets from. "
            "Add writing to corpus/ or hold out fewer pairs.",
            notes,
            seed,
        )
    stats = compile_stats(train_pieces, weights=weights, gold_voice_minimum=minimum)
    confidence = corpus_confidence(stats)
    if confidence["level"] == "low":
        notes.append(
            "The train sample is small, so treat the correlation as a direction, not a verdict."
        )

    items: list[dict[str, object]] = []
    for pair in held:
        section = pair.section if _section_exists(stats, pair.section) else None
        report = judge_text(pair.before, stats, section=section, fail_under=fail_under,
                            overrides=overrides)
        score = _score(report)
        item: dict[str, object] = {
            "kind": "edit",
            "slug": pair.slug,
            "section": pair.section,
            "origin": pair.origin,
            "words": len(words(pair.before)),
            "score": score,
            "ok": bool(report.get("ok")),
            "editRate": round(pair.edit_rate, 4),
            "accepted": pair.edit_rate <= ACCEPT_EDIT_RATE,
        }
        error = report.get("error")
        if isinstance(error, str) and error:
            item["error"] = error
        items.append(item)

    scored = [item for item in items if isinstance(item.get("score"), int)]
    n = len(scored)
    pairs_for_stats = [
        (float(item["score"]), float(item["editRate"]))
        for item in scored
        if isinstance(item.get("editRate"), (int, float))
    ]
    pearson = _pearson(pairs_for_stats)
    spearman = _spearman(pairs_for_stats)
    direction = _direction(pearson, spearman)
    if n < len(items):
        notes.append(f"{len(items) - n} held-out pair(s) could not be scored.")
    if n < MIN_CORRELATION_POINTS:
        notes.append(
            f"Only {n} held-out pair(s) produced a score. Three or more are needed "
            "to say anything about correlation."
        )

    accepted = [item for item in scored if item.get("accepted")]
    revised = [item for item in scored if not item.get("accepted")]
    acceptance = {
        "threshold": ACCEPT_EDIT_RATE,
        "accepted": {"count": len(accepted), "meanScore": _mean(_scores(accepted))},
        "revised": {"count": len(revised), "meanScore": _mean(_scores(revised))},
        "scoreGap": _gap(_mean(_scores(accepted)), _mean(_scores(revised))),
    }

    return {
        "ok": True,
        "mode": "edits",
        "seed": seed,
        "totalCount": len(pairs),
        "holdoutCount": len(held),
        "train": _train_block(train_pieces, confidence),
        "holdout": _holdout_block(items),
        "correlation": {"pearson": pearson, "spearman": spearman, "n": n},
        "acceptance": acceptance,
        "direction": direction,
        "verdict": _verdict(n, direction, spearman if spearman is not None else pearson),
        "items": items,
        "notes": notes,
    }


def _evaluate_prose(
    *,
    pieces: list[Piece],
    weights: dict[str, float],
    minimum: int,
    fail_under: int,
    overrides: dict[str, object] | None,
    holdout: int | None,
    seed: int,
    notes: list[str],
) -> dict[str, object]:
    gold = [piece for piece in pieces if piece.tier == "gold" and piece.kind == "prose"]
    if len(gold) < 2:
        return _failure(
            "prose",
            "Need at least two gold prose pieces to hold one out. "
            "Add writing to corpus/gold or learn an edit pair.",
            notes,
            seed,
        )
    _, held = split_items(list(gold), holdout, seed)
    held = [piece for piece in held if isinstance(piece, Piece)]
    held_paths = {piece.path for piece in held}
    train_pieces = [piece for piece in pieces if piece.path not in held_paths]
    if not train_pieces:
        return _failure(
            "prose",
            "Holding out every gold piece left no corpus to build targets from. "
            "Hold out fewer pieces.",
            notes,
            seed,
        )
    stats = compile_stats(train_pieces, weights=weights, gold_voice_minimum=minimum)
    confidence = corpus_confidence(stats)

    items: list[dict[str, object]] = []
    for piece in held:
        section = piece.section if _section_exists(stats, piece.section) else None
        report = judge_text(piece.text, stats, section=section, fail_under=fail_under,
                            overrides=overrides)
        score = _score(report)
        item: dict[str, object] = {
            "kind": "piece",
            "path": piece.path,
            "section": piece.section,
            "words": len(words(piece.text)),
            "score": score,
            "ok": bool(report.get("ok")),
        }
        error = report.get("error")
        if isinstance(error, str) and error:
            item["error"] = error
        items.append(item)

    notes.append(
        "No edit pairs captured yet. This only checks that the score generalizes "
        "to unseen gold; run `ghostless learn` after an editing session to test acceptance."
    )
    unscored = len(items) - len([item for item in items if item.get("score") is not None])
    if unscored:
        notes.append(f"{unscored} held-out piece(s) could not be scored.")
    return {
        "ok": True,
        "mode": "prose",
        "seed": seed,
        "totalCount": len(gold),
        "holdoutCount": len(held),
        "train": _train_block(train_pieces, confidence),
        "holdout": _holdout_block(items),
        "correlation": {"pearson": None, "spearman": None, "n": 0},
        "acceptance": None,
        "direction": "unknown",
        "verdict": (
            "No edit pairs yet, so acceptance cannot be measured. "
            "The held-out score only shows whether the targets travel."
        ),
        "items": items,
        "notes": notes,
    }


def _failure(mode: str, message: str, notes: list[str], seed: int) -> dict[str, object]:
    return {
        "ok": False,
        "mode": mode,
        "error": message,
        "seed": seed,
        "totalCount": 0,
        "holdoutCount": 0,
        "train": None,
        "holdout": None,
        "correlation": None,
        "acceptance": None,
        "direction": "unknown",
        "verdict": "",
        "items": [],
        "notes": notes,
    }


def _section_exists(stats: dict[str, object], section: str | None) -> bool:
    if not section:
        return False
    sections = stats.get("sections")
    return isinstance(sections, dict) and section in sections


def _score(report: dict[str, object]) -> int | None:
    score = report.get("score")
    if isinstance(score, bool) or not isinstance(score, int):
        return None
    if report.get("error"):
        return None
    return score


def _train_block(pieces: list[Piece], confidence: dict[str, object]) -> dict[str, object]:
    return {
        "pieces": len(pieces),
        "words": confidence.get("words"),
        "writtenWords": confidence.get("writtenWords"),
        "sentences": confidence.get("sentences"),
        "confidence": confidence.get("level"),
    }


def _holdout_block(items: list[dict[str, object]]) -> dict[str, object]:
    scores = _scores(items)
    rates = [
        float(item["editRate"])
        for item in items
        if isinstance(item.get("editRate"), (int, float))
    ]
    return {
        "pieces": len(items),
        "judged": len(scores),
        "meanScore": _mean(scores),
        "minScore": min(scores) if scores else None,
        "maxScore": max(scores) if scores else None,
        "meanEditRate": _mean(rates),
        "passRate": round(
            sum(1 for item in items if item.get("ok")) / len(items), 4
        )
        if items
        else None,
    }


def _scores(items: list[dict[str, object]]) -> list[float]:
    values: list[float] = []
    for item in items:
        score = item.get("score")
        if isinstance(score, int) and not isinstance(score, bool):
            values.append(float(score))
    return values


def _mean(values: list[float]) -> float | None:
    if not values:
        return None
    return round(sum(values) / len(values), 4)


def _gap(accepted: float | None, revised: float | None) -> float | None:
    if accepted is None or revised is None:
        return None
    return round(accepted - revised, 4)


def _pearson(pairs: list[tuple[float, float]]) -> float | None:
    if len(pairs) < MIN_CORRELATION_POINTS:
        return None
    xs = [pair[0] for pair in pairs]
    ys = [pair[1] for pair in pairs]
    mean_x = sum(xs) / len(xs)
    mean_y = sum(ys) / len(ys)
    numerator = sum((x - mean_x) * (y - mean_y) for x, y in pairs)
    sum_x = sum((x - mean_x) ** 2 for x in xs)
    sum_y = sum((y - mean_y) ** 2 for y in ys)
    if sum_x <= 0 or sum_y <= 0:
        return None
    return round(numerator / math.sqrt(sum_x * sum_y), 4)


def _spearman(pairs: list[tuple[float, float]]) -> float | None:
    if len(pairs) < MIN_CORRELATION_POINTS:
        return None
    ranked = list(zip(_ranks([pair[0] for pair in pairs]), _ranks([pair[1] for pair in pairs])))
    return _pearson(ranked)


def _ranks(values: list[float]) -> list[float]:
    order = sorted(range(len(values)), key=lambda index: values[index])
    ranks = [0.0] * len(values)
    index = 0
    while index < len(order):
        last = index
        while last + 1 < len(order) and values[order[last + 1]] == values[order[index]]:
            last += 1
        rank = (index + last) / 2.0 + 1.0
        for position in range(index, last + 1):
            ranks[order[position]] = rank
        index = last + 1
    return ranks


def _direction(pearson: float | None, spearman: float | None) -> str:
    value = spearman if spearman is not None else pearson
    if value is None:
        return "unknown"
    if abs(value) < 0.3:
        return "none"
    return "negative" if value < 0 else "positive"


def _verdict(n: int, direction: str, value: float | None) -> str:
    if n < MIN_CORRELATION_POINTS:
        return f"Only {n} held-out pair(s) produced a score; too few to measure a correlation."
    if value is None:
        return "Scores or edit rates did not vary across the held-out pairs."
    if direction == "negative":
        return f"Higher scores came with lighter edits (r={value:.2f}, n={n})."
    if direction == "positive":
        return (
            f"Higher scores came with heavier edits, so the score points the wrong way "
            f"(r={value:.2f}, n={n})."
        )
    if direction == "none":
        return f"Scores and edit rates moved independently (r={value:.2f}, n={n})."
    return "There is not enough variation to judge the score."


def format_eval(report: dict[str, object]) -> str:
    """Human-readable evaluation report."""
    if not report.get("ok"):
        lines = [f"ghostless eval: {report.get('error') or 'evaluation could not run'}"]
        lines.extend(f"note: {note}" for note in _notes(report))
        return "\n".join(lines).rstrip() + "\n"
    mode = report.get("mode")
    peer = "edit pairs" if mode == "edits" else "gold pieces"
    lines = [
        f"Held-out evaluation: {report.get('holdoutCount')} of {report.get('totalCount')} "
        f"{peer} held out (seed {report.get('seed')})"
    ]
    train = report.get("train")
    if isinstance(train, dict):
        lines.append(
            f"train: {train.get('pieces')} pieces, {_number(train.get('words'))} words, "
            f"{_number(train.get('sentences'))} sentences, "
            f"{train.get('confidence')} confidence"
        )
    holdout = report.get("holdout")
    if isinstance(holdout, dict):
        line = (
            f"held out: {holdout.get('judged')} judged, "
            f"mean score {_number(holdout.get('meanScore'))}"
        )
        if mode == "edits":
            line += f", mean edit rate {_number(holdout.get('meanEditRate'))}"
        else:
            pass_rate = holdout.get("passRate")
            if isinstance(pass_rate, (int, float)) and not isinstance(pass_rate, bool):
                line += f", pass rate {pass_rate * 100:.0f}%"
        lines.append(line)
    correlation = report.get("correlation")
    if isinstance(correlation, dict) and mode == "edits":
        lines.append(
            "correlation: "
            f"pearson {_number(correlation.get('pearson'))}, "
            f"spearman {_number(correlation.get('spearman'))} "
            f"(n={correlation.get('n')})"
        )
    verdict = report.get("verdict")
    if isinstance(verdict, str) and verdict:
        lines.append(verdict)
    acceptance = report.get("acceptance")
    if isinstance(acceptance, dict) and mode == "edits":
        accepted = acceptance.get("accepted")
        revised = acceptance.get("revised")
        if isinstance(accepted, dict) and isinstance(revised, dict):
            lines.append(
                f"acceptance (edit rate <= {acceptance.get('threshold')}): "
                f"{accepted.get('count')} accepted (mean score {_number(accepted.get('meanScore'))}), "
                f"{revised.get('count')} revised (mean score {_number(revised.get('meanScore'))}), "
                f"gap {_number(acceptance.get('scoreGap'))}"
            )
    items = report.get("items")
    if isinstance(items, list) and items:
        lines.append("")
        lines.append("items:")
        for item in items[:MAX_ITEMS_IN_REPORT]:
            if not isinstance(item, dict):
                continue
            label = item.get("slug") or item.get("path") or "?"
            line = f"  {label}  score {_number(item.get('score'))}"
            if mode == "edits":
                line += f"  edit rate {_number(item.get('editRate'))}"
                line += "  accepted" if item.get("accepted") else "  revised"
            error = item.get("error")
            if isinstance(error, str) and error:
                line += f"  ({error})"
            lines.append(line)
        if len(items) > MAX_ITEMS_IN_REPORT:
            lines.append(f"  ... {len(items) - MAX_ITEMS_IN_REPORT} more")
    notes = _notes(report)
    if notes:
        lines.append("")
        lines.extend(f"note: {note}" for note in notes)
    return "\n".join(lines).rstrip() + "\n"


def _notes(report: dict[str, object]) -> list[str]:
    raw = report.get("notes")
    if not isinstance(raw, list):
        return []
    return [note for note in raw if isinstance(note, str) and note]


def _number(value: object) -> str:
    if isinstance(value, bool) or value is None:
        return "n/a"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        text = f"{value:.2f}".rstrip("0").rstrip(".")
        return text if text else "0"
    return str(value)
