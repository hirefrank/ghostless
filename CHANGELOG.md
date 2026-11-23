# Changelog

## Unreleased

## v0.1.0 — 2026-11-23

- The README leads with a checkout install while v0.1 is unpublished. `install-skills` writes a version-independent `ghostless` launcher that names a missing suite after a Node upgrade and will not replace a command it did not install.
- `ghostless edit DRAFT` starts a session and opens the desktop editor. `ghostless start` only captures the session. `ghostless sessions --open ID` reopens one.
- The desktop editor resolves the voice project the same way as the CLI (`--project`, `GHOSTLESS_PROJECT`, the current directory, `defaultProject`) and names a missing project or draft path.
- `ghostless doctor` reports the stable launcher, a version-scoped Node install, and whether the editor is built.
- `init`, `build`, `judge`, `learn`, `export`, `install-skills`, and `update`.
- Weighted corpus: gold 3, silver 1, bronze 0.5, with bronze becoming format-only once gold reaches a configurable count.
- Spoken transcripts count only toward sentence length, contractions, first-person I, and And/But/So openers.
- `learn` stores a before/after pair as gold or edited by provenance and records what the edit changed, with sequence-aware token alignment, structural changes, and cached diff summaries (`diffVersion` + SHA-256).
- Thin skills for drafting, editing, and capturing edits, installed into detected harnesses with ownership tracking and cross-platform Windows/POSIX support.
- Once-a-day release check that sends no writing. Off with `GHOSTLESS_NO_UPDATE=1` or `updateCheck: false`.
- npm package name is the unscoped `ghostless`. The GitHub repo stays `hirefrank/ghostless`.
- Tightened npm tarball and Python wheel packaging boundaries (`tests/` excluded from npm; wheel installs report status cleanly in `ghostless doctor`).
- A yes/no check saved from an edit, for a judgment the mechanical score cannot see. It stays inactive until `ghostless checks confirm`, and a rejected check is not brought back by a later edit. `ghostless eval` grades confirmed checks as passes over checks when the draft skill or the voice guide changes, in a fresh grade session, and leaves the mechanical score as it is.
- `ghostless eval` supports leave-one-out cross-validation (`--cv`, automatic for 3–30 pairs) and uncontaminated `holdouts/` scoring.
- Optional `STYLE.md` and `AUDIENCE.md` files state what a piece must prove and who reads it; `build` folds both into the guide (and the paste export) without measuring them.
- `ghostless save --style/--audience` appends a dated editorial rule to the right file; saving the same rule on the same day is a no-op.
- `ghostless start`/`finish`/`sessions`/`snapshot` capture an immutable draft, a recoverable working copy, optional notes, and an edit receipt in `sessions/<id>/`. Finish learns the pair into `gold/` once and is idempotent; the id follows the `learn` slug (`YYYY-MM-DD[-section][-N]`).
- A native Rune desktop editor in `editor/` shows the original beside the working copy, a word-level diff, and notes over the same session format, with non-blocking autosave/finish, dynamic working-pane sizing, and unsaved close flushing.
- The voice directory is a voice-data Git repository: `init` writes ignore rules that track selected writing, finished edit pairs, sessions, and holdouts while ignoring generated guides, stats, drafts, temp files, and raw media, then creates local history with no remote and no commit.
- `ghostless migrate` upgrades a v0.1 voice directory (old "do not commit" ignore block) and stages nothing, so writing that was previously outside Git is reviewed before it is committed.
- Local storage and an optional private remote are documented separately. ghostless never adds a remote or pushes.
- Strict manifest validation (weights, overrides, placeholder tokens) with default `[WRITER: ...]` case-insensitive detection and preserved measured targets alongside pinned overrides.
- Recurring cuts, additions, and structural patterns (`struct:` punctuation and paragraph changes) in edit receipts become rule candidates only after `rules.minimumSupport` distinct pieces (default 3), and stay hypotheses until `ghostless rules confirm` (or `reject`/`retire`). Decisions live in tracked `rules.json`, survive rebuilds, and are traced to their receipts; confirmed `cut:` rules are enforced by `ghostless judge` within their section scope.
- `ghostless judge` and `ghostless export` warn on `stderr` (and `judge --json` sets `"stale": true`) when `voice-stats.json` is older than corpus, note, rule, check, or manifest changes without rewriting files on read.
