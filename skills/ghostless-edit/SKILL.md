---
name: ghostless-edit
description: Edit a user's own draft — tighten, cut, and push back — without sanding it into generic AI prose. Use when they ask to line-edit, cut, tighten, or review their writing. Show the cleaned draft and a short change list. Do not invent facts or opinions they did not write.
license: MIT
metadata:
  version: "0.1.0"
---

# Edit the user's draft

This is their draft. Make it sharper. Keep the voice they already have.

## Before editing

If a ghostless project is available (`ghostless locate`, then `voice-guide.md`), use it so cuts match their sentence length, contractions, and ban list. If there is no project, still edit, and say that you are editing without a voice guide.

## What to do

- Cut repetition, throat-clearing, and hedges.
- Prefer their words. Do not swap in catalog AI-tells (`delve`, `tapestry`, `it's not X, it's Y`, neat triplets, extra em dashes).
- Push back where a claim is vague or unsupported. Mark that in the change list. Do not invent the support.
- Do not add opinions, anecdotes, quotes, or statistics they did not write. If something is missing, leave `[WRITER: your take]` instead of filling it.

## What to show

1. The cleaned draft.
2. A short change list: what you cut or moved, and any pushback. A handful of lines, not a memo.

Then offer `ghostless judge` on the cleaned file when a voice guide exists. Do not run `ghostless learn` unless they say the piece is finished. That is the capture skill.
