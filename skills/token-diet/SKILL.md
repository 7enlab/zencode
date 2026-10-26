---
name: token-diet
description: Token-efficiency habits - keep every model call lean without losing correctness
allowed-tools: read
---

# Token Diet - Spend Tokens Like They Cost Money (They Do)

## Output discipline

- Answer in the fewest words that remain correct. No preamble, no summary of what you just did unless asked.
- Code over prose: show the diff, not a paragraph about the diff.
- Never re-read a file you already read in this session unless it changed. Quote from memory; if unsure, re-read only the section you need.

## Context discipline

- Prefer the repo map / symbol outline over full file reads for orientation.
- When a tool result is large, extract the 3 lines that matter and move on - do not paste the whole output back into your reasoning.
- Summarize, don't accumulate: if the conversation is long, compress settled facts into a short recap and drop the raw turns.

## Tool discipline

- Batch independent reads in one block instead of sequential calls.
- Use `grep`/`glob` to narrow before `read`. Never `read` a directory listing you could `glob`.
- Stop early: the first correct answer wins. Do not "double-check" by re-running the same command.

## What NOT to do

- Do not dump entire files into the context "just in case".
- Do not repeat the user's request back to them.
- Do not add "helpful" extras beyond the asked scope - scope creep burns tokens twice (yours now, theirs on review).
