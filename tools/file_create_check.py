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
(`fs::mode_like`, which `fs::create_like` calls). `File::create`,
`fs::write`, `create_dir_all` and the system's `fs::copy` take the umask's
mode, usually `0644`, readable by every user of the machine: up to 0.2.0
every file was made so (G14's review, F5).

This reads every `.rs` file under `crates/*/src` but fennec-bench's and
fennec-fuzz's -- measurement and fuzzing programs, shipped in no release --
through tools/secret_eq_check.py's reader: comments blanked, strings made
literals of their length, every item a `#[cfg(test)]` is on left out. It
finds each way a file or a directory is made or renamed into a place:
`File::create`, `File::create_new`, `fs::write`, `.create(..)` and
`.create_new(..)` given `true` or a name, `create_dir`, `create_dir_all`,
`DirBuilder`, `fs::copy`, `fs::rename`, `UnixListener::bind` and
`UnixDatagram::bind`; a function of `std::fs` imported by its name, or
under another (`use std::fs::{write, rename as moved}`), is found where it
is called so, and `File` or `fs` imported under another name too. An open
with `.create(..)` or `.create_new(..)` passes where its statement puts the
mode on it, naming `private(` or `private_beside(`; an `fs::rename` passes
where the function it is in keeps the mode, naming `create_like(` or
`mode_like(`; every other one is refused unless ALLOWED names it in its
file, by its kind, with why and how many times the file has it: one more
is a file nobody weighed, refused, and one fewer a stale pass, refused too.
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
    r"|\bfs\s*::\s*(?:write|copy|rename)\s*\("
    r"|\.\s*create(?:_new)?\s*\(\s*(?:true|!?\s*(?!false\b)[A-Za-z_][\w.]*)\s*\)"
    r"|\bcreate_dir(?:_all)?\s*\("
    r"|\bDirBuilder\b"
    r"|\bUnix(?:Listener|Datagram)\s*::\s*bind\b"
)
# An open whose statement names one of these has the mode on it.
PRIVATE = re.compile(r"\bprivate(?:_beside)?\s*\(")
# A rename in a function naming one of these renames a file given the mode
# of the one it replaces.
KEEPS = re.compile(r"\b(?:create_like|mode_like)\s*\(")

# std::fs's functions that make a file or put one in a place, by the kind
# the check names them: found called by a name a `use` gave them.
FUNCTIONS = {
    "write": "fs::write(",
    "copy": "fs::copy(",
    "rename": "fs::rename(",
    "create_dir": "create_dir(",
    "create_dir_all": "create_dir_all(",
}

# A way of making a file the check finds that is meant: by file, its kind
# (the match, its spaces dropped, `(true)` and `(` kept), how many times
# the file has it, and why.
ALLOWED = {
    "crates/fennec-core/src/fs.rs": {
        "DirBuilder": (2, "the helpers themselves: create_dir_private and create_dir_new, mode 0700 on the mkdir"),
        ".create(path)": (2, "the same helpers' DirBuilder::create, the mode set on the builder"),
        ".create(true)": (1, "create_private on Windows, where a file takes its folder's access list"),
    },
    "crates/fennec-core/src/fs/windows.rs": {
        "fs::rename(": (1, "Windows' swap, where a file renamed keeps the access list it was made with"),
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
        "fs::rename(": (
            2,
            "`renamed`, a Unix's and Windows': a record its callers made, by create_like where one is replaced "
            "(`put`), by create_private where none is (`keep_began`, only where `began` is not)",
        ),
    },
    "crates/fennec-http/src/node_archive.rs": {
        "fs::rename(": (1, "a tenant's archive directory moved aside, its inode and its mode with it"),
    },
    "crates/fennec-http/src/tenant_hooks.rs": {
        ".create(true)": (1, "the hook secrets' sidecar on Windows, made with owner_only's DACL; a Unix makes it by create_like"),
        "fs::rename(": (1, "a sidecar no key reads moved aside, its inode and its mode with it"),
    },
    "crates/fennec-http/src/tenants.rs": {
        "fs::rename(": (1, "a tenant's install, refused (409) where a file has the name: made by create_private, no mode to keep"),
    },
}


def statement(code, at):
    """The statement around `at`: from the last `;`, `{` or `}` before it
    to the next `;`."""
    start = max(code.rfind(c, 0, at) for c in ";{}") + 1
    end = code.find(";", at)
    return code[start : len(code) if end < 0 else end]


BRACES = {"{": 1, "}": -1}


def body_of(code, at):
    """The body of the innermost `fn` `at` is in, or the empty text: from
    its `{` to its `}`."""
    best = ""
    for m in re.finditer(r"\bfn\s+[A-Za-z_]\w*", code[:at]):
        # Its body's `{`, or the `;` a declaration ends with, past the
        # brackets of its signature (`[u8; 4]`).
        depth, j = 0, m.end()
        while j < len(code) and not (depth == 0 and code[j] in "{;"):
            depth += {"(": 1, "[": 1, ")": -1, "]": -1}.get(code[j], 0)
            j += 1
        if j >= len(code) or code[j] == ";" or j > at:
            continue
        depth, k = 0, j
        while k < len(code):
            depth += BRACES.get(code[k], 0)
            if depth == 0:
                break
            k += 1
        if at < k:
            best = code[j : k + 1]
    return best


def split_top(text):
    """`text` apart at the commas outside its braces."""
    parts, depth, start = [], 0, 0
    for i, c in enumerate(text):
        depth += BRACES.get(c, 0)
        if c == "," and depth == 0:
            parts.append(text[start:i])
            start = i + 1
    return parts + [text[start:]]


def leaves(tree, prefix=()):
    """A `use` tree's leaves: (its path, the name it is used by)."""
    out = []
    for part in split_top(tree):
        part = part.strip()
        if not part:
            continue
        if "{" in part:
            head = [p for p in re.split(r"\s*::\s*", part[: part.index("{")].strip()) if p]
            out += leaves(part[part.index("{") + 1 : part.rindex("}")], prefix + tuple(head))
            continue
        m = re.fullmatch(r"(.*?)(?:\s+as\s+([A-Za-z_]\w*))?", part, re.S)
        path = tuple(p for p in re.split(r"\s*::\s*", m.group(1).strip()) if p)
        full = prefix if path == ("self",) else prefix + path
        if full:
            out.append((full, m.group(2) or full[-1]))
    return out


def imported(code):
    """The ways `code`'s `use`s give a maker another way to be called: a
    pattern of the call each, and the kind it is."""
    ways = []
    for use in re.finditer(r"\buse\s+([^;]*);", code):
        for full, name in leaves(use.group(1)):
            if full[0] == "std":
                full = full[1:]
            if len(full) == 2 and full[0] == "fs" and full[1] in FUNCTIONS:
                ways.append((rf"(?<![\w.:]){name}\s*\(", FUNCTIONS[full[1]]))
            elif full == ("fs", "*"):
                ways += [(rf"(?<![\w.:]){f}\s*\(", kind) for f, kind in FUNCTIONS.items()]
            elif full == ("fs", "File") and name != "File":
                ways.append((rf"\b{name}\s*::\s*create(?:_new)?\s*\(", "File::create("))
            elif full == ("fs",) and name != "fs":
                ways.append((rf"\b{name}\s*::\s*(?:write|copy|rename)\s*\(", None))
    return ways


def kind_of(text):
    """A match's kind: its spaces dropped, an aliased module's call by
    `fs`'s name."""
    kind = re.sub(r"\s+", "", text)
    return re.sub(r"^\w+::(write|copy|rename)\($", r"fs::\1(", kind)


def findings(src):
    """Each way `src` makes a file outside the helpers: its line and kind."""
    code = reader.without_tests(reader.blank(src))
    found = []
    calls = [(m.start(), kind_of(m.group())) for m in MAKES.finditer(code)]
    for pattern, kind in imported(code):
        for m in re.finditer(pattern, code):
            if not re.search(r"\bfn\s*$", code[: m.start()]):
                calls.append((m.start(), kind or kind_of(m.group())))
    for at, kind in sorted(set(calls)):
        if kind.startswith(".create") and PRIVATE.search(statement(code, at)):
            continue
        if kind == "fs::rename(" and KEEPS.search(body_of(code, at)):
            continue
        found.append((code.count("\n", 0, at) + 1, kind))
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
                    f"{rel}:{line}: `{kind}` makes a file with the umask's mode, or renames one into a place "
                    "without the mode of what it replaces: make it with fennec_core::fs's helpers (private, "
                    "create_private, create_like, create_dir_private; mode_like before a rename), or allow it "
                    "in tools/file_create_check.py's ALLOWED, saying why"
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
    ("fn f() { std::os::unix::net::UnixDatagram::bind(p)?; }", ["UnixDatagram::bind"]),
    ("fn f() { OpenOptions::new().write(true).create(true).open(p)?; }", [".create(true)"]),
    ("fn f() { let mut o = OpenOptions::new();\n o.create_new(true); }", [".create_new(true)"]),
    # Told whether to make it by a name: a make all the same.
    ("fn f(make: bool) { OpenOptions::new().write(true).create(make).open(p)?; }", [".create(make)"]),
    ("fn f() { o.create(!self.read_only); }", [".create(!self.read_only)"]),
    # The mode on the open, in its statement: passed.
    ("fn f() { fennec_core::fs::private(OpenOptions::new().create(true).append(true)).open(p)?; }", []),
    (
        "fn f() { let f = crate::fs::private_beside(\n OpenOptions::new()\n .read(true)\n .create(true),\n)\n.open(p)?; }",
        [],
    ),
    ("fn f() { private(options.create_new(true)); }", []),
    ("fn f(make: bool) { private(OpenOptions::new().create(make)).open(p)?; }", []),
    # ... but only that statement's: the next one is read alone.
    ("fn f() { private(&mut a); OpenOptions::new().create(true).open(p); }", [".create(true)"]),
    # A rename into a place keeps the mode where its function gives it,
    # and only there.
    ("fn f() { std::fs::rename(&tmp, &path)?; }", ["fs::rename("]),
    ("fn f() { let f = create_like(&tmp, &path)?;\n f.sync_all()?;\n fs::rename(&tmp, &path) }", []),
    ("fn f(f: &File) { fennec_core::fs::mode_like(f, out)?; fs::rename(&tmp, out)?; }", []),
    (
        "fn f() { mode_like(f, out)?; }\nfn g(x: [u8; 4]) { fs::rename(&tmp, out)?; }",
        ["fs::rename("],
    ),
    ("fn f() { let g = |x: u8| { mode_like(f, out) };\n if a { fs::rename(&tmp, out)?; } }", []),
    # std::fs's functions imported by their names, or by others, and
    # `File` and `fs` under others.
    ("use std::fs::{self, write};\nfn f() { write(&p, b\"x\")?; }", ["fs::write("]),
    ("use std::{fs::{copy, rename as moved}, io};\nfn f() { copy(a, b)?; moved(a, b)?; }", ["fs::copy(", "fs::rename("]),
    ("use std::fs::create_dir_all as mkdirs;\nfn f() { mkdirs(d)?; }", ["create_dir_all("]),
    ("use std::fs::*;\nfn f() { write(p, x)?; }", ["fs::write("]),
    ("use std::fs::File as Plain;\nfn f() { Plain::create(p)?; }", ["File::create("]),
    ("use std::fs as files;\nfn f() { files::write(p, x)?; files::rename(a, b)?; }", ["fs::write(", "fs::rename("]),
    # ... but not a method, a path, a macro or a function of that name.
    ("use std::fs::write;\nfn write(x: u8) {}\nfn f() { w.write(b); io::Write::write(w, b); write!(w, \"\"); }", []),
    ("use std::io::copy;\nfn f() { copy(&mut r, &mut w)?; }", []),
    # The helpers' own names, a comment, a string and a test are not makes.
    ("fn f() { fs::create_dir_private(&d)?; fs::create_dir_new(&d)?; fs::create_like(a, b)?; }", []),
    ("// File::create(p)\nfn f() { let s = \"fs::write(p, x)\"; }", []),
    ("#[cfg(test)]\nmod tests { fn t() { std::fs::write(p, b\"\").unwrap(); std::fs::rename(a, b).unwrap(); } }", []),
    ("fn f() { x.create(false); y.created(true); z.create(); }", []),
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
