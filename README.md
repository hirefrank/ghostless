# ghostless

**Measure how you write. Hold every AI draft to it.**

The targets are counts from your files, not a "write like me" prompt. ghostless is a local CLI and a set of agent skills that measure how you write, then judge AI drafts against those numbers. Every edit you keep becomes part of the measure — the more you use it, the less ghostly it gets.

Your corpus, transcripts, and edits stay on disk. `init` gives the voice directory its own local Git history, and you can attach a private remote when you choose. ghostless never adds a remote or pushes. The only network call is an optional once-a-day check for a new release, and that call sends no writing.

Inspired by Nat Eliason's article [How to Build an AI Writing System That Actually Sounds Like You](https://x.com/nateliason/status/2115803497773904292).

It works with any harness that loads [Agent Skills](https://agentskills.io): Claude Code, Cursor, Codex, OpenCode, Pi, Gemini CLI, Hermes, Muse, and others.

## Install

Node 18+ and Python 3.11+ are required for the npm install. The npm package uses a portable Node launcher for the Python voice engine. Native CLI commands run on macOS, Windows, and Linux without Bash.

```bash
npm i -g ghostless@latest
ghostless install-skills
```

The global npm install puts `ghostless` on `PATH`. `install-skills` creates `~/.ghostless/` and installs the three skills into detected harnesses. It links on macOS/Linux and copies on Windows, where creating symlinks can require extra privileges.

If `ghostless` is not found right after a global install, the npm prefix's `bin` directory is not on `PATH` in that shell — for example `~/.local/bin` when the prefix is `~/.local`. Add it with `export PATH="$HOME/.local/bin:$PATH"` (using your own bin directory), or run `ghostless doctor` once the binary is reachable: it prints the bin directory and the exact export line when it is missing.

One-off install of the skills:

```bash
npx --yes ghostless@latest install-skills
```

```bash
pnpm dlx ghostless@latest install-skills
```

One-off runs copy the skills so clearing the package-manager cache cannot break them. They do not install a persistent `ghostless` command; use the global install for that.

From a checkout of this repo:

```bash
npm i -g .
ghostless install-skills
```

A global install from a checkout is a link, not a copy. npm points `node_modules/ghostless` at the checkout, so `git switch`, a rebase, or an uncommitted edit changes the installed CLI, and `ghostless --version` reports the package version rather than the commit. For a frozen snapshot, pack the checkout and install the tarball it writes:

```bash
npm pack
npm i -g ghostless-0.1.0.tgz
```

npm also keeps global packages inside the active Node version. After switching Node versions with a version manager, reinstall ghostless, then run `ghostless install-skills --force` when it reports the existing skill paths as not installed by ghostless ([#34](https://github.com/hirefrank/ghostless/issues/34)).

The npm package is the unscoped name `ghostless`. The GitHub repository stays [hirefrank/ghostless](https://github.com/hirefrank/ghostless). v0.1 is not published yet. Publishing needs the maintainer's credentials. The workflows and `pyproject.toml` are in place; see [docs/RELEASE.md](docs/RELEASE.md).

The native desktop editor lives in [`editor/`](editor/) and is not part of the npm package; see [docs/EDITOR.md](docs/EDITOR.md) and [#35](https://github.com/hirefrank/ghostless/issues/35). The cross-platform CLI work is [#13](https://github.com/hirefrank/ghostless/issues/13). The `start`/`finish` session workflow from [#8](https://github.com/hirefrank/ghostless/issues/8) ships in this CLI.
See [desktop distribution](docs/CROSS_PLATFORM.md) for platform behavior, updates, and uninstall.

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
ghostless start draft.md [--section NAME] [--origin ai-first|writer-first] [--source REF] [--project DIR] [--json]
ghostless finish [final.md] [--session ID] [--note TEXT] [--project DIR] [--json]
ghostless sessions [--project DIR] [--json]
ghostless rules [list|confirm|reject|retire] [ID] [--section NAME] [--project DIR] [--json]
ghostless snapshot --session ID [--project DIR]
ghostless eval [--holdout N] [--seed N] [--project DIR] [--json]
ghostless export [--excerpts] [--out PATH] [--project DIR]
ghostless save --style TEXT | --audience TEXT [--project DIR]
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

## Editorial notes: STYLE.md and AUDIENCE.md

Voice is measured; substance is not. Two optional, tracked files let you state what the engine cannot measure, and `build` folds both into the guide (and the paste export) without measuring them.

| File | What it holds | Where it lands |
| --- | --- | --- |
| `STYLE.md` | What a piece must do, contain, and prove: argument, evidence, structure, publication readiness. | Folded under `## Style` |
| `AUDIENCE.md` | Who reads it: situation, prior knowledge, wants, interests, resistance. | Folded under `## Audience` |

The boundary is deliberate. Wording and rhythm are measured in the Targets table, and recurring cut/add words become voice rules under `ghostless rules` — neither belongs in `STYLE.md`. `STYLE.md` and `AUDIENCE.md` carry substance and reader context, which no number claims to check. Absence never blocks a build; the guide marks each missing file with a one-line pointer.

```bash
ghostless save --style "Every claim needs a source."
ghostless save --audience "Readers know the domain terms; skip definitions."
ghostless build
```

`save` appends a dated line to the right file; saving the same rule on the same day is a no-op. The files are tracked like the corpus, so edit them directly or route a confirmed rule through `ghostless save`.

## How a draft should behave

The guide tells the model to use the measured targets, review the ban list, and leave `[WRITER: your take]` where an opinion, quote, statistic, or anecdote belongs. The drafting skill runs `judge` afterward. The editing skill tightens your draft and shows a short change list. The capture skill runs `learn` when you say the piece is finished.

`learn` stores each distinct before/after pair once under `gold/<date>-<section>/` with `meta.json` (`date`, `section`, `origin`, `provenance`). `origin` is `ai-first` when the before file is an AI draft (the default) or `writer-first` when you drafted first. The diff records tokens cut and added, the sentence-length shift, the contraction shift, and words you remove or add more than once.

### Edit sessions

For a resumable, editor-agnostic workflow, capture an immutable baseline and finish it later from any Markdown editor:

```bash
ghostless start draft.md --project voice --source https://example.org/reference
# Edit the printed sessions/<id>/after.md in any Markdown editor.
ghostless finish --session <id> --project voice
ghostless build --project voice
```

`start` copies the draft to `before.md` (immutable) and `after.md` (the working copy). `finish` writes a final, learns the pair into `gold/` once, and records an edit receipt with provenance, content hashes, and the measured shift. Repeating `finish` with the same final returns the same receipt; changing it after finish needs a new session. `ghostless sessions` lists them. The native Rune desktop editor in [`editor/`](editor/) shows the original beside the working copy, a word-level diff, and notes over this same session format; see [docs/EDITOR.md](docs/EDITOR.md).

`learn --before --after` remains available for pairs captured outside a session.

Where the final is measured depends on who wrote the first draft and whether you confirm it:

- `writer-first` is gold: the sentences started as yours.
- `ai-first` is `edited`: the final is still mostly the model's prose. It never sets voice targets or the ban list, and its paragraph weight is capped so a pile of lightly edited drafts cannot outvote your corpus.
- `ghostless promote <slug>` lifts one AI-first final into gold after you have rewritten it in your own words. It prints the edit rate, warns when the final is still mostly model prose, and shows the gold count and bronze threshold before and after.

The pair itself is kept either way. Promotion changes provenance, not evidence: the diff and vocabulary still feed the guide's "what your edits do" section.

### Voice rules from edits

Edit history can suggest guidance, but one changed word is not a rule. `build` mines recurring cuts and additions (`cut:delve`, `add:honestly`) and treats them as hypotheses until you decide. A word becomes a candidate only after it appears in three distinct finished pieces (`"rules": {"minimumSupport": 3}` in `manifest.json` changes the threshold). Nothing becomes an active rule without your confirmation:

```bash
ghostless rules
ghostless rules confirm cut:delve --section notes
ghostless rules reject add:honestly
ghostless rules retire cut:very
ghostless build
```

`confirm` records the receipt references it rests on. `reject` is permanent: a rejected rule is not reintroduced by later rebuilds. Decisions live in tracked `rules.json`; rebuilds read it and never rewrite it. The guide's `## Voice rules` section lists confirmed rules separately from hypotheses, and measured targets stay under `## Targets`.

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
npm test
```

Tests cover the stats, the tier and spoken rules, corpus cleanup, `judge` confidence, held-out evaluation, edit sessions, `learn`, export, and skill installation into a fake home. The sample corpus is invented. CI runs the Python suite on 3.11, 3.12, and 3.13, plus an installed npm artifact smoke test on macOS, Windows, and Linux.

Paths, the manifest, and the stats JSON are in [SCHEMA.md](SCHEMA.md).

## License

[MIT](LICENSE) © Frank Harris
