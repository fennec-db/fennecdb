#!/usr/bin/env python3
"""tools/size_budgets.py: a binary past its budget, more than 1% under it
or more than 1% from the figure the docs quote, refused, and one inside
them passed; a binary built another way held within 1% either way; an
entry no run measures refused; then the repository's own file, each
entry's measurement inside its own budget.

    python3 tools/test_size_budgets.py
"""

import importlib.util
import json
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("size_budgets", os.path.join(HERE, "size_budgets.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

ENTRY = {"measured": 1_000_000, "budget": 1_005_000}


class Judge(unittest.TestCase):
    def test_inside_the_budget_passes(self):
        for size in (1_005_000, 1_000_000, 995_000, 994_950):
            self.assertIsNone(check.judge("fennec", ENTRY, size), size)

    def test_a_byte_past_the_budget_fails_and_says_what_to_write(self):
        why = check.judge("fennec", ENTRY, 1_005_001)
        self.assertIn("1 past its budget of 1005000", why)
        self.assertIn('"measured": 1005001, "budget": 1011000', why)
        self.assertIn("what the bytes buy", why)

    def test_more_than_one_percent_under_the_budget_asks_for_a_measurement(self):
        why = check.judge("fennec", ENTRY, 994_949)
        self.assertIn("more than 1% under its budget", why)
        self.assertIn('"measured": 994949, "budget": 1000000', why)

    def test_a_budget_moved_without_its_measurement_is_refused(self):
        # A pull request that raised the budget alone: the docs quote a
        # figure 2% off what ships.
        entry = {"measured": 1_000_000, "budget": 1_025_000}
        why = check.judge("fennec", entry, 1_020_000)
        self.assertIn("more than 1% from the 1000000 the docs quote", why)

    def test_a_budget_is_half_a_percent_up_to_a_thousand_bytes(self):
        self.assertEqual(check.budget_for(1_000_000), 1_005_000)
        self.assertEqual(check.budget_for(4_852_496), 4_877_000)
        self.assertEqual(check.budget_for(8_174_128), 8_215_000)
        # From about 200 KB up the thousand bytes stay inside the room.
        for size in (200_000, 2_848_432, 9_999_999, 123_456_789):
            self.assertIsNone(check.judge("x", {"measured": size, "budget": check.budget_for(size)}, size), size)


class Near(unittest.TestCase):
    def test_within_one_percent_either_way_passes(self):
        for size in (990_000, 1_010_000, 1_000_000):
            self.assertIsNone(check.judge_near("fennec-server", ENTRY, size), size)

    def test_further_fails_either_way(self):
        for size in (989_999, 1_010_001):
            self.assertIn("built another way", check.judge_near("fennec-server", ENTRY, size))


class Run(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def binary(self, name, size):
        path = os.path.join(self.dir.name, name)
        with open(path, "wb") as f:
            f.write(b"\0" * size)
        return path

    def test_every_entry_is_measured(self):
        entries = {"a": {"measured": 1000, "budget": 1000}, "b": {"measured": 1000, "budget": 1000}}
        rows, problems = check.run(entries, [("a", self.binary("a", 1000))])
        self.assertEqual(len(rows), 1)
        self.assertEqual(problems, ["b: in tools/size-budgets.json, and this run measured no binary of it"])

    def test_near_measures_one_entry_alone(self):
        entries = {"a": {"measured": 1000, "budget": 1005}, "b": {"measured": 1000, "budget": 1005}}
        _, problems = check.run(entries, [("a", self.binary("a", 1009))], near=True)
        self.assertEqual(problems, [])

    def test_a_name_with_no_entry_and_a_missing_file_are_refused(self):
        entries = {"a": {"measured": 1000, "budget": 1000}}
        _, problems = check.run(entries, [("a", os.path.join(self.dir.name, "none")), ("zz", self.binary("zz", 1))])
        self.assertTrue(problems[0].startswith("a: "), problems)
        self.assertEqual(problems[1], "zz: no entry in tools/size-budgets.json")

    def test_the_table_names_each_binary(self):
        rows, _ = check.run({"a": {"measured": 2048, "budget": 3000}}, [("a", self.binary("a", 2048))])
        text = check.table(rows)
        self.assertIn("| `a` | 2048 | 2 | 3000 | 952 | +0.00% |", text)


class TheFile(unittest.TestCase):
    def test_each_entry_passes_its_own_measurement(self):
        with open(check.FILE, encoding="utf-8") as f:
            data = json.load(f)
        self.assertTrue(data["binaries"])
        for name, entry in data["binaries"].items():
            self.assertIsInstance(entry["measured"], int, name)
            self.assertIsInstance(entry["budget"], int, name)
            self.assertTrue(entry["build"], name)
            self.assertIsNone(check.judge(name, entry, entry["measured"]), name)
        # The image's figure is fennec-server's (site/build.py).
        self.assertIn("fennec-server", data["binaries"])


if __name__ == "__main__":
    unittest.main()
