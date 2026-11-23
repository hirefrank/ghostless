"""Stable CLI launcher and desktop-editor discovery.

`install-skills` writes a small launcher outside the active Node version.
The launcher reads `~/.ghostless/suite-root` and execs that suite, so a
toolchain upgrade that deletes the version-scoped npm bin does not remove
the `ghostless` command. A file that is not our launcher is left alone.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

from ghostless.io import atomic_write_text
from ghostless.paths import ProjectError, suite_root

LAUNCHER_MARKER = "ghostless launcher"

_VERSION_MARKERS = (
    "/mise/installs/node/",
    "/versions/node/",
    "/.nvm/",
    "/fnm/",
    "/asdf/",
)

_POSIX_LAUNCHER = """#!/bin/sh
# ghostless launcher — resolves the installed suite recorded by install-skills.
set -eu
data="${GHOSTLESS_HOME:-$HOME/.ghostless}"
root=""
if [ -f "$data/suite-root" ]; then
  root=$(sed -n '1s/^[[:space:]]*//;1s/[[:space:]]*$//;1p' "$data/suite-root" 2>/dev/null || true)
fi
export GHOSTLESS_STABLE_LAUNCHER=1
if [ -n "$root" ] && [ -x "$root/bin/ghostless" ]; then
  exec "$root/bin/ghostless" "$@"
fi
if [ -n "$root" ] && [ -f "$root/bin/ghostless.cjs" ] && command -v node >/dev/null 2>&1; then
  exec node "$root/bin/ghostless.cjs" "$@"
fi
if [ -n "$root" ]; then
  printf 'ghostless: installed suite is gone: %s\\n' "$root" >&2
  echo "A Node or toolchain upgrade removes a version-scoped npm install." >&2
  echo "Reinstall from the checkout: npm i -g . && ghostless install-skills --force" >&2
  echo "Or, once published: npm i -g ghostless@latest && ghostless install-skills --force" >&2
  exit 1
fi
echo "ghostless: no suite recorded in $data/suite-root" >&2
echo "Reinstall: npm i -g . && ghostless install-skills" >&2
exit 1
"""

_WINDOWS_LAUNCHER = r"""@echo off
rem ghostless launcher — resolves the installed suite recorded by install-skills.
setlocal EnableExtensions EnableDelayedExpansion
set "data=%GHOSTLESS_HOME%"
if "%data%"=="" set "data=%USERPROFILE%\.ghostless"
set "root="
if exist "%data%\suite-root" set /p root=<"%data%\suite-root"
set "GHOSTLESS_STABLE_LAUNCHER=1"
if not "%root%"=="" if exist "%root%\bin\ghostless.cjs" (
  node "%root%\bin\ghostless.cjs" %*
  exit /b %ERRORLEVEL%
)
if not "%root%"=="" (
  echo ghostless: installed suite is gone: %root% 1>&2
  echo A Node or toolchain upgrade removes a version-scoped npm install. 1>&2
  echo Reinstall from the checkout: npm i -g . ^&^& ghostless install-skills --force 1>&2
  echo Or, once published: npm i -g ghostless@latest ^&^& ghostless install-skills --force 1>&2
  exit /b 1
)
echo ghostless: no suite recorded in %data%\suite-root 1>&2
echo Reinstall: npm i -g . ^&^& ghostless install-skills 1>&2
exit /b 1
"""


def _is_windows() -> bool:
    return os.name == "nt"


def version_scoped_install(root: Path) -> bool:
    """True when the suite lives inside a version manager's Node prefix."""
    text = root.as_posix()
    if "/node_modules/ghostless" not in text:
        return False
    return any(marker in text for marker in _VERSION_MARKERS)


def stable_launcher_dir() -> Path:
    """A bin directory that does not move when the active Node version does."""
    home = Path.home()
    candidates = [home / ".local" / "bin", home / "bin"]
    entries = {
        os.path.normcase(str(Path(entry).expanduser()))
        for entry in os.environ.get("PATH", "").split(os.pathsep)
        if entry
    }
    for candidate in candidates:
        if os.path.normcase(str(candidate)) in entries:
            return candidate
    return candidates[0]


def stable_launcher_path() -> Path:
    name = "ghostless.cmd" if _is_windows() else "ghostless"
    return stable_launcher_dir() / name


def _read_prefix(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")[:500]
    except OSError:
        return ""


def owned_launcher(path: Path | None = None) -> bool:
    """True when `path` is a regular file this installer wrote."""
    candidate = stable_launcher_path() if path is None else path
    if not candidate.is_file() or candidate.is_symlink():
        return False
    return LAUNCHER_MARKER in _read_prefix(candidate)


def _on_path(directory: Path) -> bool:
    entries = {
        os.path.normcase(str(Path(entry).expanduser()))
        for entry in os.environ.get("PATH", "").split(os.pathsep)
        if entry
    }
    return os.path.normcase(str(directory)) in entries


def install_stable_launcher(*, enabled: bool, transient: bool) -> str:
    """Write the stable launcher, or explain why an existing command was kept."""
    if not enabled:
        return "launcher skipped (--no-launcher)"
    if transient:
        return "launcher skipped (one-off package)"
    path = stable_launcher_path()
    if path.exists() and not owned_launcher(path):
        return f"kept {path} (not a ghostless launcher)"
    path.parent.mkdir(parents=True, exist_ok=True)
    atomic_write_text(path, _WINDOWS_LAUNCHER if _is_windows() else _POSIX_LAUNCHER)
    if not _is_windows():
        path.chmod(0o755)
    if _on_path(path.parent):
        return f"ghostless → {path}"
    return f"ghostless → {path} (add {path.parent} to PATH)"


def remove_owned_launcher() -> Path | None:
    """Delete the stable launcher when we wrote it. Foreign commands stay."""
    path = stable_launcher_path()
    if not owned_launcher(path):
        return None
    path.unlink()
    return path


def launcher_status_line() -> str:
    path = stable_launcher_path()
    if owned_launcher(path):
        via = " (this command)" if os.environ.get("GHOSTLESS_STABLE_LAUNCHER") == "1" else ""
        return f"Launcher: {path}{via}"
    return "Launcher: none"


def install_scope_line(root: Path | None) -> str | None:
    """Explain a version-scoped npm install, including when doctor runs via the launcher."""
    if root is None or not version_scoped_install(root):
        return None
    if os.environ.get("GHOSTLESS_STABLE_LAUNCHER") == "1":
        return (
            f"Install: version-scoped ({root}). A Node upgrade removes this copy. "
            "This doctor run is the stable launcher, which still works after that. "
            "Reinstall into the new Node, then run `ghostless install-skills --force`."
        )
    if owned_launcher():
        return (
            f"Install: version-scoped ({root}). A Node upgrade removes this copy. "
            "The stable launcher keeps the command; reinstall, then run "
            "`ghostless install-skills --force`."
        )
    return (
        f"Install: version-scoped ({root}). A Node upgrade removes this copy. "
        "Run `ghostless install-skills` to add a launcher outside the Node version."
    )


def editor_report(root: Path | None) -> str:
    """Where the desktop editor is, or the command that builds it."""
    override = os.environ.get("GHOSTLESS_EDITOR")
    if override:
        path = Path(override).expanduser()
        if path.is_file():
            return f"editor: {path}"
        return f"editor: GHOSTLESS_EDITOR does not exist: {path}"
    binary = find_editor_binary(root, honor_override=False)
    if binary is not None:
        return f"editor: {binary}"
    if root is not None and (root / "editor" / "Cargo.toml").is_file():
        return "editor: not built (cargo build --release --manifest-path editor/Cargo.toml)"
    return "editor: not in this install (clone the repo; see docs/EDITOR.md)"


def find_editor_binary(root: Path | None, *, honor_override: bool = True) -> Path | None:
    """The editor executable, or None when it is not built and not overridden."""
    if honor_override:
        override = os.environ.get("GHOSTLESS_EDITOR")
        if override:
            path = Path(override).expanduser()
            return path if path.is_file() else None
    names = ("ghostless-editor.exe", "ghostless-editor") if _is_windows() else ("ghostless-editor",)
    candidates: list[Path] = []
    if root is not None:
        release = root / "editor" / "target" / "release"
        candidates.extend(release / name for name in names)
    for name in names:
        found = _which(name)
        if found is not None:
            candidates.append(found)
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def _which(name: str) -> Path | None:
    found = shutil.which(name)
    return Path(found) if found else None


def launch_editor(project: Path, *, draft: Path | None = None, session: str | None = None) -> int:
    """Open the desktop editor on a project. Returns the editor's exit code."""
    root = suite_root()
    binary = find_editor_binary(root)
    if binary is None:
        print(editor_report(root), file=sys.stderr)
        return 2
    command = [str(binary), "--project", str(project)]
    if session is not None:
        command.extend(["--session", session])
    elif draft is not None:
        command.extend(["--draft", str(draft)])
    else:
        raise ProjectError("Open the editor with a draft or a session id.")
    if _is_windows() and binary.suffix.lower() == ".cmd":
        command = ["cmd", "/c", *command]
    completed = subprocess.run(command, check=False)
    return int(completed.returncode or 0)
