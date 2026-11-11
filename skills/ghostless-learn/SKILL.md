---
name: ghostless-learn
description: Capture a finished piece so ghostless can learn the writer's edits. Use when the user says a piece is done, final, or ready, and both the earlier draft and their final exist. Run ghostless learn, then ghostless build. A writer-first final becomes gold; an AI-first final is edited until promoted.
license: MIT
metadata:
  version: "0.1.0"
---

# Capture the edit

When the user finishes a piece, store the earlier draft and their final. A writer-first final becomes gold; an AI-first final is measured as edited (paragraph length only, capped) until promoted. The diff feeds the voice guide either way. Everything stays on disk.

## You need both files

- `before` — the earlier draft. For an AI draft they edited, this is the AI draft (`--origin ai-first`, the default).
- `after` — the writer's final.
- If they wrote first and a model touched it before they finished, use `--origin writer-first`.
- Ask which section it belongs to when the project uses sections. Don't guess a section if they have several and didn't say.

If you only have the final, say so and stop. Do not invent a before file.

## Run

From the project (or pass `--project`):

```bash
ghostless learn --before before.md --after after.md --section notes
ghostless build
```

`learn` writes `gold/<date-slug>/` with `before.md`, `after.md`, and `meta.json`, and updates `edits-summary`. `build` folds the final into the guide at its tier (gold or edited).

Tell the user the slug and the main shifts (words, edit rate, moved text, sentence length, contractions). Do not paste their whole piece into a chat log they didn't ask for, and do not upload the gold files anywhere.
