#!/usr/bin/env python3
"""SECURITY.md is held to the tree: `python3 tools/security_md_check.py`,
CI's fmt job.

A security policy that names a flag no binary reads, a file that is gone or a
primitive nobody wrote any more is worse than none: a reporter and an
operator act on it. The check refuses, naming each:

- a `--flag` the file names that the binary it is said of does not read
  (`"--flag" =>` in fennec-server's, fennec-shard's or fennec-cli's sources:
  where the 240 characters before the flag, in its paragraph or bullet, name a binary
  -- `fennec-server`, `fennec-shard` or `the router`, `fennec import` or the
  shell -- one of those is meant; where they name none, any of the three);
- a path it names, in backticks or as a link's target, that is not in the
  repository, and an anchor on one -- a heading of a Markdown file, an `id`
  of a site page -- that is not there;
- a primitive of its list of in-tree cryptography (PRIMITIVES) whose
  function is gone (each needle is the item itself, `pub fn sha256(`, found
  outside the file's test module, so a test of the same name does not hold
  it up), one the list does not hold named in the file, or one the file
  does not name that the list holds;
- a stale claim, found by phrase (STALE, case-insensitive) in SECURITY.md,
  `crates/*/src` and `site/content/docs`, outside ALLOWED. `no TLS` was true
  until fennec-tls and went on being written -- in a code comment, a help
  text, a docs page -- after it was not; the messages a build made without
  the `tls` feature gives are true, each an ALLOWED entry with its reason. An
  entry that finds nothing is refused too: a stale allowance lets the next
  stale line through;
- the policy's two promises missing: a "Supported versions" section and
  its acknowledgement in working days, and the procedure for a release that
  carries a fix in RELEASING.md ("A security release").

It prints how many things it checked of each kind and fails when any one
kind is zero, not only the total: files scanned for stale phrases are most of
a total, so a file with every flag, path and link stripped would pass on them.
A pattern that stopped matching would otherwise pass for ever.

What a line scan cannot tell, and this does not claim to: a flag read by a
helper called from a parser rather than named in an arm (it is not found, so
it is reported -- add the arm's spelling or the flag's use); a comment it
does not see as one (a block comment, a `//` inside a string, a raw string);
test code outside a `#[cfg(test)] mod` at the start of a line; and which
binary a sentence means where no name is near its flag.

    python3 tools/security_md_check.py
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# The binaries whose argument parsers SECURITY.md's flags are read by, and
# the words the file says each by.
BINARIES = {
    "fennec-server": "crates/fennec-server/src",
    "fennec-shard": "crates/fennec-shard/src",
    "fennec-cli": "crates/fennec-cli/src",
}
SAID = {
    "fennec-server": re.compile(r"fennec-server", re.I),
    "fennec-shard": re.compile(r"fennec-shard|\brouter", re.I),
    "fennec-cli": re.compile(r"fennec-cli|fennec import|\bthe shell|`fennec [a-z]", re.I),
}
WINDOW = 240

# The cryptography we wrote, as SECURITY.md lists it: the words the file
# says it by (found case-insensitively), the file and the item that is it,
# each needle the item's own declaration. Only what is in the tree; TLS is
# rustls's and is not here.
PRIMITIVES = {
    "SHA-256": ("crates/fennec-http/src/crypto.rs", "pub fn sha256("),
    "HMAC-SHA-256": ("crates/fennec-http/src/crypto.rs", "pub fn hmac_sha256("),
    "PBKDF2-HMAC-SHA-256": ("crates/fennec-http/src/crypto.rs", "pub fn pbkdf2_sha256("),
    "ChaCha20-Poly1305": ("crates/fennec-http/src/crypto.rs", "pub fn seal_in_place("),
    "HChaCha20": ("crates/fennec-http/src/crypto.rs", "pub fn hchacha20("),
    "RSA PKCS#1 v1.5 verification": ("crates/fennec-http/src/crypto.rs", "pub struct RsaKey"),
    "ECDSA P-256 verification": ("crates/fennec-http/src/crypto/p256.rs", "pub fn verify_sha256("),
    "SCRAM-SHA-256": ("crates/fennec-wire/src/client.rs", "    fn scram("),
}
# A file's code before its test module.
TESTS = re.compile(r"^#\[cfg\(test\)\]\s*\n\s*(?:pub\s+)?mod\b", re.M)

# What is searched for, and where.
STALE = (
    r"no\s+tls",
    r"plain\s+http\s+still",
    r"latest\s+release\s+and\s+`?main",
    r"no\s+external\s+crates\*\*\s+in\s+the\s+shipped",
)
SEARCHED = ("SECURITY.md", "crates/*/src/**/*.rs", "site/content/docs/*.html")

# A line that says what is true: (file, a piece of the line, why). Each must
# find a line, once at least.
ALLOWED = (
    ("crates/fennec-http/src/conn.rs", "has no TLS: no value of it",
     "a build made without the `tls` feature has none: the type's doc says so"),
    ("crates/fennec-http/src/conn.rs", "this build has no TLS: it was built without",
     "the error of a build without the `tls` feature asked for HTTPS"),
    ("crates/fennec-http/src/conn.rs", "this build has no TLS (built without",
     "the same refusal, of `--tls-cert` and `--tls-key`"),
    ("crates/fennec-http/src/conn.rs", "is no TLS record's",
     "a plain request on a TLS listener: its first byte is not a record"),
    ("crates/fennec-http/src/dial.rs", "this build has no TLS (built without",
     "the error of `--tls-ca` in a build without the `tls` feature"),
    ("crates/fennec-http/src/dial.rs", "No TLS in this build",
     "an `https://` address refused, unsent, in a build without the feature"),
    ("crates/fennec-http/src/dial.rs", "No TLS session in a build",
     "the stand-in type of a build without the feature"),
    ("crates/fennec-server/src/main.rs", "this build has no TLS to ask it with",
     "the PostgreSQL follower's refusal in a build without the feature"),
    ("crates/fennec-abi/src/sync/mod.rs", "HTTP client has no TLS and should not grow one",
     "fennec-abi has zero dependencies and no TLS by design: the binding's own client dials"),
)

FLAG = re.compile(r"(?<![\w-])--[a-z][a-z0-9]*(?:-[a-z0-9]+)*")
ARM = re.compile(r'"(--[a-z][a-z0-9-]*)"\s*(?:=>|\|)|\|\s*"(--[a-z][a-z0-9-]*)"')
TICKS = re.compile(r"`([^`\n]+)`")
LINK = re.compile(r"\]\(([^)\s]+)\)|<(https?://[^>\s]+)>")
ROOTS = ("crates/", "tools/", "docs/", "examples/", "site/", ".github/", "web/", "integrations/")
SITE = "https://fennecdb.com"


def untested(text):
    """A source file's text before its `#[cfg(test)] mod`."""
    m = TESTS.search(text)
    return text[:m.start()] if m else text


def code_lines(text):
    """The lines of `text` with a `//` comment taken off (a `//` inside a
    string is taken for one too: it can only hide a flag, never invent one)."""
    for line in untested(text).splitlines():
        yield line.split("//", 1)[0]


def parser_flags(root):
    """Every flag an argument parser of each binary reads, by binary."""
    found = {}
    for name, d in BINARIES.items():
        found[name] = set()
        for f in sorted((root / d).rglob("*.rs")):
            for line in code_lines(f.read_text(errors="replace")):
                for m in ARM.finditer(line):
                    found[name].add(m.group(1) or m.group(2))
    return found


def slug(heading):
    """GitHub's anchor of a heading."""
    h = re.sub(r"`", "", heading.strip().lower())
    h = re.sub(r"[^\w\- ]", "", h)
    return h.replace(" ", "-")


def anchors(path):
    """The anchors a file has: a Markdown file's headings, a page's ids."""
    text = path.read_text(errors="replace")
    if path.suffix == ".md":
        out, fence = set(), False
        for line in text.splitlines():
            if line.startswith("```"):
                fence = not fence
            m = None if fence else re.match(r"#+\s+(.*)", line)
            if m:
                out.add(slug(m.group(1)))
        return out
    return set(re.findall(r'\bid="([^"]+)"', text))


def target(root, ref):
    """A path and an anchor a reference names, or None if it names no file
    of the repository: a URL that is not the site's, a flag, a command."""
    ref = ref.strip()
    anchor = None
    if ref.startswith(SITE + "/docs/") or ref.startswith("/docs/"):
        page, _, anchor = ref.split("/docs/", 1)[1].partition("#")
        return root / "site/content/docs" / (page.rstrip("/") + ".html"), anchor
    if "://" in ref or ref.startswith(("mailto:", "#", "--")):
        return (None, ref[1:]) if ref.startswith("#") else (None, None)
    path, _, anchor = ref.partition("#")
    path = path.rstrip("/")
    if path.startswith(ROOTS) or path in ("RELEASING.md", "README.md", "SECURITY.md", "web"):
        if re.fullmatch(r"[\w./@+-]+", path):
            return root / path, anchor
    return None, None


def check_references(root, text, notes):
    """Paths, links and anchors; returns problems and how many it checked."""
    problems, count = [], 0
    refs = [m.group(1) for m in TICKS.finditer(text)]
    for m in LINK.finditer(text):
        refs.append(m.group(1) or m.group(2))
    own = anchors_of_text(text)
    for ref in refs:
        path, anchor = target(root, ref)
        if path is None:
            if anchor and ref.startswith("#"):
                count += 1
                if anchor not in own:
                    problems.append(f"SECURITY.md links {ref}, a heading it has not")
            continue
        count += 1
        if not path.exists():
            problems.append(f"SECURITY.md names {ref}, which is not in the repository")
            continue
        if anchor:
            count += 1
            if path.is_file() and anchor not in anchors(path):
                problems.append(f"SECURITY.md links {ref}: {path.name} has no `{anchor}`")
    return problems, count


def anchors_of_text(text):
    return {slug(m.group(1)) for m in re.finditer(r"^#+\s+(.*)$", text, re.M)}


def meant(text, at):
    """The binaries the paragraph says of the flag at `at`: those named in
    the WINDOW characters before it, or all where none is."""
    start = max(text.rfind("\n\n", 0, at) + 2, text.rfind("\n- ", 0, at) + 1, at - WINDOW, 0)
    before = text[start:at]
    named = [b for b, rx in SAID.items() if rx.search(before)]
    return named or list(BINARIES)


def check_flags(root, text, notes):
    known = parser_flags(root)
    problems, seen = [], set()
    for m in FLAG.finditer(text):
        flag, who = m.group(0), meant(text, m.start())
        seen.add(flag)
        if not any(flag in known[b] for b in who):
            where = " or ".join(who) if len(who) < len(BINARIES) else "any binary"
            problems.append(f"SECURITY.md names {flag}, which {where}'s argument parser "
                            "does not read")
    return sorted(set(problems)), len(seen)


def check_primitives(root, text):
    problems = []
    low = " ".join(text.lower().split())
    for name, (file, needle) in PRIMITIVES.items():
        if name.lower() not in low:
            problems.append(f"SECURITY.md does not name {name}, which PRIMITIVES lists: "
                            "the list of what is written in-tree and the file must agree")
        path = root / file
        if not path.exists() or needle not in untested(path.read_text(errors="replace")):
            problems.append(f"SECURITY.md names {name}, but `{needle}` is not in {file}")
    return problems, len(PRIMITIVES)


def files_searched(root):
    for pattern in SEARCHED:
        yield from sorted(p for p in root.glob(pattern) if p.is_file())


def check_stale(root, allowed=ALLOWED, stale=STALE):
    problems, used = [], {i: 0 for i in range(len(allowed))}
    rx = re.compile("|".join(stale), re.I)
    scanned = 0
    for f in files_searched(root):
        rel = f.relative_to(root).as_posix()
        scanned += 1
        text = f.read_text(errors="replace")
        # A phrase broken over a line is still the phrase.
        for m in rx.finditer(text):
            start = text.rfind("\n", 0, m.start()) + 1
            end = text.find("\n", m.end())
            line = text[start:end if end >= 0 else len(text)]
            joined = " ".join(text[max(0, m.start() - 60):m.end() + 60].split())
            ok = False
            for i, (afile, piece, _why) in enumerate(allowed):
                if afile == rel and (piece in line or ("\n" in m.group(0) and piece in joined)):
                    used[i] += 1
                    ok = True
            if not ok:
                lineno = text.count("\n", 0, m.start()) + 1
                problems.append(f"{rel}:{lineno}: `{m.group(0)}` -- a stale claim, or an "
                                "ALLOWED entry for it with its reason")
    for i, n in used.items():
        if n == 0:
            problems.append(f"ALLOWED ({allowed[i][0]}, {allowed[i][1]!r}) finds nothing: "
                            "delete the entry")
    return problems, scanned


def check_promises(root, text):
    problems = []
    if not re.search(r"^## Supported versions\s*$", text, re.M):
        problems.append("SECURITY.md has no `## Supported versions`")
    if not re.search(r"acknowledged within \d+\s+working\s+days", text, re.I):
        problems.append("SECURITY.md gives no acknowledgement in working days")
    releasing = (root / "RELEASING.md").read_text(errors="replace")
    if not re.search(r"^## A security release\s*$", releasing, re.M):
        problems.append("RELEASING.md has no `## A security release`")
    return problems, 3


KINDS = ("flags", "paths and anchors", "primitives", "files searched", "promises")


def run(root, allowed=ALLOWED):
    """The problems, and how many things of each kind were checked."""
    text = (root / "SECURITY.md").read_text()
    problems, counts = [], {}
    for kind, result in zip(KINDS, (check_flags(root, text, None), check_references(root, text, None),
                                    check_primitives(root, text), check_stale(root, allowed),
                                    check_promises(root, text))):
        problems += result[0]
        counts[kind] = result[1]
    for kind, n in counts.items():
        if n == 0:
            problems.append(f"security_md_check: checked no {kind}: the pattern for them no "
                            "longer matches, or the file stopped naming any")
    return problems, counts


def main():
    problems, counts = run(ROOT)
    for p in problems:
        print(p, file=sys.stderr)
    total = sum(counts.values())
    if problems:
        print(f"security_md_check: {len(problems)} wrong, of {total} things checked", file=sys.stderr)
        return 1
    print(f"security_md_check: {total} things checked ("
          + ", ".join(f"{n} {k}" for k, n in counts.items()) + ")")
    return 0


if __name__ == "__main__":
    sys.exit(main())
