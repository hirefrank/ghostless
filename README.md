# ghostless

ghostless is a local CLI and a set of agent skills that measure how you write, then judge AI drafts against those numbers. Every edit you keep becomes part of the measure.

Your corpus, transcripts, and edits stay on disk. The only network call is an optional once-a-day check for a new release, and that call sends no writing.

Inspired by Nat Eliason's article [How to Build an AI Writing System That Actually Sounds Like You](https://x.com/nateliason/status/2115803497773904292).

It works with any harness that loads [Agent Skills](https://agentskills.io): Claude Code, Cursor, Codex, OpenCode, Pi, Gemini CLI, Hermes, Muse, and others.

## Install

Python 3.11+ is required. The package layout follows [network-jobs](https://github.com/hirefrank/network-jobs): an npm launcher, Python under `lib/`, skills symlinked into each harness it finds.

```bash
npm i -g @hirefrank/ghostless@latest
ghostless install-skills
```

`install-skills` creates `~/.ghostless/`, puts `ghostless` on `PATH` (`~/.local/bin` when that directory is on `PATH`), and symlinks the three skills into every detected harness.

Zero-install:

```bash
npx --yes @hirefrank/ghostless@latest install-skills
```

From a checkout of this repo:

```bash
npm i -g .
ghostless install-skills
```

v0.1 is not on npm or PyPI yet. Publishing needs the maintainer's credentials. The workflows and `pyproject.toml` are in place; see [docs/RELEASE.md](docs/RELEASE.md).

```bash
ghostless install-skills --agent claude-code,cursor,codex
ghostless install-skills --agent all
ghostless install-skills --force
ghostless update
ghostless doctor
ghostless agents
```

## Quickstart

```bash
ghostless init
cd voice
```

Put your own writing in:

| Directory | What it is |
| --- | --- |
| `corpus/gold/` | Unaided drafts and dictations. Weight 3. |
| `corpus/silver/` | Recent writing. Weight 1. |
| `corpus/bronze/` | Older writing. Weight 0.5. |
| `corpus/spoken/` | Transcribed speech, your lines only. |

Then:

```bash
ghostless build
ghostless judge draft.md
```

`build` writes `voice-guide.md` (targets, ban list, phrases) and `voice-stats.json`. Ask your agent to draft from the guide, or paste an export into a chat project that cannot read files:

```bash
ghostless export
ghostless export --excerpts
```

Excerpts are off unless you pass `--excerpts`.

The repo ships an invented sample so you can see a judge report without using your own writing. It is labeled sample data in `fixtures/sample-voice/README.md`.

```bash
SUITE="$(ghostless which | sed -n 's/^suite=//p')"
ghostless build --project "$SUITE/fixtures/sample-voice"
ghostless judge "$SUITE/fixtures/sample-voice/drafts/ai-ish.md" --project "$SUITE/fixtures/sample-voice"
```

When you finish a piece, keep the earlier draft and your final:

```bash
ghostless learn --before before.md --after after.md --section notes
ghostless build
```

## Commands

```bash
ghostless init [dir] | ghostless init --here
ghostless build [--project DIR] [--json]
ghostless judge draft.md [--section NAME] [--project DIR] [--json]
ghostless learn --before a.md --after b.md [--section NAME] [--origin ai-first|writer-first] [--date YYYY-MM-DD]
ghostless export [--excerpts] [--out PATH] [--project DIR]
ghostless install-skills [--agent auto|all|name,...] [--force] [--copy] [--no-launcher] [-v]
ghostless update [--agent ...]
ghostless doctor
ghostless agents
ghostless which
ghostless locate
ghostless routing
ghostless reset [--yes] [--data]
ghostless --version
```

`judge` exits 1 when the score is under `failUnder` (default 70) or when the draft hits a banned tell or a structural tell. `setup` is an alias of `install-skills`.

## The tier model

Weights live in `manifest.json`. Defaults:

| Tier | Weight | What belongs there |
| --- | --- | --- |
| gold | 3 | Your unaided drafts, unaided dictations, and the finals you produce with `learn`. |
| silver | 1 | Recent writing, and transcribed speech unless you mark it gold. |
| bronze | 0.5 | Older writing. |

`goldVoiceMinimum` defaults to 10. Once that many gold pieces exist, bronze still counts toward paragraph length and drops out of voice: sentence length, contractions, I, openers, em dashes, questions, signature phrases, and the ban list. Both numbers are editable.

Spoken lines are their own layer. A file in `corpus/spoken/` or `transcripts/` counts only toward:

- median sentence length and the short/medium/long mix
- contractions per 1,000 words
- first-person `I` per 1,000 words
- sentences that open with And, But, or So

Spoken lines do not set paragraph length, em dashes, questions, signature phrases, or the ban list. Their default weight is silver. Put `tier: gold` in the frontmatter, or put the file in `corpus/gold/` with `kind: spoken`, when a dictation should weigh as gold and still stay on the spoken metrics.

`build` keeps the unweighted numbers beside the weighted ones, and a per-tier breakdown, so you can see what the weights changed.

### What gets measured

- Median sentence length, and the share of short (1–10 words), medium (11–20), and long (21+) sentences
- Contractions per 1,000 words
- First-person `I` (including I'm, I've, I'd, I'll) per 1,000 words
- Em dashes per 1,000 words
- Questions per 1,000 words
- Median paragraph length
- Share of sentences opening with And, But, or So
- Signature phrases that show up at least twice in the written voice pool
- A ban list made only of catalog AI-tell words and phrases that never appear in that written pool

If you tag pieces with `section:` (or a subfolder), `build` writes per-section targets. `judge --section` uses them. The ban list stays global: a tell you use in any counted prose is not banned.

The catalog is a fixed list of distinctive model tics (`delve`, `tapestry`, `furthermore`, `it's worth noting`, …). It is not applied blindly. If you use a tell, it is reported as in use and left off the ban list. A written sample under 200 words makes a strict list, and the guide says so.

`judge` also flags tidy structure that targets do not fully catch: "it's not X, it's Y" frames, single-word triplets (`clear, concise, and compelling`), and em dashes above the target.

## How a draft should behave

The guide tells the model to hit the numbers, skip the ban list, and leave `[WRITER: your take]` where an opinion, quote, statistic, or anecdote belongs. The drafting skill runs `judge` afterward. The editing skill tightens your draft and shows a short change list. The capture skill runs `learn` when you say the piece is finished.

`learn` copies the two files to `gold/<date>-<section>/` with `meta.json` (`date`, `section`, `origin`). `origin` is `ai-first` when the before file is an AI draft (the default) or `writer-first` when you drafted first. The diff records tokens cut and added, the sentence-length shift, the contraction shift, and words you remove or add more than once. The final is gold from then on.

## How this differs

The targets are counts from your files.

Three pieces are new relative to a static "write like me" prompt:

1. **Tiered weighting.** The manifest decides whose sentences move the targets. Gold outweighs silver, silver outweighs bronze, and bronze stops voting on voice once you have enough gold.
2. **A spoken layer.** Transcribed speech informs rhythm and person, and it does not set format or the ban list.
3. **Edit learning.** The thing you did to an AI draft is stored and measured, and the final joins gold. The guide's "what your edits do" section is that history.

## Related work

- [Hiro-Inagawa/write-like-me](https://github.com/Hiro-Inagawa/write-like-me)
- [sullyo/write-like-me](https://github.com/sullyo/write-like-me)
- [conorbronsdon/write-like-me](https://github.com/conorbronsdon/write-like-me)

Those projects also aim at writing that sounds like a specific person. ghostless is the tiered, spoken-layer, edit-learning version of that idea, packaged as a CLI plus harness skills.

## Privacy

`init` gitignores `corpus/`, `transcripts/`, `gold/`, and the generated guide files. Leave them uncommitted. `export` without `--excerpts` contains the targets and the ban list. The release check is a GET of the public GitHub releases endpoint, and it is the only request the CLI makes.

Turn the check off with any of:

- `GHOSTLESS_NO_UPDATE=1`
- `GHOSTLESS_UPDATE_CHECK=0`
- `{"updateCheck": false}` in `~/.ghostless/config.json`
- `"updateCheck": false` in the project manifest

On a newer release, `update` refreshes the CLI and relinks skills so the skills cannot drift. A dirty git checkout is left unpulled. `ghostless update` does the same thing when you ask.

## Skills

| Skill | When it runs |
| --- | --- |
| [ghostless-draft](skills/ghostless-draft/SKILL.md) | Draft in your voice, then `judge`. Placeholders instead of invented takes. |
| [ghostless-edit](skills/ghostless-edit/SKILL.md) | Tighten your draft. Show the result and a short change list. |
| [ghostless-learn](skills/ghostless-learn/SKILL.md) | When you finish: `learn`, then `build`. |

`ghostless routing` prints a short snippet you can pin in a project instruction file. The skills are also self-describing.

## Develop

```bash
bash tests/run.sh
```

Tests cover the stats, the tier and spoken rules, `judge`, `learn`, export, and skill linking into a fake home. The sample corpus is invented. CI runs that suite on Python 3.11, 3.12, and 3.13.

Paths, the manifest, and the stats JSON are in [SCHEMA.md](SCHEMA.md).

## License

[MIT](LICENSE) © Frank Harris
