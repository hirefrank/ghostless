"""Exercise the built Rune editor against the installed ghostless CLI."""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unicodedata
import zlib
from itertools import pairwise
from pathlib import Path
from typing import cast

ROOT = Path(__file__).resolve().parents[1]
EDITOR = ROOT / "editor" / "target" / "release" / ("ghostless-editor.exe" if os.name == "nt" else "ghostless-editor")


def norm(text: str) -> str:
    """Normalize line endings, Unicode form, and whitespace so cross-platform edits compare equal."""
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = unicodedata.normalize("NFC", text)
    return " ".join(text.split())


def _show(text: str) -> str:
    return "".join(f"\\u{ord(ch):04x}" if ord(ch) > 126 else ch for ch in text)


Rect = tuple[float, float, float, float]
Elements = list[dict[str, object]]
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def element_rect(elements: Elements, key: str | None, role: str, *, label: str | None = None) -> Rect:
    matches = [
        item
        for item in elements
        if item.get("key") == key and item.get("role") == role and (label is None or item.get("label") == label)
    ]
    if len(matches) != 1:
        raise RuntimeError(f"Expected one {role} element keyed {key!r}, found {len(matches)}")
    rect = matches[0].get("rect")
    if (
        not isinstance(rect, list)
        or len(rect) != 4
        or not all(isinstance(value, (float, int)) and math.isfinite(value) for value in rect)
        or min(rect[2:]) <= 0
    ):
        raise RuntimeError(f"Invalid rectangle for {key}: {rect!r}")
    return cast(Rect, tuple(rect))


def require_inside(inner: Rect, outer: Rect, label: str) -> None:
    x, y, width, height = inner
    left, top, outer_width, outer_height = outer
    if not (
        left - 1 <= x and top - 1 <= y and x + width <= left + outer_width + 1 and y + height <= top + outer_height + 1
    ):
        raise RuntimeError(f"{label} extends past its containing pane: {inner} vs {outer}")


def assert_studio_layout(
    elements: Elements,
    *,
    sidebar: bool = True,
    inspector: bool = False,
    compare: bool = False,
    size: tuple[int, int] = (1280, 820),
) -> None:
    main = element_rect(elements, "main-pane", "main")
    if compare:
        working = element_rect(elements, "compare-body", "region", label="Draft comparison")
    else:
        working = element_rect(elements, "prose-input", "textbox", label="Working draft")
    require_inside(working, main, "Writing surface")
    panes = [main]
    if sidebar:
        left = element_rect(elements, "sidebar-pane", "region", label="Sidebar")
        require_inside(element_rect(elements, "workspace-files", "tree", label="Files"), left, "Workspace tree")
        panes.insert(0, left)
    if inspector:
        panes.append(element_rect(elements, "inspector-pane", "region", label="Inspector"))
    for pane in panes:
        require_inside(pane, (0.0, 0.0, *size), "Studio pane")
    for left, right in pairwise(panes):
        if left[0] + left[2] > right[0] + 1:
            raise RuntimeError(f"Studio panes overlap or appear out of order: {left} vs {right}")


def read_elements(path: Path) -> Elements:
    return cast(Elements, json.loads(path.read_text(encoding="utf-8"))["elements"])


def _paeth(left: int, up: int, corner: int) -> int:
    forecast = left + up - corner
    a, b, c = abs(forecast - left), abs(forecast - up), abs(forecast - corner)
    if a <= b and a <= c:
        return left
    return up if b <= c else corner


def _png_scanlines(data: bytes, expected: tuple[int, int]) -> tuple[bytes, int]:
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError("missing PNG signature")
    offset = len(PNG_SIGNATURE)
    channels = 0
    compressed = bytearray()
    while offset + 12 <= len(data):
        size, kind = struct.unpack_from(">I4s", data, offset)
        end = offset + 8 + size
        if end + 4 > len(data):
            raise ValueError("truncated PNG chunk")
        body = data[offset + 8 : end]
        (checksum,) = struct.unpack_from(">I", data, end)
        if zlib.crc32(kind + body) != checksum:
            raise ValueError(f"bad checksum for PNG chunk {kind!r}")
        if offset == len(PNG_SIGNATURE):
            if kind != b"IHDR" or size != 13:
                raise ValueError("missing PNG header")
            width, height, depth, color, mode, filtering, interlace = struct.unpack(">IIBBBBB", body)
            if (width, height) != expected:
                raise ValueError(f"expected {expected[0]}x{expected[1]}, got {width}x{height}")
            if depth != 8 or color not in (2, 6) or mode or filtering or interlace:
                raise ValueError("expected an 8-bit non-interlaced RGB or RGBA render")
            channels = 4 if color == 6 else 3
        elif kind == b"IDAT":
            compressed.extend(body)
        elif kind == b"IEND" and size == 0 and end + 4 == len(data):
            break
        offset = end + 4
    else:
        raise ValueError("missing final PNG chunk")

    required = expected[1] * (expected[0] * channels + 1)
    decoder = zlib.decompressobj()
    pixels = decoder.decompress(compressed, required + 1)
    if len(pixels) != required or not decoder.eof or decoder.unused_data:
        raise ValueError("wrong number of PNG scanline bytes")
    return pixels, channels


def _unfilter(row: bytearray, previous: bytearray, filter_type: int, channels: int) -> None:
    if filter_type == 0:
        return
    for index, value in enumerate(row):
        left = row[index - channels] if index >= channels else 0
        up = previous[index]
        if filter_type == 1:
            prediction = left
        elif filter_type == 2:
            prediction = up
        elif filter_type == 3:
            prediction = (left + up) // 2
        elif filter_type == 4:
            corner = previous[index - channels] if index >= channels else 0
            prediction = _paeth(left, up, corner)
        else:
            raise ValueError(f"unknown PNG scanline filter {filter_type}")
        row[index] = (value + prediction) & 255


def validate_png(path: Path, expected: tuple[int, int]) -> None:
    """Decode Rune's RGB/RGBA PNGs and reject blank, solid, or transparent renders."""
    try:
        scanlines, channels = _png_scanlines(path.read_bytes(), expected)
        stride = expected[0] * channels
        previous = bytearray(stride)
        darkest = [255, 255, 255]
        lightest = [0, 0, 0]
        varied = False
        for start in range(0, len(scanlines), stride + 1):
            row = bytearray(scanlines[start + 1 : start + 1 + stride])
            _unfilter(row, previous, scanlines[start], channels)
            previous = row
            if varied:
                continue
            for offset in range(0, stride, channels):
                if channels == 4 and row[offset + 3] < 16:
                    continue
                for c in range(3):
                    value = row[offset + c]
                    darkest[c] = min(darkest[c], value)
                    lightest[c] = max(lightest[c], value)
            varied = any(high - low >= 8 for low, high in zip(darkest, lightest, strict=True))
        if not varied:
            raise ValueError("visible pixels have no meaningful color variance")
    except (OSError, ValueError, zlib.error) as exc:
        raise RuntimeError(f"Invalid screenshot {path.name}: {exc}") from exc


def _cli_cmd(executable: str, *args: str) -> list[str]:
    if os.name == "nt" and executable.lower().endswith((".cmd", ".bat")):
        return ["cmd", "/c", executable, *args]
    return [executable, *args]


def run(args: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        env={**(os.environ if env is None else env), "PYTHONUTF8": "1"},
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"{args!r} exited {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def run_editor(args: list[str], *, cwd: Path, env: dict[str, str]) -> None:
    run([str(EDITOR), *args, "--scale", "1"], cwd=cwd, env=env)
    if "--snap" in args:
        size = args[args.index("--size") + 1]
        width, height = (int(part) for part in size.split("x"))
        validate_png(Path(args[args.index("--snap") + 1]), (width, height))


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
) -> Elements:
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
    run_editor(
        [
            *launch,
            "--script",
            str(script_path),
            "--at",
            f"mark:{mark}",
            "--size",
            STUDIO_SIZE,
            "--realtime",
            "--tree",
            str(tree),
            *options,
        ],
        cwd=base,
        env=env,
    )
    return read_elements(tree)


def studio_mentions(elements: Elements, text: str) -> bool:
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


def diff_layout(elements: Elements, layout: str) -> bool:
    return any(element.get("label") == layout and element.get("itemSelected") is True for element in elements)


def smoke_linked_session(base: Path, workspace: Path, voice: Path, draft: Path, env: dict[str, str]) -> None:
    linked_env = studio_home(base, "linked", env)
    launch = ("--folder", str(workspace))
    unified = studio_scene(base, linked_env, "linked-compare", "workspace-unified", *launch)
    assert_studio_layout(unified, compare=True)
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
    assert_studio_layout(split, compare=True)
    if not diff_layout(split, "Split") or not studio_mentions(split, "Baseline vs draft"):
        raise RuntimeError("Linked draft did not switch back to Split Compare")
    signoff = studio_scene(base, linked_env, "linked-finish", "workspace-signoff", *launch, mark="signoff")
    dialog = element_rect(signoff, "finish-session-dialog", "dialog", label="Finish Edit Session")
    require_inside(dialog, (0.0, 0.0, 1280, 820), "Sign-off dialog")
    require_inside(element_rect(signoff, "finish-notes", "textbox"), dialog, "Sign-off notes")
    require_inside(element_rect(signoff, "confirm-finish", "button", label="Finish & Learn"), dialog, "Finish button")
    prior_stats = (voice / "voice-stats.json").read_bytes()
    finished = studio_scene(base, linked_env, "linked-finish", "workspace-finished", *launch)
    assert_studio_layout(finished, compare=True)
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
    before_open = studio_scene(
        base, welcome_env, "workspace-welcome", "initial-welcome", workspace=workspace, mark="welcome"
    )
    open_folder = element_rect(before_open, None, "button", label="Open Folder…")
    require_inside(open_folder, (0.0, 0.0, 1280, 820), "Welcome Open Folder button")
    welcome = studio_scene(base, welcome_env, "workspace-welcome", "workspace-welcome", workspace=workspace)
    assert_studio_layout(welcome)
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
    assert_studio_layout(inspector, inspector=True)
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
        run_editor(
            [
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
        if not (project / str(receipt["learnedPath"])).is_dir():
            raise RuntimeError("Editor did not learn the finished edit")
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
        run_editor(
            [
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
        run_editor(
            [
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
        reopened_elements = read_elements(tree)
        assert_studio_layout(reopened_elements, sidebar=False, inspector=True, size=(1120, 800))
        recovered = base / "recovered.png"
        run_editor(
            [
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

        # Closing before the 800 ms autosave timer expires still flushes dirty edits.
        unsaved_draft = base / "unsaved-close.md"
        unsaved_draft.write_text("Initial unsaved close draft.\n", encoding="utf-8")
        run_editor(
            [
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
        run_editor(
            [
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
        tall_elements = read_elements(tall_tree)
        assert_studio_layout(tall_elements, sidebar=False, inspector=True, size=(1120, 1040))
        normal_height = element_rect(reopened_elements, "prose-input", "textbox")[3]
        tall_height = element_rect(tall_elements, "prose-input", "textbox")[3]
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
        run_editor(
            [
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
