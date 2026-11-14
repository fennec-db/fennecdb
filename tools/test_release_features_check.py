#!/usr/bin/env python3
"""tools/release_features_check.py: each way a test build's feature could
reach a release, refused, and what CI and the chaos image do, passed; then
the repository's own files.

    python3 tools/test_release_features_check.py
"""

import importlib.util
import os
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("release_features_check",
                                              os.path.join(HERE, "release_features_check.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

HTTP = "crates/fennec-http/Cargo.toml"
SERVER = "crates/fennec-server/Cargo.toml"


def manifest(features="", deps=""):
    return f'[package]\nname = "x"\nversion = "0.1.0"\n\n[features]\n{features}\n\n[dependencies]\n{deps}\n'


class Manifests(unittest.TestCase):
    def test_the_two_definitions_pass(self):
        self.assertEqual(check.manifest_problems(HTTP, manifest('default = ["tls"]\ntls = []\ntest-clock = []')), [])
        self.assertEqual(check.manifest_problems(
            SERVER, manifest('default = ["tls"]\ntls = []\ntest-clock = ["fennec-http/test-clock"]')), [])

    def test_a_default_naming_it_is_refused(self):
        for default in ['["test-clock"]', '["tls", "fennec-http/test-clock"]', '["fennec-http?/test-clock"]']:
            got = check.manifest_problems(SERVER, manifest(f"default = {default}\ntls = []"))
            self.assertEqual(len(got), 1, default)
            self.assertIn("default features", got[0])

    def test_another_feature_turning_it_on_is_refused(self):
        got = check.manifest_problems(SERVER, manifest('chaos = ["fennec-http/test-clock"]'))
        self.assertEqual(len(got), 1)
        self.assertIn("feature `chaos`", got[0])

    def test_a_definition_elsewhere_or_otherwise_is_refused(self):
        self.assertEqual(len(check.manifest_problems("crates/fennec-cli/Cargo.toml",
                                                     manifest("test-clock = []"))), 1)
        self.assertEqual(len(check.manifest_problems(SERVER, manifest(
            'test-clock = ["fennec-http/test-clock", "tls"]\ntls = []'))), 1)

    def test_a_dependency_declared_with_it_is_refused(self):
        for deps in ['fennec-http = { path = "../fennec-http", features = ["test-clock"] }',
                     '[target.\'cfg(unix)\'.dependencies]\nfennec-http = { path = "x", features = ["test-clock"] }']:
            got = check.manifest_problems(SERVER, manifest("", deps))
            self.assertEqual(len(got), 1, deps)
            self.assertIn("declared with", got[0])
        dev = '[package]\nname = "x"\nversion = "0.1.0"\n[dev-dependencies]\nfennec-http = { path = "x", features = ["test-clock"] }\n'
        self.assertEqual(len(check.manifest_problems(SERVER, dev)), 1)


CHAOS = """\
FROM rust:1.99.1-alpine AS build
RUN tools/image-cargo.sh /built --release -p fennec-server
FROM alpine AS kit
COPY --from=build /built/ /usr/local/bin/
FROM build AS chaos-build
RUN --mount=type=cache,target=/work \\
    tools/image-cargo.sh /built-chaos --release -p fennec-server --features test-clock
FROM kit AS chaos
COPY --from=chaos-build /built-chaos/fennec-server /usr/local/bin/fennec-server
"""


class Builds(unittest.TestCase):
    def test_the_chaos_stage_and_ci_s_lint_and_test_pass(self):
        self.assertEqual(check.build_problems("deploy/Dockerfile", CHAOS), [])
        ci = ("      - run: cargo clippy -p fennec-http -p fennec-server --features test-clock --all-targets\n"
              "      - run: cargo test -p fennec-http --features test-clock --lib\n"
              "        run: cargo metadata --format-version 1 --all-features\n"
              '        ["cargo", "metadata", "--format-version", "1", "--all-features"],\n'
              "      # cargo build --features test-clock, a comment\n")
        self.assertEqual(check.build_problems(".github/workflows/ci.yml", ci), [])

    def test_a_build_naming_it_anywhere_else_is_refused(self):
        for line in ["cargo build --release -p fennec-server --features test-clock",
                     "cargo build --release -p fennec-server --features=tls,test-clock",
                     'cargo build --features "fennec-server/test-clock"',
                     "cargo build --release --all-features",
                     "docker build --build-arg FEATURES=test-clock -t fennec-server .",
                     "make docker FEATURES=timing,test-clock"]:
            got = check.build_problems(".github/workflows/release.yml", f"        run: {line}\n")
            self.assertEqual(len(got), 1, line)

    def test_another_feature_named_alike_passes(self):
        self.assertEqual(check.build_problems("Makefile", "cargo build --features test-clockwork\n"), [])

    def test_the_chaos_build_moved_to_another_stage_is_refused(self):
        moved = CHAOS.replace("FROM build AS chaos-build\n", "").replace(
            "FROM kit AS chaos\n", "FROM kit AS chaos\nRUN cargo build --features test-clock\n")
        self.assertTrue(check.build_problems("deploy/Dockerfile", moved))

    def test_a_stage_built_on_or_copying_from_the_chaos_build_is_refused(self):
        on = CHAOS + "FROM chaos AS release\n"
        self.assertEqual(len(check.build_problems("deploy/Dockerfile", on)), 1)
        copies = CHAOS.replace("COPY --from=build /built/ /usr/local/bin/",
                               "COPY --from=chaos-build /built-chaos/ /usr/local/bin/")
        got = check.build_problems("deploy/Dockerfile", copies)
        self.assertEqual(len(got), 1)
        self.assertIn("stage `kit` copies", got[0])


class Repository(unittest.TestCase):
    def test_the_repositorys_files_pass(self):
        p = subprocess.run([sys.executable, os.path.join(HERE, "release_features_check.py")],
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)


if __name__ == "__main__":
    unittest.main()
