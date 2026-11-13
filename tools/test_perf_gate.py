#!/usr/bin/env python3
"""tools/perf_gate.py: the gate's judgement on results of its own making --
a synthetic +1% fails, an entry the pull request adds lets it through, one
the base holds already does not, a median holds a seed's step -- and the
reading of PERF_ACCEPTED as data, never run; then the committed list.

    python3 tools/test_perf_gate.py
"""

import contextlib
import importlib.util
import io
import json
import os
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

    def run_check(self, res, pr_entries, base_entries):
        path = self.write("results.json", json.dumps(res))
        summary = os.path.join(self.dir.name, "summary.md")
        args = ["check", path, "--accepted-from", self.write("pr.py", checker(pr_entries)), "--summary", summary]
        if base_entries is not None:
            args += ["--accepted-base", self.write("base.py", checker(base_entries))]
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


class Committed(unittest.TestCase):
    def test_the_committed_entries_are_well_formed(self):
        gate.checked(gate.accepted_in(os.path.join(HERE, "perf_gate.py")))

    def test_the_bar(self):
        # A change here moves the gate's bar: docs/design/benchmarks.md#the-gate
        # says why it stands where it does.
        self.assertEqual(gate.THRESHOLD, 0.01)
        self.assertGreaterEqual(gate.RUNS, 5)


if __name__ == "__main__":
    unittest.main()
