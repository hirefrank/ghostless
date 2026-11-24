"""Local, line-delimited JSON interface for the desktop editor."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Never

from ghostless import build, checks, judge, learn, project, rules, sessions
from ghostless.paths import ProjectError, find_project_with_source, version
from ghostless.schema import CompiledStats


def _required_text(params: dict[str, object], name: str, *, default: str | None = None) -> str:
    value = params.get(name, default)
    if not isinstance(value, str):
        raise ProjectError(f"Parameter {name!r} must be a string.")
    return value


def _optional_text(params: dict[str, object], name: str) -> str | None:
    return None if params.get(name) is None else _required_text(params, name)


def _strings(params: dict[str, object], name: str) -> list[str]:
    value = params.get(name, [])
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ProjectError(f"Parameter {name!r} must be a list of strings.")
    return list(value)


def _flag(params: dict[str, object], name: str) -> bool:
    value = params.get(name, False)
    if not isinstance(value, bool):
        raise ProjectError(f"Parameter {name!r} must be a boolean.")
    return value


def _read_object(path: Path) -> dict[str, object]:
    try:
        data: object = json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, UnicodeError) as exc:
        raise ProjectError(f"Cannot read {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise ProjectError(f"{path} must contain a JSON object.")
    return data


def _resolve_project(params: dict[str, object]) -> dict[str, object]:
    path = _optional_text(params, "path")
    workspace = Path(path).expanduser() if path else None
    candidates = (workspace, workspace / "voice") if workspace is not None else ()
    for candidate in candidates if workspace is not None else (None,):
        try:
            root, source = find_project_with_source(candidate)
        except (ProjectError, AttributeError):
            # A non-object manifest also cannot identify a ghostless project.
            continue
        return {"project": str(root), "source": source}
    return {"project": None, "source": None}


def _confirmed_checks(root: Path, text: str, section: str | None) -> list[dict[str, object]]:
    results: list[dict[str, object]] = []
    for check in checks.checks_summary(root)["confirmed"]:
        scope = check.get("section")
        if scope is not None and scope != section:
            continue
        span = check.get("span")
        polarity = check.get("polarity")
        if not isinstance(span, str) or not isinstance(polarity, str):
            raise ProjectError(f"Confirmed check {check['id']} needs a span and polarity.")
        results.append({
            "id": check["id"],
            "text": check.get("text"),
            "span": span,
            "polarity": polarity,
            "section": scope,
            "pass": checks.passes_check(text, polarity, span),
        })
    return results


def _list_sessions(root: Path) -> list[dict[str, object]]:
    items = sessions.list_sessions(root)
    for item in items:
        folder = root / str(item["path"])
        metadata = _read_object(folder / "session.json")
        item.update(section=metadata.get("section"), origin=metadata.get("origin"))
        try:
            receipt = _read_object(folder / "receipt.json")
        except FileNotFoundError:
            continue
        changes = receipt.get("changes")
        item.update(
            editRate=changes.get("editRate") if isinstance(changes, dict) else None,
            learnedPath=receipt.get("learnedPath"),
        )
    return items


def _decide_rule(root: Path, params: dict[str, object]) -> dict[str, object]:
    action = _required_text(params, "action")
    identifier = _required_text(params, "ruleId")
    section = _optional_text(params, "section")
    if action == "confirm":
        return rules.confirm_rule(root, identifier, section=section)
    if section is not None:
        raise ProjectError("Parameter 'section' applies to confirm.")
    if action == "reject":
        return rules.reject_rule(root, identifier)
    if action == "retire":
        return rules.retire_rule(root, identifier)
    raise ProjectError(f"Unknown rule action {action!r}.")


def _decide_check(root: Path, params: dict[str, object]) -> object:
    action = _required_text(params, "action")
    target = _required_text(params, "target")
    section = _optional_text(params, "section")
    if action == "confirm":
        return checks.confirm_check(root, target, section=section)
    if section is not None:
        raise ProjectError("Parameter 'section' applies to confirm.")
    if action == "reject":
        return checks.reject_check(root, target)
    if action == "add":
        if not target or target in {".", ".."} or "/" in target or "\\" in target:
            raise ProjectError(f"Invalid learned pair slug: {target!r}")
        gold = root / "gold"
        project.require_contained_path(gold, gold / target, label="slug")
        return checks.write_pair_checks(root, target, [_required_text(params, "text")])
    raise ProjectError(f"Unknown check action {action!r}.")


class _Worker:
    def __init__(self) -> None:
        self._stats: dict[Path, tuple[int, dict[str, object]]] = {}

    def handle(self, request: dict[str, object]) -> dict[str, object]:
        identifier = request.get("id")
        try:
            method = _required_text(request, "method")
            params = request.get("params", {})
            if not isinstance(params, dict):
                raise ProjectError("Parameter 'params' must be an object.")
            result = self._dispatch(method, params)
        except (ProjectError, OSError, ValueError) as exc:
            return {"id": identifier, "ok": False, "error": str(exc)}
        return {"id": identifier, "ok": True, "result": result}

    def _dispatch(self, method: str, params: dict[str, object]) -> object:
        def root() -> Path:
            path = Path(_required_text(params, "project"))
            try:
                return find_project_with_source(path)[0]
            except AttributeError as exc:
                raise ProjectError(f"Invalid project manifest at {path / 'manifest.json'}.") from exc

        match method:
            case "ping":
                return {"version": version()}
            case "resolve_project":
                return _resolve_project(params)
            case "judge_text":
                return self._judge(root(), params)
            case "sessions_list":
                return _list_sessions(root())
            case "session_start":
                return sessions.start_session(
                    root(),
                    Path(_required_text(params, "draft")).expanduser(),
                    section=_optional_text(params, "section"),
                    origin=_required_text(params, "origin", default="ai-first"),
                    sources=_strings(params, "sources"),
                )
            case "session_snapshot":
                return sessions.snapshot_session(
                    root(),
                    session=_required_text(params, "session"),
                    after=_required_text(params, "after"),
                    notes=_required_text(params, "notes"),
                )
            case "session_finish":
                return self._finish(root(), params)
            case "build":
                stats = self._build(root())
                return {
                    "goldPieces": stats["goldPieces"],
                    "counts": stats["counts"],
                    "bronzeFormatOnly": stats["bronzeFormatOnly"],
                    "warnings": stats["warnings"],
                    "generatedAt": stats["generatedAt"],
                }
            case "rules_list":
                return rules.rules_summary(root())
            case "rules_decide":
                return _decide_rule(root(), params)
            case "checks_list":
                return checks.checks_summary(root())
            case "checks_decide":
                return _decide_check(root(), params)
            case "save_note":
                saved = project.save_note(
                    root(), kind=_required_text(params, "kind"), text=_required_text(params, "text"),
                )
                return {**saved, "path": str(saved["path"])}
            case "promote":
                return learn.promote_final(root(), _required_text(params, "slug"), gold=not _flag(params, "demote"))
            case _:
                raise ProjectError(f"Unknown method {method!r}.")

    def _build(self, root: Path) -> CompiledStats:
        self._stats.pop(root, None)
        return build.build_voice(root)

    def _judge(self, root: Path, params: dict[str, object]) -> dict[str, object]:
        text = _required_text(params, "text")
        section = _optional_text(params, "section")
        path = root / "voice-stats.json"
        try:
            mtime = path.stat().st_mtime_ns
            cached = self._stats.get(root)
            if cached is None or cached[0] != mtime:
                cached = (mtime, _read_object(path))
                self._stats[root] = cached
        except FileNotFoundError:
            self._stats.pop(root, None)
            return {"needsBuild": True, "error": "No voice-stats.json yet. Run build first."}

        stats = cached[1]
        manifest = project.load_manifest(root)
        report = judge.judge_text(
            text, stats, section=section,
            fail_under=manifest["failUnder"], overrides=manifest["overrides"], placeholder=manifest["placeholder"],
        )
        stale = build.stale_inputs(root)
        sections = stats.get("sections")
        return {
            **report,
            "stale": bool(stale),
            "staleSources": stale,
            "availableSections": sorted(sections) if isinstance(sections, dict) else [],
            "confirmedChecks": _confirmed_checks(root, text, section),
        }

    def _finish(self, root: Path, params: dict[str, object]) -> dict[str, object]:
        final = _optional_text(params, "final")
        rebuild = _flag(params, "rebuild")
        promote = _flag(params, "promote")
        receipt = sessions.finish_session(
            root,
            Path(final).expanduser() if final is not None else None,
            session=_required_text(params, "session"),
            note=_optional_text(params, "note"),
            checks=_strings(params, "checks"),
        )
        result = dict(receipt)
        learned = receipt.get("learnedPath")
        if promote and receipt.get("origin") == "ai-first" and isinstance(learned, str) and learned.startswith("gold/"):
            result["promotion"] = learn.promote_final(root, learned.removeprefix("gold/"), gold=True)
        if rebuild:
            self._build(root)
            result["rebuilt"] = True
        return result


def handle_request(request: dict[str, object]) -> dict[str, object]:
    """Process one decoded request without retaining state across calls."""
    return _Worker().handle(request)


def _reject_constant(value: str) -> Never:
    raise ValueError(f"Invalid JSON constant: {value}")


def serve_stdio() -> int:
    """Read UTF-8 requests until EOF and flush each UTF-8 response."""
    worker = _Worker()
    for line in sys.stdin.buffer:
        response: dict[str, object]
        try:
            request = json.loads(line.decode("utf-8"), parse_constant=_reject_constant)
            if not isinstance(request, dict):
                raise ValueError("Request must be a JSON object.")
        except (ValueError, RecursionError) as exc:
            response = {"id": None, "ok": False, "error": str(exc)}
        else:
            response = worker.handle(request)
        try:
            encoded = json.dumps(response, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
        except (ValueError, RecursionError) as exc:
            encoded = json.dumps({"id": None, "ok": False, "error": str(exc)}, separators=(",", ":"))
        sys.stdout.buffer.write((encoded + "\n").encode("utf-8", errors="backslashreplace"))
        sys.stdout.buffer.flush()
    return 0
