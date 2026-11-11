"""Write one self-contained voice guide for hosts that cannot read local files."""

from __future__ import annotations

from pathlib import Path

from ghostless.guide import render_paste
from ghostless.project import discover, load_manifest
from ghostless.stats import Piece
from ghostless.text import words

EXCERPT_WORDS = 120
EXCERPT_LIMIT = 3


def gold_excerpts(root: Path, *, limit: int = EXCERPT_LIMIT, word_limit: int = EXCERPT_WORDS) -> list[dict[str, str]]:
    manifest = load_manifest(root)
    pieces = [piece for piece in discover(root, manifest) if _is_gold_prose(piece)]
    excerpts: list[dict[str, str]] = []
    for piece in pieces:
        snippet = _first_words(piece.text, word_limit)
        if not snippet:
            continue
        excerpts.append({"section": piece.section or "", "text": snippet})
        if len(excerpts) >= limit:
            break
    return excerpts


def write_export(
    root: Path,
    stats: dict[str, object],
    *,
    excerpts: bool,
    out: Path | None,
) -> Path:
    destination = out if out is not None else root / "voice-guide.paste.md"
    body = render_paste(stats, gold_excerpts(root) if excerpts else None)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(body, encoding="utf-8")
    return destination


def _is_gold_prose(piece: Piece) -> bool:
    return piece.tier == "gold" and piece.kind == "prose"


def _first_words(text: str, limit: int) -> str:
    tokens = words(text)
    if not tokens:
        return ""
    snippet = " ".join(tokens[:limit])
    if len(tokens) > limit:
        snippet += " …"
    return snippet
