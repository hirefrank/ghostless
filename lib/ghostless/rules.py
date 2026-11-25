"""Turn recurring edit patterns into writer-confirmed voice rules.

A word or structural edit in one piece is a data point, not a rule.
`mine_candidates` only surfaces a pattern (`cut:<word>`, `add:<word>`, or
`struct:<pattern>`) after it recurs across `rules.minimumSupport` distinct
pieces (default 3). Even then it is a *hypothesis* until the writer confirms
it. `rules.json` in the voice-data repository is the single source of truth for
those decisions; `build` reads it but never writes it, so a rejected rule
cannot come back on the next rebuild.
"""

from __future__ import annotations

import difflib
import json
import re
from collections.abc import Mapping
from pathlib import Path

from ghostless.io import atomic_write_json, text_preview, utc_now_iso
from ghostless.learn import load_edit_summary
from ghostless.paths import ProjectError
from ghostless.schema import (
    ConfirmedRule,
    RuleCandidate,
    RuleCounterexample,
    RuleExample,
    RulesState,
    RuleStatus,
    RuleSummary,
    SuppressedRule,
)
from ghostless.text import normalize, split_sentences, word_tokens

DEFAULT_MINIMUM_SUPPORT = 3
EXAMPLE_LIMIT = 3
SNIPPET_LIMIT = 180

_WORD_EDGE = r"(?<!\w){}(?!\w)"

_MARK_FEATURES: dict[str, str] = {
    "—": "em-dash",
    ";": "semicolon",
    "!": "exclamation",
}

_STRUCTURAL_RULES: tuple[tuple[str, str, int, str | None], ...] = (
    ("cut-em-dash", "em-dash", -1, "—"),
    ("add-em-dash", "em-dash", 1, "—"),
    ("cut-semicolon", "semicolon", -1, ";"),
    ("cut-exclamation", "exclamation", -1, "!"),
    ("split-paragraphs", "paragraphs", 1, None),
    ("merge-paragraphs", "paragraphs", -1, None),
)


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


def mine_candidates(root: Path, edits_summary: Mapping[str, object]) -> list[RuleCandidate]:
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

    candidates: list[RuleCandidate] = []
    for kind in ("cut", "add"):
        for word, items in supports[kind].items():
            distinct = _distinct_by_slug(items)
            if len(distinct) < threshold:
                continue
            supporting = {str(item.get("slug")) for item in distinct}
            side = "before" if kind == "cut" else "after"
            examples = [_example(kind, word, item, receipts, texts) for item in distinct[:EXAMPLE_LIMIT]]
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

    deltas_by_slug = {str(learn.get("slug")): _structural_deltas(learn) for learn in learns}
    for name, feature, sign, mark in _STRUCTURAL_RULES:
        matched_learns = [
            learn for learn in learns if deltas_by_slug.get(str(learn.get("slug")), {}).get(feature, 0) * sign > 0
        ]
        distinct = _distinct_by_slug(matched_learns)
        if len(distinct) < threshold:
            continue
        supporting = {str(item.get("slug")) for item in distinct}
        opposite_learns = _distinct_by_slug(
            [
                learn
                for learn in learns
                if str(learn.get("slug")) not in supporting
                and deltas_by_slug.get(str(learn.get("slug")), {}).get(feature, 0) * sign < 0
            ]
        )
        examples = [_structural_example(sign, mark, item, receipts, texts) for item in distinct[:EXAMPLE_LIMIT]]
        counterexamples = [
            _structural_counterexample(sign, mark, item, receipts, texts) for item in opposite_learns[:EXAMPLE_LIMIT]
        ]
        candidates.append(
            {
                "id": f"struct:{name}",
                "kind": "struct",
                "word": name,
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
    candidates.sort(key=lambda item: (-item["support"], item["id"]))
    return candidates


def default_rules() -> RulesState:
    return {
        "kind": "ghostless-rules",
        "version": 1,
        "confirmed": {},
        "rejected": {},
        "retired": {},
        "scope": {},
    }


def _decisions_map(raw: dict[object, object]) -> dict[str, dict[str, object]]:
    return {str(key): dict(value) if isinstance(value, dict) else {} for key, value in raw.items()}


def load_rules(root: Path) -> RulesState:
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
    confirmed = data.get("confirmed", {})
    if confirmed is not None:
        if not isinstance(confirmed, dict):
            raise ProjectError(f"{path}: confirmed must be an object.")
        state["confirmed"] = _decisions_map(confirmed)
    rejected = data.get("rejected", {})
    if rejected is not None:
        if not isinstance(rejected, dict):
            raise ProjectError(f"{path}: rejected must be an object.")
        state["rejected"] = _decisions_map(rejected)
    retired = data.get("retired", {})
    if retired is not None:
        if not isinstance(retired, dict):
            raise ProjectError(f"{path}: retired must be an object.")
        state["retired"] = _decisions_map(retired)
    scope = data.get("scope", {})
    if scope is not None:
        if not isinstance(scope, dict):
            raise ProjectError(f"{path}: scope must be an object.")
        state["scope"] = {key: value for key, value in scope.items() if isinstance(key, str) and isinstance(value, str)}
    return state


def save_rules(root: Path, state: RulesState) -> None:
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
    atomic_write_json(path, payload, sort_keys=True, ensure_ascii=False)


def derive_status(candidate: Mapping[str, object], state: Mapping[str, object]) -> RuleStatus:
    """Resolve a candidate against the writer's decisions.

    Explicit decisions always win, so a rejected or retired rule never returns
    as a hypothesis. Otherwise the rule is confirmed, or a hypothesis once it
    has enough support. `sparse` is only reachable when a caller passes a
    candidate that was never thresholded; `mine_candidates` drops those.
    """
    rule_id = str(candidate.get("id"))
    for key in ("rejected", "retired", "confirmed"):
        decisions = state.get(key)
        if isinstance(decisions, Mapping) and rule_id in decisions:
            match key:
                case "rejected":
                    return "rejected"
                case "retired":
                    return "retired"
                case "confirmed":
                    return "confirmed"
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
        raise ProjectError(f"No voice-rule candidate {rule_id}. Run `ghostless rules` to list candidates.")
    checked_section = _checked_section(section)
    previous_raw = state["confirmed"].get(rule_id)
    previous = previous_raw if isinstance(previous_raw, dict) else {}
    receipts = candidate["receipts"] if candidate is not None else previous.get("receipts", [])
    pieces = candidate["pieces"] if candidate is not None else previous.get("pieces", [])
    state["confirmed"][rule_id] = {
        "at": utc_now_iso(),
        "receipts": list(receipts) if isinstance(receipts, list) else [],
        "pieces": list(pieces) if isinstance(pieces, list) else [],
    }
    state["rejected"].pop(rule_id, None)
    state["retired"].pop(rule_id, None)
    # No section means every section. Re-confirming without one drops a
    # previous scope instead of leaving it in place.
    if checked_section:
        state["scope"][rule_id] = checked_section
    else:
        state["scope"].pop(rule_id, None)
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
        raise ProjectError(f"No voice-rule candidate {rule_id}. Run `ghostless rules` to list candidates.")
    at = utc_now_iso()
    state["rejected"][rule_id] = {"at": at}
    state["confirmed"].pop(rule_id, None)
    state["retired"].pop(rule_id, None)
    state["scope"].pop(rule_id, None)
    save_rules(root, state)
    return {"id": rule_id, "status": "rejected", "at": at}


def retire_rule(root: Path, rule_id: str) -> dict[str, object]:
    """Retire an active rule without forgetting the decision."""
    state = load_rules(root)
    if rule_id not in state["confirmed"]:
        raise ProjectError(f"{rule_id} is not a confirmed rule, so there is nothing to retire.")
    at = utc_now_iso()
    state["retired"][rule_id] = {"at": at}
    state["confirmed"].pop(rule_id, None)
    state["scope"].pop(rule_id, None)
    state["rejected"].pop(rule_id, None)
    save_rules(root, state)
    return {"id": rule_id, "status": "retired", "at": at}


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


def active_rules(root: Path, *, edits: Mapping[str, object] | None = None) -> list[ConfirmedRule]:
    """Confirmed rules with their scope and evidence, for the guide."""
    return rules_summary(root, edits=edits)["confirmed"]


def rules_summary(root: Path, *, edits: Mapping[str, object] | None = None) -> RuleSummary:
    """Everything the guide and `ghostless rules` need, split by status."""
    threshold = minimum_support(root)
    state = load_rules(root)
    edit_data: Mapping[str, object] = edits if edits is not None else load_edit_summary(root)
    candidates = mine_candidates(root, edit_data)
    by_id = {candidate["id"]: candidate for candidate in candidates}

    # A hand edit or merge can put one id in more than one bucket.
    # Precedence matches derive_status: rejected, then retired, then confirmed.
    rejected_ids = {rule_id for rule_id, _decision in _decision_items(state, "rejected")}
    retired_ids = {rule_id for rule_id, _decision in _decision_items(state, "retired") if rule_id not in rejected_ids}
    confirmed_ids = {
        rule_id
        for rule_id, _decision in _decision_items(state, "confirmed")
        if rule_id not in rejected_ids and rule_id not in retired_ids
    }
    confirmed: list[ConfirmedRule] = []
    for rule_id, decision in _decision_items(state, "confirmed"):
        if rule_id not in confirmed_ids:
            continue
        candidate = by_id.get(rule_id) or _candidate_from_id(rule_id, threshold)
        raw_receipts = decision.get("receipts")
        raw_pieces = decision.get("pieces")
        raw_at = decision.get("at")
        receipts = [str(item) for item in raw_receipts] if isinstance(raw_receipts, list) else candidate["receipts"]
        pieces = [str(item) for item in raw_pieces] if isinstance(raw_pieces, list) else candidate["pieces"]
        confirmed.append(
            {
                "id": candidate["id"],
                "kind": candidate["kind"],
                "word": candidate["word"],
                "support": candidate["support"],
                "minimumSupport": candidate["minimumSupport"],
                "pieces": pieces,
                "receipts": receipts,
                "sections": candidate["sections"],
                "origins": candidate["origins"],
                "examples": candidate["examples"],
                "counterexamples": candidate["counterexamples"],
                "scope": state["scope"].get(rule_id),
                "confirmedAt": raw_at if isinstance(raw_at, str) else None,
            }
        )

    hypotheses = [candidate for candidate in candidates if derive_status(candidate, state) == "hypothesis"]
    rejected_list: list[SuppressedRule] = []
    retired_list: list[SuppressedRule] = []
    for rule_id, decision in _decision_items(state, "rejected"):
        candidate = by_id.get(rule_id) or _candidate_from_id(rule_id, threshold)
        raw_at = decision.get("at")
        rejected_list.append(
            {
                "id": rule_id,
                "kind": candidate["kind"],
                "word": candidate["word"],
                "status": "rejected",
                "at": raw_at if isinstance(raw_at, str) else None,
            }
        )
    for rule_id, decision in _decision_items(state, "retired"):
        if rule_id not in retired_ids:
            continue
        candidate = by_id.get(rule_id) or _candidate_from_id(rule_id, threshold)
        raw_at = decision.get("at")
        retired_list.append(
            {
                "id": rule_id,
                "kind": candidate["kind"],
                "word": candidate["word"],
                "status": "retired",
                "at": raw_at if isinstance(raw_at, str) else None,
            }
        )
    return {
        "minimumSupport": threshold,
        "confirmed": confirmed,
        "hypotheses": hypotheses,
        "rejected": rejected_list,
        "retired": retired_list,
    }


def _learns(edits_summary: Mapping[str, object]) -> list[dict[str, object]]:
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
    values = {str(item[key]) for item in items if isinstance(item.get(key), str) and item[key]}
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
) -> RuleExample:
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
) -> RuleCounterexample:
    slug = str(learn.get("slug"))
    before, after = texts.get(slug, ("", ""))
    source = before if kind == "cut" else after
    return {
        "slug": slug,
        "receipt": _receipt_for(slug, receipts),
        "snippet": text_preview(source, SNIPPET_LIMIT),
    }


def _structural_deltas(learn: Mapping[str, object]) -> dict[str, int]:
    deltas: dict[str, int] = {}
    changes = learn.get("structuralChanges")
    if not isinstance(changes, list):
        return deltas
    for item in changes:
        if not isinstance(item, dict):
            continue
        raw_delta = item.get("delta")
        if isinstance(raw_delta, bool) or not isinstance(raw_delta, int):
            continue
        kind = item.get("kind")
        if kind == "punctuation":
            mark = item.get("mark")
            if isinstance(mark, str) and mark in _MARK_FEATURES:
                feature = _MARK_FEATURES[mark]
                deltas[feature] = deltas.get(feature, 0) + raw_delta
        elif kind == "paragraphs":
            deltas["paragraphs"] = deltas.get("paragraphs", 0) + raw_delta
    return deltas


def _structural_example(
    sign: int,
    mark: str | None,
    learn: dict[str, object],
    receipts: dict[str, str],
    texts: dict[str, tuple[str, str]],
) -> RuleExample:
    slug = str(learn.get("slug"))
    before, after = texts.get(slug, ("", ""))
    if mark is not None:
        before_sentences = split_sentences(before)
        after_sentences = split_sentences(after)
        source, target = (before_sentences, after_sentences) if sign < 0 else (after_sentences, before_sentences)
        matched = next((sentence for sentence in source if mark in normalize(sentence)), None)
        counterpart = _closest_sentence(matched, target) if matched is not None else None
        if matched is not None and counterpart is not None:
            if sign < 0:
                before_snippet = text_preview(matched, SNIPPET_LIMIT)
                after_snippet = text_preview(counterpart, SNIPPET_LIMIT)
            else:
                before_snippet = text_preview(counterpart, SNIPPET_LIMIT)
                after_snippet = text_preview(matched, SNIPPET_LIMIT)
        else:
            before_snippet = text_preview(before, SNIPPET_LIMIT)
            after_snippet = text_preview(after, SNIPPET_LIMIT)
    else:
        before_snippet = text_preview(before, SNIPPET_LIMIT)
        after_snippet = text_preview(after, SNIPPET_LIMIT)
    return {
        "slug": slug,
        "receipt": _receipt_for(slug, receipts),
        "before_snippet": before_snippet,
        "after_snippet": after_snippet,
    }


def _structural_counterexample(
    sign: int,
    mark: str | None,
    learn: dict[str, object],
    receipts: dict[str, str],
    texts: dict[str, tuple[str, str]],
) -> RuleCounterexample:
    slug = str(learn.get("slug"))
    before, after = texts.get(slug, ("", ""))
    if mark is not None:
        source = after if sign < 0 else before
        matched = next((sentence for sentence in split_sentences(source) if mark in normalize(sentence)), None)
        snippet_text = matched if matched is not None else source
    else:
        snippet_text = after
    return {
        "slug": slug,
        "receipt": _receipt_for(slug, receipts),
        "snippet": text_preview(snippet_text, SNIPPET_LIMIT),
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
        return text_preview(before, SNIPPET_LIMIT), text_preview(after, SNIPPET_LIMIT)
    counterpart = _closest_sentence(matched, target)
    if counterpart is None:
        return text_preview(before, SNIPPET_LIMIT), text_preview(after, SNIPPET_LIMIT)
    if kind == "cut":
        return text_preview(matched, SNIPPET_LIMIT), text_preview(counterpart, SNIPPET_LIMIT)
    return text_preview(counterpart, SNIPPET_LIMIT), text_preview(matched, SNIPPET_LIMIT)


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


def _checked_section(section: str | None) -> str | None:
    if section is None:
        return None
    text = section.strip()
    if not text:
        raise ProjectError("--section cannot be empty.")
    return text


def _find_candidate(root: Path, rule_id: str) -> RuleCandidate | None:
    for candidate in mine_candidates(root, load_edit_summary(root)):
        if candidate["id"] == rule_id:
            return candidate
    return None


def _decided(state: RulesState, rule_id: str) -> bool:
    return rule_id in state["confirmed"] or rule_id in state["rejected"] or rule_id in state["retired"]


def _decision_items(state: RulesState, key: str) -> list[tuple[str, dict[str, object]]]:
    match key:
        case "confirmed":
            block = state["confirmed"]
        case "rejected":
            block = state["rejected"]
        case "retired":
            block = state["retired"]
        case _:
            return []
    return list(block.items())


def _candidate_from_id(rule_id: str, threshold: int) -> RuleCandidate:
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
