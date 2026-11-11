"""Turn recurring edit patterns into writer-confirmed voice rules.

A word cut (or added) in one edit is a data point, not a rule. `mine_candidates`
only surfaces a word after it recurs across `rules.minimumSupport` distinct
pieces (default 3). Even then it is a *hypothesis* until the writer confirms
it. `rules.json` in the voice-data repository is the single source of truth for
those decisions; `build` reads it but never writes it, so a rejected rule
cannot come back on the next rebuild.

Word-level cut/add patterns are the v0.1 candidate set. Structural patterns
(repeated paragraph moves, punctuation changes) are a natural extension:
`mine_candidates` is the only place that decides what counts as a pattern.
"""

from __future__ import annotations

import difflib
import json
import os
import re
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Literal

from ghostless.learn import load_edit_summary
from ghostless.paths import ProjectError
from ghostless.text import split_sentences, word_tokens

DEFAULT_MINIMUM_SUPPORT = 3
EXAMPLE_LIMIT = 3
SNIPPET_LIMIT = 180

RuleStatus = Literal["confirmed", "rejected", "retired", "hypothesis", "sparse"]

_WORD_EDGE = r"(?<!\w){}(?!\w)"


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def minimum_support(root: Path) -> int:
    """Distinct pieces a pattern needs before it can be a candidate.

    Read from `manifest.json` `"rules": {"minimumSupport": N}`. Missing values
    fall back to three, which is the documented default.
    """
    path = root / "manifest.json"
    if not path.is_file():
        return DEFAULT_MINIMUM_SUPPORT
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ProjectError(f"{path}: {exc}") from exc
    if not isinstance(raw, dict):
        raise ProjectError(f"{path} must be an object.")
    block = raw.get("rules")
    if block is None:
        return DEFAULT_MINIMUM_SUPPORT
    if not isinstance(block, dict):
        raise ProjectError(f"{path}: rules must be an object.")
    value = block.get("minimumSupport", DEFAULT_MINIMUM_SUPPORT)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ProjectError(f"{path}: rules.minimumSupport must be an integer.")
    number = int(value)
    if number < 1:
        raise ProjectError(f"{path}: rules.minimumSupport must be at least 1.")
    return number


def mine_candidates(root: Path, edits_summary: dict[str, object]) -> list[dict[str, object]]:
    """Find cut/add words that recur across enough distinct edit pieces.

    Each candidate carries its support count, the sections and origins it came
    from, up to three before/after examples, and up to three counterexamples
    (pieces that had the word on the relevant side but were not an instance of
    the pattern). Words below the support threshold are not returned at all.
    """
    threshold = minimum_support(root)
    learns = _learns(edits_summary)
    receipts = _receipt_index(root)
    texts = _pair_texts(root, learns)
    tokens = {
        slug: {"before": set(word_tokens(before)), "after": set(word_tokens(after))}
        for slug, (before, after) in texts.items()
    }

    supports: dict[str, dict[str, list[dict[str, object]]]] = {"cut": {}, "add": {}}
    for learn in learns:
        for word in _words(learn.get("removedWords")):
            supports["cut"].setdefault(word, []).append(learn)
        for word in _words(learn.get("addedWords")):
            supports["add"].setdefault(word, []).append(learn)

    candidates: list[dict[str, object]] = []
    for kind in ("cut", "add"):
        for word, items in supports[kind].items():
            distinct = _distinct_by_slug(items)
            if len(distinct) < threshold:
                continue
            supporting = {str(item.get("slug")) for item in distinct}
            side = "before" if kind == "cut" else "after"
            examples = [
                _example(kind, word, item, receipts, texts)
                for item in distinct[:EXAMPLE_LIMIT]
            ]
            counterexamples = [
                _counterexample(kind, word, item, receipts, texts)
                for item in learns
                if str(item.get("slug")) not in supporting
                and word in tokens.get(str(item.get("slug")), {}).get(side, set())
            ][:EXAMPLE_LIMIT]
            candidates.append(
                {
                    "id": f"{kind}:{word}",
                    "kind": kind,
                    "word": word,
                    "support": len(distinct),
                    "minimumSupport": threshold,
                    "pieces": sorted(supporting),
                    "receipts": [_receipt_for(str(item.get("slug")), receipts) for item in distinct],
                    "sections": _distinct_values(distinct, "section"),
                    "origins": _distinct_values(distinct, "origin"),
                    "examples": examples,
                    "counterexamples": counterexamples,
                }
            )
    candidates.sort(key=lambda item: (-int(item["support"]), str(item["id"])))
    return candidates


def default_rules() -> dict[str, object]:
    return {
        "kind": "ghostless-rules",
        "version": 1,
        "confirmed": {},
        "rejected": {},
        "retired": {},
        "scope": {},
    }


def load_rules(root: Path) -> dict[str, object]:
    """Read `rules.json`. A missing file means no decisions yet.

    A malformed file fails loudly: silently defaulting would drop a rejection
    and let the rule back into the guide.
    """
    path = root / "rules.json"
    state = default_rules()
    if not path.is_file():
        return state
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ProjectError(f"{path}: {exc}") from exc
    if not isinstance(data, dict):
        raise ProjectError(f"{path} must be an object.")
    for key in ("confirmed", "rejected", "retired", "scope"):
        value = data.get(key, {})
        if value is None:
            continue
        if not isinstance(value, dict):
            raise ProjectError(f"{path}: {key} must be an object.")
        state[key] = value
    return state


def save_rules(root: Path, state: dict[str, object]) -> None:
    """Write `rules.json` atomically. Only writer decisions call this."""
    for key in ("confirmed", "rejected", "retired", "scope"):
        if not isinstance(state.get(key), dict):
            raise ProjectError(f"rules state is missing {key}.")
    path = root / "rules.json"
    payload = {
        "kind": "ghostless-rules",
        "version": 1,
        "confirmed": state["confirmed"],
        "rejected": state["rejected"],
        "retired": state["retired"],
        "scope": state["scope"],
    }
    text = json.dumps(payload, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output:
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def derive_status(candidate: dict[str, object], state: dict[str, object]) -> RuleStatus:
    """Resolve a candidate against the writer's decisions.

    Explicit decisions always win, so a rejected or retired rule never returns
    as a hypothesis. Otherwise the rule is confirmed, or a hypothesis once it
    has enough support. `sparse` is only reachable when a caller passes a
    candidate that was never thresholded; `mine_candidates` drops those.
    """
    rule_id = str(candidate.get("id"))
    for key in ("rejected", "retired", "confirmed"):
        decisions = state.get(key)
        if isinstance(decisions, dict) and rule_id in decisions:
            return key  # type: ignore[return-value]
    support = candidate.get("support")
    threshold = candidate.get("minimumSupport", DEFAULT_MINIMUM_SUPPORT)
    if not isinstance(support, int) or isinstance(support, bool):
        return "sparse"
    if not isinstance(threshold, int) or isinstance(threshold, bool):
        threshold = DEFAULT_MINIMUM_SUPPORT
    return "hypothesis" if support >= threshold else "sparse"


def confirm_rule(
    root: Path,
    rule_id: str,
    *,
    section: str | None = None,
) -> dict[str, object]:
    """Promote a candidate to an active rule, recording what it rests on."""
    state = load_rules(root)
    candidate = _find_candidate(root, rule_id)
    if candidate is None and not _decided(state, rule_id):
        raise ProjectError(
            f"No voice-rule candidate {rule_id}. Run `ghostless rules` to list candidates."
        )
    checked_section = _checked_section(section)
    previous = state["confirmed"].get(rule_id)
    previous = previous if isinstance(previous, dict) else {}
    receipts = candidate.get("receipts") if candidate else previous.get("receipts", [])
    pieces = candidate.get("pieces") if candidate else previous.get("pieces", [])
    state["confirmed"][rule_id] = {
        "at": _now(),
        "receipts": list(receipts) if isinstance(receipts, list) else [],
        "pieces": list(pieces) if isinstance(pieces, list) else [],
    }
    state["rejected"].pop(rule_id, None)
    state["retired"].pop(rule_id, None)
    if checked_section:
        state["scope"][rule_id] = checked_section
    save_rules(root, state)
    return {
        "id": rule_id,
        "status": "confirmed",
        "scope": state["scope"].get(rule_id),
        "receipts": state["confirmed"][rule_id]["receipts"],
        "pieces": state["confirmed"][rule_id]["pieces"],
    }


def reject_rule(root: Path, rule_id: str) -> dict[str, object]:
    """Suppress a candidate permanently. Rebuilds must not resurrect it."""
    state = load_rules(root)
    if _find_candidate(root, rule_id) is None and not _decided(state, rule_id):
        raise ProjectError(
            f"No voice-rule candidate {rule_id}. Run `ghostless rules` to list candidates."
        )
    state["rejected"][rule_id] = {"at": _now()}
    state["confirmed"].pop(rule_id, None)
    state["retired"].pop(rule_id, None)
    state["scope"].pop(rule_id, None)
    save_rules(root, state)
    return {"id": rule_id, "status": "rejected", "at": state["rejected"][rule_id]["at"]}


def retire_rule(root: Path, rule_id: str) -> dict[str, object]:
    """Retire an active rule without forgetting the decision."""
    state = load_rules(root)
    if rule_id not in state["confirmed"]:
        raise ProjectError(f"{rule_id} is not a confirmed rule, so there is nothing to retire.")
    state["retired"][rule_id] = {"at": _now()}
    state["confirmed"].pop(rule_id, None)
    state["scope"].pop(rule_id, None)
    state["rejected"].pop(rule_id, None)
    save_rules(root, state)
    return {"id": rule_id, "status": "retired", "at": state["retired"][rule_id]["at"]}


def scope_rule(root: Path, rule_id: str, section: str) -> dict[str, object]:
    """Limit a confirmed rule to one section."""
    state = load_rules(root)
    if rule_id not in state["confirmed"]:
        raise ProjectError(f"{rule_id} is not a confirmed rule. Confirm it before scoping it.")
    checked = _checked_section(section)
    if not checked:
        raise ProjectError("--section cannot be empty.")
    state["scope"][rule_id] = checked
    save_rules(root, state)
    return {"id": rule_id, "status": "confirmed", "scope": checked}


def active_rules(root: Path, *, edits: dict[str, object] | None = None) -> list[dict[str, object]]:
    """Confirmed rules with their scope and evidence, for the guide."""
    return rules_summary(root, edits=edits)["confirmed"]


def rules_summary(root: Path, *, edits: dict[str, object] | None = None) -> dict[str, object]:
    """Everything the guide and `ghostless rules` need, split by status."""
    threshold = minimum_support(root)
    state = load_rules(root)
    candidates = mine_candidates(root, edits if edits is not None else load_edit_summary(root))
    by_id = {str(candidate["id"]): candidate for candidate in candidates}

    confirmed: list[dict[str, object]] = []
    for rule_id, decision in _decision_items(state, "confirmed"):
        candidate = by_id.get(rule_id) or _candidate_from_id(rule_id, threshold)
        receipts = decision.get("receipts")
        pieces = decision.get("pieces")
        confirmed.append(
            {
                **candidate,
                "scope": state["scope"].get(rule_id),
                "confirmedAt": decision.get("at"),
                "receipts": receipts if isinstance(receipts, list) else candidate.get("receipts", []),
                "pieces": pieces if isinstance(pieces, list) else candidate.get("pieces", []),
            }
        )

    hypotheses = [
        candidate
        for candidate in candidates
        if derive_status(candidate, state) == "hypothesis"
    ]
    suppressed: list[dict[str, object]] = []
    for key in ("rejected", "retired"):
        for rule_id, decision in _decision_items(state, key):
            candidate = by_id.get(rule_id) or _candidate_from_id(rule_id, threshold)
            suppressed.append(
                {
                    "id": rule_id,
                    "kind": candidate.get("kind"),
                    "word": candidate.get("word"),
                    "status": key,
                    "at": decision.get("at"),
                }
            )
    return {
        "minimumSupport": threshold,
        "confirmed": confirmed,
        "hypotheses": hypotheses,
        "rejected": [item for item in suppressed if item["status"] == "rejected"],
        "retired": [item for item in suppressed if item["status"] == "retired"],
    }


def _learns(edits_summary: dict[str, object]) -> list[dict[str, object]]:
    raw = edits_summary.get("learns")
    if not isinstance(raw, list):
        return []
    return [item for item in raw if isinstance(item, dict) and isinstance(item.get("slug"), str)]


def _words(value: object) -> set[str]:
    words: set[str] = set()
    if not isinstance(value, list):
        return words
    for item in value:
        if isinstance(item, dict) and isinstance(item.get("word"), str):
            words.add(item["word"])
    return words


def _distinct_by_slug(items: list[dict[str, object]]) -> list[dict[str, object]]:
    seen: dict[str, dict[str, object]] = {}
    for item in items:
        seen.setdefault(str(item.get("slug")), item)
    return [seen[slug] for slug in sorted(seen)]


def _distinct_values(items: list[dict[str, object]], key: str) -> list[str]:
    values = {
        str(item[key])
        for item in items
        if isinstance(item.get(key), str) and item[key]
    }
    return sorted(values)


def _receipt_index(root: Path) -> dict[str, str]:
    """Map a learned path (`gold/<slug>`) to its finished session receipt."""
    found: dict[str, str] = {}
    folder = root / "sessions"
    if not folder.is_dir():
        return found
    for path in sorted(folder.glob("*/receipt.json")):
        try:
            receipt = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(receipt, dict):
            continue
        learned = receipt.get("learnedPath")
        if isinstance(learned, str) and learned:
            found[learned] = f"{path.relative_to(root).as_posix()}"
    return found


def _receipt_for(slug: str, receipts: dict[str, str]) -> str:
    return receipts.get(f"gold/{slug}", f"gold/{slug}")


def _pair_texts(
    root: Path,
    learns: list[dict[str, object]],
) -> dict[str, tuple[str, str]]:
    texts: dict[str, tuple[str, str]] = {}
    for learn in learns:
        slug = str(learn.get("slug"))
        if slug in texts:
            continue
        folder = root / "gold" / slug
        try:
            texts[slug] = (
                (folder / "before.md").read_text(encoding="utf-8"),
                (folder / "after.md").read_text(encoding="utf-8"),
            )
        except OSError:
            texts[slug] = ("", "")
    return texts


def _example(
    kind: str,
    word: str,
    learn: dict[str, object],
    receipts: dict[str, str],
    texts: dict[str, tuple[str, str]],
) -> dict[str, object]:
    slug = str(learn.get("slug"))
    before, after = texts.get(slug, ("", ""))
    before_snippet, after_snippet = _snippets(before, after, word, kind)
    return {
        "slug": slug,
        "receipt": _receipt_for(slug, receipts),
        "before_snippet": before_snippet,
        "after_snippet": after_snippet,
    }


def _counterexample(
    kind: str,
    word: str,
    learn: dict[str, object],
    receipts: dict[str, str],
    texts: dict[str, tuple[str, str]],
) -> dict[str, object]:
    slug = str(learn.get("slug"))
    before, after = texts.get(slug, ("", ""))
    source = before if kind == "cut" else after
    return {
        "slug": slug,
        "receipt": _receipt_for(slug, receipts),
        "snippet": _preview(source),
    }


def _snippets(before: str, after: str, word: str, kind: str) -> tuple[str, str]:
    """A before/after pair around the word, using the closest sentence."""
    before_sentences = split_sentences(before)
    after_sentences = split_sentences(after)
    if kind == "cut":
        source, target = before_sentences, after_sentences
    else:
        source, target = after_sentences, before_sentences
    matched = next(
        (sentence for sentence in source if _contains_word(sentence, word)),
        None,
    )
    if matched is None:
        return _preview(before), _preview(after)
    counterpart = _closest_sentence(matched, target)
    if counterpart is None:
        return _preview(before), _preview(after)
    if kind == "cut":
        return _preview(matched), _preview(counterpart)
    return _preview(counterpart), _preview(matched)


def _contains_word(text: str, word: str) -> bool:
    return re.search(_WORD_EDGE.format(re.escape(word)), text, re.IGNORECASE) is not None


def _closest_sentence(source: str, sentences: list[str]) -> str | None:
    best: tuple[float, str] | None = None
    folded = source.casefold()
    for sentence in sentences:
        ratio = difflib.SequenceMatcher(None, folded, sentence.casefold()).ratio()
        if best is None or ratio > best[0]:
            best = (ratio, sentence)
    return best[1] if best else None


def _preview(text: str, limit: int = SNIPPET_LIMIT) -> str:
    collapsed = " ".join(text.split())
    if len(collapsed) <= limit:
        return collapsed
    return collapsed[: limit - 1].rstrip() + "…"


def _checked_section(section: str | None) -> str | None:
    if section is None:
        return None
    text = section.strip()
    if not text:
        raise ProjectError("--section cannot be empty.")
    return text


def _find_candidate(root: Path, rule_id: str) -> dict[str, object] | None:
    for candidate in mine_candidates(root, load_edit_summary(root)):
        if candidate.get("id") == rule_id:
            return candidate
    return None


def _decided(state: dict[str, object], rule_id: str) -> bool:
    return any(
        isinstance(state.get(key), dict) and rule_id in state[key]  # type: ignore[operator]
        for key in ("confirmed", "rejected", "retired")
    )


def _decision_items(state: dict[str, object], key: str) -> list[tuple[str, dict[str, object]]]:
    block = state.get(key)
    if not isinstance(block, dict):
        return []
    items: list[tuple[str, dict[str, object]]] = []
    for rule_id, decision in block.items():
        if isinstance(rule_id, str):
            items.append((rule_id, decision if isinstance(decision, dict) else {}))
    return items


def _candidate_from_id(rule_id: str, threshold: int) -> dict[str, object]:
    kind, separator, word = rule_id.partition(":")
    if not separator:
        kind, word = "cut", rule_id
    return {
        "id": rule_id,
        "kind": kind,
        "word": word,
        "support": 0,
        "minimumSupport": threshold,
        "pieces": [],
        "receipts": [],
        "sections": [],
        "origins": [],
        "examples": [],
        "counterexamples": [],
    }
