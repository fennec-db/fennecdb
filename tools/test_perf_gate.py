#!/usr/bin/env python3
"""tools/perf_gate.py: the gate's judgement on results of its own making --
a synthetic +1% fails, an entry the pull request adds lets it through, one
the base holds already does not, a median holds a seed's step, an
allocation's cost moving with the heap passes where an allocation more
fails, a +1% in heap bytes fails as the figure's does, a workload the base
had and this tree dropped or the base did not run fails, a holdout pair is
as expected only when exactly the workloads it names fail, by the metrics
it names -- one marked `?` failing or not --, a workload its base cannot
run held to nothing only when named with its error's words and run in full
by this tree -- the reading of a profile's calls into the allocator, of a
run's heap tally, of the base's workloads, of PERF_ACCEPTED as data, never
run; which changes are counted, and the gate's own files a change edits;
the I/O counts -- one fsync more a write fails, an entry the pull request
adds lets it through, one the base holds does not, a drop fails as a rise
does, a sync swapped for another call fails by its call, a workload only
this tree has is reported, runs that differ fail -- and their reading off
captured and typed `strace -f` traces (tools/perf_gate_traces/), what the
trace cannot see refused; the workflow's choice of checker held to
`io_checker`, and its two sets of results kept apart; then the committed
list and the holdout's matrix and verdicts.

    python3 tools/test_perf_gate.py
"""

import contextlib
import importlib.util
import inspect
import io
import json
import math
import os
import re
import subprocess
import tempfile
import textwrap
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("perf_gate", os.path.join(HERE, "perf_gate.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

BASE = 2_000_000
REASON = "the keyed write's row now holds its request's digest, measured by make perf-gate"


def results(**deltas):
    """Results of workloads `a` and `b` (and those named), each side five
    runs around BASE, the head's moved by its delta, a fraction."""
    workloads = {}
    for name in sorted({"a", "b", *deltas}):
        d = deltas.get(name, 0.0)
        base = [BASE - 81, BASE, BASE, BASE + 81, BASE + 81]
        head = [round(x * (1 + d)) for x in base]
        workloads[name] = {"ops": 1_000, "base": base, "head": head}
    return {"metric": "instructions", "tool": "callgrind", "runs": 5, "workloads": workloads}


def entry(workload="a", most="+1.5%", reason=REASON):
    return {"workload": workload, "most": most, "reason": reason}


def verdicts(res, entries=()):
    rows, failed = gate.judge(res, list(entries))
    return {r[0]: r[5] for r in rows}, failed


class Gate(unittest.TestCase):
    def test_a_synthetic_one_percent_fails(self):
        v, failed = verdicts(results(a=0.01))
        self.assertTrue(failed)
        self.assertTrue(v["a"].startswith("FAILED"), v)
        self.assertEqual(v["b"], "ok")

    def test_an_entry_the_pull_request_adds_lets_it_through(self):
        v, failed = verdicts(results(a=0.01), [entry()])
        self.assertFalse(failed, v)
        self.assertTrue(v["a"].startswith("accepted"), v)

    def test_an_entry_lets_through_its_workload_alone(self):
        v, failed = verdicts(results(a=0.01, b=0.01), [entry("a")])
        self.assertTrue(failed)
        self.assertTrue(v["a"].startswith("accepted"))
        self.assertTrue(v["b"].startswith("FAILED"))

    def test_an_entry_lets_through_no_more_than_its_most(self):
        v, failed = verdicts(results(a=0.02), [entry(most="+1.5%")])
        self.assertTrue(failed)
        self.assertIn("past its PERF_ACCEPTED entry", v["a"])

    def test_under_the_threshold_passes(self):
        v, failed = verdicts(results(a=0.0099))
        self.assertFalse(failed, v)
        self.assertEqual(v["a"], "ok")

    def test_a_gain_passes(self):
        _, failed = verdicts(results(a=-0.05))
        self.assertFalse(failed)

    def test_the_median_holds_a_seeds_step(self):
        # One run in five a seed's step of 81 instructions an op above the
        # rest -- 4% of the loop here -- and the head no slower: the median
        # takes the commoner count, where a mean would fail it.
        res = results()
        w = res["workloads"]["a"]
        w["base"] = [BASE] * 5
        w["head"] = [BASE] * 4 + [BASE + 81 * w["ops"]]
        _, failed = verdicts(res)
        self.assertFalse(failed)

    def test_too_few_runs_fail(self):
        res = results()
        res["workloads"]["a"]["head"] = res["workloads"]["a"]["head"][:3]
        v, failed = verdicts(res)
        self.assertTrue(failed)
        self.assertIn("runs", v["a"])

    def test_a_new_workload_is_held_to_nothing(self):
        res = results(c=0.5)
        res["workloads"]["c"]["base"] = []
        v, failed = verdicts(res)
        self.assertFalse(failed)
        self.assertTrue(v["c"].startswith("new"))

    def test_an_entry_naming_no_workload_is_refused(self):
        with self.assertRaises(ValueError):
            gate.judge(results(), [entry("nothing")])

    def test_results_of_another_metric_are_refused(self):
        res = results()
        res["metric"] = "fsyncs"
        with self.assertRaises(ValueError):
            gate.judge(res, [])


def allocating(res, name, base, head):
    """Workload `name` of `res` given each side's runs as (total, calls,
    instructions a call): `calls` mallocs and as many frees a run, each at
    that price inside the allocator."""
    w = res["workloads"][name]
    w["allocator"] = {}
    for side, runs in (("base", base), ("head", head)):
        w[side] = sorted(t for t, _, _ in runs)
        w["allocator"][side] = [[t, 2 * c * p, 2 * c] for t, c, p in runs]
    return res


def row(res, name):
    rows, _ = gate.judge(res, [])
    return {r[0]: r for r in rows}[name]


class Allocator(unittest.TestCase):
    """The allocator's instructions counted as its calls at the base's cost
    a call: a row's delta (4) is judged, its plain count's (6) shown beside
    it."""

    BASE_RUNS = [(BASE, 1_000, 150)] * 5

    def test_an_allocations_cost_moving_with_the_heap_passes(self):
        # The same calls, each 30 instructions dearer -- a group of musl's
        # made and freed each op where the base's heap kept one -- +3% on
        # the plain count, and the code's own work the same.
        res = allocating(results(), "a", self.BASE_RUNS, [(BASE + 60_000, 1_000, 180)] * 5)
        _, failed = verdicts(res)
        self.assertFalse(failed)
        r = row(res, "a")
        self.assertAlmostEqual(r[4], 0.0)
        self.assertAlmostEqual(r[6], 0.03)

    def test_an_allocation_more_fails_at_the_bases_price(self):
        # A hundred allocations more a run, each run cheaper by the heap's
        # layout (120 a call against 150): -1.8% as counted, +1.5% at the
        # base's price, which is what the code asks more.
        res = allocating(results(), "a", self.BASE_RUNS, [(BASE - 36_000, 1_100, 120)] * 5)
        v, failed = verdicts(res)
        self.assertTrue(failed)
        self.assertTrue(v["a"].startswith("FAILED"), v)
        r = row(res, "a")
        self.assertAlmostEqual(r[4], 0.015)
        self.assertAlmostEqual(r[6], -0.018)

    def test_the_programs_own_code_counts_whatever_the_allocator_paid(self):
        # The calls the same and as dear, and 50 000 instructions more of
        # the program's own: +2.5%, failed.
        res = allocating(results(), "a", self.BASE_RUNS, [(BASE + 50_000, 1_000, 150)] * 5)
        v, failed = verdicts(res)
        self.assertTrue(failed, v)
        self.assertAlmostEqual(row(res, "a")[4], 0.025)

    def test_a_base_that_called_no_allocator_prices_nothing(self):
        res = allocating(results(), "a", [(BASE, 0, 0)] * 5, [(BASE + 10_000, 0, 0)] * 5)
        self.assertAlmostEqual(row(res, "a")[4], 0.005)

    def test_a_gain_in_the_code_passes_whatever_the_allocator_paid(self):
        res = allocating(results(), "a", self.BASE_RUNS, [(BASE + 20_000, 1_000, 200)] * 5)
        r = row(res, "a")
        self.assertLess(r[4], 0)
        self.assertGreater(r[6], 0)

    def test_counts_not_of_the_runs_beside_them_are_refused(self):
        res = allocating(results(), "a", self.BASE_RUNS, self.BASE_RUNS)
        res["workloads"]["a"]["head"] = [BASE + 1] * 5
        with self.assertRaises(ValueError):
            gate.judge(res, [])
        res = allocating(results(), "a", self.BASE_RUNS, self.BASE_RUNS)
        res["workloads"]["a"]["allocator"]["head"][0] = [BASE, 300_000]
        with self.assertRaises(ValueError):
            gate.judge(res, [])

    def test_results_without_the_allocators_counts_are_judged_as_counted(self):
        r = row(results(a=0.01), "a")
        self.assertAlmostEqual(r[4], r[6])


class Entries(unittest.TestCase):
    def test_the_form_of_an_entry(self):
        gate.checked([entry()])
        for bad in (
            [{"workload": "a", "most": "+1.5%"}],
            [dict(entry(), pr=1)],
            [entry(most="1.5%")],
            [entry(most="+0.5%")],
            [entry(most="+1,5%")],
            [entry(reason="faster later")],
            [entry(workload="")],
            entry(),
        ):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                gate.checked(bad)

    def test_an_entry_the_base_holds_lets_nothing_through(self):
        e = entry()
        self.assertEqual(gate.added([e], [e]), [])
        _, failed = verdicts(results(a=0.01), gate.added([e], [e]))
        self.assertTrue(failed)

    def test_an_entry_edited_counts_as_the_pull_requests(self):
        e = entry()
        self.assertEqual(gate.added([dict(e, most="+3%")], [e]), [dict(e, most="+3%")])


def checker(entries, prelude=""):
    """A checker's text holding `entries` as its PERF_ACCEPTED."""
    return f"{prelude}\nTHRESHOLD = 0.01\nPERF_ACCEPTED = {json.dumps(entries)}\n"


class Reading(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def write(self, name, text):
        path = os.path.join(self.dir.name, name)
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
        return path

    def test_a_pull_requests_entries_are_read_never_run(self):
        ran = os.path.join(self.dir.name, "ran")
        path = self.write("pr.py", checker([entry()], f"open({ran!r}, 'w').close()\n"))
        self.assertEqual(gate.accepted_in(path), [entry()])
        self.assertFalse(os.path.exists(ran))

    def test_an_entry_that_is_no_literal_is_refused(self):
        path = self.write("pr.py", "PERF_ACCEPTED = list()\n")
        with self.assertRaises(ValueError):
            gate.accepted_in(path)
        with self.assertRaises(ValueError):
            gate.accepted_in(self.write("none.py", "X = []\n"))

    def run_check(self, res, pr_entries, base_entries, more=()):
        path = self.write("results.json", json.dumps(res))
        summary = os.path.join(self.dir.name, "summary.md")
        args = ["check", path, "--accepted-from", self.write("pr.py", checker(pr_entries)), "--summary", summary]
        if base_entries is not None:
            args += ["--accepted-base", self.write("base.py", checker(base_entries))]
        args += list(more)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = gate.main(args)
        with open(summary, encoding="utf-8") as f:
            return code, f.read()

    def test_the_check_fails_a_synthetic_one_percent(self):
        code, summary = self.run_check(results(a=0.01), [], [])
        self.assertEqual(code, 1)
        self.assertIn("| a |", summary)
        self.assertIn("+1.00%", summary)

    def test_the_check_passes_it_under_an_entry_the_pull_request_adds(self):
        code, summary = self.run_check(results(a=0.01), [entry()], [])
        self.assertEqual(code, 0, summary)
        self.assertIn("accepted", summary)

    def test_the_check_fails_it_under_an_entry_the_base_holds(self):
        code, _ = self.run_check(results(a=0.01), [entry()], [entry()])
        self.assertEqual(code, 1)

    def test_the_check_refuses_a_malformed_entry(self):
        code, _ = self.run_check(results(), [entry(most="+0.1%")], [])
        self.assertEqual(code, 1)

    def test_the_check_holds_the_bases_workloads(self):
        program = self.write("base_gate.rs", GATE_RS)
        code, summary = self.run_check(results(), [], [], ["--base-program", program])
        self.assertEqual(code, 1)
        self.assertIn("| gone |", summary)
        self.assertIn("dropped", summary)

    def test_the_check_judges_io_results_by_their_metric(self):
        res = io_results(**{"io-put": fsyncs_of("io.fennec", 4)})
        code, summary = self.run_check(res, [], [])
        self.assertEqual(code, 1)
        self.assertIn("fsyncs an answer", summary)
        code, summary = self.run_check(res, [io_entry()], [])
        self.assertEqual(code, 0, summary)
        self.assertIn("accepted", summary)
        code, _ = self.run_check(res, [io_entry()], [io_entry()])
        self.assertEqual(code, 1)
        program = self.write("base_gate.rs", GATE_RS)
        code, summary = self.run_check(io_results(), [], [], ["--base-program", program])
        self.assertEqual(code, 1)
        self.assertIn("| io-gone |", summary)

    def test_a_missing_results_file_is_a_named_failure(self):
        # A count that failed leaves none: the judge says so, never a
        # traceback.
        summary = os.path.join(self.dir.name, "summary.md")
        missing = os.path.join(self.dir.name, "io.json")
        with contextlib.redirect_stdout(io.StringIO()):
            code = gate.main(["check", missing, "--summary", summary])
        self.assertEqual(code, 1)
        with open(summary, encoding="utf-8") as f:
            text = f.read()
        self.assertIn("refused its input", text)
        self.assertIn(f"no results to judge at {missing}", text)

    def test_the_check_says_which_program_ran(self):
        res = results()
        res["programs"] = "each tree's own"
        _, summary = self.run_check(res, [], [])
        self.assertIn("Each tree ran its own program", summary)


class Counting(unittest.TestCase):
    def test_totals_are_callgrinds_totals_line(self):
        out = textwrap.dedent(
            """\
            # callgrind format
            version: 1
            events: Ir
            fn=(1) main
            1 5
            totals: 1234567
            """
        )
        with tempfile.NamedTemporaryFile("w", suffix=".callgrind", delete=False) as f:
            f.write(out)
        self.addCleanup(os.unlink, f.name)
        self.assertEqual(gate.totals(f.name), 1_234_567)

    def test_a_file_without_totals_is_refused(self):
        with tempfile.NamedTemporaryFile("w", suffix=".callgrind", delete=False) as f:
            f.write("events: Ir\n")
        self.addCleanup(os.unlink, f.name)
        with self.assertRaises(ValueError):
            gate.totals(f.name)

    def test_the_allocators_instructions_are_its_functions_self_cost(self):
        # As the static musl build's profile writes them: names compressed
        # to an id once named, by `cfn=` too; a call's inclusive cost after
        # its `calls=` line, never counted -- callgrind charged a `malloc`
        # there with its caller's work -- and a recursion's level read as
        # its function.
        out = textwrap.dedent(
            """\
            events: Ir
            fn=(1) gate::core_put
            3 40
            cfn=(2) malloc
            calls=3 +5
            * 90000
            fn=(2)
            0 30
            cfn=(3) __libc_malloc_impl
            calls=3 +1
            * 450
            fn=(3)
            0 300
            cfn=(4) alloc_slot
            calls=1 +1
            * 120
            fn=(4)
            0 120
            fn=(5) __libc_free'2
            0 64
            fn=(6) memcpy
            0 500
            fn=(7) <alloc::raw_vec::RawVecInner>::finish_grow
            1 10
            fn=(8) mmap
            0 27
            fn=(9) __syscall_ret
            0 18
            totals: 5000
            """
        )
        with tempfile.NamedTemporaryFile("w", suffix=".callgrind", delete=False) as f:
            f.write(out)
        self.addCleanup(os.unlink, f.name)
        # memcpy, which realloc calls and the program does too, is the
        # program's; the system calls musl makes its groups with are not.
        self.assertEqual(gate.allocator(f.name), 30 + 300 + 120 + 64 + 27 + 18)


def tallied(res, name, base_bytes, head_bytes, calls=(1_000, 1_000)):
    """Workload `name` of `res` given heap tallies: each run of a side asks
    for that many bytes in that many calls."""
    w = res["workloads"][name]
    w["heap"] = {
        side: [{"calls": c, "bytes": b, "zeroed": 0, "grown": 0} for _ in w[side]]
        for side, b, c in (("base", base_bytes, calls[0]), ("head", head_bytes, calls[1]))
    }
    return res


def with_heaps(res, nbytes=500_000):
    """Every workload of `res` tallied the same on both sides."""
    for name in res["workloads"]:
        tallied(res, name, nbytes, nbytes)
    return res


class Bytes(unittest.TestCase):
    """The heap's bytes an op, held to the same 1% as the figure: what an
    allocator call priced at the base's cost hides."""

    def test_a_synthetic_one_percent_more_bytes_fails(self):
        res = tallied(with_heaps(results()), "a", 500_000, 505_000)
        v, failed = verdicts(res)
        self.assertTrue(failed)
        self.assertTrue(v["a"].startswith("FAILED"), v)
        self.assertIn("bytes", v["a"])
        self.assertEqual(v["b"], "ok")

    def test_a_bytes_entry_lets_it_through(self):
        res = tallied(with_heaps(results()), "a", 500_000, 505_000)
        v, failed = verdicts(res, [dict(entry(), metric="bytes")])
        self.assertFalse(failed, v)
        self.assertTrue(v["a"].startswith("accepted"), v)

    def test_an_instructions_entry_lets_no_bytes_through(self):
        res = tallied(with_heaps(results()), "a", 500_000, 505_000)
        _, failed = verdicts(res, [entry()])
        self.assertTrue(failed)

    def test_a_bytes_entry_lets_no_instructions_through(self):
        _, failed = verdicts(with_heaps(results(a=0.01)), [dict(entry(), metric="bytes")])
        self.assertTrue(failed)

    def test_both_fail_together_and_say_so(self):
        res = tallied(with_heaps(results(a=0.02)), "a", 500_000, 510_000)
        v, failed = verdicts(res)
        self.assertTrue(failed)
        self.assertIn("instructions", v["a"])
        self.assertIn("bytes", v["a"])

    def test_the_heaps_layout_moves_neither(self):
        # The heap-shift control's shape: every call 30 instructions dearer
        # inside musl's allocator, +3% as counted, the bytes asked the same.
        res = allocating(results(), "a", Allocator.BASE_RUNS, [(BASE + 60_000, 1_000, 180)] * 5)
        res = tallied(with_heaps(res), "a", 500_000, 500_000)
        v, failed = verdicts(res)
        self.assertFalse(failed, v)
        r = row(res, "a")
        self.assertEqual(r[7]["delta"], 0.0)

    def test_a_side_without_its_tallies_is_refused(self):
        res = with_heaps(results())
        res["workloads"]["a"]["heap"]["head"] = []
        with self.assertRaises(ValueError):
            gate.judge(res, [])

    def test_results_without_tallies_are_judged_by_the_figure(self):
        rows, failed = gate.judge(results(), [])
        self.assertFalse(failed)
        self.assertTrue(all(r[7] is None for r in rows))

    def test_bytes_where_the_base_asked_none_fail(self):
        res = tallied(with_heaps(results()), "a", 0, 64)
        v, failed = verdicts(res)
        self.assertTrue(failed, v)
        self.assertEqual(row(res, "a")[7]["delta"], math.inf)

    def test_the_table_shows_calls_and_bytes_an_op(self):
        res = tallied(with_heaps(results()), "a", 500_000, 505_000, calls=(33_000, 34_000))
        rows, _ = gate.judge(res, [])
        text = gate.table(res, rows)
        self.assertIn("calls/op", text)
        self.assertIn("33.0 → 34.0", text)
        self.assertIn("500 → 505", text)
        self.assertIn("+1.00%", text)

    def test_the_programs_heap_line_is_read(self):
        out = "core-put 2000 ops, 2769 ns an op\nheap: calls=64007 bytes=1258000 zeroed=8 grown=230400\n"
        self.assertEqual(
            gate.heap_of(out), {"calls": 64007, "bytes": 1258000, "zeroed": 8, "grown": 230400}
        )
        self.assertIsNone(gate.heap_of("core-put 2000 ops, 2769 ns an op\n"))

    def test_an_entrys_metric_is_one_the_gate_judges(self):
        gate.checked([dict(entry(), metric="bytes"), dict(entry(), metric="instructions")])
        for bad in ("Bytes", "cycles", ""):
            with self.subTest(metric=bad), self.assertRaises(ValueError):
                gate.checked([dict(entry(), metric=bad)])


TRACES = os.path.join(HERE, "perf_gate_traces")


def trace(name):
    with open(os.path.join(TRACES, name), encoding="utf-8") as f:
        return f.read().splitlines()


def counted(counts):
    """What `io_counts` (or a file's share of it) counted that is not
    none, its calls among it: what a test reads, the noughts aside."""
    return {k: v for k, v in counts.items() if v and k != "files"}


class Strace(unittest.TestCase):
    """The I/O counts read off captured `strace -f -y -s 0` traces (in
    tools/perf_gate_traces/): threads interleaved, a call begun
    `<unfinished ...>` and returned `<... resumed>`, writes to a socket and
    a pipe, calls that failed."""

    def test_a_call_resumed_is_joined_to_its_thread_and_counted_where_it_returned(self):
        calls = list(gate.strace_calls(trace("failing-calls.strace")))
        names = [(n, r) for n, _, r, _ in calls]
        # Thread 12's write began after thread 11's, returned before it.
        self.assertEqual(names[:4], [("accept4", 5), ("write", -1), ("write", 100), ("renameat", -1)])
        self.assertIn(("fdatasync", 0), names)
        self.assertEqual(calls[1][1], '7</tmp/fx/data.ro>, ""..., 7')
        self.assertEqual(len(calls), 13)

    def test_failing_calls_are_counted_apart_and_sockets_and_pipes_not_at_all(self):
        # failing_calls.py, the program captured: between its two connections
        # taken, three writes of 100, 101 and 102 bytes to a file, each
        # synced; a write to a file opened for reading (-1 EBADF) and a rename
        # of a name not there (-1 ENOENT); a rename that worked; an fsync of a
        # pipe (-1 EINVAL) and a write to a socket, which are no file's. The
        # socket's write came while the first fdatasync was still in flight:
        # its 100 bytes were not durable at that answer.
        counts, taken = gate.io_counts(trace("failing-calls.strace"), "/tmp/fx", start=1, end=2)
        self.assertEqual(taken, 2)
        self.assertEqual(counted(counts), {"fsyncs": 3, "renames": 1, "writes": 3, "written": 303, "unsynced": 100,
                                           "failed": 2})
        self.assertEqual(counted(counts["files"]["data"]),
                         {"writes": 3, "written": 303, "unsynced": 100, "fsyncs": 3, "fsync_calls": {"fdatasync": 3}})
        self.assertEqual(counts["files"]["data.kept"]["renames"], 1)
        # Each failure by its call and its file.
        self.assertEqual(counts["files"]["data.ro"]["failed_calls"], {"write": 1})
        self.assertEqual(counts["files"]["renamed"]["failed_calls"], {"renameat": 1})

    def test_nothing_outside_the_stretch_counts(self):
        counts, taken = gate.io_counts(trace("failing-calls.strace"), "/tmp/fx", start=2, end=3)
        self.assertEqual(taken, 2)
        self.assertEqual(counted(counts), {})

    def test_a_servers_compact_is_counted_between_its_second_and_third_connection(self):
        # fennec-server under `perf_gate.py io-measure`, workload io-compact:
        # the open's lock file made and truncated, the database made and its
        # magic written, a log left over unlinked (ENOENT), the setup's
        # batches, the log made, all outside; inside, the side file made and
        # truncated (O_CREAT|O_TRUNC), the image written into it (5 writes, 3
        # fdatasyncs, two of them back to back), the old file synced, the log
        # unlinked, the directory synced around the rename, the new file
        # opened again (no flag that makes or cuts it); /proc's and /sys'
        # opens no file's. The answers' writev to sockets, the fcntls, the
        # start-up's ioctl of stdin and the log lines to the stderr pipe,
        # interleaved from five threads, are no file's either; and the
        # compact's answer finds every byte it wrote durable.
        d = "/tmp/perf-gate-io-k8txs89e/io-compact.pk5eaahg"
        counts, taken = gate.io_counts(trace("io-compact.strace"), d)
        self.assertEqual(taken, 3)
        self.assertEqual(counted(counts), {"fsyncs": 6, "renames": 1, "writes": 5, "written": 2_139_997,
                                           "creates": 1, "truncates": 1, "unlinks": 1})
        self.assertEqual(counted(counts["files"]["io.fennec.beside"]), {
            "writes": 5, "written": 2_139_997, "fsyncs": 3, "fsync_calls": {"fdatasync": 3},
            "creates": 1, "truncates": 1,
        })
        self.assertEqual(counted(counts["files"]["io.fennec"]),
                         {"fsyncs": 1, "renames": 1, "fsync_calls": {"fdatasync": 1}})
        self.assertEqual(counted(counts["files"]["io.fennec.sync"]), {"unlinks": 1})
        self.assertEqual(counts["files"]["."]["fsync_calls"], {"fsync": 2})

    def test_a_servers_spill_is_two_writes_and_one_fsync(self):
        # io-spill: the spill record and the land written straight into the
        # file, and one fdatasync of it; the setup's log, made at its first
        # sync, and the shutdown's fdatasync and the log's unlink after the
        # third connection, outside, and the process's exit inside an accept4
        # (`= ?`), which takes no connection. Synced before its answer:
        # nothing unsynced.
        d = "/tmp/perf-gate-io-k8txs89e/io-spill.v_jbznvn"
        counts, taken = gate.io_counts(trace("io-spill.strace"), d)
        self.assertEqual(taken, 3)
        self.assertEqual(counted(counts), {"fsyncs": 1, "writes": 2, "written": 16_785_520 + 1_234_392})
        self.assertEqual(list(counts["files"]), ["io.fennec"])

    def test_a_servers_batch_answers_the_logs_record_durable_and_the_files_not(self):
        # io-batch: the block's 23 883 bytes written into the file and, with
        # the log's header, into the log, the log synced, then the answer.
        # The log's record is durable when the client hears; the file's is
        # not, by design -- the log holds it (durability.md#the-sync-log) --
        # and is counted, each byte once, as the base's own.
        d = "/tmp/perf-gate-io-k8txs89e/io-batch.lcotepa4"
        counts, taken = gate.io_counts(trace("io-batch.strace"), d)
        self.assertEqual(taken, 3)
        self.assertEqual(counted(counts), {"fsyncs": 1, "writes": 3, "written": 47_862, "unsynced": 23_883})
        self.assertEqual(counted(counts["files"]["io.fennec"]), {"writes": 1, "written": 23_883, "unsynced": 23_883})
        self.assertEqual(counted(counts["files"]["io.fennec.sync"]),
                         {"writes": 2, "written": 64 + 23_915, "fsyncs": 1, "fsync_calls": {"fdatasync": 1}})
        # Its log synced before the record was written, then answered: the
        # sync-before-write build's own move, every other count the same.
        lines = trace("io-batch.strace")
        at = next(i for i, line in enumerate(lines) if "23915, 4096) = 23915" in line)
        swapped = lines[:at] + [lines[at + 1], lines[at]] + lines[at + 2:]
        moved, _ = gate.io_counts(swapped, d)
        self.assertEqual(moved["files"]["io.fennec.sync"]["unsynced"], 23_915)
        self.assertEqual({k: v for k, v in counted(moved).items() if k != "unsynced"},
                         {k: v for k, v in counted(counts).items() if k != "unsynced"})

    def test_the_forms_no_capture_holds_are_counted_by_what_they_are(self):
        # hand-written.strace, typed as strace 6 prints: x86_64's rename(;
        # copy_file_range's and splice's second descriptor, sendfile's first,
        # a socket's and a pipe's passed over; fallocate a write call of no
        # bytes; msync with MS_SYNC the mapped file's fsync and without it a
        # writeback, sync every file's, syncfs its file system's,
        # sync_file_range a writeback of the file it names; the files an
        # open made and truncated, ftruncate, a link and three unlinks, one
        # failing, a directory made; an open with no such flag, an fcntl and
        # a pwritev2 that make nothing durable, passed over or counted as
        # the write they are; a file unlinked since it was opened by its
        # name and ` (deleted)`. The sendfile to a socket is an answer: the
        # 300 bytes written to `dst` before it were not durable then.
        lines = trace("hand-written.strace")
        self.assertTrue(lines[0].startswith("# Hand-written, not captured"))
        counts, taken = gate.io_counts(lines, "/tmp/hw")
        self.assertEqual(taken, 3)
        self.assertEqual(counted(counts), {
            "fsyncs": 4, "writebacks": 2, "renames": 1, "writes": 6, "written": 376, "unsynced": 300, "failed": 1,
            "creates": 3, "truncates": 3, "links": 1, "unlinks": 3,
        })
        files = {f: counted(c) for f, c in counts["files"].items()}
        self.assertEqual(files, {
            "data": {"renames": 1},
            "dst": {"writes": 4, "written": 350, "unsynced": 300, "writebacks": 1,
                    "writeback_calls": {"sync_file_range": 1}},
            "old (deleted)": {"writes": 1, "written": 10, "fsyncs": 1, "fsync_calls": {"fdatasync": 1}},
            "(mapped)": {"fsyncs": 1, "fsync_calls": {"msync": 1}, "writebacks": 1, "writeback_calls": {"msync": 1}},
            "(every file)": {"fsyncs": 1, "fsync_calls": {"sync": 1}},
            "(file system)": {"fsyncs": 1, "fsync_calls": {"syncfs": 1}},
            "made": {"creates": 1, "truncates": 2, "writes": 1, "written": 16},
            "c": {"creates": 1, "truncates": 1, "unlinks": 1},
            "made.link": {"links": 1, "unlinks": 1},
            "sub": {"creates": 1, "unlinks": 1},
            "gone": {"failed": 1, "failed_calls": {"unlinkat": 1}},
        })

    def test_a_server_that_sets_up_io_uring_is_refused_wherever_it_did(self):
        # Its ring's writes are no call of the trace: refused, before the
        # stretch as in it, and named for the gate to learn. So is Linux
        # AIO's context.
        ring = (
            "39    io_uring_setup(8, {flags=0, sq_thread_cpu=0, sq_thread_idle=0, sq_entries=8, cq_entries=16, "
            "features=IORING_FEAT_SINGLE_MMAP|IORING_FEAT_NODROP, sq_off={head=0, tail=64}, "
            "cq_off={head=128, tail=192}}) = 12<anon_inode:[io_uring]>"
        )
        aio = "39    io_setup(128, [0xffff9a2c0000]) = 0"
        lines = trace("hand-written.strace")
        first = next(i for i, line in enumerate(lines) if line.startswith("40    accept4("))
        for call, line in (("io_uring_setup", ring), ("io_setup", aio)):
            for at in (first, len(lines) - 1):
                with self.subTest(call=call, at=at), self.assertRaises(RuntimeError) as e:
                    gate.io_counts(lines[:at] + [line] + lines[at:], "/tmp/hw")
                self.assertIn(call, str(e.exception))
                self.assertIn("must learn it", str(e.exception))
            # One the kernel refused set nothing up.
            refused = line.split(" = ")[0] + " = -1 ENOSYS (Function not implemented)"
            counts, _ = gate.io_counts(lines[:first] + [refused] + lines[first:], "/tmp/hw")
            self.assertEqual(counts["fsyncs"], 4)

    def test_a_flag_that_makes_each_write_durable_or_direct_is_refused(self):
        # The same calls, every write a device flush or past the page cache:
        # refused wherever it took on a file of the data directory, before
        # the stretch -- the database opened so at the start -- as in it,
        # naming the call, the file and the flag.
        lines = trace("hand-written.strace")
        first = next(i for i, line in enumerate(lines) if line.startswith("40    accept4("))
        stretch = next(i for i, line in enumerate(lines) if line.startswith("41    rename("))
        for call, line, flag in (
            ("openat", 'openat(AT_FDCWD</tmp/hw>, "data", O_RDWR|O_CREAT|O_CLOEXEC|O_DSYNC, 0644) = 4</tmp/hw/data>',
             "O_DSYNC"),
            ("open", 'open("/tmp/hw/data", O_RDWR|O_SYNC|O_CLOEXEC) = 4</tmp/hw/data>', "O_SYNC"),
            ("openat2", 'openat2(AT_FDCWD</tmp/hw>, "data", {flags=O_RDWR|O_DIRECT|O_CLOEXEC, mode=0644, '
             'resolve=0}, 24) = 4</tmp/hw/data>', "O_DIRECT"),
            ("fcntl", "fcntl(4</tmp/hw/data>, F_SETFL, O_RDWR|O_DIRECT) = 0", "O_DIRECT"),
            ("pwritev2", 'pwritev2(4</tmp/hw/data>, [{iov_base=""..., iov_len=16}], 1, 0, RWF_DSYNC) = 16',
             "RWF_DSYNC"),
            ("pwritev2", 'pwritev2(4</tmp/hw/data>, [{iov_base=""..., iov_len=16}], 1, 0, RWF_SYNC) = 16',
             "RWF_SYNC"),
        ):
            for at in (first, stretch):
                with self.subTest(call=call, flag=flag, at=at), self.assertRaises(RuntimeError) as e:
                    gate.io_counts(lines[:at] + ["41    " + line] + lines[at:], "/tmp/hw")
                self.assertIn(f"{call} of /tmp/hw/data with {flag}", str(e.exception))
                self.assertIn("cannot judge", str(e.exception))
        # One that failed took nothing; one of a file outside the data
        # directory, before the stretch, is no file the run counts; a flag's
        # name in a path is no flag.
        for line in (
            'openat(AT_FDCWD</tmp/hw>, "data", O_RDWR|O_DIRECT, 0644) = -1 EINVAL (Invalid argument)',
            'openat(AT_FDCWD</etc>, "x", O_RDONLY|O_DIRECT) = 4</etc/x>',
            'openat(AT_FDCWD</tmp/hw>, "O_DSYNC", O_RDONLY|O_CLOEXEC) = 4</tmp/hw/O_DSYNC>',
            "fcntl(4</tmp/hw/data>, F_GETFL) = 0x4002 (flags O_RDWR|O_DIRECT)",
        ):
            with self.subTest(line=line):
                counts, _ = gate.io_counts(lines[:first] + ["41    " + line] + lines[first:], "/tmp/hw")
                self.assertEqual(counts["fsyncs"], 4)

    def test_a_count_of_a_file_outside_the_data_directory_fails_the_run(self):
        # Every file is named within the data directory, or one the trace
        # names otherwise -- a link on the way to it -- would put every file
        # outside it on both sides, and nothing would be judged file by file.
        lines = trace("hand-written.strace")
        stretch = next(i for i, line in enumerate(lines) if line.startswith("41    rename("))
        for line in ('write(15</tmp/elsewhere/x>, ""..., 4) = 4', "fdatasync(15</tmp/elsewhere/x>) = 0",
                     'unlink("/tmp/elsewhere/x") = 0', 'rename("/tmp/hw/a", "/tmp/elsewhere/b") = 0'):
            with self.subTest(line=line), self.assertRaises(RuntimeError) as e:
                gate.io_counts(lines[:stretch] + ["41    " + line] + lines[stretch:], "/tmp/hw")
            self.assertIn("/tmp/elsewhere/", str(e.exception))
            self.assertIn("outside its data directory /tmp/hw", str(e.exception))
        # Outside the stretch it is not counted, and fails nothing.
        first = next(i for i, line in enumerate(lines) if line.startswith("40    accept4("))
        gate.io_counts(lines[:first] + ['41    write(15</tmp/elsewhere/x>, ""..., 4) = 4'] + lines[first:], "/tmp/hw")

    def test_the_data_directory_is_named_as_the_trace_names_it(self):
        # A scratch directory reached through a link: the server is given,
        # and the trace read against, the directory with its links resolved.
        with tempfile.TemporaryDirectory() as real:
            link = os.path.join(real, "link")
            os.symlink(real, link)
            d = gate.io_data_dir(link, "io-put")
            self.assertEqual(d, os.path.realpath(d))
            self.assertTrue(d.startswith(os.path.realpath(real) + os.sep), d)
            self.assertTrue(os.path.basename(d).startswith("io-put."))

    def test_every_call_counted_or_refused_is_asked_of_strace(self):
        asked = set(gate.IO_CALLS)
        for kind in (gate.IO_ACCEPTS, gate.IO_SYNCS, gate.IO_RENAMES, set(gate.IO_WRITES), gate.IO_REFUSED,
                     gate.IO_OPENS, set(gate.IO_CHANGES), gate.IO_SENDS):
            self.assertLessEqual(set(kind), asked)
        for call in ("sync", "syncfs", "sync_file_range", "fallocate", "splice", "msync", "io_uring_setup",
                     "io_setup", "openat", "open", "fcntl", "ftruncate", "linkat", "unlinkat", "pwritev2", "ioctl",
                     "sendto", "symlinkat", "mknodat"):
            self.assertIn(call, asked)
        self.assertEqual(gate.IO_CALLS_OF["failed"], gate.IO_COUNTED)
        # The mappings are not: the allocator's were 4 778 of the four
        # workloads' 6 966 lines (docs/design/benchmarks.md, *Rejected*).
        for call in ("mmap", "mremap", "mprotect", "munmap"):
            self.assertNotIn(call, asked)
        self.assertEqual(gate.IO_DURABLE & gate.IO_WRITEBACKS, {"msync"})

    def test_a_line_that_is_no_call_is_passed_over(self):
        lines = [
            "17    +++ exited with 0 +++",
            "18    --- SIGTERM {si_signo=SIGTERM, si_code=SI_USER, si_pid=1, si_uid=0} ---",
            "19    <... fsync resumed>) = 0",
            "20    write(3</d/f>, \"\"..., 9) = ?",
            "garbage",
        ]
        self.assertEqual(list(gate.strace_calls(lines)), [("write", '3</d/f>, ""..., 9', None, None)])


GATE_RS = """
const WORKLOADS: &[(&str, usize, Setup)] = &[
    ("a", 1_000, core_a),
    ("b", 1_000, core_b),
    ("gone", 20, core_gone),
];

const IO_WORKLOADS: &[(&str, usize, IoPhase, IoPhase)] = &[
    ("io-put", 1_000, io_collection, io_put),
    ("io-batch", 1, io_collection, io_batch),
    ("io-gone", 1, io_collection, io_gone),
];
"""


# io-put's files as the server names them, a thousand durable puts: each
# put's record written into the database and into its sync log, an
# fdatasync each of the log but where the log filled -- the file synced in
# its place -- and the log's header written for its next generation.
IO_FILES = {
    "io.fennec": {"writes": 1_000, "written": 550_000, "fsync_calls": {"fdatasync": 2}},
    "io.fennec.sync": {"writes": 1_002, "written": 581_000, "fsync_calls": {"fdatasync": 998}},
}


def io_file(**counts):
    """A file's counts as io_counts gives them: none but `counts`, its
    fsyncs, writebacks and failures the sums of their calls'."""
    c = {k: 0 for k in gate.IO_FILE_COUNTS}
    c.update(fsync_calls={}, writeback_calls={}, failed_calls={})
    c.update(counts)
    c["fsyncs"] = sum(c["fsync_calls"].values())
    c["writebacks"] = sum(c["writeback_calls"].values())
    c["failed"] = sum(c["failed_calls"].values())
    return c


def io_run(files=None, **counts):
    """One run of an I/O workload, io-put's shape: each file's counts those
    of IO_FILES with `files` over them, the totals theirs, and `counts` over
    the rest."""
    fs = {f: dict(c) for f, c in IO_FILES.items()}
    for f, c in (files or {}).items():
        fs.setdefault(f, {}).update(c)
    fs = {f: io_file(**c) for f, c in fs.items()}
    run = {
        "answered": 1_000, "rows": 1_000, "sent": 383_000, "setup_sent": 151, "file_before": 4_400,
        "failed": 0, "files": fs,
    }
    for k in gate.IO_COUNTS:
        if k != "failed":
            run[k] = sum(c[k] for c in fs.values())
    run.update(counts)
    return run


def io_results(**heads):
    """I/O results of `io-put` and `io-batch` (and those named), three runs
    a side alike, each head's run `io_run(**heads[name])`."""
    workloads = {}
    for name in sorted({"io-put", "io-batch", *heads}):
        workloads[name] = {
            "requests": 1_000,
            "base": [io_run() for _ in range(gate.IO_RUNS)],
            "head": [io_run(**heads.get(name, {})) for _ in range(gate.IO_RUNS)],
        }
    return {"metric": "io", "tool": "strace", "runs": gate.IO_RUNS, "workloads": workloads}


def fsyncs_of(f, n, call="fdatasync"):
    """A head's counts with file `f`'s `call`s `n`, its only sync call, the
    totals following."""
    return {"files": {f: {"fsync_calls": {call: n}}}}


def io_entry(workload="io-put", metric="fsyncs", most="+100%", file=None, call=None):
    e = {"workload": workload, "metric": metric, "most": most, "reason": REASON}
    if metric in gate.IO_BY_FILE:
        e["file"] = file or "io.fennec"
    elif file:
        e["file"] = file
    if metric in gate.IO_BY_CALL:
        e["call"] = call or ("fdatasync" if metric == "fsyncs" else "sync_file_range")
    elif call:
        e["call"] = call
    return e


def io_verdicts(res, entries=(), base_names=None):
    rows, failed = gate.judge_io(res, list(entries), base_names=base_names)
    return {r[0]: r[4] for r in rows}, failed


class Io(unittest.TestCase):
    """The I/O workloads' counts, each judged to the call -- fsyncs and
    bytes written file by file, the file after the setup as a count: any
    move fails, up or down, unless an entry the pull request adds names the
    workload, the count and its file, and lets it so far."""

    def test_one_more_fsync_a_write_fails(self):
        v, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 1_002)}))
        self.assertTrue(failed)
        self.assertTrue(v["io-put"].startswith("FAILED"), v)
        self.assertIn("fdatasync of `io.fennec` 2 → 1,002 (+50,000.0%)", v["io-put"])
        self.assertEqual(v["io-batch"], "ok")

    def test_an_entry_the_pull_request_adds_lets_it_through(self):
        res = io_results(**{"io-put": fsyncs_of("io.fennec", 1_002)})
        v, failed = io_verdicts(res, [io_entry(most="+1000")])
        self.assertFalse(failed, v)
        self.assertTrue(v["io-put"].startswith("accepted"), v)
        self.assertIn("fdatasync of `io.fennec`", v["io-put"])

    def test_an_entry_the_base_holds_lets_nothing_through(self):
        e = io_entry(most="+1000")
        _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 1_002)}), gate.added([e], [e]))
        self.assertTrue(failed)

    def test_a_single_byte_more_fails(self):
        v, failed = io_verdicts(io_results(**{"io-batch": {"files": {"io.fennec": {"written": 550_001}}}}))
        self.assertTrue(failed)
        self.assertIn("written of `io.fennec` 550,000 → 550,001", v["io-batch"])

    def test_fewer_fsyncs_fail_too(self):
        # A durable write that syncs less has lost what made it durable: a
        # drop is named as a rise is, and a rise's entry lets none through.
        # (Below one an answer, io-put's floor fails it whatever an entry
        # says: `Floors`.)
        res = io_results(**{"io-other": fsyncs_of("io.fennec.sync", 997)})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec.sync` 998 → 997 (-0.1%)", v["io-other"])
        _, failed = io_verdicts(res, [io_entry("io-other", most="+1", file="io.fennec.sync")])
        self.assertTrue(failed)
        _, failed = io_verdicts(res, [io_entry("io-other", most="-1", file="io.fennec.sync")])
        self.assertFalse(failed)

    def test_an_fsync_of_the_file_where_the_log_was_fails_with_the_count_the_same(self):
        # 998 syncs of the log moved onto the file: as many fsyncs, each some
        # 356 microseconds where the log's took 65 -- what the log is for.
        res = io_results(**{"io-put": {"files": {"io.fennec": {"fsync_calls": {"fdatasync": 1_000}},
                                                 "io.fennec.sync": {"fsync_calls": {}}}}})
        self.assertEqual(res["workloads"]["io-put"]["head"][0]["fsyncs"], res["workloads"]["io-put"]["base"][0]["fsyncs"])
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec` 2 → 1,000", v["io-put"])
        self.assertIn("fdatasync of `io.fennec.sync` 998 → 0 (-100.0%)", v["io-put"])
        # An entry a file lets that file alone through.
        v, failed = io_verdicts(res, [io_entry(most="+998")])
        self.assertTrue(failed)
        self.assertNotIn("fdatasync of `io.fennec` 2", v["io-put"])
        v, failed = io_verdicts(res, [io_entry(most="+998"), io_entry(most="-100%", file="io.fennec.sync")])
        self.assertFalse(failed, v)

    def test_bytes_moved_from_one_file_to_another_fail(self):
        res = io_results(**{"io-batch": {"files": {"io.fennec": {"written": 550_100}, "io.fennec.sync": {"written": 580_900}}}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("written of `io.fennec` 550,000 → 550,100", v["io-batch"])
        self.assertIn("written of `io.fennec.sync` 581,000 → 580,900", v["io-batch"])

    def test_a_file_new_to_a_side_is_a_count_from_none(self):
        res = io_results(**{"io-put": {"files": {"io.fennec.beside": {"writes": 1, "written": 64,
                                                                       "fsync_calls": {"fdatasync": 1}}}}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec.beside` 0 → 1 (from none)", v["io-put"])
        self.assertIn("written of `io.fennec.beside` 0 → 64 (from none)", v["io-put"])

    def test_a_share_of_a_count_of_none_lets_nothing_through(self):
        # +100% of none is none: from 0 a count moves only by a number, and
        # the verdict says so where a share was given.
        res = io_results(**{"io-put": fsyncs_of("io.fennec.beside", 1)})
        v, failed = io_verdicts(res, [io_entry(most="+100%", file="io.fennec.beside")])
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec.beside` 0 → 1 (from none), which its entries (`+100%`) do not let "
                      "through -- a share of none is none: from none an entry names a count (`+1`)", v["io-put"])
        v, failed = io_verdicts(res, [io_entry(most="+1", file="io.fennec.beside")])
        self.assertFalse(failed, v)
        self.assertFalse(gate.io_lets("+100000%", 0, 1))

    def test_a_move_past_its_entries_names_them(self):
        # A count entry too small, or the other way, is said as what it is:
        # never a share of none where a count was given.
        res = io_results(**{"io-put": {"files": {"io.fennec.beside": {"fsync_calls": {"fdatasync": 3}}}}})
        v, _ = io_verdicts(res, [io_entry(most="+1", file="io.fennec.beside")])
        self.assertIn("fdatasync of `io.fennec.beside` 0 → 3 (from none), which its entries (`+1`) do not let through",
                      v["io-put"])
        self.assertNotIn("share", v["io-put"])
        v, _ = io_verdicts(res, [io_entry(most="+1", file="io.fennec.beside"),
                                 io_entry(most="+100%", file="io.fennec.beside")])
        self.assertIn("(`+1`, `+100%`) do not let through -- a share of none is none", v["io-put"])
        v, _ = io_verdicts(io_results(**{"io-other": fsyncs_of("io.fennec", 1)}), [io_entry("io-other", most="+1")])
        self.assertIn("fdatasync of `io.fennec` 2 → 1 (-50.0%), which its entries (`+1`) do not let through",
                      v["io-other"])

    def test_of_several_entries_for_one_count_any_lets_through_what_it_would_alone(self):
        # io.fennec's fdatasyncs are 2 in the base: each entry its own move.
        two = [io_entry(most="+1"), io_entry(most="+5")]
        for n in (3, 7):
            _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", n)}), two)
            self.assertFalse(failed)
        for n in (5, 8):
            _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", n)}), two)
            self.assertTrue(failed)
        # Either way, each lets its own direction through.
        both = [io_entry("io-other", most="+1"), io_entry("io-other", most="-1")]
        for n in (1, 3):
            with self.subTest(n=n):
                _, failed = io_verdicts(io_results(**{"io-other": fsyncs_of("io.fennec", n)}), both)
                self.assertFalse(failed)

    def test_an_entry_lets_through_its_count_alone_and_no_further(self):
        res = io_results(**{"io-put": {"files": {"io.fennec": {"fsyncs": 3, "written": 550_100}}}})
        _, failed = io_verdicts(res, [io_entry(most="+1")])
        self.assertTrue(failed)
        v, failed = io_verdicts(res, [io_entry(most="+1"), io_entry(metric="written", most="+100")])
        self.assertFalse(failed, v)
        _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 4)}), [io_entry(most="+1")])
        self.assertTrue(failed)
        _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 5)}), [io_entry(most="+100%")])
        self.assertTrue(failed)
        _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 3)}), [io_entry("io-batch", most="+1")])
        self.assertTrue(failed)
        _, failed = io_verdicts(io_results(**{"io-put": {"renames": 1}}), [io_entry(metric="writes", most="+1")])
        self.assertTrue(failed)

    def test_the_file_after_the_setup_is_a_count_an_entry_names(self):
        # The server's encoding of the setup -- a record's, the schema's, the
        # header's -- decides it: the same requests sent, and the failure
        # says what moved, never that the requests differed.
        res = io_results(**{"io-put": {"file_before": 4_408}, "io-batch": {"file_before": 4_408}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("the file after the setup (`file_before`) 4,400 → 4,408 (+0.2%)", v["io-put"])
        self.assertNotIn("requests", v["io-put"])
        # An entry each: one for io-put lets io-batch's own move fail.
        v, failed = io_verdicts(res, [io_entry(metric="file_before", most="+8")])
        self.assertTrue(failed)
        self.assertTrue(v["io-put"].startswith("accepted"), v)
        self.assertTrue(v["io-batch"].startswith("FAILED"), v)
        entries = [io_entry(metric="file_before", most="+8"), io_entry("io-batch", "file_before", "+0.18%")]
        v, failed = io_verdicts(res, entries)
        self.assertFalse(failed, v)
        _, failed = io_verdicts(res, [io_entry(metric="file_before", most="+7"), entries[1]])
        self.assertTrue(failed)

    def test_a_move_says_its_share_however_small(self):
        self.assertEqual(gate.io_moved(4_199_800, 4_199_808), "4,199,800 → 4,199,808 (+0.00019%)")
        self.assertEqual(gate.io_moved(1, 501), "1 → 501 (+50,000.0%)")
        self.assertEqual(gate.io_moved(0, 1), "0 → 1 (from none)")

    def test_runs_whose_file_after_the_setup_differs_fail(self):
        res = io_results()
        res["workloads"]["io-put"]["head"][2] = io_run(file_before=4_401)
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("file_before 4,400 (run 1), 4,400 (run 2), 4,401 (run 3)", v["io-put"])

    def test_a_workload_only_this_tree_has_is_reported_not_judged(self):
        res = io_results(**{"io-new": {"renames": 7}})
        res["workloads"]["io-new"]["base"] = []
        res["workloads"]["io-new"]["base_error"] = "io-new: `io` exited 101"
        rows, failed = gate.judge_io(res, [], base_names={"io-put": 1_000, "io-batch": 1})
        self.assertFalse(failed)
        row = {r[0]: r for r in rows}["io-new"]
        self.assertTrue(row[4].startswith("new"), row)
        self.assertIn("| io-new | 1,000 |", gate.io_table(res, rows))

    def test_a_workload_the_base_had_and_did_not_run_fails(self):
        res = io_results()
        res["workloads"]["io-put"]["base"] = []
        res["workloads"]["io-put"]["base_error"] = "io-put: the base's fennec-server did not build"
        v, failed = io_verdicts(res, base_names={"io-put": 1_000, "io-batch": 1})
        self.assertTrue(failed)
        self.assertIn("did not run it", v["io-put"])
        v, failed = io_verdicts(io_results(), base_names={"io-put": 1_000, "io-batch": 1, "io-gone": 1})
        self.assertTrue(failed)
        self.assertTrue(v["io-gone"].startswith("FAILED: dropped"), v)

    def test_runs_that_differ_fail_naming_every_runs_value(self):
        res = io_results()
        res["workloads"]["io-put"]["head"][1] = io_run(files={"io.fennec": {"written": 550_064}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("this tree's runs differ", v["io-put"])
        self.assertIn("written 1,131,000 (run 1), 1,131,064 (run 2), 1,131,000 (run 3)", v["io-put"])
        self.assertIn("written of `io.fennec` 550,000 (run 1), 550,064 (run 2), 550,000 (run 3)", v["io-put"])
        res = io_results()
        res["workloads"]["io-batch"]["base"][2] = io_run(files={"io.fennec.sync": {"fsync_calls": {"fdatasync": 999}}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("the base's runs differ", v["io-batch"])
        self.assertIn("fsyncs of `io.fennec.sync` 998 (run 1), 998 (run 2), 999 (run 3)", v["io-batch"])
        self.assertIn("fdatasync of `io.fennec.sync` 998 (run 1), 998 (run 2), 999 (run 3)", v["io-batch"])
        # A file one run wrote and the others did not.
        res = io_results()
        res["workloads"]["io-put"]["head"][0] = io_run(files={"stray": {"writes": 1, "written": 9}})
        v, _ = io_verdicts(res)
        self.assertIn("writes of `stray` 1 (run 1), 0 (run 2), 0 (run 3)", v["io-put"])

    def test_sides_sent_other_requests_fail_naming_what(self):
        v, failed = io_verdicts(io_results(**{"io-put": {"sent": 383_001}}))
        self.assertTrue(failed)
        self.assertIn("the two sides were not sent the same requests: sent 383,000 → 383,001", v["io-put"])

    def test_fewer_runs_fail(self):
        res = io_results()
        res["workloads"]["io-put"]["head"] = res["workloads"]["io-put"]["head"][:1]
        _, failed = io_verdicts(res)
        self.assertTrue(failed)

    def test_each_judgement_reads_its_own_entries(self):
        # An I/O entry is no instructions workload's, nor the other way.
        _, failed = verdicts(results(), [io_entry()])
        self.assertFalse(failed)
        _, failed = io_verdicts(io_results(), [entry()])
        self.assertFalse(failed)
        with self.assertRaises(ValueError):
            gate.judge_io(io_results(), [io_entry("io-nothing")])

    def test_the_form_of_an_io_entry(self):
        gate.checked([
            io_entry(), io_entry(most="-50%"), io_entry(most="+1"), io_entry(metric="written", most="-4096"),
            io_entry(metric="renames", most="+1"), io_entry(metric="file_before", most="+8"),
        ])
        for bad in ("100%", "+0", "-0%", "+1.5", "1", "+1,5%", "more"):
            with self.subTest(most=bad), self.assertRaises(ValueError):
                gate.checked([io_entry(most=bad)])
        self.assertEqual(gate.io_most("+100%"), ("share", 1.0))
        self.assertEqual(gate.io_most("-2"), ("count", -2))
        # A count judged file by file names its file; one in total, and an
        # instructions entry, name none.
        # A sync judged call by call names its call, one of its kind; any
        # other count names none.
        gate.checked([io_entry(call="fsync"), io_entry(call="syncfs", file="(file system)"),
                      io_entry(metric="writebacks", most="+1", call="msync", file="(mapped)")])
        for bad in (
            dict(io_entry(), file=""), {k: v for k, v in io_entry(metric="written").items() if k != "file"},
            io_entry(metric="renames", most="+1", file="io.fennec"), io_entry(metric="file_before", file="io.fennec"),
            dict(entry(), file="io.fennec"), {k: v for k, v in io_entry().items() if k != "call"},
            io_entry(call="sync_file_range"), io_entry(metric="writebacks", call="fdatasync"),
            io_entry(metric="written", call="fdatasync"), io_entry(metric="unlinks", most="+1", call="unlink"),
            dict(entry(), call="fsync"),
        ):
            with self.subTest(entry=bad), self.assertRaises(ValueError):
                gate.checked([bad])

    def test_the_bases_io_workloads_are_read_off_its_table(self):
        self.assertEqual(gate.io_workload_names(GATE_RS), {"io-put": 1_000, "io-batch": 1, "io-gone": 1})
        self.assertEqual(gate.io_workload_names(GATE_RS.split("const IO_WORKLOADS")[0]), {})
        with self.assertRaises(ValueError):
            gate.io_workload_names(GATE_RS.replace('("io-batch", 1,', '("io-batch", N,'))

    def test_the_table_shows_each_count_its_ratios_and_each_file(self):
        res = io_results(**{"io-put": fsyncs_of("io.fennec", 1_002)})
        rows, _ = gate.judge_io(res, [])
        text = gate.io_table(res, rows)
        self.assertIn("| io-put | 1,000 | 1,000 | 383,000 | 4,400 | 1,000 → 2,000 | 1.00 → 2.00 | 0 | 0 | 0 | 0 "
                      "| 0 | 0 | 2,002 | 1,131,000 | 0 | 2.953 | 257.045 |", text)
        self.assertIn("| io-put | `io.fennec` | 2 → 1,002 | 0 | 2 → 1,002 | - | - | 550,000 | 1,000 | 0 | 0 | - |", text)
        self.assertIn("| io-put | `io.fennec.sync` | 998 | 0 | 998 | - | - | 581,000 | 1,002 | 0 | 0 | - |", text)
        self.assertIn("| io-batch | `io.fennec` | 2 | 0 | 2 | - | - | 550,000 | 1,000 | 0 | 0 | - |", text)
        # Bytes unsynced at an answer and the calls that failed, by file.
        res = io_results(**{"io-put": {"files": {"io.fennec": {"unsynced": 550_000, "failed_calls": {"write": 2}}}}})
        rows, _ = gate.judge_io(res, [])
        text = gate.io_table(res, rows)
        self.assertIn("| io-put | `io.fennec` | 2 | 0 | 2 | - | - | 550,000 | 1,000 | 0 → 550,000 | 0 "
                      "| - → failed write 2 |", text)

    def test_results_of_another_metric_are_refused(self):
        with self.assertRaises(ValueError):
            gate.judge_io(results(), [])
        with self.assertRaises(ValueError):
            gate.judge(io_results(), [])


def floored(name, **counts):
    """Results of workload `name` alone, both sides' runs `io_run(**counts)`
    -- agreeing, so only a floor can fail them."""
    runs = lambda: [io_run(**counts) for _ in range(gate.IO_RUNS)]
    return {"metric": "io", "runs": gate.IO_RUNS, "workloads": {name: {"requests": 1, "base": runs(), "head": runs()}}}


class Floors(unittest.TestCase):
    """What a run of each workload counts at the least: under it the trace
    missed what the server did, and both sides missing it alike would pass
    on nothing."""

    def test_each_workloads_floor(self):
        put = io_run()
        self.assertEqual(gate.io_floor("io-put", put), [])
        none = {"files": {f: {"fsync_calls": {}, "writes": 0} for f in IO_FILES}}
        for name in ("io-put", "io-batch", "io-spill", "io-compact"):
            with self.subTest(name):
                v, failed = io_verdicts(floored(name, **none))
                self.assertTrue(failed)
                self.assertIn("the base's runs counted fsyncs 0", v[name])
                self.assertIn("this tree's runs counted writes 0, under", v[name])
                self.assertIn("the trace missed what the server did", v[name])

    def test_a_durable_put_answered_is_an_fsync_at_the_least(self):
        # io-put: 1 000 answered, 999 fsyncs -- one put answered unsynced, or
        # a sync the trace did not see -- and no entry lets it through.
        short = fsyncs_of("io.fennec.sync", 997)
        self.assertEqual(gate.io_floor("io-put", io_run(**short)), [("fsyncs", 999, 1_000)])
        v, failed = io_verdicts(floored("io-put", **short))
        self.assertTrue(failed)
        self.assertIn("fsyncs 999, under io-put's floor of 1,000", v["io-put"])
        res = io_results(**{"io-put": short})
        v, failed = io_verdicts(res, [io_entry(most="-1", file="io.fennec.sync")])
        self.assertTrue(failed)
        self.assertIn("this tree's runs counted fsyncs 999, under io-put's floor", v["io-put"])
        self.assertIn("a write was answered before its sync", v["io-put"])
        # io-batch's is its one answer's: the same counts hold it.
        self.assertEqual(gate.io_floor("io-batch", io_run(answered=1, **short)), [])
        self.assertEqual(gate.io_floor("io-batch", io_run(answered=1, files={f: {"fsync_calls": {}} for f in IO_FILES})),
                         [("fsyncs", 0, 1)])

    def test_a_compact_renames(self):
        self.assertEqual(gate.io_floor("io-compact", io_run(answered=1)), [("renames", 0, 1)])
        self.assertEqual(gate.io_floor("io-compact", io_run(answered=1, files={"io.fennec": {"renames": 1}})), [])
        self.assertEqual(gate.io_floor("io-spill", io_run(answered=1, files={"io.fennec.sync": {"writes": 0}})), [])

    def test_a_workload_named_nowhere_is_held_to_an_fsync_and_a_write(self):
        self.assertEqual(gate.io_floor("io-new", io_run()), [])
        nothing = io_run(files={f: {"fsync_calls": {}, "writes": 0} for f in IO_FILES})
        self.assertEqual(gate.io_floor("io-new", nothing), [("fsyncs", 0, 1), ("writes", 0, 1)])

    def test_a_new_workload_held_to_nothing_meets_no_floor(self):
        res = floored("io-new", files={f: {"fsync_calls": {}, "writes": 0} for f in IO_FILES})
        res["workloads"]["io-new"]["base"] = []
        v, failed = io_verdicts(res, base_names={})
        self.assertFalse(failed, v)


def put_trace(puts=4, sync="fdatasync(5</d/io.fennec.sync>) = 0", end=()):
    """An io-put's trace as strace prints it, data directory /d: the setup's
    connection, the run's, and each put's record written into the file and
    into its log, then the log synced by `sync`; `end` after the last put,
    then the driver's connection."""
    accept = "1  accept4(3<socket:[1]>, NULL, NULL, SOCK_CLOEXEC) = {}<socket:[{}]>"
    lines = [accept.format(6, 2), accept.format(7, 3)]
    for i in range(puts):
        lines += [
            f'2  pwrite64(4</d/io.fennec>, ""..., 550, {32 + 550 * i}) = 550',
            f'2  pwrite64(5</d/io.fennec.sync>, ""..., 581, {4096 + 581 * i}) = 581',
            "2  " + sync,
        ]
    return lines + ["2  " + line for line in end] + [accept.format(8, 4)]


def traced(name, base, head, answered=4):
    """Results of workload `name` whose sides' every run is the trace given,
    read as `io` reads a server's: the gate's own judgement of two traces."""
    def runs(lines):
        counts, taken = gate.io_counts(lines, "/d")
        assert taken == 3
        run = {"answered": answered, "rows": answered, "sent": 1_000, "setup_sent": 100, "file_before": 32, **counts}
        return [json.loads(json.dumps(run)) for _ in range(gate.IO_RUNS)]

    return {"metric": "io", "runs": gate.IO_RUNS, "workloads": {name: {"requests": answered, "base": runs(base),
                                                                         "head": runs(head)}}}


class Swaps(unittest.TestCase):
    """A sync swapped for a weaker or a costlier one, every count of every
    file the same: each sync call is judged by its name and its file, only
    those that make a file durable meet a floor, and an `msync` is one only
    with MS_SYNC. Read off traces as a server's are -- before calls were
    judged, every one of these passed."""

    def test_an_fsync_where_an_fdatasync_was_fails(self):
        # One word in SyncLog::append (`sync_data` to `sync_all`): on ext4
        # a journal commit every durable put, what the log exists to save.
        res = traced("io-put", put_trace(), put_trace(sync="fsync(5</d/io.fennec.sync>) = 0"))
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec.sync` 4 → 0 (-100.0%)", v["io-put"])
        self.assertIn("fsync of `io.fennec.sync` 0 → 4 (from none)", v["io-put"])
        # Each call is its own count, its entry named so.
        entries = [io_entry(most="-100%", file="io.fennec.sync"),
                   io_entry(most="+4", file="io.fennec.sync", call="fsync")]
        v, failed = io_verdicts(res, entries[:1])
        self.assertTrue(failed)
        v, failed = io_verdicts(res, entries)
        self.assertFalse(failed, v)

    def test_a_sync_file_range_where_an_fdatasync_was_fails(self):
        # It flushes neither the drive's cache nor the file's metadata: a
        # writeback, never a durability. io-put's floor -- an fsync a put
        # answered -- fails it whatever an entry says.
        sfr = "sync_file_range(5</d/io.fennec.sync>, 0, 0, SYNC_FILE_RANGE_WAIT_BEFORE|SYNC_FILE_RANGE_WRITE|" \
              "SYNC_FILE_RANGE_WAIT_AFTER) = 0"
        res = traced("io-put", put_trace(), put_trace(sync=sfr))
        self.assertEqual(res["workloads"]["io-put"]["head"][0]["fsyncs"], 0)
        self.assertEqual(res["workloads"]["io-put"]["head"][0]["writebacks"], 4)
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("this tree's runs counted fsyncs 0, under io-put's floor of 4", v["io-put"])
        # Where the floor holds -- the file synced once besides -- the calls
        # fail it.
        once = ["fdatasync(4</d/io.fennec>) = 0"]
        res = traced("io-batch", put_trace(end=once), put_trace(sync=sfr, end=once), answered=1)
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec.sync` 4 → 0 (-100.0%)", v["io-batch"])
        self.assertIn("sync_file_range of `io.fennec.sync` (no durability) 0 → 4 (from none)", v["io-batch"])
        _, failed = io_verdicts(res, [io_entry("io-batch", most="-100%", file="io.fennec.sync"),
                                      io_entry("io-batch", "writebacks", "+4", "io.fennec.sync")])
        self.assertFalse(failed)

    def test_a_syncfs_where_an_fdatasync_was_fails(self):
        # The whole file system synced each time: the file system's count,
        # never the file it was handed.
        res = traced("io-put", put_trace(), put_trace(sync="syncfs(5</d/io.fennec.sync>) = 0"))
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("syncfs of `(file system)` 0 → 4 (from none)", v["io-put"])
        self.assertIn("fdatasync of `io.fennec.sync` 4 → 0 (-100.0%)", v["io-put"])

    def test_an_msync_is_a_durability_only_with_ms_sync(self):
        # MS_ASYNC asks for nothing the kernel does not do anyway.
        res = traced("io-put", put_trace(sync="msync(0xffff0000, 4096, MS_SYNC) = 0"),
                     put_trace(sync="msync(0xffff0000, 4096, MS_ASYNC) = 0"))
        head = res["workloads"]["io-put"]["head"][0]
        self.assertEqual((head["fsyncs"], head["writebacks"]), (0, 4))
        self.assertEqual(head["files"]["(mapped)"]["writeback_calls"], {"msync": 4})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("this tree's runs counted fsyncs 0, under io-put's floor of 4", v["io-put"])
        both = "msync(0xffff0000, 4096, MS_SYNC|MS_INVALIDATE) = 0"
        self.assertEqual(traced("io-put", put_trace(sync=both), put_trace())["workloads"]["io-put"]["base"][0]
                         ["files"]["(mapped)"]["fsync_calls"], {"msync": 4})

    def test_a_sync_of_an_unlinked_file_is_its_own(self):
        # The log unlinked and synced where the live one was: ` (deleted)`
        # is part of its name, never the live file's count.
        gone = "fdatasync(5</d/io.fennec.sync (deleted)>) = 0"
        res = traced("io-put", put_trace(), put_trace(sync=gone))
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fdatasync of `io.fennec.sync (deleted)` 0 → 4 (from none)", v["io-put"])
        self.assertIn("fdatasync of `io.fennec.sync` 4 → 0 (-100.0%)", v["io-put"])

    def test_the_directorys_changes_are_counts_a_head_is_held_to(self):
        # A file made, truncated, linked or unlinked more is a count that
        # moved, which an entry lets through as any count.
        more = ['openat(AT_FDCWD</d>, "io.fennec.tmp", O_RDWR|O_CREAT|O_EXCL|O_CLOEXEC, 0644) = 9</d/io.fennec.tmp>',
                "ftruncate(9</d/io.fennec.tmp>, 0) = 0", 'linkat(AT_FDCWD</d>, "io.fennec.tmp", AT_FDCWD</d>, '
                '"io.fennec.kept", 0) = 0', 'unlinkat(AT_FDCWD</d>, "io.fennec.tmp", 0) = 0']
        res = traced("io-put", put_trace(), put_trace(end=more))
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        for k in ("creates", "truncates", "links", "unlinks"):
            self.assertIn(f"{k} 0 → 1 (from none)", v["io-put"])
        entries = [io_entry(metric=k, most="+1") for k in ("creates", "truncates", "links", "unlinks")]
        _, failed = io_verdicts(res, entries)
        self.assertFalse(failed)
        _, failed = io_verdicts(res, entries[:3])
        self.assertTrue(failed)


def answered_trace(puts=4, steps=("file", "log", "sync", "answer"), end=()):
    """An io-put's trace as strace prints it, data directory /d, its answers
    in it: each put's `steps` in their order -- its record written into the
    file and into its log, the log synced, the answer written to the
    run's socket -- then `end`, then the driver's connection."""
    accept = "1  accept4(3<socket:[1]>, NULL, NULL, SOCK_CLOEXEC) = {}<socket:[{}]>"
    lines = [accept.format(6, 2), accept.format(7, 3)]
    for i in range(puts):
        lines += ["2  " + {
            "file": f'pwrite64(4</d/io.fennec>, ""..., 550, {32 + 550 * i}) = 550',
            "log": f'pwrite64(5</d/io.fennec.sync>, ""..., 581, {4096 + 581 * i}) = 581',
            "sync": "fdatasync(5</d/io.fennec.sync>) = 0",
            "answer": "writev(7<socket:[3]>, [...], 2) = 153",
        }[step] for step in steps]
    return lines + ["2  " + line for line in end] + [accept.format(8, 4)]


def unsynced(lines):
    """Each file's bytes unsynced at an answer, as `io_counts` reads them."""
    counts, _ = gate.io_counts(lines, "/d")
    return {f: c["unsynced"] for f, c in counts["files"].items() if c["unsynced"]}


class Order(unittest.TestCase):
    """The order of a write and its sync: at each write to a socket -- an
    answer -- the bytes written to each file since its last durable sync,
    each byte once. A record answered before it is durable moves no other
    count: the same writes, the same syncs."""

    def test_a_write_synced_before_its_answer_is_none(self):
        # The review's three, as the server's thread makes them: the log's
        # record written, synced, answered -- the database's own record
        # left to the log, by design, at every answer.
        self.assertEqual(unsynced(answered_trace()), {"io.fennec": 4 * 550})
        # Synced before it was written, then answered; or answered before
        # its sync: the log's record not durable when the client heard.
        self.assertEqual(unsynced(answered_trace(steps=("file", "sync", "log", "answer"))),
                         {"io.fennec": 4 * 550, "io.fennec.sync": 4 * 581})
        self.assertEqual(unsynced(answered_trace(steps=("file", "log", "answer", "sync"))),
                         {"io.fennec": 4 * 550, "io.fennec.sync": 4 * 581})

    def test_a_sync_before_its_write_fails_with_no_entry(self):
        res = traced("io-put", answered_trace(), answered_trace(steps=("file", "sync", "log", "answer")))
        base, head = res["workloads"]["io-put"]["base"][0], res["workloads"]["io-put"]["head"][0]
        for k in ("fsyncs", "writes", "written"):
            self.assertEqual(base[k], head[k], k)
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertEqual(v["io-put"], "FAILED: unsynced of `io.fennec.sync` 0 → 2,324 (from none)")
        _, failed = io_verdicts(res, [io_entry(metric="unsynced", most="+2324", file="io.fennec.sync")])
        self.assertFalse(failed)
        res = traced("io-put", answered_trace(), answered_trace(steps=("file", "log", "answer", "sync")))
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("unsynced of `io.fennec.sync` 0 → 2,324", v["io-put"])

    def test_each_byte_counts_once_however_the_answer_is_written(self):
        # An answer in two writes, a send among them, counts the database's
        # bytes once: a change to how the server writes its answers moves
        # no file's count.
        two = ("file", "log", "sync", "answer", "answer")
        self.assertEqual(unsynced(answered_trace(steps=two)), {"io.fennec": 4 * 550})
        sent = answered_trace(steps=("file", "log", "sync"))
        sent.insert(-1, "2  sendto(7<socket:[3]>, \"\"..., 153, MSG_NOSIGNAL, NULL, 0) = 153")
        self.assertEqual(unsynced(sent), {"io.fennec": 4 * 550})
        # A write to a pipe -- the log -- or a failed one is no answer.
        quiet = answered_trace(steps=("file", "log", "sync"),
                               end=['write(2<pipe:[9]>, ""..., 40) = 40', "writev(7<socket:[3]>, [...], 2) = -1 EPIPE (Broken pipe)"])
        self.assertEqual(unsynced(quiet), {})

    def test_a_sync_clears_its_own_file_and_a_rename_carries_it(self):
        lines = [
            '2  pwrite64(4</d/io.fennec>, ""..., 550, 32) = 550',
            '2  pwrite64(9</d/io.fennec.beside>, ""..., 100, 0) = 100',
            "2  fdatasync(5</d/io.fennec.sync>) = 0",
            '2  renameat(AT_FDCWD</d>, "io.fennec.beside", AT_FDCWD</d>, "io.fennec.next") = 0',
            "2  writev(7<socket:[3]>, [...], 2) = 153",
            '2  pwrite64(9</d/io.fennec.next>, ""..., 10, 100) = 10',
            "2  syncfs(9</d/io.fennec.next>) = 0",
            '2  pwrite64(4</d/io.fennec>, ""..., 20, 582) = 20',
            '2  unlinkat(AT_FDCWD</d>, "io.fennec", 0) = 0',
            "2  writev(7<socket:[3]>, [...], 2) = 153",
        ]
        accept = "1  accept4(3<socket:[1]>, NULL, NULL, SOCK_CLOEXEC) = {}<socket:[{}]>"
        trace_ = [accept.format(6, 2), accept.format(7, 3), *lines, accept.format(8, 4)]
        # Another file's sync clears nothing; a rename carries what was not
        # durable to the new name; a syncfs clears every file; an unlinked
        # file's bytes are no answer's.
        self.assertEqual(unsynced(trace_), {"io.fennec": 550, "io.fennec.next": 100})

    def test_outside_the_stretch_nothing_counts(self):
        lines = answered_trace(steps=("file", "log", "answer", "sync"))
        counts, _ = gate.io_counts(lines[:1] + lines[2:-1] + [lines[1], lines[-1]], "/d")
        self.assertEqual(counts["unsynced"], 0)


class Unseen(unittest.TestCase):
    """What the trace cannot judge is refused rather than dropped: an
    `ioctl` making a file's writes durable, `__O_SYNC` alone, a counted
    call on a descriptor or in a directory strace did not name."""

    def with_line(self, line, at="stretch"):
        lines = trace("hand-written.strace")
        i = next(i for i, l in enumerate(lines) if l.startswith("41    rename(" if at == "stretch" else "40    accept4("))
        return gate.io_counts(lines[:i] + ["41    " + line] + lines[i:], "/tmp/hw")

    def test_an_ioctl_that_makes_writes_durable_is_refused(self):
        for line, request in (
            ("ioctl(4</tmp/hw/data>, FS_IOC_SETFLAGS, [FS_SYNC_FL]) = 0", "FS_IOC_SETFLAGS"),
            ("ioctl(4</tmp/hw/data>, FS_IOC_FSSETXATTR, {fsx_xflags=FS_XFLAG_SYNC}) = 0", "FS_IOC_FSSETXATTR"),
            ("ioctl(8</tmp/hw/dst>, FICLONE, 7) = 0", "FICLONE"),
        ):
            for at in ("stretch", "start"):
                with self.subTest(request=request, at=at), self.assertRaises(RuntimeError) as e:
                    self.with_line(line, at)
                self.assertIn(f"by {request}", str(e.exception))
        # Of a file outside the data directory, a socket's, or refused by the
        # kernel: passed over.
        for line in ("ioctl(15</etc/x>, FS_IOC_SETFLAGS, [FS_SYNC_FL]) = 0", "ioctl(6<socket:[102]>, FIONBIO, [1]) = 0",
                     "ioctl(4</tmp/hw/data>, FS_IOC_SETFLAGS, [FS_SYNC_FL]) = -1 ENOTTY (Inappropriate ioctl for device)"):
            with self.subTest(line=line):
                self.assertEqual(self.with_line(line)[0]["fsyncs"], 4)

    def test_the_bit_o_sync_adds_alone_is_refused(self):
        with self.assertRaises(RuntimeError) as e:
            self.with_line('openat(AT_FDCWD</tmp/hw>, "data", O_RDWR|__O_SYNC|O_CLOEXEC) = 4</tmp/hw/data>', "start")
        self.assertIn("with __O_SYNC", str(e.exception))
        self.assertEqual(gate.strace_flags("O_RDWR|__O_SYNC|O_CLOEXEC", after_path=False),
                         {"O_RDWR", "__O_SYNC", "O_CLOEXEC"})

    def test_a_counted_call_strace_did_not_name_fails_the_run(self):
        # Dropped, it would agree with itself on both sides.
        for line in ("fsync(9) = 0", 'write(9, ""..., 10) = 10', "fdatasync(9) = -1 EBADF (Bad file descriptor)",
                     "ftruncate(9, 0) = 0", 'unlinkat(9, "x", 0) = 0', 'renameat(9, "a", 9, "b") = 0',
                     'openat(9, "x", O_RDWR|O_CREAT, 0644) = -1 ENOENT (No such file or directory)',
                     'sendto(9, ""..., 10, MSG_NOSIGNAL, NULL, 0) = 10', "fcntl(9, F_SETFL, O_RDWR|O_DIRECT) = 0"):
            with self.subTest(line=line), self.assertRaises(RuntimeError) as e:
                self.with_line(line)
            self.assertIn("which strace did not name", str(e.exception))
        # Outside the stretch nothing is counted: passed over, but a flag
        # that would make every later write durable.
        self.assertEqual(self.with_line("fsync(9) = 0", "start")[0]["fsyncs"], 4)
        with self.assertRaises(RuntimeError):
            self.with_line("fcntl(9, F_SETFL, O_RDWR|O_DIRECT) = 0", "start")

    def test_a_node_or_a_symbolic_link_made_is_a_file_made(self):
        counts, _ = self.with_line('symlinkat("data", AT_FDCWD</tmp/hw>, "data.link") = 0')
        self.assertEqual(counts["files"]["data.link"]["creates"], 1)
        counts, _ = self.with_line('mknodat(AT_FDCWD</tmp/hw>, "node", S_IFREG|0644) = 0')
        self.assertEqual(counts["files"]["node"]["creates"], 1)
        counts, _ = self.with_line('symlink("/etc/passwd", "/tmp/hw/l") = 0')
        self.assertEqual(counts["files"]["l"]["creates"], 1)

    def test_a_failure_is_judged_by_its_call_and_file(self):
        # A failed write swapped for a failed fsync of another file: the
        # same total, two moves.
        base = put_trace(end=['write(4</d/io.fennec>, ""..., 8) = -1 ENOSPC (No space left on device)'])
        head = put_trace(end=["fsync(5</d/io.fennec.sync>) = -1 EIO (Input/output error)"])
        res = traced("io-put", base, head)
        self.assertEqual(res["workloads"]["io-put"]["base"][0]["failed"], res["workloads"]["io-put"]["head"][0]["failed"])
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("write of `io.fennec` that failed 1 → 0 (-100.0%)", v["io-put"])
        self.assertIn("fsync of `io.fennec.sync` that failed 0 → 1 (from none)", v["io-put"])
        entries = [io_entry(metric="failed", most="-1", call="write"),
                   io_entry(metric="failed", most="+1", file="io.fennec.sync", call="fsync")]
        _, failed = io_verdicts(res, entries)
        self.assertFalse(failed)
        gate.checked(entries)
        with self.assertRaises(ValueError):
            gate.checked([io_entry(metric="failed", most="+1", call="accept4")])


class EntriesRead(unittest.TestCase):
    """A malformed entries file is a refusal in words, never a traceback;
    an entry says the move it lets through."""

    def test_a_malformed_entries_file_is_refused_in_words(self):
        with tempfile.TemporaryDirectory() as d:
            res = os.path.join(d, "io.json")
            with open(res, "w", encoding="utf-8") as f:
                json.dump(io_results(), f)
            for text in (
                "PERF_ACCEPTED = [\n",
                "PERF_ACCEPTED = [" * 200_000 + "]" * 200_000 + "\n",
                "PERF_ACCEPTED = [{'workload': 'io-put', 'metric': 'fsyncs', 'file': 'io.fennec', 'call': ['fsync'], "
                f"'most': '+1', 'reason': {REASON!r}}}]\n",
                "PERF_ACCEPTED = [{'workload': 'io-put', 'metric': ['fsyncs'], 'most': '+1', "
                f"'reason': {REASON!r}}}]\n",
                "PERF_ACCEPTED = f()\n",
                "x = 1\x00\nPERF_ACCEPTED = []\n",
            ):
                with self.subTest(text=text[:60]):
                    src = os.path.join(d, "perf_gate.py")
                    with open(src, "w", encoding="utf-8") as f:
                        f.write(text)
                    out = io.StringIO()
                    with contextlib.redirect_stdout(out):
                        code = gate.main(["check", res, "--accepted-from", src])
                    self.assertEqual(code, 1)
                    self.assertIn("refused its input", out.getvalue())

    def test_an_entry_says_the_move_it_lets_through(self):
        # Exact counts: an entry of +500 for a move of +1 would leave room
        # a later change could take unseen.
        res = io_results(**{"io-put": fsyncs_of("io.fennec", 3)})
        v, failed = io_verdicts(res, [io_entry(most="+500")])
        self.assertTrue(failed)
        self.assertIn("which its entries (`+500`) do not let through -- an entry names the move it lets through, "
                      "and `+500` is larger than this one (+1)", v["io-put"])
        _, failed = io_verdicts(res, [io_entry(most="+1")])
        self.assertFalse(failed)
        v, failed = io_verdicts(res, [io_entry(most="+100%")])
        self.assertTrue(failed)
        self.assertIn("`+100%` is larger than this one (+1, +50%)", v["io-put"])
        _, failed = io_verdicts(res, [io_entry(most="+50%")])
        self.assertFalse(failed)
        # A share to the decimals it gives.
        self.assertTrue(gate.io_lets("+0.00019%", 4_199_800, 4_199_808))
        self.assertTrue(gate.io_lets("+0.0002%", 4_199_800, 4_199_808))
        self.assertFalse(gate.io_lets("+0.001%", 4_199_800, 4_199_808))
        self.assertFalse(gate.io_lets("+1%", 4_400, 4_408))


def ci_jobs():
    """The perf jobs of the workflow, each its lines but comments and blank
    ones (`jobs_of`)."""
    with open(os.path.join(os.path.dirname(HERE), gate.WORKFLOW), encoding="utf-8") as f:
        return gate.jobs_of(f.read())


def steps_of(job):
    """A job's steps as `jobs_of` gives its lines: {name: the step's lines}."""
    out, name = {}, None
    for line in job.splitlines():
        m = re.match(r"^      - (?:name: (.*)|uses: .*)$", line)
        if m:
            name = m.group(1) or line.strip()
            out[name] = []
        if name:
            out[name].append(line)
    return {k: "\n".join(v) for k, v in out.items()}


class Checker(unittest.TestCase):
    """Which checker counts and judges the I/O: the base's, but where the
    base's own gate.rs mentions no IO_WORKLOADS -- the pull request that
    brought them -- whatever the base's checker can run; chosen in both
    jobs by a grep of the base's gate.rs, no Python of the change's."""

    # Each job's two lines: the base's checker, unless grep read the base's
    # program and found no mention.
    CHOICE = re.compile(r"^ *checker=\S+\n *grep [^\n]*$", re.M)

    def test_the_base_wherever_its_program_mentions_them(self):
        self.assertEqual(gate.io_checker(GATE_RS), "base")
        self.assertEqual(gate.io_checker(GATE_RS.split("const IO_WORKLOADS")[0]), "this tree")
        self.assertEqual(gate.io_checker(""), "this tree")
        # However the table is spelt, or a comment naming it: the base's.
        for source in ("// no const IO_WORKLOADS here\n", "pub const IO_WORKLOADS: &[] = &[];\n",
                       "pub(crate) const IO_WORKLOADS: &[] = &[];\n", "static IO_WORKLOADS: &[] = &[];\n"):
            with self.subTest(source):
                self.assertEqual(gate.io_checker(source), "base")

    def test_both_jobs_choose_it_by_a_grep_held_to_io_checker(self):
        # The jobs' own two lines, run by bash as the runner runs them,
        # choose as `io_checker` does for every program: a pull request's
        # edit of the checker's dispatch, or of `io_checker`, chooses
        # nothing, and the two cannot drift. The first version asked the
        # base's checker for `io --help` and fell back where it failed; the
        # second ran this tree's `perf_gate.py io-checker`, the pull
        # request's Python; the third grepped for `const IO_WORKLOADS` at a
        # line's start, which read a `pub const` table as none.
        with open(os.path.join(os.path.dirname(HERE), gate.PROGRAM), encoding="utf-8") as f:
            committed = f.read()
        programs = [
            GATE_RS, GATE_RS.split("const IO_WORKLOADS")[0], "", "// no const IO_WORKLOADS here\n",
            "    const IO_WORKLOADS: &[(&str, usize, IoPhase, IoPhase)] = &[\n",
            "const IO_WORKLOADS_OLD: &[(&str, usize)] = &[];\n", "pub const IO_WORKLOADS: &[] = &[];\n",
            "pub(crate) const IO_WORKLOADS: &[] = &[];\n", "static IO_WORKLOADS: &[] = &[];\n",
            "const WORKLOADS: &[] = &[];\n", committed,
        ]
        chosen = {}
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, RUNNER_TEMP=d)
            program = os.path.join(d, "base_gate.rs")
            for job, text in ci_jobs().items():
                with self.subTest(job):
                    found = self.CHOICE.findall(text)
                    self.assertEqual(len(found), 1, text)
                    lines = textwrap.dedent(found[0]) + "\n"
                    chosen[job] = lines
                    self.assertIn('"$RUNNER_TEMP/base_gate.rs"', lines)
                    self.assertNotIn("io-checker", text)
                    self.assertNotIn("--help", text)
                    # No step runs this tree's checker: only where the base
                    # mentions no I/O workload is it `$checker`.
                    self.assertIsNone(re.search(r"python3[^\n]* tools/perf_gate\.py", text), text)
                    for source in programs:
                        with open(program, "w", encoding="utf-8") as f:
                            f.write(source)
                        ran = subprocess.run(["bash", "--noprofile", "--norc", "-e", "-c", lines + 'echo "$checker"\n'],
                                             env=env, capture_output=True, text=True, check=True)
                        want = "tools/perf_gate_base.py" if gate.io_checker(source) == "base" else "tools/perf_gate.py"
                        self.assertEqual(ran.stdout.strip(), want, source[:200])
                    # A program grep cannot read chooses the base's checker,
                    # which then fails on it: never this tree's.
                    os.unlink(program)
                    os.mkdir(program)
                    ran = subprocess.run(["bash", "--noprofile", "--norc", "-e", "-c", lines + 'echo "$checker"\n'],
                                         env=env, capture_output=True, text=True, check=True)
                    self.assertEqual(ran.stdout.strip(), "tools/perf_gate_base.py")
                    os.rmdir(program)
        self.assertEqual(len(set(chosen.values())), 1, chosen)


class KeptApart(unittest.TestCase):
    """The count job's two sets of results: the I/O counted whatever became
    of the instructions, and each kept in an artifact of its own, which
    the judge downloads apart -- never one uploaded again over the other."""

    def test_the_io_is_counted_and_kept_whatever_became_of_the_instructions(self):
        steps = steps_of(ci_jobs()["perf-count"])
        count = steps["Count each I/O workload's fsyncs, renames and bytes written against the base's server and this one's"]
        keep = steps["Keep the I/O results beside them"]
        for step in (count, keep):
            self.assertIn("if: ${{ !cancelled() && steps.reach.outputs.built == 'true' }}", step)
        kept = steps["Keep the results for the judgement"]
        self.assertIn("name: perf-results", kept)
        self.assertIn("name: perf-io-results", keep)
        self.assertNotIn("results.json", keep)
        self.assertNotIn("overwrite", ci_jobs()["perf-count"])

    def test_the_judge_downloads_each_apart(self):
        # Each into a directory of its own, which holds nothing else the
        # judge reads: an artifact carrying a `base_gate.rs`, unpacked
        # beside the base's, would have chosen this tree's checker.
        job = ci_jobs()["perf"]
        steps = steps_of(job)
        paths = []
        for step, name, where in (("The count's results", "perf-results", "count"),
                                  ("The I/O count's results", "perf-io-results", "io")):
            with self.subTest(step):
                self.assertIn(f"name: {name}", steps[step])
                self.assertIn("continue-on-error: true", steps[step])
                self.assertIn("!cancelled()", steps[step])
                self.assertIn(f"path: ${{{{ runner.temp }}}}/judge/{where}\n", steps[step] + "\n")
                paths.append(f"$RUNNER_TEMP/judge/{where}/")
        for read in re.findall(r'"(\$RUNNER_TEMP/[^"]+)"', job):
            with self.subTest(read=read):
                inside = [p for p in paths if read.startswith(p)]
                if read.endswith(("results.json", "io.json")):
                    self.assertEqual(len(inside), 1, read)
                else:
                    self.assertEqual(inside, [], read)


def run_script(step):
    """A step's `run:` as the runner hands it to bash: the block under
    `run: |`, its indentation taken off, or the one line after `run:`;
    None where the step runs nothing (a `uses:`)."""
    lines = step.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"^( *)run: ?(.*)$", line)
        if not m:
            continue
        if m.group(2).strip() != "|":
            return m.group(2) + "\n"
        body = []
        for more in lines[i + 1:]:
            if len(more) - len(more.lstrip()) <= len(m.group(1)):
                break
            body.append(more)
        return textwrap.dedent("\n".join(body)) + "\n"
    return None


# A checker each side of a scratch repository commits: what it prints says
# whose ran.
STUB_CHECKER = 'import sys\nprint("{who} CHECKER RAN", *sys.argv[1:])\nPERF_ACCEPTED = []\n'


class TheBasesChecker(unittest.TestCase):
    """Where both jobs write the base's checker, and which they run: the
    base's file as git reads it, this tree's only where git says the base
    has none -- never chosen by what the pull request's tree put at the
    path the copy is written to, nor by a git error. Each job's own first
    steps run under bash, as the runner runs them, in a scratch repository
    whose HEAD is the pull request on its base (HEAD^1)."""

    # Each job's steps up to its first run of the copy.
    FIRST = {
        "perf-count": ("The base's checker", "Whether the change reaches what the gate builds"),
        "perf": ("The base's checker and program", "The gate's own files this change edits, for the owner",
                 "Whether the change reaches what the gate builds"),
    }

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.repo = os.path.join(self.dir.name, "repo")
        self.temp = os.path.join(self.dir.name, "runner-temp")
        os.makedirs(self.repo)

    def sh(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, files=(), links=(), dirs=()):
        """A commit of `files` ({path: text}), `links` ({path: target}) and
        `dirs` ({path: {name: text}}) over the last: its id."""
        if not os.path.isdir(os.path.join(self.repo, ".git")):
            self.sh("init", "-q", "-b", "main")
            self.sh("config", "user.email", "gate@example.com")
            self.sh("config", "user.name", "gate")
        for path, text in dict(files).items():
            full = os.path.join(self.repo, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w", encoding="utf-8") as f:
                f.write(text)
        for path, target in dict(links).items():
            full = os.path.join(self.repo, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            os.symlink(target, full)
        for path, inside in dict(dirs).items():
            for name, text in inside.items():
                full = os.path.join(self.repo, path, name)
                os.makedirs(os.path.dirname(full), exist_ok=True)
                with open(full, "w", encoding="utf-8") as f:
                    f.write(text)
        self.sh("add", "-A")
        self.sh("-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "change")
        return self.sh("rev-parse", "HEAD")

    def base_and_head(self, base=True, **head):
        """A base committing its checker (or none) and README, and a pull
        request on it committing its own checker and `head`'s files."""
        files = {"README.md": "x\n"}
        if base:
            files[gate.CHECKER] = STUB_CHECKER.format(who="BASE")
        self.commit(files)
        self.commit({gate.CHECKER: STUB_CHECKER.format(who="HEAD"), **head.pop("files", {})}, **head)

    def run_job(self, job):
        """The job's first steps (FIRST) in the repository, as the runner
        runs them -- `bash -e`, stopping at the first that fails: (its exit
        status, all they printed)."""
        os.makedirs(self.temp, exist_ok=True)
        env = dict(os.environ, RUNNER_TEMP=self.temp, GITHUB_OUTPUT=os.path.join(self.temp, "output"),
                   GITHUB_STEP_SUMMARY=os.path.join(self.temp, "summary"))
        steps, said = steps_of(ci_jobs()[job]), []
        for name in self.FIRST[job]:
            p = subprocess.run(["bash", "--noprofile", "--norc", "-e", "-c", run_script(steps[name])],
                               cwd=self.repo, env=env, capture_output=True, text=True)
            said.append(f"[{name}]\n{p.stdout}{p.stderr}")
            if p.returncode:
                return p.returncode, "\n".join(said)
        return 0, "\n".join(said)

    def assertRefused(self, job, *words):
        """The job failed, ran nothing of the pull request's, and said why."""
        code, said = self.run_job(job)
        for theirs in ("HEAD CHECKER RAN", "HEAD CODE RAN"):
            self.assertNotIn(theirs, said)
        self.assertNotEqual(code, 0, said)
        for w in words:
            self.assertIn(w, said)
        return said

    def test_a_directory_at_the_path_runs_nothing_of_the_pull_requests(self):
        # The pull request commits a directory where the copy is written,
        # its `__main__.py` what `python3 tools/perf_gate_base.py` would run.
        # Before the base alone chose, the write failed into it ("is a
        # directory"), this tree's checker was copied in, and every later
        # call ran the pull request's own `__main__.py`.
        self.base_and_head(dirs={"tools/perf_gate_base.py": {"__main__.py": 'print("HEAD CODE RAN")\n'}})
        for job in self.FIRST:
            with self.subTest(job):
                self.assertRefused(job, "tools/perf_gate_base.py", "::error")

    def test_a_link_at_the_path_runs_nothing_of_the_pull_requests(self):
        # A link to /dev/null took the base's copy and ran nothing: no
        # `built=`, every judging step skipped, the job green.
        self.base_and_head(links={"tools/perf_gate_base.py": "/dev/null"})
        for job in self.FIRST:
            with self.subTest(job):
                self.assertRefused(job, "tools/perf_gate_base.py", "::error")
        # And `tools/` itself a link: the copy written through it.
        self.dir.cleanup()
        os.makedirs(self.repo)
        self.commit({"README.md": "x\n", gate.CHECKER: STUB_CHECKER.format(who="BASE")})
        self.sh("rm", "-q", "-r", "tools")
        self.commit({"elsewhere/perf_gate.py": STUB_CHECKER.format(who="HEAD")}, links={"tools": "elsewhere"})
        for job in self.FIRST:
            with self.subTest(job, tools="a link"):
                self.assertRefused(job, "tools", "::error")

    def test_this_trees_copy_only_where_the_base_has_none(self):
        # The bootstrap: a base before the checker, whose pull request is
        # counted by its own -- and only then.
        self.base_and_head(base=False)
        for job in self.FIRST:
            with self.subTest(job):
                code, said = self.run_job(job)
                self.assertEqual(code, 0, said)
                self.assertIn("HEAD CHECKER RAN", said)
                self.assertNotIn("BASE CHECKER RAN", said)
        self.dir.cleanup()
        os.makedirs(self.repo)
        self.base_and_head()
        for job in self.FIRST:
            with self.subTest(job, base="has one"):
                code, said = self.run_job(job)
                self.assertEqual(code, 0, said)
                self.assertIn("BASE CHECKER RAN", said)
                self.assertNotIn("HEAD CHECKER RAN", said)

    def test_a_git_error_is_no_base_without_a_checker(self):
        # A base git cannot read is no base without a checker: no parent
        # (a checkout one commit deep), or the base's checker's object
        # missing. Before, any failure of the write fell back to this tree's.
        self.commit({"README.md": "x\n", gate.CHECKER: STUB_CHECKER.format(who="HEAD")})
        for job in self.FIRST:
            with self.subTest(job, base="none"):
                self.assertRefused(job, "::error")
        self.dir.cleanup()
        os.makedirs(self.repo)
        self.base_and_head()
        blob = self.sh("rev-parse", f"HEAD^1:{gate.CHECKER}")
        os.unlink(os.path.join(self.repo, ".git", "objects", blob[:2], blob[2:]))
        for job in self.FIRST:
            with self.subTest(job, base="its checker unreadable"):
                self.assertRefused(job, "::error")


class Tracing(unittest.TestCase):
    """Where strace cannot count, `io` and `io-measure` count nothing: exit
    0 on a developer's machine, a failure under CI."""

    def test_no_strace_fails_under_ci_alone(self):
        with mock.patch.object(gate.shutil, "which", return_value=None), \
                contextlib.redirect_stderr(io.StringIO()) as err:
            with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true"}):
                self.assertEqual(gate.main(["io-measure", "--head-server", "s", "--client", "c", "--out", "o.json"]), 1)
            with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": ""}):
                self.assertEqual(gate.main(["io-measure", "--head-server", "s", "--client", "c", "--out", "o.json"]), 0)
        self.assertIn("nothing measured", err.getvalue())
        self.assertIn("Nothing measured", err.getvalue())


class Names(unittest.TestCase):
    """This tree's workloads held to the base's own program's: none dropped,
    none the base had and did not run passed on."""

    NAMES = {"a": 1_000, "b": 1_000, "gone": 20}

    def test_the_bases_workloads_are_read_off_its_table(self):
        self.assertEqual(gate.workload_names(GATE_RS), self.NAMES)

    def test_a_base_without_the_program_has_none(self):
        self.assertEqual(gate.workload_names(""), {})

    def test_a_table_not_read_whole_is_refused(self):
        for bad in (
            "fn main() {}",
            GATE_RS.replace('("b", 1_000, core_b),', '("b", ROWS, core_b),'),
        ):
            with self.subTest(bad=bad[:40]), self.assertRaises(ValueError):
                gate.workload_names(bad)

    def test_a_dropped_workload_fails_by_name(self):
        rows, failed = gate.judge(results(), [], base_names=self.NAMES)
        self.assertTrue(failed)
        v = {r[0]: r[5] for r in rows}
        self.assertTrue(v["gone"].startswith("FAILED: dropped"), v)
        self.assertEqual(v["a"], "ok")

    def test_a_workload_the_base_had_and_did_not_run_fails(self):
        res = results()
        res["workloads"]["a"]["base"] = []
        res["workloads"]["a"]["base_error"] = "a: exit 101\nthread 'main' panicked"
        rows, failed = gate.judge(res, [], base_names={"a": 1_000, "b": 1_000})
        self.assertTrue(failed)
        self.assertIn("did not run it", {r[0]: r[5] for r in rows}["a"])

    def test_a_new_workload_the_base_cannot_run_is_held_to_nothing(self):
        res = results(c=0.5)
        res["workloads"]["c"]["base"] = []
        res["workloads"]["c"]["base_error"] = "c: no statement `frobnicate`"
        rows, failed = gate.judge(res, [], base_names={"a": 1_000, "b": 1_000})
        self.assertFalse(failed)
        self.assertTrue({r[0]: r[5] for r in rows}["c"].startswith("new"))

    def test_without_the_bases_names_a_base_that_failed_fails(self):
        res = results()
        res["workloads"]["a"]["base"] = []
        res["workloads"]["a"]["base_error"] = "a: exit 101"
        _, failed = gate.judge(res, [])
        self.assertTrue(failed)

    def test_a_workload_this_tree_did_not_run_fails(self):
        res = results()
        res["workloads"]["a"]["head"] = []
        _, failed = gate.judge(res, [])
        self.assertTrue(failed)


WAKE = "policy line 4: `wake` is none of read, read(<fields>), insert"


def unrun(res, name, error=f'called `Result::unwrap()` on an `Err` value: "{WAKE}"'):
    """`res` with workload `name` not run by the base, which said `error`."""
    res["workloads"][name]["base"] = []
    res["workloads"][name]["base_error"] = error
    return res


class Holdout(unittest.TestCase):
    """A replayed pair's verdict: the workloads its base cannot run named
    with their error's words, the workloads that fail named with their
    metrics, and the job as expected only when exactly those fail."""

    NAMES = {"a": 1_000, "b": 1_000}

    def judged(self, res, cannot_run=None):
        rows, failed = gate.judge(res, [], base_names=self.NAMES, cannot_run=cannot_run)
        return rows, failed, {r[0]: r[5] for r in rows}

    def test_a_workload_named_that_the_base_could_not_run_is_held_to_nothing(self):
        rows, failed, v = self.judged(unrun(results(), "a"), {"a": "`wake` is none of"})
        self.assertFalse(failed, v)
        self.assertTrue(v["a"].startswith("the base cannot run it"), v)
        self.assertEqual(gate.failing(rows), {})

    def test_a_workload_named_that_the_base_ran_fails(self):
        rows, failed, v = self.judged(results(), {"a": "`wake` is none of"})
        self.assertTrue(failed)
        self.assertIn("the base ran it", v["a"])
        self.assertEqual(gate.failing(rows), {"a": {"other"}})

    def test_a_workload_named_that_the_base_did_not_run_for_another_reason_fails(self):
        res = unrun(results(), "a", "a: exit 101\nthread 'main' panicked at index out of bounds")
        rows, failed, v = self.judged(res, {"a": "`wake` is none of"})
        self.assertTrue(failed)
        self.assertIn("not the one named", v["a"])
        self.assertEqual(gate.failing(rows), {"a": {"other"}})

    def test_a_workload_named_that_this_tree_ran_fewer_times_fails(self):
        res = unrun(results(), "a")
        res["workloads"]["a"]["head"] = res["workloads"]["a"]["head"][:4]
        rows, failed, v = self.judged(res, {"a": "`wake` is none of"})
        self.assertTrue(failed)
        self.assertIn("fewer than 5 times", v["a"])
        self.assertEqual(gate.failing(rows), {"a": {"other"}})

    def test_a_workload_the_base_did_not_run_and_none_named_fails(self):
        res = unrun(unrun(results(), "a"), "b")
        rows, failed, v = self.judged(res, {"a": "`wake` is none of"})
        self.assertTrue(failed)
        self.assertTrue(v["a"].startswith("the base cannot run it"), v)
        self.assertIn("did not run it", v["b"])
        self.assertEqual(gate.failing(rows), {"b": {"other"}})

    def test_a_workload_named_that_none_is_is_refused(self):
        with self.assertRaises(ValueError):
            gate.judge(results(), [], base_names=self.NAMES, cannot_run={"nothing": "why"})

    def test_failing_reads_each_metric_and_every_other_failure(self):
        res = tallied(with_heaps(results(a=0.02, b=0.02, c=0.0, d=0.02, e=0.0)), "b", 500_000, 510_000)
        res = tallied(res, "c", 500_000, 510_000)
        res["workloads"]["e"]["head"] = []
        res["workloads"]["e"]["heap"]["head"] = []
        rows, _ = gate.judge(res, [entry("d", most="+2.5%")], base_names={**self.NAMES, "gone": 20})
        self.assertEqual(
            gate.failing(rows),
            {"a": {"instructions"}, "b": {"instructions", "bytes"}, "c": {"bytes"}, "e": {"other"}, "gone": {"other"}},
        )
        res = results()
        res["workloads"]["a"]["head"] = res["workloads"]["a"]["head"][:3]
        self.assertEqual(gate.failing(gate.judge(res, [])[0]), {"a": {"other"}})

    def test_an_expectation_is_pass_or_workloads_and_their_metrics(self):
        both, i, b, none = {"instructions", "bytes"}, {"instructions"}, {"bytes"}, set()
        self.assertEqual(gate.expected_of("pass"), {})
        self.assertEqual(gate.expected_of(" pass\n"), {})
        self.assertEqual(
            gate.expected_of("a:both b:bytes,\n c:instructions"),
            {"a": (both, none), "b": (b, none), "c": (i, none)},
        )
        self.assertEqual(
            gate.expected_of("a:bytes+instructions? b:instructions? c:both? d:instructions+bytes"),
            {"a": (b, i), "b": (none, i), "c": (none, both), "d": (both, none)},
        )
        for bad in (
            "", "fail", "a", "a:", ":both", "a:cycles", "a:Bytes", "a:both a:bytes", "pass a:both",
            "a:?", "a:bytes+", "a:+bytes", "a:bytes+bytes?", "a:both+bytes", "a:bytes??", "a:bytes?+both",
        ):
            with self.subTest(text=bad), self.assertRaises(ValueError):
                gate.expected_of(bad)

    def test_an_expectation_is_said_as_it_is_spelt(self):
        for text in ("a:both b:bytes+instructions? c:instructions?", "a:both? b:bytes"):
            with self.subTest(text=text):
                said = gate.expected_said(gate.expected_of(text))
                self.assertEqual(said, ", ".join(f"`{n}` {m}" for n, m in (t.split(":") for t in text.split())))
        self.assertEqual(gate.expected_said(gate.expected_of("pass")), "none")

    def test_workloads_the_base_cannot_run_are_named_with_their_errors_words(self):
        self.assertEqual(gate.cannot_run_of(""), {})
        self.assertEqual(gate.cannot_run_of("  "), {})
        self.assertEqual(gate.cannot_run_of("a,b=`wake` is none of"), {"a": "`wake` is none of", "b": "`wake` is none of"})
        self.assertEqual(gate.cannot_run_of("a = x = y, as its error says"), {"a": "x = y, as its error says"})
        # Too few words to tell one error from another: `panicked` is in
        # every panic's.
        self.assertEqual(gate.cannot_run_of("a=" + "w" * gate.WHY_AT_LEAST), {"a": "w" * gate.WHY_AT_LEAST})
        for bad in ("a", "a=", "=why", " , =why", "a = x = y", "a=panicked", "a= " + "w" * (gate.WHY_AT_LEAST - 1) + " "):
            with self.subTest(text=bad), self.assertRaises(ValueError):
                gate.cannot_run_of(bad)

    def test_exactly_the_named_failures_are_as_expected(self):
        rows, _ = gate.judge(tallied(with_heaps(results(a=0.02)), "b", 500_000, 510_000), [])
        self.assertEqual(gate.unexpected(rows, gate.expected_of("a:instructions b:bytes")), [])
        for expected in (
            "pass",  # two failed
            "a:instructions",  # one fewer
            "a:instructions b:bytes c:bytes",  # one more
            "a:both b:bytes",  # a's bytes did not fail
            "a:instructions b:instructions",  # b failed by another metric
        ):
            with self.subTest(expected=expected):
                self.assertTrue(gate.unexpected(rows, gate.expected_of(expected)))

    def test_a_metric_that_may_fail_may_fail_or_not(self):
        # `a` fails by instructions alone, `b` by both, `c` by nothing.
        res = tallied(with_heaps(results(a=0.02, b=0.02, c=0.0)), "b", 500_000, 510_000)
        rows, _ = gate.judge(res, [])
        self.assertEqual(gate.failing(rows), {"a": {"instructions"}, "b": {"instructions", "bytes"}})
        for expected in (
            "a:instructions b:both",  # as it failed
            "a:instructions+bytes? b:both",  # an optional metric not failing
            "a:instructions b:instructions+bytes?",  # an optional metric failing
            "a:instructions b:both?",  # both optional, both failing
            "a:instructions? b:both c:instructions?",  # a workload that may fail, failing or passing
            "a:both? b:both? c:both?",
        ):
            with self.subTest(expected=expected):
                self.assertEqual(gate.unexpected(rows, gate.expected_of(expected)), [])
        for expected in (
            "a:instructions b:bytes+instructions? c:bytes",  # the required one not failing: `c`
            "a:bytes+instructions? b:both",  # the required one not failing: `a`'s bytes
            "a:instructions b:bytes",  # one unnamed failing: `b`'s instructions
            "a:instructions?",  # an unnamed workload failing: `b`
            "b:both c:instructions?",  # an unnamed workload failing: `a`
        ):
            with self.subTest(expected=expected):
                self.assertTrue(gate.unexpected(rows, gate.expected_of(expected)))
        self.assertEqual(
            gate.unexpected(rows, gate.expected_of("a:bytes+instructions? b:both")),
            ["`a`: expected to fail by bytes (maybe instructions), failed by instructions"],
        )

    def test_a_failure_of_no_metric_is_never_as_expected(self):
        rows, _ = gate.judge(unrun(results(a=0.02), "b"), [], base_names=self.NAMES)
        for expected in ("a:instructions", "a:instructions b:both", "a:instructions b:both?"):
            with self.subTest(expected=expected):
                self.assertTrue(gate.unexpected(rows, gate.expected_of(expected)))


class HoldoutCheck(unittest.TestCase):
    """`check --expect --cannot-run`: the holdout's step, its exit 0 only as
    expected."""

    setUp = Reading.setUp
    write = Reading.write
    run_check = Reading.run_check

    def holdout(self, res, expect, *cannot_run):
        program = self.write("gate.rs", GATE_RS.replace('    ("gone", 20, core_gone),\n', ""))
        more = ["--base-program", program, "--expect", expect]
        for text in cannot_run:
            more += ["--cannot-run", text]
        return self.run_check(res, [], [], more)

    def test_a_pair_failing_as_named_is_as_expected(self):
        code, summary = self.holdout(results(a=0.02), "a:instructions")
        self.assertEqual(code, 0, summary)
        self.assertIn("**As expected**: failed, `a` instructions.", summary)

    def test_a_pair_failing_otherwise_is_not(self):
        code, summary = self.holdout(results(a=0.02, b=0.02), "a:instructions")
        self.assertEqual(code, 1)
        self.assertIn("**Not as expected**", summary)
        self.assertIn("`b`: expected to fail by nothing, failed by instructions", summary)
        code, _ = self.holdout(results(), "a:instructions")
        self.assertEqual(code, 1)

    def test_a_metric_that_may_fail_is_read_from_the_command_line(self):
        res = tallied(with_heaps(results(a=0.02)), "b", 500_000, 510_000)
        code, summary = self.holdout(res, "a:instructions+bytes? b:bytes")
        self.assertEqual(code, 0, summary)
        self.assertIn("**As expected**: failed, `a` instructions, `b` bytes.", summary)
        code, summary = self.holdout(res, "a:instructions b:bytes+instructions")
        self.assertEqual(code, 1)
        self.assertIn("`b`: expected to fail by bytes and instructions, failed by bytes", summary)

    def test_pass_is_no_workload_failing(self):
        self.assertEqual(self.holdout(results(), "pass")[0], 0)
        self.assertEqual(self.holdout(results(a=0.02), "pass")[0], 1)

    def test_a_workload_the_base_cannot_run_is_named_or_fails(self):
        res = unrun(results(), "a")
        self.assertEqual(self.holdout(res, "pass", "a=`wake` is none of")[0], 0)
        self.assertEqual(self.holdout(res, "pass", "")[0], 1)
        self.assertEqual(self.holdout(res, "pass", "a=no such statement")[0], 1)
        self.assertEqual(self.holdout(results(), "pass", "a=`wake` is none of")[0], 1)
        code, summary = self.holdout(res, "pass", "a=`wake`")
        self.assertEqual(code, 1)
        self.assertIn("characters at the least", summary)

    def test_an_expectation_left_empty_is_refused(self):
        code, summary = self.holdout(results(), "")
        self.assertEqual(code, 1)
        self.assertIn("refused", summary)

    def test_the_holdouts_metrics_are_the_instructions_alone(self):
        # The I/O counts joined the checker's metrics in the same merge that
        # brought the holdout: `both` stays instructions and bytes, and no
        # pair expects an I/O count.
        both = frozenset(gate.INSTRUCTION_METRICS)
        self.assertEqual(gate.expected_of("a:both"), {"a": (both, frozenset())})
        self.assertEqual(gate.failures_said({"a": set(both)}), "`a` both")
        self.assertEqual(gate.expected_said({"a": (both, frozenset())}), "`a` both")
        for bad in ("a:fsyncs", "a:bytes+written"):
            with self.subTest(bad), self.assertRaises(ValueError):
                gate.expected_of(bad)
        rows, _ = gate.judge(tallied(with_heaps(results(a=0.02)), "a", 500_000, 510_000))
        self.assertEqual(gate.failing(rows), {"a": set(both)})

    def test_io_results_refuse_the_holdouts_options(self):
        # The holdout replays instructions: an I/O count is held to the
        # base's to the call, and a pair's expectation stands for nothing in
        # its place -- said, not passed over.
        for more in (["--expect", "pass"], ["--cannot-run", "io-put=the base's own error words"]):
            with self.subTest(more):
                code, summary = self.run_check(io_results(), [], [], more)
                self.assertEqual(code, 1)
                self.assertIn("refused its input", summary)
                self.assertIn("judge instruction results", summary)


class CheckArgs(unittest.TestCase):
    """`run` and `io` judge through `check_args`, which makes the check's
    arguments as its command line does: an option added to the check is
    one neither can leave out."""

    setUp = Reading.setUp
    write = Reading.write

    def test_every_option_of_the_command_line_is_there(self):
        seen = []
        real = gate.check
        gate.check = seen.append
        try:
            gate.main(["check", "r.json"])
        finally:
            gate.check = real
        cli = {k: v for k, v in vars(seen[0]).items() if k != "command"}
        self.assertEqual(vars(gate.check_args("r.json")), cli)
        self.assertIn("expect", cli)
        self.assertIn("cannot_run", cli)

    def test_given_options_are_set_and_unknown_ones_refused(self):
        ns = gate.check_args("-r.json", summary="s.md", accepted_base="b.py")
        self.assertEqual((ns.results, ns.summary, ns.accepted_base, ns.expect), ("-r.json", "s.md", "b.py", None))
        with self.assertRaises(TypeError):
            gate.check_args("r.json", expected="pass")

    def test_run_and_io_judge_through_it(self):
        for f in (gate.run, gate.io):
            with self.subTest(f.__name__):
                source = inspect.getsource(f)
                self.assertIn("check(check_args(", source)
                self.assertNotIn("Namespace(", source)

    def test_io_results_are_judged_as_io_judges_them(self):
        # What `make perf-gate-io` does once it has counted: before the
        # merge's fix, a namespace without the holdout's options, which the
        # check read and died on.
        path = self.write("io.json", json.dumps(io_results()))
        with contextlib.redirect_stdout(io.StringIO()):
            code = gate.check(gate.check_args(path, accepted_from=self.write("pr.py", checker([]))))
        self.assertEqual(code, 0)


class Reaches(unittest.TestCase):
    """What the job counts: everything but what no build reads, so a place a
    build reads is counted before anyone lists it."""

    def test_what_no_build_reads_is_not_counted(self):
        for paths in (
            ["site/content/docs/index.html"],
            ["docs/design/benchmarks.md", "README.md", "crates/fennec-core/CLAUDE.md"],
            ["integrations/python/fennec.py", "web/fennec.js", "studio/app.js", "deploy/region.yml"],
        ):
            with self.subTest(paths=paths):
                self.assertFalse(gate.reaches(paths))

    def test_anything_else_is(self):
        for paths in (
            ["Cargo.toml"],
            [".cargo/config.toml"],
            ["rust-toolchain.toml"],
            ["Cargo.lock"],
            ["crates/fennec-core/src/engine.rs"],
            ["tools/perf_gate.py"],
            [".github/workflows/ci.yml"],
            ["Makefile"],
            ["docs/design/benchmarks.md", "Cargo.toml"],
            ["sitemap/x.rs"],
            [],
        ):
            with self.subTest(paths=paths):
                self.assertTrue(gate.reaches(paths))


CI = """name: ci
on:
  pull_request:

jobs:
  fmt:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all --check

  # The count.
  perf-count:
    name: perf (count)
    steps:
      - run: python3 tools/perf_gate.py run --base HEAD^1

  perf:
    name: perf (instructions)
    needs: perf-count
    steps:
      # The judgement.
      - run: python3 tools/perf_gate.py check results.json

  test:
    steps:
      - run: make test
"""


class Workflow(unittest.TestCase):
    def test_the_perf_jobs_are_read_by_their_lines(self):
        jobs = gate.jobs_of(CI)
        self.assertEqual(set(jobs), {"perf-count", "perf"})
        self.assertIn("run --base HEAD^1", jobs["perf-count"])
        self.assertNotIn("make test", jobs["perf"])
        self.assertNotIn("The judgement", jobs["perf"])

    def test_a_comment_is_no_edit_and_a_step_is(self):
        self.assertEqual(gate.jobs_of(CI), gate.jobs_of(CI.replace("# The judgement.", "# Judged.")))
        edited = CI.replace("check results.json", "check results.json || true")
        self.assertNotEqual(gate.jobs_of(CI)["perf"], gate.jobs_of(edited)["perf"])
        renamed = gate.jobs_of(CI.replace("  perf:\n", "  perf-judge:\n"))
        self.assertNotIn("perf", renamed)


class Edits(unittest.TestCase):
    """The gate's own files a change edits, read off two commits of a
    repository of its own."""

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.addCleanup(setattr, gate, "ROOT", gate.ROOT)
        gate.ROOT = self.dir.name
        self.sh("init", "-q", "-b", "main")
        self.sh("config", "user.email", "gate@example.com")
        self.sh("config", "user.name", "gate")
        self.commit({gate.PROGRAM: GATE_RS, gate.CHECKER: checker([]), gate.WORKFLOW: CI, "README.md": "x\n"})
        self.base = self.sh("rev-parse", "HEAD")

    def sh(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.dir.name, capture_output=True, text=True, check=True
        ).stdout.strip()

    def commit(self, files):
        for path, text in files.items():
            full = os.path.join(self.dir.name, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w", encoding="utf-8") as f:
                f.write(text)
        self.sh("add", "-A")
        self.sh("-c", "commit.gpgsign=false", "commit", "-q", "-m", "change")
        return self.sh("rev-parse", "HEAD")

    def test_a_change_beside_the_gate_names_nothing(self):
        head = self.commit({"README.md": "y\n", gate.WORKFLOW: CI.replace("# The judgement.", "# Judged.")})
        self.assertEqual(gate.gate_edits(self.base, head), [])

    def test_each_of_the_gates_files_is_named(self):
        head = self.commit({
            gate.PROGRAM: GATE_RS.replace("1_000, core_a", "100, core_a"),
            gate.CHECKER: checker([entry()]),
            gate.WORKFLOW: CI.replace("check results.json", "check results.json || true"),
        })
        notes = gate.gate_edits(self.base, head)
        text = "\n".join(notes)
        self.assertIn(gate.PROGRAM, text)
        self.assertIn(gate.CHECKER, text)
        self.assertIn("the `perf` job", text)
        self.assertIn("PERF_ACCEPTED gains an entry: `a`", text)
        self.assertNotIn("perf-count", text)

    def test_an_io_entrys_file_and_call_are_named_for_the_owner(self):
        e = io_entry(most="+499", file="io.fennec.sync", call="fsync")
        mid = self.commit({gate.CHECKER: checker([e])})
        gains = gate.gate_edits(self.base, mid)
        self.assertIn("PERF_ACCEPTED gains an entry: `io-put`, fsyncs of `io.fennec.sync` by fsync up to +499 -- "
                      + REASON, gains)
        head = self.commit({gate.CHECKER: checker([])})
        self.assertIn("PERF_ACCEPTED loses an entry, the list's history cut or an entry edited: `io-put`, fsyncs of "
                      "`io.fennec.sync` by fsync up to +499", gate.gate_edits(mid, head))

    def test_an_entry_removed_is_named(self):
        mid = self.commit({gate.CHECKER: checker([entry()])})
        head = self.commit({gate.CHECKER: checker([])})
        self.assertTrue(any("loses an entry" in n for n in gate.gate_edits(mid, head)))

    def test_a_job_renamed_is_named(self):
        head = self.commit({gate.WORKFLOW: CI.replace("  perf:\n", "  perf-judge:\n")})
        self.assertTrue(any("removed or renamed" in n for n in gate.gate_edits(self.base, head)))

    def test_the_cli_writes_a_warning_and_the_summary(self):
        head = self.commit({gate.CHECKER: checker([entry()])})
        summary = os.path.join(self.dir.name, "summary.md")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.main(["edits", "--base", self.base, "--head", head, "--summary", summary])
        self.assertEqual(code, 0)
        self.assertIn("::warning title=perf gate", out.getvalue())
        with open(summary, encoding="utf-8") as f:
            self.assertIn("owner approves", f.read())

    def test_reaches_reads_the_change(self):
        docs = self.commit({"README.md": "z\n"})
        out = os.path.join(self.dir.name, "out")
        with contextlib.redirect_stdout(io.StringIO()):
            gate.main(["reaches", "--base", self.base, "--head", docs, "--output", out])
            code = self.commit({gate.CHECKER: checker([])})
            gate.main(["reaches", "--base", docs, "--head", code, "--output", out])
        with open(out, encoding="utf-8") as f:
            self.assertEqual(f.read(), "built=false\nbuilt=true\n")


class Committed(unittest.TestCase):
    def test_the_committed_entries_are_well_formed(self):
        gate.checked(gate.accepted_in(os.path.join(HERE, "perf_gate.py")))

    def test_the_program_lists_what_the_next_base_will_hold(self):
        # A pull request's workloads are held to these names once it lands:
        # a table this checker cannot read would fail every later change.
        with open(os.path.join(os.path.dirname(HERE), gate.PROGRAM), encoding="utf-8") as f:
            names = gate.workload_names(f.read())
        self.assertGreaterEqual(len(names), 25)
        self.assertIn("sync-apply", names)
        self.assertTrue(all(ops > 0 for ops in names.values()))

    def test_the_program_lists_the_io_workloads(self):
        with open(os.path.join(os.path.dirname(HERE), gate.PROGRAM), encoding="utf-8") as f:
            source = f.read()
        self.assertEqual(set(gate.io_workload_names(source)), {"io-put", "io-batch", "io-spill", "io-compact"})
        # Once it is a base, every pull request after it is counted and
        # judged by the base's checker: the jobs' grep, held to this
        # (`Checker`), reads the committed program so too.
        self.assertEqual(gate.io_checker(source), "base")

    def test_the_holdouts_pairs_name_workloads_the_program_runs(self):
        # Each pair's `expect` and `cannot_run` as the job reads them, held
        # here rather than first at a dispatch: a name misspelt would fail
        # the pair an hour into its run.
        root = os.path.dirname(HERE)
        with open(os.path.join(root, gate.PROGRAM), encoding="utf-8") as f:
            names = gate.workload_names(f.read())
        with open(os.path.join(root, ".github/workflows/perf-holdout.yml"), encoding="utf-8") as f:
            text = f.read()
        expects = re.findall(r'\bexpect: (?:"([^"]*)"|([a-z]+))', text)
        self.assertEqual(len(expects), len(re.findall(r'\bslug: "', text)))
        self.assertGreaterEqual(len(expects), 24)
        for quoted, bare in expects:
            for name in gate.expected_of(quoted or bare):
                self.assertIn(name, names)
        cannot = re.findall(r'\bcannot_run: "([^"]*)"', text)
        self.assertTrue(cannot)
        for spec in cannot:
            for name in gate.cannot_run_of(spec):
                self.assertIn(name, names)

    def test_the_holdouts_verdicts_read_every_pair_of_its_matrix(self):
        # The verdicts job's list is typed out where no job's matrix can be
        # read: a pair added to one and not the other would go unreported,
        # or be reported as a job that did not finish.
        with open(os.path.join(os.path.dirname(HERE), ".github/workflows/perf-holdout.yml"), encoding="utf-8") as f:
            text = f.read()
        matrix = re.findall(r'\bslug: "([^"]+)"', text)
        m = re.search(r"for slug in ([^;]*); do", text)
        self.assertTrue(m)
        listed = m.group(1).replace("\\\n", " ").split()
        self.assertEqual(listed, matrix + ["control"])
        self.assertEqual(len(set(matrix)), len(matrix))

    def test_the_workflow_holds_the_jobs_edits_watches(self):
        with open(os.path.join(os.path.dirname(HERE), gate.WORKFLOW), encoding="utf-8") as f:
            self.assertEqual(set(gate.jobs_of(f.read())), set(gate.JOBS))

    def test_the_bar(self):
        # A change here moves the gate's bar: docs/design/benchmarks.md#the-gate
        # says why it stands where it does.
        self.assertEqual(gate.THRESHOLD, 0.01)
        self.assertGreaterEqual(gate.RUNS, 5)
        self.assertGreaterEqual(gate.IO_RUNS, 3)


if __name__ == "__main__":
    unittest.main()
