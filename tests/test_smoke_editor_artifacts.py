"""The editor smoke keeps screenshots after its temp project is removed."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any, cast

ROOT = Path(__file__).resolve().parents[1]


def _keep_artifacts() -> Callable[[Path, str | None], None]:
    spec = importlib.util.spec_from_file_location("ghostless_smoke_editor", ROOT / "tests" / "smoke_editor.py")
    if spec is None or spec.loader is None:
        raise AssertionError("tests/smoke_editor.py is missing")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    loaded: Any = module
    return cast(Callable[[Path, str | None], None], loaded.keep_artifacts)


class ArtifactCopyTests(unittest.TestCase):
    def test_only_png_and_tree_json_are_copied(self) -> None:
        keep_artifacts = _keep_artifacts()
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            dest = Path(tmp) / "snaps"
            source.mkdir()
            (source / "finished.png").write_bytes(b"png")
            (source / "reopened-tree.json").write_text("{}\n", encoding="utf-8")
            (source / "notes.md").write_text("leave me\n", encoding="utf-8")
            keep_artifacts(source, str(dest))
            self.assertEqual(sorted(path.name for path in dest.iterdir()), ["finished.png", "reopened-tree.json"])
            self.assertEqual((dest / "finished.png").read_bytes(), b"png")
            keep_artifacts(source, None)
