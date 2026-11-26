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


STUDIO_SIZE = "1280x820"
STUDIO_DRAFT = """# The late train

The platform went quiet after nine. A cleaner dragged his broom along the yellow line.
Across the street, the café still had a light on.

## What I remember

I kept the ticket in my coat pocket. It was the first night I took that train home.
"""
BLUR_EDIT = "A new line for the late train."
LINKED_FINAL = "# The late train\n\nI made the final train. Café was still open.\n"
LINKED_NOTE = "I kept the line about the café and cut the opening."


def studio_home(base: Path, name: str, env: dict[str, str]) -> dict[str, str]:
    home = base / f"home-{name}"
    home.mkdir()
    (home / "editor-state.json").write_text(
        json.dumps({"ui": {"theme": "paper", "sidebar_width": 250, "inspector_width": 318}}),
        encoding="utf-8",
    )
    return {**env, "GHOSTLESS_HOME": str(home)}


def studio_scene(
    base: Path,
    env: dict[str, str],
    script: str,
    label: str,
    *launch: str,
    mark: str = "end",
    snap: Path | None = None,
    workspace: Path | None = None,
) -> list[dict[str, object]]:
    """Render a script checkpoint and return the accessibility elements at that instant."""
    script_path = ROOT / "editor" / "tests" / f"{script}.json"
    if workspace is not None:
        # Supply a native path to the same scripted file-dialog event on every OS.
        script_data = json.loads(script_path.read_text(encoding="utf-8"))
        for event in script_data["events"]:
            if event.get("type") == "file-dialog":
                event["path"] = str(workspace)
        script_path = base / f"{script}-local.json"
        script_path.write_text(json.dumps(script_data), encoding="utf-8")
    tree = base / f"{label}-tree.json"
    options = ["--snap", str(snap)] if snap is not None else []
    run(
        [
            str(EDITOR),
            *launch,
            "--script",
            str(script_path),
            "--at",
            f"mark:{mark}",
            "--size",
            STUDIO_SIZE,
            "--realtime",
            "--scale",
            "1",
            "--tree",
            str(tree),
            *options,
        ],
        cwd=base,
        env=env,
    )
    elements: list[dict[str, object]] = json.loads(tree.read_text(encoding="utf-8"))["elements"]
    return elements


def studio_mentions(elements: list[dict[str, object]], text: str) -> bool:
    return any(text in str(element.get(field, "")) for element in elements for field in ("label", "text", "value"))


def studio_workspace(base: Path, cli: str, env: dict[str, str]) -> tuple[Path, Path, Path]:
    workspace = base / "Writing Studio"
    piece = workspace / "seeds" / "demo"
    piece.mkdir(parents=True)
    voice = workspace / "voice"
    sample = ROOT / "fixtures" / "sample-voice"
    shutil.copytree(sample, voice)
    draft = piece / "draft.md"
    draft.write_text(STUDIO_DRAFT, encoding="utf-8")
    (piece / "questions.md").write_text(
        "# Questions\n\nWhat changed between the first and last train?\n", encoding="utf-8"
    )
    (piece / "NEXT.md").write_text("# Next\n\nRead the opening aloud.\n", encoding="utf-8")
    (piece / "ai-ish.md").write_text(
        "# The late train\n\n"
        "I was the only passenger left on the platform. The station clock was three minutes slow.\n\n"
        "It's not a note, it's a framework. We can delve into what waiting does to a familiar place.\n\n"
        "## After the bell\n\n"
        "I could hear the café owner stacking cups. When she turned her sign over, I walked home.\n",
        encoding="utf-8",
    )
    run(_cli_cmd(cli, "build", "--project", str(voice)), cwd=base, env=env)
    return workspace, voice, draft


def smoke_workspace_edit(base: Path, workspace: Path, draft: Path, env: dict[str, str]) -> None:
    launch = ("--folder", str(workspace))
    blur_env = studio_home(base, "blur", env)
    blur = studio_scene(base, blur_env, "workspace-edit", "workspace-blur", *launch, mark="blurred")
    if norm(draft.read_text(encoding="utf-8")) != BLUR_EDIT or not studio_mentions(blur, "Saved"):
        raise RuntimeError("The editor did not flush the edited draft on blur before the autosave timer")
    draft.write_text(STUDIO_DRAFT, encoding="utf-8")

    undo_env = studio_home(base, "undo", env)
    undo = studio_scene(base, undo_env, "workspace-edit", "workspace-undo", *launch)
    if not studio_mentions(undo, "questions.md") or norm(draft.read_text(encoding="utf-8")) != norm(STUDIO_DRAFT):
        raise RuntimeError("The draft's undo history did not survive switching to a second tab")


def diff_layout(elements: list[dict[str, object]], layout: str) -> bool:
    return any(element.get("label") == layout and element.get("itemSelected") is True for element in elements)


def smoke_linked_session(base: Path, workspace: Path, voice: Path, draft: Path, env: dict[str, str]) -> None:
    linked_env = studio_home(base, "linked", env)
    launch = ("--folder", str(workspace))
    unified = studio_scene(base, linked_env, "linked-compare", "workspace-unified", *launch)
    if not diff_layout(unified, "Unified") or not studio_mentions(unified, "Baseline vs draft"):
        raise RuntimeError("Linked draft did not switch to Unified Compare")
    sessions = [path for path in (voice / "sessions").iterdir() if (path / "session.json").is_file()]
    if len(sessions) != 1:
        raise RuntimeError(f"Expected one workspace-linked session, found {len(sessions)}")
    session = sessions[0]
    if any(norm(path.read_text(encoding="utf-8")) != norm(LINKED_FINAL) for path in (draft, session / "after.md")):
        raise RuntimeError("The working draft and its linked session copy did not save together")
    if (session / "before.md").read_text(encoding="utf-8") != STUDIO_DRAFT:
        raise RuntimeError("The linked save modified the immutable session baseline")
    if (session / "receipt.json").exists():
        raise RuntimeError("Compare or save finished the session")

    split = studio_scene(base, linked_env, "linked-finish", "workspace-split", *launch, mark="split")
    if not diff_layout(split, "Split") or not studio_mentions(split, "Baseline vs draft"):
        raise RuntimeError("Linked draft did not switch back to Split Compare")
    prior_stats = (voice / "voice-stats.json").read_bytes()
    finished = studio_scene(base, linked_env, "linked-finish", "workspace-finished", *launch)
    if not studio_mentions(finished, "Session: finished"):
        raise RuntimeError("Finish & Learn did not lock the workspace-linked session")
    receipt = json.loads((session / "receipt.json").read_text(encoding="utf-8"))
    if receipt["tier"] != "gold" or not (voice / "gold" / session.name / "after.md").is_file():
        raise RuntimeError("Finish & Learn did not save the writer-first final in the gold tier")
    if (session / "notes.md").read_text(encoding="utf-8") != LINKED_NOTE:
        raise RuntimeError("Finish & Learn did not save the modal notes")
    if (voice / "voice-stats.json").read_bytes() == prior_stats:
        raise RuntimeError("Finish & Learn did not rebuild the voice guide")
    if norm(draft.read_text(encoding="utf-8")) != norm(LINKED_FINAL):
        raise RuntimeError("Finish & Learn changed the final workspace draft")


def smoke_writing_studio(base: Path, cli: str, env: dict[str, str]) -> None:
    workspace, voice, draft = studio_workspace(base, cli, env)
    welcome_env = studio_home(base, "welcome", env)
    welcome = studio_scene(base, welcome_env, "workspace-welcome", "workspace-welcome", workspace=workspace)
    for label in ("NEXT.md", "questions.md", "draft.md", "Rules & Checks"):
        if not studio_mentions(welcome, label):
            raise RuntimeError(f"Open Folder did not show {label} from the workspace or nested voice project")

    smoke_workspace_edit(base, workspace, draft, env)

    inspector_env = studio_home(base, "inspector", env)
    inspector = studio_scene(
        base,
        inspector_env,
        "workspace-inspector",
        "workspace-inspector",
        "--folder",
        str(workspace),
        snap=base / "editor-preview.png",
    )
    for label in ("Score ", "delve", "Voice lens"):
        if not studio_mentions(inspector, label):
            raise RuntimeError(f"The live Voice Inspector did not report {label}")
    if not any("not-x-its-y" in str(item.get("help", "")) for item in inspector):
        raise RuntimeError("The live Voice Inspector did not flag the contrast tell")

    inspector_prefs = Path(inspector_env["GHOSTLESS_HOME"]) / "editor-state.json"
    if not json.loads(inspector_prefs.read_text(encoding="utf-8"))["ui"]["voice_lens"]:
        raise RuntimeError("The Voice Lens shortcut did not enable inline flags")

    smoke_linked_session(base, workspace, voice, draft, env)


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
                "--realtime",
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
        smoke_writing_studio(base, cli, env)
        print(f"Rune editor + installed CLI smoke passed on {sys.platform}")
    finally:
        keep_artifacts(base, args.artifacts_dir)
        temporary.cleanup()


if __name__ == "__main__":
    main()
