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

CRYPTO = ("pub fn sha256(a) pub fn hmac_sha256(a) pub fn pbkdf2_sha256(a) "
          "pub fn seal_in_place(a) pub struct RsaKey\n")

FILES = {
    "SECURITY.md": GOOD,
    "RELEASING.md": "## A security release\n",
    "tools/a.py": "",
    "docs/design/x.md": "# X\n\n## A heading\n",
    "crates/fennec-server/src/main.rs": '"--http-token" => 1,\n',
    "crates/fennec-shard/src/main.rs": '"--tls-ca" => 1,\n',
    "crates/fennec-cli/src/main.rs": "",
    "crates/fennec-http/src/crypto.rs": CRYPTO,
    "crates/fennec-http/src/crypto/p256.rs": "pub fn verify_sha256(\n",
    "crates/fennec-wire/src/client.rs": "    fn scram(&mut self, p: &str) {}\n",
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
        problems, counts = run()
        self.assertEqual(problems, [])
        self.assertTrue(all(n > 0 for n in counts.values()), counts)


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
        self.wrong("pub fn hmac_sha256(",
                   **{"crates/fennec-http/src/crypto.rs": CRYPTO.replace("pub fn hmac_sha256(", "fn other(")})

    def test_a_primitive_renamed_but_still_a_test_function(self):
        # `fn sha256_vectors` and a test's `fn sha256(` in the test module are not it.
        tests = "#[cfg(test)]\nmod tests {\n    pub fn sha256(a) pub fn pbkdf2_sha256(a)\n}\n"
        text = CRYPTO.replace("pub fn sha256(", "pub fn renamed(").replace("pub fn pbkdf2_sha256(", "pub fn x(")
        self.wrong("pub fn sha256(", **{"crates/fennec-http/src/crypto.rs": text + tests})
        self.wrong("pub fn pbkdf2_sha256(", **{"crates/fennec-http/src/crypto.rs": text + tests})

    def test_a_flag_another_binary_reads_is_not_the_one_meant(self):
        # The router's `--token` is fennec-cli's `backup` flag, not fennec-shard's.
        self.wrong("--token", **{
            "SECURITY.md": GOOD + "\nThe router's `--token` is checked.\n",
            "crates/fennec-cli/src/main.rs": '"--token" => 1,\n'})
        problems, _ = run(**{"SECURITY.md": GOOD + "\nThe router's `--token` is checked.\n",
                             "crates/fennec-shard/src/main.rs": '"--tls-ca" => 1, "--token" => 1,\n'})
        self.assertEqual(problems, [])

    def test_a_flag_in_a_comment_or_a_test_is_not_read(self):
        self.wrong("--tls-ca", **{"crates/fennec-shard/src/main.rs": '// "--tls-ca" => 1,\n'})
        self.wrong("--tls-ca", **{"crates/fennec-shard/src/main.rs":
                                  '#[cfg(test)]\nmod tests {\n    "--tls-ca" => 1,\n}\n'})

    def test_a_file_without_flags_paths_or_links_is_refused_though_files_are_scanned(self):
        bare = "# Security policy\n\nAcknowledged within 3 working days.\n\n## Supported versions\n\n" \
               "Own SHA-256 and HMAC-SHA-256 and PBKDF2-HMAC-SHA-256 and ChaCha20-Poly1305 and RSA " \
               "PKCS#1 v1.5 verification and ECDSA P-256 verification and SCRAM-SHA-256.\n"
        problems, counts = run(**{"SECURITY.md": bare})
        self.assertEqual(counts["flags"], 0)
        self.assertEqual(counts["paths and anchors"], 0)
        self.assertGreater(counts["files searched"], 0)
        self.assertTrue(any("checked no flags" in p for p in problems), problems)
        self.assertTrue(any("checked no paths and anchors" in p for p in problems), problems)

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
            self.assertEqual(check.check_flags(root, "", None)[0], [])


class Repository(unittest.TestCase):
    def test_the_repositorys_own_file(self):
        problems, counts = check.run(check.ROOT)
        self.assertEqual(problems, [])
        self.assertTrue(all(n > 0 for n in counts.values()), counts)


if __name__ == "__main__":
    unittest.main()
