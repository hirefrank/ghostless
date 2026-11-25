# Editor Engineering Guardrails

## Single source of truth between rendering and scroll/action handlers
- Never duplicate eligibility, range, or target-selection predicates between
  `view`/`build` functions and scroll or action handlers.
- Extract one shared helper and drive both paths from it (e.g.
  `ui::preview::rendered_anchor_range` must govern both `preview-find-anchor`
  highlight emission in `build()` and target selection in
  `find_scroll_target()`, including filtering out hidden Markdown source such
  as HTML comments).
- Enforce source-byte vs rendered-surface parity for every span, find, jump,
  highlight, or revert feature across YAML frontmatter (`\r\n` and `\n`), hidden
  Markdown (HTML comments, code fences, link URLs), and multi-byte UTF-8.

## Unified state and focus-return contracts across views and overlays
- Views over the same document (`Write`, `Preview`, `Compare`) must share the
  same saved navigation/find anchor so cross-view switches preserve position.
- Sibling search and modal overlays (Find bar, Quick Switcher, Folder Search,
  dialogs) must route every dismissal path (`Escape`, close button, outside
  click) through one shared helper restoring focus to the active mounted
  document pane (`prose-input`, `working-body`, `compare-body`, or
  `preview-body`).
- Store per-document caches (`decoration_cache`, `cached_judge`) on
  `DocumentBuffer`, tagged with their validity key
  (`(voice_project, judge_epoch, section)` / `(generation, theme_key)`), and
  expose them through one validated accessor (`active_judge_report()`). Do not
  mirror them into separate `Editor` fields or `tab_key`-only maps that
  lifecycle transitions can forget to invalidate.

## Single file-protection and path-containment policy
- Drive every mutability and containment decision solely from
  `workspace_tree::is_protected_voice_path` / `is_protected_voice_relative` and
  `fs_io::is_contained_in`—never duplicate file/folder protection rules per UI
  menu or command.
- Enforce those checks at **both** UI/command entry and the `worker.rs` I/O
  boundary, including files opened outside the scanned sidebar tree (Quick
  Switcher, session links, CLI `--draft`, or symlinks).

## Mode and editability guards in state, not only UI
- Enforce view-mode and editability invariants inside `state.rs` action methods
  rather than relying only on disabled/hidden UI buttons.
- Test buttons, `on_key` shortcuts, and command dispatch across `(Write |
  Preview | Compare) × (editable | read-only | locked | voice-guide.md)` so
  shortcuts cannot mutate protected/non-editable buffers or strand overlays.

## Background worker isolation, cancellation, and generation-gated replies
- Keep critical writes (`WorkerRequest` saves and `Finish`) isolated from
  cancellable read, analysis, or watch lanes (`SearchWorker`, `FolderSearch`,
  NDJSON bridge analysis/`judge`, file-watcher scans).
- Stamp every async request with `(doc_id, generation/request_id)` and drop
  stale replies unconditionally.
- Test in-flight races explicitly: edit-during-save, superseded search/analysis
  queries, and close/discard while recovery or save I/O is pending.

## Fail-safe dirty-buffer and mirror lifecycle transitions
- Every transition (`close tab`, `switch workspace`, `rename/delete`, window
  close, quit/`Drop`) must preserve dirty buffers on write failure, surface the
  error visibly in `ui/status_bar.rs`, and keep retry enabled until a write
  succeeds.
- Linked session mirroring must fail safe to the local session copy whenever
  workspace containment or baseline SHA-256 hashes are missing or divergent,
  and must never silently mutate standalone `--draft` files.

## End-to-end `rune::Runtime` interaction tests
- When changing cross-view navigation, scroll/highlight alignment, mode
  shortcuts, or overlay focus, test the full interaction sequence (`edit ->
  switch view -> open/dismiss`) in `rune::Runtime` rather than testing handlers
  only in isolation—covering frontmatter, hidden Markdown, and multi-byte UTF-8.
- Assert `rt.elements()` directly: absence of suppressed or orphaned element
  keys, viewport containment of the chosen scroll target, and restored focus on
  the mounted document surface.
