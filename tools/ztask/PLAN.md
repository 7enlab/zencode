# PLAN - Viec 10 (ztask) MVP-0

Owner: ZC Platform. Checkpoint: 2026-10-28. Acceptance: 2026-10-30.
Branch: dev/task-channel (local only, base 98779ad). No push, no PR, no merge.

Note: the repo `.gitignore` ignores `/PLAN.md` at the root, so this plan lives at
`tools/ztask/PLAN.md` to stay inside the allowed scope and be reviewable.

## Repo facts that shape the plan

- ZenCode is a Rust workspace (`crates/zen-*`, edition 2021 in Cargo.toml).
  Workspace deps have no SQLite crate and no `sha2` (sha2 0.10.9 is only a
  transitive entry in Cargo.lock).
- `tools/` holds standalone scripts: bash benches and a Python tool with a
  stdlib `unittest` suite (`tools/terminal-bench/tb_runner.py` +
  `test_tb_runner.py`). CI (`.github/workflows/ci.yml`) runs only cargo.
- Box toolchain: Python 3.13.5 with stdlib `sqlite3` (SQLite 3.46.1, so WAL and
  `VACUUM INTO` are available), cargo/rustc 1.99.0.

## Decision for MVP-0

- A Rust crate would need `rusqlite` (new dependency) plus a Cargo.toml
  workspace change outside `tools/ztask/`. Both are out of scope without a
  Leader decision.
- Python stdlib has `sqlite3` + `hashlib`, so MVP-0 can implement the real
  design (SQLite WAL + SHA-256 hash chain) with zero new dependencies, in the
  same style as `tools/terminal-bench/`.
- The Rust port (or a `zen task` subcommand) is an open decision for ZC Leader.

## Steps

1. Write `tools/ztask/PLAN.md` (this file) and `tools/ztask/DESIGN.md`.
2. Implement `tools/ztask/ztask.py` (stdlib only): store, hash chain, state
   machine, lease, idempotency, goal_ref, secret precheck, escalation,
   notification outbox + listeners, backup, rebuild, CLI.
3. Implement `tools/ztask/test_ztask.py` (stdlib `unittest`), including the 3
   acceptance tests: idempotent log, 12h lease expiry, TamperEvident verify.
4. Add `tools/ztask/README.md` with usage.
5. Run tests on the box; capture real output.
6. If tests pass and only `tools/ztask/` changed: commit locally as
   "ZC Platform <zc-platform@zencode.local>". Otherwise leave uncommitted.
7. Report: files, outline, test output, git status, diff --stat, commit hash,
   open decisions.

## Guardrails

- Touch only `tools/ztask/`. No dependency, no rename, no refactor elsewhere.
- Backup before overwrite or delete (nothing existing is overwritten here;
  `tools/ztask/` does not exist at base).
- Same step fails twice: stop, record, re-plan.
- DONE only with real test output + `git status` + `git diff --stat`.
- Pre-deploy gate: Z Ops 6-item verification checklist must pass before any
  deploy. MVP-0 is local only and is not a deploy.
