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
does, a workload only this tree has is reported, runs that differ fail --
and their reading off captured `strace -f` traces
(tools/perf_gate_traces/); then the committed list and the holdout's
matrix and verdicts.

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


class Strace(unittest.TestCase):
    """The I/O counts read off captured `strace -f -y -s 0` traces (in
    tools/perf_gate_traces/): threads interleaved, a call begun
    `<unfinished ...>` and returned `<... resumed>`, writes to a socket and
    a pipe, calls that failed."""

    def test_a_call_resumed_is_joined_to_its_thread_and_counted_where_it_returned(self):
        calls = list(gate.strace_calls(trace("failing-calls.strace")))
        names = [(n, r) for n, _, r in calls]
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
        # pipe (-1 EINVAL) and a write to a socket, which are no file's.
        counts, taken = gate.io_counts(trace("failing-calls.strace"), "/tmp/fx", start=1, end=2)
        self.assertEqual(taken, 2)
        self.assertEqual(
            {k: counts[k] for k in gate.IO_COUNTS},
            {"fsyncs": 3, "renames": 1, "writes": 3, "written": 303, "failed": 2},
        )
        self.assertEqual(counts["files"]["data"], {"writes": 3, "written": 303, "fsyncs": 3, "renames": 0})
        self.assertEqual(counts["files"]["data.kept"]["renames"], 1)

    def test_nothing_outside_the_stretch_counts(self):
        counts, taken = gate.io_counts(trace("failing-calls.strace"), "/tmp/fx", start=2, end=3)
        self.assertEqual(taken, 2)
        self.assertEqual(sum(counts[k] for k in gate.IO_COUNTS), 0)

    def test_a_servers_compact_is_counted_between_its_second_and_third_connection(self):
        # fennec-server under `perf_gate.py io`, workload io-compact: the
        # open's lock file and magic, the setup's batches, the log made, and
        # after the third connection the shutdown's fdatasync, all outside;
        # inside, the image written beside (5 writes, 3 fdatasyncs), the old
        # file synced, the directory synced around the rename. The answers'
        # writev to sockets and the log lines to the stderr pipe, all
        # interleaved from five threads, are no file's.
        d = "/tmp/perf-gate-io-0_b8e9bu/io-compact.ji10qz3z"
        counts, taken = gate.io_counts(trace("io-compact.strace"), d)
        self.assertEqual(taken, 3)
        self.assertEqual(
            {k: counts[k] for k in gate.IO_COUNTS},
            {"fsyncs": 6, "renames": 1, "writes": 5, "written": 2_139_997, "failed": 0},
        )
        self.assertEqual(counts["files"]["io.fennec.beside"], {"writes": 5, "written": 2_139_997, "fsyncs": 3, "renames": 0})
        self.assertEqual(counts["files"]["io.fennec"], {"writes": 0, "written": 0, "fsyncs": 1, "renames": 1})
        self.assertEqual(counts["files"]["."]["fsyncs"], 2)

    def test_a_servers_spill_is_two_writes_and_one_fsync(self):
        # io-spill: the spill record and the land written straight into the
        # file, and one fdatasync of it; the setup's log, made at its first
        # sync, and the shutdown's fdatasync after the third connection,
        # outside, and the process's exit inside an accept4 (`= ?`), which
        # takes no connection.
        d = "/tmp/perf-gate-io-0_b8e9bu/io-spill.kg34x97g"
        counts, taken = gate.io_counts(trace("io-spill.strace"), d)
        self.assertEqual(taken, 3)
        self.assertEqual(
            {k: counts[k] for k in gate.IO_COUNTS},
            {"fsyncs": 1, "renames": 0, "writes": 2, "written": 16_785_520 + 1_234_392, "failed": 0},
        )
        self.assertEqual(list(counts["files"]), ["io.fennec"])

    def test_the_forms_no_capture_holds_are_counted_by_what_they_are(self):
        # hand-written.strace, typed as strace 6 prints: x86_64's rename(;
        # copy_file_range's and splice's second descriptor, sendfile's first,
        # a socket's and a pipe's passed over; fallocate a write call of no
        # bytes; msync the mapped file's fsync and sync every file's; syncfs
        # the file it names; a file unlinked since it was opened by its name.
        lines = trace("hand-written.strace")
        self.assertTrue(lines[0].startswith("# Hand-written, not captured"))
        counts, taken = gate.io_counts(lines, "/tmp/hw")
        self.assertEqual(taken, 3)
        self.assertEqual(
            {k: counts[k] for k in gate.IO_COUNTS},
            {"fsyncs": 4, "renames": 1, "writes": 5, "written": 360, "failed": 0},
        )
        self.assertEqual(counts["files"]["dst"], {"writes": 4, "written": 350, "fsyncs": 1, "renames": 0})
        self.assertEqual(counts["files"]["old"], {"writes": 1, "written": 10, "fsyncs": 1, "renames": 0})
        self.assertEqual(counts["files"]["data"], {"writes": 0, "written": 0, "fsyncs": 0, "renames": 1})
        self.assertEqual(counts["files"]["(mapped)"]["fsyncs"], 1)
        self.assertEqual(counts["files"]["(every file)"]["fsyncs"], 1)
        self.assertEqual(set(counts["files"]), {"dst", "old", "data", "(mapped)", "(every file)"})

    def test_a_server_that_sets_up_io_uring_is_refused_wherever_it_did(self):
        # Its ring's writes are no call of the trace: refused, before the
        # stretch as in it, and named for the gate to learn.
        ring = (
            "39    io_uring_setup(8, {flags=0, sq_thread_cpu=0, sq_thread_idle=0, sq_entries=8, cq_entries=16, "
            "features=IORING_FEAT_SINGLE_MMAP|IORING_FEAT_NODROP, sq_off={head=0, tail=64}, "
            "cq_off={head=128, tail=192}}) = 12<anon_inode:[io_uring]>"
        )
        lines = trace("hand-written.strace")
        for at in (8, len(lines) - 1):
            with self.subTest(at=at), self.assertRaises(RuntimeError) as e:
                gate.io_counts(lines[:at] + [ring] + lines[at:], "/tmp/hw")
            self.assertIn("io_uring_setup", str(e.exception))
            self.assertIn("must learn it", str(e.exception))
        # One the kernel refused set nothing up.
        refused = ring.split(" = ")[0] + " = -1 ENOSYS (Function not implemented)"
        counts, _ = gate.io_counts(lines[:8] + [refused] + lines[8:], "/tmp/hw")
        self.assertEqual(counts["fsyncs"], 4)

    def test_every_call_counted_or_refused_is_asked_of_strace(self):
        asked = set(gate.IO_CALLS)
        for kind in (gate.IO_ACCEPTS, gate.IO_SYNCS, gate.IO_RENAMES, set(gate.IO_WRITES), gate.IO_REFUSED):
            self.assertLessEqual(set(kind), asked)
        for call in ("sync", "syncfs", "sync_file_range", "fallocate", "splice", "msync", "io_uring_setup"):
            self.assertIn(call, asked)

    def test_a_line_that_is_no_call_is_passed_over(self):
        lines = [
            "17    +++ exited with 0 +++",
            "18    --- SIGTERM {si_signo=SIGTERM, si_code=SI_USER, si_pid=1, si_uid=0} ---",
            "19    <... fsync resumed>) = 0",
            "20    write(3</d/f>, \"\"..., 9) = ?",
            "garbage",
        ]
        self.assertEqual(list(gate.strace_calls(lines)), [("write", '3</d/f>, ""..., 9', None)])


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
# put's record written into the database and into its sync log, an fsync
# each of the log but where the log filled -- the file synced in its
# place -- and the log's header written for its next generation.
IO_FILES = {
    "io.fennec": {"writes": 1_000, "written": 550_000, "fsyncs": 2, "renames": 0},
    "io.fennec.sync": {"writes": 1_002, "written": 581_000, "fsyncs": 998, "renames": 0},
}


def io_run(files=None, **counts):
    """One run of an I/O workload, io-put's shape: each file's counts those
    of IO_FILES with `files` over them, the totals theirs, and `counts` over
    the rest."""
    fs = {f: dict(c) for f, c in IO_FILES.items()}
    for f, c in (files or {}).items():
        fs.setdefault(f, {"writes": 0, "written": 0, "fsyncs": 0, "renames": 0}).update(c)
    run = {
        "answered": 1_000, "rows": 1_000, "sent": 383_000, "setup_sent": 151, "file_before": 4_400,
        "failed": 0, "files": fs,
    }
    for k in ("fsyncs", "renames", "writes", "written"):
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


def fsyncs_of(f, n):
    """A head's counts with file `f`'s fsyncs `n`, the totals following."""
    return {"files": {f: {"fsyncs": n}}}


def io_entry(workload="io-put", metric="fsyncs", most="+100%", file=None):
    e = {"workload": workload, "metric": metric, "most": most, "reason": REASON}
    if metric in gate.IO_BY_FILE:
        e["file"] = file or "io.fennec"
    elif file:
        e["file"] = file
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
        self.assertIn("fsyncs of `io.fennec` 2 → 1,002 (+50,000.0%)", v["io-put"])
        self.assertEqual(v["io-batch"], "ok")

    def test_an_entry_the_pull_request_adds_lets_it_through(self):
        res = io_results(**{"io-put": fsyncs_of("io.fennec", 1_002)})
        v, failed = io_verdicts(res, [io_entry(most="+1000")])
        self.assertFalse(failed, v)
        self.assertTrue(v["io-put"].startswith("accepted"), v)
        self.assertIn("fsyncs of `io.fennec`", v["io-put"])

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
        self.assertIn("fsyncs of `io.fennec.sync` 998 → 997 (-0.1%)", v["io-other"])
        _, failed = io_verdicts(res, [io_entry("io-other", most="+1", file="io.fennec.sync")])
        self.assertTrue(failed)
        _, failed = io_verdicts(res, [io_entry("io-other", most="-1", file="io.fennec.sync")])
        self.assertFalse(failed)

    def test_an_fsync_of_the_file_where_the_log_was_fails_with_the_count_the_same(self):
        # 998 syncs of the log moved onto the file: as many fsyncs, each some
        # 356 microseconds where the log's took 65 -- what the log is for.
        res = io_results(**{"io-put": {"files": {"io.fennec": {"fsyncs": 1_000}, "io.fennec.sync": {"fsyncs": 0}}}})
        self.assertEqual(res["workloads"]["io-put"]["head"][0]["fsyncs"], res["workloads"]["io-put"]["base"][0]["fsyncs"])
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fsyncs of `io.fennec` 2 → 1,000", v["io-put"])
        self.assertIn("fsyncs of `io.fennec.sync` 998 → 0 (-100.0%)", v["io-put"])
        # An entry a file lets that file alone through.
        v, failed = io_verdicts(res, [io_entry(most="+998")])
        self.assertTrue(failed)
        self.assertNotIn("fsyncs of `io.fennec` 2", v["io-put"])
        v, failed = io_verdicts(res, [io_entry(most="+998"), io_entry(most="-100%", file="io.fennec.sync")])
        self.assertFalse(failed, v)

    def test_bytes_moved_from_one_file_to_another_fail(self):
        res = io_results(**{"io-batch": {"files": {"io.fennec": {"written": 550_100}, "io.fennec.sync": {"written": 580_900}}}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("written of `io.fennec` 550,000 → 550,100", v["io-batch"])
        self.assertIn("written of `io.fennec.sync` 581,000 → 580,900", v["io-batch"])

    def test_a_file_new_to_a_side_is_a_count_from_none(self):
        res = io_results(**{"io-put": {"files": {"io.fennec.beside": {"writes": 1, "written": 64, "fsyncs": 1}}}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("fsyncs of `io.fennec.beside` 0 → 1 (from none)", v["io-put"])
        self.assertIn("written of `io.fennec.beside` 0 → 64 (from none)", v["io-put"])

    def test_a_share_of_a_count_of_none_lets_nothing_through(self):
        # +100% of none is none: from 0 a count moves only by a number, and
        # the verdict says so where a share was given.
        res = io_results(**{"io-put": fsyncs_of("io.fennec.beside", 1)})
        v, failed = io_verdicts(res, [io_entry(most="+100%", file="io.fennec.beside")])
        self.assertTrue(failed)
        self.assertIn("a share of none lets nothing through: an entry names a count (`+1`)", v["io-put"])
        v, failed = io_verdicts(res, [io_entry(most="+1", file="io.fennec.beside")])
        self.assertFalse(failed, v)
        self.assertFalse(gate.io_lets("+100000%", 0, 1))

    def test_of_several_entries_for_one_count_any_lets_through_what_it_would_alone(self):
        two = [io_entry(most="+1"), io_entry(most="+5")]
        _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 5)}), two)
        self.assertFalse(failed)
        _, failed = io_verdicts(io_results(**{"io-put": fsyncs_of("io.fennec", 8)}), two)
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
        entries = [io_entry(metric="file_before", most="+8"), io_entry("io-batch", "file_before", "+1%")]
        v, failed = io_verdicts(res, entries)
        self.assertFalse(failed, v)
        _, failed = io_verdicts(res, [io_entry(metric="file_before", most="+7"), entries[1]])
        self.assertTrue(failed)

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
        res["workloads"]["io-batch"]["base"][2] = io_run(files={"io.fennec.sync": {"fsyncs": 999}})
        v, failed = io_verdicts(res)
        self.assertTrue(failed)
        self.assertIn("the base's runs differ", v["io-batch"])
        self.assertIn("fsyncs of `io.fennec.sync` 998 (run 1), 998 (run 2), 999 (run 3)", v["io-batch"])
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
        for bad in (
            dict(io_entry(), file=""), {k: v for k, v in io_entry(metric="written").items() if k != "file"},
            io_entry(metric="renames", most="+1", file="io.fennec"), io_entry(metric="file_before", file="io.fennec"),
            dict(entry(), file="io.fennec"),
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
        self.assertIn("| io-put | 1,000 | 1,000 | 383,000 | 4,400 | 1,000 → 2,000 | 1.00 → 2.00 |", text)
        self.assertIn("| 1,131,000 | 2.953 | 257.045 |", text)
        self.assertIn("| io-put | `io.fennec` | 2 → 1,002 | 550,000 | 1,000 | 0 |", text)
        self.assertIn("| io-put | `io.fennec.sync` | 998 | 581,000 | 1,002 | 0 |", text)
        self.assertIn("| io-batch | `io.fennec` | 2 | 550,000 | 1,000 | 0 |", text)

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
        none = {"files": {f: {"fsyncs": 0, "writes": 0} for f in IO_FILES}}
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
        self.assertEqual(gate.io_floor("io-batch", io_run(answered=1, files={f: {"fsyncs": 0} for f in IO_FILES})),
                         [("fsyncs", 0, 1)])

    def test_a_compact_renames(self):
        self.assertEqual(gate.io_floor("io-compact", io_run(answered=1)), [("renames", 0, 1)])
        self.assertEqual(gate.io_floor("io-compact", io_run(answered=1, files={"io.fennec": {"renames": 1}})), [])
        self.assertEqual(gate.io_floor("io-spill", io_run(answered=1, files={"io.fennec.sync": {"writes": 0}})), [])

    def test_a_workload_named_nowhere_is_held_to_an_fsync_and_a_write(self):
        self.assertEqual(gate.io_floor("io-new", io_run()), [])
        nothing = io_run(files={f: {"fsyncs": 0, "writes": 0} for f in IO_FILES})
        self.assertEqual(gate.io_floor("io-new", nothing), [("fsyncs", 0, 1), ("writes", 0, 1)])

    def test_a_new_workload_held_to_nothing_meets_no_floor(self):
        res = floored("io-new", files={f: {"fsyncs": 0, "writes": 0} for f in IO_FILES})
        res["workloads"]["io-new"]["base"] = []
        v, failed = io_verdicts(res, base_names={})
        self.assertFalse(failed, v)


class Checker(unittest.TestCase):
    """Which checker counts and judges the I/O: the base's, but where the
    base's own gate.rs lists no IO_WORKLOADS -- the pull request that
    brought them -- whatever the base's checker can run."""

    setUp = Reading.setUp
    write = Reading.write

    def test_the_base_wherever_its_program_lists_them(self):
        self.assertEqual(gate.io_checker(GATE_RS), "base")
        self.assertEqual(gate.io_checker(GATE_RS.split("const IO_WORKLOADS")[0]), "this tree")
        self.assertEqual(gate.io_checker(""), "this tree")
        self.assertEqual(gate.io_checker("// no const IO_WORKLOADS here\n"), "this tree")

    def test_the_command_prints_the_checker_to_run(self):
        def ran(text):
            out = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                code = gate.main(["io-checker", "--base-program", self.write("g.rs", text), "B.py", "T.py"])
            return code, out.getvalue().strip()

        self.assertEqual(ran(GATE_RS), (0, "B.py"))
        self.assertEqual(ran(""), (0, "T.py"))
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(gate.main(["io-checker", "--base-program", "/nonexistent/g.rs", "B.py", "T.py"]), 1)

    def test_both_jobs_ask_it_and_probe_nothing(self):
        # The fallback was `io --help` on the base's checker: a base that
        # dropped or broke `io` would have handed every later pull request
        # to its own checker.
        with open(os.path.join(os.path.dirname(HERE), gate.WORKFLOW), encoding="utf-8") as f:
            jobs = gate.jobs_of(f.read())
        for job in gate.JOBS:
            with self.subTest(job):
                self.assertIn("io-checker --base-program", jobs[job])
                self.assertNotIn("--help", jobs[job])


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
            names = gate.io_workload_names(f.read())
        self.assertEqual(set(names), {"io-put", "io-batch", "io-spill", "io-compact"})

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
