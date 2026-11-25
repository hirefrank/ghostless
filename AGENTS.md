# Repository Engineering Guardrails

- **Editor (`editor/`)**: Read [editor/AGENTS.md](editor/AGENTS.md) before
  modifying Rust/Rune UI, worker, state, or file-lifecycle code.
- **Cross-platform Python & CI (Windows cp1252 trap)**: Always pass
  `encoding="utf-8"` when reading/writing UTF-8 text files and include
  `PYTHONUTF8=1` when spawning Python subprocesses in scripts and tests.
- **Python architecture invariants (`tests/test_architecture.py`)**: Keep zero
  in-function imports in `lib/ghostless/`, maintain a clean module DAG, define
  shared helpers once (never duplicate `_atomic_write`/`_now`/etc.), allow zero
  `# type: ignore` comments, and use `atomic_write_text` / `atomic_write_json`.
- **Pinned dependencies**: Do not modify `editor/Cargo.lock` or bump the pinned
  `anomalyco/rune` commit (`fa07f485dd52d8f66db317460566ef4dc4784821`) in
  `editor/Cargo.toml` unless explicitly requested.

## Learned User Preferences

- When a pull request should close GitHub issues, attach those issues to the PR so merge closes them automatically.
