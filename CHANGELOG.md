# Changelog

## Unreleased

- `ghostless start`/`finish`/`sessions`/`snapshot` capture an immutable draft, a recoverable working copy, optional notes, and an edit receipt in `sessions/<id>/`. Finish learns the pair into `gold/` once and is idempotent; the id follows the `learn` slug (`YYYY-MM-DD[-section][-N]`).
- A native Rune desktop editor in `editor/` shows the original beside the working copy, a word-level diff, and notes over the same session format, and finishes through the CLI.
- The voice directory is a voice-data Git repository: `init` writes ignore rules that track selected writing, finished edit pairs, sessions, and holdouts while ignoring generated guides, stats, drafts, temp files, and raw media, then creates local history with no remote and no commit.
- `ghostless migrate` upgrades a v0.1 voice directory (old "do not commit" ignore block) and stages nothing, so writing that was previously outside Git is reviewed before it is committed.
- Local storage and an optional private remote are documented separately. ghostless never adds a remote or pushes.
- Recurring cuts and additions in edit receipts become rule candidates only after three distinct pieces, and stay hypotheses until `ghostless rules confirm` (or `reject`/`retire`). Decisions live in tracked `rules.json`, survive rebuilds, and are traced to their receipts; the guide separates confirmed rules from hypotheses and measured targets.

## v0.1.0 — 2026-11-11

- `init`, `build`, `judge`, `learn`, `export`, `install-skills`, and `update`.
- Weighted corpus: gold 3, silver 1, bronze 0.5, with bronze becoming format-only once gold reaches a configurable count.
- Spoken transcripts count only toward sentence length, contractions, first-person I, and And/But/So openers.
- `learn` stores a before/after pair as gold and records what the edit changed.
- Thin skills for drafting, editing, and capturing edits, installed into detected harnesses.
- Once-a-day release check that sends no writing. Off with `GHOSTLESS_NO_UPDATE=1` or `updateCheck: false`.
- npm package name is the unscoped `ghostless`. The GitHub repo stays `hirefrank/ghostless`.
