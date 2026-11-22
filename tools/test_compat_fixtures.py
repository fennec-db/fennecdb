#!/usr/bin/env python3
"""tools/compat_fixtures.py writes a release's fixture set only with that
release's programs (docs/design/compatibility.md, *Fixture sets*): `write`
holds the directory `build` filled to its MANIFEST.json before any program
of it runs -- every file named there by its SHA-256 and none other, the
tag and its commit, the compiler the tag pins, every file taken from the
tag's tree the tag's own bytes, every program the set needs built -- and
refuses a set named other than its tag or a further set of it
(`<tag>+<n>`, n from 2). Run against a repository of its own holding a
tag, so it needs no tag of this clone's (CI checks out none)."""

import hashlib
import importlib.util
import json
import os
import pathlib
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("compat_fixtures", HERE / "compat_fixtures.py")
cf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cf)

TAG = "v9.9.9"


def git(repo, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return subprocess.run(["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
                          check=True, capture_output=True, text=True, env=env).stdout


def refused(f, *args):
    """What `f` died saying, or None where it went on."""
    try:
        f(*args)
    except SystemExit as e:
        return str(e.code)
    return None


class Scratch:
    """A repository tagged `TAG` and a directory of programs `build` would
    have made from it, with their manifest; `ROOT` pointed at the
    repository while it lasts."""

    def __enter__(self):
        self.dir = tempfile.TemporaryDirectory()
        d = pathlib.Path(self.dir.name)
        self.repo, self.bins = d / "repo", d / "bins"
        tree = {
            "rust-toolchain.toml": b'[toolchain]\nchannel = "1.99.1"\n',
            "web/fennec.js": b"export const tag = 'v9.9.9'\n",
            "crates/fennec-core/src/tz/zones.bin": b"zones",
            "integrations/cloudflare/index.js": b"export function persist() {}\n",
        }
        for name, data in tree.items():
            (self.repo / name).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / name).write_bytes(data)
        git(self.repo, "init", "-q")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "a release")
        git(self.repo, "tag", TAG)
        self.commit = git(self.repo, "rev-parse", "HEAD").strip()
        # A program that says another version: one run past a skipped
        # check refuses in other words than the check's.
        built = {name: b"#!/bin/sh\necho 'fennec 0.0.0'\n" for name in ("fennec-server", "fennec", "compat_writer")}
        built["web/fennec.wasm"] = b"\0asm"
        kept = {"web/fennec.js": tree["web/fennec.js"], "web/collate/zones.bin": b"zones",
                "cloudflare/index.js": tree["integrations/cloudflare/index.js"]}
        for name, data in {**built, **kept}.items():
            (self.bins / name).parent.mkdir(parents=True, exist_ok=True)
            (self.bins / name).write_bytes(data)
            os.chmod(self.bins / name, 0o755)
        self.manifest = {
            "tag": TAG, "commit": self.commit, "toolchain": "1.99.1",
            "rustc": "rustc 1.99.1 (58cf8f9c3 2026-10-13)", "cargo": "cargo 1.99.1 (5f94df478 2026-08-27)",
            "cargo_home": "/home/t/.cargo", "sdk": "macOS SDK 26.2, ld 1230.1",
            "tree": "/private/tmp/fennec-compat-build-v9.9.9/src",
            "files": {name: hashlib.sha256(data).hexdigest() for name, data in {**built, **kept}.items()},
        }
        self.save()
        self.root, cf.ROOT = cf.ROOT, self.repo
        return self

    def save(self):
        (self.bins / "MANIFEST.json").write_text(json.dumps(self.manifest, indent=1))

    def put(self, name, data):
        """`name` in the programs' directory as `data`, the manifest naming
        the new bytes: the manifest whole, the file another one."""
        (self.bins / name).write_bytes(data)
        self.manifest["files"][name] = hashlib.sha256(data).hexdigest()
        self.save()

    def __exit__(self, *exc):
        cf.ROOT = self.root
        self.dir.cleanup()


class Manifest(unittest.TestCase):
    def test_a_whole_directory_is_taken(self):
        with Scratch() as s:
            self.assertEqual(cf.checked(s.bins, TAG)["commit"], s.commit)

    def test_write_refuses_a_program_not_the_manifest_s_before_it_runs(self):
        with Scratch() as s:
            (s.bins / "fennec-server").write_bytes(b"#!/bin/sh\necho 'fennec 9.9.9'\n")
            out = pathlib.Path(s.dir.name) / "sets" / TAG
            why = refused(cf.write, s.bins, out)
            self.assertIn("fennec-server: not the bytes the manifest names", why or "")
            self.assertFalse(out.exists() and any(out.iterdir()), "files written by a refused write")

    def test_each_refusal(self):
        cases = [
            ("a file beside the manifest's", lambda s: (s.bins / "extra").write_bytes(b"x"),
             "extra: not in the manifest"),
            ("a program missing", lambda s: (s.bins / "fennec").unlink(), "fennec: missing"),
            ("the web client not the tag's", lambda s: s.put("web/fennec.js", b"export const tag = 'main'\n"),
             f"web/fennec.js: not {TAG}'s web/fennec.js"),
            ("a collation not the tag's", lambda s: s.put("web/collate/zones.bin", b"other"),
             f"web/collate/zones.bin: not {TAG}'s crates/fennec-core/src/tz/zones.bin"),
            ("the module not the tag's", lambda s: s.put("cloudflare/index.js", b"//\n"),
             f"cloudflare/index.js: not {TAG}'s integrations/cloudflare/index.js"),
            ("another compiler", lambda s: (s.manifest.update(rustc="rustc 1.100.0 (x 2026-11-27)"), s.save()),
             "not the pinned rustc 1.99.1"),
            ("another commit", lambda s: (s.manifest.update(commit="0" * 40), s.save()),
             f"built from commit {'0' * 40}"),
            ("another tag", lambda s: (s.manifest.update(tag="v9.9.8"), s.save()), "built from v9.9.8"),
            ("the writer not built", lambda s: ((s.bins / "compat_writer").unlink(),
                                                s.manifest["files"].pop("compat_writer"), s.save()),
             "compat_writer: not built"),
            # What the programs' digests are its build's alone by.
            *[(f"no {k}", lambda s, k=k: (s.manifest.pop(k), s.save()), f"no `{k}`")
              for k in ("cargo_home", "sdk", "tree")],
        ]
        for what, change, words in cases:
            with self.subTest(what), Scratch() as s:
                change(s)
                why = refused(cf.checked, s.bins, TAG)
                self.assertIn(words, why or "", f"{what}: {why}")

    def test_a_set_is_named_by_its_tag_or_a_further_set_of_it(self):
        with Scratch() as s:
            sets = pathlib.Path(s.dir.name) / "sets"
            for name in ("v9.9.9+1", "v9.9.9+02", "v9.9.9-rc.1", "9.9.9", "v9.9.9+x"):
                why = refused(cf.write, s.bins, sets / name)
                self.assertIn("a set is named by its tag", why or "", f"{name}: {why}")
            # A further set is held to its tag's programs as the first is.
            (s.bins / "fennec").write_bytes(b"changed")
            why = refused(cf.write, s.bins, sets / "v9.9.9+2")
            self.assertIn("fennec: not the bytes the manifest names", why or "")


class Forms(unittest.TestCase):
    """The README's count reads the set's `RELEASES` line to its element's
    close however rustfmt lays the list out, and no further."""

    LINE = """    set: "{set}",
    tag: "v9.9.9",
    sums: "00",
    kinds: &[1, 2],
    ops: &[1],
    graphs: &[9],
    schema_tags: &[1],
    value_tags: &[0],
    indexes: &[0],
    shapes: SHAPES,
    absent: &[
        (
            "claim: 3 bytes",
            "why",
        ),
    ],"""
    REST = """
const SHAPES: &[&str] = &[
    "front: records",
    "frame: put",
];

impl<'a> Cur<'a> {
    pub fn new(b: &'a [u8]) -> Cur<'a> {
        Cur { b, at: 0, absent: &[("x", "y")] }
    }
}
"""

    def test_one_line_and_several(self):
        one = "pub const RELEASES: &[Writes] = &[Writes {\n" + self.LINE.format(set="v9.9.9") + "\n}];\n" + self.REST
        indent = lambda s: "\n".join("    " + l for l in s.splitlines())
        several = ("pub const RELEASES: &[Writes] = &[\n    Writes {\n" + indent(self.LINE.format(set="v9.9.9"))
                   + "\n    },\n    Writes {\n" + indent(self.LINE.format(set="v9.9.9+2")) + "\n    },\n];\n"
                   + self.REST)
        self.assertEqual(cf.forms("v9.9.9", one), (2, 1, 7))
        self.assertEqual(cf.forms("v9.9.9", several), (2, 1, 7))
        self.assertEqual(cf.forms("v9.9.9+2", several), (2, 1, 7))

    def test_the_tree_s_line(self):
        held, absent, numbers = cf.forms("v0.2.0")
        self.assertGreater(held, 0)
        self.assertGreater(numbers, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
