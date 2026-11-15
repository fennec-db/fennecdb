"""Tests of tools/osv_check.py's parsing: python3 -I tools/test_osv_check.py"""
import datetime
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import osv_check as o  # noqa: E402


class Gradle(unittest.TestCase):
    def test_parses_all_shipped_configurations(self):
        self.assertEqual(o.maven_deps('api("a:b:1")\nimplementation("c:d:2")\nruntimeOnly("e:f:3")'), [("a:b", "1"), ("c:d", "2"), ("e:f", "3")])

    def test_unparsed_line_fails(self):
        for text in ('implementation(project(":x"))', 'api("a:b")', "implementation(libs.x)", 'compileOnly(group = "a", name = "b")'):
            with self.assertRaises(o.AuditError, msg=text):
                o.maven_deps(text)

    def test_variable_version_fails(self):
        for text in ('api("a:b:$v")', 'api("a:b:${v}")'):
            with self.assertRaises(o.AuditError, msg=text):
                o.maven_deps(text)

    def test_test_dependencies_are_not_audited(self):
        self.assertEqual(o.maven_deps('testImplementation("a:b:1")'), [])


class Pub(unittest.TestCase):
    V = ["0.2.9", "0.3.0", "0.3.7", "0.4.0", "1.0.0", "1.4.2", "2.0.0"]

    def test_caret_on_zero_minor(self):
        self.assertEqual(o.pick_pub(self.V, "^0.3.0"), "0.3.7")
        self.assertEqual(o.pick_pub(self.V, "^0.3"), "0.3.7")

    def test_caret_on_zero_zero(self):
        self.assertEqual(o.pick_pub(["0.0.3", "0.0.4"], "^0.0.3"), "0.0.3")

    def test_caret_on_major(self):
        self.assertEqual(o.pick_pub(self.V, "^1.0.0"), "1.4.2")

    def test_nothing_admitted_fails(self):
        with self.assertRaises(o.AuditError):
            o.pick_pub(self.V, "^3.0.0")


class Ignores(unittest.TestCase):
    def write(self, entries):
        f = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
        json.dump({"ignore": entries}, f)
        f.close()
        self.addCleanup(os.unlink, f.name)
        return f.name

    def entry(self, expires):
        return {"id": "GHSA-aaaa-bbbb-cccc", "package": "p", "reason": "r", "expires": expires}

    def test_expired_fails(self):
        with self.assertRaises(o.AuditError):
            o.load_ignores(self.write([self.entry("2026-01-01")]), datetime.date(2026, 2, 1))

    def test_live_entry_is_honoured(self):
        ig = o.load_ignores(self.write([self.entry("2026-12-31")]), datetime.date(2026, 2, 1))
        self.assertTrue(o.ignored(["ghsa-aaaa-bbbb-cccc"], ig))

    def test_missing_field_fails(self):
        with self.assertRaises(o.AuditError):
            o.load_ignores(self.write([{"id": "CVE-2026-0001", "package": "p"}]))

    def test_shipped_list_loads(self):
        o.load_ignores()


class Others(unittest.TestCase):
    def test_nuget_unfetched_data_fails(self):
        self.assertTrue(o.nuget_findings("warning NU1900: Error occurred while getting package vulnerability data", {}))
        self.assertTrue(o.nuget_findings("restored, nothing printed", {}))
        self.assertEqual(o.nuget_findings("The given project `F` has no vulnerable packages given the current sources.", {}), [])

    def test_npm_ignore(self):
        rep = {"vulnerabilities": {"x": {"via": [{"severity": "high", "title": "t", "url": "https://github.com/advisories/GHSA-aaaa-bbbb-cccc", "source": 1}]}}}
        self.assertEqual(len(o.npm_findings(rep, {})), 1)
        self.assertEqual(o.npm_findings(rep, {"GHSA-AAAA-BBBB-CCCC": {}}), [])


if __name__ == "__main__":
    unittest.main()
