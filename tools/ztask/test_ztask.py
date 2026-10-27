#!/usr/bin/env python3
"""Unit tests for ztask.py (stdlib unittest, no network, temp SQLite files).

Acceptance tests for Viec 10 (2026-10-30) are the three `test_a_`, `test_b_`,
`test_c_` cases in AcceptanceTests.
"""

import io
import json
import os
import sqlite3
import sys
import tempfile
import threading
import unittest
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import ztask  # noqa: E402
from ztask import (  # noqa: E402
    AhoCorasick, IdempotencyConflict, InvalidTransition, NotAllowed,
    SecretDetected, Store, TamperEvident, ZtaskError,
)

LEADER = "ZC Leader"
DEV = "ZC Developer"
DEV2 = "ZC Platform"
T0 = datetime(2026, 10, 28, 9, 0, 0, tzinfo=timezone.utc)


class Clock:
    def __init__(self, start=T0):
        self.t = start

    def __call__(self):
        return self.t

    def advance(self, **kw):
        self.t += timedelta(**kw)


class Base(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, "ztask.db")
        self.clock = Clock()
        self.store = Store(self.path, now=self.clock)
        self.store.init([LEADER])

    def tearDown(self):
        self.store.close()
        self.dir.cleanup()

    def new_task(self, title="Write DESIGN.md", **kw):
        ev = self.store.create(title=title, goal="ztask design for Viec 10",
                               owner=LEADER,
                               acceptance="Leader reviews DESIGN.md", **kw)
        return ev["task_id"]

    def raw(self):
        conn = sqlite3.connect(self.path, isolation_level=None)
        return conn


class AcceptanceTests(Base):
    def test_a_idempotent_event_log(self):
        tid = self.new_task(idem_key="create:viec10:design")
        again = self.store.create(title="Write DESIGN.md",
                                  goal="ztask design for Viec 10",
                                  owner=LEADER,
                                  acceptance="Leader reviews DESIGN.md",
                                  idem_key="create:viec10:design")
        self.assertTrue(again.get("replayed"))
        self.assertEqual(again["task_id"], tid)

        first = self.store.claim(tid, DEV, idem_key="claim:T1:dev:1")
        retry = self.store.claim(tid, DEV, idem_key="claim:T1:dev:1")
        self.assertEqual(first["seq"], retry["seq"])
        self.assertTrue(retry.get("replayed"))

        # Exactly one created + one claimed event, one task.
        types = [e["type"] for e in self.store.events()]
        self.assertEqual(types, ["task.created", "task.claimed"])
        self.assertEqual(len(self.store.tasks()), 1)

        # Same key, different request: refused, nothing appended.
        with self.assertRaises(IdempotencyConflict):
            self.store.claim(tid, DEV2, idem_key="claim:T1:dev:1")
        self.assertEqual(len(self.store.events()), 2)
        self.store.verify()

    def test_b_lease_12h_expiry_returns_to_queue(self):
        tid = self.new_task()
        ev = self.store.claim(tid, DEV)
        token = ev["data"]["token"]
        self.assertEqual(ev["data"]["lease_until"], "2026-10-28T21:00:00Z")

        self.clock.advance(hours=11, minutes=59)
        self.store.sweep()
        self.assertEqual(self.store.task(tid)["state"], "working")
        self.store.heartbeat(tid, DEV, token)  # extends to now + 12h
        self.assertEqual(self.store.task(tid)["lease_until"],
                         "2026-10-29T08:59:00Z")

        self.clock.advance(hours=12, minutes=1)  # past the extended lease
        # Another agent claims: the sweep inside the write expires first.
        ev2 = self.store.claim(tid, DEV2)
        types = [e["type"] for e in self.store.events()]
        self.assertIn("task.lease_expired", types)
        self.assertLess(types.index("task.lease_expired"),
                        types.index("task.claimed", 2))
        task = self.store.task(tid)
        self.assertEqual(task["assignee"], DEV2)
        self.assertEqual(ev2["data"]["token"], token + 1)

        # The old holder comes back with its stale fencing token.
        with self.assertRaises(NotAllowed):
            self.store.heartbeat(tid, DEV, token)
        with self.assertRaises(NotAllowed):
            self.store.submit(tid, DEV, token, evidence="late")
        self.store.verify()

    def test_c_verify_detects_hand_edit_TamperEvident(self):
        tid = self.new_task()
        self.store.claim(tid, DEV)
        self.store.create(title="Child", goal="g", owner=LEADER,
                          acceptance="a", goal_ref=tid)
        self.assertTrue(self.store.verify()["ok"])

        # Hand-edit event 2 into different, still valid JSON.
        conn = self.raw()
        body = conn.execute("SELECT body FROM events WHERE seq=2").fetchone()[0]
        edited = json.loads(body)
        edited["data"]["assignee"] = "Mallory"
        conn.execute("UPDATE events SET body=? WHERE seq=2",
                     (ztask.canonical(edited),))
        conn.close()
        with self.assertRaises(TamperEvident) as cm:
            self.store.verify()
        self.assertEqual(cm.exception.seq, 2)
        self.assertIn("hash mismatch", cm.exception.reason)

        # CLI reports it with exit code 2.
        err = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(err):
            rc = ztask.main(["--db", self.path, "verify"])
        self.assertEqual(rc, 2)
        self.assertIn("tamper_evident", err.getvalue())

        # Restore, then remove a middle row: seq gap.
        conn = self.raw()
        conn.execute("UPDATE events SET body=? WHERE seq=2", (body,))
        self.store.verify()
        conn.execute("DELETE FROM events WHERE seq=2")
        conn.close()
        with self.assertRaises(TamperEvident) as cm:
            self.store.verify()
        self.assertEqual(cm.exception.seq, 2)

    def test_c2_tail_truncation_detected(self):
        self.new_task()
        self.new_task(title="Second")
        conn = self.raw()
        conn.execute("DELETE FROM events WHERE seq=2")
        conn.close()
        with self.assertRaises(TamperEvident) as cm:
            self.store.verify()
        self.assertIn("chain head mismatch", cm.exception.reason)


class StateMachineTests(Base):
    def test_happy_path_and_done_only_by_leader(self):
        tid = self.new_task()
        tok = self.store.claim(tid, DEV)["data"]["token"]
        self.store.block(tid, DEV, tok, blocked_by="waiting on Z Ops")
        self.assertEqual(self.store.task(tid)["state"], "blocked")
        tok = self.store.unblock(tid, DEV)["data"]["token"]
        with self.assertRaises(ZtaskError):
            self.store.submit(tid, DEV, tok, evidence="  ")
        self.store.submit(tid, DEV, tok, evidence="test output + diff stat")
        self.assertEqual(self.store.task(tid)["state"], "review")
        with self.assertRaises(NotAllowed):
            self.store.approve(tid, DEV)
        tok = self.store.reject(tid, LEADER, reason="add tests")["data"]["token"]
        self.assertEqual(self.store.task(tid)["state"], "working")
        self.store.submit(tid, DEV, tok, evidence="tests added")
        self.store.approve(tid, LEADER)
        self.assertEqual(self.store.task(tid)["state"], "done")
        with self.assertRaises(InvalidTransition):
            self.store.claim(tid, DEV2)
        self.store.verify()

    def test_illegal_transitions(self):
        tid = self.new_task()
        with self.assertRaises(InvalidTransition):
            self.store.approve(tid, LEADER)          # todo -> done
        with self.assertRaises(InvalidTransition):
            self.store.submit(tid, DEV, 1, evidence="x")  # todo -> review
        with self.assertRaises(InvalidTransition):
            self.store.unblock(tid, LEADER)          # todo -> working
        self.store.claim(tid, DEV)
        with self.assertRaises(InvalidTransition):
            self.store.claim(tid, DEV2)              # double claim

    def test_task_record_requires_four_elements(self):
        with self.assertRaises(ZtaskError):
            self.store.create(title="x", goal="", owner=LEADER,
                              acceptance="a")
        with self.assertRaises(ZtaskError):
            self.store.create(title="x", goal="g", owner=LEADER,
                              acceptance=" ")

    def test_goal_ref_child_done_before_parent(self):
        parent = self.new_task(title="Parent")
        child = self.new_task(title="Child", goal_ref=parent)
        with self.assertRaises(ztask.NotFound):
            self.new_task(title="Orphan", goal_ref="T99")
        ptok = self.store.claim(parent, DEV)["data"]["token"]
        self.store.submit(parent, DEV, ptok, evidence="e")
        with self.assertRaises(InvalidTransition):
            self.store.approve(parent, LEADER)
        ctok = self.store.claim(child, DEV2)["data"]["token"]
        self.store.submit(child, DEV2, ctok, evidence="e")
        self.store.approve(child, LEADER)
        self.store.approve(parent, LEADER)
        self.assertEqual(self.store.task(parent)["state"], "done")

    def test_concurrent_claim_has_one_winner(self):
        tid = self.new_task()
        results = []

        def worker(name):
            s = Store(self.path, now=self.clock)
            try:
                s.claim(tid, name)
                results.append(("ok", name))
            except InvalidTransition:
                results.append(("lost", name))
            finally:
                s.close()

        threads = [threading.Thread(target=worker, args=(f"agent-{i}",))
                   for i in range(8)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(sum(1 for r in results if r[0] == "ok"), 1)
        self.assertEqual(len(results), 8)
        self.store.verify()


class SafetyAndOpsTests(Base):
    def test_secret_precheck_blocks_write(self):
        tid = self.new_task()
        tok = self.store.claim(tid, DEV)["data"]["token"]
        n = len(self.store.events())
        with self.assertRaises(SecretDetected) as cm:
            self.store.submit(tid, DEV, tok,
                              evidence="token ghp_abcdefghijklmnop1234")
        self.assertNotIn("abcdefghijklmnop", str(cm.exception))
        self.assertEqual(len(self.store.events()), n)
        # "task-1" must not trip the scanner.
        self.store.submit(tid, DEV, tok, evidence="see task-1 and sk-learn")

    def test_aho_corasick(self):
        ac = AhoCorasick(["he", "she", "his", "hers"])
        self.assertEqual(sorted(ac.find("ushers")), ["he", "hers", "she"])

    def test_instant_notification_and_outbox(self):
        seen = []
        self.store.listeners.append(lambda ev, who: seen.append((ev["type"],
                                                                 who)))
        tid = self.new_task()
        tok = self.store.claim(tid, DEV)["data"]["token"]
        self.store.submit(tid, DEV, tok, evidence="e")
        self.assertEqual([s[0] for s in seen],
                         ["task.created", "task.claimed", "task.submitted"])
        self.assertIn(LEADER, seen[-1][1])
        pending = self.store.outbox()
        self.assertTrue(pending)
        self.store.ack(pending[0]["id"])
        self.assertEqual(len(self.store.outbox()), len(pending) - 1)

    def test_deadline_and_aging_escalation(self):
        tid = self.new_task(deadline="2026-10-28T12:00:00Z")
        self.clock.advance(hours=4)
        self.assertEqual(self.store.sweep(), 1)          # deadline
        self.clock.advance(hours=21)
        self.assertEqual(self.store.sweep(), 1)          # aging (todo > 24h)
        self.assertEqual(self.store.sweep(), 0)          # once per reason
        self.assertEqual(self.store.task(tid)["escalation"], "deadline,aging")

    def test_rebuild_matches_projection_and_detects_drift(self):
        tid = self.new_task()
        self.store.claim(tid, DEV)
        before = self.store.tasks()
        conn = self.raw()
        conn.execute("UPDATE tasks SET state='done' WHERE id=?", (tid,))
        conn.close()
        with self.assertRaises(ZtaskError):
            self.store.verify()
        self.assertEqual(self.store.rebuild(), 1)
        self.assertEqual(self.store.tasks(), before)
        self.store.verify()

    def test_backup_vacuum_into_verifies(self):
        self.new_task()
        dest = os.path.join(self.dir.name, "backup.db")
        self.store.backup(dest)
        copy = Store(dest)
        try:
            self.assertEqual(copy.verify()["events"], 1)
        finally:
            copy.close()
        with self.assertRaises(ZtaskError):
            self.store.backup(dest)

    def test_cli_roundtrip(self):
        out = io.StringIO()
        with redirect_stdout(out):
            rc = ztask.main(["--db", self.path, "create", "--title", "CLI",
                             "--goal", "g", "--owner", LEADER,
                             "--acceptance", "a"])
        self.assertEqual(rc, 0)
        self.assertEqual(json.loads(out.getvalue())["type"], "task.created")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(ztask.main(["--db", self.path, "verify"]), 0)


if __name__ == "__main__":
    unittest.main()
