#!/usr/bin/env python3
"""Check that README commands, rendered guides, AGENTS snippet, and skills stay in sync with the CLI."""

from __future__ import annotations

import argparse
import contextlib
import io
import re
import sys
import unittest
from pathlib import Path
from typing import Any, cast
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SAMPLE = ROOT / "fixtures" / "sample-voice"

sys.path.insert(0, str(ROOT / "lib"))

from ghostless import cli  # noqa: E402
from ghostless.guide import render_guide, render_paste  # noqa: E402
from ghostless.project import discover, load_manifest  # noqa: E402
from ghostless.schema import CompiledStats  # noqa: E402
from ghostless.stats import DEFAULT_WEIGHTS, compile_stats  # noqa: E402


def _readme_commands_block(readme_text: str) -> str:
    match = re.search(r"^## Commands\n(.*?)(?=^## |\Z)", readme_text, re.MULTILINE | re.DOTALL)
    if match is None:
        raise AssertionError("README.md is missing the '## Commands' section.")
    code = re.search(r"```[^\n]*\n(.*?)```", match.group(1), re.DOTALL)
    if code is None:
        raise AssertionError("README.md '## Commands' section is missing a fenced code block.")
    return code.group(1)


def _cli_top_level_commands() -> set[str]:
    recorded: list[argparse._SubParsersAction[argparse.ArgumentParser]] = []
    orig_add_subparsers = argparse.ArgumentParser.add_subparsers

    def capture_subparsers(
        self: argparse.ArgumentParser, *args: Any, **kwargs: Any
    ) -> argparse._SubParsersAction[argparse.ArgumentParser]:
        action = orig_add_subparsers(self, *args, **kwargs)
        recorded.append(action)
        return action

    with (
        patch.object(argparse.ArgumentParser, "add_subparsers", capture_subparsers),
        contextlib.redirect_stdout(io.StringIO()),
    ):
        rc = cli.main([])
    if rc != 0:
        raise AssertionError(f"ghostless.cli.main([]) returned {rc}")
    if len(recorded) != 1:
        raise AssertionError(f"Expected 1 subparser group, got {len(recorded)}")
    argparse_cmds = set(recorded[0].choices.keys())
    return (argparse_cmds | set(cli._MAINTENANCE_COMMANDS)) - {"setup"}


class DocsSyncTests(unittest.TestCase):
    def test_readme_commands_block_covers_all_cli_commands(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        block = _readme_commands_block(readme)
        documented = set(re.findall(r"\bghostless\s+([a-z][a-z0-9-]*)", block))
        expected = _cli_top_level_commands()
        missing = sorted(expected - documented)
        self.assertEqual(missing, [], f"Commands missing from README.md ## Commands: {missing}")

        self.assertIn("ghostless checks [list|confirm|reject|add]", block)
        self.assertIn("ghostless use DIR", block)
        self.assertIn("ghostless uninstall", block)
        self.assertIn("ghostless eval [--holdout N | --cv]", block)

        commands_section = readme.split("## Commands\n", 1)[1].split("\n## ", 1)[0]
        self.assertIn("cut:", commands_section)
        self.assertIn("--section", commands_section)
        self.assertIn("--cv", commands_section)
        self.assertIn("holdouts/", commands_section)
        self.assertIn("### Checks from an edit", readme)

    def test_render_guide_targets_direction_and_edited_weight(self) -> None:
        manifest = load_manifest(SAMPLE)
        raw_weights = manifest["weights"]
        assert isinstance(raw_weights, dict)
        stats = compile_stats(
            discover(SAMPLE, manifest),
            weights={str(k): float(v) for k, v in raw_weights.items()},
            gold_voice_minimum=int(manifest["goldVoiceMinimum"]),
        )
        for text in (render_guide(stats), render_paste(stats, None)):
            self.assertIn("Use the targets below as diagnostics", text)
            self.assertNotIn("targets above", text)
            weights_section = text.split("## Weights\n", 1)[1].split("\n## ", 1)[0]
            self.assertIn("gold 3, silver 1, bronze 0.5, edited 0.5", weights_section)
            self.assertIn("paragraph length only, capped at 25%", weights_section)

        minimal = compile_stats([], weights=dict(DEFAULT_WEIGHTS), gold_voice_minimum=10)
        without_edited = dict(minimal)
        without_edited.pop("edited", None)
        minimal_weights = (
            render_guide(cast(CompiledStats, without_edited)).split("## Weights\n", 1)[1].split("\n## ", 1)[0]
        )
        self.assertIn("edited 0.5 (paragraph length only)", minimal_weights)

    def test_agents_snippet_and_skills_cover_sessions_rules_and_placeholders(self) -> None:
        snippet = (ROOT / "docs" / "AGENTS.md.snippet").read_text(encoding="utf-8")
        self.assertIn("ghostless start", snippet)
        self.assertIn("ghostless finish", snippet)
        self.assertIn("ghostless rules", snippet)
        self.assertIn("ghostless checks", snippet)
        self.assertIn("voice-guide.md", snippet)
        self.assertIn("[WRITER: your take]", snippet)
        self.assertIn("token", snippet)

        draft = (ROOT / "skills" / "ghostless-draft" / "SKILL.md").read_text(encoding="utf-8")
        self.assertIn("## Confirmed checks", draft)
        self.assertIn("## Style", draft)
        self.assertIn("## Audience", draft)
        self.assertIn("## Voice rules", draft)
        self.assertIn("cut:", draft)
        self.assertIn("hypotheses", draft.lower())
        self.assertIn("ghostless start", draft)
        self.assertIn("ghostless finish", draft)
        self.assertIn("voice guide", draft.lower())
        self.assertIn("[WRITER: ...]", draft)

        edit = (ROOT / "skills" / "ghostless-edit" / "SKILL.md").read_text(encoding="utf-8")
        self.assertIn("STYLE.md", edit)
        self.assertIn("AUDIENCE.md", edit)
        self.assertIn("## Voice rules", edit)
        self.assertIn("ghostless rules", edit)
        self.assertIn("ghostless start", edit)
        self.assertIn("ghostless finish", edit)
        self.assertIn("voice guide", edit.lower())
        self.assertIn("[WRITER: your take]", edit)

        learn = (ROOT / "skills" / "ghostless-learn" / "SKILL.md").read_text(encoding="utf-8")
        self.assertIn("## A judgment the counts cannot see", learn)
        self.assertIn("ghostless start", learn)
        self.assertIn("ghostless finish", learn)
        self.assertIn("ghostless rules", learn)
        self.assertIn("confirm", learn)
        self.assertIn("reject", learn)
        self.assertIn("retire", learn)

        all_skills = "\n".join([draft, edit, learn])
        self.assertIn("start", all_skills)
        self.assertIn("finish", all_skills)
        self.assertIn("rules", all_skills)
        self.assertIn("placeholder token", all_skills.lower())


if __name__ == "__main__":
    unittest.main()
