"""Portable skill installation and maintenance commands.

The npm launcher and the Python console script both enter here. No command
requires Bash, so the same project and edit-session files work on Windows.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from ghostless.paths import ProjectError, data_home, find_project_with_source, suite_root, version

SKILLS = ("ghostless-draft", "ghostless-edit", "ghostless-learn")
AGENTS = {
    "claude-code": (".claude", "skills"),
    "cursor": (".cursor", "skills"),
    "codex": (".codex", "skills"),
    "opencode": (".config", "opencode", "skills"),
    "pi": (".pi", "agent", "skills"),
    "gemini-cli": (".gemini", "skills"),
    "hermes-agent": (".hermes", "skills"),
    "muse": (".agents", "skills"),
    "agents": (".agents", "skills"),
}
COPY_MARKER = ".ghostless-installed"


def agent_dir(name: str) -> Path:
    if name not in AGENTS:
        raise ProjectError(f"Unknown agent {name!r}. Run `ghostless agents` for choices.")
    return Path.home().joinpath(*AGENTS[name])


def detected_agents() -> list[str]:
    found = [name for name, parts in AGENTS.items() if Path.home().joinpath(*parts[:-1]).is_dir()]
    if "muse" not in found and shutil.which("muse"):
        found.append("muse")
    return found or ["claude-code"]


def selected_agents(value: str) -> list[str]:
    if value == "auto":
        return detected_agents()
    if value == "all":
        return list(AGENTS)
    names = list(dict.fromkeys(name.strip() for name in value.split(",") if name.strip()))
    if not names:
        raise ProjectError("--agent needs at least one agent name.")
    for name in names:
        agent_dir(name)
    return names


def _root() -> Path:
    root = suite_root()
    if root is None or not (root / "skills").is_dir():
        raise ProjectError("Skill commands need the full npm package or a Git checkout.")
    return root


def _ephemeral_package(root: Path) -> bool:
    location = root.as_posix().lower()
    return (
        "/_npx/" in location
        or "/pnpm/dlx/" in location
        or "/.cache/pnpm/" in location
        or "/pnpm-cache/dlx/" in location  # Windows: %LOCALAPPDATA%\pnpm-cache\dlx
    )


def _owned(dest: Path, src: Path) -> bool:
    if dest.is_symlink():
        return dest.resolve() == src.resolve()
    if dest.is_dir():
        marker = dest / COPY_MARKER
        return marker.is_file() and marker.read_text(encoding="utf-8").strip() == f"ghostless:{src.name}"
    return False


def _remove_owned_or_forced(dest: Path) -> None:
    if dest.is_symlink() or dest.is_file():
        dest.unlink()
    elif dest.is_dir():
        shutil.rmtree(dest)


def install_skills(*, agent: str, force: bool, copy: bool, no_launcher: bool, verbose: bool) -> int:
    root = _root()
    transient = _ephemeral_package(root)
    custom = os.environ.get("GHOSTLESS_SKILLS_DIR")
    targets = [("custom", Path(custom).expanduser())] if custom else [
        (name, agent_dir(name)) for name in selected_agents(agent)
    ]
    source_dir = root / "skills"
    installed = 0
    skipped: list[str] = []
    for label, parent in targets:
        parent.mkdir(parents=True, exist_ok=True)
        for skill in SKILLS:
            src = source_dir / skill
            if not (src / "SKILL.md").is_file():
                raise ProjectError(f"Missing packaged skill: {src}")
            dest = parent / skill
            present = dest.is_symlink() or dest.exists()
            owned = _owned(dest, src) if present else False
            if present and not owned and not force:
                skipped.append(str(dest))
                print(f"kept {dest} (not installed by ghostless)")
                continue
            if present:
                if not owned:
                    print(f"replacing {dest} (--force)")
                _remove_owned_or_forced(dest)
            if copy or os.name == "nt" or transient:
                shutil.copytree(src, dest)
                (dest / COPY_MARKER).write_text(f"ghostless:{skill}\n", encoding="utf-8")
                method = "copied"
            else:
                dest.symlink_to(src, target_is_directory=True)
                method = "linked"
            installed += 1
            if verbose:
                print(f"{method} {dest}")
        if verbose:
            print(f"checked {label}: {parent}")

    home = data_home()
    (home / "logs").mkdir(parents=True, exist_ok=True)
    config = home / "config.json"
    if not config.exists():
        config.write_text('{"updateCheck": true}\n', encoding="utf-8")
    if not transient:
        (home / "suite-root").write_text(str(root) + "\n", encoding="utf-8")
    (home / "version").write_text(version() + "\n", encoding="utf-8")
    with (home / "logs" / "setup.log").open("a", encoding="utf-8") as log:
        log.write(f"{datetime.now(timezone.utc).isoformat()} install-skills suite={root} installed={installed}\n")
    print(f"ghostless skills installed: {installed}; existing paths kept: {len(skipped)}")
    if not no_launcher:
        print("CLI launcher: supplied by npm or the Python package")
    return 0


def list_agents() -> int:
    detected = set(detected_agents())
    for name in AGENTS:
        print(f"{name}\t{agent_dir(name)}\t{'detected' if name in detected else ''}")
    return 0


def doctor() -> int:
    root = suite_root()
    print(f"Python: {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}")
    print(f"Suite: {root or 'none'}")
    print(f"Data: {data_home()}")
    try:
        project, source = find_project_with_source()
        print(f"Project: {project} (via {source})")
    except ProjectError as exc:
        print(f"Project: none ({exc})")
    if root is None:
        return 1
    for name in AGENTS:
        parent = agent_dir(name)
        if not parent.parent.is_dir():
            continue
        count = sum((parent / skill / "SKILL.md").is_file() for skill in SKILLS)
        print(f"{name}: {count}/{len(SKILLS)} skills in {parent}")
    return 0


def reset(*, agent: str, data: bool, yes: bool) -> int:
    root = _root()
    targets = [(name, agent_dir(name)) for name in selected_agents(agent)]
    custom = os.environ.get("GHOSTLESS_SKILLS_DIR")
    if custom:
        targets = [("custom", Path(custom).expanduser())]
    print("Remove ghostless skill links/copies from: " + ", ".join(name for name, _ in targets))
    if data:
        print(f"Also remove installer data: {data_home()}")
    if not yes:
        if not sys.stdin.isatty():
            raise ProjectError("Non-interactive reset requires --yes.")
        if input("Continue? [y/N] ").strip().lower() not in {"y", "yes"}:
            print("Aborted.")
            return 1
    removed = 0
    for _, parent in targets:
        for skill in SKILLS:
            dest = parent / skill
            if _owned(dest, root / "skills" / skill):
                _remove_owned_or_forced(dest)
                print(f"removed {dest}")
                removed += 1
            elif dest.is_symlink() or dest.exists():
                print(f"kept {dest} (not installed by ghostless)")
    if data and data_home().exists():
        shutil.rmtree(data_home())
        print(f"removed {data_home()}")
    print(f"Reset complete ({removed} skill paths removed). The CLI is still installed.")
    return 0


def update(*, agent: str) -> int:
    if os.environ.get("GHOSTLESS_LAUNCHER") != "npm":
        print("This CLI was not launched from npm. Upgrade it with its package manager, then run `ghostless install-skills --force`.")
        return 1
    if _ephemeral_package(_root()):
        print("This is a one-off npm/pnpm copy. Run `npx --yes ghostless@latest install-skills --force` to refresh the installed skills.")
        return 1
    if "/node_modules/ghostless" not in _root().as_posix().lower():
        print("This is a checkout. Update its Git branch, then run `ghostless install-skills --force`.")
        return 1
    npm = shutil.which("npm.cmd" if os.name == "nt" else "npm")
    if npm is None:
        raise ProjectError("npm is needed to update this installation.")
    # Windows .cmd shims must run through the command interpreter.
    if os.name == "nt":
        command = ["cmd", "/c", npm, "install", "-g", "ghostless@latest"]
    else:
        command = [npm, "install", "-g", "ghostless@latest"]
    result = subprocess.run(command, check=False)
    if result.returncode:
        return int(result.returncode)
    return install_skills(agent=agent, force=True, copy=False, no_launcher=False, verbose=False)


def main(argv: list[str]) -> int:
    command, *extra = argv
    if command in {"install-skills", "setup"}:
        parser = argparse.ArgumentParser(prog=f"ghostless {command}")
        parser.add_argument("--agent", "--agents", default="auto")
        parser.add_argument("--force", action="store_true")
        parser.add_argument("--copy", action="store_true")
        parser.add_argument("--no-launcher", action="store_true")
        parser.add_argument("-v", "--verbose", action="store_true")
        parser.add_argument("-y", "--yes", action="store_true")
        args = parser.parse_args(extra)
        return install_skills(agent=args.agent, force=args.force, copy=args.copy,
                              no_launcher=args.no_launcher, verbose=args.verbose)
    if command == "agents":
        return list_agents()
    if command == "doctor":
        return doctor()
    if command == "routing":
        print((_root() / "docs" / "AGENTS.md.snippet").read_text(encoding="utf-8"), end="")
        return 0
    if command == "update":
        parser = argparse.ArgumentParser(prog="ghostless update")
        parser.add_argument("--agent", "--agents", default="auto")
        args = parser.parse_args(extra)
        return update(agent=args.agent)
    if command in {"reset", "uninstall"}:
        parser = argparse.ArgumentParser(prog=f"ghostless {command}")
        parser.add_argument("--agent", "--agents", default="auto")
        parser.add_argument("--data", action="store_true")
        parser.add_argument("-y", "--yes", action="store_true")
        args = parser.parse_args(extra)
        return reset(agent=args.agent, data=args.data or command == "uninstall", yes=args.yes)
    raise ProjectError(f"Unknown maintenance command: {command}")
