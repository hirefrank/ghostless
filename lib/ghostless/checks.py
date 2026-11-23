"""Writer-confirmed yes/no checks saved from a real edit.

A check is a judgment the mechanical score cannot see: the opener states the
problem, a term is explained, a label is not copied. The sentence is the
writer's, taken from the edit. Nothing is active until they confirm it, and a
rejected check is not brought back by a later edit or a rebuild.

Grading is local. The witness is a short span the edit added or cut, not a
model reading the sentence. The after file is the case that already passes.
Held-out befores pass or fail on that same span. The report is passes over
checks. It never changes the mechanical judge score, and it never sends prose
to a model.

A fresh grade session starts when the drafting skill, the voice guide, the
confirmed set, or the holdout changes. The same inputs keep the same session.
"""

from __future__ import annotations

import difflib
import hashlib
import json
import os
import re
from pathlib import Path
from typing import Literal

from ghostless.harness import AGENTS, COPY_MARKER, agent_dir
from ghostless.io import atomic_write_json, sha256_text, utc_now_iso
from ghostless.paths import ProjectError, suite_root
from ghostless.schema import ChecksState, ChecksSummary
from ghostless.tells import AI_TELLS
from ghostless.text import STOPWORDS, WORD_RE, normalize, strip_non_prose, word_tokens

MAX_CHECK_CHARS = 400
MAX_SPAN_CHARS = 120
MAX_SPAN_TOKENS = 12

# Phrases that restate a metric `judge` already compares. A check that is
# only one of these is refused. "The opener states the problem" is not in
# this list: opener share is, the content of the opening is not.
MECHANICAL_PHRASES = (
    "sentence length",
    "median sentence",
    "contraction",
    "em dash",
    "em-dash",
    "per 1k",
    "per 1,000",
    "opener share",
    "paragraph length",
    "ban list",
    "first-person",
    "questions per",
    "sentence share",
    "and/but/so",
)

# Straight and curly double quotes are the same quotation. Either side may
# use either character; a pasted sentence often mixes them.
QUOTE_RE = re.compile(r'["“]([^"“”\n]{2,})["”]')
# Apostrophe stays inside the word, matching WORD_RE. A boundary on `\w`
# alone finds `writer` inside `writer's`.
_WORD_EDGE = r"[\w']"
_HEADING_MARK = re.compile(r"^[ \t]{0,3}#{1,6}[ \t]+", re.MULTILINE)
_TRAILING_PUNCT = ".,;:!?"
TELLS = frozenset(tell.casefold() for tell in AI_TELLS)


def check_id(text: str) -> str:
    """Stable id for one check sentence. The same words are the same check.

    Straight and curly quotes are the same sentence. A rejected check cannot
    come back by retyping it with a smart apostrophe.
    """
    normalized = " ".join(normalize(text).split()).casefold()
    slug = re.sub(r"[^a-z0-9]+", "-", normalized).strip("-")
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()[:6]
    if len(slug) > 40:
        slug = slug[:40].rstrip("-")
    if not slug:
        slug = "text"
    return f"check:{slug}-{digest}"


def mechanical_overlap(text: str) -> str | None:
    """The metric phrase this check restates, if it restates one."""
    folded = normalize(text).casefold()
    for phrase in MECHANICAL_PHRASES:
        if phrase in folded:
            return phrase
    return None


def default_checks() -> ChecksState:
    return {
        "kind": "ghostless-checks",
        "version": 1,
        "confirmed": {},
        "rejected": {},
    }


def _records_dict(raw: dict[object, object]) -> dict[str, dict[str, object]]:
    return {
        str(key): dict(value) if isinstance(value, dict) else {}
        for key, value in raw.items()
    }


def load_checks(root: Path) -> ChecksState:
    """Read `checks.json`. A missing file means no decisions yet.

    A malformed file fails loudly. Defaulting would drop a rejection and let
    the check back into the guide.
    """
    path = root / "checks.json"
    state = default_checks()
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
        state["confirmed"] = _records_dict(confirmed)
    rejected = data.get("rejected", {})
    if rejected is not None:
        if not isinstance(rejected, dict):
            raise ProjectError(f"{path}: rejected must be an object.")
        state["rejected"] = _records_dict(rejected)
    return state


def save_checks(root: Path, state: ChecksState) -> None:
    """Write `checks.json` atomically. Only a writer decision calls this."""
    if not isinstance(state.get("confirmed"), dict):
        raise ProjectError("checks state is missing confirmed.")
    if not isinstance(state.get("rejected"), dict):
        raise ProjectError("checks state is missing rejected.")
    path = root / "checks.json"
    payload = {
        "kind": "ghostless-checks",
        "version": 1,
        "confirmed": state["confirmed"],
        "rejected": state["rejected"],
    }
    atomic_write_json(path, payload, sort_keys=True, ensure_ascii=False)


def prepare_checks(
    root: Path,
    before: str,
    after: str,
    texts: list[str],
) -> list[dict[str, object]]:
    """Turn writer sentences into pending checks, or refuse them.

    The text is stored as given (stripped). A model does not write it. A
    check that restates a judge metric is refused. A rejected check stays
    rejected: saving it again does not create a new hypothesis.
    """
    state = load_checks(root)
    prepared: list[dict[str, object]] = []
    seen: set[str] = set()
    for raw in texts:
        if not isinstance(raw, str):
            raise ProjectError("A check must be text from the edit.")
        cleaned = raw.strip()
        if not cleaned:
            raise ProjectError("A check cannot be empty.")
        if "\n" in cleaned or len(cleaned) > MAX_CHECK_CHARS:
            raise ProjectError(
                "A check is one sentence from the edit, not a draft and not a list."
            )
        overlap = mechanical_overlap(cleaned)
        if overlap:
            raise ProjectError(
                f"This check restates {overlap!r}, which judge already scores. "
                "Save a judgment those counts cannot see."
            )
        identifier = check_id(cleaned)
        if identifier in seen:
            raise ProjectError(f"Duplicate check: {cleaned}")
        seen.add(identifier)
        rejected = _record(state, "rejected", identifier)
        if rejected is not None:
            raise ProjectError(
                f"{identifier} was rejected and stays rejected. "
                "A later edit does not bring it back."
            )
        confirmed = _record(state, "confirmed", identifier)
        if confirmed is not None:
            prepared.append({**confirmed, "id": identifier, "status": "confirmed", "reused": True})
            continue
        existing = _find_pending(root, identifier)
        if existing is not None:
            prepared.append({**existing, "status": "pending", "reused": True})
            continue
        witness = witness_for(cleaned, before, after)
        prepared.append(
            {
                "id": identifier,
                "text": cleaned,
                "span": witness["span"],
                "polarity": witness["polarity"],
                "status": "pending",
                "reused": False,
            }
        )
    return prepared


def stored_check(item: dict[str, object]) -> dict[str, object]:
    """The fields kept on a pair. Decisions live in checks.json, not here."""
    return {
        "id": item["id"],
        "text": item["text"],
        "span": item["span"],
        "polarity": item["polarity"],
    }


def new_pair_checks(prepared: list[dict[str, object]]) -> list[dict[str, object]]:
    """Checks this edit establishes. Reused and already-confirmed ones stay put."""
    return [
        stored_check(item)
        for item in prepared
        if item.get("status") == "pending" and not item.get("reused")
    ]


def write_pair_checks(root: Path, slug: str, texts: list[str]) -> list[dict[str, object]]:
    """Attach checks to an existing learned pair. The pair is the edit."""
    folder = root / "gold" / slug
    meta_path = folder / "meta.json"
    before_path = folder / "before.md"
    after_path = folder / "after.md"
    if not meta_path.is_file() or not before_path.is_file() or not after_path.is_file():
        raise ProjectError(f"No learned pair at gold/{slug}.")
    try:
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ProjectError(f"{meta_path}: {exc}") from exc
    if not isinstance(meta, dict):
        raise ProjectError(f"{meta_path} must be an object.")
    before = before_path.read_text(encoding="utf-8")
    after = after_path.read_text(encoding="utf-8")
    prepared = prepare_checks(root, before, after, texts)
    fresh = new_pair_checks(prepared)
    if fresh:
        current = meta.get("checks")
        merged = [item for item in current if isinstance(item, dict)] if isinstance(current, list) else []
        known = {item.get("id") for item in merged}
        for item in fresh:
            if item["id"] not in known:
                merged.append(item)
        meta["checks"] = merged
        atomic_write_json(meta_path, meta, ensure_ascii=False)
    return prepared


def confirm_check(
    root: Path,
    token: str,
    *,
    section: str | None = None,
) -> dict[str, object]:
    """Make a saved check active. An explicit confirm can reverse a rejection.

    A rebuild or a new edit cannot. That is the same gate as a voice rule:
    rejection sticks until the writer confirms it again on purpose.
    """
    state = load_checks(root)
    identifier = resolve_check_id(root, token)
    proposal = _proposal(root, state, identifier)
    if proposal is None:
        raise ProjectError(
            f"No check {token}. Save one from an edit with "
            "`ghostless learn --check` or `ghostless checks add <slug> --text`."
        )
    checked = _checked_section(section)
    if checked is None:
        current = proposal.get("section")
        checked = current if isinstance(current, str) and current else None
    state["confirmed"][identifier] = {
        "at": utc_now_iso(),
        "text": proposal["text"],
        "span": proposal["span"],
        "polarity": proposal["polarity"],
        "pair": proposal["pair"],
        "section": checked,
    }
    state["rejected"].pop(identifier, None)
    save_checks(root, state)
    record = state["confirmed"][identifier]
    return {"id": identifier, "status": "confirmed", **record}


def reject_check(root: Path, token: str) -> dict[str, object]:
    """Suppress a check. Later edits that repeat the sentence do not revive it."""
    state = load_checks(root)
    identifier = resolve_check_id(root, token)
    proposal = _proposal(root, state, identifier)
    if proposal is None:
        raise ProjectError(
            f"No check {token}. Save one from an edit before rejecting it."
        )
    state["rejected"][identifier] = {
        "at": utc_now_iso(),
        "text": proposal["text"],
        "span": proposal["span"],
        "polarity": proposal["polarity"],
        "pair": proposal["pair"],
        "section": proposal.get("section"),
    }
    state["confirmed"].pop(identifier, None)
    save_checks(root, state)
    return {
        "id": identifier,
        "status": "rejected",
        "at": state["rejected"][identifier]["at"],
        "text": proposal["text"],
    }


def resolve_check_id(root: Path, token: str) -> str:
    """Accept a check id or the sentence the writer saved."""
    text = token.strip()
    if not text:
        raise ProjectError("A check id cannot be empty.")
    state = load_checks(root)
    if _known(root, state, text):
        return text
    derived = check_id(text)
    if _known(root, state, derived):
        return derived
    raise ProjectError(
        f"No check {token}. Save one from an edit with "
        "`ghostless learn --check` or `ghostless checks add <slug> --text`."
    )


def checks_summary(root: Path) -> ChecksSummary:
    """Confirmed, still-pending, and rejected checks for the guide and the CLI."""
    state = load_checks(root)
    confirmed_ids = set(state["confirmed"])
    rejected_ids = set(state["rejected"])
    pending: list[dict[str, object]] = []
    seen: set[str] = set()
    for item in _pair_checks(root):
        identifier = str(item["id"])
        if identifier in confirmed_ids or identifier in rejected_ids or identifier in seen:
            continue
        seen.add(identifier)
        pending.append(item)
    confirmed = [
        {"id": identifier, **record}
        for identifier, record in _items(state, "confirmed")
    ]
    rejected = [
        {"id": identifier, "status": "rejected", **record}
        for identifier, record in _items(state, "rejected")
    ]
    confirmed.sort(key=lambda item: str(item["id"]))
    pending.sort(key=lambda item: str(item["id"]))
    rejected.sort(key=lambda item: str(item["id"]))
    return {"confirmed": confirmed, "pending": pending, "rejected": rejected}


def witness_for(text: str, before: str, after: str) -> dict[str, str]:
    """The span this edit added or cut, which the after file already satisfies.

    A quoted span in the check wins. Otherwise the edit must have exactly one
    short phrase judge does not already score. Guessing among several would
    invent the judgment.
    """
    quotes = _quotes(text)
    if len(quotes) > 1:
        raise ProjectError("One check quotes one span. Split the judgments.")
    if quotes:
        span = " ".join(quotes[0].split())
        polarity = _polarity_for_quote(span, before, after)
    else:
        spans = _candidate_spans(before, after)
        if len(spans) == 1:
            polarity, span = spans[0]
        elif not spans:
            raise ProjectError(
                "This edit only changes counts judge already scores, or it has no "
                "short phrase to keep. Quote the span this check is about, in double quotes."
            )
        else:
            preview = ", ".join(f"{kind} {shown!r}" for kind, shown in spans[:4])
            raise ProjectError(
                "This edit changes more than one phrase. Quote the span this check "
                f"is about, in double quotes. Candidates: {preview}."
            )
    if not passes_check(after, polarity, span):
        raise ProjectError(
            "The after file would fail this check, so it cannot be the case that already passes."
        )
    return {"span": span, "polarity": polarity}


def passes_check(text: str, polarity: str, span: str) -> bool:
    """Yes/no on one draft. Exact span, no paraphrase, no model."""
    present = _contains(text, span)
    match polarity:
        case "forbid":
            return not present
        case "require":
            return present
        case _:
            raise ProjectError(f"Unknown check polarity {polarity!r}.")


def grade_confirmed(
    root: Path,
    held: list[dict[str, object]],
    *,
    seed: int,
    skill_path: Path | None = None,
    guide_path: Path | None = None,
) -> dict[str, object]:
    """Grade confirmed checks on held-out befores plus each check's after.

    The result is passes/total. It is not a blended score. A new session id
    is written when the drafting skill, the voice guide, the confirmed set,
    or this holdout changes. Otherwise the same session id is kept and the
    cases are graded again from the files, not copied from the old pass list.
    """
    summary = checks_summary(root)
    confirmed = summary["confirmed"]
    note = (
        "Passes over confirmed checks. Each check runs on the held-out befores "
        "in its section, and on its after file, the case that already passes. "
        "This count is not the mechanical judge score."
    )
    if not confirmed:
        return {
            "passes": 0,
            "total": 0,
            "label": None,
            "session": None,
            "fresh": False,
            "skillSha256": None,
            "skillFiles": {},
            "guideSha256": None,
            "items": [],
            "note": "No confirmed checks yet. A saved check stays inactive until you confirm it.",
        }
    skill_paths = [skill_path] if skill_path is not None else draft_skill_paths()
    skill_files = {str(path.resolve()): _sha256(path) for path in skill_paths}
    guide = guide_path if guide_path is not None else root / "voice-guide.md"
    skill_sha = _skill_sha256(skill_files)
    guide_sha = _sha256(guide)
    held_slugs = [str(item.get("slug")) for item in held if item.get("slug")]
    cases = [
        {
            "slug": item.get("slug"),
            "section": item.get("section"),
            "beforeSha256": _text_sha256(str(item.get("before", ""))),
        }
        for item in held
    ]
    after_hashes = {
        str(item["pair"]): _text_sha256(_read_after(root, str(item["pair"])))
        for item in confirmed if item.get("pair")
    }
    fingerprint = _fingerprint(skill_sha, guide_sha, seed, cases, after_hashes, confirmed)
    items, skipped, missing_afters = _grade_items(root, confirmed, held)
    passes = sum(1 for item in items if item["pass"])
    total = len(items)
    if skipped:
        note = f"{note} {skipped} confirmed check(s) had no span and were not graded."
    if missing_afters:
        note = f"{note} {missing_afters} confirmed check(s) had no readable after file."
    session_id = fingerprint
    fresh = _write_grade(
        root,
        session_id,
        {
            "kind": "ghostless-check-grade",
            "version": 1,
            "id": session_id,
            "skillSha256": skill_sha,
            "skillFiles": skill_files,
            "guideSha256": guide_sha,
            "seed": seed,
            "holdout": held_slugs,
            "holdoutCases": cases,
            "afterSha256": after_hashes,
            "passes": passes,
            "total": total,
            "label": f"{passes}/{total}" if total else None,
            "items": items,
        },
    )
    return {
        "passes": passes,
        "total": total,
        "label": f"{passes}/{total}" if total else None,
        "session": session_id,
        "fresh": fresh,
        "skillSha256": skill_sha,
        "skillFiles": skill_files,
        "guideSha256": guide_sha,
        "items": items,
        "note": note,
    }


def draft_skill_path() -> Path | None:
    root = suite_root()
    if root is None:
        return None
    path = root / "skills" / "ghostless-draft" / "SKILL.md"
    return path if path.is_file() else None


def draft_skill_paths() -> list[Path]:
    """Packaged skill and installed copies, since eval cannot infer the host.

    Include every installed copy so changing any host's instructions starts
    a new session. GHOSTLESS_SKILLS_DIR covers custom installation parents.
    """
    source = draft_skill_path()
    found = {source.resolve()} if source is not None else set()
    custom = os.environ.get("GHOSTLESS_SKILLS_DIR")
    parents = [Path(custom).expanduser()] if custom else [agent_dir(name) for name in AGENTS]
    for parent in parents:
        folder = parent / "ghostless-draft"
        path = folder / "SKILL.md"
        marker = folder / COPY_MARKER
        if not path.is_file():
            continue
        try:
            owned_copy = marker.read_text(encoding="utf-8").strip() == "ghostless:ghostless-draft"
        except (OSError, UnicodeError):
            owned_copy = False
        if folder.is_symlink() or owned_copy:
            found.add(path.resolve())
    return sorted(found)


def _skill_sha256(files: dict[str, str | None]) -> str | None:
    if not files:
        return None
    if len(files) == 1:
        return next(iter(files.values()))
    return _text_sha256(json.dumps(files, sort_keys=True))


def _text_sha256(text: str | None) -> str | None:
    return sha256_text(text) if text is not None else None


def empty_confirmed_checks(note: str) -> dict[str, object]:
    return {
        "passes": 0,
        "total": 0,
        "label": None,
        "session": None,
        "fresh": False,
        "skillSha256": None,
        "skillFiles": {},
        "guideSha256": None,
        "items": [],
        "note": note,
    }


def _grade_items(
    root: Path,
    confirmed: list[dict[str, object]],
    held: list[dict[str, object]],
) -> tuple[list[dict[str, object]], int, int]:
    items: list[dict[str, object]] = []
    skipped = 0
    missing_afters = 0
    for check in confirmed:
        span = check.get("span")
        polarity = check.get("polarity")
        if not isinstance(span, str) or not span or polarity not in ("forbid", "require"):
            skipped += 1
            continue
        pair = check.get("pair")
        text = check.get("text")
        identifier = check.get("id")
        raw_section = check.get("section")
        section = raw_section if isinstance(raw_section, str) else None
        if isinstance(pair, str) and pair:
            after = _read_after(root, pair)
            if after is None:
                missing_afters += 1
            else:
                items.append(
                    _grade_row(
                        check_id=str(identifier),
                        text=str(text),
                        slug=pair,
                        case="after",
                        passed=passes_check(after, str(polarity), span),
                    )
                )
        for held_item in held:
            slug = held_item.get("slug")
            before = held_item.get("before")
            if not isinstance(slug, str) or not isinstance(before, str):
                continue
            if not _in_scope(section, held_item.get("section")):
                continue
            items.append(
                _grade_row(
                    check_id=str(identifier),
                    text=str(text),
                    slug=slug,
                    case="before",
                    passed=passes_check(before, str(polarity), span),
                )
            )
    items.sort(key=lambda item: (str(item["check"]), str(item["slug"]), str(item["case"])))
    return items, skipped, missing_afters


def _grade_row(
    *,
    check_id: str,
    text: str,
    slug: str,
    case: str,
    passed: bool,
) -> dict[str, object]:
    return {
        "check": check_id,
        "text": text,
        "slug": slug,
        "case": case,
        "pass": passed,
    }


def _in_scope(check_section: str | None, pair_section: object) -> bool:
    if not check_section:
        return True
    return pair_section == check_section


def _read_after(root: Path, slug: str) -> str | None:
    path = root / "gold" / slug / "after.md"
    if not path.is_file():
        return None
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return None


def _fingerprint(
    skill_sha: str | None,
    guide_sha: str | None,
    seed: int,
    cases: list[dict[str, object]],
    after_hashes: dict[str, str | None],
    confirmed: list[dict[str, object]],
) -> str:
    payload = {
        "skill": skill_sha,
        "guide": guide_sha,
        "seed": seed,
        "holdout": cases,
        "afters": after_hashes,
        "confirmed": [
            {
                "id": item.get("id"),
                "span": item.get("span"),
                "polarity": item.get("polarity"),
                "section": item.get("section"),
                "pair": item.get("pair"),
            }
            for item in confirmed
        ],
    }
    raw = json.dumps(payload, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()[:16]


def _write_grade(root: Path, session_id: str, payload: dict[str, object]) -> bool:
    """Write the grade session. Return True when this id is new."""
    folder = root / "stats" / "check-grades"
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / f"{session_id}.json"
    fresh = not path.is_file()
    graded_at = utc_now_iso()
    if not fresh:
        try:
            previous = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            previous = None
        if isinstance(previous, dict) and isinstance(previous.get("gradedAt"), str):
            graded_at = previous["gradedAt"]
    payload["gradedAt"] = graded_at
    atomic_write_json(path, payload, ensure_ascii=False)
    return fresh


def _sha256(path: Path | None) -> str | None:
    if path is None or not path.is_file():
        return None
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _polarity_for_quote(span: str, before: str, after: str) -> str:
    if not _usable_span(span, quoted=True):
        if _judge_already_sees(span):
            raise ProjectError(
                f"{span!r} is a tell judge already scores. "
                "A check is for a judgment those counts cannot see."
            )
        raise ProjectError(
            f"{span!r} is not a span this check can grade. "
            "Quote a short phrase the edit added or cut."
        )
    in_before = _contains(before, span)
    in_after = _contains(after, span)
    if in_before and not in_after:
        return "forbid"
    if in_after and not in_before:
        return "require"
    if in_before and in_after:
        raise ProjectError(
            f"{span!r} is in both the before and the after, so the edit did not decide it."
        )
    raise ProjectError(
        f"{span!r} is not in this edit. Quote a span from the before or the after."
    )


def _candidate_spans(before: str, after: str) -> list[tuple[str, str]]:
    before_words = _prose_words(before)
    after_words = _prose_words(after)
    matcher = difflib.SequenceMatcher(
        None,
        [word.casefold() for word in before_words],
        [word.casefold() for word in after_words],
        autojunk=False,
    )
    spans: list[tuple[str, str]] = []
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag in ("delete", "replace"):
            span = _surface(before, before_words[i1:i2])
            if span and _usable_span(span, quoted=False) and _contains(before, span) and not _contains(after, span):
                spans.append(("forbid", span))
        if tag in ("insert", "replace"):
            span = _surface(after, after_words[j1:j2])
            if span and _usable_span(span, quoted=False) and _contains(after, span) and not _contains(before, span):
                spans.append(("require", span))
    unique: list[tuple[str, str]] = []
    seen: set[tuple[str, str]] = set()
    for item in spans:
        if item in seen:
            continue
        seen.add(item)
        unique.append(item)
    return unique


def _surface(text: str, tokens: list[str]) -> str | None:
    if not tokens:
        return None
    pattern = (
        rf"(?i)(?<!{_WORD_EDGE})"
        + r"\W+".join(re.escape(token) for token in tokens)
        + rf"(?!{_WORD_EDGE})"
    )
    match = re.search(pattern, _body(text))
    if match is None:
        return " ".join(tokens)
    return " ".join(match.group(0).split())


def _usable_span(span: str, *, quoted: bool) -> bool:
    if _judge_already_sees(span):
        return False
    tokens = word_tokens(span)
    if not tokens or len(span) > MAX_SPAN_CHARS or len(tokens) > MAX_SPAN_TOKENS:
        return False
    content = [
        token
        for token in tokens
        if any(character.isdigit() for character in token)
        or (token not in STOPWORDS and len(token) >= 3)
    ]
    if not content:
        return False
    if quoted:
        return True
    return len(tokens) >= 2 or any(any(character.isdigit() for character in token) for token in tokens)


def _judge_already_sees(span: str) -> bool:
    folded = " ".join(normalize(span).split()).casefold().strip(_TRAILING_PUNCT)
    if folded in TELLS:
        return True
    tokens = word_tokens(folded)
    return len(tokens) == 1 and tokens[0] in TELLS


def _quotes(text: str) -> list[str]:
    found: list[str] = []
    for match in QUOTE_RE.finditer(text):
        span = match.group(1)
        if span:
            found.append(span.strip())
    return found


def _contains(text: str, span: str) -> bool:
    """Whether the span's words are in the prose, in order.

    A line break, a tab, or a run of spaces between those words is still the
    span. An apostrophe is part of the word, so `writer` is not `writer's`.
    """
    parts = [re.escape(part) for part in normalize(span).split()]
    if not parts:
        return False
    pattern = rf"(?<!{_WORD_EDGE})" + r"\s+".join(parts) + rf"(?!{_WORD_EDGE})"
    return re.search(pattern, _body(text), re.IGNORECASE) is not None


def _body(text: str) -> str:
    """Prose a check can see.

    Measurement drops an entire ATX heading. The heading is still the edit,
    so keep its words and then strip the other non-prose.
    """
    return strip_non_prose(_HEADING_MARK.sub("", text))


def _prose_words(text: str) -> list[str]:
    return WORD_RE.findall(_body(text))


def _pair_checks(root: Path) -> list[dict[str, object]]:
    found: list[dict[str, object]] = []
    folder = root / "gold"
    if not folder.is_dir():
        return found
    for meta_path in sorted(folder.glob("*/meta.json")):
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            continue
        if not isinstance(meta, dict):
            continue
        raw = meta.get("checks")
        if not isinstance(raw, list):
            continue
        section = meta.get("section") if isinstance(meta.get("section"), str) and meta.get("section") else None
        for item in raw:
            parsed = _parse_pair_check(item, pair=meta_path.parent.name, section=section)
            if parsed is not None:
                found.append(parsed)
    return found


def _parse_pair_check(
    item: object,
    *,
    pair: str,
    section: str | None,
) -> dict[str, object] | None:
    if not isinstance(item, dict):
        return None
    identifier = item.get("id")
    text = item.get("text")
    span = item.get("span")
    polarity = item.get("polarity")
    if not all(isinstance(value, str) and value for value in (identifier, text, span)):
        return None
    if polarity not in ("forbid", "require"):
        return None
    return {
        "id": identifier,
        "text": text,
        "span": span,
        "polarity": polarity,
        "pair": pair,
        "section": section,
    }


def _find_pending(root: Path, identifier: str) -> dict[str, object] | None:
    for item in _pair_checks(root):
        if item.get("id") == identifier:
            return item
    return None


def _proposal(root: Path, state: ChecksState, identifier: str) -> dict[str, object] | None:
    for key in ("confirmed", "rejected"):
        record = _record(state, key, identifier)
        if record is not None and isinstance(record.get("text"), str) and isinstance(record.get("span"), str):
            section = record.get("section") if isinstance(record.get("section"), str) else None
            pair = record.get("pair") if isinstance(record.get("pair"), str) else ""
            return {
                "text": record["text"],
                "span": record["span"],
                "polarity": record.get("polarity"),
                "pair": pair,
                "section": section,
            }
    return _find_pending(root, identifier)


def _known(root: Path, state: ChecksState, identifier: str) -> bool:
    if any(_record(state, key, identifier) is not None for key in ("confirmed", "rejected")):
        return True
    return _find_pending(root, identifier) is not None


def _bucket(state: ChecksState, key: Literal["confirmed", "rejected"]) -> dict[str, dict[str, object]]:
    return state["confirmed"] if key == "confirmed" else state["rejected"]


def _record(state: ChecksState, key: Literal["confirmed", "rejected"], identifier: str) -> dict[str, object] | None:
    return _bucket(state, key).get(identifier)


def _items(state: ChecksState, key: Literal["confirmed", "rejected"]) -> list[tuple[str, dict[str, object]]]:
    return list(_bucket(state, key).items())


def _checked_section(section: str | None) -> str | None:
    if section is None:
        return None
    text = section.strip()
    if not text:
        raise ProjectError("--section cannot be empty.")
    return text
