#!/usr/bin/env python3
"""deploy/drill/oracle.py held to histories made by hand, each check found
failing where it must: `python3 deploy/drill/test_oracle.py`."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from oracle import (Op, check_appends, check_epochs, check_registers,  # noqa: E402
                    differences, digest)


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
