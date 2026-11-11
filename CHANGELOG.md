# Changelog

## v0.1.0 — 2026-11-11

- `init`, `build`, `judge`, `learn`, `export`, `install-skills`, and `update`.
- Weighted corpus: gold 3, silver 1, bronze 0.5, with bronze becoming format-only once gold reaches a configurable count.
- Spoken transcripts count only toward sentence length, contractions, first-person I, and And/But/So openers.
- `learn` stores a before/after pair as gold and records what the edit changed.
- Thin skills for drafting, editing, and capturing edits, installed into detected harnesses.
- Once-a-day release check that sends no writing. Off with `GHOSTLESS_NO_UPDATE=1` or `updateCheck: false`.
- npm package name is the unscoped `ghostless`. The GitHub repo stays `hirefrank/ghostless`.
