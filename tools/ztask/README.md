# ztask (MVP-0)

Team task channel for ZenCode (Viec 10). Append-only, SHA-256 hash-chained
event log in SQLite (WAL); task state is a rebuildable projection.
Python 3 stdlib only, no dependencies. Design: [DESIGN.md](DESIGN.md).
Plan: [PLAN.md](PLAN.md).

## Run

```bash
cd tools/ztask
python3 ztask.py --db /tmp/ztask.db init --leader "ZC Leader"
python3 ztask.py --db /tmp/ztask.db create --title "Write DESIGN.md" \
  --goal "ztask design" --owner "ZC Leader" --acceptance "Leader review"
python3 ztask.py --db /tmp/ztask.db claim T1 --actor "ZC Platform"
python3 ztask.py --db /tmp/ztask.db heartbeat T1 --actor "ZC Platform" --token 1
python3 ztask.py --db /tmp/ztask.db submit T1 --actor "ZC Platform" --token 1 \
  --evidence "test output + git diff --stat"
python3 ztask.py --db /tmp/ztask.db approve T1 --actor "ZC Leader"
python3 ztask.py --db /tmp/ztask.db verify
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
`test_c_verify_detects_hand_edit_TamperEvident`.
