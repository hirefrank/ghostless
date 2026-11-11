"""AI-tell catalog and tidy-structure detectors.

The catalog is a generic list of words and phrases widely associated with
model prose. A tell becomes a ban only when the writer's own written sources
never use it. Spoken lines do not count toward that check.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

# High-precision tells. Everyday words a small corpus might simply omit
# (important, however, because) are not in this list.
AI_TELLS: tuple[str, ...] = (
    "a testament to",
    "additionally",
    "akin",
    "at the end of the day",
    "beacon",
    "bolster",
    "bustling",
    "cannot be overstated",
    "cutting-edge",
    "deep dive",
    "delve",
    "delved",
    "delves",
    "delving",
    "elevate",
    "embark",
    "ever-evolving",
    "facilitate",
    "foster",
    "furthermore",
    "game-changer",
    "groundbreaking",
    "harness",
    "holistic",
    "in conclusion",
    "in the realm of",
    "in today's world",
    "interplay",
    "intricate",
    "it's important to note",
    "it's worth noting",
    "leverage",
    "meticulous",
    "moreover",
    "multifaceted",
    "myriad",
    "nestled",
    "overarching",
    "paradigm",
    "paving the way",
    "pivotal",
    "plays a crucial role",
    "plethora",
    "rapidly evolving",
    "realm",
    "rich tapestry",
    "robust",
    "seamless",
    "shed light on",
    "showcase",
    "showcasing",
    "streamline",
    "synergy",
    "tapestry",
    "testament",
    "the landscape of",
    "transformative",
    "underscore",
    "unpack",
    "utilize",
    "vibrant",
    "when it comes to",
)

_NOT_X_ITS_Y = (
    re.compile(r"(?i)\bit(?:'s| is)\s+not\b[^.!?\n]{0,90}?\bit(?:'s| is)\b"),
    re.compile(r"(?i)\b(?:this|that|it)\s+isn't\b[^.!?\n]{0,90}?\bit(?:'s| is)\b"),
    re.compile(
        r"(?i)\bnot\s+(?:[A-Za-z]+(?:'[A-Za-z]+)?\s+){1,8}[A-Za-z]+(?:'[A-Za-z]+)?,\s*it(?:'s| is)\b"
    ),
)
_TRIPLET = re.compile(
    r"\b([A-Za-z][A-Za-z'-]{3,}),\s+([A-Za-z][A-Za-z'-]{3,}),\s+and\s+([A-Za-z][A-Za-z'-]{3,})\b"
)
_ORDINAL = re.compile(
    r"(?i)\bfirst(?:ly)?,\s+[^.!?\n]{0,80}?\bsecond(?:ly)?,\s+[^.!?\n]{0,80}?\bthird(?:ly)?\b"
)


@dataclass(frozen=True)
class StructureHit:
    kind: str
    excerpt: str


def tell_pattern(tell: str) -> re.Pattern[str]:
    parts = [re.escape(part) for part in tell.split()]
    body = r"\s+".join(parts)
    return re.compile(rf"(?i)(?<![A-Za-z0-9]){body}(?![A-Za-z0-9])")


def count_tell(text: str, tell: str) -> int:
    return len(tell_pattern(tell).findall(text))


def split_catalog(text: str) -> tuple[list[str], list[str]]:
    """Return (absent, present) catalog tells.

    Longer tells claim their span first, so "delve into" does not also
    count as the bare word when the phrase form is in the catalog.
    """
    if not text.strip():
        return [], []
    occupied = [False] * len(text)
    present: list[str] = []
    absent: list[str] = []
    ordered = sorted(AI_TELLS, key=lambda tell: (len(tell), tell), reverse=True)
    found: set[str] = set()
    for tell in ordered:
        count = 0
        for match in tell_pattern(tell).finditer(text):
            span = range(match.start(), match.end())
            if any(occupied[index] for index in span):
                continue
            for index in span:
                occupied[index] = True
            count += 1
        if count:
            found.add(tell)
            present.append(tell)
    for tell in AI_TELLS:
        if tell not in found:
            absent.append(tell)
    present.sort()
    absent.sort()
    return absent, present


def ban_hits(text: str, ban_list: list[str]) -> list[tuple[str, int]]:
    """Count banned tells in a draft. Longer matches consume their span."""
    if not text or not ban_list:
        return []
    occupied = [False] * len(text)
    hits: list[tuple[str, int]] = []
    ordered = sorted(set(ban_list), key=lambda tell: (len(tell), tell), reverse=True)
    for tell in ordered:
        count = 0
        for match in tell_pattern(tell).finditer(text):
            span = range(match.start(), match.end())
            if any(occupied[index] for index in span):
                continue
            for index in span:
                occupied[index] = True
            count += 1
        if count:
            hits.append((tell, count))
    hits.sort()
    return hits


def structural_tells(text: str) -> list[StructureHit]:
    """Flag tidy frames that show up constantly in model prose.

    Each sentence contributes at most one not-x-its-y hit (the first
    matching frame) and any triplet or ordinal list inside it.
    """
    hits: list[StructureHit] = []
    for sentence in re.split(r"(?<=[.!?])\s+|\n+", text):
        region = sentence.strip()
        if not region:
            continue
        for pattern in _NOT_X_ITS_Y:
            match = pattern.search(region)
            if match:
                hits.append(StructureHit("not-x-its-y", _excerpt(region, match.start(), match.end())))
                break
        triplet = _TRIPLET.search(region)
        if triplet:
            hits.append(StructureHit("triplet", triplet.group(0)))
        ordinal = _ORDINAL.search(region)
        if ordinal:
            hits.append(StructureHit("triplet", _excerpt(region, ordinal.start(), ordinal.end())))
    return hits


def _excerpt(text: str, start: int, end: int) -> str:
    left = max(0, start - 12)
    right = min(len(text), end + 12)
    snippet = text[left:right].replace("\n", " ").strip()
    if len(snippet) > 160:
        snippet = snippet[:157] + "..."
    return snippet
