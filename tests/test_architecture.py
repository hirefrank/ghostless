"""Architectural invariants, clean DAG boundaries, and shared atomic I/O tests (#42)."""

from __future__ import annotations

import ast
import json
import sys
import tempfile
import unittest
from graphlib import TopologicalSorter
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "lib" / "ghostless"
sys.path.insert(0, str(ROOT / "lib"))

from ghostless.build import build_voice as build_mod_build_voice  # noqa: E402
from ghostless.cli import build_voice as cli_build_voice  # noqa: E402
from ghostless.io import atomic_write_json, atomic_write_text, text_preview, utc_now_iso  # noqa: E402
from ghostless.learn import find_existing_learn_pair, learn_edit  # noqa: E402
from ghostless.project import init_project  # noqa: E402


def _module_imports() -> dict[str, set[str]]:
    graph: dict[str, set[str]] = {}
    for path in sorted(PACKAGE.glob("*.py")):
        module_name = "__init__" if path.stem == "__init__" else path.stem
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        deps: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name == "ghostless":
                        deps.add("__init__")
                    elif alias.name.startswith("ghostless."):
                        deps.add(alias.name.removeprefix("ghostless.").split(".", 1)[0])
            elif isinstance(node, ast.ImportFrom) and node.module is not None:
                if node.module == "ghostless":
                    deps.add("__init__")
                elif node.module.startswith("ghostless."):
                    deps.add(node.module.removeprefix("ghostless.").split(".", 1)[0])
        deps.discard(module_name)
        graph[module_name] = deps
    return graph


class ArchitectureTests(unittest.TestCase):
    def test_zero_in_function_imports(self) -> None:
        in_function: list[str] = []
        for path in sorted(PACKAGE.glob("*.py")):
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    for child in ast.walk(node):
                        if isinstance(child, (ast.Import, ast.ImportFrom)):
                            in_function.append(f"{path.name}:{child.lineno} in {node.name}")
        self.assertEqual(in_function, [])

    def test_module_dependency_graph_is_a_clean_dag(self) -> None:
        graph = _module_imports()
        order = list(TopologicalSorter(graph).static_order())
        self.assertEqual(set(order), set(graph))
        self.assertEqual(graph["io"], set())
        self.assertEqual(graph["schema"], set())
        self.assertNotIn("judge", graph["project"])
        self.assertNotIn("judge", graph["guide"])
        self.assertNotIn("cli", graph["build"])
        self.assertNotIn("stats", graph["cli"])
        self.assertIs(cli_build_voice, build_mod_build_voice)

    def test_shared_helpers_are_defined_once_and_no_type_ignores(self) -> None:
        defs: dict[str, list[str]] = {}
        forbidden = {
            "_atomic_write",
            "_now",
            "_preview",
            "_existing_learn",
            "_block",
            "_metrics",
        }
        tracked = {
            "atomic_write_text",
            "atomic_write_json",
            "utc_now_iso",
            "text_preview",
            "find_existing_learn_pair",
        }
        ignores: list[str] = []
        for path in sorted(PACKAGE.glob("*.py")):
            source = path.read_text(encoding="utf-8")
            for lineno, line in enumerate(source.splitlines(), start=1):
                if "type: ignore" in line:
                    ignores.append(f"{path.name}:{lineno}")
            tree = ast.parse(source, filename=str(path))
            for node in tree.body:
                if isinstance(node, ast.FunctionDef) and (node.name in forbidden or node.name in tracked):
                    defs.setdefault(node.name, []).append(path.name)

        self.assertEqual(ignores, [])
        for name in forbidden:
            self.assertNotIn(name, defs)
        self.assertEqual(defs.get("atomic_write_text"), ["io.py"])
        self.assertEqual(defs.get("atomic_write_json"), ["io.py"])
        self.assertEqual(defs.get("utc_now_iso"), ["io.py"])
        self.assertEqual(defs.get("text_preview"), ["io.py"])
        self.assertEqual(defs.get("find_existing_learn_pair"), ["learn.py"])


class SharedIoTests(unittest.TestCase):
    def test_atomic_write_text_and_json_preserve_existing_on_replace_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            target = base / "nested" / "config.json"
            atomic_write_json(target, {"b": 1, "a": 2}, sort_keys=True)
            original = target.read_text(encoding="utf-8")
            self.assertEqual(json.loads(original), {"a": 2, "b": 1})
            self.assertTrue(original.endswith("\n"))

            with (
                patch("ghostless.io.os.replace", side_effect=OSError("disk full")),
                self.assertRaises(OSError),
            ):
                atomic_write_text(target, "corrupted\n")

            self.assertEqual(target.read_text(encoding="utf-8"), original)
            siblings = sorted(p.name for p in target.parent.iterdir())
            self.assertEqual(siblings, ["config.json"])

    def test_utc_now_iso_and_text_preview(self) -> None:
        stamp = utc_now_iso()
        self.assertRegex(stamp, r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
        self.assertEqual(text_preview("  one   two\nthree  ", limit=80), "one two three")
        self.assertEqual(text_preview("abcdefghi", limit=5), "abcd…")

    def test_find_existing_learn_pair_ignores_trailing_newlines(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            project = init_project(base / "voice", git=False)
            before = base / "before.md"
            after = base / "after.md"
            before.write_text("First line without newline", encoding="utf-8")
            after.write_text("Edited line without newline", encoding="utf-8")
            result = learn_edit(
                project,
                before=before,
                after=after,
                section="notes",
                origin="writer-first",
                date="2026-04-01",
            )
            matched = find_existing_learn_pair(
                project,
                "First line without newline",
                "Edited line without newline",
            )
            self.assertEqual(matched, project / "gold" / str(result["slug"]))
            self.assertIsNone(find_existing_learn_pair(project, "Different draft", "Edited line without newline"))


if __name__ == "__main__":
    unittest.main()
