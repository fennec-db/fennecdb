#!/usr/bin/env python3
"""tools/lints_check.py: each way an `unsafe` block or `unsafe impl` goes
without saying why it is sound, refused whatever `cfg` it is under, and
each place clippy reads a `// SAFETY:` comment from, passed; then the
repository's own files.

    python3 tools/test_lints_check.py
"""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("lints_check", os.path.join(HERE, "lints_check.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def refused(src):
    return check.findings(src)[0]


class Refused(unittest.TestCase):
    def test_a_block_with_no_comment(self):
        self.assertEqual(refused("fn f(p: *const u8) -> u8 {\n    unsafe { *p }\n}\n"),
                         [(2, "an `unsafe` block")])

    def test_an_impl_with_no_comment(self):
        self.assertEqual(refused("struct X;\nunsafe impl Send for X {}\n"), [(2, "an `unsafe impl`")])
        self.assertEqual(refused("// X is plain data.\n#[cfg(unix)]\nunsafe impl<T> Sync for X<T> {}\n"),
                         [(3, "an `unsafe impl`")])

    def test_a_block_under_a_cfg_no_ci_run_compiles(self):
        src = (
            "#[cfg(windows)]\nmod w {\n    #[cfg(target_arch = \"aarch64\")]\n"
            "    pub fn f(p: *const u8) -> u8 {\n        unsafe { *p }\n    }\n}\n"
        )
        self.assertEqual(refused(src), [(5, "an `unsafe` block")])

    def test_an_operation_in_an_unsafe_fn_body(self):
        src = (
            "/// # Safety\n/// `p` is valid.\npub unsafe fn f(p: *const u8) -> u8 {\n"
            "    let x = 1;\n    unsafe { *p.add(x) }\n}\n"
        )
        self.assertEqual(refused(src), [(5, "an `unsafe` block")])

    def test_a_safety_heading_is_no_safety_comment(self):
        src = "/// # Safety\n/// `p` is valid.\nfn f(p: *const u8) -> u8 {\n    unsafe { *p }\n}\n"
        self.assertEqual(len(refused(src)), 1)

    def test_a_comment_with_code_between(self):
        self.assertEqual(len(refused("fn f() {\n    // SAFETY: nothing.\n    let a = 1;\n    unsafe { g(a) }\n}\n")), 1)
        # A comment after code on the line above is not above the block.
        self.assertEqual(len(refused("fn f() {\n    g(); // SAFETY: nothing.\n    unsafe { h() }\n}\n")), 1)

    def test_a_comment_above_a_call_is_not_its_argument_s(self):
        src = "fn f() {\n    // SAFETY: nothing.\n    g(1,\n        unsafe { h() });\n}\n"
        self.assertEqual(refused(src), [(4, "an `unsafe` block")])

    def test_a_comment_above_an_expression_it_is_part_of(self):
        src = "fn f() {\n    // SAFETY: nothing.\n    let x = 1\n        + unsafe { h() };\n}\n"
        self.assertEqual(len(refused(src)), 1)

    def test_a_block_on_its_function_s_first_line(self):
        # Clippy reads a comment inside the body, above the block's line.
        src = "static mut P: u8 = 0;\n// SAFETY: one thread.\nfn f() -> u8 { unsafe { P } }\n"
        self.assertEqual(refused(src), [(3, "an `unsafe` block")])
        src = "// SAFETY: none.\nfn f() -> u8 { let x = unsafe { g() }; x }\n"
        self.assertEqual(refused(src), [(2, "an `unsafe` block")])

    def test_each_block_needs_its_own(self):
        src = "fn f() {\n    // SAFETY: the first.\n    unsafe { a() };\n    unsafe { b() };\n}\n"
        self.assertEqual(refused(src), [(4, "an `unsafe` block")])

    def test_the_masking_sees_past_lifetimes_and_quotes(self):
        src = (
            "fn f<'a>(s: &'a str) -> char {\n    let q = '\"';\n    let e = '\\'';\n"
            "    let r = r#\"unsafe { }\"#;\n    unsafe { g(s, q, e, r) }\n}\n"
        )
        self.assertEqual(refused(src), [(5, "an `unsafe` block")])


class Passed(unittest.TestCase):
    def test_a_comment_just_above(self):
        self.assertEqual(refused("fn f() {\n    // SAFETY: g has no precondition.\n    unsafe { g() }\n}\n"), [])

    def test_a_comment_of_several_lines_blank_lines_skipped(self):
        src = "fn f() {\n    // SAFETY: g has no\n    // precondition.\n\n    unsafe { g() }\n}\n"
        self.assertEqual(refused(src), [])

    def test_a_comment_above_the_let_it_is_the_value_of(self):
        self.assertEqual(refused("fn f() {\n    // SAFETY: none.\n    let x: u8 =\n        unsafe { g() };\n}\n"), [])
        self.assertEqual(refused("fn f() {\n    // SAFETY: none.\n    let p = unsafe {\n        g()\n    };\n}\n"), [])
        self.assertEqual(refused("// SAFETY: none.\npub static X: u8 = unsafe { g() };\n"), [])

    def test_a_comment_above_the_attributes(self):
        self.assertEqual(refused("// SAFETY: plain data.\n#[cfg(all(unix,\n    windows))]\nunsafe impl Send for X {}\n"), [])
        self.assertEqual(refused("fn f() {\n    // SAFETY: none.\n    #[cfg(unix)]\n    unsafe { g() };\n}\n"), [])

    def test_a_comment_on_a_match_arm_or_an_argument(self):
        src = (
            "fn f(b: bool) {\n    let x = match b {\n        true => 0,\n"
            "        // SAFETY: none.\n        false => unsafe { g() },\n    };\n"
            "    h(\n        // SAFETY: none.\n        unsafe { g() },\n    );\n}\n"
        )
        self.assertEqual(refused(src), [])

    def test_a_block_on_a_later_line_of_its_function(self):
        src = (
            "fn f(o: &std::sync::Once, b: bool) -> u8 {\n    // SAFETY: none.\n    o.call_once(|| unsafe { g() });\n"
            "    // SAFETY: none.\n    if b { unsafe { h() } } else { 0 }\n}\n"
        )
        self.assertEqual(refused(src), [])

    def test_a_code_fence_in_the_comment(self):
        src = "fn f(p: *const u8) -> u8 {\n    // SAFETY: `p` is valid, as\n    // ```\n    // let x = 1;\n    // ```\n    // says.\n    unsafe { *p }\n}\n"
        self.assertEqual(refused(src), [])

    def test_a_block_comment_and_any_case(self):
        self.assertEqual(refused("fn f() {\n    /* Safety: none. */\n    unsafe { g() }\n}\n"), [])

    def test_what_is_no_block(self):
        src = (
            "// unsafe { }\n/* unsafe { } */\nunsafe fn f() {}\nunsafe extern \"C\" {\n    fn g();\n}\n"
            "unsafe trait T {}\n#[unsafe(no_mangle)]\npub extern \"C\" fn h() { let _ = \"unsafe { }\"; }\n"
        )
        self.assertEqual(check.findings(src), ([], 0))


class Repository(unittest.TestCase):
    def test_every_block_says_why_and_every_crate_takes_the_lints(self):
        self.assertEqual(check.main(), 0)


if __name__ == "__main__":
    unittest.main()
