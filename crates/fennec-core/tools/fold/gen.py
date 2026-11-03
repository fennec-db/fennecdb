#!/usr/bin/env python3
"""Writes src/text/fold.rs: what `@text` folds a Latin or a Greek letter to,
so that `sutlac` finds `Sütlaç`, `kofte` `köfte` and `ελληνικα` `ελληνικά`.

    python3 crates/fennec-core/tools/fold/gen.py [--check] [UnicodeData.txt]

Without a path the script fetches Unicode 17.0.0's UnicodeData.txt, the
version of the standard library's case mappings (`char::UNICODE_VERSION`),
which the tokenizer lowercases with before it folds. It writes the file
through rustfmt, as `cargo fmt --check` wants it; `--check` writes
nothing and fails where src/text/fold.rs is not what it would write -- a
table edited by hand, or formatted apart from this script (CI's fmt job
runs it).

A letter folds to its base letter, its marks dropped: its canonical
decomposition's first code point when that is a letter of its own script
and the rest are combining marks -- `ş` is `s` and U+0327, `ǖ` is `ü` and
U+0304, so `u` -- folded again in turn. A letter Unicode decomposes only
for compatibility, into letters of its own script and marks, folds to
those letters (`ĳ` to `ij`, `ſ` to `s`, `ǆ` to `dz`, `ϐ` to `β`). Unicode
gives some no decomposition at all, and their names say what they are: a
letter `WITH STROKE` or `WITH MIDDLE DOT` is its letter (`ø`, `ł`, `đ`,
`ħ`, `ŧ`, `ŀ`), `æ` and `œ` are `ae` and `oe`, `ß` is `ss`, and the final
sigma `ς` is `σ`, which `Σ` lowercases to wherever it stands. Every other
letter, Cyrillic's among them, stays as it is; so do `ı` and `İ`, which the
tokenizer folds to `i` itself.

A letter and its lowercase fold the same, or case would decide a word
again: a letter folds only through its lowercase, and not at all where its
lowercase is outside the table, which the tokenizer then never folds.

The table is a byte a code point over three ranges -- Latin-1's letters
with Latin Extended-A and -B, Greek and Coptic, and Latin Extended
Additional (Vietnamese) with Greek Extended (polytonic Greek): 0 for a
code point that stays, an ASCII letter for itself, `n` up to 31 for the
`n`th of `PAIRS`, and from 0x80 up U+0300 past it -- a Greek letter. 1 056
bytes; ASCII text never reaches it. Where a code point is in it is a
function of the ranges, `at`, each range's offset a constant of its arm:
a loop over the ranges, summing their lengths, cost Cyrillic and Greek a
walk of them for every letter.
"""

import hashlib
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.request

UNICODE = "17.0.0"
URL = "https://www.unicode.org/Public/%s/ucd/UnicodeData.txt" % UNICODE
# The file the table was written from: a download that differs is refused
# rather than written or checked against.
SHA256 = "2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "..", "src", "text", "fold.rs"))

# What the table holds, each first code point and the one past the last.
RANGES = [
    (0x00C0, 0x0250),  # Latin-1's letters, Latin Extended-A and -B
    (0x0370, 0x0400),  # Greek and Coptic
    (0x1E00, 0x2000),  # Latin Extended Additional, Greek Extended
]


class Char:
    def __init__(self, fields):
        self.name = fields[1]
        self.category = fields[2]
        decomposition = fields[5].split()
        self.tag = None
        if decomposition and decomposition[0].startswith("<"):
            self.tag = decomposition.pop(0)
        self.parts = [int(p, 16) for p in decomposition]
        self.lower = int(fields[13], 16) if fields[13] else None


def load(path):
    chars = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            fields = line.rstrip("\n").split(";")
            # The ranges given by their ends (`<CJK Ideograph, First>`) hold
            # no letter this table folds.
            chars[int(fields[0], 16)] = Char(fields)
    return chars


def fetch():
    """UnicodeData.txt into a directory of its own, tried three times, when
    no path is given (CI gives the copy it keeps by version)."""
    path = os.path.join(tempfile.mkdtemp(), "UnicodeData.txt")
    for attempt in range(3):
        try:
            urllib.request.urlretrieve(URL, path)
            return path
        except OSError:
            if attempt == 2:
                raise
            time.sleep(5 * (attempt + 1))


def main():
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    path = args[0] if args else fetch()
    with open(path, "rb") as fh:
        digest = hashlib.sha256(fh.read()).hexdigest()
    if digest != SHA256:
        sys.exit("%s is not Unicode %s's UnicodeData.txt (sha256 %s)" % (path, UNICODE, digest))
    chars = load(path)

    def lower(cp):
        c = chars.get(cp)
        return c.lower if c is not None and c.lower is not None else cp

    def script(cp):
        c = chars.get(cp)
        if c is None or not c.category.startswith("L"):
            return None
        for s in ("LATIN", "GREEK"):
            if c.name.startswith(s + " "):
                return s
        return None

    def mark(cp):
        c = chars.get(cp)
        return c is not None and c.category.startswith("M")

    def fold(cp):
        """What the letter `cp` folds to, lowercase; None where it stays."""
        cp = lower(cp)
        s = script(cp)
        if s is None:
            return None
        c = chars[cp]
        m = re.fullmatch(r"LATIN SMALL (?:LETTER|LIGATURE) (AE|OE)", c.name)
        if m:
            return m.group(1).lower()
        if c.name == "LATIN SMALL LETTER SHARP S":
            return "ss"
        m = re.fullmatch(r"LATIN SMALL LETTER ([A-Z]) WITH (?:STROKE|MIDDLE DOT)", c.name)
        if m:
            return m.group(1).lower()
        if c.name == "GREEK SMALL LETTER FINAL SIGMA":
            return "σ"
        whole = lambda p: fold(p) or chr(lower(p))
        if c.tag is None and c.parts:
            first, rest = c.parts[0], c.parts[1:]
            if script(first) == s and all(mark(p) for p in rest):
                return whole(first)
        if c.tag == "<compat>":
            letters = [p for p in c.parts if not mark(p)]
            if letters and all(script(p) == s for p in letters):
                return "".join(whole(p) for p in letters)
        return None

    def inside(cp):
        return any(lo <= cp < hi for lo, hi in RANGES)

    folds = {}
    left = []
    for lo, hi in RANGES:
        for cp in range(lo, hi):
            f = fold(cp)
            if f is None:
                continue
            if not inside(lower(cp)) and lower(cp) >= 0x80:
                # Its lowercase stays wherever it is written, so must it.
                left.append((cp, f, "its lowercase is outside the table"))
                continue
            if f == chr(lower(cp)):
                continue
            folds[cp] = f

    pairs = sorted({f for f in folds.values() if len(f) > 1})
    table = []
    for lo, hi in RANGES:
        for cp in range(lo, hi):
            f = folds.get(cp)
            if f is None:
                table.append(0)
            elif len(f) == 1 and "a" <= f <= "z":
                table.append(ord(f))
            elif len(f) == 1 and 0x0380 <= ord(f) < 0x0400:
                table.append(ord(f) - 0x0300)
            elif len(f) == 2 and f.isascii() and f.isalpha():
                table.append(pairs.index(f) + 1)
            else:
                left.append((cp, f, "no byte holds it"))
                table.append(0)
    assert len(pairs) < 0x20, pairs

    out = [
        "// Written by crates/fennec-core/tools/fold/gen.py from Unicode %s's" % UNICODE,
        "// UnicodeData.txt; do not edit. What a Latin or Greek letter folds to under",
        "// `@text` -- the rule is the script's.",
        "",
        "/// Where the code point `u` is in `TABLE`, if it holds it: the ranges it",
        "/// holds in order, each at the bytes of those before it.",
        "pub(super) fn at(u: u32) -> Option<usize> {",
        "    let at = match u {",
    ]
    before = 0
    for lo, hi in RANGES:
        # The first range's offset left out: clippy's `identity_op`.
        plus = " + 0x%04X" % before if before else ""
        out.append("        0x%04X..=0x%04X => u - 0x%04X%s," % (lo, hi - 1, lo, plus))
        before += hi - lo
    out += [
        "        _ => return None,",
        "    };",
        "    Some(at as usize)",
        "}",
        "",
        "/// The folds of two letters, the `n`th named by a byte `n` of `TABLE`.",
        "pub(super) const PAIRS: [&str; %d] = [%s];" % (
            len(pairs), ", ".join('"%s"' % p for p in pairs)),
        "",
        "/// A byte a code point: 0 where it stays, an ASCII letter for itself, a",
        "/// pair of `PAIRS` from 1, and from 0x80 up U+0300 past it (a Greek letter).",
        "pub(super) static TABLE: [u8; %d] = [" % len(table),
    ]
    at = 0
    for lo, hi in RANGES:
        for row in range(lo, hi, 16):
            n = min(16, hi - row)
            out.append("    %s // U+%04X" % (
                " ".join("0x%02X," % b for b in table[at:at + n]), row))
            at += n
    out.append("];")
    # As `cargo fmt --check` wants it: written here, then through rustfmt,
    # so that what the script writes is the file as committed.
    target = OUT
    if check:
        target = os.path.join(tempfile.mkdtemp(), "fold.rs")
    with open(target, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out) + "\n")
    subprocess.run(["rustfmt", "--edition", "2021", target], check=True)
    if check:
        with open(target, encoding="utf-8") as a, open(OUT, encoding="utf-8") as b:
            if a.read() != b.read():
                sys.exit("%s is not what tools/fold/gen.py writes: run it" % OUT)
        print("%s is what tools/fold/gen.py writes" % OUT)
        return

    print("%d code points fold, %d bytes, pairs %s" % (len(folds), len(table), pairs))
    for cp, f, why in left:
        print("left as it is: U+%04X %s -> %s (%s)" % (cp, chars[cp].name, f, why))


if __name__ == "__main__":
    main()
