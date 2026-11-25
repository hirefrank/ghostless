# Contributing

This file is for people. Agents load `AGENTS.md` and `editor/AGENTS.md`.
`CONTRIBUTING.md` is not listed in the `package.json` `files` array, so it is
not part of the npm package.

Before a pull request, run `./scripts/check`. Editor changes also run the Rust
format, Clippy, and test commands in `AGENTS.md`. Follow the checklists there
when a change adds a CLI command, a schema field, a bridge method, or a
version bump.

Do not modify `editor/Cargo.lock` or the pinned Rune revision unless the
change is explicitly about that dependency.
