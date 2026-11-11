# ghostless project schema

A voice project is a directory with `manifest.json` whose `"kind"` is `"ghostless"`. `ghostless init` creates one. Commands walk upward from the working directory until they find that file, or you pass `--project`.

## Layout

```text
manifest.json
corpus/gold/          weight 3, prose. Your own unaided drafts.
corpus/silver/        weight 1, prose. Recent writing.
corpus/bronze/        weight 0.5, prose. Older writing.
corpus/spoken/        spoken layer. Default tier silver.
transcripts/          same as corpus/spoken.
gold/<date-slug>/     a learn pair: before.md, after.md, meta.json
voice-guide.md        written by build
voice-stats.json      written by build
voice-guide.paste.md  written by export
edits-summary.md
edits-summary.json
```

`init` writes a `.gitignore` that ignores `corpus/`, `transcripts/`, `gold/`, and the generated guide files. Those are your writing.

Text files are `.md`, `.markdown`, and `.txt`. `README.md` is skipped. A subfolder of a tier becomes the section (`corpus/gold/notes/desk.md` → section `notes`) unless frontmatter sets `section`.

## manifest.json

```json
{
  "kind": "ghostless",
  "version": 1,
  "weights": { "gold": 3, "silver": 1, "bronze": 0.5 },
  "goldVoiceMinimum": 10,
  "updateCheck": true,
  "failUnder": 70,
  "pieces": []
}
```

| Field | Meaning |
| --- | --- |
| `weights` | Positive multipliers. Defaults above. |
| `goldVoiceMinimum` | Once this many gold written pieces exist, bronze is format-only. Spoken gold does not count. |
| `updateCheck` | `false` turns off the daily release check when you run from this project. |
| `failUnder` | `judge` exits 1 below this score, and when there is no usable voice data to compare against. Banned tells and structural tells lower the score and set `reviewRequired`, but do not fail on their own. |
| `pieces` | Optional explicit `{path, tier, section, kind}` rows. They override discovery. |

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

| Pool | Metrics | Spoken | Bronze after the gold threshold |
| --- | --- | --- | --- |
| rhythm | median sentence length, sentence-length mix, contractions per 1k, I per 1k, And/But/So openers | counts, at its tier weight | excluded |
| diction | em dashes per 1k, questions per 1k, signature phrases, ban list | excluded | excluded |
| format | median paragraph length | excluded | counts |

Sentence mix: short is 1–10 words, medium 11–20, long 21+. The median is the smallest value whose cumulative weight reaches half the weight in that pool.

Signature phrases are 2- and 3-word sequences that appear at least twice in the diction pool, not made of function words.

The absent-tell list is the catalog in `ghostless.tells.AI_TELLS` minus any tell that appears in the diction pool. A written sample under 1,500 words makes the list provisional; review each hit in context. Spoken lines never remove a tell from the ban list. Tells that do appear are listed as in use so they are not banned.

`build` writes weighted targets and the same pools at weight 1 (unweighted), plus a per-tier breakdown. The per-tier bronze numbers still include bronze voice, so you can see what the threshold dropped. `judge` uses the weighted targets. `--section` swaps in that section's weighted targets. The ban list stays global.

## learn meta.json

```json
{
  "date": "2026-01-02",
  "section": "notes",
  "origin": "ai-first",
  "kind": "prose",
  "beforeFile": "before.md",
  "afterFile": "after.md"
}
```

`origin` is `ai-first` (an AI draft the writer edited) or `writer-first` (the writer drafted first). Both finals are gold. `edits-summary.json` rolls up tokens removed and added, the sentence-length shift, the contraction shift, and words cut or added in more than one distinct edit pair. Identical before/after pairs cannot be learned twice.

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
