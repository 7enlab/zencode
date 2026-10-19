# GitHub integration

ZenCode talks to GitHub through the [`gh` CLI](https://cli.github.com) — auth
belongs to `gh` (`gh auth login`), zen never sees tokens. Everything zen fetches
is read-only; applying fixes happens through normal agent turns, gated by the
permission mode.

## `/pr_comments [PR#]` (REPL)

Fetches PR review comments (inline code comments + conversation comments) and
displays them grouped by file with a severity estimate:

```
PR #1: docs: refresh README branding to ZenCode
https://github.com/atzenn/zencode/pull/1
3 comment(s):

## src/main.rs (2 comments)
  [CRITICAL] @reviewer:42
  > This is a security issue: unsanitized input reaches the shell.
  https://github.com/...
```

Severity is a keyword heuristic (`CRITICAL` for security/blocking language,
`HIGH` for bugs, `MEDIUM` for questions, `LOW` for suggestions, `NIT` for
style) — a triage aid, not a verdict.

After displaying, zen offers to address the comments with the agent:

| Permission mode | Behaviour |
|---|---|
| `plan` | Read-only: never offers (write tools are withheld anyway) |
| `default` | Asks `Address these comments with the agent? [y/N]` |
| `acceptEdits` / `bypassPermissions` / `dontAsk` | Dispatches the fix turn directly |

The fix turn receives every comment (file, line, author, severity) and is told
to fix the code, not to post replies on GitHub.

## `zen pr-comments [PR#] [--json]`

Headless, read-only version of `/pr_comments` for scripts and CI. With no PR
number it uses the PR for the current branch. `--json` prints the raw
structured data. Exit code is 0 on success.

## `/ci-fix` (REPL)

Fetches the failing GitHub Actions runs for the current branch, shows each with
a heuristic failure summary extracted from the failed log, then offers (same
permission gating as above) to dispatch an agent turn that diagnoses and fixes
the underlying issues. The agent is told not to touch workflow files unless the
failure is clearly in the workflow itself.

## `zen ci-fix [--json]`

Headless, read-only: prints failing checks with log excerpts and likely-cause
lines. Exit code is **1 when any check is failing** (0 when all pass), so it
works as a CI gate: `zen ci-fix` in a script fails the build while CI is red.

## Requirements

- `gh` CLI installed and authenticated (`gh auth login`)
- Current directory inside a GitHub checkout
- `gh` calls time out after 30 s and never hang the REPL
