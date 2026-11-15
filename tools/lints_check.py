#!/usr/bin/env python3
"""Every crate takes the workspace's lints, and every `unsafe` block says
why it is sound, whatever builds it: `python3 tools/lints_check.py`.

docs/design/conventions.md#unsafe-code. The root Cargo.toml's
`[workspace.lints]` denies clippy's `undocumented_unsafe_blocks` and
rustc's `unsafe_op_in_unsafe_fn`. A crate reaches that table only by
writing `[lints] workspace = true`, which a new crate could leave out, and
then nothing it holds would be asked: this holds every member of the
workspace to it, and the table to the two lints.

Clippy reads only the code a run compiles, and CI's runs compile x86_64
Linux, wasm32 and part of Windows: an aarch64 kernel, an Apple system
call, a Windows test or a macOS-only example is compiled by none of them.
So this reads every `.rs` file the repository holds, whatever its `cfg`,
and refuses an `unsafe` block or `unsafe impl` with no comment saying
`SAFETY:` where clippy looks for one: on the lines just above it, blank
lines skipped, or above the statement it is -- itself, or a `let`,
`static` or `const` it is the value of -- attributes included. An unsafe
operation in an `unsafe fn` is in a block of its own, which rustc's deny
holds on every build for every target, so its comment is a block's.
CI's fmt job runs it, `tools/test_lints_check.py` first."""

import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent

# What the root Cargo.toml's `[workspace.lints]` must hold.
LINTS = {
    ("clippy", "undocumented_unsafe_blocks"): "deny",
    ("rust", "unsafe_op_in_unsafe_fn"): "deny",
}

IDENT = re.compile(r"[A-Za-z0-9_]")
UNSAFE = re.compile(r"(?<![A-Za-z0-9_#])unsafe(?![A-Za-z0-9_])")
SAFETY = re.compile(r"SAFETY:", re.I)
# What may come before an unsafe block in its statement for a comment
# above the statement to be its: nothing, or a `let`, `static` or `const`
# whose value it is.
INITIALIZER = re.compile(
    r"^(?:(?:pub(?:\([^)]*\))?\s+)?(?:static|const)\b|let\b)[^;{}]*[^=!<>+\-*/%&|^]=$", re.S
)


def mask(src):
    """`(code, comments)`: `src` twice at the same offsets, newlines kept --
    once with every comment and the inside of every string and character
    literal blanked to spaces, once with only the comments."""
    code = list(src)
    comments = [c if c == "\n" else " " for c in src]
    n = len(src)
    i = 0

    def blank(a, b, keep_comment=False):
        for k in range(a, b):
            if src[k] != "\n":
                code[k] = " "
            if keep_comment:
                comments[k] = src[k]

    while i < n:
        c = src[i]
        if src.startswith("//", i):
            end = src.find("\n", i)
            end = n if end < 0 else end
            blank(i, end, True)
            i = end
        elif src.startswith("/*", i):
            depth, k = 1, i + 2
            while k < n and depth:
                if src.startswith("/*", k):
                    depth, k = depth + 1, k + 2
                elif src.startswith("*/", k):
                    depth, k = depth - 1, k + 2
                else:
                    k += 1
            blank(i, k, True)
            i = k
        elif c in "rbc" and (i == 0 or not IDENT.match(src[i - 1])) and (
            m := re.compile(r"(?:br|cr|r)(#*)\"").match(src, i)
        ):
            close = '"' + m.group(1)
            end = src.find(close, m.end())
            end = n if end < 0 else end
            blank(m.end(), end)
            i = end + len(close)
        elif c == '"':
            k = i + 1
            while k < n and src[k] != '"':
                k += 2 if src[k] == "\\" else 1
            blank(i + 1, min(k, n))
            i = k + 1
        elif c == "'":
            # A character literal, or a lifetime's quote.
            if src.startswith("\\", i + 1):
                end = src.find("'", i + 3)
                end = n if end < 0 else end
                blank(i + 1, end)
                i = end + 1
            elif i + 2 < n and src[i + 2] == "'" and src[i + 1] != "\n":
                blank(i + 1, i + 2)
                i += 3
            else:
                i += 1
        else:
            i += 1
    return "".join(code), "".join(comments)


def attributes_before(code, pos):
    """Where the attributes right before `pos` begin: `pos` when there are none."""
    while True:
        k = pos - 1
        while k >= 0 and code[k].isspace():
            k -= 1
        if k < 0 or code[k] != "]":
            return pos
        depth = 0
        while k >= 0:
            if code[k] == "]":
                depth += 1
            elif code[k] == "[":
                depth -= 1
                if depth == 0:
                    break
            k -= 1
        j = k - 1
        if j >= 0 and code[j] == "!":
            j -= 1
        if j < 0 or code[j] != "#":
            return pos
        pos = j


def statement_start(code, pos):
    """Where the statement holding the code at `pos` begins, within the
    parentheses, brackets or block around it."""
    depth = 0
    k = pos - 1
    while k >= 0:
        c = code[k]
        if c in ")]}":
            # A block statement ended on its own line: the one before this.
            end = code.find("\n", k)
            if c == "}" and depth == 0 and not code[k + 1 : len(code) if end < 0 else end].strip():
                return k + 1
            depth += 1
        elif c in "([{":
            if depth == 0:
                return k + 1
            depth -= 1
        elif c == ";" and depth == 0:
            return k + 1
        k -= 1
    return 0


def documented(code, comments, line_starts, anchor):
    """Whether the lines just above the one `anchor` is on, blank ones
    skipped, are comments, one of them saying SAFETY: -- clippy's reading."""
    line = line_of(line_starts, anchor)
    found = False
    for at in range(line - 1, -1, -1):
        a, b = line_starts[at], line_starts[at + 1] if at + 1 < len(line_starts) else len(code)
        if code[a:b].strip():
            break
        text = comments[a:b]
        if not text.strip():
            continue
        if text.strip().lstrip("/").strip().startswith("```"):
            return False
        if SAFETY.search(text):
            found = True
            break
    return found


def line_of(line_starts, pos):
    lo, hi = 0, len(line_starts) - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if line_starts[mid] <= pos:
            lo = mid
        else:
            hi = mid - 1
    return lo


def sites(code):
    """`(match, what)` for each `unsafe` block and `unsafe impl` of masked `code`."""
    for m in UNSAFE.finditer(code):
        rest = code[m.end() :].lstrip()
        if rest.startswith("{"):
            yield m, "an `unsafe` block"
        elif re.match(r"impl\b", rest):
            yield m, "an `unsafe impl`"
        # Else `unsafe fn`, `unsafe extern`, `unsafe trait`, `#[unsafe(...)]`.


def findings(src):
    """`(line, what)` for each `unsafe` block or `unsafe impl` of `src` no
    comment says is sound, lines counted from 1; and how many there are."""
    code, comments = mask(src)
    line_starts = [0] + [m.end() for m in re.finditer("\n", src)]
    out = []
    count = 0
    for m, what in sites(code):
        count += 1
        anchors = [attributes_before(code, m.start())]
        if what.endswith("block"):
            start = statement_start(code, m.start())
            first = len(code[start:]) - len(code[start:].lstrip())
            start += first
            prefix = code[start : m.start()]
            # Its attributes are the statement's.
            bare = prefix
            while (a := re.match(r"\s*#!?\[", bare)) is not None:
                depth, k = 0, a.end() - 1
                while k < len(bare):
                    depth += {"[": 1, "]": -1}.get(bare[k], 0)
                    if depth == 0:
                        break
                    k += 1
                bare = bare[k + 1 :]
            bare = bare.strip()
            if bare == "" or INITIALIZER.match(bare):
                anchors.append(attributes_before(code, start))
        if not any(documented(code, comments, line_starts, a) for a in anchors):
            out.append((line_of(line_starts, m.start()) + 1, what))
    return out, count


def lint_table():
    root = tomllib.loads((ROOT / "Cargo.toml").read_text())
    workspace = root["workspace"]
    problems = []
    for (tool, lint), want in LINTS.items():
        level = workspace.get("lints", {}).get(tool, {}).get(lint)
        if level != want:
            problems.append(f"Cargo.toml: [workspace.lints.{tool}] {lint} is {level!r}, not {want!r}")
    members = workspace.get("members", [])
    if not members:
        problems.append("Cargo.toml: the workspace names no members")
    for member in members:
        manifest = ROOT / member / "Cargo.toml"
        lints = tomllib.loads(manifest.read_text()).get("lints")
        if lints != {"workspace": True}:
            problems.append(
                f"{member}/Cargo.toml: takes {lints!r} for its lints, not the "
                "workspace's: write `[lints]` and `workspace = true`"
            )
    return problems, len(members)


def sources():
    """Every `.rs` file the repository holds, by its path from the root."""
    listed = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "*.rs"],
        cwd=ROOT, capture_output=True, check=True,
    ).stdout.decode()
    return sorted(p for p in listed.split("\0") if p)


def main() -> int:
    problems, crates = lint_table()
    total = 0
    files = sources()
    for path in files:
        src = (ROOT / path).read_text(encoding="utf-8")
        if "unsafe" not in src:
            continue
        found, count = findings(src)
        total += count
        for line, what in found:
            problems.append(
                f"{path}:{line}: {what} with no `// SAFETY:` comment above it, "
                "or above the statement it is (docs/design/conventions.md#unsafe-code)"
            )
    for p in problems:
        print(p, file=sys.stderr)
    if not problems:
        print(
            f"{crates} crates take the workspace's lints; {total} unsafe blocks and impls "
            f"in {len(files)} files, each saying why it is sound"
        )
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
