#!/usr/bin/env python3
"""tools/perf_gate.py: the gate's judgement on results of its own making --
a synthetic +1% fails, an entry the pull request adds lets it through, one
the base holds already does not, a median holds a seed's step, an
allocation's cost moving with the heap passes where an allocation more
fails, a +1% in heap bytes fails as the figure's does, a workload the base
had and this tree dropped or the base did not run fails -- the reading of
a profile's calls into the allocator, of a run's heap tally, of the base's
workloads, of PERF_ACCEPTED as data, never run; which changes are counted,
and the gate's own files a change edits; then the committed list.

    python3 tools/test_perf_gate.py
"""

import contextlib
import importlib.util
import io
import json
import math
import os
import subprocess
import tempfile
import textwrap
import unittest

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


GATE_RS = """
const WORKLOADS: &[(&str, usize, Setup)] = &[
    ("a", 1_000, core_a),
    ("b", 1_000, core_b),
    ("gone", 20, core_gone),
];
"""


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

    def test_the_workflow_holds_the_jobs_edits_watches(self):
        with open(os.path.join(os.path.dirname(HERE), gate.WORKFLOW), encoding="utf-8") as f:
            self.assertEqual(set(gate.jobs_of(f.read())), set(gate.JOBS))

    def test_the_bar(self):
        # A change here moves the gate's bar: docs/design/benchmarks.md#the-gate
        # says why it stands where it does.
        self.assertEqual(gate.THRESHOLD, 0.01)
        self.assertGreaterEqual(gate.RUNS, 5)


if __name__ == "__main__":
    unittest.main()
