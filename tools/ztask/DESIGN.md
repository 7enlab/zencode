# ztask - DESIGN (Viec 10)

Status: draft for checkpoint 2026-10-28. Acceptance 2026-10-30.
Updated 2026-10-27: aligned with Quy trinh MVP v3.0, Muc 18 (see section 0).
Owner: ZC Platform. Reviewer: ZC Leader. Spec is a living document.

The original approved design doc lives in ZC Leader's workspace
(`zencode-task-channel-design-2026-10-21.md`) and was not available to copy.
This file restates the decisions already made and fills the gaps needed for
MVP-0. Anything marked OPEN is a decision for ZC Leader.

## 0. Reconciliation with Quy trinh MVP v3.0, Muc 18

The team process page "Quy trinh MVP v3.0", Muc 18 (task model, effective
2026-10-25) is the live rule. Where it differs from older ztask specs, Muc 18
wins (Quy trinh outranks other specs per Muc 12). Replaced text below is kept
and marked "superseded by Muc 18"; nothing is deleted.

| Topic | Older spec (Viec 10 acceptance / aba714b) | Muc 18 (now implemented) |
|-------|-------------------------------------------|--------------------------|
| States | "5-level state machine": todo, working, blocked, review, done | 7 values: QUEUED, CLAIMED, DOING, BLOCKED, REVIEW, DONE, DROPPED |
| Removal | none (no delete, no cancel) | DROPPED replaces deletion (leader only) |
| Done | leader approval | leader approval AND verified triple; REVIEW is not DONE |
| Verification | evidence only | verified_by + verification command + receipt; no self-verify |
| BLOCKED | one `blocked_by` reason | 4 fields: what is blocking, what was tried, who must unblock, effect on deadline |
| Task card | goal, owner, acceptance, evidence + ops fields | adds verification, checkpoint, idempotency key, permission level L0-L4 |
| Lease | fixed 12h | per task; 12h default kept; pilot presets 30 min (buffer S) and 1 shift |
| Orphans | OPEN: dead-letter after 3 | dead-letter after 3 re-queues; recovery resumes from checkpoint |

Decision: implement Muc 18's 7 values. The acceptance line "5-level state
machine" predates Muc 18; every behaviour it asked for (claim, block, review,
leader-only done) is still present, so the acceptance intent is met with a
finer model. FLAG for ZC Leader: please confirm the acceptance check for
2026-10-30 reads against the 7-value model (see section 10).

## 1. Goal

ztask is the team task channel: one service that owns task state, enforces the
state machine at the service layer (not by discipline), keeps a tamper-evident
history, and notifies the right people the moment something changes.

Acceptance (Zen, 2026-10-30):

1. Idempotent event log.
2. Task record with 4 elements + 5-state machine (state machine superseded
   by Muc 18: 7 values, see section 0 and 2.14).
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

### 2.3 Task record: 4 elements (kept; card extended by Muc 18, see 2.13)

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
`created_at`, `updated_at`, `escalated_at`. (Superseded by Muc 18: the full
card is in 2.13; `escalated_at` is stored as `escalation`.)

### 2.4 State machine: 5 states (superseded by Muc 18, see 2.14)

Kept as history of the aba714b design. Not implemented any more.

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
- Lease = 12h. Heartbeat extends it to now + 12h. (Superseded by Muc 18:
  12h is now the default of a per-task lease, see 2.17.)
- Each claim increments `lease_token` (fencing token). Heartbeat, submit,
  block and release must present it; a stale holder whose lease expired and
  was re-claimed is rejected even if it comes back.
- Expired leases return the task to `todo` (now `QUEUED`) through an explicit
  `task.lease_expired` event (so expiry is in the chain, not silent). The sweep
  runs at the start of every write and on `ztask sweep` (cron).
- Team rule to add in a later version: dead-letter after 3 re-deliveries
  (OPEN, counter is already derivable from events). Superseded by Muc 18:
  implemented, see 2.17.

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
- A parent cannot be approved while any child is not `done`. (Muc 18: a
  child that is DONE or DROPPED no longer holds the parent open; a parent
  with a live child cannot be dropped; a DONE or DROPPED task cannot be a
  new goal_ref.)

### 2.8 Secret precheck

- Every write payload is scanned before anything is logged, with an
  Aho-Corasick automaton over known secret markers (private key headers,
  `ghp_`, `github_pat_`, `glpat-`, `sk-`, `sk_live_`, `xoxb-`, `xoxp-`,
  `AKIA`, `AIza`, ...). A hit rejects the write; the log never holds it.
- The marker list is data in code for MVP-0; OPEN whether to share it with
  the CLI's existing secret rules.

### 2.9 Aging and deadline escalation

- Sweep emits `task.escalated` once per level when a non-done task passes its
  `deadline`, or a `todo` (now `QUEUED`) task ages past a threshold (default 24h). Escalation
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

### 2.13 Task card (Muc 18)

| Muc 18 card item | Field(s) | Set by |
|------------------|----------|--------|
| status | `state` (7 values, 2.14) | state machine only |
| owner, separate from executor | `owner` (accountable) vs `assignee` (executor) | create / claim |
| evidence | `evidence` | submit (required) |
| verification | `verified_by`, `verification_cmd`, `verification_receipt` | attest (2.16) |
| blocked_by | `blocked_by`, `blocked_tried`, `blocked_needs`, `blocked_impact` | block (2.15) |
| lease until | `lease_until`, `lease_seconds` | claim / start / heartbeat (2.17) |
| heartbeat | `heartbeat_at` | claim / start / heartbeat |
| checkpoint | `checkpoint` | heartbeat `--checkpoint`; kept across re-queues |
| fencing token | `lease_token` | +1 on every claim, unblock, reject, recover |
| idempotency key | `idem_key` (key of the last event on the task; every event carries one) | every write |
| permission level | `level` L0-L4, default L2 | create |

Bookkeeping: `requeue_count`, `dead_letter`, `drop_reason`, `escalation`,
`goal`, `goal_ref`, `acceptance`, `deadline`, `priority`, timestamps.

### 2.14 State machine: 7 values (Muc 18)

QUEUED -> CLAIMED -> DOING -> BLOCKED -> REVIEW -> DONE -> DROPPED.
DONE and DROPPED are terminal. "Holder" = the assignee presenting the current
fencing token while the task is CLAIMED or DOING.

| From | To | Event | Who | Guard |
|------|----|-------|-----|-------|
| (new) | QUEUED | task.created | any | 4 elements; level L0-L4; valid lease; goal_ref exists and not terminal |
| QUEUED | CLAIMED | task.claimed | any agent | atomic; not dead-lettered; new token; lease = task lease; carries `resume_from` = checkpoint if any |
| CLAIMED | DOING | task.started | holder | refreshes lease |
| CLAIMED, DOING | same | task.heartbeat | holder | extends lease; optional checkpoint |
| CLAIMED, DOING | QUEUED | task.released | holder | requeue_count + 1 |
| CLAIMED, DOING | QUEUED | task.lease_expired | system sweep | lease_until < now; requeue_count + 1 |
| CLAIMED, DOING | BLOCKED | task.blocked | holder | all 4 BLOCKED fields (2.15) |
| BLOCKED | DOING | task.unblocked | assignee or leader | new token + lease; BLOCKED fields cleared |
| DOING | REVIEW | task.submitted | holder | evidence non-empty |
| REVIEW | REVIEW | task.verified | not the executor | command + receipt non-empty (2.16) |
| REVIEW | DONE | task.approved | leader only | verified triple present; no open children |
| REVIEW | DOING | task.rejected | leader only | reason; same executor, new token + lease; verification cleared |
| QUEUED, CLAIMED, DOING, BLOCKED, REVIEW | DROPPED | task.dropped | leader only | reason; no open children; replaces deletion |
| QUEUED (dead-lettered) | QUEUED or CLAIMED | task.recovered | leader only | resets requeue_count; keeps checkpoint (2.17) |

Everything else is rejected at the single writer. Wrong state raises
InvalidTransition; wrong actor or stale token raises NotAllowed.
BLOCKED leases are cleared (no lease runs while blocked).

### 2.15 BLOCKED needs 4 fields (Muc 18)

`block` refuses the write unless all four are non-empty (nothing is logged):
`blocked_by` (what is blocking), `blocked_tried` (what was tried),
`blocked_needs` (who must unblock), `blocked_impact` (effect on deadline).
CLI: `--by --tried --needs --impact`, all required. `task.blocked` notifies
owner, assignee and leaders. The team escalation timing (BLOCKED over 2h to
area leader, over 4h to ZC Leader) is not automated in MVP-0 (OPEN).

### 2.16 Verified = triple, no self-verify (Muc 18)

- `attest` on a REVIEW task records `verified_by` (the actor), the
  verification command, and a receipt (output digest, log line, link).
- The executor (`assignee`) cannot attest its own task (NotAllowed,
  "no self-verify").
- `approve` (leader only) refuses DONE unless all three are present.
  REVIEW is never DONE.
- `reject` voids the triple; the rework must be verified again.

### 2.17 Lease per task, dead-letter, recovery (Muc 18)

- Each task has `lease_seconds`, set at create with `--lease`:
  `default` = 12h (Viec 10 acceptance, used when omitted),
  `buffer-s` = 30 min (Muc 18 pilot, small buffer tasks),
  `shift` = 8h (Muc 18 pilot, one shift for main tasks; shifts are 8h),
  or an explicit `30m` / `8h` / seconds. Allowed range 1 minute to 7 days.
- Every claim, start, heartbeat, unblock, reject and recover sets
  `lease_until = now + lease_seconds`.
- Expiry or release returns the task to QUEUED and adds 1 to
  `requeue_count`. The checkpoint is kept.
- After 3 re-queues the sweep (or release) appends `task.dead_lettered`
  (notifies leaders) and sets `dead_letter = 1`. A dead-lettered task cannot
  be claimed.
- Recovery: a leader runs `recover` (optionally `--assignee`). The task goes
  back to QUEUED, or straight to CLAIMED for that assignee, with
  `requeue_count = 0`. The event carries `resume_from` = checkpoint, so the
  next executor resumes from it; with no checkpoint it is a restart, and the
  executor must reconcile side effects using the idempotency keys in the log.
- Release counts as a re-queue (inferred reading of "3 lan giao lai"; OPEN).

### 2.18 Permission level L0-L4 (Muc 18)

`level` is a card field, set at create, default L2. MVP-0 records it and
validates the value. It does not yet gate actions: Muc 18 says L3-L4 need
approval before work (prod deploy, data deletion, money, permission change,
outward posting, credentials). OPEN: enforce as "claim of an L3/L4 task needs
a leader pre-approval event".

### 2.19 Event schema version

Events now carry `"v": 2` (Muc 18 event types: task.started, task.verified,
task.dropped, task.dead_lettered, task.recovered). The chain construction
(`ztask.chain.v1`) is unchanged. Databases created by aba714b (v1 events) are
not migrated; none exist outside test temp dirs.

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
       fingerprint TEXT NOT NULL, task_id TEXT, body TEXT NOT NULL,
       prev_hash TEXT NOT NULL, hash TEXT NOT NULL)
       -- ts, actor, type live inside body (hashed), not as columns
chain_head(id INTEGER PRIMARY KEY CHECK (id = 1), length INTEGER, hash TEXT)
tasks(id TEXT PRIMARY KEY, title, goal, goal_ref REFERENCES tasks(id),
      owner, acceptance, level CHECK (level IN ('L0'..'L4')),
      state CHECK (state IN ('QUEUED','CLAIMED','DOING','BLOCKED',
                              'REVIEW','DONE','DROPPED')),
      assignee, evidence, verified_by, verification_cmd,
      verification_receipt, blocked_by, blocked_tried, blocked_needs,
      blocked_impact, lease_seconds INTEGER, lease_until,
      lease_token INTEGER, heartbeat_at, checkpoint, idem_key,
      requeue_count INTEGER, dead_letter INTEGER, drop_reason,
      deadline, priority, created_at, updated_at, escalation)
outbox(id INTEGER PRIMARY KEY, seq REFERENCES events(seq), recipient,
       kind, delivered_at)
```

## 5. CLI (MVP-0)

```
ztask --db PATH init --leader "ZC Leader"
ztask create --title T --goal G --owner O --acceptance A [--goal-ref ID]
             [--deadline ISO] [--level L0..L4] [--lease default|buffer-s|shift|30m]
ztask claim TASK --actor A           ztask start TASK --actor A --token N
ztask heartbeat TASK --actor A --token N [--checkpoint TEXT]
ztask block TASK --actor A --token N --by WHAT --tried TRIED --needs WHO --impact DEADLINE
ztask unblock TASK --actor A
ztask submit TASK --actor A --token N --evidence E
ztask attest TASK --actor VERIFIER --cmd COMMAND --receipt RECEIPT
ztask approve TASK --actor LEADER    ztask reject TASK --actor LEADER --reason R
ztask drop TASK --actor LEADER --reason R
ztask recover TASK --actor LEADER [--assignee A]
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

Muc 18 tests (`Muc18Tests`): 7 states declared, DROPPED path, BLOCKED
4-field rule, self-verify rejected and DONE needs the triple, dead-letter
after 3 re-queues with recover from checkpoint, release counts toward
dead-letter, configurable lease (default 12h, buffer-s, shift, custom, bad
values), task card fields and level.

Plus: 7-state transitions and illegal moves (was 5-state, superseded by
Muc 18), Done only by leader, goal_ref ancestry, secret precheck, instant notification + outbox, rebuild equals
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
anchor, Rust port. (Dead-letter: superseded by Muc 18, now a flag plus
leader recovery inside the store, see 2.17; a separate queue is still a
non-goal.)

## 10. Open decisions for ZC Leader

1. Language for MVP-1+: Python stdlib tool vs Rust (new dep `rusqlite`).
2. Confirm the 4 task elements (goal, owner, acceptance, evidence).
3. Confirm 5 state names (todo, working, blocked, review, done).
   Superseded by Muc 18: 7 values implemented. CONFIRM the 2026-10-30
   acceptance check reads "5-level state machine" against the 7-value model.
4. Notification transport (Telegram group bot vs socket subscribers).
5. External anchor target for chain_head.
6. Whether CI should run `python3 -m unittest` for `tools/ztask/`
   (ci.yml change, outside current scope).
7. Escalation thresholds (default 24h todo aging) and dead-letter after 3.
   Dead-letter after 3: superseded by Muc 18, implemented. Aging stays 24h.
8. Release counts as a re-queue toward dead-letter (inferred). Keep or
   count only lease expiry?
9. L3/L4 enforcement: add leader pre-approval before claim, or keep `level`
   as a recorded field only?
10. Shift preset = 8h (team shifts A/B/C are 8h). Confirm, or bind lease to
    the shift end time instead of a fixed length?
11. Default level for new tasks: L2 chosen. Confirm.
12. Who may unblock: assignee or leader now; should the person named in
    `blocked_needs` also be allowed?
13. Owner of Muc 4 page: Muc 4 still shows the old Vietnamese state names;
    Muc 18 is newer. Align Muc 4 (ZC Leader page).

## 11. Quyết định Leader 30/10 (tin 545)

ZC Leader verified and ACCEPTED MVP-0 at `cba8586` (Telegram 7EN Labs,
tin 545, 2026-10-30 01:58 +07). Decisions, closing the matching items in
section 10:

| # | Decision | MVP-0 (this code) | Moved to | Closes (section 10) |
|---|----------|-------------------|----------|---------------------|
| 1 | Release counts toward dead-letter after 3 re-queues | Keep code as is (2.17) | none | 8 |
| 2 | L3/L4: record the level and validate its value only | Keep code as is (2.18) | MVP-1: leader approval gate before claim | 9 |
| 3 | CI job `python-tests` for `tools/ztask/` | No workflow file now | Phase 6 merge PR adds the job | 6 |
| 4 | Notification transport Telegram | Outbox + listeners only (2.10) | MVP-1: Telegram transport | 4 |
| 5 | State model | 7 states per Muc 18 (2.14), final | none | 3 |

Moved to MVP-1: the L3/L4 approval gate before claim, and the Telegram
transport. Moved to the Phase 6 merge PR: the `python-tests` CI job.
Process note from the Leader: unrelated `ci.yml` changes go in a separate
change next time.

No code behaviour changes in this commit (docs only).
