#!/usr/bin/env python3
"""Each AGENTS.md written from the CLAUDE.md beside it: `make agents-md`,
`--check` in CI.

AGENTS.md is the same working summary for agents that read that name, under
a title and a first line of its own. Copied by hand beside each change, it
fell behind -- some 360 lines of CLAUDE.md missing and ten as they stood
before -- so it is written from CLAUDE.md instead, and CI fails when the
two differ.

`--check` also fails once a CLAUDE.md passes its cap. Every session and
every agent reads the root's whole before a word of its task: it had grown
to 511 KB, some 120 000 tokens, each invariant's measurements, history and
rejected alternatives among them, which live in docs/design/ now. At 64 000
bytes it held every rule, and each new one met the cap; so the root keeps
the rules a change anywhere can break and an index, and each area's rules
are in a CLAUDE.md beside the code that enforces them, which Claude Code
loads once it reads a file there. The index is what tells an agent which of
those to read before it changes code a rule binds from outside its
directory, so `--check` holds it to the files: every rule's title held by
one file alone, and each area file's titles listed under it, no more and
no fewer."""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# What a CLAUDE.md may weigh: its rules in a few sentences each, the long
# form in docs/design/. An area's cap leaves it room to grow before it is
# split again.
ROOT_MAX = 32_000
AREA_MAX = 24_000

# The root's title and the line under it; the rest is shared word for word.
CLAUDE_HEAD = (
    "# CLAUDE.md\n"
    "\n"
    "This file provides guidance to Claude Code (claude.ai/code) when working"
    " with code in this repository.\n"
)
AGENTS_HEAD = (
    "# AGENTS.md\n"
    "\n"
    "This file provides guidance to Codex and other coding agents when working"
    " with code in this repository. It is written from CLAUDE.md by `make"
    " agents-md`: edit that one.\n"
)
# An area file's title; its first line names the area and points back to
# the root, AGENTS.md's naming the root's AGENTS.md, then this.
TITLE = "# CLAUDE.md\n\n"
WRITTEN = " It is written from CLAUDE.md by `make agents-md`: edit that one."

INDEX = "## Area invariants"
RULE = re.compile(r"^\*\*\[(.+?)\]\(", re.M | re.S)


def agents(rel, claude):
    """The AGENTS.md beside CLAUDE.md `rel`, whose text is `claude`."""
    if rel == "CLAUDE.md":
        if not claude.startswith(CLAUDE_HEAD):
            sys.exit("CLAUDE.md: its first three lines moved; update tools/agents_md.py")
        return AGENTS_HEAD + claude[len(CLAUDE_HEAD):]
    if not claude.startswith(TITLE):
        sys.exit(f"{rel}: an area file starts with `# CLAUDE.md`, a blank line"
                 " and one line naming its area")
    first, nl, rest = claude[len(TITLE):].partition("\n")
    if not first.strip() or not nl:
        sys.exit(f"{rel}: its first line, naming its area, is missing")
    return ("# AGENTS.md\n\n" + first.replace("CLAUDE.md", "AGENTS.md") + WRITTEN
            + "\n" + rest)


def section(text, heading):
    """The body of `## heading` in `text`, to the next `## ` or the end."""
    start = text.find("\n" + heading + "\n")
    if start < 0:
        return ""
    body = text[start + len(heading) + 2:]
    end = re.search(r"^## ", body, re.M)
    return body[:end.start()] if end else body


def rules(text):
    """Each rule's (section, title) in a CLAUDE.md's `## Invariants`."""
    out = []
    head = None
    body = section(text, "## Invariants")
    for m in re.finditer(r"^### ([^\n]+)$|" + RULE.pattern, body, re.M | re.S):
        if m.group(1) is not None:
            head = m.group(1).strip()
        else:
            out.append((head, " ".join(m.group(2).split())))
    return out


def index(root):
    """The root's index: each area file with its (section, title)s."""
    files, rel, head = {}, None, None
    for line in section(root, INDEX).splitlines():
        m = re.match(r"^### `(.+/CLAUDE\.md)`\s*$", line)
        if m:
            rel, head = m.group(1), None
            if rel in files:
                sys.exit(f"CLAUDE.md: the index lists {rel} twice")
            files[rel] = []
        elif re.match(r"^\*[^*].*\*\s*$", line):
            head = line.strip()[1:-1]
        elif line.startswith("- ") and rel:
            files[rel].append((head, " ".join(line[2:].split())))
    return files


def on_disk():
    """Every CLAUDE.md git tracks or would, or None without git."""
    try:
        out = subprocess.run(
            ["git", "ls-files", "-z", "-co", "--exclude-standard"],
            cwd=ROOT, check=True, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL).stdout.decode()
    except (OSError, subprocess.CalledProcessError):
        return None
    return {p for p in out.split("\0")
            if p.split("/")[-1] == "CLAUDE.md" and "node_modules/" not in p
            and not p.startswith("target/")}


def check(paths, texts, listed):
    problems = []
    for rel in paths:
        path = ROOT / rel
        mirror = path.with_name("AGENTS.md")
        have = mirror.read_text(encoding="utf-8") if mirror.exists() else ""
        if have != agents(rel, texts[rel]):
            problems.append(f"{mirror.relative_to(ROOT)} differs from {rel}:"
                            " run `make agents-md`")
        size = len(texts[rel].encode("utf-8"))
        cap = ROOT_MAX if rel == "CLAUDE.md" else AREA_MAX
        if size > cap:
            problems.append(
                f"{rel} is {size} bytes, past {cap}: keep an invariant's rule"
                " there in a few sentences, and its measurements, rejected"
                " alternatives and history in its docs/design/ file; split an"
                " area that outgrew its file")

    held = {}
    for rel in paths:
        for head, title in rules(texts[rel]):
            held.setdefault(title, []).append((rel, head))
    for title, where in held.items():
        if len(where) > 1:
            problems.append(f"the rule {title!r} is in "
                            + " and ".join(r for r, _ in where) + ": keep it in one")

    for rel, entries in listed.items():
        have = {(h, t) for h, t in rules(texts[rel])}
        want = set(entries)
        for head, title in entries:
            if (head, title) in have:
                continue
            elsewhere = [f"{r} under {h!r}" for r, h in held.get(title, [])]
            problems.append(
                f"CLAUDE.md's index lists {title!r} under {rel}'s {head!r}, but "
                + (f"it is in {', '.join(elsewhere)}" if elsewhere
                   else "no CLAUDE.md holds it"))
        for head, title in sorted(have - want, key=lambda e: e[1]):
            problems.append(f"{rel} holds {title!r} under {head!r}, which"
                            " CLAUDE.md's index does not list there")
        if len(entries) != len(want):
            problems.append(f"CLAUDE.md's index lists a title of {rel} twice")

    found = on_disk()
    if found is not None:
        for rel in sorted(found - set(paths)):
            problems.append(f"{rel} is in no entry of CLAUDE.md's index: add"
                            " one, then `make agents-md`")
    return problems


def main():
    root = (ROOT / "CLAUDE.md").read_text(encoding="utf-8")
    listed = index(root)
    paths = ["CLAUDE.md", *listed]
    texts = {"CLAUDE.md": root}
    for rel in listed:
        path = ROOT / rel
        if not path.exists():
            sys.exit(f"CLAUDE.md's index lists {rel}, which is not there")
        texts[rel] = path.read_text(encoding="utf-8")

    if sys.argv[1:] == ["--check"]:
        problems = check(paths, texts, listed)
        if problems:
            sys.exit("\n".join(problems))
        return
    if sys.argv[1:]:
        sys.exit("usage: agents_md.py [--check]")
    for rel in paths:
        (ROOT / rel).with_name("AGENTS.md").write_text(agents(rel, texts[rel]),
                                                       encoding="utf-8")


if __name__ == "__main__":
    main()
