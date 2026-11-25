# Repository Engineering Guardrails

## Determinism Hierarchy

Fix a bug at the highest level that can enforce it: the codebase, then static
analysis, then verification, then prose, then human review. A compiler check,
a lint, or a test replaces a sentence that only asks a reviewer to remember.
When a rule moves up, delete the prose that restated it. Leave prose for
judgment a compiler or a test cannot make.

## Map

- `lib/ghostless/` is the voice CLI, the schema, and the `ghostless bridge`
  worker. Project layout and the bridge protocol live in `SCHEMA.md`.
- `editor/` is the Rune desktop editor. Read `editor/AGENTS.md` before changing
  its UI, worker, state, or file lifecycle. Do not modify `editor/Cargo.lock`
  or the pinned `anomalyco/rune` revision in `editor/Cargo.toml` unless asked.
- `skills/*/SKILL.md` are the agent skills. `docs/RELEASE.md` is the release
  runbook. `CONTRIBUTING.md` is the cold-loaded human checklist.

## Packaging

The npm tarball is the `files` list in `package.json`: `bin/`, `lib/`,
`skills/`, `docs/`, `fixtures/`, and the listed root docs. `editor/` and
`CONTRIBUTING.md` are not in that list. The PyPI wheel, built from
`pyproject.toml`, ships the Python package and its console script. Skill
commands need the npm package or a checkout. `docs/EDITOR.md` builds the
editor from a checkout; it is not part of the npm package.

## Verification

```bash
./scripts/check
cargo fmt --manifest-path editor/Cargo.toml -- --check
cargo clippy --manifest-path editor/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path editor/Cargo.toml --locked
python3 tests/smoke_package.py
python3 tests/smoke_editor.py
```

Python subprocesses need `PYTHONUTF8=1`. Read and write UTF-8 text with
`encoding="utf-8"`. `tests/test_architecture.py` requires zero in-function
imports in `lib/ghostless/`, a clean module DAG, one shared helper for each
job, zero `# type: ignore` comments, and `atomic_write_text` /
`atomic_write_json`.

## Change checklists

New CLI command: add the subparser in `lib/ghostless/cli.py`, the README
command block, and any skill that should invoke it. `tests/test_docs_sync.py`
fails when a command is missing from that block.

New schema field: update the TypedDict in `lib/ghostless/schema.py`, the
reader and writer, and the matching section of `SCHEMA.md`.

New bridge method: add a `match` arm in `lib/ghostless/bridge.py`, the result
type in `lib/ghostless/schema.py`, and the method list in `SCHEMA.md`. Wire
the editor client only when the desktop UI calls it.

Version bump: change `package.json`, `pyproject.toml`,
`ghostless.__version__` in `lib/ghostless/__init__.py`, `editor/Cargo.toml`,
and `metadata.version` in `skills/*/SKILL.md` together. Follow
`docs/RELEASE.md` for the changelog and the tag.

## State invariants

`receipt.json` is the authority for a finished session. Finish validates the
baseline and refuses a session that already has a receipt before it mutates
the session.

A decision's precedence is `rejected`, then `retired`, then `confirmed`. A
rejected rule or check stays rejected when a later edit or rebuild would
otherwise confirm it.

One open editor tab owns a `(voice_project, session_id)` pair. Opening that
session again focuses the existing tab. One tab owns a writable `mirror_path`;
a linked file already open in another tab stays a session-local copy.

## Learned User Preferences

- When a pull request should close GitHub issues, attach those issues to the
  PR so merge closes them automatically.
