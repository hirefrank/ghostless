"""Exercise the built Rune editor against the installed ghostless CLI."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EDITOR = ROOT / "editor" / "target" / "release" / ("ghostless-editor.exe" if os.name == "nt" else "ghostless-editor")


def norm(text: str) -> str:
    """Normalize line endings, Unicode form, and whitespace so cross-platform edits compare equal."""
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = unicodedata.normalize("NFC", text)
    return " ".join(text.split())


def _show(text: str) -> str:
    return "".join(f"\\u{ord(ch):04x}" if ord(ch) > 126 else ch for ch in text)


def _working_box_height(tree_path: Path) -> float:
    data = json.loads(tree_path.read_text(encoding="utf-8"))
    for element in data.get("elements", []):
        if not isinstance(element, dict):
            continue
        path = str(element.get("path", ""))
        if element.get("role") == "textbox" and "working-body" in path:
            rect = element.get("rect")
            if isinstance(rect, list) and len(rect) == 4:
                return float(rect[3])
    raise RuntimeError(f"Working draft textbox not found in {tree_path}")


def _cli_cmd(executable: str, *args: str) -> list[str]:
    if os.name == "nt" and executable.lower().endswith((".cmd", ".bat")):
        return ["cmd", "/c", executable, *args]
    return [executable, *args]


def run(args: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(args, cwd=cwd, env=env, text=True, encoding="utf-8", capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"{args!r} exited {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def find_cli() -> str:
    # From a checkout, prefer the local launcher so the smoke exercises the
    # code under test rather than a stale global install. On Windows the bash
    # launcher cannot run, so fall back to an installed console script there.
    launcher = ROOT / "bin" / "ghostless"
    if os.name != "nt" and launcher.is_file():
        return str(launcher)
    on_path = shutil.which("ghostless")
    if on_path:
        return on_path
    raise RuntimeError("ghostless CLI not found. Run `pip install .` or build from a checkout.")


def install_packed_npm_cli(base: Path) -> tuple[str, Path]:
    npm = shutil.which("npm.cmd" if os.name == "nt" else "npm")
    if npm is None:
        raise RuntimeError("npm is required for --npm-pack editor smoke")
    pack = json.loads(run(_cli_cmd(npm, "pack", "--json", "--pack-destination", str(base)), cwd=ROOT))
    archive = base / pack[0]["filename"]
    prefix = base / "npm-prefix"
    run(_cli_cmd(npm, "install", "--global", "--prefix", str(prefix), str(archive)), cwd=base)
    executable = prefix / ("ghostless.cmd" if os.name == "nt" else "bin/ghostless")
    if not executable.is_file():
        raise RuntimeError(f"npm did not install the CLI: {executable}")
    return str(executable), prefix


def keep_artifacts(source: Path, destination: str | None) -> None:
    """Copy rendered screenshots and accessibility trees out of the temp project."""
    if not destination:
        return
    dest = Path(destination)
    dest.mkdir(parents=True, exist_ok=True)
    for path in source.iterdir():
        if path.is_file() and (path.suffix == ".png" or path.name.endswith("-tree.json")):
            shutil.copy2(path, dest / path.name)


def main() -> None:  # noqa: C901  # linear GUI smoke scenario, not a library function
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--npm-pack",
        action="store_true",
        help="Pack and install the npm CLI and verify the editor invokes it.",
    )
    parser.add_argument(
        "--artifacts-dir",
        help="Copy rendered PNGs and tree JSON here before the temp project is removed",
    )
    args = parser.parse_args()

    if not EDITOR.is_file():
        raise RuntimeError(f"Build the editor first: {EDITOR}")
    temporary = tempfile.TemporaryDirectory()
    base = Path(temporary.name)
    try:
        env = os.environ.copy()
        env.update(
            {
                "GHOSTLESS_NO_UPDATE": "1",
                "GHOSTLESS_HOME": str(base / "home"),
                "PYTHONUTF8": "1",
            }
        )
        if args.npm_pack:
            cli, prefix = install_packed_npm_cli(base)
            bin_dir = prefix if os.name == "nt" else prefix / "bin"
            env["PATH"] = os.pathsep.join([str(bin_dir), env.get("PATH", "")])
            env.setdefault("GHOSTLESS_PYTHON", sys.executable)
            if os.name == "nt":
                # Exercise the editor's PATH fallback calling npm's ghostless.cmd.
                env.pop("GHOSTLESS_CLI", None)
                resolved = shutil.which("ghostless", path=env["PATH"])
                if resolved is None or Path(resolved).resolve() != Path(cli).resolve():
                    raise RuntimeError(f"Expected PATH ghostless to resolve to {cli}, got {resolved!r}")
            else:
                env["GHOSTLESS_CLI"] = cli
        else:
            cli = find_cli()
            env["GHOSTLESS_CLI"] = cli

        project = base / "voice"
        run(_cli_cmd(cli, "init", "--no-git", str(project)), cwd=base, env=env)
        draft = base / "draft.md"
        draft.write_text("It's not a note, it's a framework.\n", encoding="utf-8")
        snap = base / "finished.png"
        run(
            [
                str(EDITOR),
                "--project",
                str(project),
                "--draft",
                str(draft),
                "--script",
                str(ROOT / "editor" / "tests" / "edit-session.json"),
                "--snap",
                str(snap),
                "--at",
                "mark:end",
                "--size",
                "1120x800",
            ],
            cwd=base,
            env=env,
        )
        sessions = [p for p in (project / "sessions").glob("*") if (p / "session.json").is_file()]
        if len(sessions) != 1:
            raise RuntimeError(f"Expected one session, found {len(sessions)}")
        session = sessions[0]
        final = (session / "after.md").read_text(encoding="utf-8")
        notes = (session / "notes.md").read_text(encoding="utf-8")
        receipt = json.loads((session / "receipt.json").read_text(encoding="utf-8"))
        if norm(final) != norm("I changed this draft. Café stays. And the ending is mine."):
            raise RuntimeError(
                f"Unexpected editor final: {_show(final)!r} (expected {_show('I changed this draft. Café stays. And the ending is mine.')!r})"
            )
        if norm(notes) != "I cut the filler.":
            raise RuntimeError(f"Unexpected editor notes: {_show(notes)!r}")
        if (session / "before.md").read_text(encoding="utf-8") != draft.read_text(encoding="utf-8"):
            raise RuntimeError("The immutable original changed")
        if not snap.is_file() or not (project / str(receipt["learnedPath"])).is_dir():
            raise RuntimeError("Editor did not render or learn the finished edit")
        run(_cli_cmd(cli, "build", "--project", str(project)), cwd=base, env=env)
        judged = subprocess.run(
            _cli_cmd(cli, "judge", str(session / "after.md"), "--project", str(project)),
            cwd=base,
            env=env,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )
        if judged.returncode not in (0, 1) or "ghostless mechanical check" not in judged.stdout:
            raise RuntimeError(f"Cannot judge the editor final: {judged.stdout}\n{judged.stderr}")
        again = json.loads(
            run(
                _cli_cmd(cli, "finish", "--session", session.name, "--project", str(project), "--json"),
                cwd=base,
                env=env,
            )
        )
        if again != receipt:
            raise RuntimeError("Finish was not idempotent")
        recovery_draft = base / "recovery.md"
        recovery_draft.write_text("Before the interruption.\n", encoding="utf-8")
        run(
            [
                str(EDITOR),
                "--project",
                str(project),
                "--draft",
                str(recovery_draft),
                "--script",
                str(ROOT / "editor" / "tests" / "save-reopen.json"),
                "--snap",
                str(base / "saved.png"),
                "--at",
                "mark:end",
                "--size",
                "1120x800",
            ],
            cwd=base,
            env=env,
        )
        recovery_sessions = [
            p for p in (project / "sessions").glob("*") if (p / "session.json").is_file() and p != session
        ]
        if len(recovery_sessions) != 1:
            raise RuntimeError("Cannot find the interrupted edit session")
        recovery = recovery_sessions[0]
        if norm((recovery / "after.md").read_text(encoding="utf-8")) != "Recovery text. こんにちは ✨ pasted":
            raise RuntimeError("Undo/redo or UTF-8 recovery failed")
        if norm((recovery / "notes.md").read_text(encoding="utf-8")) != "Resume after closing.":
            raise RuntimeError("Notes were not saved before interruption")
        if (recovery / "receipt.json").exists():
            raise RuntimeError("Save must not finish a session")
        moved = base / "moved-voice"
        shutil.copytree(project, moved)
        reopened = base / "reopened.png"
        tree = base / "reopened-tree.json"
        run(
            [
                str(EDITOR),
                "--project",
                str(moved),
                "--session",
                session.name,
                "--snap",
                str(reopened),
                "--tree",
                str(tree),
                "--size",
                "1120x800",
            ],
            cwd=base,
            env=env,
        )
        if not reopened.is_file():
            raise RuntimeError("Cannot reopen a finished session")
        if not tree.is_file() or "Working draft" not in tree.read_text(encoding="utf-8"):
            raise RuntimeError("Accessible editor label missing from Rune tree")
        recovered = base / "recovered.png"
        run(
            [
                str(EDITOR),
                "--project",
                str(moved),
                "--session",
                recovery.name,
                "--snap",
                str(recovered),
                "--size",
                "1120x800",
            ],
            cwd=base,
            env=env,
        )
        if not recovered.is_file():
            raise RuntimeError("Cannot reopen an interrupted session after moving the project")

        # Closing before the 800 ms autosave timer expires still flushes dirty edits.
        unsaved_draft = base / "unsaved-close.md"
        unsaved_draft.write_text("Initial unsaved close draft.\n", encoding="utf-8")
        run(
            [
                str(EDITOR),
                "--project",
                str(project),
                "--draft",
                str(unsaved_draft),
                "--script",
                str(ROOT / "editor" / "tests" / "close-without-save.json"),
                "--snap",
                str(base / "unsaved-close.png"),
                "--at",
                "mark:end",
                "--size",
                "1120x800",
            ],
            cwd=base,
            env=env,
        )
        unsaved_sessions = [
            p
            for p in (project / "sessions").glob("*")
            if (p / "session.json").is_file() and p not in {session, recovery}
        ]
        if len(unsaved_sessions) != 1:
            raise RuntimeError("Cannot find the unsaved-close session")
        unsaved = unsaved_sessions[0]
        if norm((unsaved / "after.md").read_text(encoding="utf-8")) != "Unsaved close text.":
            raise RuntimeError("Unsaved working text was not flushed on exit")
        if norm((unsaved / "notes.md").read_text(encoding="utf-8")) != "Flushed on exit.":
            raise RuntimeError("Unsaved notes were not flushed on exit")
        if (unsaved / "receipt.json").exists():
            raise RuntimeError("Exit flush must not finish a session")

        # Working-copy TextArea grows dynamically with window height.
        tall_tree = base / "tall-tree.json"
        run(
            [
                str(EDITOR),
                "--project",
                str(moved),
                "--session",
                recovery.name,
                "--snap",
                str(base / "tall.png"),
                "--tree",
                str(tall_tree),
                "--size",
                "1120x1040",
            ],
            cwd=base,
            env=env,
        )
        normal_height = _working_box_height(tree)
        tall_height = _working_box_height(tall_tree)
        if tall_height < normal_height + 200.0:
            raise RuntimeError(
                f"Expected working draft height to grow with window size: 800 -> {normal_height}, 1040 -> {tall_height}"
            )

        missing_project = base / "missing-voice"
        missing = subprocess.run(
            [str(EDITOR), "--project", str(missing_project), "--draft", str(draft)],
            cwd=base,
            env=env,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )
        if missing.returncode == 0 or f"Project directory does not exist: {missing_project}" not in missing.stderr:
            raise RuntimeError(f"Missing project path was not named:\n{missing.stderr}")
        if "ghostless init" not in missing.stderr:
            raise RuntimeError(f"Missing project did not point at ghostless init:\n{missing.stderr}")
        missing_draft = base / "missing-draft.md"
        missing_file = subprocess.run(
            [str(EDITOR), "--project", str(project), "--draft", str(missing_draft)],
            cwd=base,
            env=env,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )
        if f"Draft does not exist: {missing_draft}" not in missing_file.stderr:
            raise RuntimeError(f"Missing draft path was not named:\n{missing_file.stderr}")
        cwd_draft = base / "cwd-draft.md"
        cwd_draft.write_text("Opened from the project directory.\n", encoding="utf-8")
        before = {path.name for path in (project / "sessions").iterdir()}
        run(
            [
                str(EDITOR),
                "--draft",
                str(cwd_draft),
                "--script",
                str(ROOT / "editor" / "tests" / "open-quit.json"),
                "--snap",
                str(base / "cwd.png"),
                "--at",
                "mark:end",
                "--size",
                "800x600",
            ],
            cwd=project,
            env=env,
        )
        opened = {path.name for path in (project / "sessions").iterdir()} - before
        if len(opened) != 1:
            raise RuntimeError("Editor did not open a session from the project directory without --project")
        print(f"Rune editor + installed CLI smoke passed on {sys.platform}")
    finally:
        keep_artifacts(base, args.artifacts_dir)
        temporary.cleanup()


if __name__ == "__main__":
    main()
