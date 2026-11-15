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
- the policy's promises missing: a "Supported versions" section and
  its acknowledgement in working days, the procedure for a release that
  carries a fix in RELEASING.md ("A security release"), and what was decided
  of an outside audit, said as "no outside audit has been made" and never
  the reverse (AUDITED), with its link to the threat model's section.

The threat model, `docs/design/security.md` (THREAT), is held the same way,
and in this program rather than a sibling of it: it reads the same paths,
anchors and flags, and one program is one line in CI's fmt job. Its table
of boundaries -- the one whose head has an "Enforced by" and a "Proved by"
column -- is read row by row, and the check refuses:

- fewer rows than MIN_BOUNDARIES (G2's exit: 15), or a row whose boundary
  links no heading of the file, where its section is;
- an enforcing item, `path::item` in backticks (`path::Type::method`), that
  is not defined at that path: in a Rust file a `fn`, `struct`, `enum`,
  `trait`, `type`, `const`, `static` or `mod` of that name outside the
  file's test module, a method's type a definition or an `impl` there; in a
  script a function, a class or a binding; in a workflow a job or a step;
- a cited test, `path::name`, that is not a test there: a Rust `#[test] fn`
  (never one `#[ignore]`d, which CI does not run), its file under
  `crates/*/tests/` a `mod` of the crate's `tests/all.rs` or a `[[test]]`
  of its Cargo.toml, else it is never built; a JS `test('<name>'` or
  `it(`; a Python function; a shell function, or the title a shell suite's
  helper is called with first. A renamed function and a deleted test fail
  here, the row naming them;
- a row with neither a cited test nor the words "no test": a boundary
  nothing proves says so, it is never left blank;
- and, as in SECURITY.md, a flag no parser reads and a path, link or anchor
  that is not there, a link read from the file's own folder.

It prints how many things it checked of each kind and fails when any one
kind is zero, not only the total: files scanned for stale phrases are most of
a total, so a file with every flag, path and link stripped would pass on them.
A pattern that stopped matching would otherwise pass for ever. The rows
saying "no test" are named on a line of their own: each is a finding still
open, not a pass.

What a line scan cannot tell, and this does not claim to: a flag read by a
helper called from a parser rather than named in an arm (it is not found, so
it is reported -- add the arm's spelling or the flag's use); a comment it
does not see as one (a block comment, a `//` inside a string, a raw string);
test code outside a `#[cfg(test)] mod` at the start of a line; and which
binary a sentence means where no name is near its flag. Nor whether a cited
test would fail with its enforcement taken away: that is the row's claim,
made by reading the test, and a reviewer's to try; this holds only that the
test and the item are still there by the names the row gives.

    python3 tools/security_md_check.py
"""

import functools
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
    "RSA PKCS#1 v1.5 verification": ("crates/fennec-http/src/crypto.rs", "pub struct RsaKey"),
    "ECDSA P-256 verification": ("crates/fennec-http/src/crypto/p256.rs", "pub fn verify_sha256("),
    "SCRAM-SHA-256": ("crates/fennec-wire/src/client.rs", "fn scram(&mut self"),
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

# The threat model, and the fewest boundaries its table may hold (the 1.0
# plan's G2: "at least 15 boundary rows").
THREAT = "docs/design/security.md"
MIN_BOUNDARIES = 15
# What SECURITY.md says of an outside audit: the decision, written so that it
# cannot be read as a review that was made. Its reverse is refused.
AUDIT_SAID = re.compile(r"no\s+outside\s+audit\s+has\s+been\s+made", re.I)
AUDITED = re.compile(r"\b(?:has|have)\s+been\s+(?:independently\s+|externally\s+)?audited\b|"
                     r"\baudited\s+by\b|\bpassed\s+an?\s+(?:outside\s+|external\s+)?audit\b", re.I)
AUDIT_LINK = THREAT + "#an-outside-audit"

FLAG = re.compile(r"(?<![\w-])--[a-z][a-z0-9]*(?:-[a-z0-9]+)*")
ARM = re.compile(r'"(--[a-z][a-z0-9-]*)"\s*(?:=>|\|)|\|\s*"(--[a-z][a-z0-9-]*)"')
TICKS = re.compile(r"`([^`\n]+)`")
LINK = re.compile(r"\]\(([^)\s]+)\)|<(https?://[^>\s]+)>")
ROOTS = ("crates/", "tools/", "docs/", "examples/", "site/", ".github/", "web/", "integrations/")
SITE = "https://fennecdb.com"


@functools.lru_cache(maxsize=None)
def read(path):
    """A file's text, read once a run (`run` empties the caches): the
    threat model names some files dozens of times."""
    return path.read_text(errors="replace")


def untested(text):
    """A source file's text before its `#[cfg(test)] mod`."""
    m = TESTS.search(text)
    return text[:m.start()] if m else text


def code_lines(text):
    """The lines of `text` with a `//` comment taken off (a `//` inside a
    string is taken for one too: it can only hide a flag, never invent one)."""
    for line in untested(text).splitlines():
        yield line.split("//", 1)[0]


@functools.lru_cache(maxsize=None)
def parser_flags(root):
    """Every flag an argument parser of each binary reads, by binary."""
    found = {}
    for name, d in BINARIES.items():
        found[name] = set()
        for f in sorted((root / d).rglob("*.rs")):
            for line in code_lines(read(f)):
                for m in ARM.finditer(line):
                    found[name].add(m.group(1) or m.group(2))
    return found


def slug(heading):
    """GitHub's anchor of a heading."""
    h = re.sub(r"`", "", heading.strip().lower())
    h = re.sub(r"[^\w\- ]", "", h)
    return h.replace(" ", "-")


@functools.lru_cache(maxsize=None)
def anchors(path):
    """The anchors a file has: a Markdown file's headings, a page's ids."""
    text = read(path)
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


def linked(root, base, ref):
    """A link's path and anchor, read from `base`, the folder of the file
    that holds it; (None, None) where it names no file of the repository."""
    site = ref.startswith((SITE + "/docs/", "/docs/"))
    if base == root or site or "://" in ref or ref.startswith(("mailto:", "#", "--")):
        return target(root, ref)
    path, _, anchor = ref.partition("#")
    if not re.fullmatch(r"[\w./@+-]+", path):
        return None, None
    return base / path.rstrip("/"), anchor


def check_references(root, text, notes, name="SECURITY.md"):
    """Paths, links and anchors; returns problems and how many it checked.
    A backticked path is the repository's; a link is read from the folder
    of the file `name` holding it."""
    problems, count = [], 0
    base = (root / name).parent
    refs = [(m.group(1), False) for m in TICKS.finditer(text)]
    for m in LINK.finditer(text):
        refs.append((m.group(1) or m.group(2), True))
    own = anchors_of_text(text)
    for ref, link in refs:
        # `#[test]` in backticks is Rust, not a link to a heading.
        if not link and ref.startswith("#") and not re.fullmatch(r"#[\w-]+", ref):
            continue
        path, anchor = linked(root, base, ref) if link else target(root, ref)
        if path is None:
            if anchor and ref.startswith("#"):
                count += 1
                if anchor not in own:
                    problems.append(f"{name} links {ref}, a heading it has not")
            continue
        count += 1
        if not path.exists():
            problems.append(f"{name} names {ref}, which is not in the repository")
            continue
        if anchor:
            count += 1
            if path.is_file() and anchor not in anchors(path):
                problems.append(f"{name} links {ref}: {path.name} has no `{anchor}`")
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


def check_flags(root, text, notes, name="SECURITY.md"):
    known = parser_flags(root)
    problems, seen = [], set()
    for m in FLAG.finditer(text):
        flag, who = m.group(0), meant(text, m.start())
        seen.add(flag)
        if not any(flag in known[b] for b in who):
            where = " or ".join(who) if len(who) < len(BINARIES) else "any binary"
            problems.append(f"{name} names {flag}, which {where}'s argument parser "
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
    # The decision on an outside audit (the 1.0 plan's G14): said as what was
    # not done, linked to the scope an audit would take, and never turned
    # into a claim of a review nobody made.
    if not AUDIT_SAID.search(text):
        problems.append("SECURITY.md does not say \"no outside audit has been made\": the "
                        "decision on an outside audit is written there")
    if AUDIT_LINK not in text:
        problems.append(f"SECURITY.md does not link {AUDIT_LINK}, the decision's long form")
    for m in AUDITED.finditer(text):
        problems.append(f"SECURITY.md says `{m.group(0)}`: no outside audit has been made, "
                        "and the file says so, never the reverse")
    return problems, 4


# ------------------------------------------------------------ the threat model

# `path::item`: a file of the repository, by its extension one this reads,
# and what is defined in it.
ITEM = re.compile(r"^([\w./@+-]+\.(rs|js|mjs|ts|py|sh|ps1|yml|yaml))::(.+)$")
NO_TEST = re.compile(r"\bno\s+test\b", re.I)
SCRIPTS = ("js", "mjs", "ts")


def cells(line):
    """A table row's cells, a `|` inside backticks or escaped kept."""
    out, cur, code, i = [], [], False, 0
    line = line.strip()
    if line.startswith("|"):
        line = line[1:]
    if line.endswith("|") and not line.endswith("\\|"):
        line = line[:-1]
    while i < len(line):
        c = line[i]
        if c == "\\" and i + 1 < len(line) and line[i + 1] == "|":
            cur.append("|")
            i += 2
            continue
        if c == "`":
            code = not code
        if c == "|" and not code:
            out.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
        i += 1
    out.append("".join(cur).strip())
    return out


def boundary_rows(text):
    """The rows of the table whose head has "Enforced by" and "Proved by":
    (line number, boundary cell, enforced cell, proved cell) each, or None
    where the file holds no such table."""
    lines = text.splitlines()
    for at, line in enumerate(lines):
        if not line.lstrip().startswith("|"):
            continue
        head = [c.lower() for c in cells(line)]
        if "enforced by" not in head or "proved by" not in head:
            continue
        b = next((i for i, c in enumerate(head) if c in ("boundary", "trust boundary")), 0)
        e, p = head.index("enforced by"), head.index("proved by")
        rows = []
        for n in range(at + 2, len(lines)):
            if not lines[n].lstrip().startswith("|"):
                break
            row = cells(lines[n])
            row += [""] * (len(head) - len(row))
            rows.append((n + 1, row[b], row[e], row[p]))
        return rows
    return None


def rust_items(text, name):
    """Where `name` is declared in Rust source `text`."""
    return re.search(r"\b(?:fn|struct|enum|trait|type|const|static|mod|union)\s+" + re.escape(name)
                     + r"\b|\bmacro_rules!\s*" + re.escape(name) + r"\b", text)


def rust_owner(text, name):
    """Whether `text` defines the type or module `name`, or implements it."""
    return rust_items(text, name) or re.search(
        r"\bimpl\b(?:\s*<[^{;]*?>)?\s+(?:[\w:<>, ]+\s+for\s+)?(?:\w+::)*" + re.escape(name) + r"\b", text)


def script_item(text, name):
    """A function, a class, a binding or a method `name` in a script."""
    n = re.escape(name)
    return re.search(r"\b(?:function\s*\*?\s*|class\s+|const\s+|let\s+|var\s+)" + n + r"\b"
                     r"|^\s*(?:static\s+)?(?:async\s+)?#?" + n + r"\s*\([^)\n]*\)[^{;\n]*\{", text, re.M)


def declared(root, ref, test):
    """The problem with `path::item` -- an enforcing item, or a test where
    `test` -- or None where it is there by that name."""
    m = ITEM.match(ref)
    if not m:
        return None
    rel, ext, item = m.group(1), m.group(2), m.group(3).strip()
    path = root / rel
    what = "test" if test else "item"
    if not path.is_file():
        return f"{THREAT} cites `{ref}`: {rel} is not in the repository"
    text = read(path)
    gone = f"{THREAT} cites `{ref}`: no {what} `{item}` in {rel}"
    if ext == "rs":
        *owners, name = item.split("::")
        if test:
            found = re.search(r"#\[test\]\s*((?:#\[[^\]]*\]\s*|//[^\n]*\n\s*)*)(?:pub\s+)?(?:async\s+)?fn\s+"
                              + re.escape(name) + r"\s*\(", text)
            if not found:
                return gone
            if "#[ignore" in found.group(1):
                return f"{THREAT} cites `{ref}`, an #[ignore]d test: CI does not run it"
            return unbuilt(root, rel, ref)
        code = untested(text)
        if not rust_items(code, name):
            return gone + " (outside its test module)"
        for owner in owners:
            if not rust_owner(code, owner):
                return f"{THREAT} cites `{ref}`: {rel} defines no `{owner}`"
        return None
    if ext in SCRIPTS:
        if test:
            titled = re.search(r"\b(?:test|it)\(\s*(['\"`])" + re.escape(item) + r"\1", text)
            # A check script with no test() of its own is proved by its
            # functions, which it runs.
            plain = not re.search(r"\b(?:test|it)\(\s*['\"`]", text) and script_item(text, item)
            return None if titled or plain else gone
        return None if script_item(text, item) else gone
    if ext == "py":
        return None if re.search(r"^\s*def\s+" + re.escape(item) + r"\s*\(", text, re.M) or (
            not test and re.search(r"^" + re.escape(item) + r"\s*=", text, re.M)) else gone
    if ext == "sh":
        # A function; or, of a test, the title its helper is called with
        # first (`expect "tampered: installs nothing" absent ...`).
        return None if re.search(r"^\s*(?:function\s+)?" + re.escape(item) + r"\s*\(\)", text, re.M) or (
            test and re.search(r"^\s*[\w-]+\s+([\"'])" + re.escape(item) + r"\1", text, re.M)) else gone
    if ext == "ps1":
        return None if re.search(r"^\s*function\s+" + re.escape(item) + r"\b", text, re.M | re.I) else gone
    # A workflow: a job's id, or a step's name.
    return None if re.search(r"^\s*" + re.escape(item) + r":\s*$|^\s*-?\s*name:\s*['\"]?"
                             + re.escape(item) + r"['\"]?\s*$", text, re.M) else gone


def unbuilt(root, rel, ref):
    """A test file under `crates/*/tests/` that no target builds: neither a
    `mod` of its crate's `tests/all.rs` nor a `[[test]]` of its own, it is
    never compiled (`autotests = false`), and its tests prove nothing."""
    m = re.fullmatch(r"(crates/[\w-]+)/tests/([\w-]+)\.rs", rel)
    if not m or m.group(2) == "all":
        return None
    crate, stem = m.group(1), m.group(2)
    all_rs = root / crate / "tests/all.rs"
    if all_rs.is_file() and re.search(r"^\s*(?:pub\s+)?mod\s+" + re.escape(stem) + r"\s*;",
                                      read(all_rs), re.M):
        return None
    cargo = root / crate / "Cargo.toml"
    if cargo.is_file() and f'path = "tests/{stem}.rs"' in read(cargo):
        return None
    if cargo.is_file() and "autotests = false" not in read(cargo):
        return None
    return f"{THREAT} cites `{ref}`: {rel} is not built -- no `mod {stem};` in tests/all.rs, no [[test]]"


def check_threat_model(root, name=THREAT):
    """The threat model's table: its rows, each enforcing item and each
    cited test, and the rows that say "no test". Returns the problems, the
    counts of rows, items and tests, and the rows saying "no test"."""
    path = root / name
    if not path.is_file():
        return [f"{name} is not in the repository: the threat model is"], (0, 0, 0), []
    text = path.read_text(errors="replace")
    rows = boundary_rows(text)
    if rows is None:
        return [f"{name} has no table of boundaries (a head with \"Enforced by\" and "
                "\"Proved by\")"], (0, 0, 0), []
    problems, items, tests, untested_rows = [], 0, 0, []
    own = anchors_of_text(text)
    if len(rows) < MIN_BOUNDARIES:
        problems.append(f"{name} has {len(rows)} boundaries, fewer than {MIN_BOUNDARIES}")
    for line, boundary, enforced, proved in rows:
        title = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", boundary) or f"line {line}"
        where = f"{name}:{line} ({title})"
        links = re.findall(r"\]\(#([^)\s]+)\)", boundary)
        if not links or any(a not in own for a in links):
            problems.append(f"{where}: the boundary links no section of the file")
        named = [m.group(1) for m in TICKS.finditer(enforced) if ITEM.match(m.group(1))]
        if not named:
            problems.append(f"{where}: names no enforcing item (`path::item`)")
        for ref in named:
            items += 1
            p = declared(root, ref, test=False)
            if p:
                problems.append(f"{where}: {p}")
        cited = [m.group(1) for m in TICKS.finditer(proved) if ITEM.match(m.group(1))]
        said = NO_TEST.search(re.sub(r"`[^`]*`", "", proved))
        if not cited and not said:
            problems.append(f"{where}: cites no test and does not say \"no test\"")
        if said:
            untested_rows.append(title)
        for ref in cited:
            tests += 1
            p = declared(root, ref, test=True)
            if p:
                problems.append(f"{where}: {p}")
    return problems, (len(rows), items, tests), untested_rows


KINDS = ("flags", "paths and anchors", "primitives", "files searched", "promises",
         "boundaries", "enforcing items", "cited tests", "threat model's paths and anchors",
         "threat model's flags")


def run(root, allowed=ALLOWED, notes=None):
    """The problems, and how many things of each kind were checked. `notes`,
    a list, is given the boundaries whose rows say "no test"."""
    for cached in (read, anchors, parser_flags):
        cached.cache_clear()
    text = (root / "SECURITY.md").read_text()
    threat = (root / THREAT).read_text(errors="replace") if (root / THREAT).is_file() else ""
    model, (rows, items, tests), open_rows = check_threat_model(root)
    results = (check_flags(root, text, None), check_references(root, text, None),
               check_primitives(root, text), check_stale(root, allowed),
               check_promises(root, text), (model, rows), ([], items), ([], tests),
               check_references(root, threat, None, THREAT), check_flags(root, threat, None, THREAT))
    problems, counts = [], {}
    for kind, result in zip(KINDS, results):
        problems += result[0]
        counts[kind] = result[1]
    for kind, n in counts.items():
        if n == 0:
            problems.append(f"security_md_check: checked no {kind}: the pattern for them no "
                            "longer matches, or the file stopped naming any")
    if notes is not None:
        notes.extend(open_rows)
    return problems, counts


def main():
    notes = []
    problems, counts = run(ROOT, notes=notes)
    for p in problems:
        print(p, file=sys.stderr)
    total = sum(counts.values())
    if problems:
        print(f"security_md_check: {len(problems)} wrong, of {total} things checked", file=sys.stderr)
        return 1
    print(f"security_md_check: {total} things checked ("
          + ", ".join(f"{n} {k}" for k, n in counts.items()) + ")")
    if notes:
        print(f"security_md_check: {len(notes)} boundaries of {THREAT} say \"no test\": "
              + "; ".join(notes))
    return 0


if __name__ == "__main__":
    sys.exit(main())
