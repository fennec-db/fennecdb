#!/usr/bin/env python3
"""tools/toolchain_check.py: each way a toolchain file, a Dockerfile, a
script's image or a toolchain it names to rustup leaves the compiler CI
tests with, refused by file and line, and what pins it, passed; then the
repository's own files.

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


class Scripts(unittest.TestCase):
    def test_the_release_by_digest_passes(self):
        text = (f'docker run --rm -v "$ROOT":/src \\\n  rust:1.99.1-slim-bookworm{D} \\\n  cargo build\n'
                f"image: docker.io/library/rust:1.99.1-alpine{D}\n")
        self.assertEqual(check.script_problems("run.sh", text, "1.99.1"), [])

    def test_another_release_or_no_digest_is_refused(self):
        for ref in ("rust:1-slim-bookworm", "rust:alpine", "rust:latest", "rust:1.99.1-slim-bookworm",
                    f"rust:1.98.0-slim-bookworm{D}", f"library/rust:1.100.0{D}"):
            got = check.script_problems("run.sh", f"docker run --rm {ref} cargo -V\n", "1.99.1")
            self.assertEqual(len(got), 1, ref)
            self.assertIn(f"run.sh:1: {ref.split('@')[0]}", got[0])

    def test_what_names_no_image_passes(self):
        text = ("# rust:1-slim-bookworm built it before the pin\n"
                "  --rust: #9c3f1c;\n"
                '  rust: "#9c4a26",\n'
                '    println!("rust: ok");\n'
                "if not triple and not rust:\n"
                "trust:1 and crust:2\n")
        self.assertEqual(check.script_problems("x.py", text, "1.99.1"), [])


class Toolchains(unittest.TestCase):
    def test_another_release_named_to_rustup_is_refused_by_file_and_line(self):
        text = ("env:\n"
                "  RUSTUP_TOOLCHAIN: 1.99.1\n"
                "jobs:\n"
                "      - run: |\n"
                "          rustup toolchain install 1.99.1 --profile minimal --target x86_64-unknown-linux-musl\n"
                "          rustup default stable\n"
                "          RUSTUP_TOOLCHAIN='1.100' cargo build\n"
                "          rustup install beta && cargo -V\n"
                "          rustup toolchain install 1.100.0 nightly --profile minimal\n")
        got = check.toolchain_problems(".github/workflows/x.yml", text, "1.100.0")
        self.assertEqual([g.split(": ", 1)[0] for g in got],
                         [".github/workflows/x.yml:%d" % n for n in (2, 5, 6, 7, 8, 9)], got)
        self.assertIn("RUSTUP_TOOLCHAIN names 1.99.1: rust-toolchain.toml names 1.100.0", got[0])
        self.assertIn("rustup toolchain install names 1.99.1", got[1])
        self.assertIn("rustup default names stable", got[2])
        self.assertIn("RUSTUP_TOOLCHAIN names 1.100:", got[3])
        self.assertIn("rustup install names beta", got[4])
        self.assertIn("rustup toolchain install names nightly:", got[5])

    def test_the_release_a_dated_nightly_and_what_the_check_cannot_read_pass(self):
        text = ("env:\n"
                '  RUSTUP_TOOLCHAIN: "1.100.0"\n'
                "          rustup toolchain install 1.100.0 --profile minimal --target x86_64-unknown-linux-musl\n"
                "          rustup toolchain install 1.100.0-x86_64-unknown-linux-gnu\n"
                "          rustup toolchain install nightly-2026-11-01 --profile minimal\n"
                '          rustup toolchain install "$base" --profile minimal --target x86_64-unknown-linux-musl\n'
                "\trustup toolchain install $(FUZZ_NIGHTLY) --profile minimal\n"
                "  sh -c 'export RUSTUP_TOOLCHAIN=$(ls /usr/local/rustup/toolchains | head -1) &&\n"
                '        env["RUSTUP_TOOLCHAIN"] = channel\n'
                "  RUSTUP_TOOLCHAIN: ${{ matrix.toolchain }}\n"
                "          rustup toolchain install\n"
                "          rustup target add x86_64-pc-windows-msvc 1.99.1\n"
                "# RUSTUP_TOOLCHAIN: 1.99.1 built it before the pin; rustup default stable\n"
                "          cargo -V  # rustup default stable would move it\n")
        self.assertEqual(check.toolchain_problems("x.yml", text, "1.100.0"), [])


class Committed(unittest.TestCase):
    def test_the_repository_passes(self):
        want, why = check.release((check.ROOT / "rust-toolchain.toml").read_text())
        self.assertIsNone(why)
        for path in check.dockerfiles():
            text = (check.ROOT / path).read_text()
            self.assertEqual(check.problems(path, text, want, path in check.PINNED), [], path)
        self.assertTrue(set(check.PINNED) <= set(check.dockerfiles()))
        scripts = check.scripts()
        for path in ("integrations/kotlin/run-tests.sh", "examples/kotlin-android/run-smoke.sh",
                     "tools/stress-test.sh", ".github/workflows/perf-holdout.yml"):
            self.assertIn(path, scripts)
        for path in scripts:
            text = (check.ROOT / path).read_text(encoding="utf-8", errors="replace")
            self.assertEqual(check.script_problems(path, text, want), [], path)
            self.assertEqual(check.toolchain_problems(path, text, want), [], path)


if __name__ == "__main__":
    unittest.main()
