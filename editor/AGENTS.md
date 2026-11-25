# Editor Engineering Guardrails

## Single source of truth between rendering and scroll/action handlers
- Never duplicate eligibility, range, or target-selection predicates between
  `view`/`build` functions and scroll or action handlers.
- Extract one shared helper and drive both paths from it (e.g.
  `ui::preview::rendered_anchor_range` must govern both `preview-find-anchor`
  highlight emission in `build()` and target selection in
  `find_scroll_target()`, including filtering out hidden Markdown source such
  as HTML comments).

## Unified state and focus-return contracts across views and overlays
- Views over the same document (`Write`, `Preview`, `Compare`) must share the
  same saved navigation/find anchor so cross-view switches preserve position.
- Sibling search and modal overlays (Find bar, Quick Switcher, Folder Search,
  dialogs) must route every dismissal path (`Escape`, close button, outside
  click) through one shared helper restoring focus to the active mounted
  document pane (`prose-input`, `working-body`, `compare-body`, or
  `preview-body`).

## End-to-end `rune::Runtime` interaction tests
- When changing cross-view navigation, scroll/highlight alignment, or overlay
  focus, test the full interaction sequence (`edit -> switch view ->
  open/dismiss`) in `rune::Runtime` rather than testing handlers only in
  isolation.
- Assert `rt.elements()` directly: absence of suppressed or orphaned element
  keys, viewport containment of the chosen scroll target, and restored focus on
  the mounted document surface.
