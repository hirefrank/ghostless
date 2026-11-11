"""Voice-project layout: manifest, source discovery, init."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Literal, Never

from ghostless.paths import ProjectError
from ghostless.stats import DEFAULT_WEIGHTS, Kind, Piece, Tier
from ghostless.text import split_frontmatter

TEXT_SUFFIXES = {".md", ".markdown", ".txt"}
SKIP_NAMES = {"readme.md", ".gitkeep"}
GITIGNORE_MARKER = "# ghostless: local corpus (do not commit)"
GITIGNORE_BLOCK = """# ghostless: local corpus (do not commit)
# Your writing stays on this machine. The guide is derived from it.
corpus/
transcripts/
gold/
voice-stats.json
voice-guide.md
voice-guide.paste.md
edits-summary.md
edits-summary.json
"""

Origin = Literal["ai-first", "writer-first"]


def parse_origin(value: str, *, path: str) -> Origin:
    match value:
        case "ai-first" | "writer-first":
            return value
        case _:
            raise ProjectError(
                f"{path}: origin must be 'ai-first' or 'writer-first', got {value!r}"
            )


def _never(value: Never) -> Never:
    raise RuntimeError(f"unhandled variant: {value!r}")


def default_manifest() -> dict[str, object]:
    return {
        "kind": "ghostless",
        "version": 1,
        "weights": {"gold": DEFAULT_WEIGHTS["gold"], "silver": DEFAULT_WEIGHTS["silver"], "bronze": DEFAULT_WEIGHTS["bronze"]},
        "goldVoiceMinimum": 10,
        "updateCheck": True,
        "failUnder": 70,
    }


def load_manifest(root: Path) -> dict[str, object]:
    path = root / "manifest.json"
    if not path.is_file():
        raise ProjectError(f"Missing manifest.json in {root}. Run `ghostless init`.")
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ProjectError(f"{path}: {exc}") from exc
    if not isinstance(raw, dict) or raw.get("kind") != "ghostless":
        raise ProjectError(f"{path} is not a ghostless manifest (kind must be \"ghostless\").")
    weights = dict(DEFAULT_WEIGHTS)
    custom = raw.get("weights") or {}
    if not isinstance(custom, dict):
        raise ProjectError(f"{path}: weights must be an object.")
    for key in ("gold", "silver", "bronze"):
        if key not in custom:
            continue
        try:
            number = float(custom[key])
        except (TypeError, ValueError) as exc:
            raise ProjectError(f"{path}: weights.{key} must be a number.") from exc
        if number <= 0:
            raise ProjectError(f"{path}: weights.{key} must be positive.")
        match key:
            case "gold" | "silver" | "bronze":
                weights[key] = number
            case _ as unexpected:
                _never(unexpected)
    minimum = _positive_int(raw.get("goldVoiceMinimum", 10), path=path, field="goldVoiceMinimum")
    fail_under = raw.get("failUnder", 70)
    try:
        fail_under_int = int(fail_under)
    except (TypeError, ValueError) as exc:
        raise ProjectError(f"{path}: failUnder must be an integer.") from exc
    if not 0 <= fail_under_int <= 100:
        raise ProjectError(f"{path}: failUnder must be between 0 and 100.")
    update_check = raw.get("updateCheck", True)
    if not isinstance(update_check, bool):
        raise ProjectError(f"{path}: updateCheck must be true or false.")
    pieces = raw.get("pieces") or []
    if not isinstance(pieces, list):
        raise ProjectError(f"{path}: pieces must be a list.")
    return {
        "root": root,
        "weights": weights,
        "goldVoiceMinimum": minimum,
        "failUnder": fail_under_int,
        "updateCheck": update_check,
        "pieces": pieces,
    }


def _positive_int(value: object, *, path: Path, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, (int, float, str)):
        raise ProjectError(f"{path}: {field} must be an integer.")
    try:
        number = int(value)
    except ValueError as exc:
        raise ProjectError(f"{path}: {field} must be an integer.") from exc
    if number < 1:
        raise ProjectError(f"{path}: {field} must be at least 1.")
    return number


def init_project(directory: Path, *, force: bool = False) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    for relative in (
        "corpus/gold",
        "corpus/silver",
        "corpus/bronze",
        "corpus/spoken",
        "transcripts",
        "gold",
    ):
        folder = directory / relative
        folder.mkdir(parents=True, exist_ok=True)
        keep = folder / ".gitkeep"
        if not keep.exists():
            keep.write_text("", encoding="utf-8")
    manifest_path = directory / "manifest.json"
    if manifest_path.exists() and not force:
        raise ProjectError(
            f"{manifest_path} already exists. Pass --force to rewrite the manifest defaults."
        )
    manifest_path.write_text(
        json.dumps(default_manifest(), indent=2) + "\n",
        encoding="utf-8",
    )
    _ensure_gitignore(directory / ".gitignore")
    return directory


def _ensure_gitignore(path: Path) -> None:
    existing = path.read_text(encoding="utf-8") if path.exists() else ""
    if GITIGNORE_MARKER in existing:
        return
    prefix = existing
    if prefix and not prefix.endswith("\n"):
        prefix += "\n"
    if prefix and not prefix.endswith("\n\n"):
        prefix += "\n"
    path.write_text(prefix + GITIGNORE_BLOCK, encoding="utf-8")


def discover(root: Path, manifest: dict[str, object]) -> list[Piece]:
    weights = manifest["weights"]
    if not isinstance(weights, dict):
        raise ProjectError("manifest weights are missing.")
    pieces: list[Piece] = []
    specs: list[tuple[Path, Tier, Kind]] = [
        (root / "corpus" / "gold", "gold", "prose"),
        (root / "corpus" / "silver", "silver", "prose"),
        (root / "corpus" / "bronze", "bronze", "prose"),
        (root / "corpus" / "spoken", "silver", "spoken"),
        (root / "transcripts", "silver", "spoken"),
    ]
    for folder, tier, kind in specs:
        for path in _iter_texts(folder):
            pieces.append(
                _piece_from_file(
                    root,
                    path,
                    default_tier=tier,
                    default_kind=kind,
                    weights=weights,
                )
            )
    pieces.extend(_learned_pieces(root, weights))
    pieces = _apply_overrides(root, pieces, manifest.get("pieces") or [], weights)
    pieces = [piece for piece in pieces if piece.text.strip()]
    pieces.sort(key=lambda piece: (piece.tier, piece.kind, piece.path))
    return pieces


def _iter_texts(folder: Path) -> list[Path]:
    if not folder.is_dir():
        return []
    found: list[Path] = []
    for path in folder.rglob("*"):
        if not path.is_file():
            continue
        if path.name.startswith(".") or path.name.lower() in SKIP_NAMES:
            continue
        if path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        found.append(path)
    return sorted(found)


def _piece_from_file(
    root: Path,
    path: Path,
    *,
    default_tier: Tier,
    default_kind: Kind,
    weights: dict[str, float],
    learned: bool = False,
    origin: str | None = None,
    section_hint: str | None = None,
) -> Piece:
    try:
        raw = path.read_text(encoding="utf-8")
    except UnicodeError as exc:
        raise ProjectError(f"{path} is not valid UTF-8.") from exc
    meta, body = split_frontmatter(raw)
    tier = _override_tier(meta.get("tier"), default_tier, path=str(path))
    kind = _override_kind(meta.get("kind"), default_kind, path=str(path))
    section = meta.get("section") or section_hint or _section_from_path(path, root, tier, kind)
    if section == "":
        section = None
    return Piece(
        path=_rel(root, path),
        tier=tier,
        kind=kind,
        section=section,
        weight=float(weights[tier]),
        text=body.strip(),
        origin=origin,
        learned=learned,
    )


def _section_from_path(path: Path, root: Path, tier: Tier, kind: Kind) -> str | None:
    if kind == "spoken":
        base = root / "corpus" / "spoken"
        alt = root / "transcripts"
    else:
        base = root / "corpus" / tier
        alt = base
    for folder in (base, alt):
        try:
            relative = path.relative_to(folder)
        except ValueError:
            continue
        if len(relative.parts) > 1:
            return relative.parts[0]
        return None
    return None


def _learned_pieces(root: Path, weights: dict[str, float]) -> list[Piece]:
    folder = root / "gold"
    if not folder.is_dir():
        return []
    pieces: list[Piece] = []
    for meta_path in sorted(folder.glob("*/meta.json")):
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise ProjectError(f"{meta_path}: {exc}") from exc
        if not isinstance(meta, dict):
            raise ProjectError(f"{meta_path} must be an object.")
        after = meta_path.parent / str(meta.get("afterFile") or "after.md")
        if not after.is_file():
            raise ProjectError(f"{meta_path} points at a missing after file.")
        origin_raw = meta.get("origin")
        origin = parse_origin(str(origin_raw), path=str(meta_path)) if origin_raw else None
        section = meta.get("section")
        section_hint = section if isinstance(section, str) and section else None
        kind_raw = meta.get("kind")
        default_kind: Kind = "prose"
        if isinstance(kind_raw, str) and kind_raw:
            default_kind = _override_kind(kind_raw, "prose", path=str(meta_path))
        piece = _piece_from_file(
            root,
            after,
            default_tier="gold",
            default_kind=default_kind,
            weights=weights,
            learned=True,
            origin=origin,
            section_hint=section_hint,
        )
        pieces.append(piece)
    return pieces


def _apply_overrides(
    root: Path,
    pieces: list[Piece],
    entries: object,
    weights: dict[str, float],
) -> list[Piece]:
    if not isinstance(entries, list) or not entries:
        return pieces
    by_path = {piece.path: piece for piece in pieces}
    for entry in entries:
        if not isinstance(entry, dict) or "path" not in entry:
            raise ProjectError("manifest pieces entries need a path.")
        rel = str(entry["path"])
        path = root / rel
        if not path.is_file():
            raise ProjectError(f"manifest piece not found: {rel}")
        existing = by_path.get(rel.replace("\\", "/"))
        default_tier: Tier = existing.tier if existing else "silver"
        default_kind: Kind = existing.kind if existing else "prose"
        tier = _override_tier(
            str(entry["tier"]) if entry.get("tier") else None,
            default_tier,
            path=rel,
        )
        kind = _override_kind(
            str(entry["kind"]) if entry.get("kind") else None,
            default_kind,
            path=rel,
        )
        section_value = entry.get("section", existing.section if existing else None)
        section = str(section_value) if isinstance(section_value, str) and section_value else None
        try:
            raw = path.read_text(encoding="utf-8")
        except UnicodeError as exc:
            raise ProjectError(f"{path} is not valid UTF-8.") from exc
        _, body = split_frontmatter(raw)
        by_path[rel.replace("\\", "/")] = Piece(
            path=rel.replace("\\", "/"),
            tier=tier,
            kind=kind,
            section=section,
            weight=float(weights[tier]),
            text=body.strip(),
            origin=existing.origin if existing else None,
            learned=existing.learned if existing else False,
        )
    return list(by_path.values())


def _override_tier(value: str | None, default: Tier, *, path: str) -> Tier:
    if not value:
        return default
    match value:
        case "gold" | "silver" | "bronze":
            return value
        case _:
            raise ProjectError(f"{path}: unknown tier {value!r}")


def _override_kind(value: str | None, default: Kind, *, path: str) -> Kind:
    if not value:
        return default
    match value:
        case "prose" | "spoken":
            return value
        case _:
            raise ProjectError(f"{path}: unknown kind {value!r}")


def _rel(root: Path, path: Path) -> str:
    return path.resolve().relative_to(root.resolve()).as_posix()
