# REVIEW.md - what matters when reviewing ZenCode

ZenCode is a Rust coding-agent CLI. A review exists to stop changes that break users, not to polish style.

## What failure looks like here
- **Tools:** a tool runs something the user did not ask for, loses output, hangs past its timeout, or leaves child processes behind.
- **Sessions:** a saved session no longer loads, resumes, forks or replays the same way (old session files must keep loading).
- **Agent loop:** the model gets a request that differs from what the log says it got, or a turn silently drops a tool result.
- **Permissions/safety:** anything that widens what runs without approval, leaks a secret into logs, or writes outside the workspace.
- **Cross-platform:** path, shell or process handling that only works on Linux (check Windows and macOS).

## How to report
- Start with a count: "3 findings (1 Important, 2 Nit)" or "No findings."
- Every finding gives file and line as evidence, not a guess from a name.
- Tag each one **Important**, **Nit**, or **Pre-existing** (bug this PR did not introduce).
- At most 3 Nits per review. Flag the class of bug once so every instance gets fixed; do not repeat a finding already posted.
- On a bug fix, check it fixes the root cause and has a test that failed before.

## Skip
- `target/`, generated files, lockfile churn, formatting-only changes (rustfmt decides).
- `PLAN*.md` and docs typos unless they mislead.

## Rounds
- At most 3 review/fix rounds. After that, step back and rethink the approach instead of another patch.
- After round 1, re-review only the new diff.

## Always needs a person (Zen)
- `.github/workflows/`, release/installer scripts, anything touching permissions, secrets, auto-update or network calls to new hosts.
