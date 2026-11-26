"""The editor smoke keeps screenshots after its temp project is removed."""

from __future__ import annotations

import importlib.util
import struct
import sys
import tempfile
import unittest
import zlib
from collections.abc import Sequence
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parents[1]


def _load_smoke() -> ModuleType:
    spec = importlib.util.spec_from_file_location("ghostless_smoke_editor", ROOT / "tests" / "smoke_editor.py")
    if spec is None or spec.loader is None:
        raise AssertionError("tests/smoke_editor.py is missing")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ArtifactCopyTests(unittest.TestCase):
    def test_only_png_and_tree_json_are_copied(self) -> None:
        keep_artifacts = _load_smoke().keep_artifacts
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


def chunk(kind: bytes, body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))


def png(width: int, rows: Sequence[bytes]) -> bytes:
    header = chunk(b"IHDR", struct.pack(">IIBBBBB", width, len(rows), 8, 6, 0, 0, 0))
    compressed = chunk(b"IDAT", zlib.compress(b"".join(rows)))
    return b"\x89PNG\r\n\x1a\n" + header + compressed + chunk(b"IEND", b"")


class ScreenshotValidationTests(unittest.TestCase):
    def test_rejects_solid_filtered_and_fully_transparent_frames(self) -> None:
        validate_png = _load_smoke().validate_png
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.png"
            white = bytes([255] * 4)
            solid_white = b"\x01" + white + bytes(4)
            transparent = b"\x00" + bytes([255, 255, 255, 0, 0, 0, 0, 0])
            for row in (solid_white, transparent):
                path.write_bytes(png(2, [row, row]))
                with self.assertRaisesRegex(RuntimeError, "color variance"):
                    validate_png(path, (2, 2))

    def test_reads_filtered_pixels_and_checks_dimensions_and_integrity(self) -> None:
        validate_png = _load_smoke().validate_png
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "capture.png"
            first = b"\x00" + bytes([10, 20, 30, 255, 10, 20, 30, 255])
            second = b"\x02" + bytes([100, 0, 0, 0, 0, 0, 0, 0])
            data = png(2, [first, second])
            path.write_bytes(data)
            validate_png(path, (2, 2))
            with self.assertRaisesRegex(RuntimeError, "expected 4x4, got 2x2"):
                validate_png(path, (4, 4))
            path.write_bytes(data[:-1] + bytes([data[-1] ^ 1]))
            with self.assertRaisesRegex(RuntimeError, "bad checksum"):
                validate_png(path, (2, 2))
