#!/usr/bin/env python3
"""tools/security_md_check.py: each way SECURITY.md falls out of step with
the tree, refused on a small fixture tree, and what holds, passed; then the
repository's own files.

    python3 tools/test_security_md_check.py
"""

import importlib.util
import os
import pathlib
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("security_md_check", os.path.join(HERE, "security_md_check.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

GOOD = """# Security policy

Acknowledged within 3 working days.

## Supported versions

Today none. Flags `--http-token` and --tls-ca. See `tools/a.py`,
[the design](docs/design/x.md#a-heading) and [here](#supported-versions).
Own SHA-256 and HMAC-SHA-256 and PBKDF2-HMAC-SHA-256 and ChaCha20-Poly1305
and RSA PKCS#1 v1.5 verification and ECDSA P-256 verification and
SCRAM-SHA-256.
"""

FILES = {
    "SECURITY.md": GOOD,
    "RELEASING.md": "## A security release\n",
    "tools/a.py": "",
    "docs/design/x.md": "# X\n\n## A heading\n",
    "crates/fennec-server/src/main.rs": '"--http-token" => 1,\n',
    "crates/fennec-shard/src/main.rs": '"--tls-ca" => 1,\n',
    "crates/fennec-cli/src/main.rs": "",
    "crates/fennec-http/src/crypto.rs":
        "fn sha256 fn hmac_sha256 fn pbkdf2_sha256 fn seal_in_place struct RsaKey\n",
    "crates/fennec-http/src/crypto/p256.rs": "fn verify_sha256\n",
    "crates/fennec-wire/src/client.rs": "pbkdf2_sha256(\n",
}


class Tree:
    """FILES with changes, written under a temporary directory."""

    def __init__(self, **changes):
        self.files = dict(FILES)
        self.files.update(changes)

    def __enter__(self):
        self.dir = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.dir.name)
        for name, text in self.files.items():
            f = root / name
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(text)
        (root / "site/content/docs").mkdir(parents=True, exist_ok=True)
        return root

    def __exit__(self, *a):
        self.dir.cleanup()


def run(**changes):
    with Tree(**changes) as root:
        return check.run(root, ())


class Holds(unittest.TestCase):
    def test_a_true_file_passes_and_counts(self):
        problems, total = run()
        self.assertEqual(problems, [])
        self.assertGreater(total, 0)


class Refused(unittest.TestCase):
    def wrong(self, text, **changes):
        problems, _ = run(**changes)
        self.assertTrue(any(text in p for p in problems), problems)

    def test_a_flag_no_parser_reads(self):
        self.wrong("--gone", **{"SECURITY.md": GOOD + "\nUse --gone.\n"})

    def test_a_flag_only_in_a_message_is_not_read(self):
        self.wrong("--tls-ca", **{"crates/fennec-shard/src/main.rs": 'eprintln!("--tls-ca");\n'})

    def test_a_missing_path(self):
        self.wrong("tools/nothing.py", **{"SECURITY.md": GOOD + "\n`tools/nothing.py`\n"})

    def test_a_missing_anchor(self):
        self.wrong("has no `gone`", **{"SECURITY.md": GOOD + "\n[x](docs/design/x.md#gone)\n"})

    def test_a_missing_own_heading(self):
        self.wrong("a heading it has not", **{"SECURITY.md": GOOD + "\n[x](#nothing)\n"})

    def test_a_primitive_whose_function_is_gone(self):
        self.wrong("fn hmac_sha256", **{"crates/fennec-http/src/crypto.rs": "fn sha256\n"})

    def test_a_primitive_the_file_stopped_naming(self):
        self.wrong("does not name SCRAM-SHA-256",
                   **{"SECURITY.md": GOOD.replace("SCRAM-SHA-256", "SCRAM")})

    def test_a_stale_claim_in_the_file(self):
        self.wrong("SECURITY.md:", **{"SECURITY.md": GOOD + "\nThere is no TLS.\n"})

    def test_a_stale_claim_in_a_comment(self):
        self.wrong("crates/fennec-http/src/lib.rs:1",
                   **{"crates/fennec-http/src/lib.rs": "// Since there is no TLS, x\n"})

    def test_a_stale_claim_in_a_page(self):
        self.wrong("site/content/docs/p.html", **{"site/content/docs/p.html": "<p>No TLS here</p>"})

    def test_missing_promises(self):
        self.wrong("Supported versions", **{"SECURITY.md": GOOD.replace("## Supported versions", "## Old")})
        self.wrong("acknowledgement", **{"SECURITY.md": GOOD.replace("3 working days", "a few days")})
        self.wrong("A security release", **{"RELEASING.md": ""})


class Allowed(unittest.TestCase):
    FILE = "crates/fennec-http/src/a.rs"
    ALLOW = ((FILE, "this build has no TLS", "true of a build without the feature"),)

    def test_an_allowed_line_passes(self):
        with Tree(**{self.FILE: "// this build has no TLS\n"}) as root:
            problems, _ = check.check_stale(root, self.ALLOW)
            self.assertEqual(problems, [])

    def test_an_allowance_that_finds_nothing_is_refused(self):
        with Tree() as root:
            problems, _ = check.check_stale(root, self.ALLOW)
            self.assertTrue(any("finds nothing" in p for p in problems), problems)

    def test_an_allowance_does_not_cover_another_line_of_the_file(self):
        text = "// this build has no TLS\n// and no TLS here either\n"
        with Tree(**{self.FILE: text}) as root:
            problems, _ = check.check_stale(root, self.ALLOW)
            self.assertTrue(any("a.rs:2" in p for p in problems), problems)


class Counts(unittest.TestCase):
    def test_a_file_naming_nothing_counts_zero_flags(self):
        with Tree() as root:
            self.assertEqual(check.check_flags(root, "", None)[1], 0)


class Repository(unittest.TestCase):
    def test_the_repositorys_own_file(self):
        problems, total = check.run(check.ROOT)
        self.assertEqual(problems, [])
        self.assertGreater(total, 0)


if __name__ == "__main__":
    unittest.main()
