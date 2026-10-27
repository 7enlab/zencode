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

## Re-plan 2026-10-27: align with Quy trinh MVP v3.0, Muc 18

Trigger: the team process rule "Quy trinh MVP v3.0, Muc 18" (effective
2026-10-25) overrides older specs (Quy trinh wins per Muc 12). The spec is
living: replaced parts of DESIGN.md are kept and marked "superseded by Muc 18",
not deleted. Still local only: no push, no PR, no merge, no new dependency,
only `tools/ztask/` changes.

Steps (in order, plan before code):

8. Re-plan (this section). Backup of the aba714b copies kept outside the repo.
9. DESIGN.md: 7-value state model QUEUED -> CLAIMED -> DOING -> BLOCKED ->
   REVIEW -> DONE -> DROPPED and new transition table; BLOCKED needs 4 fields
   (what is blocking, what was tried, who must unblock, effect on deadline);
   full task card (status, owner separate from executor, evidence,
   verification, blocked_by, lease_until, heartbeat, checkpoint, fencing token,
   idempotency key, permission level L0-L4); verified triple (verified_by +
   verification command + receipt, no self-verify, DONE needs triple + leader);
   per-task lease (12h default, pilot presets 30 min and 1 shift); dead-letter
   after 3 re-queues with recovery from checkpoint; reconciliation section
   (acceptance said "5-level state machine", Muc 18 says 7; chose Muc 18,
   flagged for ZC Leader).
10. ztask.py: implement the above; keep hash chain, TamperEvident,
    idempotency, WAL, VACUUM INTO, secret precheck, notifications, goal_ref.
11. test_ztask.py: update existing tests to the 7 states; add tests for the
    DROPPED path, BLOCKED 4-field rule, self-verify rejected, dead-letter after
    3, configurable lease.
12. README.md: new commands and states.
13. Run the suite twice on the box, check em dash count = 0 and
    `git diff --stat`; commit locally only if all green. Same step fails
    twice: stop and report.
