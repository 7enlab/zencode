# ztask - DESIGN (Viec 10)

Status: draft for checkpoint 2026-10-28. Acceptance 2026-10-30.
Owner: ZC Platform. Reviewer: ZC Leader. Spec is a living document.

The original approved design doc lives in ZC Leader's workspace
(`zencode-task-channel-design-2026-10-21.md`) and was not available to copy.
This file restates the decisions already made and fills the gaps needed for
MVP-0. Anything marked OPEN is a decision for ZC Leader.

## 1. Goal

ztask is the team task channel: one service that owns task state, enforces the
state machine at the service layer (not by discipline), keeps a tamper-evident
history, and notifies the right people the moment something changes.

Acceptance (Zen, 2026-10-30):

1. Idempotent event log.
2. Task record with 4 elements + 5-state machine.
3. Instant notification.
4. 12h lease + hash chain + 3 tests.

## 2. Architecture

```
clients (agents, leaders, cron)
   |  authenticated local socket, or token over HTTP (never bare TCP)
   v
ztask service  (single writer process)
   |  secret precheck -> idempotency check -> sweep (leases, escalation)
   |  -> validate transition -> append event + update projection
   |     + enqueue notifications, all in ONE transaction
   v
SQLite (WAL) on box-local disk
   events      append-only, SHA-256 hash-chained   <- source of truth
   chain_head  length + last hash                  <- catches tail truncation
   tasks       projection, rebuildable from events
   outbox      notifications, at-least-once delivery
   meta        chain spec, leaders, config
```

### 2.1 Source of truth: append-only event log

- Every state change is an event. The `tasks` table is a projection and can be
  dropped and rebuilt by replaying `events` (`ztask rebuild`).
- `ztask verify` checks the chain AND that the projection equals a fresh replay.
- No UPDATE or DELETE ever runs against `events` in code.

### 2.2 Hash chain (tamper evidence)

One chain over the whole log, in append order (`seq` 1, 2, 3, ...).

```
preimage = CHAIN_SPEC || "\n" || prev_hash || "\n" || seq || "\n" || body
hash     = lowercase_hex(sha256(preimage))
CHAIN_SPEC = "ztask.chain.v1"   (recorded in meta)
prev_hash of seq 1 = 64 x "0"
```

- `body` is the exact stored bytes of the event (canonical JSON: sorted keys,
  compact separators, UTF-8). The hash covers stored bytes, not a
  re-serialization, so a rewrite that is still valid JSON is still detected.
- Separators are unambiguous: hex, decimal and compact JSON contain no raw
  newline.
- `chain_head` (row count + last hash) is updated in the same transaction as
  each append, so dropping rows from the end is detected.
- `verify` reports the first bad `seq` and the reason: hash mismatch,
  prev_hash link broken, seq gap (row removed or inserted), or head mismatch.
- Limit (same as salvor): the chain is unkeyed, so someone with write access
  to the file can rewrite everything from genesis and recompute. Closing that
  needs an external anchor: periodically export `chain_head` to a location the
  box cannot rewrite (OPEN: where, see section 10).

### 2.3 Task record: 4 elements

Each task carries four required elements (OPEN: confirm this is the intended
set of 4):

| # | Element | Field(s) | Rule |
|---|---------|----------|------|
| 1 | Goal | `title`, `goal`, optional `goal_ref` | goal_ref must point to an existing task |
| 2 | Owner | `owner` (accountable), separate from `assignee` (executor) | owner required at create |
| 3 | Acceptance | `acceptance` (how it will be verified) | required at create |
| 4 | Evidence | `evidence` | required to move to review |

Operational fields: `state`, `assignee`, `lease_until`, `lease_token`
(fencing), `heartbeat_at`, `blocked_by`, `deadline`, `priority`,
`created_at`, `updated_at`, `escalated_at`.

### 2.4 State machine: 5 states

Team standard: Todo -> Working -> Blocked -> Ready for Review -> Done.

| From | To | Event | Who | Guard |
|------|----|-------|-----|-------|
| todo | working | task.claimed | any agent | atomic; sets 12h lease + new fencing token |
| working | working | task.heartbeat | assignee | token matches, lease not expired; extends to now + 12h |
| working | todo | task.released | assignee | token matches |
| working | todo | task.lease_expired | system sweep | lease_until < now |
| working | blocked | task.blocked | assignee | token matches, `blocked_by` non-empty |
| blocked | working | task.unblocked | assignee or leader | new 12h lease |
| working | review | task.submitted | assignee | token matches, `evidence` non-empty |
| review | done | task.approved | leader only | all children (goal_ref) are done |
| review | working | task.rejected | leader only | reason non-empty; new lease for same assignee |

Everything else is rejected at the single writer. `done` is terminal.

### 2.5 Atomic claim, 12h lease, heartbeat

- Claim runs inside `BEGIN IMMEDIATE` on the single writer, so two agents can
  never both win. The loser gets "not claimable".
- Lease = 12h. Heartbeat extends it to now + 12h.
- Each claim increments `lease_token` (fencing token). Heartbeat, submit,
  block and release must present it; a stale holder whose lease expired and
  was re-claimed is rejected even if it comes back.
- Expired leases return the task to `todo` through an explicit
  `task.lease_expired` event (so expiry is in the chain, not silent). The sweep
  runs at the start of every write and on `ztask sweep` (cron).
- Team rule to add in a later version: dead-letter after 3 re-deliveries
  (OPEN, counter is already derivable from events).

### 2.6 Delivery semantics and idempotency

- Clients retry, so command delivery is at-least-once.
- Every write carries an `idempotency_key` (workflow-id style, client chosen,
  e.g. `claim:T12:ZC-Developer:2026-10-28T09`). `events.idem_key` is UNIQUE.
- Same key + same request fingerprint: return the original event, append
  nothing. Same key + different request: reject as conflict.
- Result: at-least-once delivery, effectively-once effect in the log.
- MVP-0 CLI convenience: if `--idem` is omitted, the key is derived from the
  request fingerprint plus the task's current event count (heartbeats also
  add the time). A later identical command is then a new attempt, and a
  duplicate `create` with identical fields is deduplicated. Real clients must
  pass their own key.

### 2.7 goal_ref ancestry

- `goal_ref` makes tasks a tree. The parent must exist at create (FK), and ids
  are new at create, so cycles cannot form.
- A parent cannot be approved while any child is not `done`.

### 2.8 Secret precheck

- Every write payload is scanned before anything is logged, with an
  Aho-Corasick automaton over known secret markers (private key headers,
  `ghp_`, `github_pat_`, `glpat-`, `sk-`, `sk_live_`, `xoxb-`, `xoxp-`,
  `AKIA`, `AIza`, ...). A hit rejects the write; the log never holds it.
- The marker list is data in code for MVP-0; OPEN whether to share it with
  the CLI's existing secret rules.

### 2.9 Aging and deadline escalation

- Sweep emits `task.escalated` once per level when a non-done task passes its
  `deadline`, or a `todo` task ages past a threshold (default 24h). Escalation
  goes through the same notification path.

### 2.10 Instant notification

- Each event writes outbox rows for the affected parties in the same
  transaction (owner, assignee, leaders on review/escalation).
- After commit the service pushes immediately to in-process listeners and the
  configured transport (OPEN: Telegram 7EN Labs group bot or local socket
  subscribers). Delivery is at-least-once; consumers ack by outbox id.
- MVP-0: listeners + outbox table + `ztask outbox` / `ztask ack`.

### 2.11 Storage settings

- SQLite file on box-local disk only (WAL needs shared memory on one host;
  never NFS or synced folders).
- `PRAGMA journal_mode=WAL`, `synchronous=FULL` for the log, `foreign_keys=ON`,
  `busy_timeout=5000`. Short transactions, `BEGIN IMMEDIATE` for writes.
- Single writer: the ztask service is the only process that writes; clients
  go through its API. MVP-0 CLI opens the DB per call and relies on
  `BEGIN IMMEDIATE` + busy_timeout.
- Backup: `VACUUM INTO '<path>'` on a schedule, then `verify` the copy.
- Move to Postgres only if ztask must run multi-box or become a shared store.

### 2.12 Access

- Authenticated local Unix socket (file mode 0600, peer credentials) or a
  bearer token over HTTP bound to localhost / tailnet. Never bare TCP.
- Actor identity comes from the token, not from the request body.
- MVP-0 is a local CLI only; no network listener.

## 3. Comparison with salvor-store 0.10.0

salvor-store (Rust, docs.rs/salvor-store 0.10.0) is an EventStore trait with a
SQLite WAL backend for an agent runtime. Its chain module:
https://docs.rs/salvor-store/latest/salvor_store/chain/index.html

| Topic | salvor-store | ztask | Borrow? |
|-------|--------------|-------|---------|
| Chain scope | per run, append order | one global chain, append order | pattern yes |
| Preimage | domain tag, prev, run_id, seq, exact envelope bytes, `\n` separators | domain tag, prev, seq, exact body bytes | yes |
| Spec string recorded in DB | `salvor.chain.v1` in metadata | `ztask.chain.v1` in meta | yes |
| Head record | ChainHead (count + last hash) | chain_head row | yes, catches truncation |
| Read-time verify | `read_log` verifies before returning; `StoreError::TamperEvident` names run + seq | `ztask verify` names first bad seq; error class `TamperEvident` | yes, name kept for test (c) |
| External anchor | `salvor anchor` exports heads to a file | planned, OPEN | later |
| Idempotency | `call_commitments` table keyed by idempotency key | UNIQUE `idem_key` on events + fingerprint | similar |
| Dependency | rusqlite, sha2, thiserror | none (Python stdlib) | no dependency added |

Borrowed as a pattern only. No code copied, no dependency added.

## 4. Data model (SQLite)

```
meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)
events(seq INTEGER PRIMARY KEY, idem_key TEXT NOT NULL UNIQUE,
       fingerprint TEXT NOT NULL, ts TEXT NOT NULL, actor TEXT NOT NULL,
       type TEXT NOT NULL, task_id TEXT, body TEXT NOT NULL,
       prev_hash TEXT NOT NULL, hash TEXT NOT NULL)
chain_head(id INTEGER PRIMARY KEY CHECK (id = 1), length INTEGER, hash TEXT)
tasks(id TEXT PRIMARY KEY, title, goal, goal_ref REFERENCES tasks(id),
      owner, acceptance, evidence, state CHECK (state IN (...5...)),
      assignee, lease_until, lease_token INTEGER, heartbeat_at, blocked_by,
      deadline, priority, created_at, updated_at, escalated_at)
outbox(id INTEGER PRIMARY KEY, seq REFERENCES events(seq), recipient,
       kind, delivered_at)
```

## 5. CLI (MVP-0)

```
ztask --db PATH init --leader "ZC Leader"
ztask create --title T --goal G --owner O --acceptance A [--goal-ref ID] [--deadline ISO]
ztask claim TASK --actor A           ztask heartbeat TASK --actor A --token N
ztask block TASK --actor A --token N --by REASON     ztask unblock TASK --actor A
ztask submit TASK --actor A --token N --evidence E
ztask approve TASK --actor LEADER    ztask reject TASK --actor LEADER --reason R
ztask release TASK --actor A --token N
ztask sweep | list | show TASK | log | verify | rebuild | backup PATH | outbox | ack ID
```

Every write accepts `--idem KEY`; if omitted the CLI derives one from the
request (fine for local use, real clients must pass their own).

## 6. Tests (MVP-0, stdlib unittest)

Acceptance tests:

- (a) `test_a_idempotent_event_log`: same key twice appends one event; same
  key with a different payload is rejected.
- (b) `test_b_lease_12h_expiry_returns_to_queue`: lease holds at 11h59m,
  heartbeat extends, expiry returns the task to todo through a logged event,
  another agent claims, stale fencing token is rejected.
- (c) `test_c_verify_detects_hand_edit_TamperEvident`: a raw SQL edit of one
  event body (still valid JSON) makes `verify` fail with TamperEvident at that
  seq; row deletion is also detected (`test_c2_tail_truncation_detected`
  covers dropping rows from the end).

Plus: 5-state transitions and illegal moves, Done only by leader, goal_ref
ancestry, secret precheck, instant notification + outbox, rebuild equals
projection, backup copy verifies, concurrent claim has one winner.

## 7. Delivery path

1. MVP-0 (this branch, local): Python stdlib in `tools/ztask/`.
2. MVP-1: service process with Unix socket + token auth, notifier transport,
   cron sweep, scheduled backup + anchor export.
3. Port decision (OPEN): keep Python tool, or Rust (`zen task` / own crate)
   which needs `rusqlite` (+ `sha2` as a direct dep).

## 8. Pre-deploy gate

No deploy of ztask until Z Ops' 6-item verification checklist for the team
comms box passes, plus the team verification lane for the change (lane B for
code/PR; lane C if it becomes outward-facing). MVP-0 is local only.

## 9. Non-goals for MVP-0

Network listener, multi-box, Notion sync, UI, dead-letter queue, external
anchor, Rust port.

## 10. Open decisions for ZC Leader

1. Language for MVP-1+: Python stdlib tool vs Rust (new dep `rusqlite`).
2. Confirm the 4 task elements (goal, owner, acceptance, evidence).
3. Confirm 5 state names (todo, working, blocked, review, done).
4. Notification transport (Telegram group bot vs socket subscribers).
5. External anchor target for chain_head.
6. Whether CI should run `python3 -m unittest` for `tools/ztask/`
   (ci.yml change, outside current scope).
7. Escalation thresholds (default 24h todo aging) and dead-letter after 3.
