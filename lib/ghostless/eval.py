"""Held-out evaluation: does the judge score track the writer's edits?

A score is only useful if it predicts acceptance. This module splits the
learned edit pairs into train and holdout, builds voice targets from the train
pieces only, judges each held-out pair's `before` draft, and compares that
score with the writer's own edit rate for the pair. The edit rate is the one
defined by `ghostless.learn.diff_texts` (#10): edited aligned token positions
divided by all aligned positions, bounded 0..1. A writer who accepted a draft
with light edits has a low rate, so a score that predicts acceptance should
correlate *negatively* with it.

Confirmed edit checks are a second, separate count: passes over checks on the
held-out befores, with each check's after file as the case that already
passes. That count is not blended into the mechanical score.

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

from ghostless.checks import empty_confirmed_checks, grade_confirmed
from ghostless.judge import corpus_confidence, judge_text
from ghostless.learn import diff_texts
from ghostless.paths import ProjectError
from ghostless.project import (
    SKIP_NAMES,
    TEXT_SUFFIXES,
    discover,
    load_manifest,
    pair_file_path,
    require_contained_path,
    split_frontmatter,
)
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
            pair_dir = require_contained_path(folder, meta_path.parent, label=str(meta_path))
        except ProjectError:
            notes.append(f"Skipped {meta_path.parent.name}: before/after file escapes pair directory.")
            continue
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            notes.append(f"Skipped {meta_path.parent.name}: unreadable meta.json.")
            continue
        if not isinstance(meta, dict):
            notes.append(f"Skipped {meta_path.parent.name}: meta.json is not an object.")
            continue
        try:
            before_path = pair_file_path(meta, meta_path, "beforeFile", "before.md", base=pair_dir)
            after_path = pair_file_path(meta, meta_path, "afterFile", "after.md", base=pair_dir)
        except ProjectError as exc:
            if "escapes" in str(exc) or "not a valid path" in str(exc):
                notes.append(
                    f"Skipped {meta_path.parent.name}: before/after file escapes pair directory."
                )
            else:
                notes.append(f"Skipped {meta_path.parent.name}: {exc}")
            continue
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


def load_holdouts(root: Path, manifest: dict[str, object]) -> list[Piece]:
    """Read prose files under `holdouts/` without letting them enter the build corpus."""
    folder = root / "holdouts"
    if not folder.is_dir():
        return []
    weights = manifest.get("weights")
    if not isinstance(weights, dict) or "gold" not in weights:
        raise ProjectError("manifest weights are missing.")
    gold_weight = float(weights["gold"])
    root_resolved = root.resolve()
    paths: list[Path] = []
    for path in folder.rglob("*"):
        if not path.is_file():
            continue
        relative = path.relative_to(folder)
        if any(part.startswith(".") for part in relative.parts):
            continue
        if path.name.lower() in SKIP_NAMES:
            continue
        if path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        paths.append(path)
    pieces: list[Piece] = []
    for path in sorted(paths):
        resolved = require_contained_path(folder, path, label=str(path))
        try:
            raw = resolved.read_text(encoding="utf-8")
        except UnicodeError as exc:
            raise ProjectError(f"{path} is not valid UTF-8.") from exc
        meta, body = split_frontmatter(raw)
        text = body.strip()
        if not text:
            continue
        relative = path.relative_to(folder)
        meta_section = meta.get("section")
        section = (
            meta_section
            if isinstance(meta_section, str) and meta_section
            else (relative.parts[0] if len(relative.parts) > 1 else None)
        )
        pieces.append(
            Piece(
                path=resolved.relative_to(root_resolved).as_posix(),
                tier="gold",
                kind="prose",
                section=section,
                weight=gold_weight,
                text=text,
                origin="unaided",
                confirmation="origin",
            )
        )
    pieces.sort(key=lambda piece: piece.path)
    return pieces


def evaluate(
    root: Path,
    *,
    holdout: int | None = None,
    seed: int = 0,
    cv: bool | None = None,
) -> dict[str, object]:
    """Run the held-out evaluation for one voice project."""
    if cv is True and holdout is not None:
        raise ProjectError("Pass either --cv or --holdout, not both.")
    manifest = load_manifest(root)
    pieces = discover(root, manifest)
    weights = manifest["weights"]
    if not isinstance(weights, dict):
        raise ProjectError("manifest weights are missing.")
    weights_map = {str(key): float(value) for key, value in weights.items()}
    minimum = int(manifest["goldVoiceMinimum"])
    fail_under = int(manifest["failUnder"])
    overrides = manifest.get("overrides") if isinstance(manifest.get("overrides"), dict) else None
    placeholder = manifest.get("placeholder") if isinstance(manifest.get("placeholder"), str) else None

    holdouts = load_holdouts(root, manifest)
    holdout_eval = _evaluate_holdouts(
        pieces=pieces,
        holdouts=holdouts,
        weights=weights_map,
        minimum=minimum,
        fail_under=fail_under,
        overrides=overrides,
        placeholder=placeholder,
    )

    pairs, notes = load_edit_pairs(root)
    if pairs:
        use_cv = cv if cv is not None else (holdout is None and 3 <= len(pairs) <= 30)
        if use_cv:
            report = _evaluate_edits_cv(
                pieces=pieces,
                pairs=pairs,
                weights=weights_map,
                minimum=minimum,
                fail_under=fail_under,
                overrides=overrides,
                placeholder=placeholder,
                seed=seed,
                notes=notes,
            )
            held_pairs = pairs
        else:
            report = _evaluate_edits(
                pieces=pieces,
                pairs=pairs,
                weights=weights_map,
                minimum=minimum,
                fail_under=fail_under,
                overrides=overrides,
                placeholder=placeholder,
                holdout=holdout,
                seed=seed,
                notes=notes,
            )
            _, held = split_items(list(pairs), holdout, seed)
            held_pairs = [pair for pair in held if isinstance(pair, EditPair)]
        report["holdouts"] = holdout_eval
        held_payload = [
            {"slug": pair.slug, "section": pair.section, "before": pair.before}
            for pair in held_pairs
        ]
        report["confirmedChecks"] = grade_confirmed(root, held_payload, seed=seed)
        return report

    gold_prose_count = sum(
        1 for piece in pieces if piece.tier == "gold" and piece.kind == "prose"
    )
    if holdout_eval is not None and gold_prose_count < 2:
        return {
            "ok": True,
            "mode": "holdouts",
            "cv": False,
            "seed": seed,
            "totalCount": len(holdouts),
            "holdoutCount": len(holdouts),
            "train": holdout_eval["train"],
            "holdout": holdout_eval["summary"],
            "correlation": {"pearson": None, "spearman": None, "n": 0},
            "acceptance": None,
            "direction": "unknown",
            "verdict": "Evaluated files in holdouts/ against the corpus targets (no edit pairs yet).",
            "items": holdout_eval["items"],
            "holdouts": holdout_eval,
            "confirmedChecks": empty_confirmed_checks(
                "No edit pairs, so there are no confirmed checks to grade."
            ),
            "notes": notes,
        }

    report = _evaluate_prose(
        pieces=pieces,
        weights=weights_map,
        minimum=minimum,
        fail_under=fail_under,
        overrides=overrides,
        placeholder=placeholder,
        holdout=holdout,
        seed=seed,
        notes=notes,
    )
    report["cv"] = False
    report["holdouts"] = holdout_eval
    report["confirmedChecks"] = empty_confirmed_checks(
        "No edit pairs, so there are no confirmed checks to grade."
    )
    return report


def _evaluate_holdouts(
    *,
    pieces: list[Piece],
    holdouts: list[Piece],
    weights: dict[str, float],
    minimum: int,
    fail_under: int,
    overrides: dict[str, object] | None,
    placeholder: str | None,
) -> dict[str, object] | None:
    """Evaluate `holdouts/` prose against targets compiled from `pieces`."""
    if not holdouts or not pieces:
        return None
    stats = compile_stats(pieces, weights=weights, gold_voice_minimum=minimum)
    confidence = corpus_confidence(stats)
    items = [
        _judge_prose_piece(
            piece,
            stats,
            fail_under=fail_under,
            overrides=overrides,
            placeholder=placeholder,
        )
        for piece in holdouts
    ]
    return {
        "pieces": len(holdouts),
        "train": _train_block(pieces, confidence),
        "summary": _holdout_block(items),
        "items": items,
    }


def _evaluate_edits(
    *,
    pieces: list[Piece],
    pairs: list[EditPair],
    weights: dict[str, float],
    minimum: int,
    fail_under: int,
    overrides: dict[str, object] | None,
    placeholder: str | None,
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
    items = [
        _judge_edit_pair(
            pair,
            stats,
            fail_under=fail_under,
            overrides=overrides,
            placeholder=placeholder,
        )
        for pair in held
    ]
    return _finalize_edits_report(
        pairs=pairs,
        train_pieces=train_pieces,
        confidence=confidence,
        items=items,
        seed=seed,
        notes=notes,
        cv=False,
    )


def _evaluate_edits_cv(
    *,
    pieces: list[Piece],
    pairs: list[EditPair],
    weights: dict[str, float],
    minimum: int,
    fail_under: int,
    overrides: dict[str, object] | None,
    placeholder: str | None,
    seed: int,
    notes: list[str],
) -> dict[str, object]:
    """Leave-one-out cross-validation across all edit pairs."""
    items: list[dict[str, object]] = []
    fold_trains: list[tuple[list[Piece], dict[str, object]]] = []
    for pair in pairs:
        train_pieces = [piece for piece in pieces if piece.path != pair.after_path]
        if not train_pieces:
            items.append(
                _edit_item(
                    pair,
                    score=None,
                    ok=False,
                    error="Holding out this edit pair left no corpus to build targets from.",
                )
            )
            continue
        stats = compile_stats(train_pieces, weights=weights, gold_voice_minimum=minimum)
        fold_trains.append((train_pieces, corpus_confidence(stats)))
        items.append(
            _judge_edit_pair(
                pair,
                stats,
                fail_under=fail_under,
                overrides=overrides,
                placeholder=placeholder,
            )
        )
    if not fold_trains:
        return _failure(
            "edits",
            "Holding out an edit pair left no corpus to build targets from. Add writing to corpus/.",
            notes,
            seed,
            cv=True,
        )
    smallest_train, smallest_confidence = min(
        fold_trains,
        key=lambda fold: (len(fold[0]), int(fold[1].get("words") or 0)),
    )
    if smallest_confidence["level"] == "low":
        notes.append(
            "The train sample is small, so treat the correlation as a direction, not a verdict."
        )
    return _finalize_edits_report(
        pairs=pairs,
        train_pieces=smallest_train,
        confidence=smallest_confidence,
        items=items,
        seed=seed,
        notes=notes,
        cv=True,
    )


def _edit_item(
    pair: EditPair,
    *,
    score: int | None,
    ok: bool,
    error: object = None,
) -> dict[str, object]:
    item: dict[str, object] = {
        "kind": "edit",
        "slug": pair.slug,
        "section": pair.section,
        "origin": pair.origin,
        "words": len(words(pair.before)),
        "score": score,
        "ok": ok,
        "editRate": round(pair.edit_rate, 4),
        "accepted": pair.edit_rate <= ACCEPT_EDIT_RATE,
    }
    if isinstance(error, str) and error:
        item["error"] = error
    return item


def _judge_edit_pair(
    pair: EditPair,
    stats: dict[str, object],
    *,
    fail_under: int,
    overrides: dict[str, object] | None,
    placeholder: str | None,
) -> dict[str, object]:
    section = pair.section if _section_exists(stats, pair.section) else None
    report = judge_text(
        pair.before,
        stats,
        section=section,
        fail_under=fail_under,
        overrides=overrides,
        placeholder=placeholder,
    )
    return _edit_item(
        pair,
        score=_score(report),
        ok=bool(report.get("ok")),
        error=report.get("error"),
    )


def _finalize_edits_report(
    *,
    pairs: list[EditPair],
    train_pieces: list[Piece],
    confidence: dict[str, object],
    items: list[dict[str, object]],
    seed: int,
    notes: list[str],
    cv: bool,
) -> dict[str, object]:
    scored = [
        item
        for item in items
        if isinstance(item.get("score"), int) and not isinstance(item.get("score"), bool)
    ]
    n = len(scored)
    pairs_for_stats = [
        (float(item["score"]), float(item["editRate"]))
        for item in scored
        if isinstance(item.get("editRate"), (int, float))
        and not isinstance(item.get("editRate"), bool)
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
        "cv": cv,
        "seed": seed,
        "totalCount": len(pairs),
        "holdoutCount": len(items),
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
    placeholder: str | None,
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

    items = [
        _judge_prose_piece(
            piece,
            stats,
            fail_under=fail_under,
            overrides=overrides,
            placeholder=placeholder,
        )
        for piece in held
    ]

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


def _judge_prose_piece(
    piece: Piece,
    stats: dict[str, object],
    *,
    fail_under: int,
    overrides: dict[str, object] | None,
    placeholder: str | None,
) -> dict[str, object]:
    section = piece.section if _section_exists(stats, piece.section) else None
    report = judge_text(
        piece.text,
        stats,
        section=section,
        fail_under=fail_under,
        overrides=overrides,
        placeholder=placeholder,
    )
    item: dict[str, object] = {
        "kind": "piece",
        "path": piece.path,
        "section": piece.section,
        "words": len(words(piece.text)),
        "score": _score(report),
        "ok": bool(report.get("ok")),
    }
    error = report.get("error")
    if isinstance(error, str) and error:
        item["error"] = error
    return item


def _failure(
    mode: str,
    message: str,
    notes: list[str],
    seed: int,
    *,
    cv: bool = False,
) -> dict[str, object]:
    return {
        "ok": False,
        "mode": mode,
        "cv": cv,
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
        and not isinstance(item.get("editRate"), bool)
    ]
    judged = len(scores)
    passed = sum(
        1
        for item in items
        if isinstance(item.get("score"), int)
        and not isinstance(item.get("score"), bool)
        and item.get("ok")
    )
    return {
        "pieces": len(items),
        "judged": judged,
        "meanScore": _mean(scores),
        "minScore": min(scores) if scores else None,
        "maxScore": max(scores) if scores else None,
        "meanEditRate": _mean(rates),
        "passRate": round(passed / judged, 4) if judged else None,
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
        lines.extend(_format_checks(report))
        lines.extend(f"note: {note}" for note in _notes(report))
        return "\n".join(lines).rstrip() + "\n"
    mode = report.get("mode")
    if report.get("cv"):
        headline = f"Leave-one-out evaluation: {report.get('holdoutCount')} edit pairs (out-of-sample)"
    elif mode == "holdouts":
        headline = f"Holdouts evaluation: {report.get('holdoutCount')} piece(s) from holdouts/"
    else:
        peer = "edit pairs" if mode == "edits" else "gold pieces"
        headline = (
            f"Held-out evaluation: {report.get('holdoutCount')} of {report.get('totalCount')} "
            f"{peer} held out (seed {report.get('seed')})"
        )
    lines = [headline]
    train = report.get("train")
    if isinstance(train, dict):
        lines.append(
            f"train: {train.get('pieces')} pieces, {_number(train.get('words'))} words, "
            f"{_number(train.get('sentences'))} sentences, "
            f"{train.get('confidence')} confidence"
        )
    holdout = report.get("holdout")
    if isinstance(holdout, dict):
        lines.append(_summary_line("held out", holdout, include_edit_rate=(mode == "edits")))
    holdouts_block = report.get("holdouts")
    if mode != "holdouts" and isinstance(holdouts_block, dict):
        summary = holdouts_block.get("summary")
        if isinstance(summary, dict):
            lines.append(_summary_line("holdouts/", summary))
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
    lines.extend(_format_checks(report))
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


def _summary_line(
    prefix: str,
    block: dict[str, object],
    *,
    include_edit_rate: bool = False,
) -> str:
    line = f"{prefix}: {block.get('judged')} judged, mean score {_number(block.get('meanScore'))}"
    if include_edit_rate:
        line += f", mean edit rate {_number(block.get('meanEditRate'))}"
    else:
        pass_rate = block.get("passRate")
        if isinstance(pass_rate, (int, float)) and not isinstance(pass_rate, bool):
            line += f", pass rate {pass_rate * 100:.0f}%"
    return line


def _format_checks(report: dict[str, object]) -> list[str]:
    """Passes over confirmed checks. Separate from the mechanical score."""
    block = report.get("confirmedChecks")
    if not isinstance(block, dict):
        return []
    label = block.get("label")
    if isinstance(label, str) and label:
        line = f"confirmed checks: {label}"
        if block.get("fresh"):
            line += " (fresh session)"
        elif block.get("session"):
            line += " (same session)"
    else:
        line = "confirmed checks: none"
    lines = [line]
    note = block.get("note")
    if isinstance(note, str) and note:
        lines.append(f"  {note}")
    items = block.get("items")
    if isinstance(items, list):
        failing = [item for item in items if isinstance(item, dict) and item.get("pass") is False]
        if failing:
            lines.append("  failing:")
            for item in failing[:8]:
                lines.append(
                    f"    {item.get('slug')} {item.get('case')}: {item.get('text')}"
                )
            if len(failing) > 8:
                lines.append(f"    ... {len(failing) - 8} more")
    return lines


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
