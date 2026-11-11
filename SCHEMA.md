# ghostless project schema

A voice project is a directory with `manifest.json` whose `"kind"` is `"ghostless"`. `ghostless init` creates one. Commands walk upward from the working directory until they find that file, or you pass `--project`.

The voice directory is also a Git data repository. It is self-contained: no remote is required, and `build` works offline.

## Layout

```text
manifest.json           tracked    project settings
corpus/gold/            tracked    weight 3, prose. Your own unaided drafts.
corpus/silver/          tracked    weight 1, prose. Recent writing.
corpus/bronze/          tracked    weight 0.5, prose. Older writing.
corpus/spoken/          tracked    spoken layer. Default tier silver.
transcripts/            tracked    same as corpus/spoken.
gold/<date-slug>/       tracked    the edit library: before.md, after.md, meta.json
sessions/<session-id>/  tracked    an in-progress edit session (see below)
holdouts/               tracked    writing kept out of the build on purpose
voice-guide.md          ignored    written by build
voice-stats.json        ignored    written by build
voice-guide.paste.md    ignored    written by export
edits-summary.md        ignored
edits-summary.json      ignored
drafts/                 ignored    scratch AI drafts
stats/, exports/        ignored    suggested homes for extra output
raw/                    ignored    source recordings and video
```

| Path | Git | Read by build |
| --- | --- | --- |
| `manifest.json`, `corpus/`, `transcripts/` | tracked | yes |
| `gold/` (edit pairs) | tracked | yes, the `after` files, as gold or edited by provenance |
| `sessions/` (in-progress edits) | tracked | no |
| `holdouts/` | tracked | no |
| generated guides, stats, summaries, exports | ignored | `judge` and `export` read them when present |
| `drafts/`, `raw/`, temp files | ignored | no |

`init` writes the rules to `.gitignore` under `ghostless: voice-data repository`. They also ignore `*.paste.md`, `*.tmp`, `*.swp`, `*.swo`, `*~`, `*.bak`, `.DS_Store`, `Thumbs.db`, and raw media (`*.wav`, `*.mp3`, `*.m4a`, `*.aac`, `*.flac`, `*.ogg`, `*.opus`, `*.aiff`, `*.aif`, `*.caf`, `*.mp4`, `*.mov`, `*.mkv`, `*.webm`). A rebuild writes only ignored paths, so `git status` stays clean.

Tracked is not uploaded. A file is only sent somewhere when the writer adds a remote and pushes.

### Edit sessions

`sessions/<session-id>/` is a working edit record:

```text
sessions/<session-id>/
  session.json    id, date, section, origin, status, beforeFile, afterFile
  before.md       the AI draft or earlier version
  after.md        the writer's current final
```

```json
{
  "id": "2026-01-02-notes",
  "date": "2026-01-02",
  "section": "notes",
  "origin": "ai-first",
  "status": "open",
  "beforeFile": "before.md",
  "afterFile": "after.md"
}
```

`origin` is `ai-first` or `writer-first` (same values as `learn`), and it is only a claim about the first draft. It does not by itself decide whether the final is gold; see [Provenance](#provenance). `status` is `open` while the writer is still editing, `final` when they are done, and `promoted` once the pair has been copied into the `gold/` edit library.

`<session-id>` is `YYYY-MM-DD[-<section>][-N]`:

- the UTC date,
- the section, lowercased with non-alphanumeric runs collapsed to `-` and trimmed (omitted when there is no section),
- `-2`, `-3`, … only when that id already exists under `sessions/`.

`learn` uses the same slug for `gold/`. Ids are never reused; callers ask `ghostless.project.new_session_id(root, date=..., section=...)` for an unused one. An editor writes `session.json`; `build` ignores `sessions/` entirely. The session is not evidence until the writer finishes it, at which point `learn` copies the final into `gold/`. A session in `sessions/` is tracked, so committing it commits the AI draft too; leave scratch drafts in ignored `drafts/` instead.

`holdouts/` is tracked and never read by `build` or `judge`. Use it for writing you want to keep out of the measurement while testing whether the guide generalizes.

## Voice-data repository

`ghostless init` creates the directories, writes `.gitignore`, and runs `git init` in the voice directory. That is local history only: it does not add a remote, stage files, commit, or push. `init --no-git` skips the repository and still writes the rules. A directory that is already inside a larger Git repository gets the rules but no nested repository.

Migrate a v0.1 project with:

```bash
ghostless migrate --project ~/voice
```

v0.1 wrote a block that ignored `corpus/`, `transcripts/`, and `gold/`. `migrate`:

1. detects that exact block and removes it (and fails closed if the block was edited by hand),
2. appends the voice-data rules once, preserving rules the writer added,
3. creates missing directories and `.gitkeep` files,
4. creates local Git history unless `--no-git`,
5. stages and commits nothing.

It prints the path-scoped `git add` to run after reviewing `git status`. On a directory whose writing was kept out of Git before, prefer that explicit path list over `git add -A`.

`migrate` also reports how existing pairs land: writer-first pairs stay gold, and AI-first pairs are measured as edited until you promote them. No `meta.json` is rewritten; the default is applied on read. See [Provenance](#provenance).

A private remote is optional and always explicit:

```bash
cd ~/voice
git add -- manifest.json .gitignore corpus transcripts gold sessions holdouts
git commit -m "Start voice-data history"
git remote add origin <private-repo-url>
git push -u origin HEAD
```

Pushing sends the tracked writing and edit pairs to that host. ghostless never creates a remote, pushes, or fetches; `init`, `migrate`, `build`, `judge`, `learn`, and `export` are local. The only network call in the CLI is the optional release check, which sends no writing.

Text files are `.md`, `.markdown`, and `.txt`. `README.md` is skipped. A subfolder of a tier becomes the section (`corpus/gold/notes/desk.md` → section `notes`) unless frontmatter sets `section`.

## manifest.json

```json
{
  "kind": "ghostless",
  "version": 1,
  "weights": { "gold": 3, "silver": 1, "bronze": 0.5, "edited": 0.5 },
  "goldVoiceMinimum": 10,
  "updateCheck": true,
  "failUnder": 70,
  "pieces": []
}
```

| Field | Meaning |
| --- | --- |
| `weights` | Positive multipliers for `gold`, `silver`, `bronze`, and `edited`. Defaults above. |
| `goldVoiceMinimum` | Once this many gold written pieces exist, bronze is format-only. Spoken gold and edited AI-first finals do not count. |
| `updateCheck` | `false` turns off the daily release check when you run from this project. |
| `failUnder` | `judge` exits 1 below this score, and when there is no usable voice data to compare against. Banned tells and structural tells lower the score and set `reviewRequired`, but do not fail on their own. |
| `pieces` | Optional explicit `{path, tier, section, kind}` rows. They override discovery. |

`gold`, `silver`, and `bronze` hold writing the writer stands behind. `edited` holds AI-first finals: model prose the writer revised but has not promoted. An old manifest without an `edited` weight gets the default of 0.5.

Frontmatter on a source file is a simple `key: value` block:

```markdown
---
section: notes
tier: gold
kind: spoken
---
```

`kind: spoken` on a file in `corpus/gold` keeps gold's weight but only the spoken metrics. `tier: gold` on a file in `corpus/spoken` does the same. Use that for an unaided dictation you want at gold weight.

## Pools

| Pool | Metrics | Spoken | Bronze after the gold threshold | Edited AI-first finals |
| --- | --- | --- | --- | --- |
| rhythm | median sentence length, sentence-length mix, contractions per 1k, I per 1k, And/But/So openers | counts, at its tier weight | excluded | excluded |
| diction | em dashes per 1k, questions per 1k, signature phrases, ban list | excluded | excluded | excluded |
| format | median paragraph length | excluded | counts | counts, capped (see below) |

An `edited` piece is the `after` file of a learned AI-first pair that the writer has not promoted. Its words are mostly the model's, so it never sets rhythm, signature phrases, or the ban list. It informs paragraph length only, and the whole edited pool is capped at 25% of the weight of the writer-authored prose (`gold` + `silver` + `bronze`). One unevidenced AI draft cannot outvote the corpus, and neither can fifty. The writer's *changes* still count as edit evidence: the diff stays in `meta.json`, `edits-summary.json`, and the guide's "what your edits do" section regardless of tier. A section that holds only edited finals is not offered as a section target; `judge --section` on it fails the same way an unknown section does.

Sentence mix: short is 1–10 words, medium 11–20, long 21+. The median is the smallest value whose cumulative weight reaches half the weight in that pool.

Signature phrases are 2- and 3-word sequences that appear at least twice in the diction pool, not made of function words.

The absent-tell list is the catalog in `ghostless.tells.AI_TELLS` minus any tell that appears in the diction pool. A written sample under 1,500 words makes the list provisional; review each hit in context. Spoken lines never remove a tell from the ban list. Tells that do appear are listed as in use so they are not banned.

`build` writes weighted targets and the same pools at weight 1 (unweighted), plus a per-tier breakdown. The per-tier bronze numbers still include bronze voice, so you can see what the threshold dropped. `judge` uses the weighted targets. `--section` swaps in that section's weighted targets. The ban list stays global.

`voice-stats.json` also carries `goldPieces`, an `edited` block (pieces, configured weight, counted weight, cap share), a `provenance` count by origin, and per-file `origin`, `confirmation`, `model`, and `tool` in `files[]`.

## Provenance

Provenance is recorded, never measured. It answers two separate questions:

| Field | Values | Meaning |
| --- | --- | --- |
| `origin` | `unaided`, `writer-first`, `ai-first` | Where the first draft came from. |
| `confirmation` | `none`, `origin`, `promoted` | What the writer says about the final. |
| `model` | free text or `null` | The model that wrote the first draft, when known. |
| `tool` | free text or `null` | The harness or app that produced it, when known. |

Corpus files are `unaided` and confirmed by their origin, because they are the writer's own files. A learned pair carries whatever its `meta.json` says. Placement follows from provenance:

| Origin | Confirmation | Tier |
| --- | --- | --- |
| `unaided` | `origin` | `gold` (the tier comes from the folder) |
| `writer-first` | `origin` or `promoted` | `gold` |
| `ai-first` | `none` | `edited` |
| `ai-first` | `promoted` | `gold` |
| unknown (no `origin`) | `none` | `edited` |

Promotion is explicit:

```bash
ghostless learn --before a.md --after b.md --promote
ghostless promote 2026-01-02-notes
ghostless promote 2026-01-02-notes --demote
```

`promote` prints the edit rate, a warning when the final is still mostly model prose (below a 0.25 edit rate), the gold prose count before and after, and whether bronze just became format-only. Demoting returns the pair to `edited` and keeps the receipt. A `writer-first` pair is gold through its origin; demote only applies to AI-first finals.

### v0.1 pairs

v0.1 wrote `origin` and nothing else. There is no rewrite step: `build` applies this default on read.

| v0.1 `meta.json` | Placement |
| --- | --- |
| `origin: writer-first` | `gold` (confirmation is treated as `origin`) |
| `origin: ai-first` | `edited` (confirmation is treated as `none`) |
| no `origin` | `edited`, and `build` warns that the pair has no recorded origin |

The old pair still supplies edit evidence either way. Run `ghostless promote <slug>` to lift a specific AI-first final into gold; `ghostless learn --promote` does the same for a new pair.

## learn meta.json

```json
{
  "date": "2026-01-02",
  "section": "notes",
  "origin": "ai-first",
  "provenance": {
    "origin": "ai-first",
    "confirmation": "none",
    "model": "claude-sonnet-4-5",
    "tool": "cursor"
  },
  "kind": "prose",
  "beforeFile": "before.md",
  "afterFile": "after.md"
}
```

`provenance` is canonical. The top-level `origin` is a v0.1 mirror kept in sync for older readers. `origin` is `ai-first` (an AI draft the writer edited) or `writer-first` (the writer drafted first); `model` and `tool` are optional and there is no fixed vocabulary. An AI-first final is measured as `edited` unless its `provenance.confirmation` is `promoted`; a writer-first final is gold. Identical before/after pairs cannot be learned twice.

`meta.json` also carries an edit receipt:

| Field | Meaning |
| --- | --- |
| `deletions`, `insertions`, `replacements` | Sequence-aware token counts. A replacement counts once; moved tokens are taken out of deletions and insertions. |
| `movedTokens`, `movedSpans` | Tokens that appear on both the deleted and inserted side, grouped into contiguous before-side spans. A pure reorder is nonzero here and zero in deletions/insertions. |
| `tokenEdits` | `deletions + insertions + replacements + movedTokens`. |
| `unchangedTokens` | Tokens the alignment left in place. |
| `editRate`, `editRateDenominator` | `tokenEdits / (tokenEdits + unchangedTokens)`: the share of aligned token positions that were edited. `0` means the token sequence did not change; `1` means nothing survived in place. Reorders count because moved tokens are edits. The rate stays in `[0, 1]` and is comparable across drafts and sessions. Structural changes are reported separately because punctuation marks and word tokens are different units. |
| `structuralChanges` | Punctuation deltas by mark (plus punctuation reordering when the counts match but positions moved), paragraph-count changes, and moved paragraphs. This keeps a punctuation-only or paragraph-only edit visible even when every word is unchanged. |
| `paragraphsBefore`, `paragraphsAfter`, `sentencesBefore`, `sentencesAfter` | Structural counts for the same receipt. |

`removedWords` and `addedWords` stay bag-of-words vocabulary lists for the repeated cut/add rollups. `wordsRemoved` and `wordsAdded` are sequence-aware: each is deletions or insertions plus replacements. `edits-summary.json` rolls up those counts, the sentence-length shift, the contraction shift, the edit rate, moves, structure, and words cut or added in more than one distinct edit pair.

## Suite data

`~/.ghostless/` (or `GHOSTLESS_HOME`) holds the installer state: `suite-root`, `version`, `config.json`, `last-update-check`, `logs/`. It is not the corpus. `config.json` `{"updateCheck": false}` disables the release check on this machine. `GHOSTLESS_NO_UPDATE=1` and `GHOSTLESS_UPDATE_CHECK=0` do the same.

Skill links, from `lib/agents.sh`:

| Harness | Skills directory |
| --- | --- |
| claude-code | `~/.claude/skills` |
| cursor | `~/.cursor/skills` |
| codex | `~/.codex/skills` |
| opencode | `~/.config/opencode/skills` |
| pi | `~/.pi/agent/skills` |
| gemini-cli | `~/.gemini/skills` |
| hermes-agent | `~/.hermes/skills` |
| muse, agents | `~/.agents/skills` |

Detection is the harness home directory, plus a `muse` binary for Muse. `auto` falls back to Claude Code when nothing is detected.
