"""Once-a-day release check.

The request is a GET of the public GitHub releases endpoint. It sends no
corpus, draft, or transcript. A failed check never blocks the command.
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path

from ghostless.paths import ProjectError, data_home, find_project, version
from ghostless.project import load_manifest

RELEASE_URL = "https://api.github.com/repos/hirefrank/ghostless/releases/latest"
DEFAULT_INTERVAL_S = 86_400


def parse_version(value: str) -> tuple[int, ...]:
    text = value.strip()
    if text[:1] in {"v", "V"}:
        text = text[1:]
    text = text.split("-", 1)[0]
    parts: list[int] = []
    for part in text.split("."):
        if not part.isdigit():
            raise ValueError(f"not a version: {value}")
        parts.append(int(part))
    if not parts:
        raise ValueError(f"not a version: {value}")
    return tuple(parts)


def version_gt(remote: str, local: str) -> bool:
    left = parse_version(remote)
    right = parse_version(local)
    width = max(len(left), len(right))
    left = left + (0,) * (width - len(left))
    right = right + (0,) * (width - len(right))
    return left > right


def normalize_version(value: str) -> str:
    numbers = parse_version(value)
    return ".".join(str(number) for number in numbers)


def plan(
    *,
    disabled: bool,
    stamp_age_s: float | None,
    interval_s: float,
    remote: str | None,
    local: str,
    fetch_ok: bool,
) -> str:
    """Decide what a run should do. Pure: no clock and no network."""
    if disabled:
        return "disabled"
    if stamp_age_s is not None and stamp_age_s < interval_s:
        return "skip"
    if not fetch_ok or not remote:
        return "current"
    try:
        newer = version_gt(remote, local)
        remote_text = normalize_version(remote)
    except ValueError:
        return "current"
    if newer:
        return f"update {remote_text}"
    return "current"


def updates_disabled() -> bool:
    if os.environ.get("GHOSTLESS_NO_UPDATE") == "1":
        return True
    if os.environ.get("GHOSTLESS_UPDATE_CHECK", "1") == "0":
        return True
    if os.environ.get("GHOSTLESS_JUST_UPDATED") == "1":
        return True
    config = _read_json(data_home() / "config.json")
    if isinstance(config, dict) and config.get("updateCheck") is False:
        return True
    try:
        root = find_project()
    except ProjectError:
        return False
    try:
        manifest = load_manifest(root)
    except ProjectError:
        return False
    return manifest.get("updateCheck") is False


def run_once(*, fetcher: object = None) -> str:
    """Check at most once per interval. Returns a plan line and updates the stamp."""
    if updates_disabled():
        return "disabled"
    home = data_home()
    stamp_path = home / "last-update-check"
    interval = _interval()
    age = _stamp_age(stamp_path)
    if age is not None and age < interval:
        return "skip"
    fetch = fetcher if callable(fetcher) else fetch_latest_release
    fetch_ok, remote = fetch()
    decision = plan(
        disabled=False,
        stamp_age_s=None,
        interval_s=interval,
        remote=remote,
        local=version(),
        fetch_ok=fetch_ok,
    )
    _write_stamp(stamp_path)
    return decision


def fetch_latest_release(timeout: float = 3.0) -> tuple[bool, str | None]:
    request = urllib.request.Request(
        RELEASE_URL,
        headers={
            "Accept": "application/vnd.github+json",
            "User-Agent": "ghostless",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return True, None
        return False, None
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError):
        return False, None
    if not isinstance(payload, dict):
        return False, None
    tag = payload.get("tag_name")
    if not isinstance(tag, str) or not tag.strip():
        return True, None
    return True, tag.strip()


def _interval() -> float:
    raw = os.environ.get("GHOSTLESS_UPDATE_INTERVAL_SECONDS")
    if not raw:
        return float(DEFAULT_INTERVAL_S)
    try:
        value = float(raw)
    except ValueError:
        return float(DEFAULT_INTERVAL_S)
    if value < 0:
        return float(DEFAULT_INTERVAL_S)
    return value


def _stamp_age(path: Path) -> float | None:
    try:
        stored = float(path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None
    return max(0.0, time.time() - stored)


def _write_stamp(path: Path) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(str(int(time.time())), encoding="utf-8")
    except OSError:
        return


def _read_json(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def main() -> int:
    print(run_once())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
