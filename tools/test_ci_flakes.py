#!/usr/bin/env python3
"""tools/ci_flakes.py over recorded `gh` answers, no network.

    python3 tools/test_ci_flakes.py
"""

import importlib.util
import json
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("ci_flakes", os.path.join(HERE, "ci_flakes.py"))
cf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cf)

REPO = "o/r"


def run(id, event, branch, sha, at, conclusion="failure", title="t"):
    return {"databaseId": id, "conclusion": conclusion, "status": "completed", "headSha": sha,
            "headBranch": branch, "createdAt": at, "event": event, "displayTitle": title}


def job(name, conclusion, step=None, step_conclusion="failure"):
    steps = [{"name": "Set up job", "conclusion": "success"}]
    if step:
        steps.append({"name": step, "conclusion": step_conclusion})
    return {"name": name, "conclusion": conclusion, "steps": steps}


class Recorded:
    """Stands in for cf.gh and cf.gh_lines: answers recorded per request."""

    def __init__(self, runs=None, jobs=None, commits=None):
        self.runs, self.jobs, self.commits = runs or {}, jobs or {}, commits or {}

    def gh(self, *args):
        if args[0] == "run" and args[1] == "list":
            return self.runs[args[args.index("--event") + 1]]
        if args[0] == "run" and args[1] == "view":
            return {"jobs": self.jobs[int(args[2])]}
        raise AssertionError(args)

    def gh_lines(self, *args):
        sha = args[1].rsplit("/", 1)[1]
        if sha not in self.commits:
            raise RuntimeError("gone")
        parent, files = self.commits[sha]
        if args[-1] == ".parents[0]":
            return [{"sha": parent}]
        return [{"filename": f, "sha": b} for f, b in files.items()]


class Case(unittest.TestCase):
    def setUp(self):
        self.saved = (cf.gh, cf.gh_lines)
        cf._commits.clear()
        cf.jobs_of.__defaults__[0].clear()

    def tearDown(self):
        cf.gh, cf.gh_lines = self.saved

    def use(self, rec):
        cf.gh, cf.gh_lines = rec.gh, rec.gh_lines

    def test_infra_names_not_substrings(self):
        for step in ("Set up job", "Complete job", "Post Run actions/checkout@abc", "Run actions/cache@abc",
                     "Run actions/upload-artifact@abc", "(no step: job-level startup_failure)"):
            self.assertTrue(cf.INFRA.match(step), step)
        for step in ("Both sync runners ran every scenario", "Run cargo test", "Check the cache key",
                     "Upload test reports to the server", "Run make test"):
            self.assertFalse(cf.INFRA.match(step), step)

    def test_every_conclusion_but_success_skipped_cancelled(self):
        jobs = [job("a", "success"), job("b", "skipped"), job("c", "cancelled"),
                job("d", "timed_out", "Run make test", "cancelled"),
                job("e", "startup_failure"), job("f", "failure", "Build")]
        self.use(Recorded(jobs={1: jobs}))
        got = dict(cf.failed_jobs(REPO, 1))
        self.assertEqual(sorted(got), ["d", "e", "f"])
        self.assertEqual(got["f"], "Build")
        self.assertTrue(got["e"].startswith("(no step"))

    def test_owns(self):
        self.assertTrue(cf.owns("clippy", "crates/fennec-core/src/lib.rs"))
        self.assertFalse(cf.owns("clippy", "crates/fennec-core/README.md"))
        self.assertFalse(cf.owns("clippy", "Cargo.lock"))
        self.assertFalse(cf.owns("clippy", "docs/design/x.rs"))
        self.assertFalse(cf.owns("clippy", "integrations/go/a.go"))
        self.assertTrue(cf.owns("mobile (Swift, iOS, XCFramework)", "integrations/swift/A.swift"))
        self.assertFalse(cf.owns("mobile (Swift, iOS, XCFramework)", "web/fennec.js"))
        self.assertTrue(cf.owns("deploy (systemd units)", "deploy/x.service"))
        self.assertTrue(cf.owns("fmt", ".github/workflows/ci.yml"))

    def merge_group(self, base_b, files_a, files_b):
        runs = {"push": [], "merge_group": [
            run(2, "merge_group", "gh-readonly-queue/main/pr-7-aaa", "bbb", "2026-11-02T00:00:00Z", "success"),
            run(1, "merge_group", "gh-readonly-queue/main/pr-7-aaa", "aaa", "2026-11-01T00:00:00Z")]}
        jobs = {1: [job("clippy", "failure", "Run cargo clippy")], 2: [job("clippy", "success")]}
        cf._commits.clear()
        self.use(Recorded(runs, jobs, {"aaa": ("base1", files_a), "bbb": (base_b, files_b)}))
        return cf.ledger(REPO, 10)[1]

    def test_merge_group_same_blobs_is_unfixed(self):
        rows = self.merge_group("base1", {"crates/a.rs": "1"}, {"crates/a.rs": "1"})
        self.assertEqual(rows[0]["verdict"], "unfixed")

    def test_merge_group_changed_source_is_fixed(self):
        rows = self.merge_group("base1", {"crates/a.rs": "1"}, {"crates/a.rs": "2"})
        self.assertEqual(rows[0]["verdict"], "fixed")

    def test_merge_group_docs_only_change_is_unfixed(self):
        rows = self.merge_group("base1", {"crates/a.rs": "1", "README.md": "1"}, {"crates/a.rs": "1", "README.md": "2"})
        self.assertEqual(rows[0]["verdict"], "unfixed")

    def test_merge_group_base_drift_is_not_credited(self):
        rows = self.merge_group("base2", {"crates/a.rs": "1"}, {"crates/a.rs": "9"})
        self.assertEqual(rows[0]["verdict"], "unfixed")
        rows = self.merge_group("base2", {"crates/a.rs": "1"}, {"crates/a.rs": "9", "crates/b.rs": "1"})
        self.assertEqual(rows[0]["verdict"], "fixed")

    def test_merge_group_without_a_later_run_is_open(self):
        runs = {"push": [], "merge_group": [run(1, "merge_group", "gh-readonly-queue/main/pr-7-aaa", "aaa", "2026-11-01T00:00:00Z")]}
        self.use(Recorded(runs, {1: [job("fmt", "failure", "Run cargo fmt")]}, {"aaa": ("b", {})}))
        self.assertEqual(cf.ledger(REPO, 10)[1][0]["verdict"], "open")

    def push(self, later_files):
        runs = {"merge_group": [], "push": [
            run(2, "push", "main", "bbb", "2026-11-02T00:00:00Z", "success"),
            run(1, "push", "main", "aaa", "2026-11-01T00:00:00Z")]}
        jobs = {1: [job("test (ubuntu-latest)", "timed_out", "Run make test", "cancelled")],
                2: [job("test (ubuntu-latest)", "success")]}
        self.use(Recorded(runs, jobs, {"aaa": ("p", {"crates/a.rs": "1"}), "bbb": ("aaa", later_files)}))
        return cf.ledger(REPO, 10)[1]

    def test_push_timed_out_counts_and_a_shared_unowned_file_does_not_fix(self):
        rows = self.push({"Cargo.lock": "2", "site/content/docs/a.md": "2"})
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["verdict"], "unfixed")

    def test_push_source_change_fixes(self):
        self.assertEqual(self.push({"crates/b.rs": "2"})[0]["verdict"], "fixed")

    def test_over_300_files_read_from_json_lines(self):
        """gh prints one object a line however many pages: 650 files parse."""
        class R:
            returncode = 0
            stdout = "\n".join(json.dumps({"filename": f"f{i}", "sha": str(i)}) for i in range(650)) + "\n"
        import subprocess
        saved = subprocess.run
        subprocess.run = lambda *a, **k: R()
        try:
            self.assertEqual(len(cf.gh_lines("api", "x", "--paginate", "-q", ".files[]")), 650)
        finally:
            subprocess.run = saved


if __name__ == "__main__":
    unittest.main()
