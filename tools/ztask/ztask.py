#!/usr/bin/env python3
"""ztask MVP-0: team task channel with a hash-chained, append-only event log.

Python stdlib only (sqlite3, hashlib, json). See DESIGN.md in this directory.

The event log is the source of truth. The `tasks` table is a projection that
`rebuild` recreates by replaying events, and `verify` checks both the SHA-256
hash chain and that the projection matches a fresh replay.

State model follows Quy trinh MVP v3.0, Muc 18:
QUEUED -> CLAIMED -> DOING -> BLOCKED -> REVIEW -> DONE -> DROPPED.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import sys
from datetime import datetime, timedelta, timezone
from typing import Callable, Dict, Iterable, List, Optional

CHAIN_SPEC = "ztask.chain.v1"
EVENT_VERSION = 2  # v2 = Muc 18 state model (v1 logs from aba714b not replayed)
GENESIS_PREV_HASH = "0" * 64

# Lease: 12h default (Viec 10 acceptance), configurable per task.
DEFAULT_LEASE = timedelta(hours=12)
LEASE = DEFAULT_LEASE  # kept for callers of the MVP-0 name
LEASE_PRESETS = {
    "default": DEFAULT_LEASE,          # acceptance criterion
    "buffer-s": timedelta(minutes=30),  # Muc 18 pilot: small buffer tasks
    "shift": timedelta(hours=8),        # Muc 18 pilot: 1 shift, main tasks
}
LEASE_MIN = timedelta(minutes=1)
LEASE_MAX = timedelta(days=7)
DEAD_LETTER_AFTER = 3  # re-queues before a task is dead-lettered
DEFAULT_AGING = timedelta(hours=24)

STATES = ("QUEUED", "CLAIMED", "DOING", "BLOCKED", "REVIEW", "DONE", "DROPPED")
TERMINAL = ("DONE", "DROPPED")
HELD = ("CLAIMED", "DOING")  # states with a live lease
LEVELS = ("L0", "L1", "L2", "L3", "L4")
DEFAULT_LEVEL = "L2"

# BLOCKED needs all four (Muc 18): what is blocking, what was tried,
# who must unblock, effect on the deadline.
BLOCK_FIELDS = ("blocked_by", "blocked_tried", "blocked_needs",
                "blocked_impact")
VERIFY_FIELDS = ("verified_by", "verification_cmd", "verification_receipt")

TASK_FIELDS = (
    "id", "title", "goal", "goal_ref", "owner", "acceptance", "level",
    "state", "assignee", "evidence",
    "verified_by", "verification_cmd", "verification_receipt",
    "blocked_by", "blocked_tried", "blocked_needs", "blocked_impact",
    "lease_seconds", "lease_until", "lease_token", "heartbeat_at",
    "checkpoint", "idem_key", "requeue_count", "dead_letter", "drop_reason",
    "deadline", "priority", "created_at", "updated_at", "escalation",
)

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    seq         INTEGER PRIMARY KEY,
    idem_key    TEXT NOT NULL UNIQUE,
    fingerprint TEXT NOT NULL,
    task_id     TEXT,
    body        TEXT NOT NULL,
    prev_hash   TEXT NOT NULL,
    hash        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_task ON events(task_id);
CREATE TABLE IF NOT EXISTS chain_head (
    id     INTEGER PRIMARY KEY CHECK (id = 1),
    length INTEGER NOT NULL,
    hash   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
    id                   TEXT PRIMARY KEY,
    title                TEXT NOT NULL,
    goal                 TEXT NOT NULL,
    goal_ref             TEXT REFERENCES tasks(id),
    owner                TEXT NOT NULL,
    acceptance           TEXT NOT NULL,
    level                TEXT NOT NULL CHECK (level IN
                         ('L0', 'L1', 'L2', 'L3', 'L4')),
    state                TEXT NOT NULL CHECK (state IN
                         ('QUEUED', 'CLAIMED', 'DOING', 'BLOCKED', 'REVIEW',
                          'DONE', 'DROPPED')),
    assignee             TEXT,
    evidence             TEXT,
    verified_by          TEXT,
    verification_cmd     TEXT,
    verification_receipt TEXT,
    blocked_by           TEXT,
    blocked_tried        TEXT,
    blocked_needs        TEXT,
    blocked_impact       TEXT,
    lease_seconds        INTEGER NOT NULL,
    lease_until          TEXT,
    lease_token          INTEGER NOT NULL DEFAULT 0,
    heartbeat_at         TEXT,
    checkpoint           TEXT,
    idem_key             TEXT,
    requeue_count        INTEGER NOT NULL DEFAULT 0,
    dead_letter          INTEGER NOT NULL DEFAULT 0,
    drop_reason          TEXT,
    deadline             TEXT,
    priority             INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    escalation           TEXT
);
CREATE TABLE IF NOT EXISTS outbox (
    id           INTEGER PRIMARY KEY,
    seq          INTEGER NOT NULL REFERENCES events(seq),
    recipient    TEXT NOT NULL,
    kind         TEXT NOT NULL,
    delivered_at TEXT
);
"""


# --------------------------------------------------------------------------
# Errors


class ZtaskError(Exception):
    """Base error. `code` is a stable machine-readable class name."""

    code = "error"


class InvalidTransition(ZtaskError):
    code = "invalid_transition"


class NotAllowed(ZtaskError):
    code = "not_allowed"


class IdempotencyConflict(ZtaskError):
    code = "idempotency_conflict"


class SecretDetected(ZtaskError):
    code = "secret_detected"


class NotFound(ZtaskError):
    code = "not_found"


class TamperEvident(ZtaskError):
    """The stored log does not match its hash chain. Treat as an incident."""

    code = "tamper_evident"

    def __init__(self, seq: Optional[int], reason: str):
        super().__init__(f"TamperEvident at seq {seq}: {reason}")
        self.seq = seq
        self.reason = reason


# --------------------------------------------------------------------------
# Helpers


def canonical(obj) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False)


def sha256_hex(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def row_hash(prev_hash: str, seq: int, body: str) -> str:
    preimage = "\n".join((CHAIN_SPEC, prev_hash, str(seq), body))
    return sha256_hex(preimage)


def fmt_time(dt: datetime) -> str:
    return dt.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_time(text: str) -> datetime:
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    dt = datetime.fromisoformat(text)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def utc_now() -> datetime:
    return datetime.now(timezone.utc).replace(microsecond=0)


# --------------------------------------------------------------------------
# Secret precheck: Aho-Corasick over known secret markers


SECRET_MARKERS = (
    "-----BEGIN RSA PRIVATE KEY",
    "-----BEGIN OPENSSH PRIVATE KEY",
    "-----BEGIN EC PRIVATE KEY",
    "-----BEGIN PRIVATE KEY",
    "-----BEGIN PGP PRIVATE KEY",
    "ghp_", "gho_", "ghs_", "ghu_", "github_pat_",
    "glpat-",
    "sk-ant-", "sk-proj-", "sk-or-v1-",
    "sk_live_", "rk_live_",
    "xoxb-", "xoxp-", "xoxa-",
    "AKIA",
    "AIza",
)


class AhoCorasick:
    """Minimal Aho-Corasick automaton for literal pattern search."""

    def __init__(self, patterns: Iterable[str]):
        self.goto: List[Dict[str, int]] = [{}]
        self.fail: List[int] = [0]
        self.out: List[List[str]] = [[]]
        for pat in patterns:
            node = 0
            for ch in pat:
                nxt = self.goto[node].get(ch)
                if nxt is None:
                    nxt = len(self.goto)
                    self.goto[node][ch] = nxt
                    self.goto.append({})
                    self.fail.append(0)
                    self.out.append([])
                node = nxt
            self.out[node].append(pat)
        queue = list(self.goto[0].values())
        while queue:
            node = queue.pop(0)
            for ch, nxt in self.goto[node].items():
                queue.append(nxt)
                f = self.fail[node]
                while f and ch not in self.goto[f]:
                    f = self.fail[f]
                cand = self.goto[f].get(ch, 0)
                self.fail[nxt] = cand if cand != nxt else 0
                self.out[nxt] = self.out[nxt] + self.out[self.fail[nxt]]

    def find(self, text: str) -> List[str]:
        node = 0
        hits: List[str] = []
        for ch in text:
            while node and ch not in self.goto[node]:
                node = self.fail[node]
            node = self.goto[node].get(ch, 0)
            hits.extend(self.out[node])
        return hits


_SECRET_SCANNER = AhoCorasick(SECRET_MARKERS)


def secret_precheck(values: Iterable) -> None:
    for value in values:
        if isinstance(value, str):
            hits = _SECRET_SCANNER.find(value)
            if hits:
                # Never echo the value itself, only the marker class.
                raise SecretDetected(
                    f"write rejected: looks like a secret (marker {hits[0]!r})")




def parse_lease(value) -> int:
    """Lease length in seconds from a preset name, '30m'/'8h'/'90s', or int."""
    if value is None or value == "":
        secs = int(DEFAULT_LEASE.total_seconds())
    elif isinstance(value, timedelta):
        secs = int(value.total_seconds())
    elif isinstance(value, int) and not isinstance(value, bool):
        secs = value
    else:
        text = str(value).strip().lower()
        units = {"s": 1, "m": 60, "h": 3600}
        if text in LEASE_PRESETS:
            secs = int(LEASE_PRESETS[text].total_seconds())
        elif text[-1:] in units and text[:-1].isdigit():
            secs = int(text[:-1]) * units[text[-1]]
        elif text.isdigit():
            secs = int(text)
        else:
            raise ZtaskError(
                f"bad lease {value!r}: use a preset "
                f"({', '.join(sorted(LEASE_PRESETS))}) or e.g. 30m, 8h, 12h")
    if not (LEASE_MIN.total_seconds() <= secs <= LEASE_MAX.total_seconds()):
        raise ZtaskError("lease must be between 1 minute and 7 days")
    return secs


# --------------------------------------------------------------------------
# Projection: one pure function, used by live writes and by rebuild


def apply_event(tasks: Dict[str, dict], ev: dict) -> None:
    t = ev["type"]
    tid = ev["task_id"]
    d = ev["data"]
    ts = ev["ts"]
    if t == "task.created":
        task = {f: None for f in TASK_FIELDS}
        task.update(
            id=tid, title=d["title"], goal=d["goal"],
            goal_ref=d.get("goal_ref"), owner=d["owner"],
            acceptance=d["acceptance"], level=d.get("level", DEFAULT_LEVEL),
            state="QUEUED",
            lease_seconds=d.get("lease_seconds",
                                int(DEFAULT_LEASE.total_seconds())),
            lease_token=0, requeue_count=0, dead_letter=0,
            deadline=d.get("deadline"), priority=d.get("priority", 0),
            created_at=ts, updated_at=ts, idem_key=ev["idem_key"])
        tasks[tid] = task
        return
    task = tasks[tid]
    task["updated_at"] = ts
    task["idem_key"] = ev["idem_key"]
    if t == "task.claimed":
        task.update(state="CLAIMED", assignee=d["assignee"],
                    lease_token=d["token"], lease_until=d["lease_until"],
                    heartbeat_at=ts)
    elif t == "task.started":
        task.update(state="DOING", lease_until=d["lease_until"],
                    heartbeat_at=ts)
    elif t == "task.heartbeat":
        task.update(lease_until=d["lease_until"], heartbeat_at=ts)
        if d.get("checkpoint"):
            task["checkpoint"] = d["checkpoint"]
    elif t in ("task.released", "task.lease_expired"):
        # Checkpoint is kept so the next executor can resume from it.
        task.update(state="QUEUED", assignee=None, lease_until=None,
                    requeue_count=(task["requeue_count"] or 0) + 1)
    elif t == "task.dead_lettered":
        task["dead_letter"] = 1
    elif t == "task.recovered":
        task.update(dead_letter=0, requeue_count=0)
        if d.get("assignee"):
            task.update(state="CLAIMED", assignee=d["assignee"],
                        lease_token=d["token"], lease_until=d["lease_until"],
                        heartbeat_at=ts)
    elif t == "task.blocked":
        task.update(state="BLOCKED", lease_until=None,
                    **{f: d[f] for f in BLOCK_FIELDS})
    elif t in ("task.unblocked", "task.rejected"):
        task.update(state="DOING", assignee=d["assignee"],
                    lease_token=d["token"], lease_until=d["lease_until"],
                    heartbeat_at=ts, **{f: None for f in BLOCK_FIELDS})
        if t == "task.rejected":  # rework: the old verification is void
            task.update(**{f: None for f in VERIFY_FIELDS})
    elif t == "task.submitted":
        task.update(state="REVIEW", evidence=d["evidence"], lease_until=None)
    elif t == "task.verified":
        task.update(verified_by=d["verified_by"],
                    verification_cmd=d["command"],
                    verification_receipt=d["receipt"])
    elif t == "task.approved":
        task.update(state="DONE", lease_until=None)
    elif t == "task.dropped":
        task.update(state="DROPPED", lease_until=None,
                    drop_reason=d["reason"])
    elif t == "task.escalated":
        prior = [x for x in (task.get("escalation") or "").split(",") if x]
        if d["reason"] not in prior:
            prior.append(d["reason"])
        task["escalation"] = ",".join(prior)
    else:
        raise ZtaskError(f"unknown event type {t}")


LEADER_EVENTS = ("task.submitted", "task.blocked", "task.escalated",
                 "task.lease_expired", "task.dead_lettered", "task.verified",
                 "task.dropped", "task.recovered")


def recipients_for(ev: dict, task: dict, leaders: List[str]) -> List[str]:
    t = ev["type"]
    who: List[str] = []
    if t == "task.heartbeat":
        return []
    who.append(task["owner"])
    if task.get("assignee"):
        who.append(task["assignee"])
    if t == "task.lease_expired":
        who.append(ev["data"].get("previous_assignee") or "")
    if t in LEADER_EVENTS:
        who.extend(leaders)
    seen: List[str] = []
    for w in who:
        if w and w not in seen:
            seen.append(w)
    return seen


# --------------------------------------------------------------------------
# Store


Listener = Callable[[dict, List[str]], None]


class Store:
    def __init__(self, path: str, now: Optional[Callable[[], datetime]] = None,
                 aging: timedelta = DEFAULT_AGING):
        self.path = path
        self._now = now or utc_now
        self.aging = aging
        self.listeners: List[Listener] = []
        self.conn = sqlite3.connect(path, isolation_level=None, timeout=5.0)
        self.conn.row_factory = sqlite3.Row
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA synchronous=FULL")
        self.conn.execute("PRAGMA foreign_keys=ON")
        self.conn.execute("PRAGMA busy_timeout=5000")
        self.conn.executescript(SCHEMA)

    def close(self) -> None:
        self.conn.close()

    def now(self) -> datetime:
        return self._now()

    # -- meta ---------------------------------------------------------------

    def init(self, leaders: List[str]) -> None:
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            self.conn.execute(
                "INSERT OR IGNORE INTO meta(key, value) VALUES('chain_spec', ?)",
                (CHAIN_SPEC,))
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('leaders', ?) "
                "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                (json.dumps(leaders),))
            self.conn.execute(
                "INSERT OR IGNORE INTO chain_head(id, length, hash) "
                "VALUES(1, 0, ?)", (GENESIS_PREV_HASH,))
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise

    def leaders(self) -> List[str]:
        row = self.conn.execute(
            "SELECT value FROM meta WHERE key='leaders'").fetchone()
        return json.loads(row["value"]) if row else []

    # -- reads --------------------------------------------------------------

    def task(self, tid: str) -> dict:
        row = self.conn.execute("SELECT * FROM tasks WHERE id=?",
                                (tid,)).fetchone()
        if row is None:
            raise NotFound(f"no task {tid}")
        return dict(row)

    def tasks(self) -> List[dict]:
        return [dict(r) for r in self.conn.execute(
            "SELECT * FROM tasks ORDER BY rowid")]

    def events(self) -> List[dict]:
        out = []
        for r in self.conn.execute(
                "SELECT seq, body, hash FROM events ORDER BY seq"):
            ev = json.loads(r["body"])
            ev["seq"] = r["seq"]
            ev["hash"] = r["hash"]
            out.append(ev)
        return out

    def outbox(self, pending_only: bool = True) -> List[dict]:
        q = "SELECT * FROM outbox"
        if pending_only:
            q += " WHERE delivered_at IS NULL"
        return [dict(r) for r in self.conn.execute(q + " ORDER BY id")]

    def ack(self, outbox_id: int) -> None:
        self.conn.execute(
            "UPDATE outbox SET delivered_at=? WHERE id=? AND "
            "delivered_at IS NULL", (fmt_time(self.now()), outbox_id))

    # -- write core ---------------------------------------------------------

    def _load_task(self, tid: str) -> dict:
        row = self.conn.execute("SELECT * FROM tasks WHERE id=?",
                                (tid,)).fetchone()
        if row is None:
            raise NotFound(f"no task {tid}")
        return dict(row)

    def _save_task(self, task: dict) -> None:
        cols = ", ".join(TASK_FIELDS)
        marks = ", ".join("?" for _ in TASK_FIELDS)
        sets = ", ".join(f"{c}=excluded.{c}" for c in TASK_FIELDS if c != "id")
        self.conn.execute(
            f"INSERT INTO tasks({cols}) VALUES({marks}) "
            f"ON CONFLICT(id) DO UPDATE SET {sets}",
            tuple(task.get(c) for c in TASK_FIELDS))

    def _append(self, etype: str, task_id: str, actor: str, data: dict,
                idem_key: str, fingerprint: str, pending: List) -> dict:
        """Append one event inside the caller's open transaction."""
        head = self.conn.execute(
            "SELECT length, hash FROM chain_head WHERE id=1").fetchone()
        if head is None:
            raise ZtaskError("store not initialised: run `ztask init`")
        seq = head["length"] + 1
        prev = head["hash"]
        ev = {"v": EVENT_VERSION, "type": etype, "task_id": task_id,
              "actor": actor, "ts": fmt_time(self.now()),
              "idem_key": idem_key, "fingerprint": fingerprint, "data": data}
        body = canonical(ev)
        h = row_hash(prev, seq, body)
        self.conn.execute(
            "INSERT INTO events(seq, idem_key, fingerprint, task_id, body, "
            "prev_hash, hash) VALUES(?, ?, ?, ?, ?, ?, ?)",
            (seq, idem_key, fingerprint, task_id, body, prev, h))
        self.conn.execute("UPDATE chain_head SET length=?, hash=? WHERE id=1",
                          (seq, h))
        tasks: Dict[str, dict] = {}
        if etype != "task.created":
            tasks[task_id] = self._load_task(task_id)
        apply_event(tasks, ev)
        task = tasks[task_id]
        self._save_task(task)
        recips = recipients_for(ev, task, self.leaders())
        for r in recips:
            self.conn.execute(
                "INSERT INTO outbox(seq, recipient, kind) VALUES(?, ?, ?)",
                (seq, r, etype))
        ev["seq"] = seq
        ev["hash"] = h
        pending.append((ev, recips))
        return ev

    def _after_requeue(self, task_id: str, pending: List) -> None:
        """Dead-letter a task once it has been re-queued DEAD_LETTER_AFTER
        times. Inside the caller's transaction."""
        task = self._load_task(task_id)
        if task["dead_letter"] or task["requeue_count"] < DEAD_LETTER_AFTER:
            return
        key = (f"sys:dead_lettered:{task_id}:{task['lease_token']}:"
               f"{task['requeue_count']}")
        self._append("task.dead_lettered", task_id, "system",
                     {"requeue_count": task["requeue_count"],
                      "checkpoint": task["checkpoint"]},
                     key, sha256_hex(key), pending)

    def _sweep(self, pending: List) -> int:
        """Expire leases, dead-letter, escalate. Inside a transaction."""
        now = self.now()
        n = 0
        for row in self.conn.execute(
                "SELECT * FROM tasks WHERE state IN ('CLAIMED', 'DOING') AND "
                "lease_until IS NOT NULL").fetchall():
            if parse_time(row["lease_until"]) < now:
                key = f"sys:lease_expired:{row['id']}:{row['lease_token']}"
                self._append("task.lease_expired", row["id"], "system",
                             {"previous_assignee": row["assignee"],
                              "previous_state": row["state"],
                              "token": row["lease_token"]},
                             key, sha256_hex(key), pending)
                self._after_requeue(row["id"], pending)
                n += 1
        for row in self.conn.execute(
                "SELECT * FROM tasks WHERE state NOT IN ('DONE', 'DROPPED')"
                ).fetchall():
            done = (row["escalation"] or "").split(",")
            reasons = []
            if row["deadline"] and parse_time(row["deadline"]) < now:
                reasons.append("deadline")
            if (row["state"] == "QUEUED"
                    and parse_time(row["created_at"]) + self.aging < now):
                reasons.append("aging")
            for reason in reasons:
                if reason in done:
                    continue
                key = f"sys:escalated:{row['id']}:{reason}"
                self._append("task.escalated", row["id"], "system",
                             {"reason": reason}, key, sha256_hex(key),
                             pending)
                n += 1
        return n

    def _write(self, cmd: str, task_id: Optional[str], actor: str,
               args: dict, idem_key: Optional[str],
               build: Callable[[List, str, str], dict],
               auto_salt: str = "") -> dict:
        secret_precheck([actor, task_id] + list(args.values()))
        fingerprint = sha256_hex(canonical(
            {"cmd": cmd, "task_id": task_id, "actor": actor, "args": args}))
        if idem_key:
            secret_precheck([idem_key])
        pending: List = []
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            if not idem_key:
                # Derived key for local CLI use: tied to the task's current
                # version, so a later identical command is a new attempt.
                # Real clients pass their own workflow-id style key.
                version = self.conn.execute(
                    "SELECT COUNT(*) FROM events WHERE task_id IS ?",
                    (task_id,)).fetchone()[0] if task_id else 0
                idem_key = f"auto:{fingerprint}:v{version}{auto_salt}"
            prior = self.conn.execute(
                "SELECT seq, fingerprint, body, hash FROM events "
                "WHERE idem_key=?", (idem_key,)).fetchone()
            if prior is not None:
                self.conn.execute("ROLLBACK")
                if prior["fingerprint"] != fingerprint:
                    raise IdempotencyConflict(
                        f"idempotency key {idem_key!r} reused with a "
                        f"different request")
                ev = json.loads(prior["body"])
                ev.update(seq=prior["seq"], hash=prior["hash"], replayed=True)
                return ev
            self._sweep(pending)
            ev = build(pending, idem_key, fingerprint)
            self.conn.execute("COMMIT")
        except Exception:
            if self.conn.in_transaction:
                self.conn.execute("ROLLBACK")
            raise
        self._notify(pending)
        return ev

    def _notify(self, pending: List) -> None:
        for ev, recips in pending:
            for fn in self.listeners:
                try:
                    fn(ev, recips)
                except Exception:  # outbox keeps it for redelivery
                    pass

    def _require_holder(self, task: dict, actor: str, token: int,
                        states=HELD) -> None:
        if task["state"] not in HELD:
            raise InvalidTransition(
                f"{task['id']} is {task['state']}, no live lease")
        if task["assignee"] != actor or int(token) != task["lease_token"]:
            raise NotAllowed(
                f"{actor} does not hold the current lease on {task['id']} "
                f"(stale or wrong fencing token)")
        if task["state"] not in states:
            raise InvalidTransition(
                f"{task['id']} is {task['state']}, needs "
                f"{' or '.join(states)}")

    def _require_leader(self, actor: str, what: str) -> None:
        if actor not in self.leaders():
            raise NotAllowed(f"only a leader can {what}")

    def _open_children(self, task_id: str) -> List[str]:
        return [r["id"] for r in self.conn.execute(
            "SELECT id FROM tasks WHERE goal_ref=? AND "
            "state NOT IN ('DONE', 'DROPPED')", (task_id,))]

    def _lease_until(self, task: dict) -> str:
        return fmt_time(self.now() + timedelta(seconds=task["lease_seconds"]))

    # -- commands -----------------------------------------------------------

    def create(self, title: str, goal: str, owner: str, acceptance: str,
               actor: Optional[str] = None, goal_ref: Optional[str] = None,
               deadline: Optional[str] = None, priority: int = 0,
               level: Optional[str] = None, lease=None,
               idem_key: Optional[str] = None) -> dict:
        actor = actor or owner
        level = (level or DEFAULT_LEVEL).strip().upper()
        if level not in LEVELS:
            raise ZtaskError(f"level must be one of {', '.join(LEVELS)}")
        args = {"title": title, "goal": goal, "owner": owner,
                "acceptance": acceptance, "goal_ref": goal_ref,
                "deadline": deadline, "priority": priority, "level": level,
                "lease_seconds": parse_lease(lease)}
        for name in ("title", "goal", "owner", "acceptance"):
            if not (args[name] or "").strip():
                raise ZtaskError(f"task record needs a non-empty {name}")
        if deadline:
            deadline = fmt_time(parse_time(deadline))
            args["deadline"] = deadline

        def build(pending, key, fp):
            if goal_ref is not None:
                parent = self._load_task(goal_ref)
                if parent["state"] in TERMINAL:
                    raise InvalidTransition(
                        f"goal_ref {goal_ref} is already {parent['state']}")
            n = self.conn.execute(
                "SELECT COUNT(*) FROM tasks").fetchone()[0]
            tid = f"T{n + 1}"
            return self._append("task.created", tid, actor, args, key, fp,
                                pending)

        return self._write("create", None, actor, args, idem_key, build)

    def claim(self, task_id: str, actor: str,
              idem_key: Optional[str] = None) -> dict:
        """QUEUED -> CLAIMED. Atomic; new fencing token and lease."""
        def build(pending, key, fp):
            task = self._load_task(task_id)
            if task["state"] != "QUEUED":
                raise InvalidTransition(
                    f"{task_id} is {task['state']}, not claimable")
            if task["dead_letter"]:
                raise NotAllowed(
                    f"{task_id} is dead-lettered; a leader must recover it")
            data = {"assignee": actor, "token": task["lease_token"] + 1,
                    "lease_until": self._lease_until(task)}
            if task["checkpoint"]:
                data["resume_from"] = task["checkpoint"]
            return self._append("task.claimed", task_id, actor, data, key,
                                fp, pending)

        return self._write("claim", task_id, actor, {}, idem_key, build)

    def start(self, task_id: str, actor: str, token: int,
              idem_key: Optional[str] = None) -> dict:
        """CLAIMED -> DOING (holder). Refreshes the lease."""
        args = {"token": int(token)}

        def build(pending, key, fp):
            task = self._load_task(task_id)
            self._require_holder(task, actor, token, ("CLAIMED",))
            return self._append("task.started", task_id, actor,
                                {"token": int(token),
                                 "lease_until": self._lease_until(task)},
                                key, fp, pending)

        return self._write("start", task_id, actor, args, idem_key, build)

    def heartbeat(self, task_id: str, actor: str, token: int,
                  checkpoint: Optional[str] = None,
                  idem_key: Optional[str] = None) -> dict:
        args = {"token": int(token), "checkpoint": checkpoint}

        def build(pending, key, fp):
            task = self._load_task(task_id)
            self._require_holder(task, actor, token)
            data = {"lease_until": self._lease_until(task)}
            if checkpoint:
                data["checkpoint"] = checkpoint
            return self._append("task.heartbeat", task_id, actor, data,
                                key, fp, pending)

        # Heartbeats repeat, so the derived key is salted with the time.
        return self._write("heartbeat", task_id, actor, args, idem_key, build,
                           auto_salt=":" + fmt_time(self.now()))

    def release(self, task_id: str, actor: str, token: int,
                idem_key: Optional[str] = None) -> dict:
        def build(pending, key, fp):
            task = self._load_task(task_id)
            self._require_holder(task, actor, token)
            ev = self._append("task.released", task_id, actor,
                              {"token": int(token)}, key, fp, pending)
            self._after_requeue(task_id, pending)
            return ev

        return self._write("release", task_id, actor, {"token": int(token)},
                           idem_key, build)

    def block(self, task_id: str, actor: str, token: int, blocked_by: str,
              tried: str = "", needs: str = "", impact: str = "",
              idem_key: Optional[str] = None) -> dict:
        """CLAIMED/DOING -> BLOCKED. All four Muc 18 fields are required."""
        fields = {"blocked_by": blocked_by, "blocked_tried": tried,
                  "blocked_needs": needs, "blocked_impact": impact}
        missing = [k for k, v in fields.items() if not (v or "").strip()]
        if missing:
            raise ZtaskError(
                "BLOCKED needs 4 fields (what is blocking, what was tried, "
                "who must unblock, effect on deadline); missing: "
                + ", ".join(missing))
        args = dict(fields, token=int(token))

        def build(pending, key, fp):
            task = self._load_task(task_id)
            self._require_holder(task, actor, token)
            return self._append("task.blocked", task_id, actor, fields,
                                key, fp, pending)

        return self._write("block", task_id, actor, args, idem_key, build)

    def unblock(self, task_id: str, actor: str,
                idem_key: Optional[str] = None) -> dict:
        """BLOCKED -> DOING (assignee or leader). New token and lease."""
        def build(pending, key, fp):
            task = self._load_task(task_id)
            if task["state"] != "BLOCKED":
                raise InvalidTransition(
                    f"{task_id} is {task['state']}, not BLOCKED")
            if actor != task["assignee"] and actor not in self.leaders():
                raise NotAllowed("only the assignee or a leader can unblock")
            return self._append(
                "task.unblocked", task_id, actor,
                {"assignee": task["assignee"],
                 "token": task["lease_token"] + 1,
                 "lease_until": self._lease_until(task)}, key, fp, pending)

        return self._write("unblock", task_id, actor, {}, idem_key, build)

    def submit(self, task_id: str, actor: str, token: int, evidence: str,
               idem_key: Optional[str] = None) -> dict:
        """DOING -> REVIEW (holder). REVIEW is not DONE."""
        if not (evidence or "").strip():
            raise ZtaskError("submit needs non-empty evidence")
        args = {"token": int(token), "evidence": evidence}

        def build(pending, key, fp):
            task = self._load_task(task_id)
            self._require_holder(task, actor, token, ("DOING",))
            return self._append("task.submitted", task_id, actor,
                                {"evidence": evidence}, key, fp, pending)

        return self._write("submit", task_id, actor, args, idem_key, build)

    def attest(self, task_id: str, actor: str, command: str, receipt: str,
               idem_key: Optional[str] = None) -> dict:
        """Record the verified triple on a REVIEW task: verified_by (actor)
        + verification command + receipt. The executor cannot verify."""
        for name, value in (("verification command", command),
                            ("receipt", receipt)):
            if not (value or "").strip():
                raise ZtaskError(f"verification needs a non-empty {name}")
        args = {"command": command, "receipt": receipt}

        def build(pending, key, fp):
            task = self._load_task(task_id)
            if task["state"] != "REVIEW":
                raise InvalidTransition(
                    f"{task_id} is {task['state']}, not REVIEW")
            if actor == task["assignee"]:
                raise NotAllowed(
                    f"{actor} executed {task_id} and cannot verify it "
                    f"(no self-verify)")
            return self._append("task.verified", task_id, actor,
                                {"verified_by": actor, "command": command,
                                 "receipt": receipt}, key, fp, pending)

        return self._write("attest", task_id, actor, args, idem_key, build)

    def approve(self, task_id: str, actor: str,
                idem_key: Optional[str] = None) -> dict:
        """REVIEW -> DONE. Leader only, needs the verified triple."""
        def build(pending, key, fp):
            self._require_leader(actor, "approve DONE")
            task = self._load_task(task_id)
            if task["state"] != "REVIEW":
                raise InvalidTransition(
                    f"{task_id} is {task['state']}, not REVIEW")
            if not all(task[f] for f in VERIFY_FIELDS):
                raise InvalidTransition(
                    f"{task_id} is not verified: DONE needs verified_by + "
                    f"verification command + receipt")
            open_children = self._open_children(task_id)
            if open_children:
                raise InvalidTransition(
                    f"{task_id} has open children: "
                    f"{', '.join(open_children)}")
            return self._append("task.approved", task_id, actor, {}, key, fp,
                                pending)

        return self._write("approve", task_id, actor, {}, idem_key, build)

    def reject(self, task_id: str, actor: str, reason: str,
               idem_key: Optional[str] = None) -> dict:
        """REVIEW -> DOING (leader). Same executor, new token and lease."""
        if not (reason or "").strip():
            raise ZtaskError("reject needs a reason")

        def build(pending, key, fp):
            self._require_leader(actor, "reject a review")
            task = self._load_task(task_id)
            if task["state"] != "REVIEW":
                raise InvalidTransition(
                    f"{task_id} is {task['state']}, not REVIEW")
            return self._append(
                "task.rejected", task_id, actor,
                {"assignee": task["assignee"], "reason": reason,
                 "token": task["lease_token"] + 1,
                 "lease_until": self._lease_until(task)}, key, fp, pending)

        return self._write("reject", task_id, actor, {"reason": reason},
                           idem_key, build)

    def drop(self, task_id: str, actor: str, reason: str,
             idem_key: Optional[str] = None) -> dict:
        """Any non-terminal state -> DROPPED (leader). Replaces deletion."""
        if not (reason or "").strip():
            raise ZtaskError("drop needs a reason")

        def build(pending, key, fp):
            self._require_leader(actor, "drop a task")
            task = self._load_task(task_id)
            if task["state"] in TERMINAL:
                raise InvalidTransition(
                    f"{task_id} is {task['state']}, already terminal")
            open_children = self._open_children(task_id)
            if open_children:
                raise InvalidTransition(
                    f"{task_id} has open children: "
                    f"{', '.join(open_children)} (drop or finish them first)")
            return self._append("task.dropped", task_id, actor,
                                {"reason": reason,
                                 "previous_state": task["state"]},
                                key, fp, pending)

        return self._write("drop", task_id, actor, {"reason": reason},
                           idem_key, build)

    def recover(self, task_id: str, actor: str,
                assignee: Optional[str] = None,
                idem_key: Optional[str] = None) -> dict:
        """Leader recovers a dead-lettered task: back to QUEUED, or straight
        to CLAIMED for `assignee`. The checkpoint is kept (resume_from)."""
        args = {"assignee": assignee}

        def build(pending, key, fp):
            self._require_leader(actor, "recover a dead-lettered task")
            task = self._load_task(task_id)
            if task["state"] != "QUEUED" or not task["dead_letter"]:
                raise InvalidTransition(f"{task_id} is not dead-lettered")
            data = {"assignee": assignee, "resume_from": task["checkpoint"]}
            if assignee:
                data.update(token=task["lease_token"] + 1,
                            lease_until=self._lease_until(task))
            return self._append("task.recovered", task_id, actor, data,
                                key, fp, pending)

        return self._write("recover", task_id, actor, args, idem_key, build)

    def sweep(self) -> int:
        pending: List = []
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            n = self._sweep(pending)
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise
        self._notify(pending)
        return n

    # -- integrity ----------------------------------------------------------

    def replay(self) -> Dict[str, dict]:
        tasks: Dict[str, dict] = {}
        for r in self.conn.execute("SELECT body FROM events ORDER BY seq"):
            apply_event(tasks, json.loads(r["body"]))
        return tasks

    def verify(self, check_projection: bool = True) -> dict:
        """Recompute the chain. Raises TamperEvident on the first mismatch."""
        running = GENESIS_PREV_HASH
        expected = 0
        for r in self.conn.execute(
                "SELECT seq, idem_key, task_id, body, prev_hash, hash "
                "FROM events ORDER BY seq"):
            expected += 1
            seq = r["seq"]
            if seq != expected:
                raise TamperEvident(
                    expected, f"seq gap: expected {expected}, found {seq} "
                    f"(row removed or inserted)")
            if r["prev_hash"] != running:
                raise TamperEvident(seq, "prev_hash link broken")
            if row_hash(r["prev_hash"], seq, r["body"]) != r["hash"]:
                raise TamperEvident(seq, "hash mismatch (event bytes edited)")
            try:
                body = json.loads(r["body"])
            except ValueError:
                raise TamperEvident(seq, "body is not JSON")
            if (body.get("idem_key") != r["idem_key"]
                    or body.get("task_id") != r["task_id"]):
                raise TamperEvident(seq, "index columns disagree with body")
            running = r["hash"]
        head = self.conn.execute(
            "SELECT length, hash FROM chain_head WHERE id=1").fetchone()
        if head is None:
            raise TamperEvident(None, "chain_head missing")
        if head["length"] != expected or head["hash"] != running:
            raise TamperEvident(
                expected + 1 if head["length"] > expected else expected,
                f"chain head mismatch: head says {head['length']} events, "
                f"log has {expected} (rows dropped or head edited)")
        if check_projection:
            replayed = self.replay()
            stored = {t["id"]: t for t in self.tasks()}
            if set(replayed) != set(stored):
                raise ZtaskError("projection drift: task set differs from "
                                 "replay (run `ztask rebuild`)")
            for tid, t in replayed.items():
                for c in TASK_FIELDS:
                    if t.get(c) != stored[tid].get(c):
                        raise ZtaskError(
                            f"projection drift on {tid}.{c} "
                            f"(run `ztask rebuild`)")
        return {"ok": True, "events": expected, "head": running}

    def rebuild(self) -> int:
        self.verify(check_projection=False)
        tasks = self.replay()
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            self.conn.execute("DELETE FROM tasks")
            for t in tasks.values():
                self._save_task(t)
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise
        return len(tasks)

    def backup(self, dest: str) -> str:
        if os.path.exists(dest):
            raise ZtaskError(f"backup target {dest} exists; not overwriting")
        self.conn.execute("VACUUM INTO ?", (dest,))
        return dest


# --------------------------------------------------------------------------
# CLI


def _now_from_env() -> Optional[Callable[[], datetime]]:
    fixed = os.environ.get("ZTASK_NOW")
    if fixed:
        dt = parse_time(fixed)
        return lambda: dt
    return None


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="ztask", description=__doc__.split("\n")[0])
    p.add_argument("--db", default=os.environ.get("ZTASK_DB", "ztask.db"))
    sub = p.add_subparsers(dest="cmd", required=True)

    def add(name, *fields, actor=True, token=False, idem=True):
        sp = sub.add_parser(name)
        for f in fields:
            sp.add_argument(f)
        if actor:
            sp.add_argument("--actor", required=True)
        if token:
            sp.add_argument("--token", required=True, type=int)
        if idem:
            sp.add_argument("--idem", default=None)
        return sp

    sp = sub.add_parser("init")
    sp.add_argument("--leader", action="append", required=True)
    sp = add("create", actor=False)
    for f in ("--title", "--goal", "--owner", "--acceptance"):
        sp.add_argument(f, required=True)
    sp.add_argument("--actor")
    sp.add_argument("--goal-ref")
    sp.add_argument("--deadline")
    sp.add_argument("--priority", type=int, default=0)
    sp.add_argument("--level", default=DEFAULT_LEVEL, choices=LEVELS)
    sp.add_argument("--lease", default=None,
                    help="default (12h), buffer-s (30m), shift (8h), or "
                         "e.g. 30m / 8h")
    add("claim", "task")
    add("start", "task", token=True)
    add("heartbeat", "task", token=True).add_argument("--checkpoint")
    add("release", "task", token=True)
    sp = add("block", "task", token=True)
    sp.add_argument("--by", required=True, help="what is blocking")
    sp.add_argument("--tried", required=True, help="what was tried")
    sp.add_argument("--needs", required=True, help="who must unblock")
    sp.add_argument("--impact", required=True, help="effect on deadline")
    add("unblock", "task")
    add("submit", "task", token=True).add_argument("--evidence", required=True)
    sp = add("attest", "task")
    sp.add_argument("--cmd", required=True, dest="vcmd",
                    help="verification command that was run")
    sp.add_argument("--receipt", required=True,
                    help="receipt: output digest, log link, or result")
    add("approve", "task")
    add("reject", "task").add_argument("--reason", required=True)
    add("drop", "task").add_argument("--reason", required=True)
    add("recover", "task").add_argument("--assignee")
    for name in ("sweep", "list", "log", "verify", "rebuild", "outbox"):
        sub.add_parser(name)
    sub.add_parser("show").add_argument("task")
    sub.add_parser("backup").add_argument("path")
    sub.add_parser("ack").add_argument("id", type=int)
    return p


def main(argv: Optional[List[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    store = Store(args.db, now=_now_from_env())
    try:
        c = args.cmd
        if c == "init":
            store.init(args.leader)
            out = {"ok": True, "leaders": args.leader}
        elif c == "create":
            out = store.create(args.title, args.goal, args.owner,
                               args.acceptance, actor=args.actor,
                               goal_ref=args.goal_ref, deadline=args.deadline,
                               priority=args.priority, level=args.level,
                               lease=args.lease, idem_key=args.idem)
        elif c == "claim":
            out = store.claim(args.task, args.actor, args.idem)
        elif c == "start":
            out = store.start(args.task, args.actor, args.token, args.idem)
        elif c == "heartbeat":
            out = store.heartbeat(args.task, args.actor, args.token,
                                  args.checkpoint, args.idem)
        elif c == "release":
            out = store.release(args.task, args.actor, args.token, args.idem)
        elif c == "block":
            out = store.block(args.task, args.actor, args.token, args.by,
                              args.tried, args.needs, args.impact, args.idem)
        elif c == "unblock":
            out = store.unblock(args.task, args.actor, args.idem)
        elif c == "submit":
            out = store.submit(args.task, args.actor, args.token,
                               args.evidence, args.idem)
        elif c == "attest":
            out = store.attest(args.task, args.actor, args.vcmd,
                               args.receipt, args.idem)
        elif c == "approve":
            out = store.approve(args.task, args.actor, args.idem)
        elif c == "reject":
            out = store.reject(args.task, args.actor, args.reason, args.idem)
        elif c == "drop":
            out = store.drop(args.task, args.actor, args.reason, args.idem)
        elif c == "recover":
            out = store.recover(args.task, args.actor, args.assignee,
                                args.idem)
        elif c == "sweep":
            out = {"events": store.sweep()}
        elif c == "list":
            out = store.tasks()
        elif c == "show":
            out = store.task(args.task)
        elif c == "log":
            out = store.events()
        elif c == "verify":
            out = store.verify()
        elif c == "rebuild":
            out = {"tasks": store.rebuild()}
        elif c == "backup":
            out = {"backup": store.backup(args.path)}
        elif c == "outbox":
            out = store.outbox()
        elif c == "ack":
            store.ack(args.id)
            out = {"ok": True}
        else:  # pragma: no cover
            raise ZtaskError(f"unknown command {c}")
        print(json.dumps(out, ensure_ascii=False, indent=2))
        return 0
    except TamperEvident as e:
        print(json.dumps({"ok": False, "error": e.code, "seq": e.seq,
                          "reason": e.reason}), file=sys.stderr)
        return 2
    except ZtaskError as e:
        print(json.dumps({"ok": False, "error": e.code, "message": str(e)},
                         ensure_ascii=False), file=sys.stderr)
        return 1
    finally:
        store.close()


if __name__ == "__main__":
    sys.exit(main())
