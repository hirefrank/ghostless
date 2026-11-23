"""Shipped docs must not point at files the npm tarball does not contain."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

_CHECKOUT_PHRASES = (
    "not part of the npm package",
    "not in the tarball",
    "not included in the npm",
    "not in the npm package",
)


def _packaged_prefixes() -> set[str]:
    package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
    files = package["files"]
    if not isinstance(files, list):
        raise AssertionError("package.json files must be a list")
    return {str(item).rstrip("/") for item in files}


class PackagedDocsTests(unittest.TestCase):
    def test_editor_doc_opens_with_checkout_prerequisites(self) -> None:
        text = (ROOT / "docs" / "EDITOR.md").read_text(encoding="utf-8")
        head = text[:1500].lower()
        self.assertIn("not part of the npm package", head)
        self.assertTrue("checkout" in head or "clone" in head, head)
        self.assertIn("rust 1.99", head)
        self.assertIn("draft", head)
        self.assertIn("ghostless init", head)

    def test_docs_that_mention_editor_source_say_the_checkout_is_required(self) -> None:
        packaged = _packaged_prefixes()
        self.assertNotIn("editor", packaged)
        offenders: list[str] = []
        for doc in sorted((ROOT / "docs").glob("*.md")):
            text = doc.read_text(encoding="utf-8")
            if "editor/" not in text:
                continue
            lower = text.lower()
            names_boundary = any(phrase in lower for phrase in _CHECKOUT_PHRASES)
            names_checkout = "checkout" in lower or "clone" in lower
            if not (names_boundary and names_checkout):
                offenders.append(doc.name)
        self.assertEqual(
            offenders,
            [],
            "docs mention editor/ without saying it is outside the npm package and needs a checkout",
        )


if __name__ == "__main__":
    unittest.main()
