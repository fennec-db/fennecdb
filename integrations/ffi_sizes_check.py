#!/usr/bin/env python3
"""Holds integrations/ffi-sizes.md, the native library's sizes the docs quote
(site/build.py reads it), to what CI just measured.

    integrations/ffi-sizes.sh <targets> | tee out.md
    python3 integrations/ffi_sizes_check.py out.md

Every target in `out.md` must be in the committed file and within 1% of it,
by bytes; a drift fails, and the fix is to copy the run's table into
ffi-sizes.md and the docs' figures with it (site/build.py names them). A
linker or a compiler bump moves these by a few KB; 1% is what that stays in.
"""

import os
import re
import sys

COMMITTED = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ffi-sizes.md")
ROW = re.compile(r"^\| `([^`]+)` \| [\d.]+ MB \((\d+) bytes\) \|$", re.M)


def read(path):
    with open(path, encoding="utf-8") as f:
        return {t: int(b) for t, b in ROW.findall(f.read())}


def drift(committed, measured, tol=0.01):
    """The targets measured that the committed table lacks or that moved
    further than `tol` of it."""
    out = []
    for target, got in measured.items():
        want = committed.get(target)
        if want is None:
            out.append(f"{target}: measured {got} bytes, not in ffi-sizes.md")
        elif abs(got - want) > tol * want:
            out.append(f"{target}: measured {got} bytes, ffi-sizes.md has {want} ({(got - want) / want:+.1%})")
    return out


def main():
    measured = read(sys.argv[1])
    if not measured:
        sys.exit(f"{sys.argv[1]}: no row of ffi-sizes.sh's table")
    problems = drift(read(COMMITTED), measured)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        sys.exit(1)


if __name__ == "__main__":
    main()
