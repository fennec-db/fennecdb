#!/usr/bin/env python3
"""A test build's features never reach a release: `python3
tools/release_features_check.py`, CI's fmt job.

`test-clock` (crates/fennec-http/src/test_clock.rs) lets a file move a
tenant node's clock, for `make chaos`, which Docker cannot do for one
container. A release carrying it would let anyone who can write a file on
the machine move every tenant's `now()` and `@ttl`. It is held out three
ways here, and two more at release time (release.yml: the image's and every
tarball's `fennec-server --version` say the version alone, never
`+test-clock`):

- in the manifests, only fennec-http (`test-clock = []`) and fennec-server
  (`test-clock = ["fennec-http/test-clock"]`) define it, no other feature
  -- `default` above all -- lists it or a dependency's, and no dependency
  is declared with it, so a build has it only by naming it;
- nothing builds with it but deploy/Dockerfile's `chaos-build` stage: every
  `--features ...test-clock`, `FEATURES=...test-clock` or `--all-features`
  outside a comment, in any file but the docs and the Rust sources, is a
  `cargo clippy`, `test`, `check`, `doc` or `metadata` -- none of which
  makes a binary anyone ships -- or a line of that stage;
- in deploy/Dockerfile only the `chaos` stage copies from `chaos-build`,
  and no stage is built on either.

    python3 tools/release_features_check.py   # exit 1, naming each problem
"""

import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent

# A test build's features, and where each may be defined, as what.
TEST = "test-clock"
DEFINED = {
    "crates/fennec-http/Cargo.toml": [],
    "crates/fennec-server/Cargo.toml": ["fennec-http/test-clock"],
}

# The one stage that builds with it, and the one that takes what it built.
CHAOS_FILE = "deploy/Dockerfile"
CHAOS_BUILD = "chaos-build"
CHAOS = "chaos"

# A line turning features on: a list naming the test feature, or all of them.
ENABLES = re.compile(
    r"--all-features\b"
    r"|--features(?:=|\s+)[\"']?[^\s\"']*\b" + re.escape(TEST) + r"\b"
    r"|\bFEATURES=[\"']?[^\s\"']*\b" + re.escape(TEST) + r"\b")
# Cargo commands that make nothing a release ships: in a shell's words or
# a program's list of them (`["cargo", "metadata", ...]`).
NOTHING_SHIPPED = re.compile(r"\bcargo[\"',\s]+(?:\+[^\"',\s]+[\"',\s]+)?"
                             r"(?:clippy|test|check|doc|metadata)\b")
FROM = re.compile(r"^\s*FROM\s+(?:--platform=\S+\s+)?(\S+)(?:\s+AS\s+(\S+))?", re.I)
COPY_FROM = re.compile(r"^\s*COPY\s+.*--from=(\S+)", re.I)

# The checker and its tests name what they look for.
SELF = {"tools/release_features_check.py", "tools/test_release_features_check.py"}


def names_test(item):
    """Whether a feature list's item turns the test feature on: its own, or
    a dependency's (`dep/test-clock`, `dep?/test-clock`)."""
    return item == TEST or item.rsplit("/", 1)[-1] == TEST


def dependency_tables(manifest):
    """Every table of dependencies a manifest holds, by where it is."""
    out = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        if key in manifest:
            out.append((key, manifest[key]))
    for target, tables in manifest.get("target", {}).items():
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            if key in tables:
                out.append((f"target.{target}.{key}", tables[key]))
    if "dependencies" in manifest.get("workspace", {}):
        out.append(("workspace.dependencies", manifest["workspace"]["dependencies"]))
    return out


def manifest_problems(path, text):
    """What a Cargo.toml at `path` holding `text` does to let the test
    feature into a build that does not name it."""
    out = []
    manifest = tomllib.loads(text)
    for name, items in manifest.get("features", {}).items():
        if name == TEST:
            want = DEFINED.get(path)
            if want is None:
                out.append(f"{path}: defines `{TEST}`, which only "
                           f"{' and '.join(sorted(DEFINED))} may")
            elif items != want:
                out.append(f"{path}: `{TEST} = {items}`, where it is `{want}`: it turns on "
                           "nothing but the test clock")
            continue
        for item in items:
            if names_test(item):
                what = "the default features" if name == "default" else f"feature `{name}`"
                out.append(f"{path}: {what} turn on `{item}`: a build would carry a test "
                           "build's clock without naming it")
    for where, deps in dependency_tables(manifest):
        for dep, spec in deps.items():
            if isinstance(spec, dict) and any(names_test(f) for f in spec.get("features", [])):
                out.append(f"{path}: [{where}] {dep} is declared with `{TEST}`: every build "
                           "of the crate would carry it")
    return out


def stages(text):
    """Each line's stage in a Dockerfile, by number, and the stages' bases."""
    at, of, bases = None, {}, []
    for n, line in enumerate(text.splitlines(), 1):
        m = FROM.match(line)
        if m:
            at = (m.group(2) or "").lower() or None
            bases.append((n, m.group(1).lower(), at))
        of[n] = at
    return of, bases


def code_lines(path, text):
    """`text`'s lines but whole-line comments, by number: a Dockerfile's,
    a shell's, a workflow's, a Makefile's and a TOML file's are `#`."""
    for n, line in enumerate(text.splitlines(), 1):
        if not line.lstrip().startswith("#"):
            yield n, line


def build_problems(path, text):
    """Where `path`, holding `text`, builds with the test feature outside
    deploy/Dockerfile's chaos-build stage."""
    out = []
    of = stages(text)[0] if path == CHAOS_FILE else {}
    for n, line in code_lines(path, text):
        if not ENABLES.search(line):
            continue
        if NOTHING_SHIPPED.search(line):
            continue
        if path == CHAOS_FILE and of.get(n) == CHAOS_BUILD:
            continue
        out.append(f"{path}:{n}: builds with `{TEST}` (or every feature), which only "
                   f"{CHAOS_FILE}'s `{CHAOS_BUILD}` stage may: {line.strip()}")
    if path == CHAOS_FILE:
        of, bases = stages(text)
        for n, base, stage in bases:
            if base in (CHAOS, CHAOS_BUILD):
                out.append(f"{path}:{n}: stage `{stage}` is built on `{base}`, whose binary is a "
                           "test build's")
        for n, line in code_lines(path, text):
            m = COPY_FROM.match(line)
            if m and m.group(1).lower() == CHAOS_BUILD and of.get(n) != CHAOS:
                out.append(f"{path}:{n}: stage `{of.get(n)}` copies from `{CHAOS_BUILD}`, which "
                           f"only `{CHAOS}` may")
    return out


def tracked():
    """Every file git tracks, by its path from the root."""
    listed = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z"], check=True,
                            capture_output=True, text=True).stdout.split("\0")
    return [p for p in listed if p]


def skipped(path):
    """The docs say what a build is; the Rust sources are what it compiles."""
    return path in SELF or path.endswith((".md", ".html", ".rs", ".json", ".svg", ".png", ".lock"))


def main(argv):
    if argv[1:]:
        sys.exit("usage: release_features_check.py")
    found = []
    for path in tracked():
        if skipped(path):
            continue
        try:
            text = (ROOT / path).read_text()
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            continue
        if path.rsplit("/", 1)[-1] == "Cargo.toml":
            found += manifest_problems(path, text)
        found += build_problems(path, text)
    for path in DEFINED:
        manifest = tomllib.loads((ROOT / path).read_text())
        if TEST not in manifest.get("features", {}):
            found.append(f"{path}: no `{TEST}` feature, which this check holds: update DEFINED")
    if found:
        sys.exit("\n".join(found))
    print(f"ok  `{TEST}` is defined by {', '.join(sorted(DEFINED))} alone, turned on by no other "
          f"feature or dependency, and built only by {CHAOS_FILE}'s `{CHAOS_BUILD}`")


if __name__ == "__main__":
    main(sys.argv)
