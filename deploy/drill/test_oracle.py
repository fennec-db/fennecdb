#!/usr/bin/env python3
"""deploy/drill/oracle.py held to histories made by hand, each check found
failing where it must: `python3 deploy/drill/test_oracle.py`."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from oracle import (Op, as_unknown, check_appends, check_epochs, check_kept,  # noqa: E402
                    check_registers, differences, digest, documented)


def w(value, s, e, outcome="ok", key=1, tenant="t", seq=None, history=None):
    return Op("w", tenant, key, value, s, e, outcome, seq, history)


def r(value, s, e, key=1, tenant="t"):
    return Op("r", tenant, key, value, s, e, "ok")


def a(value, outcome="ok", tenant="t", s=0.0, e=0.1, seq=None, history=None):
    return Op("a", tenant, value, value, s, e, outcome, seq, history)


class Registers(unittest.TestCase):
    def test_reads_in_turn_pass(self):
        h = [r(None, 0, 1), w(10, 2, 3), r(10, 4, 5), w(11, 6, 7), r(11, 8, 9)]
        self.assertEqual(check_registers(h), [])

    def test_a_read_finding_the_value_before_the_last_write_answered_fails(self):
        h = [w(10, 0, 1), w(11, 2, 3), r(10, 4, 5)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_a_read_finding_nothing_after_a_write_answered_fails(self):
        h = [w(10, 0, 1), r(None, 2, 3)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_a_read_overlapping_the_first_write_may_find_it(self):
        # No write had ended before the read: it passes, and says nothing.
        h = [w(10, 0, 3), r(10, 1, 2), w(11, 4, 5, outcome="unknown"), r(11, 6, 7, key=1)]
        self.assertEqual(check_registers(h[:2]), [])
        h = [w(11, 4, 5, outcome="unknown"), r(11, 6, 7)]
        self.assertEqual(check_registers(h), [])

    def test_a_read_finding_a_write_sent_after_it_fails(self):
        h = [w(10, 0, 1), w(11, 5, 6), r(11, 2, 3)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_a_read_overlapping_writes_may_find_either(self):
        h = [w(10, 0, 1), w(11, 2, 6), r(10, 3, 4), r(11, 3, 4), w(12, 5, 8), r(12, 6, 7)]
        self.assertEqual(check_registers(h), [])

    def test_a_write_whose_outcome_is_unknown_may_land_at_any_later_moment(self):
        # Sent at 2, never answered: any read after it may find it, or the
        # value before it -- a read is held to the writes, never to the
        # reads before it, as YCSB's checker holds it.
        h = [w(10, 0, 1), w(11, 2, 3, outcome="unknown"), r(10, 9, 9.5), r(11, 10, 10.5), r(10, 11, 12)]
        self.assertEqual(check_registers(h), [])
        # It never ends before a read, so it hides no write answered after
        # it: one answered later, then an older value found, fails.
        h = [w(10, 0, 1), w(11, 2, 3, outcome="unknown"), w(12, 4, 5), r(10, 6, 7)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_a_write_refused_is_never_found(self):
        h = [w(10, 0, 1), w(11, 2, 3, outcome="fail"), r(11, 4, 5)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_a_value_no_write_wrote_fails(self):
        h = [w(10, 0, 1), r(99, 2, 3)]
        self.assertEqual(len(check_registers(h)), 1)
        # Nor one written to another register.
        h = [w(10, 0, 1, key=2), r(10, 2, 3, key=1)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_an_answered_write_lost_shows_in_the_final_read(self):
        # A failover took the write answered at 3: the read at the end
        # finds the one before it.
        h = [w(10, 0, 1), w(11, 2, 3), r(10, 100, 101)]
        self.assertEqual(len(check_registers(h)), 1)

    def test_tenants_and_registers_are_apart(self):
        h = [w(10, 0, 1, tenant="a"), w(11, 0, 1, tenant="b"), r(10, 2, 3, tenant="a"), r(11, 2, 3, tenant="b")]
        self.assertEqual(check_registers(h), [])


class Appends(unittest.TestCase):
    def test_lost_and_ran(self):
        h = [a(1), a(2), a(3, "unknown"), a(4, "fail"), a(5, "fail")]
        lost, ran = check_appends(h, {"t": {1, 3, 5}})
        self.assertEqual([op.value for op in lost], [2])
        self.assertEqual([op.value for op in ran], [5])


class Ledger(unittest.TestCase):
    """Each register write lands its value in the ledger `wl` in its own
    block: a loss the reads cannot see is found there."""

    def test_a_write_lost_and_written_over_before_any_read_is_found(self):
        h = [w(10, 0, 1), w(11, 2, 3), w(12, 4, 5), r(12, 6, 7)]
        self.assertEqual(check_registers(h), [], "no read can see it")
        lost, ran = check_kept(h, {"t": {10, 12}}, "w")
        self.assertEqual([op.value for op in lost], [11])
        self.assertEqual(ran, [])

    def test_an_unknown_write_does_not_mask_a_loss(self):
        # w10 answered, w11 unknown, w12 answered, a read finding 11: the
        # registers pass -- 11 may have landed after 12 -- and the ledger
        # says 12 never landed.
        h = [w(10, 0, 1), w(11, 2, 3, outcome="unknown"), w(12, 4, 5), r(11, 6, 7)]
        self.assertEqual(check_registers(h), [])
        lost, _ = check_kept(h, {"t": {10, 11}}, "w")
        self.assertEqual([op.value for op in lost], [12])
        # Found in the ledger, 11 landed after 12, and nothing is lost.
        lost, _ = check_kept(h, {"t": {10, 11, 12}}, "w")
        self.assertEqual(lost, [])

    def test_a_write_refused_and_in_the_ledger_ran(self):
        h = [w(10, 0, 1), w(11, 2, 3, outcome="fail")]
        _, ran = check_kept(h, {"t": {10, 11}}, "w")
        self.assertEqual([op.value for op in ran], [11])

    def test_appends_and_writes_are_kept_apart(self):
        h = [w(10, 0, 1), a(10, s=2, e=3)]
        self.assertEqual(check_kept(h, {"t": {10}}, "w"), ([], []))
        self.assertEqual(check_kept(h, {"t": set()}, "a")[0][0].kind, "a")


def window(n, fault, s, e, placed, behind_on=(), links=(), start=None):
    return {"n": n, "fault": fault, "from": s, "start": s if start is None else start, "to": e,
            "placed": placed, "behind_on": list(behind_on), "links": [list(x) for x in links]}


class Window(unittest.TestCase):
    """A write answered and lost is the design's only where a failover took
    it while its replica was behind, as the drill made it."""

    PLACED = {"t": ["n1", "n2", [1, 0xa]]}
    FINAL = {"t": [2, 0xb]}

    def test_a_loss_while_the_fault_held_the_replica_behind_is_the_designs(self):
        lost = w(5, 3, 4, seq=9, history=(1, 0xa))
        for spec in [dict(behind_on=["n2"]), dict(links=[["n2", "n1"]])]:
            ok, why = documented(lost, [window(1, "replica", 0, 10, self.PLACED, **spec)], self.FINAL)
            self.assertTrue(ok, why)

    def test_a_loss_the_fault_made_no_room_for_fails(self):
        lost = w(5, 3, 4, seq=9, history=(1, 0xa))
        # A partition between the router and the primary: its replica kept
        # up, so a failover keeps every write answered.
        for spec in [dict(), dict(behind_on=["n1"]), dict(links=[["n1", "n3"]])]:
            ok, why = documented(lost, [window(1, "router", 0, 10, self.PLACED, **spec)], self.FINAL)
            self.assertFalse(ok, why)

    def test_a_loss_in_the_term_a_tenant_ends_in_fails(self):
        # No failover ended it: nothing the design names loses it.
        lost = w(5, 3, 4, seq=9, history=(2, 0xb))
        ok, _ = documented(lost, [window(1, "kill", 0, 10, self.PLACED, behind_on=["n2"])], self.FINAL)
        self.assertFalse(ok)

    def test_a_loss_in_a_term_the_fault_began_is_the_designs(self):
        # Promoted during the fault, its new copy starts behind.
        lost = w(5, 3, 4, seq=9, history=(2, 0xc))
        ok, why = documented(lost, [window(1, "kill", 0, 10, self.PLACED)], {"t": [3, 0xd]})
        self.assertTrue(ok, why)

    def test_a_loss_in_an_older_term_or_another_history_fails(self):
        for h in [(1, 0xe), (0, 0x1)]:
            lost = w(5, 3, 4, seq=9, history=h)
            ok, why = documented(lost, [window(1, "kill", 0, 10, self.PLACED, behind_on=["n2"])],
                                 self.FINAL)
            self.assertFalse(ok, f"{h}: {why}")

    def test_a_write_with_no_history_is_of_the_roots_term(self):
        # No Fennec-History: the root's, before any promotion forked it.
        root = {"t": ["n1", "n2", [0, 0]]}
        ok, why = documented(w(5, 3, 4), [window(1, "pause", 0, 10, root, behind_on=["n2"])], self.FINAL)
        self.assertTrue(ok, why)
        ok, _ = documented(w(5, 3, 4), [window(1, "pause", 0, 10, root)], self.FINAL)
        self.assertFalse(ok)
        # Lost in the root's term, the tenant never failed over.
        ok, _ = documented(w(5, 3, 4), [window(1, "pause", 0, 10, root, behind_on=["n2"])], {"t": [0, 0]})
        self.assertFalse(ok)

    def test_a_loss_outside_every_span_fails(self):
        lost = w(5, 30, 40, seq=9, history=(1, 0xa))
        self.assertFalse(documented(lost, [window(1, "kill", 0, 10, self.PLACED, behind_on=["n2"])],
                                    self.FINAL)[0])

    def test_a_write_answered_before_the_fault_began_had_its_replica(self):
        # The span runs from the settle before; the fault began at 5.
        ws = [window(1, "replica", 0, 10, self.PLACED, links=[["n1", "n2"]], start=5)]
        self.assertFalse(documented(w(5, 3, 4, history=(1, 0xa)), ws, self.FINAL)[0])
        self.assertTrue(documented(w(5, 5, 6, history=(1, 0xa)), ws, self.FINAL)[0])

    def test_the_span_is_the_one_the_write_was_answered_in(self):
        # Held behind by the first fault, answered in the second's span,
        # which held nothing behind.
        ws = [window(1, "replica", 0, 10, self.PLACED, links=[["n1", "n2"]]),
              window(2, "clock", 10, 20, self.PLACED)]
        self.assertTrue(documented(w(5, 3, 9.5, history=(1, 0xa)), ws, self.FINAL)[0])
        self.assertFalse(documented(w(5, 9, 10.5, history=(1, 0xa)), ws, self.FINAL)[0])

    def test_a_write_lost_inside_the_window_reads_as_unknown(self):
        # 11 answered and lost as the documented window allows: a read on
        # its primary found it, and the final read the value before it.
        h = [w(10, 0, 1), w(11, 2, 3), r(11, 4, 5), r(10, 100, 101)]
        self.assertEqual(len(check_registers(h)), 1)
        self.assertEqual(check_registers(as_unknown(h, {11})), [])


class Epochs(unittest.TestCase):
    def test_terms_in_turn_pass(self):
        h = [w(1, 0, 1, seq=1, history=(1, 0xa)), a(2, s=2, e=3, seq=2, history=(1, 0xa)),
             w(3, 4, 5, seq=3, history=(2, 0xb)), w(4, 6, 7, seq=4, history=(2, 0xb))]
        self.assertEqual(check_epochs(h), [])

    def test_one_change_given_two_writes_fails(self):
        h = [w(1, 0, 1, seq=7, history=(1, 0xa)), w(2, 0, 1, key=2, seq=7, history=(1, 0xa))]
        self.assertEqual(len(check_epochs(h)), 1)

    def test_an_older_term_answering_after_a_newer_one_fails(self):
        # Term 2 answered at 3; a write sent at 4 answered in term 1: the
        # primary failed over was still writing.
        h = [w(1, 0, 1, seq=1, history=(1, 0xa)), w(2, 2, 3, seq=2, history=(2, 0xb)),
             w(3, 4, 5, seq=2, history=(1, 0xa))]
        self.assertEqual(len(check_epochs(h)), 1)
        # Overlapping, either order may be answered.
        h = [w(2, 2, 6, seq=2, history=(2, 0xb)), w(3, 4, 5, seq=2, history=(1, 0xa))]
        self.assertEqual(check_epochs(h), [])

    def test_a_history_naming_two_terms_fails(self):
        h = [w(1, 0, 1, seq=1, history=(1, 0xa)), w(2, 2, 3, seq=2, history=(2, 0xa))]
        self.assertEqual(len(check_epochs(h)), 1)

    def test_two_histories_in_one_term_fail(self):
        # Two copies promoted from term 1, each forked to term 2 under an id
        # of its own, both answering: every other check passes it -- no
        # change given two writes within a history, no older term after a
        # newer -- and one primary a term is what fails.
        h = [w(1, 0, 1, seq=1, history=(1, 0xa)),
             w(2, 2, 3, seq=2, history=(2, 0xb)), a(3, s=2.5, e=3.5, seq=2, history=(2, 0xc)),
             w(4, 4, 5, seq=3, history=(2, 0xb)), a(5, s=4.5, e=5.5, seq=3, history=(2, 0xc))]
        found = check_epochs(h)
        self.assertEqual(len(found), 1, found)
        self.assertIn("term 2 is two histories", found[0])


class Digests(unittest.TestCase):
    def test_the_same_rows_in_any_order_are_one_digest(self):
        x = {"regs": [{"id": 1, "v": 2}, {"id": 2, "v": 3}], "log": [{"id": 5, "k": 0}]}
        y = {"log": [{"k": 0, "id": 5}], "regs": [{"v": 3, "id": 2}, {"id": 1, "v": 2}]}
        self.assertEqual(digest(x), digest(y))
        self.assertEqual(differences(x, y), [])
        z = {"regs": [{"id": 1, "v": 9}], "log": [{"id": 5, "k": 0}, {"id": 6, "k": 0}]}
        self.assertNotEqual(digest(x), digest(z))
        self.assertEqual(len(differences(x, z)), 2)


if __name__ == "__main__":
    unittest.main()
