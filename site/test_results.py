"""The results schema and what build.py takes from it: a median per
(family, cell, metric), superseded runs left out, a re-measured cell counted
once, and no figure of a page left unheld."""
import os
import tempfile
import unittest

import build
import figures

HEAD = "\t".join(build.RESULTS_COLUMNS)


def row(run, cell, metric, value, family="f"):
    return "\t".join([run, "abc12345", "m", "2026-01-01", family, cell, metric, value])


class Results(unittest.TestCase):
    def medians(self, lines, superseded=()):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "f.tsv"), "w") as f:
                f.write("\n".join([HEAD] + lines) + "\n")
            with open(os.path.join(d, "superseded.tsv"), "w") as f:
                f.write("\n".join(["family\trun\tcell\twhy"] + list(superseded)) + "\n")
            old = build.RESULTS_DIR
            build.RESULTS_DIR = d
            try:
                return build.results_medians()
            finally:
                build.RESULTS_DIR = old

    def test_median_of_runs_upper_middle(self):
        m = self.medians([row("a", "x:1", "ops_s", "10"), row("b", "x:1", "ops_s", "30"),
                          row("c", "x:1", "ops_s", "20"), row("d", "x:1", "ops_s", "40")])
        self.assertEqual(m[("f", "x:1", "ops_s")], 30)

    def test_superseded_run_counts_in_no_median(self):
        m = self.medians([row("a", "sys:1", "v", "1"), row("b", "sys:1", "v", "5")],
                         ["f\ta\tsys\tmeasured again"])
        self.assertEqual(m[("f", "sys:1", "v")], 5)

    def test_a_cell_measured_again_counts_once_as_its_last_line(self):
        m = self.medians([row("a", "x", "v", "1"), row("a", "x", "v", "9"), row("b", "x", "v", "9")])
        self.assertEqual(m[("f", "x", "v")], 9)


class Pages(unittest.TestCase):
    def test_every_figure_of_the_pages_is_held(self):
        self.assertEqual(build.check_claims(), [])

    def test_every_figure_names_a_recorded_row(self):
        medians = build.results_medians()
        for _, _, family, cell, metric, _ in figures.FIGURES:
            self.assertIn((family, cell, metric), medians)


if __name__ == "__main__":
    unittest.main()
