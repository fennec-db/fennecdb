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
Own SHA-256 and HMAC-SHA-256 and PBKDF2-HMAC-SHA-256 and ChaCha20-Poly1305 and HChaCha20
and RSA PKCS#1 v1.5 verification and ECDSA P-256 verification and
SCRAM-SHA-256.

No outside audit has been made ([the decision](docs/design/security.md#an-outside-audit)).
"""


def row(i, enforced="`crates/fennec-http/src/guard.rs::Guard::check`",
        proved="`crates/fennec-http/tests/guard.rs::a_guard_refuses`", link=None):
    return f"| {i} | [Boundary {i}](#{link or f'boundary-{i}'}) | a peer | {enforced} | {proved} |\n"


def threat(rows=None, extra=""):
    """A threat model of 15 rows, `rows` (a dict by number) changing some."""
    rows = rows or {}
    table = "| # | Boundary | Attacker | Enforced by | Proved by |\n|---|---|---|---|---|\n"
    table += "".join(rows.get(i, row(i)) for i in range(1, 16))
    sections = "".join(f"\n### Boundary {i}\n\nText.\n" for i in range(1, 16))
    return ("# Security\n\n## The boundaries\n\n" + table + sections
            + "\n## An outside audit\n\nfennec-server's `--http-token`, "
            "[the policy](../../SECURITY.md#supported-versions).\n" + extra)


GUARD = """pub struct Guard;
impl Guard {
    pub fn check(&self) {}
}
pub fn refuse() {}

#[cfg(test)]
mod tests {
    #[test]
    fn inline_one() {}
    fn only_in_tests() {}
}
"""

GUARD_TESTS = """#[test]
fn a_guard_refuses() {}

#[test]
// A comment between.
#[should_panic]
fn a_guard_panics() {}

#[test]
#[ignore]
fn an_ignored_one() {}
"""

CRYPTO = ("pub fn sha256(a) pub fn hmac_sha256(a) pub fn pbkdf2_sha256(a) "
          "pub fn seal_in_place(a) pub fn hchacha20(a) pub struct RsaKey\n")

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
    "docs/design/security.md": threat(),
    "crates/fennec-http/Cargo.toml": "[package]\nautotests = false\n",
    "crates/fennec-http/src/guard.rs": GUARD,
    "crates/fennec-http/tests/all.rs": "mod guard;\n",
    "crates/fennec-http/tests/guard.rs": GUARD_TESTS,
    "crates/fennec-http/tests/loose.rs": "#[test]\nfn never_built() {}\n",
    "web/x.test.js": "test('a page is refused', () => {});\nfunction helper() {}\n",
    "web/x.js": "export async function persist(db) {}\nclass Box {\n  async #seal(b) {\n  }\n}\n",
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
        self.assertEqual(counts["boundaries"], 15)
        self.assertEqual(counts["enforcing items"], 15)
        self.assertEqual(counts["cited tests"], 15)

    def test_every_kind_of_item_and_test_is_read(self):
        # A method by its type, a function, a test module's test, a test
        # with attributes and a comment before its `fn`, a JS test by its
        # title, and a script's function and private method.
        rows = {
            2: row(2, "`crates/fennec-http/src/guard.rs::refuse`",
                   "`crates/fennec-http/src/guard.rs::inline_one`"),
            3: row(3, "`crates/fennec-http/src/guard.rs::Guard`",
                   "`crates/fennec-http/tests/guard.rs::a_guard_panics`"),
            4: row(4, "`web/x.js::persist` and `web/x.js::seal`", "`web/x.test.js::a page is refused`"),
        }
        problems, counts = run(**{"docs/design/security.md": threat(rows)})
        self.assertEqual(problems, [])
        self.assertEqual(counts["enforcing items"], 16)

    def test_a_row_saying_no_test_passes_and_is_named(self):
        notes = []
        with Tree(**{"docs/design/security.md": threat({5: row(5, proved="no test: a finding")})}) as root:
            problems, _ = check.run(root, (), notes)
        self.assertEqual(problems, [])
        self.assertEqual(notes, ["Boundary 5"])


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

    def test_the_audit_decision_missing_or_reversed(self):
        self.wrong("no outside audit has been made",
                   **{"SECURITY.md": GOOD.replace("No outside audit has been made", "Audits")})
        self.wrong("does not link docs/design/security.md#an-outside-audit",
                   **{"SECURITY.md": GOOD.replace("#an-outside-audit", "")})
        self.wrong("never the reverse", **{"SECURITY.md": GOOD + "\nThe code has been audited.\n"})
        self.wrong("never the reverse", **{"SECURITY.md": GOOD + "\nIt was audited by a firm.\n"})


class ThreatModel(unittest.TestCase):
    """docs/design/security.md's table held to the tree."""

    def wrong(self, text, rows=None, extra="", **changes):
        changes.setdefault("docs/design/security.md", threat(rows, extra))
        problems, _ = run(**changes)
        self.assertTrue(any(text in p for p in problems), problems)

    def test_a_renamed_function(self):
        self.wrong("no item `Guard::check` in crates/fennec-http/src/guard.rs",
                   **{"crates/fennec-http/src/guard.rs": GUARD.replace("fn check(", "fn verify(")})

    def test_a_function_left_only_in_the_test_module(self):
        self.wrong("(outside its test module)",
                   {1: row(1, "`crates/fennec-http/src/guard.rs::only_in_tests`")})

    def test_a_function_a_cfg_test_is_on_outside_the_test_module(self):
        guard = GUARD.replace("pub fn refuse() {}", "#[cfg(test)]\npub fn refuse() {}")
        self.wrong("(outside its test module)", {1: row(1, "`crates/fennec-http/src/guard.rs::refuse`")},
                   **{"crates/fennec-http/src/guard.rs": guard})

    def ghost(self, text, cite="`crates/fennec-http/src/guard.rs::ghost`", owner=False):
        """`text` in guard.rs, its only mention of the cited name: refused."""
        guard = GUARD.replace("pub fn refuse() {}", "pub fn refuse() {}\n" + text)
        self.wrong("defines no" if owner else "no item", {1: row(1, cite)},
                   **{"crates/fennec-http/src/guard.rs": guard})

    def test_a_name_only_in_a_line_comment_is_no_declaration(self):
        self.ghost("// fn ghost(x) {}\n")
        self.ghost("/// ```\n/// pub struct ghost;\n/// ```\npub fn documented() {}\n")
        # The comment a declaration once was, renamed below it.
        self.ghost("// fn ghost(x) {}\nfn real(x) {}\n")

    def test_a_name_only_in_a_block_comment_is_no_declaration(self):
        self.ghost("/*\n}\nfn ghost() {}\n*/\n")
        # Nested, as Rust's are: the first `*/` does not end it.
        self.ghost("/* outer /* inner */\n}\npub fn ghost() {}\n*/\n")

    def test_a_name_only_in_a_string_is_no_declaration(self):
        self.ghost('pub fn f() { let s = "struct ghost"; }\n')
        self.ghost('pub const T: &str = "x;\\nfn ghost() {}";\n')
        self.ghost('pub const R: &str = r#"\n";\nfn ghost() {} "\n"#;\n')
        self.ghost("pub fn c() -> char { '\"' }\npub const S: &str = \"\n}\nfn ghost() {}\n\";\n")

    def test_a_name_only_in_a_type_is_no_declaration(self):
        self.ghost("pub fn p(x: *const ghost, y: &'static ghost) {}\n")
        self.ghost("pub fn n<const ghost: usize>() {}\n")
        self.ghost("pub fn g() -> impl Ghost {}\npub fn h(x: &mut impl Ghost) {}\n",
                   cite="`crates/fennec-http/src/guard.rs::Ghost::check`", owner=True)

    def test_a_real_declaration_passes_with_its_attributes_and_visibility(self):
        decls = (
            "#[inline]\n#[must_use = \"its answer ] is the point\"]\npub(crate) const unsafe fn real_fn() {}\n"
            "/// Its doc.\n#[derive(Debug)]\npub(in crate::guard) struct RealStruct;\n"
            "pub(crate) static mut REAL_STATIC: u8 = 0;\n"
            "pub unsafe extern \"C\" fn real_extern() {}\n"
            "impl<'a> From<&'a str> for RealStruct { fn from(_: &'a str) -> Self { RealStruct } }\n"
            # A lifetime is not a character: read as one, `'a>(s: &'` would
            # be a literal, and the `\"` of `'\"'` would open a string over
            # the declaration after it.
            "pub fn life<'a>(s: &'a str) -> &'a str { s }\npub fn quote() -> char { '\"' }\n"
            "pub fn after_lifetimes() {}\n"
            "macro_rules! real_macro { () => {} }\n")
        guard = GUARD.replace("pub fn refuse() {}", "pub fn refuse() {}\n// fn ghost()\n" + decls)
        cites = ", ".join(f"`crates/fennec-http/src/guard.rs::{n}`" for n in (
            "real_fn", "RealStruct", "REAL_STATIC", "real_extern", "after_lifetimes", "real_macro",
            "RealStruct::from"))
        problems, _ = run(**{"crates/fennec-http/src/guard.rs": guard,
                             "docs/design/security.md": threat({1: row(1, cites)})})
        self.assertEqual(problems, [])

    def test_a_test_only_in_a_comment_or_a_string_is_none(self):
        tests = GUARD_TESTS.replace("#[test]\nfn a_guard_refuses() {}\n",
                                    "// #[test]\n// fn a_guard_refuses() {}\n"
                                    "const T: &str = \"#[test]\nfn a_guard_refuses() {}\";\n")
        self.wrong("no test `a_guard_refuses`", **{"crates/fennec-http/tests/guard.rs": tests})

    def test_a_test_whose_attribute_holds_a_bracket_in_a_string(self):
        tests = GUARD_TESTS + '\n#[test]\n#[should_panic(expected = "index [0] ]")]\nfn a_bracket() {}\n'
        problems, _ = run(**{"crates/fennec-http/tests/guard.rs": tests,
                             "docs/design/security.md": threat(
                                 {1: row(1, proved="`crates/fennec-http/tests/guard.rs::a_bracket`")})})
        self.assertEqual(problems, [])

    def test_a_method_of_a_type_the_file_does_not_have(self):
        self.wrong("defines no `Other`", {1: row(1, "`crates/fennec-http/src/guard.rs::Other::check`")})

    def test_a_file_that_is_gone(self):
        self.wrong("crates/fennec-http/src/gone.rs is not in the repository",
                   {1: row(1, "`crates/fennec-http/src/gone.rs::check`")})

    def test_a_deleted_test(self):
        self.wrong("no test `a_guard_refuses` in crates/fennec-http/tests/guard.rs",
                   **{"crates/fennec-http/tests/guard.rs": GUARD_TESTS.replace("a_guard_refuses", "other")})

    def test_a_function_that_is_no_test(self):
        # A plain function of the cited name is not a test.
        self.wrong("no test `a_guard_refuses`",
                   **{"crates/fennec-http/tests/guard.rs": "fn a_guard_refuses() {}\n"})

    def test_an_ignored_test(self):
        self.wrong("#[ignore]d test", {1: row(1, proved="`crates/fennec-http/tests/guard.rs::an_ignored_one`")})

    def test_a_test_in_a_file_nothing_builds(self):
        self.wrong("is not built", {1: row(1, proved="`crates/fennec-http/tests/loose.rs::never_built`")})

    def test_a_js_test_renamed(self):
        self.wrong("no test `a page is refused` in web/x.test.js",
                   {1: row(1, proved="`web/x.test.js::a page is refused`")},
                   **{"web/x.test.js": "test('a page is let in', () => {});\n"})

    def test_a_js_helper_is_no_test_where_the_file_has_tests(self):
        self.wrong("no test `helper`", {1: row(1, proved="`web/x.test.js::helper`")})

    def test_a_row_without_a_test(self):
        self.wrong("cites no test and does not say \"no test\"", {7: row(7, proved="see the docs")})

    def test_a_row_without_an_enforcing_item(self):
        self.wrong("names no enforcing item", {7: row(7, enforced="the server")})

    def test_a_row_linking_no_section(self):
        self.wrong("links no section of the file", {3: row(3, link="nowhere")})

    def test_fewer_rows_than_the_plan_asks(self):
        short = "\n".join(line for line in threat().splitlines() if not line.startswith("| 15 "))
        self.wrong("14 boundaries, fewer than 15", **{"docs/design/security.md": short})

    def test_no_table(self):
        self.wrong("has no table of boundaries", **{"docs/design/security.md": "# Security\n"})

    def test_a_link_read_from_its_folder(self):
        self.wrong("has no `gone`", extra="[x](../../SECURITY.md#gone)\n")
        self.wrong("names access.md, which is not in the repository", extra="[x](access.md)\n")

    def test_a_flag_no_parser_reads(self):
        self.wrong("docs/design/security.md names --gone", extra="fennec-server's --gone.\n")


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
