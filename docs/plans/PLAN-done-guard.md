# PLAN - done-guard (roadmap v0.7.0 #9)

## Task
Implement the Definition-of-done guard: done checklist, `/done`, scope guard, YAGNI check.

## Acceptance checklist
- [x] Done checklist: tests + clippy + acceptance criteria (todos) - `/done` runs all three
- [x] `/done` command (REPL + headless `-p`, exit 1 on failure for CI)
- [x] `/scope` command: show/set/clear; seeded from first prompt; post-turn warnings; turn-guard nudges
- [x] YAGNI: flags new pub items with zero callers (same-file uses excluded from flags)
- [x] Tests: 4 glob + 6 guard + 14 doneguard unit tests; E2E `/done` on empty dir and on the repo
- [x] Docs: README "Definition of done" section + SPEC.md C5.1 builtin list
- [x] Full `cargo test --workspace`: 30 suites pass; `cargo clippy --workspace --all-targets -- -D warnings`: clean
- [x] Fixtures regenerated (additive slash_commands only)

## Files
- crates/zen-core/src/lib.rs - glob_match
- crates/zen-agent/src/guard.rs - TurnGuard scope nudges
- crates/zen-agent/src/agent.rs, subagent.rs - AgentConfig.done_scope
- crates/zen-agent/src/finish.rs - check-before-finish mentions /done
- crates/zen-cli/src/doneguard.rs (new) - project detection, checks, scope, yagni, report
- crates/zen-cli/src/{lib,commands,repl,agent_run,system_prompt}.rs - wiring
- docs/fixtures/stream-json/*.ndjson - regenerated
- README.md, SPEC.md - docs

## Notes
- No new dependencies (hand-rolled glob). /data filled up mid-task; freed space by removing target/ from completed sibling worktrees only.
- YAGNI false-positive fix: same-file callers count (declaration line excluded).
