#!/usr/bin/env python3
"""tools/coverage_report.py over a small hand-made export: the crates' and
the boundaries' numbers, a registry's and a test's file left out, a file with
no functions, percent formatting, stable ordering, a listed path that is gone
failing the tool, and the committed list's paths existing.

    python3 tools/test_coverage_report.py
"""

import importlib.util
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("coverage_report", os.path.join(HERE, "coverage_report.py"))
cov = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cov)


def kind(count, covered):
    return {"count": count, "covered": covered, "percent": 0}


def file(name, lines, regions, functions):
    return {"filename": name, "summary": {"lines": kind(*lines), "regions": kind(*regions), "functions": kind(*functions)}}


EXPORT = {"data": [{
    "files": [
        file("/w/crates/zeta/src/lib.rs", (10, 5), (4, 1), (2, 1)),
        file("/w/crates/alpha/src/parser.rs", (100, 87), (50, 40), (10, 9)),
        file("/w/crates/alpha/src/other.rs", (100, 13), (50, 10), (10, 1)),
        file("/w/crates/alpha/src/empty.rs", (0, 0), (0, 0), (0, 0)),
        file("/home/r/.cargo/registry/src/x/serde/src/lib.rs", (999, 999), (9, 9), (9, 9)),
        file("/w/crates/alpha/tests/all.rs", (999, 999), (9, 9), (9, 9)),
    ],
    "functions": [
        {"name": "_RNvCs1_5alpha5parse", "count": 3, "filenames": ["/w/crates/alpha/src/parser.rs"],
         "regions": [[1, 1, 2, 2, 3, 0, 0, 0], [3, 1, 4, 2, 0, 0, 0, 0], [5, 1, 5, 2, 0, 0, 0, 2]]},
        {"name": "_RNvCs1_5alpha14parse_unrelated", "count": 0, "filenames": ["/w/crates/alpha/src/parser.rs"],
         "regions": [[1, 1, 2, 2, 0, 0, 0, 0]]},
        {"name": "_RNvCs1_5alpha5parseB3_", "count": 0, "filenames": ["/w/crates/alpha/src/parser.rs"],
         "regions": [[1, 1, 2, 2, 0, 0, 0, 0]]},
    ],
}]}
BOUNDS = [
    {"path": "crates/alpha/src/parser.rs", "reads": "text", "functions": ["parse"]},
    {"path": "crates/alpha/src/empty.rs", "reads": "nothing"},
    {"path": "crates/alpha/src/absent.rs", "reads": "not compiled"},
]


class Report(unittest.TestCase):
    def setUp(self):
        self.r = cov.build(EXPORT, BOUNDS)

    def test_crates_sum_their_files_and_leave_out_registry_and_tests(self):
        a = self.r["crates"]["alpha"]
        self.assertEqual(a["lines"], {"count": 200, "covered": 100})
        self.assertEqual(a["regions"], {"count": 100, "covered": 50})
        self.assertEqual(a["functions"], {"count": 20, "covered": 10})
        self.assertEqual(list(self.r["crates"]), ["alpha", "zeta"])

    def test_boundary_rows(self):
        p, e, g = self.r["boundaries"]
        self.assertEqual(p["file"]["lines"], {"count": 100, "covered": 87})
        self.assertEqual(e["file"]["functions"], {"count": 0, "covered": 0})
        self.assertIsNone(g["file"])
        self.assertEqual([b["path"] for b in self.r["boundaries"]], [b["path"] for b in BOUNDS])

    def test_function_row_matches_exact_names_and_code_regions(self):
        fn = self.r["boundaries"][0]["functions"]
        # `parse` and its monomorphised copy, not `parse_unrelated`.
        self.assertEqual(fn["functions"], {"count": 2, "covered": 1})
        self.assertEqual(fn["regions"], {"count": 3, "covered": 1})

    def test_percent_formatting(self):
        self.assertEqual(cov.percent(87, 100), "87.0%")
        self.assertEqual(cov.percent(1, 3), "33.3%")
        self.assertEqual(cov.percent(0, 0), "-")
        self.assertEqual(cov.percent(0, 5), "0.0%")

    def test_markdown_is_stable_and_states_figures(self):
        text = cov.render(self.r)
        self.assertEqual(text, cov.render(cov.build(EXPORT, BOUNDS)))
        self.assertIn("| alpha | 100 / 200 (50.0%) | 50 / 100 (50.0%) | 10 / 20 (50.0%) |", text)
        self.assertIn("| `crates/alpha/src/parser.rs` | text | 87 / 100 (87.0%)", text)
        self.assertIn("| `crates/alpha/src/empty.rs` | nothing | 0 / 0 (-)", text)
        self.assertIn("not in the export", text)
        self.assertLess(text.index("| alpha |"), text.index("| zeta |"))
        self.assertNotIn("serde", text)


class Paths(unittest.TestCase):
    def test_missing_path_is_reported_and_fails_the_tool(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "crates/a/src"))
            open(os.path.join(d, "crates/a/src/here.rs"), "w").close()
            bounds = [{"path": "crates/a/src/here.rs", "reads": "x"}, {"path": "crates/a/src/renamed.rs", "reads": "y"}]
            self.assertEqual(cov.missing_paths(d, bounds), ["crates/a/src/renamed.rs"])
            self.assertEqual(cov.missing_paths(d, bounds[:1]), [])
        saved = cov.BOUNDARIES
        cov.BOUNDARIES = [{"path": "crates/nowhere/src/gone.rs", "reads": "x"}]
        try:
            self.assertEqual(cov.main(["--check-paths"]), 1)
        finally:
            cov.BOUNDARIES = saved

    def test_committed_list_exists_and_each_row_says_what_it_reads(self):
        self.assertEqual(cov.missing_paths(), [])
        self.assertEqual(cov.main(["--check-paths"]), 0)
        for b in cov.BOUNDARIES:
            self.assertTrue(b["reads"].strip())


if __name__ == "__main__":
    unittest.main()
