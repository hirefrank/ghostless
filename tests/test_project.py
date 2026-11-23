"""Project lifecycle, editorial notes, default-project precedence, git migration, and version checks."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless import __version__  # noqa: E402
from ghostless.build import build_voice  # noqa: E402
from ghostless.cli import main  # noqa: E402
from ghostless.paths import ProjectError, find_project_with_source  # noqa: E402
from ghostless.project import (  # noqa: E402
    GITIGNORE_MARKER,
    LEGACY_GITIGNORE_BLOCK,
    LEGACY_GITIGNORE_MARKER,
    discover,
    init_project,
    load_manifest,
    migrate_project,
    new_session_id,
)


class SampleProjectTests(unittest.TestCase):
    def test_sample_is_invented(self) -> None:
        blob = "\n".join(
            path.read_text(encoding="utf-8")
            for path in SAMPLE.rglob("*")
            if path.is_file()
        )
        self.assertNotIn("Frank Harris", blob)
        self.assertIn("SAMPLE DATA", (SAMPLE / "README.md").read_text(encoding="utf-8"))

    def test_build_tiers_sections_and_bans(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            stats = build_voice(project)
            self.assertFalse(stats["bronzeFormatOnly"])
            ban = stats["weighted"]["banList"]
            used = stats["weighted"]["tellsInUse"]
            self.assertIn("delve", ban)
            self.assertIn("leverage", ban)
            self.assertIn("furthermore", ban)
            self.assertIn("foster", used)
            self.assertNotIn("foster", ban)
            phrases = [item["phrase"] for item in stats["weighted"]["signaturePhrases"]]
            self.assertIn("cold coffee", phrases)
            self.assertEqual(stats["weighted"]["metrics"]["emDashesPer1k"], 0)
            notes = stats["sections"]["notes"]["weighted"]["metrics"]["medianSentenceLength"]
            archive = stats["sections"]["archive"]["weighted"]["metrics"]["medianSentenceLength"]
            self.assertIsNotNone(notes)
            self.assertIsNotNone(archive)
            self.assertGreater(archive, notes)
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertNotIn("purple hedgehog", guide)
            self.assertIn("cold coffee", guide)
            self.assertIn("Unweighted", guide)

            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            manifest["goldVoiceMinimum"] = 1
            (project / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            limited = build_voice(project)
            self.assertTrue(limited["bronzeFormatOnly"])
            self.assertIn("foster", limited["weighted"]["banList"])
            self.assertNotIn("foster", limited["weighted"]["tellsInUse"])
            archive_metrics = limited["sections"]["archive"]["weighted"]["metrics"]
            self.assertIsNone(archive_metrics["medianSentenceLength"])
            self.assertIsNotNone(archive_metrics["medianParagraphLength"])


class NotesTests(unittest.TestCase):
    def test_notes_fold_into_guide_and_export(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            (project / "STYLE.md").write_text(
                "# Style\n\n- Every claim needs a source.\n", encoding="utf-8"
            )
            (project / "AUDIENCE.md").write_text(
                "# Audience\n\n- Engineers who skim.\n", encoding="utf-8"
            )
            stats = build_voice(project)
            self.assertEqual(stats["style"], "# Style\n\n- Every claim needs a source.")
            self.assertEqual(stats["audience"], "# Audience\n\n- Engineers who skim.")
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("## Style", guide)
            self.assertIn("Every claim needs a source.", guide)
            self.assertIn("## Audience", guide)
            self.assertIn("Engineers who skim.", guide)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["export", "--project", str(project)]), 0)
            paste = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertIn("Every claim needs a source.", paste)
            self.assertIn("Engineers who skim.", paste)

    def test_missing_notes_render_a_pointer(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            stats = build_voice(project)
            self.assertIsNone(stats["style"])
            self.assertIsNone(stats["audience"])
            guide = (project / "voice-guide.md").read_text(encoding="utf-8")
            self.assertIn("No STYLE.md yet", guide)
            self.assertIn("No AUDIENCE.md yet", guide)

    def test_save_appends_dated_rule_and_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    main(["save", "--style", "Every claim needs a source.", "--project", str(project)]),
                    0,
                )
            style = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertIn("# Style", style)
            self.assertIn("Every claim needs a source.", style)
            # The same rule on the same day must not add a duplicate line.
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    main(["save", "--style", "Every claim needs a source.", "--project", str(project)]),
                    0,
                )
            second = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertEqual(second.count("Every claim needs a source."), 1)
            # A different kind routes to the other file.
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    main(["save", "--audience", "Engineers who skim.", "--project", str(project)]),
                    0,
                )
            audience = (project / "AUDIENCE.md").read_text(encoding="utf-8")
            self.assertIn("# Audience", audience)
            self.assertIn("Engineers who skim.", audience)

    def test_save_does_not_treat_prefix_as_duplicate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            with contextlib.redirect_stdout(io.StringIO()):
                main(["save", "--style", "Every claim needs a source, and a link.", "--project", str(project)])
                # A shorter same-day rule that is only a prefix of the stored line
                # must still append, not be mistaken for the line already present.
                self.assertEqual(
                    main(["save", "--style", "Every claim needs a source", "--project", str(project)]),
                    0,
                )
            style = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertEqual(style.count("Every claim needs a source"), 2)

    def test_save_collapses_newlines_to_a_single_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    main(["save", "--style", "Every claim\nneeds a source.", "--project", str(project)]),
                    0,
                )
            style = (project / "STYLE.md").read_text(encoding="utf-8")
            self.assertIn("Every claim needs a source.", style)
            self.assertNotIn("Every claim\n", style)
            # Same-day idempotency holds once the rule is collapsed.
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    main(["save", "--style", "Every claim\nneeds a source.", "--project", str(project)]),
                    0,
                )
            self.assertEqual(
                (project / "STYLE.md").read_text(encoding="utf-8").count("Every claim needs a source."),
                1,
            )

    def test_save_rejects_empty_rule(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(["save", "--style", "   ", "--project", str(project)])
            self.assertEqual(code, 2)
            self.assertFalse((project / "STYLE.md").exists())

    def test_notes_are_tracked_not_measured(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project)
            (project / "STYLE.md").write_text("# Style\n", encoding="utf-8")
            (project / "AUDIENCE.md").write_text("# Audience\n", encoding="utf-8")
            (project / "corpus" / "gold" / "desk.md").write_text("I write.\n", encoding="utf-8")
            # Tracked: the voice-data rules never ignore them.
            self.assertFalse(_ignored(project, "STYLE.md"))
            self.assertFalse(_ignored(project, "AUDIENCE.md"))
            # Not measured: discovery reads corpus/, transcripts/, and gold/ only.
            pieces = discover(project, load_manifest(project))
            self.assertEqual([piece.path for piece in pieces], ["corpus/gold/desk.md"])


class ProjectTests(unittest.TestCase):
    def test_init_writes_data_repo_rules_and_local_git(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["init", str(project)]), 0)
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn(GITIGNORE_MARKER, ignore)
            self.assertNotIn(LEGACY_GITIGNORE_MARKER, ignore)
            self.assertTrue((project / ".git").is_dir())
            self.assertEqual(_git(project, "remote").stdout.strip(), "")
            # The writing, finished edit pairs, sessions, and holdouts are tracked.
            for tracked in (
                "manifest.json",
                "corpus/gold/desk.md",
                "corpus/silver/recent.md",
                "transcripts/talk.md",
                "gold/2026-01-02-notes/after.md",
                "sessions/2026-01-02-notes/before.md",
                "holdouts/kept.md",
            ):
                self.assertFalse(_ignored(project, tracked), tracked)
            # Generated output, drafts, temp files, and raw media stay out.
            for ignored in (
                "voice-stats.json",
                "voice-guide.md",
                "voice-guide.paste.md",
                "notes.paste.md",
                "edits-summary.json",
                "edits-summary.md",
                "stats/summary.json",
                "exports/guide.md",
                "drafts/ai.md",
                "raw/take.wav",
                "take.mp3",
                "take.m4a",
                "clip.mp4",
                "notes.md.tmp",
                ".DS_Store",
            ):
                self.assertTrue(_ignored(project, ignored), ignored)
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["kind"], "ghostless")
            self.assertEqual(manifest["weights"]["gold"], 3)
            self.assertEqual(manifest["goldVoiceMinimum"], 10)
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(["init", str(project)]), 2)
            # Appending twice must not duplicate the block.
            self.assertEqual(ignore.count(GITIGNORE_MARKER), 1)

    def test_scratch_dirs_are_root_only(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["init", str(project)]), 0)
            # A corpus section named like a scratch dir is still measured by
            # build, so it must stay tracked rather than be swallowed by an
            # unanchored ignore pattern.
            for section in ("drafts", "stats", "exports", "raw"):
                self.assertFalse(_ignored(project, f"corpus/gold/{section}/note.md"), section)
            # The top-level scratch dirs themselves are still ignored.
            for scratch in ("drafts", "stats", "exports", "raw"):
                self.assertTrue(_ignored(project, f"{scratch}/note.md"), scratch)

    def test_init_can_skip_git(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["init", "--no-git", str(project)]), 0)
            self.assertFalse((project / ".git").exists())
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn(GITIGNORE_MARKER, ignore)

    def test_rebuild_does_not_dirty_git(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project)
            (project / "corpus" / "gold" / "desk.md").write_text(
                "I write at the desk.\n\nI stop when the coffee is cold.\n",
                encoding="utf-8",
            )
            _commit_all(project)
            self.assertEqual(_git(project, "status", "--porcelain").stdout, "")

            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["build", "--project", str(project)]), 0)
                self.assertEqual(main(["export", "--project", str(project)]), 0)
            self.assertTrue((project / "voice-stats.json").is_file())
            self.assertTrue((project / "voice-guide.md").is_file())
            self.assertTrue((project / "voice-guide.paste.md").is_file())
            self.assertEqual(_git(project, "status", "--porcelain").stdout, "")

    def test_migrate_v01_project_keeps_writing_out_of_git(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            before = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertIn(LEGACY_GITIGNORE_MARKER, before)

            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["migrate", "--project", str(project)]), 0)
            ignore = (project / ".gitignore").read_text(encoding="utf-8")
            self.assertNotIn(LEGACY_GITIGNORE_MARKER, ignore)
            self.assertIn(GITIGNORE_MARKER, ignore)
            self.assertEqual(ignore.count(GITIGNORE_MARKER), 1)
            # Rules the writer wrote themselves survive.
            self.assertIn("node_modules/", ignore)

            # Nothing was staged, committed, or pushed by the migration.
            self.assertEqual(_git(project, "ls-files").stdout.strip(), "")
            self.assertEqual(_git(project, "diff", "--cached", "--name-only").stdout.strip(), "")
            self.assertNotEqual(_git(project, "rev-parse", "--verify", "HEAD").returncode, 0)
            self.assertEqual(_git(project, "remote").stdout.strip(), "")

            # Writing and finished edit pairs are trackable; generated files,
            # drafts, and raw media are still ignored.
            for tracked in (
                "corpus/gold/desk.md",
                "gold/2026-01-02-notes/before.md",
                "gold/2026-01-02-notes/after.md",
            ):
                self.assertFalse(_ignored(project, tracked), tracked)
            for ignored in ("voice-stats.json", "drafts/ai.md", "raw/take.wav", "notes.md.tmp"):
                self.assertTrue(_ignored(project, ignored), ignored)

            # Missing v0.1 directories were added, and nothing was deleted.
            self.assertTrue((project / "corpus" / "silver" / ".gitkeep").is_file())
            self.assertTrue((project / "transcripts" / ".gitkeep").is_file())
            self.assertTrue((project / "sessions" / ".gitkeep").is_file())
            self.assertTrue((project / "holdouts" / ".gitkeep").is_file())
            self.assertTrue((project / "corpus" / "gold" / "desk.md").is_file())

            # A second run is a no-op.
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["migrate", "--project", str(project)]), 0)
            self.assertEqual((project / ".gitignore").read_text(encoding="utf-8"), ignore)

    def test_migrate_prints_no_remote_in_parent_repo(self) -> None:
        _require_git(self)
        with tempfile.TemporaryDirectory() as tmp:
            parent = Path(tmp) / "parent"
            parent.mkdir()
            self.assertEqual(_git(parent, "init", "-q").returncode, 0)
            project = _write_v01_project(parent / "voice")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(main(["migrate", "--project", str(project)]), 0)
            text = stdout.getvalue()
            self.assertNotIn("git remote add origin", text)
            self.assertNotIn("git push", text)
            self.assertIn("parent", text.lower())

    def test_migrate_no_git_prints_no_git_steps(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(main(["migrate", "--project", str(project), "--no-git"]), 0)
            text = stdout.getvalue()
            self.assertNotIn("git remote add origin", text)
            self.assertNotIn("git push", text)
            self.assertNotIn("git commit", text)

    def test_init_refuses_a_v01_ignore_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = main(["init", str(project), "--force"])
            self.assertEqual(code, 2)
            self.assertIn("ghostless migrate", stderr.getvalue())
            self.assertFalse((project / ".git").exists())

    def test_migrate_reports_the_edited_default_for_v01_pairs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(
                    main(["migrate", "--project", str(project), "--no-git"]), 0
                )
            text = stdout.getvalue()
            self.assertIn("1 AI-first pair(s) are measured as edited", text)
            self.assertIn("ghostless promote <slug>", text)

    def test_migrate_refuses_a_hand_edited_v01_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = _write_v01_project(Path(tmp) / "voice")
            (project / ".gitignore").write_text(
                LEGACY_GITIGNORE_BLOCK.replace("gold/\n", ""),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ProjectError, "was edited"):
                migrate_project(project)
            # Fail closed: the edited rules are left exactly as they were.
            self.assertNotIn(
                GITIGNORE_MARKER,
                (project / ".gitignore").read_text(encoding="utf-8"),
            )

    def test_sessions_and_holdouts_are_not_measured(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            (project / "corpus" / "gold" / "desk.md").write_text("I write.\n", encoding="utf-8")
            (project / "sessions" / "2026-01-02-notes").mkdir(parents=True)
            (project / "sessions" / "2026-01-02-notes" / "before.md").write_text(
                "An AI draft.\n", encoding="utf-8"
            )
            (project / "holdouts" / "kept.md").write_text("Held out.\n", encoding="utf-8")
            pieces = discover(project, load_manifest(project))
            self.assertEqual([piece.path for piece in pieces], ["corpus/gold/desk.md"])

    def test_session_ids_follow_the_learn_slug(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            init_project(project, git=False)
            self.assertEqual(
                new_session_id(project, date="2026-01-02", section="Field Notes"),
                "2026-01-02-field-notes",
            )
            (project / "sessions" / "2026-01-02-field-notes").mkdir()
            self.assertEqual(
                new_session_id(project, date="2026-01-02", section="Field Notes"),
                "2026-01-02-field-notes-2",
            )
            self.assertEqual(
                new_session_id(project, date="2026-01-03"),
                "2026-01-03",
            )
            with self.assertRaisesRegex(ProjectError, "YYYY-MM-DD"):
                new_session_id(project, date="01/02/2026")

    def test_export_excerpts_are_opt_in(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["build", "--project", str(project)]), 0)
                self.assertEqual(main(["export", "--project", str(project)]), 0)
            paste = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertNotIn("purple hedgehog", paste)
            self.assertIn("Ban list", paste)
            self.assertIn("cold coffee", paste)  # signature phrase, included by default
            self.assertIn("paste", paste.lower())
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["export", "--project", str(project), "--excerpts"]), 0)
            with_bits = (project / "voice-guide.paste.md").read_text(encoding="utf-8")
            self.assertIn("purple hedgehog", with_bits)

    def test_judge_command_exit_codes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "voice"
            shutil.copytree(SAMPLE, project)
            build_voice(project)
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(
                    main(["judge", str(project / "drafts" / "ai-ish.md"), "--project", str(project)]),
                    1,
                )
                self.assertEqual(
                    main(["judge", str(project / "drafts" / "closer.md"), "--project", str(project)]),
                    0,
                )
                self.assertEqual(
                    main(
                        [
                            "judge",
                            str(project / "drafts" / "closer.md"),
                            "--project",
                            str(project),
                            "--section",
                            "missing",
                        ]
                    ),
                    2,
                )


class DefaultProjectTests(unittest.TestCase):
    def test_precedence_explicit_env_cwd_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            default = base / "default-voice"
            nearby = base / "nearby-voice"
            init_project(default, git=False)
            init_project(nearby, git=False)
            elsewhere = base / "elsewhere"
            elsewhere.mkdir()
            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(main(["use", str(default)]), 0)
                self.assertEqual(
                    json.loads((home / "config.json").read_text())["defaultProject"],
                    str(default.resolve()),
                )
                old = Path.cwd()
                try:
                    os.chdir(elsewhere)
                    # No manifest above, no env: the default wins.
                    self.assertEqual(
                        find_project_with_source(), (default.resolve(), "defaultProject")
                    )
                    # A cwd manifest beats the default.
                    os.chdir(nearby)
                    self.assertEqual(
                        find_project_with_source(), (nearby.resolve(), "cwd")
                    )
                    # The env var beats the cwd manifest.
                    with patch.dict(os.environ, {"GHOSTLESS_PROJECT": str(default)}):
                        self.assertEqual(
                            find_project_with_source(), (default.resolve(), "GHOSTLESS_PROJECT")
                        )
                        # An explicit path beats the env var.
                        self.assertEqual(
                            find_project_with_source(nearby), (nearby.resolve(), "--project")
                        )
                finally:
                    os.chdir(old)

    def test_use_preserves_other_config_keys_and_rejects_non_project(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            home.mkdir()
            (home / "config.json").write_text('{"updateCheck": false}\n', encoding="utf-8")
            project = base / "voice"
            init_project(project, git=False)
            plain = base / "plain"
            plain.mkdir()
            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(main(["use", str(project)]), 0)
                config = json.loads((home / "config.json").read_text())
                self.assertEqual(config["updateCheck"], False)
                self.assertEqual(config["defaultProject"], str(project.resolve()))
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    self.assertEqual(main(["use", str(plain)]), 2)
                self.assertEqual(
                    json.loads((home / "config.json").read_text())["defaultProject"],
                    str(project.resolve()),
                )

    def test_stale_default_project_fails_loudly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            home.mkdir()
            stale = base / "gone"
            stale.mkdir()
            (home / "config.json").write_text(
                json.dumps({"defaultProject": str(stale)}), encoding="utf-8"
            )
            old = Path.cwd()
            try:
                os.chdir(base)
                with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                    with self.assertRaises(ProjectError) as caught:
                        find_project_with_source()
                self.assertIn("Default project", str(caught.exception))
            finally:
                os.chdir(old)

    def test_locate_and_doctor_show_which_source_won(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            home = base / "home"
            project = base / "my-voice"
            init_project(project, git=False)
            elsewhere = base / "elsewhere"
            elsewhere.mkdir()
            with patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(main(["use", str(project)]), 0)
                old = Path.cwd()
                try:
                    os.chdir(elsewhere)
                    out = io.StringIO()
                    with contextlib.redirect_stdout(out):
                        self.assertEqual(main(["locate"]), 0)
                    self.assertIn(f"{project.resolve()} (via defaultProject)", out.getvalue())
                    out = io.StringIO()
                    with contextlib.redirect_stdout(out):
                        self.assertEqual(main(["doctor"]), 0)
                    self.assertIn(f"Project: {project.resolve()} (via defaultProject)", out.getvalue())
                finally:
                    os.chdir(old)


class VersionFileTests(unittest.TestCase):
    def test_versions_match(self) -> None:
        package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        pyproject = (ROOT / "pyproject.toml").read_text(encoding="utf-8")

        self.assertEqual(package["name"], "ghostless")
        self.assertEqual(package["version"], "0.1.0")
        self.assertEqual(__version__, "0.1.0")
        self.assertIn('version = "0.1.0"', pyproject)
        self.assertIn('name = "ghostless"', pyproject)


def _require_git(test: unittest.TestCase) -> None:
    if shutil.which("git") is None:
        test.skipTest("git is not installed")


def _git(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=str(cwd),
        check=False,
        capture_output=True,
        text=True,
    )


def _ignored(cwd: Path, relative: str) -> bool:
    """True when .gitignore matches the path, using Git's own matcher."""
    result = _git(cwd, "check-ignore", "--no-index", "-q", relative)
    if result.returncode not in (0, 1):
        raise AssertionError(result.stderr)
    return result.returncode == 0


def _commit_all(root: Path) -> None:
    added = _git(root, "add", "-A")
    if added.returncode != 0:
        raise AssertionError(added.stderr)
    committed = _git(
        root,
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "-q",
        "-m",
        "test: voice data",
    )
    if committed.returncode != 0:
        raise AssertionError(committed.stderr)


def _write_v01_project(root: Path) -> Path:
    """A representative v0.1 project: old ignore block, writing, generated output."""
    for relative in ("corpus/gold", "corpus/silver", "corpus/bronze", "corpus/spoken", "transcripts"):
        (root / relative).mkdir(parents=True)
    pair = root / "gold" / "2026-01-02-notes"
    pair.mkdir(parents=True)
    (root / "manifest.json").write_text(
        json.dumps(
            {
                "kind": "ghostless",
                "version": 1,
                "weights": {"gold": 3, "silver": 1, "bronze": 0.5},
                "goldVoiceMinimum": 10,
                "updateCheck": True,
                "failUnder": 70,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    (root / "corpus" / "gold" / "desk.md").write_text(
        "I write at the desk.\n\nI stop when the coffee is cold.\n",
        encoding="utf-8",
    )
    (pair / "before.md").write_text("I write at my desk.\n", encoding="utf-8")
    (pair / "after.md").write_text("I write at the desk.\n", encoding="utf-8")
    (pair / "meta.json").write_text(
        json.dumps(
            {
                "date": "2026-01-02",
                "section": "notes",
                "origin": "ai-first",
                "kind": "prose",
                "beforeFile": "before.md",
                "afterFile": "after.md",
            }
        )
        + "\n",
        encoding="utf-8",
    )
    (root / "voice-stats.json").write_text("{}\n", encoding="utf-8")
    (root / "voice-guide.md").write_text("# voice guide\n", encoding="utf-8")
    (root / "drafts").mkdir()
    (root / "drafts" / "ai.md").write_text("An AI draft.\n", encoding="utf-8")
    (root / "raw").mkdir()
    (root / "raw" / "take.wav").write_text("RIFF", encoding="utf-8")
    (root / ".gitignore").write_text(
        "node_modules/\n\n" + LEGACY_GITIGNORE_BLOCK,
        encoding="utf-8",
    )
    return root


if __name__ == "__main__":
    unittest.main()
