"""Portable, local edit sessions shared by the CLI and desktop editor.

A session captures an immutable original (`before.md`), a recoverable working
copy (`after.md`), optional writer notes, and — on finish — an edit receipt and
a learned gold pair. The id follows the `learn` slug scheme
(`YYYY-MM-DD[-section][-N]`), so a session and its learned pair share a name.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import tempfile
from datetime import datetime, timezone
from pathlib import Path, PureWindowsPath

from ghostless.learn import diff_texts, learn_edit
from ghostless.paths import ProjectError
from ghostless.project import (
    new_session_id,
    parse_origin,
    placement_for,
    read_provenance,
)

# Session ids are `YYYY-MM-DD[-section][-N]`. The section and counter are
# lowercase slugs of `[a-z0-9-]`, so this also rejects path traversal.
SESSION_ID_RE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}(?:-[a-z0-9]+)*\Z")


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _atomic_write(path: Path, text: str) -> None:
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


def _write_json(path: Path, value: dict[str, object]) -> None:
    _atomic_write(path, json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def _load_json(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ProjectError(f"Cannot read session metadata at {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ProjectError(f"Invalid session metadata at {path}.")
    return value


def _session_dir(root: Path, session: str | None) -> Path:
    if session:
        if not SESSION_ID_RE.fullmatch(session):
            raise ProjectError("--session must be an ID printed by `ghostless start`.")
        folder = root / "sessions" / session
        if not (folder / "session.json").is_file():
            raise ProjectError(f"Session {session} not found in {root}.")
        return folder
    active = sorted(
        folder
        for folder in (root / "sessions").glob("*")
        if (folder / "session.json").is_file() and not (folder / "receipt.json").exists()
    )
    if len(active) != 1:
        raise ProjectError(
            f"Found {len(active)} unfinished sessions. Pass --session with an ID "
            "from `ghostless sessions`."
        )
    return active[0]


def start_session(
    root: Path,
    draft: Path,
    *,
    section: str | None,
    origin: str,
    sources: list[str],
) -> dict[str, object]:
    if not draft.is_file():
        raise ProjectError(f"Draft file not found: {draft}")
    text = draft.read_text(encoding="utf-8")
    if not text.strip():
        raise ProjectError(f"Draft is empty: {draft}")
    checked_origin = parse_origin(origin, path="--origin")
    if any(not source.strip() for source in sources):
        raise ProjectError("--source values cannot be empty.")
    if any(
        Path(source).is_absolute() or PureWindowsPath(source).is_absolute() for source in sources
    ):
        raise ProjectError("--source must be a portable reference, URL, or project-relative path.")
    date_text = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    session_id = new_session_id(root, date=date_text, section=section)
    folder = root / "sessions" / session_id
    try:
        folder.mkdir(parents=True, exist_ok=False)
    except FileExistsError as exc:
        raise ProjectError(f"Session {session_id} already exists. Start again.") from exc
    _atomic_write(folder / "before.md", text)
    _atomic_write(folder / "after.md", text)
    metadata: dict[str, object] = {
        "kind": "ghostless-edit-session",
        "version": 1,
        "id": session_id,
        "createdAt": _now(),
        "section": section,
        "origin": checked_origin,
        "sources": sources,
        "sourceDraftName": draft.name,
        "status": "open",
        "beforeFile": "before.md",
        "afterFile": "after.md",
        "notesFile": "notes.md",
        "receiptFile": "receipt.json",
        "beforeSha256": _hash(text),
    }
    _write_json(folder / "session.json", metadata)
    return {"id": session_id, "path": folder.relative_to(root).as_posix(), **metadata}


def list_sessions(root: Path) -> list[dict[str, object]]:
    found: list[dict[str, object]] = []
    for path in sorted((root / "sessions").glob("*/session.json")):
        metadata = _load_json(path)
        found.append(
            {
                "id": metadata.get("id"),
                "createdAt": metadata.get("createdAt"),
                "sourceDraftName": metadata.get("sourceDraftName"),
                "status": "finished" if (path.parent / "receipt.json").is_file() else "editing",
                "path": path.parent.relative_to(root).as_posix(),
            }
        )
    return found


def snapshot_session(root: Path, *, session: str, after: str, notes: str) -> dict[str, object]:
    folder = _session_dir(root, session)
    if (folder / "receipt.json").exists():
        raise ProjectError("This session is already finished. Start a new session to edit again.")
    metadata = _load_json(folder / "session.json")
    if _hash((folder / "before.md").read_text(encoding="utf-8")) != metadata.get("beforeSha256"):
        raise ProjectError(f"The immutable baseline has changed in {folder}.")
    _atomic_write(folder / "after.md", after)
    _atomic_write(folder / "notes.md", notes)
    return {"id": session, "path": folder.relative_to(root).as_posix(), "savedAt": _now()}


def finish_session(
    root: Path,
    final: Path | None,
    *,
    session: str | None,
    note: str | None = None,
) -> dict[str, object]:
    folder = _session_dir(root, session)
    metadata = _load_json(folder / "session.json")
    if metadata.get("kind") != "ghostless-edit-session" or metadata.get("version") != 1:
        raise ProjectError(f"Unsupported session format in {folder}.")
    before = (folder / "before.md").read_text(encoding="utf-8")
    if _hash(before) != metadata.get("beforeSha256"):
        raise ProjectError(f"The immutable baseline has changed in {folder}.")
    source = final or folder / "after.md"
    if not source.is_file():
        raise ProjectError(f"Final file not found: {source}")
    after = source.read_text(encoding="utf-8")
    if not after.strip():
        raise ProjectError(f"Final file is empty: {source}")
    receipt_path = folder / "receipt.json"
    if receipt_path.is_file():
        receipt = _load_json(receipt_path)
        if receipt.get("finalSha256") == _hash(after):
            return receipt
        raise ProjectError("This session is already finished with a different final. Start a new session.")
    if before.rstrip("\n") == after.rstrip("\n"):
        raise ProjectError("The final is identical to the original draft. Make an edit before finishing.")
    if note is not None:
        _atomic_write(folder / "notes.md", note)
    notes_path = folder / "notes.md"
    notes = notes_path.read_text(encoding="utf-8") if notes_path.is_file() else ""
    # Keep the session self-contained: after.md is the final, whatever its source.
    _atomic_write(folder / "after.md", after)
    existing = _existing_learn(root, before, after)
    if existing:
        learned_path = existing
        tier, confirmation = _existing_placement(root, existing)
    else:
        result = learn_edit(
            root,
            before=folder / "before.md",
            after=folder / "after.md",
            section=metadata.get("section") if isinstance(metadata.get("section"), str) else None,
            origin=str(metadata.get("origin")),
        )
        learned_path = str(result["path"])
        tier = result.get("tier")
        confirmation = result.get("confirmation")
    receipt: dict[str, object] = {
        "kind": "ghostless-edit-receipt",
        "version": 1,
        "sessionId": metadata["id"],
        "finishedAt": _now(),
        "origin": metadata["origin"],
        "section": metadata.get("section"),
        "sources": metadata.get("sources", []),
        "sourceDraftName": metadata.get("sourceDraftName"),
        "beforeSha256": metadata["beforeSha256"],
        "finalSha256": _hash(after),
        "beforeFile": "before.md",
        "finalFile": "after.md",
        "notesFile": "notes.md" if notes else None,
        "notesSha256": _hash(notes) if notes else None,
        "learnedPath": learned_path,
        "tier": tier,
        "confirmation": confirmation,
        "changes": diff_texts(before, after),
    }
    _write_json(receipt_path, receipt)
    metadata["status"] = "final"
    _write_json(folder / "session.json", metadata)
    return receipt


def _existing_learn(root: Path, before: str, after: str) -> str | None:
    for meta_path in sorted((root / "gold").glob("*/meta.json")):
        folder = meta_path.parent
        if not (folder / "before.md").is_file() or not (folder / "after.md").is_file():
            continue
        if (
            (folder / "before.md").read_text(encoding="utf-8").rstrip("\n") == before.rstrip("\n")
            and (folder / "after.md").read_text(encoding="utf-8").rstrip("\n")
            == after.rstrip("\n")
        ):
            return folder.relative_to(root).as_posix()
    return None


def _existing_placement(root: Path, relative: str) -> tuple[str | None, str | None]:
    meta = _load_json(root / relative / "meta.json")
    provenance = read_provenance(meta, path=str(root / relative / "meta.json"))
    return placement_for(provenance), provenance.confirmation
