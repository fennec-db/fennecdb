#!/usr/bin/env python3
"""tools/toolchain_check.py: each way a toolchain file or a Dockerfile
leaves the compiler CI tests with, refused, and what pins it, passed; then
the repository's own files.

    python3 tools/test_toolchain_check.py
"""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("toolchain_check", os.path.join(HERE, "toolchain_check.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

D = "@sha256:" + "ab" * 32
RUST = f"FROM rust:1.99.1-alpine{D} AS build\n"


def toml(channel):
    return f'[toolchain]\nchannel = "{channel}"\ntargets = ["wasm32-unknown-unknown"]\n'


class Release(unittest.TestCase):
    def test_an_exact_release_is_read(self):
        self.assertEqual(check.release(toml("1.99.1")), ("1.99.1", None))
        self.assertEqual(check.release('[toolchain]\nchannel = "1.100.0"  # a bump\n'),
                         ("1.100.0", None))

    def test_a_channel_that_moves_is_refused(self):
        for channel in ("stable", "beta", "nightly", "nightly-2026-11-01", "1.99", "1", ""):
            got, why = check.release(toml(channel))
            self.assertIsNone(got, channel)
            self.assertIn("X.Y.Z", why)

    def test_no_channel_or_two_is_refused(self):
        self.assertIsNone(check.release("[toolchain]\n")[0])
        self.assertIsNone(check.release('channel = "1.99.1"\nchannel = "1.99.1"\n')[0])


class Dockerfiles(unittest.TestCase):
    def test_the_release_by_digest_passes(self):
        text = RUST + "RUN cargo --version\nFROM scratch\nCOPY --from=build /a /a\n"
        self.assertEqual(check.problems("Dockerfile", text, "1.99.1", True), [])

    def test_a_stage_of_its_own_and_a_pinned_image_pass(self):
        text = RUST + f"FROM alpine:3.24.4{D} AS kit\nFROM build AS again\nFROM --platform=linux/amd64 kit\n"
        self.assertEqual(check.problems("deploy/Dockerfile", text, "1.99.1", True), [])

    def test_another_release_is_refused_naming_both(self):
        for ref in (f"rust:1.98.0-alpine{D}", f"rust:1.99.0-alpine{D}",
                    f"docker.io/library/rust:1.100.0-alpine{D}"):
            got = check.problems("Dockerfile", f"FROM {ref} AS build\n", "1.99.1", True)
            self.assertEqual(len(got), 1, ref)
            self.assertIn("rust-toolchain.toml names 1.99.1", got[0])
            self.assertIn(f"Dockerfile:1: FROM {ref}", got[0])

    def test_a_rust_image_without_a_digest_is_refused_in_any_dockerfile(self):
        for ref in ("rust:alpine", "rust:1.99.1-alpine", "rust", "rust:1-slim-bookworm",
                    f"rust:1.99.1-alpine{D}"[:-1] + "x"):
            got = check.problems("examples/x/Dockerfile", f"FROM {ref}\n", "1.99.1", False)
            self.assertEqual(len(got), 1, ref)

    def test_an_image_without_a_digest_is_refused_where_pinned(self):
        text = RUST + "FROM alpine:3 AS kit\nFROM python:3.13-slim AS systemd\n"
        got = check.problems("deploy/Dockerfile", text, "1.99.1", True)
        self.assertEqual([g.split(":")[1] for g in got], ["2", "3"])
        self.assertIn("alpine:3@sha256:", got[0])
        # Elsewhere an image other than rust's is the file's own business.
        self.assertEqual(check.problems("integrations/x/Dockerfile", "FROM node:24-alpine\n",
                                        "1.99.1", False), [])

    def test_removing_the_toolchain_file_is_refused(self):
        for run in ("RUN rm -f rust-toolchain.toml\n", "RUN rm rust-toolchain.toml && true\n",
                    "RUN apk add rsync && \\\n    rm -f /src/rust-toolchain.toml\n"):
            got = check.problems("Dockerfile", RUST + run, "1.99.1", True)
            self.assertEqual(len(got), 1, run)
            self.assertIn("removes rust-toolchain.toml", got[0])

    def test_setting_rustup_toolchain_is_refused(self):
        for line in ("ENV RUSTUP_TOOLCHAIN=1.98.0\n", "RUN RUSTUP_TOOLCHAIN=stable cargo build\n",
                     "ARG RUSTUP_TOOLCHAIN\n"):
            got = check.problems("Dockerfile", RUST + line, "1.99.1", True)
            self.assertEqual(len(got), 1, line)
            self.assertIn("RUSTUP_TOOLCHAIN", got[0])

    def test_a_comment_is_no_instruction(self):
        text = RUST + "# RUN rm -f rust-toolchain.toml, as it was; RUSTUP_TOOLCHAIN neither\n"
        self.assertEqual(check.problems("Dockerfile", text, "1.99.1", True), [])


class Committed(unittest.TestCase):
    def test_the_repository_passes(self):
        want, why = check.release((check.ROOT / "rust-toolchain.toml").read_text())
        self.assertIsNone(why)
        for path in check.dockerfiles():
            text = (check.ROOT / path).read_text()
            self.assertEqual(check.problems(path, text, want, path in check.PINNED), [], path)
        self.assertTrue(set(check.PINNED) <= set(check.dockerfiles()))


if __name__ == "__main__":
    unittest.main()
