#!/usr/bin/env python3
"""The power cut's check held to what it must find (client.py): a cut's
events written as `load` notes them, read back by a `Check` over a server
that answers with the rows the cut left, and the verdict asked.

    python3 .github/power-cut/test_client.py

`--sync 250`'s window: a syncer that keeps its interval passes, and so does
one whose disk is slow for two syncs, as run 43462058075's was; one that
stopped 3 s before the cut, one that syncs every second, one every half
second and one stuck for a second and a half in the middle each fail --
the first two the cases PR #627's review found the window's bound, read
from the pages being judged, letting pass (bounds of 3 058 and 1 708 ms;
3 050 and 1 707 here). A cut with no write in flight fails as the rig's,
and so does a standby far behind the node; one at its tip counts what a
promotion at the cut would have lost. The standard library alone, as
client.py: ci.yml's fmt job runs it.
"""

import argparse
import contextlib
import io
import json
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import client  # noqa: E402

# A host's clock far from zero, as time.time() reads.
T0 = 1_800_000_000.0
CUT = 1


class Rows:
    """The server after the cut: the rows of each collection it kept."""

    def __init__(self, docs):
        self.docs = docs

    def rows(self, q, params=()):
        if q == "get docs":
            return list(self.docs)
        return []


def load(interval, own, until, stop=None, stall=None, slow=None, every=0.004):
    """A cut's events and what the cut kept, for a syncer that sleeps
    `interval` after each sync, takes every write landed by then and hands
    its page over `own` later -- taking none from `stop` seconds into the
    load, none from `stall[0]` to `stall[1]`, and the first sync taking its
    writes from each of `slow`'s seconds in its own time. A single put is answered
    every `every` s from the load's start to the cut, `until` s in, each the
    next change; one more is sent as QEMU is killed, and never answered.
    The cut keeps every write a sync took whose page came before it."""
    cut_at = T0 + until
    killed = cut_at + 0.002
    ev = [{"ev": "start", "cut": CUT, "t": T0}]
    acks = []
    n = 0
    t = T0 + every
    while t < cut_at:
        n += 1
        i = CUT * client.BASE + n
        ev.append({"ev": "send", "op": "s", "q": n, "id": i, "t": round(t, 4)})
        ev.append({"ev": "ack", "op": "s", "q": n, "status": 200, "seq": n, "t": round(t + 0.0005, 4)})
        acks.append((t + 0.0005, n, i))
        t += every
    n += 1
    ev.append({"ev": "send", "op": "s", "q": n, "id": CUT * client.BASE + n, "t": round(cut_at + 0.001, 4)})
    kept = []
    taken = 0
    at = prev = T0
    take = T0 + 0.1
    slow = dict(slow or {})
    while True:
        if stall and stall[0] <= take - T0 < stall[1]:
            take = T0 + stall[1]
        due = [k for k in slow if k <= take - T0]
        took = slow.pop(min(due)) if due else own
        if (stop is not None and take - T0 >= stop) or take + took > cut_at:
            break
        at = take + took
        new = [(s, i) for a, s, i in acks if a < take and s > taken]
        if new:
            taken = new[-1][0]
            kept += new
            ev.append(
                {
                    "ev": "durable",
                    "sent": round(prev + 0.002, 4),
                    "at": round(at, 4),
                    "next": taken,
                    "seq": taken,
                    "w": [[s, "docs", i] for s, i in new],
                    "t": round(at, 4),
                }
            )
            prev = at
        take = at + interval
    rows = [{"id": i, "w": "s", "c": CUT, "p": client.text("s", i, n=client.SINGLE_LEN)} for _, i in kept]
    return ev, rows, cut_at, killed


class Cut:
    """A `Check` of one cut, its files in a directory of its own."""

    def __init__(self, test, ev, cut_at, killed, sync_ms=250):
        d = tempfile.mkdtemp(prefix="power-cut-test-")
        test.addCleanup(shutil.rmtree, d)
        with open(os.path.join(d, "token"), "w") as f:
            f.write("t")
        with open(os.path.join(d, "state.json"), "w") as f:
            json.dump({"docs": {}, "churn": {}, "spill": None, "idem": []}, f)
        with open(os.path.join(d, f"events-{CUT}.jsonl"), "w") as f:
            for e in ev:
                f.write(json.dumps(e) + "\n")
        args = argparse.Namespace(
            url="http://127.0.0.1:9",
            token_file=os.path.join(d, "token"),
            state=d,
            cut=CUT,
            seed="0",
            label="plain",
            cut_time=f"{cut_at} {killed}",
            commit=False,
            prefix="",
            name="",
            ops="",
            sync_ms=sync_ms,
            replica_url="",
        )
        self.check = client.Check(args)
        self.out = io.StringIO()

    def __call__(self, *steps):
        """Runs the check's steps, its printing kept; what it found."""
        with contextlib.redirect_stdout(self.out):
            for step in steps:
                step()
        return self.check.bad

    def window(self, rows):
        self.check.s = Rows(rows)
        return self(self.check.docs, self.check.window)


def has(bad, what):
    return any(what in b for b in bad)


class Window(unittest.TestCase):
    def test_a_syncer_keeping_its_interval_passes(self):
        # 250 ms of sleep and 8 ms of syncing, the cut 13 s in: what it
        # took was answered within the last sync's window.
        ev, rows, cut_at, killed = load(0.25, 0.008, 13.0)
        c = Cut(self, ev, cut_at, killed)
        self.assertEqual(c.window(rows), [], c.out.getvalue())
        lost = sorted(c.check.lost)
        self.assertTrue(lost, "the cut took no acknowledged write")
        self.assertLessEqual(cut_at - lost[0][0], 0.25 + 0.008 * 2)
        self.assertIn("LOST a prefix kept", c.out.getvalue())

    def test_a_syncer_stopped_3_s_before_the_cut_fails(self):
        # The review's first case: the writes gone from 3.0 s before the
        # cut, which the bound read from the pages, 250 + 8 (the last
        # sync) + 2 742 (the one "under way") + 50 = 3 050 ms, let pass.
        ev, rows, cut_at, killed = load(0.25, 0.008, 13.0, stop=10.0)
        last = max(e["at"] for e in ev if e["ev"] == "durable")
        # The cut 3.0 s after the last sync took its writes.
        ev, rows, cut_at, killed = load(0.25, 0.008, last - 0.008 - T0 + 3.0, stop=10.0)
        c = Cut(self, ev, cut_at, killed)
        bad = c.window(rows)
        lost = sorted(c.check.lost)
        self.assertAlmostEqual(cut_at - lost[0][0], 3.0, delta=0.005)
        self.assertTrue(has(bad, "past the window's 650 ms"), bad)
        self.assertTrue(has(bad, "a late sync") and has(bad, "from the last page to the cut"), bad)

    def test_a_syncer_every_second_fails(self):
        # The review's second case: a second's sleep and 8 ms of syncing,
        # the writes gone from 907 ms before the cut, which the bound read
        # from the pages, 1 707 ms, let pass.
        ev, rows, cut_at, killed = load(1.0, 0.008, 10.0)
        last = max(e["at"] for e in ev if e["ev"] == "durable")
        # The cut 907 ms after the last sync took its writes.
        ev, rows, cut_at, killed = load(1.0, 0.008, last - 0.008 - T0 + 0.907)
        c = Cut(self, ev, cut_at, killed)
        bad = c.window(rows)
        lost = sorted(c.check.lost)
        self.assertAlmostEqual(cut_at - lost[0][0], 0.907, delta=0.005)
        self.assertTrue(has(bad, "past the window's 650 ms"), bad)
        self.assertTrue(has(bad, "a late sync"), bad)
        self.assertTrue(has(bad, "a slowed syncer"), bad)

    def test_a_syncer_every_half_second_fails(self):
        # Slowed under the ceiling, and its losses -- half a second at most
        # -- under the bound: the median of its pages fails it at any cut.
        ev, rows, cut_at, killed = load(0.5, 0.008, 9.0)
        c = Cut(self, ev, cut_at, killed)
        bad = c.window(rows)
        self.assertFalse(has(bad, "past the window") or has(bad, "a late sync"), bad)
        self.assertTrue(has(bad, "a slowed syncer: its pages came every 508 ms at the median"), bad)

    def test_a_few_slow_syncs_pass(self):
        # The runner's disk, as run 43462058075 met it: syncs of 105 and
        # 177 ms back to back, then on time again; the cut just before the
        # second's page, so that its first loss was answered as long before
        # it as a write then took to be handed over, 532 ms.
        slow = {3.0: 0.105, 3.3: 0.177}
        ev, *_ = load(0.25, 0.008, 6.0, slow=slow)
        pages = [e["at"] for e in ev if e["ev"] == "durable"]
        slowest = next(y for x, y in zip(pages, pages[1:]) if y - x > 0.4)
        ev, rows, cut_at, killed = load(0.25, 0.008, slowest - T0 - 0.001, slow=slow)
        c = Cut(self, ev, cut_at, killed)
        self.assertEqual(c.window(rows), [], c.out.getvalue())
        lost = sorted(c.check.lost)
        self.assertAlmostEqual(cut_at - lost[0][0], 0.531, delta=0.005)

    def test_a_syncer_stuck_in_the_middle_fails(self):
        # A sync 1.5 s late 4 s in, then on time again: no write lost past
        # the bound at the cut, and the stall named all the same.
        ev, rows, cut_at, killed = load(0.25, 0.008, 9.0, stall=(4.0, 5.5))
        c = Cut(self, ev, cut_at, killed)
        bad = c.window(rows)
        self.assertFalse(has(bad, "past the window"), bad)
        self.assertTrue(has(bad, "a late sync: 1 gap(s)") and has(bad, "between pages"), bad)

    def test_a_write_a_page_held_gone_fails(self):
        ev, rows, cut_at, killed = load(0.25, 0.008, 6.0)
        c = Cut(self, ev, cut_at, killed)
        bad = c.window(rows[:-1])
        self.assertTrue(has(bad, "handed over by /_changes as on disk and is gone"), bad)


class MidWrite(unittest.TestCase):
    def test_a_cut_with_a_write_in_flight_passes(self):
        ev, _, cut_at, killed = load(0.25, 0.008, 3.0)
        c = Cut(self, ev, cut_at, killed)
        self.assertEqual(c(c.check.describe_load), [])
        self.assertIn("INFLIGHT single puts", c.out.getvalue())

    def test_a_cut_between_writes_fails(self):
        # Every write answered before the kill: the cut landed between
        # them, and held nothing to a write cut short.
        ev, _, cut_at, killed = load(0.25, 0.008, 3.0)
        ev = [e for e in ev if not (e["ev"] == "send" and e["t"] > cut_at)]
        c = Cut(self, ev, cut_at, killed)
        bad = c(c.check.describe_load)
        self.assertTrue(has(bad, "landed between writes"), bad)
        self.assertIn("RIG cut 1 landed between writes", c.out.getvalue())

    def test_a_write_tried_after_the_kill_is_not_in_flight(self):
        ev, _, cut_at, killed = load(0.25, 0.008, 3.0)
        for e in ev:
            if e["ev"] == "send" and e["t"] > cut_at:
                e["t"] = round(killed + 0.3, 4)
        c = Cut(self, ev, cut_at, killed)
        bad = c(c.check.describe_load)
        self.assertTrue(has(bad, "landed between writes"), bad)
        self.assertIn("AFTER single puts", c.out.getvalue())


class Standby(unittest.TestCase):
    def cut(self):
        ev, _, cut_at, killed = load(0.25, 0.008, 5.0, every=0.01)
        return Cut(self, ev, cut_at, killed, sync_ms=0)

    def test_a_standby_at_the_tip_counts_what_a_promotion_would_lose(self):
        c = self.cut()
        tip = max(e["seq"] for e in c.check.acked.values())
        bad = c(lambda: c.check.standby_near(tip - 3, tip + 1))
        self.assertEqual(bad, [])
        out = c.out.getvalue()
        self.assertIn("3 behind the last acknowledged", out)
        self.assertIn("4 behind the node as it came back", out)
        self.assertIn("3 acknowledged writes above it", out)

    def test_a_standby_holding_every_acknowledged_write_would_lose_none(self):
        c = self.cut()
        tip = max(e["seq"] for e in c.check.acked.values())
        self.assertEqual(c(lambda: c.check.standby_near(tip, tip)), [])
        self.assertIn("a promotion at the cut would have lost none", c.out.getvalue())

    def test_writes_answered_with_no_change_hold_the_standby_to_nothing(self):
        c = self.cut()
        for e in c.check.acked.values():
            e.pop("seq")
        bad = c(lambda: c.check.standby_near(0, 0))
        self.assertTrue(has(bad, "with its change"), bad)

    def test_a_standby_far_behind_fails(self):
        # It lacks every write answered in the last 2 s: what it would
        # have been held to of the node's, it never had.
        c = self.cut()
        held = max(e["seq"] for e in c.check.acked.values() if e["t"] < c.check.cut_at - 2.0)
        bad = c(lambda: c.check.standby_near(held, held + 200))
        self.assertTrue(has(bad, "the standby lacked a write the node acknowledged"), bad)
        self.assertIn("RIG the standby lacked", c.out.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=2)
