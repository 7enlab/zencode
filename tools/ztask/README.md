# ztask (MVP-0)

Team task channel for ZenCode (Viec 10). Append-only, SHA-256 hash-chained
event log in SQLite (WAL); task state is a rebuildable projection.
Python 3 stdlib only, no dependencies. Design: [DESIGN.md](DESIGN.md).
Plan: [PLAN.md](PLAN.md).

State model follows Quy trinh MVP v3.0, Muc 18 (DESIGN.md sections 0 and
2.13-2.18):

```
QUEUED -> CLAIMED -> DOING -> BLOCKED -> REVIEW -> DONE -> DROPPED
```

- DONE needs the verified triple (verified_by + command + receipt, never the
  executor) and leader approval. REVIEW is not DONE.
- DROPPED replaces deletion (leader only, reason required).
- BLOCKED needs 4 fields: what is blocking, what was tried, who must unblock,
  effect on deadline.
- Lease per task: 12h default; presets `buffer-s` (30 min) and `shift` (8h).
  Dead-letter after 3 re-queues; a leader recovers it and the next executor
  resumes from the checkpoint.

## Run

```bash
cd tools/ztask
DB=/tmp/ztask.db
python3 ztask.py --db $DB init --leader "ZC Leader"
python3 ztask.py --db $DB create --title "Write DESIGN.md" \
  --goal "ztask design" --owner "ZC Leader" --acceptance "Leader review" \
  --level L2 --lease shift
python3 ztask.py --db $DB claim T1 --actor "ZC Platform"          # token 1
python3 ztask.py --db $DB start T1 --actor "ZC Platform" --token 1
python3 ztask.py --db $DB heartbeat T1 --actor "ZC Platform" --token 1 \
  --checkpoint "tests written, docs next"
python3 ztask.py --db $DB submit T1 --actor "ZC Platform" --token 1 \
  --evidence "test output + git diff --stat"
python3 ztask.py --db $DB attest T1 --actor "ZC QA" \
  --cmd "python3 -m unittest discover -s tools/ztask -p 'test_*.py'" \
  --receipt "Ran 24 tests OK"
python3 ztask.py --db $DB approve T1 --actor "ZC Leader"
python3 ztask.py --db $DB verify
```

Other commands:

```bash
python3 ztask.py --db $DB block T1 --actor A --token N \
  --by "what blocks" --tried "what was tried" --needs "who unblocks" \
  --impact "effect on deadline"
python3 ztask.py --db $DB unblock T1 --actor A
python3 ztask.py --db $DB reject T1 --actor "ZC Leader" --reason "add tests"
python3 ztask.py --db $DB drop T1 --actor "ZC Leader" --reason "scope cut"
python3 ztask.py --db $DB recover T1 --actor "ZC Leader" [--assignee A]
python3 ztask.py --db $DB release T1 --actor A --token N
python3 ztask.py --db $DB sweep | list | show T1 | log | rebuild | outbox
python3 ztask.py --db $DB backup /tmp/ztask-backup.db
```

`ZTASK_DB` sets the default database path. Exit codes: 0 ok, 1 rejected
command, 2 TamperEvident (log does not match its hash chain).

Keep the database on box-local disk only (never NFS or a synced folder).

## Test

```bash
python3 -m unittest discover -s tools/ztask -p 'test_*.py' -v
```

The three acceptance tests are `test_a_idempotent_event_log`,
`test_b_lease_12h_expiry_returns_to_queue` and
`test_c_verify_detects_hand_edit_TamperEvident`. Muc 18 alignment tests are
in `Muc18Tests`.
