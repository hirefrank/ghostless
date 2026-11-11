"""Voice commands. Skill install, update, and doctor live on the bash launcher."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from ghostless.export import write_export
from ghostless.guide import render_guide
from ghostless.judge import format_report, judge_text
from ghostless.learn import learn_edit, load_edit_summary, promote_final, refresh_edit_summary
from ghostless.paths import ProjectError, data_home, find_project, suite_root, version
from ghostless.project import (
    TRACKED_PATHS,
    discover,
    ensure_local_git,
    git_toplevel,
    init_project,
    load_manifest,
    migrate_project,
    placement_for,
    read_provenance,
)
from ghostless.stats import compile_stats

EPILOG = """
Launcher commands:
  install-skills    Link skills into detected harnesses
  update            Install the latest release and relink skills
  doctor            Check Python, the data dir, and skill links
  agents            List harnesses and skill directories
  routing           Print a project-instruction snippet
  reset             Remove installed skill links
  uninstall         reset --data

Disable the daily release check with GHOSTLESS_NO_UPDATE=1, GHOSTLESS_UPDATE_CHECK=0,
~/.ghostless/config.json {"updateCheck": false}, or "updateCheck": false in the manifest.
The check only GETs the public GitHub release. It does not send your writing.
"""


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="ghostless",
        description="Measure a local writing corpus and judge drafts against that voice.",
        epilog=EPILOG,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("-V", "--version", action="store_true", help="Print the version and exit")
    sub = parser.add_subparsers(dest="command")

    init_parser = sub.add_parser("init", help="Create a voice project directory and manifest")
    init_parser.add_argument("directory", nargs="?", help="Project directory (default: ./voice)")
    init_parser.add_argument("--here", action="store_true", help="Use the current directory")
    init_parser.add_argument("--force", action="store_true", help="Rewrite manifest defaults")
    init_parser.add_argument(
        "--no-git",
        action="store_true",
        help="Do not create local Git history (ignore rules are still written)",
    )

    migrate_parser = sub.add_parser(
        "migrate",
        help="Upgrade a v0.1 voice directory to the voice-data repository layout",
    )
    _add_project(migrate_parser)
    migrate_parser.add_argument(
        "--no-git",
        action="store_true",
        help="Do not create local Git history (ignore rules are still written)",
    )

    build_parser = sub.add_parser("build", help="Compute weighted stats and write voice-guide.md")
    _add_project(build_parser)
    build_parser.add_argument("--json", action="store_true", help="Print voice-stats.json")

    judge_parser = sub.add_parser("judge", help="Score a draft against the voice guide")
    judge_parser.add_argument("draft", help="Draft markdown file")
    judge_parser.add_argument("--section", help="Use targets for this section")
    _add_project(judge_parser)
    judge_parser.add_argument("--json", action="store_true")

    learn_parser = sub.add_parser("learn", help="Store a before/after edit and its provenance")
    learn_parser.add_argument("--before", required=True, help="Earlier draft (often the AI draft)")
    learn_parser.add_argument("--after", required=True, help="Writer's final")
    learn_parser.add_argument("--section", help="Section tag stored on the learned piece")
    learn_parser.add_argument(
        "--origin",
        default="ai-first",
        choices=("ai-first", "writer-first"),
        help="ai-first: AI draft then the writer's edit (default). writer-first: the writer drafted first.",
    )
    learn_parser.add_argument("--model", help="Model that wrote the first draft, when known")
    learn_parser.add_argument("--tool", help="Tool or harness that produced the first draft, when known")
    learn_parser.add_argument(
        "--promote",
        action="store_true",
        help="Count this final as gold now instead of edited. Prints what that changes.",
    )
    learn_parser.add_argument("--date", help="YYYY-MM-DD (default: today, UTC)")
    _add_project(learn_parser)

    promote_parser = sub.add_parser(
        "promote",
        help="Deliberately count a learned final as gold (or return it to edited)",
    )
    promote_parser.add_argument("slug", help="Pair id under gold/, e.g. 2026-01-02-notes")
    promote_parser.add_argument(
        "--demote",
        action="store_true",
        help="Move it back to the edited tier without deleting the edit receipt",
    )
    _add_project(promote_parser)

    export_parser = sub.add_parser("export", help="Write one paste-ready voice guide")
    export_parser.add_argument(
        "--excerpts",
        action="store_true",
        help="Also include a few short gold excerpts. Off by default; the export "
        "already contains targets, the ban list, signature phrases, per-section "
        "targets, and edit vocabulary.",
    )
    export_parser.add_argument("--out", help="Output path (default: voice-guide.paste.md)")
    _add_project(export_parser)

    sub.add_parser("locate", help="Print the voice project directory")
    which_parser = sub.add_parser("which", help="Print suite, data, and project paths")
    _add_project(which_parser)

    skills_parser = sub.add_parser("install-skills", help="Link skills into detected harnesses")
    skills_parser.add_argument("args", nargs=argparse.REMAINDER)
    setup_parser = sub.add_parser("setup", help="Alias for install-skills")
    setup_parser.add_argument("args", nargs=argparse.REMAINDER)
    for name, help_text in (
        ("update", "Install the latest release and relink skills"),
        ("doctor", "Check Python, the data dir, and skill links"),
        ("agents", "List harnesses and skill directories"),
        ("routing", "Print a project-instruction snippet"),
        ("reset", "Remove installed skill links"),
        ("uninstall", "Remove skill links and ~/.ghostless"),
    ):
        launcher_parser = sub.add_parser(name, help=help_text)
        launcher_parser.add_argument("args", nargs=argparse.REMAINDER)

    try:
        args = parser.parse_args(argv)
    except SystemExit as exc:
        code = exc.code
        return int(code) if isinstance(code, int) else 2

    if args.version:
        print(f"ghostless {version()}")
        return 0
    if not args.command:
        parser.print_help()
        return 0
    try:
        return _dispatch(args)
    except ProjectError as exc:
        print(f"ghostless: {exc}", file=sys.stderr)
        return 2
    except OSError as exc:
        print(f"ghostless: {exc}", file=sys.stderr)
        return 2


def _dispatch(args: argparse.Namespace) -> int:
    command = args.command
    match command:
        case "init":
            return _cmd_init(args)
        case "migrate":
            return _cmd_migrate(args)
        case "build":
            return _cmd_build(args)
        case "judge":
            return _cmd_judge(args)
        case "learn":
            return _cmd_learn(args)
        case "promote":
            return _cmd_promote(args)
        case "export":
            return _cmd_export(args)
        case "locate":
            print(find_project())
            return 0
        case "which":
            return _cmd_which(args)
        case "install-skills" | "setup":
            return _cmd_install_skills(args.args)
        case "update" | "doctor" | "agents" | "routing" | "reset" | "uninstall":
            return _cmd_launcher(command, args.args)
        case _:
            print(f"ghostless: unknown command {command}", file=sys.stderr)
            return 2


def _cmd_init(args: argparse.Namespace) -> int:
    if args.here and args.directory:
        print("ghostless: pass either a directory or --here.", file=sys.stderr)
        return 2
    if args.here:
        directory = Path.cwd()
    elif args.directory:
        directory = Path(args.directory)
    else:
        directory = Path.cwd() / "voice"
    created = init_project(directory, force=args.force, git=False)
    print(f"Created {created.resolve()}")
    print("  manifest.json")
    print("  corpus/gold  corpus/silver  corpus/bronze  corpus/spoken")
    print("  transcripts/  gold/  sessions/  holdouts/")
    _report_git(created, skip=args.no_git)
    print("Next:")
    print("  Put your own writing in corpus/gold (best), corpus/silver, and corpus/bronze.")
    print("  Put transcripts of your speech in corpus/spoken (your lines only).")
    print("  ghostless build")
    return 0


def _cmd_migrate(args: argparse.Namespace) -> int:
    root = _project(args.project)
    result = migrate_project(root)
    created = result.get("created")
    print(f"Voice data: {root}")
    if result.get("legacy_removed"):
        print("  .gitignore: removed the v0.1 block that kept corpus/, transcripts/, and gold/ out of Git")
    if result.get("changed"):
        print("  .gitignore: now tracks writing and ignores generated files, drafts, and raw media")
    else:
        print("  .gitignore: already on the voice-data layout")
    if isinstance(created, list) and created:
        print(f"  created {', '.join(str(item) for item in created)}")
    placements = _learned_placements(root)
    if placements["gold"] or placements["edited"]:
        print(
            f"  gold/: {placements['gold']} pair(s) stay gold, "
            f"{placements['edited']} AI-first pair(s) are measured as edited"
        )
        if placements["edited"]:
            print("  Promote one you stand behind: ghostless promote <slug>")
    state = _report_git(root, skip=args.no_git)
    print("Nothing was staged, committed, or pushed, and no remote was added.")
    if state in ("created", "existing"):
        print("Next, review what Git would track, then add it on purpose:")
        print(f"  cd {root}")
        print("  git status")
        print("  git add -- " + " ".join(TRACKED_PATHS))
        print('  git commit -m "Start voice-data history"')
        print("Optional private remote. Pushing sends your writing to that host:")
        print("  git remote add origin <private-repo-url>")
        print("  git push -u origin HEAD")
    elif state is not None and state.startswith("parent:"):
        print(f"Next, review in the parent repository at {state.removeprefix('parent:')}:")
        print(f"  cd {root}")
        print("  git status")
        print("  git add -- " + " ".join(TRACKED_PATHS))
        print("Do not add a remote here; that would attach to the parent repository.")
    return 0


def _report_git(root: Path, *, skip: bool) -> str | None:
    if skip:
        print("Git: skipped (--no-git). No repository was created.")
        return None
    state = ensure_local_git(root)
    match state:
        case "created":
            print(f"Git: created local history at {root} (no remote, nothing pushed).")
        case "existing":
            print(f"Git: using the existing local repository at {root} (no remote, nothing pushed).")
        case "unavailable":
            print("Git: unavailable; install git to version this voice data.")
        case _ if state.startswith("parent:"):
            print(
                f"Git: {root} is inside the existing repository at {state.removeprefix('parent:')}; "
                "no nested repository was created."
            )
        case _:
            print(f"Git: {state}")
    print("  Nothing was staged or committed. Commit when you choose.")
    return state


def _cmd_build(args: argparse.Namespace) -> int:
    root = _project(args.project)
    stats = build_voice(root)
    if args.json:
        print(json.dumps(stats, indent=2, ensure_ascii=False))
    else:
        print(_format_build(root, stats))
    return 0


def _cmd_judge(args: argparse.Namespace) -> int:
    root = _project(args.project)
    draft_path = Path(args.draft)
    if not draft_path.is_file():
        print(f"ghostless: draft not found: {draft_path}", file=sys.stderr)
        return 2
    stats = _load_stats(root)
    manifest = load_manifest(root)
    fail_under = manifest.get("failUnder")
    if not isinstance(fail_under, int):
        fail_under = 70
    report = judge_text(
        draft_path.read_text(encoding="utf-8"),
        stats,
        section=args.section,
        fail_under=fail_under,
    )
    if args.json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    else:
        print(format_report(report), end="")
    return 0 if report.get("ok") else 1


def _cmd_learn(args: argparse.Namespace) -> int:
    root = _project(args.project)
    result = learn_edit(
        root,
        before=Path(args.before),
        after=Path(args.after),
        section=args.section,
        origin=args.origin,
        date=args.date,
        model=args.model,
        tool=args.tool,
        promote=args.promote,
    )
    meta = result["meta"]
    if not isinstance(meta, dict):
        print(f"Stored gold/{result['slug']}", file=sys.stderr)
        return 0
    print(f"gold/{result['slug']}")
    print(f"  origin {meta.get('origin')}  section {meta.get('section') or 'unsectioned'}")
    if result.get("model") or result.get("tool"):
        print(f"  model/tool {result.get('model') or 'unnamed model'}"
              f"{' via ' + str(result.get('tool')) if result.get('tool') else ''}")
    print(
        f"  words {meta.get('wordsBefore')} → {meta.get('wordsAfter')} "
        f"(removed {meta.get('wordsRemoved')}, added {meta.get('wordsAdded')})"
    )
    print(
        f"  edit rate {meta.get('editRate')} "
        f"({meta.get('tokenEdits')} token edits / {meta.get('editRateDenominator')} aligned positions), "
        f"{meta.get('movedTokens')} moved tokens, "
        f"{meta.get('structuralChangeCount')} structural changes"
    )
    print(
        f"  sentence length {meta.get('medianSentenceLengthBefore')} → "
        f"{meta.get('medianSentenceLengthAfter')}"
    )
    print(
        f"  contractions per 1k {meta.get('contractionsPer1kBefore')} → "
        f"{meta.get('contractionsPer1kAfter')}"
    )
    print(f"  measured as {result.get('tier')} ({_placement_note(str(result.get('tier')))})")
    warning = result.get("warning")
    if isinstance(warning, str):
        print(f"  warning: {warning}")
    print("  The before/after pair is kept either way, and its diff feeds the guide's edit section.")
    if result.get("tier") == "gold":
        print("  after.md is gold. Run ghostless build to fold it into the guide.")
    else:
        print(
            "  after.md is edited, not gold: it shapes paragraph length only and never the "
            "ban list or rhythm."
        )
        print(f"  To count it as gold later: ghostless promote {result.get('slug')}")
    return 0


def _cmd_promote(args: argparse.Namespace) -> int:
    root = _project(args.project)
    manifest = load_manifest(root)
    minimum = manifest["goldVoiceMinimum"]
    before = _gold_prose_pieces(root)
    result = promote_final(root, args.slug, gold=not args.demote)
    after = _gold_prose_pieces(root)
    tier = result.get("tier")
    print(f"gold/{result.get('slug')}")
    if result.get("changed"):
        print(f"  {result.get('previous')} → {tier}")
    else:
        print(f"  unchanged: already {tier}")
    print(
        f"  origin {result.get('origin') or 'unspecified'}, confirmation "
        f"{result.get('confirmation')}"
    )
    warning = result.get("warning")
    if isinstance(warning, str):
        print(f"  warning: {warning}")
    if isinstance(minimum, int):
        print(
            f"  gold prose pieces: {before} → {after} (threshold {minimum}; "
            f"bronze is {'format-only' if after >= minimum else 'still in voice'})"
        )
    if tier == "gold":
        print("  This final now counts at gold weight and toward goldVoiceMinimum.")
    else:
        print(
            "  This final is back in the edited tier. Its edit receipt and vocabulary "
            "evidence stay in place."
        )
    print("Run ghostless build to fold the change into the guide.")
    return 0


def _gold_prose_pieces(root: Path) -> int:
    pieces = discover(root, load_manifest(root))
    return sum(1 for piece in pieces if piece.tier == "gold" and piece.kind == "prose")


def _learned_placements(root: Path) -> dict[str, int]:
    """How v0.1 and current pairs land under the provenance defaults."""
    counts = {"gold": 0, "edited": 0}
    folder = root / "gold"
    if not folder.is_dir():
        return counts
    for meta_path in sorted(folder.glob("*/meta.json")):
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            if not isinstance(meta, dict):
                continue
            provenance = read_provenance(meta, path=str(meta_path))
        except (OSError, json.JSONDecodeError, ProjectError):
            # A malformed pair fails loudly at build time, not during migrate.
            continue
        counts[placement_for(provenance)] += 1
    return counts


def _placement_note(tier: str) -> str:
    match tier:
        case "gold":
            return "gold weight, counts toward the bronze threshold"
        case "edited":
            return "paragraph length only, capped, not gold"
        case _:
            return tier


def _cmd_export(args: argparse.Namespace) -> int:
    root = _project(args.project)
    stats = _load_stats(root)
    destination = write_export(
        root,
        stats,
        excerpts=args.excerpts,
        out=Path(args.out) if args.out else None,
    )
    print(destination)
    if args.excerpts:
        print("Included short gold excerpts.")
    else:
        print("Excerpts omitted. Pass --excerpts to include short gold samples.")
    return 0


def _cmd_which(args: argparse.Namespace) -> int:
    root = suite_root()
    print(f"suite={root if root is not None else 'none'}")
    print(f"data={data_home()}")
    print(f"version={version()}")
    try:
        project = _project(args.project) if args.project else find_project()
    except ProjectError:
        print("project=none")
        return 0
    print(f"project={project}")
    return 0


def _cmd_launcher(command: str, extra: list[str]) -> int:
    """Hand installer commands to the bash launcher, which owns symlinks and npm."""
    root = suite_root()
    if root is None or not (root / "bin" / "ghostless").is_file():
        print(
            f"ghostless: `{command}` needs the full suite (npm install or a git checkout).",
            file=sys.stderr,
        )
        return 1
    env = os.environ.copy()
    env["GHOSTLESS_FROM_BIN"] = "1"
    completed = subprocess.run(
        [str(root / "bin" / "ghostless"), command, *extra],
        check=False,
        env=env,
    )
    return int(completed.returncode)


def _cmd_install_skills(extra: list[str]) -> int:
    root = suite_root()
    if root is None or not (root / "setup").is_file():
        print(
            "ghostless: install-skills needs the full suite (npm install or a git checkout).",
            file=sys.stderr,
        )
        return 1
    completed = subprocess.run(["bash", str(root / "setup"), *extra], check=False)
    return int(completed.returncode)


def build_voice(root: Path) -> dict[str, object]:
    manifest = load_manifest(root)
    pieces = discover(root, manifest)
    edits = refresh_edit_summary(root)
    if not edits.get("learns"):
        edits = load_edit_summary(root)
    raw_weights = manifest["weights"]
    if not isinstance(raw_weights, dict):
        raise ProjectError("manifest weights are missing.")
    weights = {str(key): float(value) for key, value in raw_weights.items()}
    minimum = manifest["goldVoiceMinimum"]
    if not isinstance(minimum, int):
        raise ProjectError("goldVoiceMinimum is missing.")
    stats = compile_stats(
        pieces,
        weights=weights,
        gold_voice_minimum=minimum,
        edits=edits,
    )
    stats["generatedAt"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    fail_under = manifest.get("failUnder")
    stats["failUnder"] = fail_under if isinstance(fail_under, int) else 70
    (root / "voice-stats.json").write_text(
        json.dumps(stats, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    (root / "voice-guide.md").write_text(render_guide(stats), encoding="utf-8")
    return stats


def _load_stats(root: Path) -> dict[str, object]:
    path = root / "voice-stats.json"
    if not path.is_file():
        print("No voice-stats.json yet. Building first.", file=sys.stderr)
        return build_voice(root)
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ProjectError("voice-stats.json is unreadable. Run ghostless build.")
    return data


def _project(explicit: str | None) -> Path:
    if explicit:
        return find_project(Path(explicit))
    return find_project()


def _add_project(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--project", help="Voice project directory (default: search from cwd)")


def _format_build(root: Path, stats: dict[str, object]) -> str:
    counts = stats.get("counts") if isinstance(stats.get("counts"), dict) else {}
    prose = counts.get("prose") if isinstance(counts, dict) and isinstance(counts.get("prose"), dict) else {}
    spoken = counts.get("spoken") if isinstance(counts, dict) and isinstance(counts.get("spoken"), dict) else {}
    spoken_total = 0
    if isinstance(spoken, dict):
        for value in spoken.values():
            if isinstance(value, int):
                spoken_total += value
    weighted = _metrics(stats.get("weighted"))
    unweighted = _metrics(stats.get("unweighted"))
    ban = _block(stats.get("weighted")).get("banList")
    ban_count = len(ban) if isinstance(ban, list) else 0
    lines = [
        f"voice guide: {root / 'voice-guide.md'}",
        (
            "pieces: "
            f"gold {prose.get('gold', 0) if isinstance(prose, dict) else 0}, "
            f"silver {prose.get('silver', 0) if isinstance(prose, dict) else 0}, "
            f"bronze {prose.get('bronze', 0) if isinstance(prose, dict) else 0}, "
            f"edited {prose.get('edited', 0) if isinstance(prose, dict) else 0}, "
            f"spoken {spoken_total}"
        ),
        f"bronze format-only: {'yes' if stats.get('bronzeFormatOnly') else 'no'}",
        (
            "gold prose pieces: "
            f"{stats.get('goldPieces', 0)} of {stats.get('goldVoiceMinimum')} to drop bronze from voice"
        ),
        (
            "median sentence: "
            f"{weighted.get('medianSentenceLength')} words "
            f"(unweighted {unweighted.get('medianSentenceLength')})"
        ),
        f"ban list: {ban_count}",
    ]
    warnings = stats.get("warnings")
    if isinstance(warnings, list):
        for warning in warnings:
            if isinstance(warning, str):
                lines.append(f"note: {warning}")
    return "\n".join(lines)


def _block(value: object) -> dict[str, object]:
    return value if isinstance(value, dict) else {}


def _metrics(value: object) -> dict[str, object]:
    metrics = _block(value).get("metrics")
    return metrics if isinstance(metrics, dict) else {}
