# ghostless

ghostless is a local CLI and a set of agent skills that measure how you write, then judge AI drafts against those numbers. Every edit you keep becomes part of the measure.

Your corpus, transcripts, and edits stay on disk. `init` gives the voice directory its own local Git history, and you can attach a private remote when you choose. ghostless never adds a remote or pushes. The only network call is an optional once-a-day check for a new release, and that call sends no writing.

Inspired by Nat Eliason's article [How to Build an AI Writing System That Actually Sounds Like You](https://x.com/nateliason/status/2115803497773904292).

It works with any harness that loads [Agent Skills](https://agentskills.io): Claude Code, Cursor, Codex, OpenCode, Pi, Gemini CLI, Hermes, Muse, and others.

## Install

Python 3.11+ is required. The npm package includes a bash launcher, Python code under `lib/`, and skills symlinked into detected agent harnesses.

```bash
npm i -g ghostless@latest
ghostless install-skills
```

`install-skills` creates `~/.ghostless/`, puts `ghostless` on `PATH` (`~/.local/bin` when that directory is on `PATH`), and symlinks the three skills into every detected harness.

Zero-install:

```bash
npx --yes ghostless@latest install-skills
```

```bash
pnpm dlx ghostless@latest install-skills
```

From a checkout of this repo:

```bash
npm i -g .
ghostless install-skills
```

The npm package is the unscoped name `ghostless`. The GitHub repository stays [hirefrank/ghostless](https://github.com/hirefrank/ghostless). v0.1 is not published yet. Publishing needs the maintainer's credentials. The workflows and `pyproject.toml` are in place; see [docs/RELEASE.md](docs/RELEASE.md).

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

The voice directory is a voice-data repository. `corpus/`, `transcripts/`, `gold/`, `sessions/`, and `holdouts/` are meant to be committed; generated guides, stats, drafts, and raw media are ignored, so a rebuild leaves `git status` clean. `init` runs `git init` and adds no remote.

```bash
git status
git add -- manifest.json .gitignore corpus transcripts gold sessions holdouts
git commit -m "Start voice-data history"
```

To back it up or move it to another machine, attach a private remote yourself:

```bash
git remote add origin <private-repo-url>
git push -u origin HEAD
```

Pushing sends your writing to that host. ghostless never creates a remote or pushes.

Upgrading a v0.1 voice directory? Run `ghostless migrate --project voice`. It replaces the old ignore block, adds the new directories, and stages nothing.

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

An AI-first final lands in the `edited` tier, not gold. Promote it once you have made it yours:

```bash
ghostless learn --before before.md --after after.md --section notes --promote
```

## Commands

```bash
ghostless init [dir] [--no-git] | ghostless init --here
ghostless migrate [--project DIR] [--no-git]
ghostless build [--project DIR] [--json]
ghostless judge draft.md [--section NAME] [--project DIR] [--json]
ghostless learn --before a.md --after b.md [--section NAME] [--origin ai-first|writer-first] [--model NAME] [--tool NAME] [--promote] [--date YYYY-MM-DD]
ghostless promote SLUG [--demote] [--project DIR]
ghostless eval [--holdout N] [--seed N] [--project DIR] [--json]
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

`judge` exits 1 when its mechanical score is under `failUnder` (default 70). Banned tells, tidy frames, and unresolved placeholders are flagged for review. The score does not check factual fidelity or establish that the draft sounds like you. `setup` is an alias of `install-skills`.

`judge` also reports how much corpus it is comparing against: pieces, measured words, written prose words, and sentences. When the sample is small (under 1,500 words or fewer than three pieces) it marks the score low-confidence instead of showing a bare number. The guide has the same sample-size block, so a thin corpus is visible before you trust a score.

`ghostless eval` checks that the score means something. It holds out part of the corpus, builds targets from the rest, judges the held-out pieces, and reports whether the score tracks your edits. With learned pairs, it compares each held-out pair's score with that pair's edit rate (edited token positions divided by aligned positions, the same measure `learn` stores): higher scores should come with lighter edits. With no learned pairs it holds out gold prose pieces instead and reports only whether the targets generalize.

```bash
ghostless eval [--holdout N] [--seed N] [--project DIR] [--json]
```

`eval` exits 1 when there is not enough corpus to hold anything out. The split is seeded (`--seed`, default 0), so a rerun is reproducible.

## The tier model

Weights live in `manifest.json`. Defaults:

| Tier | Weight | What belongs there |
| --- | --- | --- |
| gold | 3 | Your unaided drafts, unaided dictations, and `learn` finals you wrote first or promoted. |
| silver | 1 | Recent writing, and transcribed speech unless you mark it gold. |
| bronze | 0.5 | Older writing. |
| edited | 0.5 | AI-first finals you revised but have not promoted. |

`goldVoiceMinimum` defaults to 10. Once that many gold written pieces exist, bronze still counts toward paragraph length and drops out of voice: sentence length, contractions, I, openers, em dashes, questions, signature phrases, and the ban list. Gold transcripts do not count toward that threshold, because spoken lines set no written voice. Neither do AI-first finals, which are `edited` until you promote them. Both numbers are editable.

An `edited` final is mostly the model's prose, so it never sets sentence length, contractions, I, openers, em dashes, questions, signature phrases, or the ban list. It shapes paragraph length only, and the whole edited pool is capped at 25% of the weight of your own prose. What *you* changed is kept as edit evidence: the diff, the words you cut and added, and the before → after shift show up in the guide's "what your edits do" section either way. See [Provenance and promotion](#provenance-and-promotion).

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

Measurement reads prose, not formatting. Before counting, it strips YAML frontmatter, fenced and inline code, link and image syntax (link text stays; URLs and image alt text do not), bare URLs, HTML tags and comments, heading lines, list and blockquote markers, horizontal rules, and table rules. A prose-only file measures exactly as it did before; a link URL or a code sample can no longer move a target.

If you tag pieces with `section:` (or a subfolder), `build` writes per-section targets. `judge --section` uses them. The ban list stays global: a tell you use in any counted prose is not banned.

The catalog is a fixed list of distinctive model tics (`delve`, `tapestry`, `furthermore`, `it's worth noting`, …). If you use a tell, it is reported as in use and left off the ban list. Absence is weak evidence in a small corpus: below 1,500 written words, the guide marks the list provisional. Review a hit in context before changing a draft.

`judge` also flags tidy structure that targets do not fully catch: "it's not X, it's Y" frames, single-word triplets (`clear, concise, and compelling`), and em dashes above the target.

## How a draft should behave

The guide tells the model to use the measured targets, review the ban list, and leave `[WRITER: your take]` where an opinion, quote, statistic, or anecdote belongs. The drafting skill runs `judge` afterward. The editing skill tightens your draft and shows a short change list. The capture skill runs `learn` when you say the piece is finished.

`learn` stores each distinct before/after pair once under `gold/<date>-<section>/` with `meta.json` (`date`, `section`, `origin`, `provenance`). `origin` is `ai-first` when the before file is an AI draft (the default) or `writer-first` when you drafted first. The diff records tokens cut and added, the sentence-length shift, the contraction shift, and words you remove or add more than once.

Where the final is measured depends on who wrote the first draft and whether you confirm it:

- `writer-first` is gold: the sentences started as yours.
- `ai-first` is `edited`: the final is still mostly the model's prose. It never sets voice targets or the ban list, and its paragraph weight is capped so a pile of lightly edited drafts cannot outvote your corpus.
- `ghostless promote <slug>` lifts one AI-first final into gold after you have rewritten it in your own words. It prints the edit rate, warns when the final is still mostly model prose, and shows the gold count and bronze threshold before and after.

The pair itself is kept either way. Promotion changes provenance, not evidence: the diff and vocabulary still feed the guide's "what your edits do" section.

## Provenance and promotion

Provenance is recorded separately from the measured statistics, so a number never implies a claim about authorship.

```bash
ghostless learn --before draft.md --after final.md --section notes --model claude-sonnet-4-5 --tool cursor
ghostless promote 2026-01-02-notes
ghostless promote 2026-01-02-notes --demote
```

`meta.json` stores `origin` (`ai-first` or `writer-first`), `confirmation` (`none`, `origin`, or `promoted`), and the optional `model` and `tool` you name. AI-first finals are `edited` until promoted, however many you learn. `build` warns when AI-first finals are present so they never inflate gold counts silently.

Pairs learned by v0.1 need no rewrite. `build` reads them with a default: `writer-first` stays gold, `ai-first` becomes `edited`, and a pair with no recorded origin is treated as edited with a warning. Promote the ones you stand behind.

## How this differs

The targets are counts from your files.

Three pieces are new relative to a static "write like me" prompt:

1. **Tiered weighting.** The manifest decides whose sentences move the targets. Gold outweighs silver, silver outweighs bronze, and bronze stops voting on voice once you have enough gold. AI-first finals sit in their own capped `edited` tier until you promote them.
2. **A spoken layer.** Transcribed speech informs rhythm and person, and it does not set format or the ban list.
3. **Edit learning.** The thing you did to an AI draft is stored and measured, and the writer's own finals join gold. The guide's "what your edits do" section is that history.

## Related work

- [Hiro-Inagawa/write-like-me](https://github.com/Hiro-Inagawa/write-like-me)
- [sullyo/write-like-me](https://github.com/sullyo/write-like-me)
- [conorbronsdon/write-like-me](https://github.com/conorbronsdon/write-like-me)

Those projects also aim at writing that sounds like a specific person. ghostless is the tiered, spoken-layer, edit-learning version of that idea, packaged as a CLI plus harness skills.

## Privacy

Storage is local by default. The voice directory, `~/.ghostless/` installer state, and generated guides live on this machine. `init` writes a `.gitignore` that treats `corpus/`, `transcripts/`, `gold/`, `sessions/`, and `holdouts/` as the voice-data repository and ignores generated guides, stats, drafts, temp files, and raw media. Nothing is staged, committed, or pushed for you, and a rebuild never dirties the working tree.

Remote storage is optional and explicit. A private host receives your writing only when you add a remote and push. `ghostless migrate` upgrades a v0.1 project by removing the old "do not commit" ignore block and writing the new rules; it stages and commits nothing, so you can review `git status` first. On a directory that was previously outside Git, prefer the path-scoped `git add -- manifest.json .gitignore corpus transcripts gold sessions holdouts` over `git add -A`.

`export` writes a paste file that, even without `--excerpts`, contains your measured targets, the absent-tell ban list, your signature phrases, per-section targets, and the words you cut or added in `learn` edits. `--excerpts` adds short gold samples. Treat the paste file as your writing: it is meant for a chat project you control, not for sharing.

The release check is a GET of the public GitHub releases endpoint, and it is the only request the CLI makes.

Turn the check off with any of:

- `GHOSTLESS_NO_UPDATE=1`
- `GHOSTLESS_UPDATE_CHECK=0`
- `{"updateCheck": false}` in `~/.ghostless/config.json`
- `"updateCheck": false` in the project manifest

On a newer release, the CLI prints an update notice. Run `ghostless update` to refresh the CLI and relink skills. A dirty git checkout is left unpulled.

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

Tests cover the stats, the tier and spoken rules, corpus cleanup, `judge` confidence, held-out evaluation, `learn`, export, and skill linking into a fake home. The sample corpus is invented. CI runs that suite on Python 3.11, 3.12, and 3.13.

Paths, the manifest, and the stats JSON are in [SCHEMA.md](SCHEMA.md).

## License

[MIT](LICENSE) © Frank Harris
