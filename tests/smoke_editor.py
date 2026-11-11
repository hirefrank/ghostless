"""Exercise the built Rune editor against the installed ghostless CLI."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EDITOR = ROOT / "editor" / "target" / "release" / ("ghostless-editor.exe" if os.name == "nt" else "ghostless-editor")


def run(args: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True, check=False)
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


def main() -> None:
    if not EDITOR.is_file():
        raise RuntimeError(f"Build the editor first: {EDITOR}")
    cli = find_cli()
    with tempfile.TemporaryDirectory() as temporary:
        base = Path(temporary)
        env = os.environ.copy()
        env.update({
            "GHOSTLESS_CLI": cli,
            "GHOSTLESS_NO_UPDATE": "1",
            "GHOSTLESS_HOME": str(base / "home"),
        })
        project = base / "voice"
        run([cli, "init", "--no-git", str(project)], cwd=base, env=env)
        draft = base / "draft.md"
        draft.write_text("It's not a note, it's a framework.\n", encoding="utf-8")
        snap = base / "finished.png"
        run([str(EDITOR), "--project", str(project), "--draft", str(draft),
             "--script", str(ROOT / "editor" / "tests" / "edit-session.json"),
             "--snap", str(snap), "--at", "mark:end", "--size", "1120x800"], cwd=base, env=env)
        sessions = [p for p in (project / "sessions").glob("*") if (p / "session.json").is_file()]
        if len(sessions) != 1:
            raise RuntimeError(f"Expected one session, found {len(sessions)}")
        session = sessions[0]
        final = (session / "after.md").read_text(encoding="utf-8")
        notes = (session / "notes.md").read_text(encoding="utf-8")
        receipt = json.loads((session / "receipt.json").read_text(encoding="utf-8"))
        if final != "I changed this draft. Café stays. And the ending is mine.":
            raise RuntimeError(f"Unexpected editor final: {final!r}")
        if notes != "I cut the filler.":
            raise RuntimeError(f"Unexpected editor notes: {notes!r}")
        if (session / "before.md").read_text(encoding="utf-8") != draft.read_text(encoding="utf-8"):
            raise RuntimeError("The immutable original changed")
        if not snap.is_file() or not (project / str(receipt["learnedPath"])).is_dir():
            raise RuntimeError("Editor did not render or learn the finished edit")
        run([cli, "build", "--project", str(project)], cwd=base, env=env)
        judged = subprocess.run([cli, "judge", str(session / "after.md"),
                                 "--project", str(project)], cwd=base, env=env,
                                text=True, capture_output=True, check=False)
        if judged.returncode not in (0, 1) or "ghostless mechanical check" not in judged.stdout:
            raise RuntimeError(f"Cannot judge the editor final: {judged.stdout}\n{judged.stderr}")
        again = json.loads(run([cli, "finish", "--session", session.name,
                                "--project", str(project), "--json"], cwd=base, env=env))
        if again != receipt:
            raise RuntimeError("Finish was not idempotent")
        recovery_draft = base / "recovery.md"
        recovery_draft.write_text("Before the interruption.\n", encoding="utf-8")
        run([str(EDITOR), "--project", str(project), "--draft", str(recovery_draft),
             "--script", str(ROOT / "editor" / "tests" / "save-reopen.json"),
             "--snap", str(base / "saved.png"), "--at", "mark:end", "--size", "1120x800"],
            cwd=base, env=env)
        recovery_sessions = [
            p for p in (project / "sessions").glob("*")
            if (p / "session.json").is_file() and p != session
        ]
        if len(recovery_sessions) != 1:
            raise RuntimeError("Cannot find the interrupted edit session")
        recovery = recovery_sessions[0]
        if (recovery / "after.md").read_text(encoding="utf-8") != "Recovery text. こんにちは ✨ pasted":
            raise RuntimeError("Undo/redo or UTF-8 recovery failed")
        if (recovery / "notes.md").read_text(encoding="utf-8") != "Resume after closing.":
            raise RuntimeError("Notes were not saved before interruption")
        if (recovery / "receipt.json").exists():
            raise RuntimeError("Save must not finish a session")
        moved = base / "moved-voice"
        shutil.copytree(project, moved)
        reopened = base / "reopened.png"
        tree = base / "reopened-tree.json"
        run([str(EDITOR), "--project", str(moved), "--session", session.name,
             "--snap", str(reopened), "--tree", str(tree), "--size", "1120x800"], cwd=base, env=env)
        if not reopened.is_file():
            raise RuntimeError("Cannot reopen a finished session")
        if not tree.is_file() or "Working draft" not in tree.read_text(encoding="utf-8"):
            raise RuntimeError("Accessible editor label missing from Rune tree")
        recovered = base / "recovered.png"
        run([str(EDITOR), "--project", str(moved), "--session", recovery.name,
             "--snap", str(recovered), "--size", "1120x800"], cwd=base, env=env)
        if not recovered.is_file():
            raise RuntimeError("Cannot reopen an interrupted session after moving the project")
    print(f"Rune editor + installed CLI smoke passed on {sys.platform}")


if __name__ == "__main__":
    main()
