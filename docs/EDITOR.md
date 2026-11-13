# Local desktop editor

The editor in `editor/` is a native [Rune](https://github.com/anomalyco/rune) app. `editor/Cargo.toml` pins Rune to commit `fa07f485dd52d8f66db317460566ef4dc4784821`. The UI has a read-only original, an editable working copy, notes, and a split word-level diff. It uses the same session files and Finish command as the CLI (see [SCHEMA.md](../SCHEMA.md#edit-sessions)).

![Rune editor showing the sample draft's split word diff and notes](editor-preview.png)

Requirements for building from source: Rust 1.99+, Node 18+, Python 3.11+, and the native graphics libraries Rune requires for the host OS. A writer using a packaged editor should not need Rust. The editor calls the installed `ghostless` CLI; set `GHOSTLESS_CLI` to its executable path if it is not on `PATH`. From a checkout it uses `bin/ghostless` automatically.

The editor is not part of the npm package. `npm i -g ghostless` installs the CLI and the skills; the editor source exists only in the Git checkout ([#35](https://github.com/hirefrank/ghostless/issues/35)), so build it from a clone:

```bash
git clone https://github.com/hirefrank/ghostless ~/Projects/ghostless
```

You also need a voice project with a draft that has text in it. `start` rejects an empty file with `Draft is empty`, so the editor exits before it opens a window:

```bash
ghostless init /path/to/voice
ghostless build --project /path/to/voice
```

## Start from a draft

```bash
cargo run --release --manifest-path editor/Cargo.toml -- --project /path/to/voice --draft /path/to/draft.md
```

Build once with `cargo build --release --manifest-path editor/Cargo.toml` and run `editor/target/release/ghostless-editor` directly if you would rather not go through `cargo run`. Pass exactly one of `--draft` or `--session`.

`--project` must name an existing voice project. The editor does not yet use the CLI's project resolution (`--project`, `GHOSTLESS_PROJECT`, the current directory, `defaultProject`), so running inside the project or after `ghostless use` does not fill it in ([#33](https://github.com/hirefrank/ghostless/issues/33)).

This starts a new session (`ghostless start` under the hood).

## Resume a saved session

```bash
ghostless sessions --project /path/to/voice
cargo run --release --manifest-path editor/Cargo.toml -- --project /path/to/voice --session <id>
```

Edits and notes save after an 800 ms pause. Ctrl/Cmd+S saves immediately; Ctrl/Cmd+D switches between Edit and Diff. Finish writes an immutable final and receipt and adds the pair to gold once. The editor then becomes read-only. The original draft (`before.md`) is never overwritten. If the app closes before Finish, reopen the session by ID and continue from `after.md`.

All text stays on the local filesystem. The editor invokes only the local CLI, with release checks disabled. Its session metadata uses UTF-8 and relative filenames, so copying a project between macOS, Windows, and Linux preserves the session. The exact file format is in [SCHEMA.md](../SCHEMA.md).

The separate distribution work still needs signed or otherwise straightforward desktop installers and a clean-machine test of the combined CLI and editor. The [Rune editor workflow](../.github/workflows/editor.yml) already builds and uploads a per-OS `ghostless-editor` artifact on every push, but those binaries are not release assets and nothing installs them. Nothing open tracks the remaining work: [issue #13](https://github.com/hirefrank/ghostless/issues/13) closed with the cross-platform CLI, and [issue #9](https://github.com/hirefrank/ghostless/issues/9) closed with the editor itself. The packaging and first-run gaps are recorded in [#33](https://github.com/hirefrank/ghostless/issues/33) and [#35](https://github.com/hirefrank/ghostless/issues/35).
