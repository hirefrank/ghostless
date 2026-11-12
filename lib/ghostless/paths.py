"""Suite root, data home, and project location."""

from __future__ import annotations

import json
import os
from pathlib import Path

from ghostless import __version__


class ProjectError(Exception):
    """A voice project is missing or its manifest is not usable."""


def suite_root() -> Path | None:
    """Directory that contains package.json, bin/ghostless, and skills/.

    Walks from this file and from the current working directory so an
    editable checkout and the npm package layout both resolve.
    """
    env = os.environ.get("GHOSTLESS_SUITE")
    if env:
        candidate = Path(env).expanduser()
        if _is_suite(candidate):
            return candidate.resolve()
    starts = [Path(__file__).resolve(), Path.cwd().resolve()]
    seen: set[Path] = set()
    for start in starts:
        for parent in (start, *start.parents):
            if parent in seen:
                continue
            seen.add(parent)
            if _is_suite(parent):
                return parent
    return None


def _is_suite(path: Path) -> bool:
    return (
        (path / "package.json").is_file()
        and (path / "bin" / "ghostless").is_file()
        and (path / "skills").is_dir()
    )


def data_home() -> Path:
    override = os.environ.get("GHOSTLESS_HOME")
    if override:
        return Path(override).expanduser()
    return Path.home() / ".ghostless"


def version() -> str:
    root = suite_root()
    if root is not None:
        try:
            data = json.loads((root / "package.json").read_text(encoding="utf-8"))
            value = data.get("version")
            if isinstance(value, str) and value:
                return value
        except (OSError, json.JSONDecodeError):
            pass
    return __version__


def load_home_config() -> dict[str, object]:
    """`~/.ghostless/config.json` as a dict; missing or unreadable means empty.

    A broken config must never block a voice command; the installer rewrites
    it on the next install-skills run.
    """
    path = data_home() / "config.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def default_project_path() -> Path | None:
    """The `defaultProject` from the home config, or None when unset."""
    value = load_home_config().get("defaultProject")
    if not isinstance(value, str) or not value.strip():
        return None
    return Path(value.strip()).expanduser()


def set_default_project(path: Path) -> Path:
    """Persist `path` as `defaultProject` in the home config.

    Validates that it is a ghostless project first, and preserves every
    other key already in config.json.
    """
    resolved = path.expanduser().resolve()
    _require_project(resolved)
    home = data_home()
    home.mkdir(parents=True, exist_ok=True)
    config_path = home / "config.json"
    config = load_home_config()
    config["defaultProject"] = str(resolved)
    config_path.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")
    return resolved


def find_project(start: Path | None = None) -> Path:
    """Find a directory whose manifest.json has kind 'ghostless'.

    Precedence: an explicit path, GHOSTLESS_PROJECT, a walk from cwd,
    then the `defaultProject` in ~/.ghostless/config.json.
    """
    path, _source = find_project_with_source(start)
    return path


def find_project_with_source(start: Path | None = None) -> tuple[Path, str]:
    """Like `find_project`, plus which source won.

    Sources: `--project`, `GHOSTLESS_PROJECT`, `cwd`, `defaultProject`.
    """
    if start is not None:
        path = start.expanduser().resolve()
        _require_project(path)
        return path, "--project"
    override = os.environ.get("GHOSTLESS_PROJECT")
    if override:
        path = Path(override).expanduser().resolve()
        _require_project(path)
        return path, "GHOSTLESS_PROJECT"
    current = Path.cwd().resolve()
    for parent in (current, *current.parents):
        manifest = parent / "manifest.json"
        if manifest.is_file() and _manifest_kind(manifest) == "ghostless":
            return parent, "cwd"
        nested = parent / "voice" / "manifest.json"
        if nested.is_file() and _manifest_kind(nested) == "ghostless":
            return nested.parent, "cwd"
    default = default_project_path()
    if default is not None:
        path = default.resolve()
        try:
            _require_project(path)
        except ProjectError as exc:
            raise ProjectError(
                f"Default project is not a ghostless project: {path}. "
                "Point `ghostless use` at the right directory."
            ) from exc
        return path, "defaultProject"
    raise ProjectError(
        "No ghostless project found. Run `ghostless init` in the folder where your writing lives."
    )


def _require_project(path: Path) -> None:
    manifest = path / "manifest.json"
    if not manifest.is_file() or _manifest_kind(manifest) != "ghostless":
        raise ProjectError(f"Not a ghostless project: {path}")


def _manifest_kind(path: Path) -> str | None:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    kind = data.get("kind")
    return kind if isinstance(kind, str) else None
