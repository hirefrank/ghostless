"""Install the packed npm artifact and exercise its installed CLI, offline."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(args: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> str:
    completed = subprocess.run(args, cwd=cwd, env=env, check=False, text=True,
                               capture_output=True)
    if completed.returncode:
        raise RuntimeError(
            f"{args!r} exited {completed.returncode}\n{completed.stdout}\n{completed.stderr}"
        )
    return completed.stdout


def main() -> int:
    npm = shutil.which("npm.cmd" if os.name == "nt" else "npm")
    if npm is None:
        raise RuntimeError("npm is required for the artifact smoke test")
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        pack = json.loads(run([npm, "pack", "--json", "--pack-destination", str(base)], cwd=ROOT))
        files = {row["path"] for row in pack[0]["files"]}
        required = {"bin/ghostless.cjs", "lib/ghostless/harness.py", "lib/ghostless/cli.py",
                    "lib/ghostless/sessions.py", "skills/ghostless-draft/SKILL.md"}
        if not required.issubset(files):
            raise RuntimeError(f"Missing packaged files: {sorted(required - files)}")
        archive = base / pack[0]["filename"]
        prefix = base / "npm-prefix"
        run([npm, "install", "--global", "--prefix", str(prefix), str(archive)], cwd=base)
        executable = prefix / ("ghostless.cmd" if os.name == "nt" else "bin/ghostless")
        if not executable.is_file():
            raise RuntimeError(f"npm did not install the CLI: {executable}")

        env = os.environ.copy()
        env.update({
            "GHOSTLESS_PYTHON": sys.executable,
            "GHOSTLESS_NO_UPDATE": "1",
            "GHOSTLESS_HOME": str(base / "home"),
            "GHOSTLESS_SKILLS_DIR": str(base / "skills"),
        })
        cli = lambda *args: run([str(executable), *args], cwd=base, env=env)
        assert "ghostless 0.1.0" in cli("--version")
        cli("install-skills")
        skill = base / "skills" / "ghostless-draft"
        assert (skill / "SKILL.md").is_file()
        assert (skill / ".ghostless-installed").is_file() if os.name == "nt" else skill.is_symlink()

        project = base / "voice"
        cli("init", str(project))
        (project / "corpus" / "gold" / "mine.md").write_text(
            "I write this myself. And I write another sentence.\n", encoding="utf-8"
        )
        cli("build", "--project", str(project))
        assert (project / "voice-stats.json").is_file()
        draft = base / "draft.md"
        draft.write_text("I write this myself.\n", encoding="utf-8")
        final = base / "final.md"
        final.write_text("I wrote this myself. And I kept the ending.\n", encoding="utf-8")
        cli("learn", "--before", str(draft), "--after", str(final), "--project", str(project))
        cli("build", "--project", str(project))
        # A mechanical mismatch may return 1; inspect the report rather than its verdict.
        judged = subprocess.run([str(executable), "judge", str(draft), "--project", str(project)],
                                cwd=base, env=env, check=False, text=True, capture_output=True)
        if judged.returncode not in (0, 1) or "ghostless mechanical check" not in judged.stdout:
            raise RuntimeError(f"Installed judge failed: {judged.stdout}\n{judged.stderr}")
        # Edit sessions (from #8): start, snapshot, and finish through the
        # installed CLI, then learn once into gold/.
        session = json.loads(cli("start", str(draft), "--project", str(project), "--json",
                                 "--source", "https://example.org/reference"))
        session_dir = project / session["path"]
        if (session_dir / "before.md").read_text(encoding="utf-8") != draft.read_text(encoding="utf-8"):
            raise RuntimeError("The session baseline differs from the draft")
        snap = subprocess.run(
            [str(executable), "snapshot", "--session", session["id"], "--project", str(project)],
            cwd=base, env=env,
            input=json.dumps({"after": "A better line.\n", "notes": "Cut the filler."}),
            text=True, capture_output=True, check=False,
        )
        if snap.returncode:
            raise RuntimeError(f"Installed snapshot failed: {snap.stdout}\n{snap.stderr}")
        receipt = json.loads(cli("finish", "--session", session["id"], "--project", str(project), "--json"))
        if receipt["sources"] != ["https://example.org/reference"]:
            raise RuntimeError(f"Session sources not recorded: {receipt.get('sources')!r}")
        if (session_dir / "after.md").read_text(encoding="utf-8") != "A better line.\n":
            raise RuntimeError("The session final was not stored")
        if json.loads(cli("finish", "--session", session["id"], "--project", str(project), "--json")) != receipt:
            raise RuntimeError("Finish was not idempotent")
        print(f"Installed artifact smoke passed on {sys.platform}: {archive.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
