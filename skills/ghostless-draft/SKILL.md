---
name: ghostless-draft
description: Draft in the user's own voice from a local ghostless voice guide. Use when they ask for a draft, post, essay, newsletter, email, memo, or a rewrite that should sound like them. After writing, run ghostless judge. Leave [WRITER: your take] placeholders instead of inventing opinions, quotes, statistics, or anecdotes.
license: MIT
metadata:
  version: "0.1.0"
---

# Draft in the user's voice

The voice lives in a local ghostless project. Read `voice-guide.md`. Do not upload the corpus, transcripts, or gold edits.

## Find the project

```bash
ghostless locate
```

If that fails, ask the user where the project is, or run `ghostless init` in the folder they choose. If `voice-guide.md` is missing, they need sources under `corpus/` and then `ghostless build`.

## Write

- Match the weighted targets in the guide (sentence length, contractions, I, openers, paragraphs, em dashes, questions).
- Reach a first-person target with observational and opinion statements ("I see this a lot", "I think", "I keep coming back to"). Never invent an anecdote to hit the number — no "I remember when..." stories the writer did not tell you. If the piece needs a story only the writer can tell, leave it as a placeholder for them.
- Do not use the ban list.
- Use a signature phrase only when it fits. Do not stuff them in.
- If the piece has a section the guide knows, keep that section's targets in mind.
- Spoken-layer notes in the guide are already baked into the targets. Do not treat a transcript as a format sample.
- Never invent a quote, a number, an anecdote, or the writer's opinion. Where the draft needs their take, insert a placeholder on its own line:

```text
[WRITER: your take — what you need from them, in a few words]
```

## Then judge

Write the draft to a file and run:

```bash
ghostless judge draft.md
ghostless judge draft.md --section notes
```

Review flagged words, tidy frames, and em dashes in context. The mechanical score does not check factual fidelity or prove the draft sounds like the writer. Do not "fix" a placeholder by guessing the opinion. Show the draft and the judge summary. One revision pass is enough unless the user asks for another.
