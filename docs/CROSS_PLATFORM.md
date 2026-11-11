# Desktop distribution

The supported npm install uses `bin/ghostless.cjs`, a Node launcher that finds Python 3.11+ and runs `python -m ghostless` from the packaged `lib/` directory. It needs no Bash on macOS, Windows, or Linux. Set `GHOSTLESS_PYTHON` to select a specific interpreter. The Python wheel's console entry point remains available for the voice commands, but the npm package is the supported way to get the packaged skills.

`ghostless install-skills` links skills on macOS and Linux and copies them on Windows. A one-off npx/pnpm package also copies skills, because cache cleanup would break links into the temporary package. The installer leaves foreign skill paths alone unless `--force` is supplied. npm supplies the persistent launcher for a global install; a one-off package run does not create one.

`ghostless update` upgrades a global npm install and refreshes its skills. For a checkout, update Git and rerun `ghostless install-skills --force`. For a one-off install, rerun `npx --yes ghostless@latest install-skills --force`. `ghostless reset` removes owned skill links/copies; `ghostless uninstall` also removes `~/.ghostless/`. Remove the global npm package separately with `npm uninstall -g ghostless`. Voice projects are separate and are not removed.

The npm artifact is packed, installed in a temporary prefix, and exercised by `tests/smoke_package.py` on macOS, Windows, and Linux in `.github/workflows/package.yml`. The smoke checks the installed launcher, skill installation, project initialization, build, learn, and judge. It runs from outside the checkout.

The Rune editor in [issue #9](https://github.com/hirefrank/ghostless/issues/9) is a separate deliverable. Its package must consume the same project and edit-session format as the CLI, with UTF-8 text, project-relative paths serialized with `/`, and no machine-specific absolute paths in committed session metadata. The `start`/`finish` workflow from [issue #8](https://github.com/hirefrank/ghostless/issues/8) and the editor artifact are not present in this version; the desktop artifact smoke must grow to cover them when they land.
