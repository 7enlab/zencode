## What and why
<!-- One or two sentences: the problem and the change. -->

## Testing
<!-- Commands run and their result, e.g. `cargo test --workspace` (N passed), `cargo clippy --workspace --all-targets -- -D warnings` (clean). Paste real output, not "tested". -->

## Risk
<!-- Low (docs, internal, behind a flag) / Medium (normal behaviour change) / High (tools, sessions, permissions, CI, release). What could break? -->

## How to revert
<!-- e.g. `git revert <sha>`; any data/session format migration to undo? -->

## Checklist
- [ ] Tests added or updated for the behaviour change
- [ ] Old session files still load (if session/eventlog touched)
- [ ] Works on Windows/macOS paths and shells (if tools/process touched)
- [ ] REVIEW.md findings fixed or closed with a reason
