"""Shared atomic file writes, UTC timestamps, text previews, and digests."""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
from datetime import UTC, datetime
from pathlib import Path


def atomic_write_text(path: Path, text: str) -> None:
    """Write UTF-8 text atomically via a sibling temporary file and fsync."""
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output:
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def atomic_write_json(
    path: Path,
    value: object,
    *,
    sort_keys: bool = False,
    ensure_ascii: bool = False,
) -> None:
    """Serialize `value` as indented JSON with a trailing newline and write atomically."""
    atomic_write_text(
        path,
        json.dumps(value, indent=2, ensure_ascii=ensure_ascii, sort_keys=sort_keys) + "\n",
    )


def utc_now_iso() -> str:
    """Current UTC timestamp in ISO-8601 second precision (`...Z`)."""
    return datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def utc_today() -> str:
    """Current UTC calendar date in `YYYY-MM-DD` format."""
    return datetime.now(UTC).strftime("%Y-%m-%d")


def text_preview(text: str, limit: int = 80) -> str:
    """Collapse whitespace and truncate `text` to at most `limit` characters."""
    collapsed = " ".join(text.split())
    if len(collapsed) <= limit:
        return collapsed
    return collapsed[: limit - 1].rstrip() + "…"


def sha256_text(text: str) -> str:
    """Return the lowercase hex SHA-256 digest of a UTF-8 string."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()
