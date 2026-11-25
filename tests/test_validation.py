#!/usr/bin/env python3
"""Validation, path containment, and section inference tests (#39)."""

from __future__ import annotations

import contextlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.eval import evaluate, load_edit_pairs  # noqa: E402
from ghostless.learn import learn_edit, promote_final, refresh_edit_summary  # noqa: E402
from ghostless.paths import ProjectError, find_project_with_source  # noqa: E402
from ghostless.project import (  # noqa: E402
    discover,
    init_project,
    load_manifest,
    new_session_id,
    require_contained_path,
    validate_date,
)


class SectionAndTierInferenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)

    def test_frontmatter_kind_override_preserves_folder_section(self) -> None:
        notes_dir = self.project / "corpus" / "gold" / "notes"
        notes_dir.mkdir(parents=True)
        (notes_dir / "dictation.md").write_text(
            "---\nkind: spoken\n---\nI spoke this draft aloud before writing.\n",
            encoding="utf-8",
        )
        pieces = discover(self.project, load_manifest(self.project))
        self.assertEqual(len(pieces), 1)
        self.assertEqual(pieces[0].tier, "gold")
        self.assertEqual(pieces[0].kind, "spoken")
        self.assertEqual(pieces[0].section, "notes")

    def test_frontmatter_tier_override_preserves_folder_section(self) -> None:
        essays_dir = self.project / "corpus" / "silver" / "essays"
        essays_dir.mkdir(parents=True)
        (essays_dir / "piece.md").write_text(
            "---\ntier: gold\n---\nThis essay moved into gold via frontmatter.\n",
            encoding="utf-8",
        )
        pieces = discover(self.project, load_manifest(self.project))
        self.assertEqual(len(pieces), 1)
        self.assertEqual(pieces[0].tier, "gold")
        self.assertEqual(pieces[0].kind, "prose")
        self.assertEqual(pieces[0].section, "essays")

    def test_spoken_frontmatter_prose_override_preserves_section(self) -> None:
        standup_dir = self.project / "corpus" / "spoken" / "standup"
        standup_dir.mkdir(parents=True)
        (standup_dir / "talk.md").write_text(
            "---\nkind: prose\n---\nClean notes written down from standup.\n",
            encoding="utf-8",
        )
        pieces = discover(self.project, load_manifest(self.project))
        self.assertEqual(len(pieces), 1)
        self.assertEqual(pieces[0].tier, "silver")
        self.assertEqual(pieces[0].kind, "prose")
        self.assertEqual(pieces[0].section, "standup")

    def test_unpromoted_ai_first_after_cannot_escalate_to_gold_via_frontmatter(self) -> None:
        before = self.base / "before.md"
        after = self.base / "after.md"
        before.write_text("Furthermore, we leverage robust alignment.\n", encoding="utf-8")
        after.write_text(
            "---\ntier: gold\n---\nI keep the draft plain and cut the filler.\n",
            encoding="utf-8",
        )
        receipt = learn_edit(
            self.project,
            before=before,
            after=after,
            section="notes",
            origin="ai-first",
            date="2026-03-15",
        )
        self.assertEqual(receipt["tier"], "edited")
        manifest = load_manifest(self.project)
        pieces = discover(self.project, manifest)
        learned = [piece for piece in pieces if piece.learned]
        self.assertEqual(len(learned), 1)
        self.assertEqual(learned[0].tier, "edited")
        self.assertEqual(learned[0].weight, manifest["weights"]["edited"])

    def test_promoted_learned_pair_cannot_be_downgraded_via_frontmatter(self) -> None:
        before = self.base / "before.md"
        after = self.base / "after.md"
        before.write_text("AI draft line.\n", encoding="utf-8")
        after.write_text(
            "---\ntier: bronze\n---\nI rewrote every sentence in my own voice.\n",
            encoding="utf-8",
        )
        learn_edit(
            self.project,
            before=before,
            after=after,
            section="notes",
            origin="writer-first",
            date="2026-03-16",
        )
        pieces = discover(self.project, load_manifest(self.project))
        learned = [piece for piece in pieces if piece.learned]
        self.assertEqual(len(learned), 1)
        self.assertEqual(learned[0].tier, "gold")


class ManifestAndDateValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)

    def _write_manifest(self, updates: dict[str, object]) -> None:
        path = self.project / "manifest.json"
        data = json.loads(path.read_text(encoding="utf-8"))
        data.update(updates)
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    def test_non_object_manifest_fails_as_project_error(self) -> None:
        for payload in ("[]", '"x"', "123", "true"):
            with self.subTest(payload=payload):
                (self.project / "manifest.json").write_text(payload + "\n", encoding="utf-8")
                with self.assertRaises(ProjectError) as caught:
                    find_project_with_source(self.project)
                self.assertNotIsInstance(caught.exception.__cause__, AttributeError)
                self.assertIn("Not a ghostless project", str(caught.exception))

    def test_non_object_manifest_is_ignored_while_walking(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp)
            (bad / "manifest.json").write_text("[]\n", encoding="utf-8")
            nested = bad / "voice"
            nested.mkdir()
            (nested / "manifest.json").write_text("123\n", encoding="utf-8")
            home = bad / "home"
            home.mkdir()
            with contextlib.chdir(bad), patch.dict(os.environ, {"GHOSTLESS_HOME": str(home)}):
                os.environ.pop("GHOSTLESS_PROJECT", None)
                with self.assertRaisesRegex(ProjectError, "No ghostless project"):
                    find_project_with_source()

    def test_manifest_rejects_boolean_weights(self) -> None:
        for tier in ("gold", "silver", "bronze", "edited"):
            for bad_bool in (True, False):
                self._write_manifest({"weights": {tier: bad_bool}})
                with self.assertRaisesRegex(ProjectError, f"weights.{tier} must be a number"):
                    load_manifest(self.project)

    def test_manifest_rejects_non_finite_weights(self) -> None:
        for bad in ("nan", "inf", "-inf"):
            self._write_manifest({"weights": {"gold": bad}})
            with self.assertRaisesRegex(ProjectError, "weights.gold must be a number"):
                load_manifest(self.project)

    def test_manifest_rejects_boolean_and_fractional_fail_under(self) -> None:
        for bad in (True, False, 70.5, "70.5"):
            self._write_manifest({"failUnder": bad})
            with self.assertRaisesRegex(ProjectError, "failUnder must be an integer"):
                load_manifest(self.project)

    def test_validate_date_rejects_nonexistent_calendar_dates(self) -> None:
        for bad in ("2026-02-30", "2026-99-99", "2025-02-29", "2026-00-01", "2026-2-3", "01/02/2026"):
            with self.assertRaisesRegex(ProjectError, "valid YYYY-MM-DD date"):
                validate_date(bad, label="Test date")
        self.assertEqual(validate_date("2024-02-29", label="Test date"), "2024-02-29")

    def test_new_session_id_and_learn_edit_reject_invalid_calendar_dates(self) -> None:
        before = self.base / "before.md"
        after = self.base / "after.md"
        before.write_text("Original draft.\n", encoding="utf-8")
        after.write_text("Edited final.\n", encoding="utf-8")

        for bad in ("2026-02-30", "2026-99-99", "2025-02-29"):
            with self.assertRaisesRegex(ProjectError, "valid YYYY-MM-DD date"):
                new_session_id(self.project, date=bad)
            with self.assertRaisesRegex(ProjectError, "valid YYYY-MM-DD date"):
                learn_edit(
                    self.project,
                    before=before,
                    after=after,
                    section="notes",
                    origin="ai-first",
                    date=bad,
                )

        self.assertEqual(new_session_id(self.project, date="2024-02-29"), "2024-02-29")


class PathContainmentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.project = init_project(self.base / "voice", git=False)
        self.outside = self.base / "secret.md"
        self.outside.write_text("Outside secret prose.\n", encoding="utf-8")

    def test_require_contained_path_allows_interior_and_rejects_escapes(self) -> None:
        inside = self.project / "corpus" / "gold" / "desk.md"
        self.assertEqual(
            require_contained_path(self.project, inside, label="piece"),
            inside.resolve(),
        )
        for bad in (
            self.project / ".." / "secret.md",
            Path("/etc/passwd"),
        ):
            with self.assertRaisesRegex(ProjectError, "escapes"):
                require_contained_path(self.project, bad, label="piece")

        link = self.project / "corpus" / "gold" / "escape-link.md"
        link.symlink_to(self.outside)
        with self.assertRaisesRegex(ProjectError, "escapes"):
            require_contained_path(self.project, link, label="piece")

    def test_manifest_piece_path_traversal_is_rejected(self) -> None:
        manifest_path = self.project / "manifest.json"
        for bad_rel in ("../secret.md", str(self.outside)):
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["pieces"] = [{"path": bad_rel, "tier": "gold"}]
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(ProjectError, "escapes"):
                discover(self.project, load_manifest(self.project))

    def test_manifest_piece_dot_relative_normalizes_without_duplication(self) -> None:
        desk = self.project / "corpus" / "gold" / "desk.md"
        desk.write_text("Short plain prose.\n", encoding="utf-8")
        manifest_path = self.project / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["pieces"] = [{"path": "./corpus/gold/desk.md", "section": "notes"}]
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        pieces = discover(self.project, load_manifest(self.project))
        self.assertEqual(len(pieces), 1)
        self.assertEqual(pieces[0].path, "corpus/gold/desk.md")
        self.assertEqual(pieces[0].section, "notes")

    def _make_pair(
        self,
        slug: str = "2026-01-01-notes",
        *,
        before_file: object = "before.md",
        after_file: object = "after.md",
    ) -> Path:
        pair_dir = self.project / "gold" / slug
        pair_dir.mkdir(parents=True)
        (pair_dir / "before.md").write_text("Furthermore, we leverage alignment.\n", encoding="utf-8")
        (pair_dir / "after.md").write_text("I keep the note short.\n", encoding="utf-8")
        (pair_dir / "meta.json").write_text(
            json.dumps(
                {
                    "date": "2026-01-01",
                    "section": "notes",
                    "origin": "ai-first",
                    "kind": "prose",
                    "beforeFile": before_file,
                    "afterFile": after_file,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        return pair_dir

    def test_learned_pieces_and_refresh_edit_summary_reject_escaped_pair_files(self) -> None:
        self._make_pair("2026-01-01-after", after_file="../../secret.md")
        with self.assertRaisesRegex(ProjectError, "escapes"):
            discover(self.project, load_manifest(self.project))
        with self.assertRaisesRegex(ProjectError, "escapes"):
            refresh_edit_summary(self.project)

    def test_refresh_edit_summary_rejects_escaped_before_file(self) -> None:
        self._make_pair("2026-01-01-before", before_file=str(self.outside))
        with self.assertRaisesRegex(ProjectError, "escapes"):
            refresh_edit_summary(self.project)

    def test_learned_pieces_rejects_non_string_after_file(self) -> None:
        self._make_pair("2026-01-01-nonstr", after_file=123)
        with self.assertRaisesRegex(ProjectError, "afterFile must be a string"):
            discover(self.project, load_manifest(self.project))

    def test_promote_final_rejects_invalid_and_escaping_slugs(self) -> None:
        self._make_pair("2026-01-01-notes")
        for bad_slug in ("", ".", "..", "../escape", "a/b", "a\\b"):
            with self.assertRaisesRegex(ProjectError, "Invalid learned pair slug"):
                promote_final(self.project, bad_slug)

        escaped_dir = self.base / "outside-pair"
        escaped_dir.mkdir()
        (escaped_dir / "meta.json").write_text(
            json.dumps({"origin": "ai-first"}) + "\n", encoding="utf-8"
        )
        (self.project / "gold" / "link-pair").symlink_to(escaped_dir)
        with self.assertRaisesRegex(ProjectError, "escapes"):
            promote_final(self.project, "link-pair")

    def test_load_edit_pairs_skips_escaping_pairs_safely(self) -> None:
        self._make_pair("2026-01-01-valid")
        self._make_pair("2026-01-02-escape-before", before_file="../../secret.md")
        self._make_pair("2026-01-03-escape-after", after_file=str(self.outside))

        escaped_dir = self.base / "outside-pair-eval"
        escaped_dir.mkdir()
        (escaped_dir / "before.md").write_text("Before.\n", encoding="utf-8")
        (escaped_dir / "after.md").write_text("After.\n", encoding="utf-8")
        (escaped_dir / "meta.json").write_text(
            json.dumps({"origin": "ai-first"}) + "\n", encoding="utf-8"
        )
        (self.project / "gold" / "2026-01-04-link").symlink_to(escaped_dir)

        pairs, notes = load_edit_pairs(self.project)
        self.assertEqual([pair.slug for pair in pairs], ["2026-01-01-valid"])
        self.assertEqual(len(notes), 3)
        self.assertTrue(all("escapes pair directory" in note for note in notes))

    def test_evaluate_runs_when_before_file_escapes_and_is_skipped(self) -> None:
        (self.project / "corpus" / "gold" / "desk.md").write_text(
            "I keep the note short. I don't polish the first line.\n", encoding="utf-8"
        )
        (self.project / "corpus" / "gold" / "walk.md").write_text(
            "I take the long way home. So the walk clears my head.\n", encoding="utf-8"
        )
        self._make_pair("2026-01-01-valid")
        self._make_pair("2026-01-02-escape-before", before_file="../../secret.md")

        report = evaluate(self.project, holdout=1, seed=0)
        self.assertTrue(report["ok"])
        self.assertEqual(report["totalCount"], 1)
        self.assertTrue(
            any("2026-01-02-escape-before" in note and "escapes pair directory" in note for note in report["notes"])
        )


if __name__ == "__main__":
    unittest.main()
