# Ghostless Writing Studio

The native Rune editor opens a writing folder as a workspace. It uses the local Ghostless CLI and the same [edit-session files](../SCHEMA.md#edit-sessions) as the terminal workflow. This editor is not part of the npm package. Build the editor from a checkout or download the binary for your OS from a successful [Rune editor workflow](../.github/workflows/editor.yml) run.

![Ghostless Writing Studio with the workspace sidebar, Markdown draft, and Voice Inspector](editor-preview.png)

## Launch the editor

Build with Rust 1.99+ and the native graphics libraries required by Rune. The CLI uses Node 18+ and Python 3.11+. The editor pins Rune to `fa07f485dd52d8f66db317460566ef4dc4784821`. It finds the checkout's `bin/ghostless` launcher on Unix or an installed CLI on `PATH`. Set `GHOSTLESS_CLI` to choose an executable explicitly.

```bash
cargo build --manifest-path editor/Cargo.toml --locked --release
editor/target/release/ghostless-editor
editor/target/release/ghostless-editor --folder /path/to/writing
```

Run `ghostless init /path/to/writing/voice` to create a voice project for a new writing workspace.

On first launch, choose **Open Folder** or **Open File** from Welcome. Later launches restore the last workspace when it still exists. A workspace can be any writing directory. The editor searches it and its ancestors for `manifest.json` or `voice/manifest.json` and attaches the first Ghostless project it finds. Use `--project /path/to/voice` to attach a specific project. A folder with no match opens in plain Markdown mode.

For a standalone session, start from a non-empty draft or reopen an existing session ID.

```bash
ghostless edit draft.md
ghostless sessions --open <id>
editor/target/release/ghostless-editor --project /path/to/voice --draft /path/to/draft.md
editor/target/release/ghostless-editor --project /path/to/voice --session <id>
```

For `--draft` and `--session`, project resolution checks `--project`, `GHOSTLESS_PROJECT`, the current directory, then the default set by `ghostless use`. `ghostless edit` links the draft when it starts the session. A standalone `--draft` edits the session copy and accepts `--link-draft` to opt into a workspace link. The editor stores workspace links, preferences, and recovery copies under `GHOSTLESS_HOME` or `~/.ghostless/`.

## Write and review

- **Sidebar** shows the folder tree, open edit sessions, and voice files. Use the quick switcher to open a file or session. Use workspace search for text across files.
- **Canvas** edits styled Markdown source in **Write**. **Preview** renders it. The **Reference Split** can hold another file or the session original beside the draft. **Compare** shows a session's changes in Split or Unified layout, with individual hunk reverts for editable sessions.
- **Voice Inspector** shows a live mechanical score, tells, checks, notes, references, and outline. **Voice Lens** underlines flagged passages in Write. The inspector and the placeholder shortcut can jump to the source. The reference files `STYLE.md` and `AUDIENCE.md` hold editorial guidance and reader context.

Workspace files save automatically after about 1.5 seconds. Session edits and notes save after about 800 ms. Switching tabs, losing window focus, or closing flushes pending automatic saves. Use Save Now to flush immediately. Turn off **Auto-save files** in View Settings for manual workspace saves. Each tab keeps its own undo history. Protected corpus, learned examples, immutable session metadata, and generated guides open read-only.

The editor watches the workspace and the attached voice project. External changes refresh the tree and voice data. A clean open file reloads and offers **Compare** in a notification. If you have unsaved edits, the canvas offers **Keep Mine**, **Reload from Disk**, or **Compare**. Keep Mine acknowledges the observed change so the next save can replace that copy. Reload from Disk takes the external version. Compare leaves both versions available for review.

## Track and finish an edit session

Open a workspace draft and choose **Track Session**, or use **Start Edit Session** in the Command Palette. Tracking freezes the original as `before.md`. The draft stays in its folder, and saves mirror its content to `sessions/<id>/after.md`. When the linked file has changed externally or moved out of reach, the editor can keep saving into the local session copy while you resolve the link.

**Finish** first saves pending work, then opens **Finish & Learn**. Review the edit summary, jump to any remaining `[WRITER: ...]` placeholders, edit session notes, and optionally enter a yes/no check that quotes a changed phrase. The guide rebuild is on by default. A writer-first final lands in gold. An AI-first final lands in the edited tier unless you select **Promote to gold**. Standalone sessions can use **Save Final Copy As…** to export the working text. If the CLI rejects the check, revise it in the modal and retry.

Finishing writes a receipt and archives the session as read-only. Use the Sessions sidebar or `ghostless sessions --open <id>` to resume an open session. The editor offers recovery from its local copy if a prior quit left changes unsaved.

## Review voice rules and checks

Open **Rules & Checks** from the Voice section in the sidebar. Its badge counts rule hypotheses and pending checks. Review supporting edits, choose a section or **All sections**, and confirm, reject, or retire items as applicable. Rebuild the voice guide after a decision to apply it. Decisions are kept in the project's tracked `rules.json` and `checks.json` files.

## Keyboard shortcuts

Use Cmd on macOS and Ctrl on Linux or Windows. The **Command Palette** also lists contextual actions, voice files, themes, fonts, and font size controls.

| Shortcut | Action |
| --- | --- |
| Cmd/Ctrl+O | Open Folder |
| Cmd/Ctrl+Shift+O | Open File |
| Cmd/Ctrl+N | New File |
| Cmd/Ctrl+S | Save Now |
| Cmd/Ctrl+W | Close the active tab |
| Cmd/Ctrl+Tab / Cmd/Ctrl+Shift+Tab | Next / previous tab |
| Cmd/Ctrl+P | Go to File or Session |
| Cmd/Ctrl+Shift+P | Command Palette |
| Cmd/Ctrl+Shift+B | Toggle sidebar |
| Cmd/Ctrl+J | Toggle Voice Inspector |
| Cmd/Ctrl+\ | Toggle Reference Split |
| Cmd/Ctrl+Shift+V | Toggle Markdown Preview |
| Cmd/Ctrl+D | Toggle Compare |
| Cmd/Ctrl+L | Toggle Voice Lens |
| F8 or Cmd/Ctrl+Shift+. | Jump to the next placeholder |
| Cmd/Ctrl+F | Find in the active document |
| Cmd/Ctrl+H | Find and replace |
| Cmd/Ctrl+Shift+F | Search in Workspace |
| F11 or Cmd/Ctrl+Shift+Enter | Toggle Focus Mode |
| Cmd/Ctrl+B / Cmd/Ctrl+I / Cmd/Ctrl+K | Bold / italic / insert link in Write |
| Cmd/Ctrl+= / Cmd/Ctrl+- / Cmd/Ctrl+0 | Increase / decrease / reset font size |

Text, settings, and edit evidence stay on the local filesystem. The editor runs the local `ghostless bridge` with release checks disabled. A copied project keeps its UTF-8 session files and their relative paths.
