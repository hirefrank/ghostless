#!/usr/bin/env bash
# Run ghostless tests. Quiet unless a test fails.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PYTHONPATH="$ROOT/lib${PYTHONPATH:+:$PYTHONPATH}"
export GHOSTLESS_NO_UPDATE=1
python3 -m unittest discover -s "$ROOT/tests" -p 'test_*.py'
bash -n "$ROOT/bin/ghostless" "$ROOT/setup" "$ROOT/lib/agents.sh"
