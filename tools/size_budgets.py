#!/usr/bin/env python3
"""What a release ships, held to its budget: CI's build job, once it has
built each binary as tools/size-budgets.json says (its `about` says how).

    python3 tools/size_budgets.py [--summary FILE] NAME=PATH ...
    python3 tools/size_budgets.py --near NAME=PATH

Each entry of the file's `binaries` carries what CI measured when it was
last written (`measured`, the bytes the docs quote: site/build.py holds
every figure they write within 1% of it) and a budget a little above it.
A binary past its budget fails: the pull request that grew it writes its
new measurement and budget, which the failure prints, and says in its body
what the bytes buy. One more than 1% under its budget, or more than 1% from
its measurement, fails too, asking for the same: the docs would no longer
quote what ships. Every entry is measured by the run, so none is left
unheld; --summary appends the table, Markdown, to FILE (the job's summary).

--near holds a build of an entry's binary made another way -- the image's
fennec-server, which the root Dockerfile builds on Alpine with the same
toolchain -- within 1% of the entry's measurement, either way: the docs
quote that figure for the image too.
"""

import json
import math
import os
import sys

FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "size-budgets.json")

# How far under its budget, or from its measurement, a binary may be before
# it is measured again: the room site/build.py gives the docs' figures.
ROOM = 0.01
# A budget written from a measurement is this much above it, rounded up to
# a thousand bytes: a change of a few bytes passes, the room above it is
# about the room below, and a pull request adding more than half a percent
# measures it and says what it bought. A budget of the measurement itself
# stopped every pull request that added a byte, and two in the merge queue
# that each wrote one conflicted; one of 5% above it, the 1.0 plan's
# ceiling, left the docs' figures free to drift 5% unheld.
HEADROOM = 0.005


def budget_for(size):
    """The budget a measurement of `size` bytes is written with."""
    return math.ceil(size * (1 + HEADROOM) / 1000) * 1000


def again(size):
    """What to write for a binary measured at `size` bytes."""
    return (f'measure it again: "measured": {size}, "budget": {budget_for(size)} in '
            f"tools/size-budgets.json, and the docs' figures site/build.py names")


def judge(name, entry, size):
    """Why `size` bytes of `name`'s binary fail its entry, or None."""
    budget, measured = entry["budget"], entry["measured"]
    if size > budget:
        return (f"{name}: {size} bytes, {size - budget} past its budget of {budget} "
                f"({(size - budget) / budget:+.2%}); {again(size)}, and say in the pull "
                f"request what the bytes buy")
    if size < (1 - ROOM) * budget:
        return (f"{name}: {size} bytes, more than 1% under its budget of {budget} "
                f"({(size - budget) / budget:+.2%}); {again(size)}")
    if abs(size - measured) > ROOM * measured:
        return (f"{name}: {size} bytes, more than 1% from the {measured} the docs quote "
                f"({(size - measured) / measured:+.2%}); {again(size)}")
    return None


def judge_near(name, entry, size):
    """Why `size` bytes of `name`'s binary, built another way, are too far
    from the figure the docs quote for it, or None."""
    measured = entry["measured"]
    if abs(size - measured) > ROOM * measured:
        return (f"{name}, built another way: {size} bytes, more than 1% from the {measured} "
                f"the docs quote for it ({(size - measured) / measured:+.2%}); measure "
                f"both and say which the docs' figure is")
    return None


def table(rows):
    """The measurements as a Markdown table."""
    out = ["| binary | bytes | KB | budget | room left | against `measured` |",
           "| --- | ---: | ---: | ---: | ---: | ---: |"]
    for name, size, entry in rows:
        budget, measured = entry["budget"], entry["measured"]
        # KB rounded half up, as the docs write them.
        out.append(f"| `{name}` | {size} | {int(size / 1024 + 0.5)} | {budget} | {budget - size} | "
                   f"{(size - measured) / measured:+.2%} |")
    return "\n".join(out) + "\n"


def run(entries, pairs, near=False):
    """`(rows, problems)` for the NAME=PATH pairs against the file's entries."""
    rows, problems = [], []
    for name, path in pairs:
        entry = entries.get(name)
        if entry is None:
            problems.append(f"{name}: no entry in tools/size-budgets.json")
            continue
        try:
            size = os.path.getsize(path)
        except OSError as e:
            problems.append(f"{name}: {path}: {e.strerror}")
            continue
        rows.append((name, size, entry))
        why = (judge_near if near else judge)(name, entry, size)
        if why:
            problems.append(why)
    if not near:
        given = {name for name, _ in pairs}
        problems += [f"{name}: in tools/size-budgets.json, and this run measured no binary of it"
                     for name in entries if name not in given]
    return rows, problems


def main(argv):
    summary, near, pairs = None, False, []
    args = list(argv)
    while args:
        arg = args.pop(0)
        if arg == "--summary" and args:
            summary = args.pop(0)
        elif arg == "--near":
            near = True
        elif "=" in arg and not arg.startswith("-"):
            name, path = arg.split("=", 1)
            pairs.append((name, path))
        else:
            sys.exit(__doc__)
    if not pairs:
        sys.exit(__doc__)
    with open(FILE, encoding="utf-8") as f:
        entries = json.load(f)["binaries"]
    rows, problems = run(entries, pairs, near)
    text = table(rows)
    print(text, end="")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(("The image's binary, against the figure the docs quote for it\n\n" if near
                     else "What a release ships, against tools/size-budgets.json\n\n") + text + "\n")
    for p in problems:
        print(f"::error::{p}" if os.environ.get("GITHUB_ACTIONS") else p, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
