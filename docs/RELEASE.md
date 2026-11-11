# Releasing ghostless

v0.1 is ready to publish and is not published. npm and PyPI both need the maintainer's credentials. Do not tag a release until you mean to ship it: a `v*` tag runs the npm publish workflow.

## npm (supported install)

The npm package includes a bash launcher under `bin/`, the voice logic under `lib/ghostless/`, and agent skills under `skills/`.

1. Bump `version` in `package.json`, `pyproject.toml`, and `ghostless.__version__` together. `bash tests/run.sh` fails when they disagree.
2. Add a `CHANGELOG.md` section `## vX.Y.Z — YYYY-MM-DD`.
3. Run `bash tests/run.sh`.
4. Tag and push when you want the workflow to run:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   [`.github/workflows/publish.yml`](../.github/workflows/publish.yml) checks that the tag matches `package.json`, reruns the tests, publishes `ghostless` with npm trusted publishing (OIDC, no `NPM_TOKEN`), and then opens the GitHub release from the changelog section.

Trusted publishing has to be turned on once in the npmjs.com UI: package `ghostless`, GitHub user `hirefrank`, repository `ghostless`, workflow `publish.yml`, environment blank. Until that entry exists the publish job fails and nothing is uploaded.

Manual fallback, from a real terminal:

```bash
npm pack --pack-destination /tmp/ghostless-pack
npm publish
```

The first publish of a new package cannot use a stage-only token.

## PyPI

`pyproject.toml` builds a wheel of the Python package (`pip install build && python -m build`). The console script runs the voice commands (`init`, `build`, `judge`, `learn`, `export`). Skill install, the daily updater, and the launcher are the npm package. There is no PyPI publish workflow on purpose, so a tag cannot upload a package that was never claimed on PyPI.

## What the release check looks at

Installed copies call `https://api.github.com/repos/hirefrank/ghostless/releases/latest` at most once a day. They compare `tag_name` to the local version. No corpus is sent. Until the first GitHub release exists, that request 404s and the CLI continues.
