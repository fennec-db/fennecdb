#!/usr/bin/env python3
"""Every file the crates make is made by fennec_core::fs's helpers:
`python3 tools/file_create_check.py`.

docs/design/durability.md#files-modes. A file fennecdb makes holds rows,
secrets or refused requests, so on a Unix it is made `0600`, a directory
`0700`, by the call that makes it, whatever the umask -- `fs::private` or
`fs::private_beside` on the `OpenOptions` that creates it, `fs::create_private`
or `fs::create_like` for a side file (`O_EXCL`, never through a link put at
its name), `fs::create_dir_private` or `fs::create_dir_new` for a directory
-- and a file renamed over another takes that one's mode on its open handle
(`fs::mode_like`). `File::create`, `fs::write`, `create_dir_all` and the
system's `fs::copy` take the umask's mode, usually `0644`, readable by every
user of the machine: up to 0.2.0 every file was made so (G14's review, F5).

This reads every `.rs` file under `crates/*/src` but fennec-bench's and
fennec-fuzz's -- measurement and fuzzing programs, shipped in no release --
through tools/secret_eq_check.py's reader: comments blanked, strings made
literals of their length, every item a `#[cfg(test)]` is on left out. It
finds each way a file or a directory is made: `File::create`,
`File::create_new`, `fs::write`, `.create(true)`, `.create_new(true)`,
`create_dir`, `create_dir_all`, `DirBuilder`, `fs::copy` and
`UnixListener::bind`. An open with `.create(true)` or `.create_new(true)`
passes where its statement puts the mode on it, naming `private(` or
`private_beside(`; every other one is refused unless ALLOWED names it in its
file, by its kind, with why and how many times the file has it: one more is
a file nobody weighed, refused, and one fewer a stale pass, refused too.
`--self-test` holds the reader to CASES and COUNTED below. CI's fmt job
runs both."""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import secret_eq_check as reader  # noqa: E402  (the source reader, shared)

ROOT = reader.ROOT

# Crates shipped in no release, whose programs write their own results.
SKIPPED = ("fennec-bench", "fennec-fuzz")

MAKES = re.compile(
    r"\bFile\s*::\s*create(?:_new)?\s*\("
    r"|\bfs\s*::\s*write\s*\("
    r"|\.\s*create(?:_new)?\s*\(\s*true\s*\)"
    r"|\bcreate_dir(?:_all)?\s*\("
    r"|\bDirBuilder\b"
    r"|\bfs\s*::\s*copy\s*\("
    r"|\bUnixListener\s*::\s*bind\b"
)
# An open whose statement names one of these has the mode on it.
PRIVATE = re.compile(r"\bprivate(?:_beside)?\s*\(")

# A way of making a file the check finds that is meant: by file, its kind
# (the match, its spaces dropped, `(true)` and `(` kept), how many times
# the file has it, and why.
ALLOWED = {
    "crates/fennec-core/src/fs.rs": {
        "DirBuilder": (2, "the helpers themselves: create_dir_private and create_dir_new, mode 0700 on the mkdir"),
        ".create(true)": (1, "create_private on Windows, where a file takes its folder's access list"),
    },
    "crates/fennec-core/src/engine/unwinding.rs": {
        "create_dir_all(": (1, "a scratch directory of the unwinding checks, holding no data of anyone's"),
    },
    "crates/fennec-cli/src/backup.rs": {
        ".create_new(true)": (1, "`fennec key`'s file: new (O_EXCL), mode 0600 on its open, a DACL on Windows"),
    },
    "crates/fennec-cli/src/types.rs": {
        "fs::write(": (1, "`fennec types`' TypeScript: source code, no data, written as source files are"),
    },
    "crates/fennec-http/src/archive.rs": {
        "fs::copy(": (1, "`copied`: an image its owner's alone, copied by the system (a clone) with its mode, whatever was at the name removed first; a wider image is copied into create_private's file"),
        ".create(true)": (1, "`copied` on Windows, where a file takes its folder's access list"),
    },
    "crates/fennec-http/src/tenant_hooks.rs": {
        ".create(true)": (1, "the hook secrets' sidecar on Windows, made with owner_only's DACL; a Unix makes it by create_like"),
    },
}


def statement(code, at):
    """The statement around `at`: from the last `;`, `{` or `}` before it
    to the next `;`."""
    start = max(code.rfind(c, 0, at) for c in ";{}") + 1
    end = code.find(";", at)
    return code[start : len(code) if end < 0 else end]


def findings(src):
    """Each way `src` makes a file outside the helpers: its line and kind."""
    code = reader.without_tests(reader.blank(src))
    found = []
    for m in MAKES.finditer(code):
        kind = re.sub(r"\s+", "", m.group())
        if kind.startswith(".create") and PRIVATE.search(statement(code, m.start())):
            continue
        found.append((code.count("\n", 0, m.start()) + 1, kind))
    return found


def sources():
    for rel, path in reader.sources():
        if rel.split("/")[1] not in SKIPPED:
            yield rel, path


def verdicts(found, allowed):
    """What fails, from each file's findings [(line, kind)] and an
    ALLOWED: a kind it does not name, or names a number of times other
    than the file has it."""
    failed = []
    for rel in sorted(set(found) | set(allowed)):
        entries, lines = allowed.get(rel, {}), {}
        for line, kind in found.get(rel, []):
            lines.setdefault(kind, []).append(line)
        for kind, at in lines.items():
            if kind not in entries:
                failed += [
                    f"{rel}:{line}: `{kind}` makes a file with the umask's mode: make it with fennec_core::fs's "
                    "helpers (private, create_private, create_like, create_dir_private), or allow it in "
                    "tools/file_create_check.py's ALLOWED, saying why"
                    for line in at
                ]
        for kind, (count, _why) in entries.items():
            at = lines.get(kind, [])
            if len(at) > count:
                failed.append(
                    f"{rel}: `{kind}` is in ALLOWED {count} time(s) and in the code {len(at)}, lines "
                    f"{', '.join(map(str, at))}: make the new one with fennec_core::fs's helpers, or count "
                    "it, saying why"
                )
            elif len(at) < count:
                failed.append(
                    f"{rel}: `{kind}` is in ALLOWED {count} time(s) and in the code {len(at)}: "
                    "lower its count, or remove it"
                )
    return failed


def check():
    found = {}
    for rel, path in sources():
        with open(path, encoding="utf-8") as fh:
            got = findings(fh.read())
        if got:
            found[rel] = got
    failed = verdicts(found, ALLOWED)
    for message in failed:
        print(message)
    if failed:
        sys.exit(1)
    print("every file crates/*/src make is made by fennec_core::fs's helpers, or allowed")


# (source, the kinds the check finds in it).
CASES = [
    ("fn f() { std::fs::File::create(&p)?; }", ["File::create("]),
    ("fn f() { File :: create_new(p); }", ["File::create_new("]),
    ("fn f() { std::fs::write(&p, b\"x\")?; }", ["fs::write("]),
    ("fn f() { fs::copy(a, b)?; }", ["fs::copy("]),
    ("fn f() { std::fs::create_dir_all(&d)?; fs::create_dir(&d)?; }", ["create_dir_all(", "create_dir("]),
    ("fn f() { let b = std::fs::DirBuilder::new(); }", ["DirBuilder"]),
    ("fn f() { UnixListener::bind(p); }", ["UnixListener::bind"]),
    ("fn f() { OpenOptions::new().write(true).create(true).open(p)?; }", [".create(true)"]),
    ("fn f() { let mut o = OpenOptions::new();\n o.create_new(true); }", [".create_new(true)"]),
    # The mode on the open, in its statement: passed.
    ("fn f() { fennec_core::fs::private(OpenOptions::new().create(true).append(true)).open(p)?; }", []),
    (
        "fn f() { let f = crate::fs::private_beside(\n OpenOptions::new()\n .read(true)\n .create(true),\n)\n.open(p)?; }",
        [],
    ),
    ("fn f() { private(options.create_new(true)); }", []),
    # ... but only that statement's: the next one is read alone.
    ("fn f() { private(&mut a); OpenOptions::new().create(true).open(p); }", [".create(true)"]),
    # The helpers' own names, a comment, a string and a test are not makes.
    ("fn f() { fs::create_dir_private(&d)?; fs::create_dir_new(&d)?; fs::create_like(a, b)?; }", []),
    ("// File::create(p)\nfn f() { let s = \"fs::write(p, x)\"; }", []),
    ("#[cfg(test)]\nmod tests { fn t() { std::fs::write(p, b\"\").unwrap(); } }", []),
    ("fn f() { x.create(false); y.created(true); }", []),
]

# (source, its file's ALLOWED, how many failures the check reports): an
# entry lets as many through as it counts.
ONCE = {"fs::write(": (1, "a file of no data")}
COUNTED = [
    ("fn f() { fs::write(p, x); }", ONCE, 0),
    ("fn f() { fs::write(p, x); }\nfn g() { fs::write(q, x); }", ONCE, 1),
    ("fn f() {}", ONCE, 1),
    ("fn f() { fs::write(p, x); File::create(q); }", ONCE, 1),
]


def self_test():
    failed = 0
    for src, want in CASES:
        got = [k for _, k in findings(src)]
        if got != want:
            failed += 1
            print(f"FAIL {src!r}: found {got}, want {want}")
    for src, entries, want in COUNTED:
        got = verdicts({"f.rs": findings(src)}, {"f.rs": entries})
        if len(got) != want:
            failed += 1
            print(f"FAIL {src!r} under {entries}: {got}, want {want} failure(s)")
    if failed:
        sys.exit(1)
    print(f"{len(CASES) + len(COUNTED)} cases read as meant")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        check()
