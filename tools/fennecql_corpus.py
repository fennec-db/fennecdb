#!/usr/bin/env python3
"""Every FennecQL text the repository holds, parsed by this tree and by
another, and the texts a release's own parser read, by this tree:
`make fennecql-corpus BASE=origin/main`, `make fennecql-corpus
BASE=v0.2.0` (docs/design/fennecql.md#the-corpus).

The rule it holds: no text that parsed reads as another statement, nor is
refused. A named statement is stored as its text and read again at every
open, its tree's digest held to the one stored
(docs/design/named-statements.md), so a text read otherwise would be an
API that changed under its callers; FennecQL is frozen through 1.x
(docs/design/compatibility.md).

The texts are harvested from the files git tracks:

- the docs' `<pre data-lang="fennecql">` and `"sql"` blocks, and every inline
  `<code>` that begins a statement (site/content);
- Markdown's ```fennecql and ```sql blocks, and every inline code span that
  begins a statement (docs/, README.md, every CLAUDE.md, each integration's);
- every string of the golden files and the sync's scenarios that begins a
  statement (integrations/*.json);
- every string literal of the Rust, JS, TS, Python, Go, C#, Swift, Kotlin
  and Dart sources that begins a statement, and every inline code span of
  their comments that does.

A block is a text whole, and each of its lines that begins a statement a
text of its own. Each text is read by fennec-ql's `corpus` example with
`parse`, `parse_exact` and, a definition, `stored_definition` -- as an open
reads one -- each statement written as a word: a statement a named
statement may hold, and a definition, as its canonical tree's digest
(`Definition::digest`); any other as a hash of its `Debug` form. The
canonical tree writes a field only where it holds something, so a field
added since BASE moves no text that does not use it.

BASE is a git ref, its tree built from `git archive` in a temporary
directory with this tree's example copied in, or the name of a corpus
frozen at a tag (`integrations/fennecql-corpus/<tag>.jsonl`): the texts the
tag's tree held, as the tag's own parser read them, written once by
`--freeze <tag>` and never again. The report counts the texts, those whose
reading changed and those refused now that parsed -- which must be none but
those ACCEPTED names -- those that parse now and were refused, and those
refused in other words.

    python3 tools/fennecql_corpus.py origin/main       # against a tree
    python3 tools/fennecql_corpus.py v0.2.0            # against the corpus frozen at it
    python3 tools/fennecql_corpus.py --freeze v0.2.0   # write that corpus, by the tag's code
    python3 tools/fennecql_corpus.py --list            # the texts, as JSON lines
"""

import html
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
HOME_CARGO = os.path.expanduser("~/.cargo/bin/cargo")
CARGO = os.environ.get("CARGO") or (HOME_CARGO if os.path.exists(HOME_CARGO) else "cargo")
EXAMPLE = os.path.join("crates", "fennec-ql", "examples", "corpus.rs")
FROZEN = os.path.join("integrations", "fennecql-corpus")

# A change made on purpose: a text whose reading changed, or that is
# refused now, let through by its entry. Each names the text as harvested,
# the pull request, why, and -- where a digest moved, a `c`, `e` or `s` word
# or a definition's stored reading -- the grammar epoch it was raised to:
# a digest moves only with `GRAMMAR_EPOCH` raised past BASE's, a major
# version (docs/design/compatibility.md#the-grammar-epoch), and the epoch's
# comment in crates/fennec-core/src/named.rs names its texts as 2 to 4 did.
# A text refused now whose stored reading holds, or a statement no
# definition holds read otherwise, needs no epoch: its entry says why.
# Under 1.x an entry is a break, made only where the old reading was a bug
# no caller could rely on -- and never one moving a digest, since 1.x keeps
# the epoch at 4.
#
#   {"text": "get t where x < 1e400", "pr": "#632",
#    "why": "an infinity, which JSON writes as null: refused now, and a "
#           "stored text reads it as it did"}
ACCEPTED = []

# The words a statement begins with: FennecQL's, SQL's that it reads, and
# those that are refused naming what to write. A text whose first word is
# another is no statement.
WORDS = {
    "get", "select", "put", "insert", "set", "update", "del", "delete", "create",
    "drop", "alter", "explain", "collections", "describe", "compact", "truncate",
    "begin", "start", "commit", "rollback", "call", "statements",
}

SOURCES = {
    ".rs": ('"',),
    ".js": ('"', "'", "`"),
    ".mjs": ('"', "'", "`"),
    ".cjs": ('"', "'", "`"),
    ".ts": ('"', "'", "`"),
    ".tsx": ('"', "'", "`"),
    ".py": ('"', "'"),
    ".go": ('"', "`"),
    ".cs": ('"',),
    ".swift": ('"',),
    ".kt": ('"',),
    ".dart": ('"', "'"),
}

# What a tree's build reads: an archive of these is enough to build its
# fennec-ql, and a tenth of the whole.
BUILT = ("Cargo.toml", "Cargo.lock", ".cargo", "crates")


def begins_statement(text):
    word = re.match(r"\s*([A-Za-z_]+)", text)
    return bool(word) and word.group(1).lower() in WORDS


def tracked(ref=None):
    """The paths git tracks: this checkout's, or a ref's tree."""
    cmd = ["git", "ls-tree", "-r", "-z", "--name-only", ref] if ref else ["git", "ls-files", "-z"]
    out = subprocess.run(cmd, cwd=ROOT, check=True, stdout=subprocess.PIPE).stdout.decode()
    return [p for p in out.split("\0") if p]


def read(root, path):
    try:
        with open(os.path.join(root, path), encoding="utf-8") as f:
            return f.read()
    except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
        return None


def block(add, text, where):
    """A block whole, and each of its lines that begins a statement."""
    add(text, where)
    for line in text.splitlines():
        if begins_statement(line):
            add(line, where)


def spans(add, text, where):
    """Each inline code span, between single backticks, that begins one."""
    for m in re.finditer(r"(?<!`)`([^`\n]+)`(?!`)", text):
        if begins_statement(m.group(1)):
            add(m.group(1), where)


def from_html(add, text, where):
    for m in re.finditer(r'<pre data-lang="(fennecql|sql)"[^>]*>(.*?)</pre>', text, re.S):
        body = re.sub(r"<[^>]+>", "", m.group(2))
        block(add, html.unescape(body), where)
    for m in re.finditer(r"<code>(.*?)</code>", text, re.S):
        code = html.unescape(re.sub(r"<[^>]+>", "", m.group(1)))
        if begins_statement(code):
            add(code, where)


def from_markdown(add, text, where):
    for m in re.finditer(r"^```(fennecql|sql)\s*\n(.*?)^```", text, re.S | re.M):
        block(add, m.group(2), where)
    spans(add, re.sub(r"^```.*?^```", "", text, flags=re.S | re.M), where)


def from_json(add, value, where):
    if isinstance(value, str):
        if begins_statement(value):
            block(add, value, where)
    elif isinstance(value, list):
        for v in value:
            from_json(add, v, where)
    elif isinstance(value, dict):
        for v in value.values():
            from_json(add, v, where)


def unescape(raw, quote):
    """A string literal's text, its common escapes read; a Rust line ending
    in a backslash joined to the next line's text."""
    out, i = [], 0
    while i < len(raw):
        c = raw[i]
        if c == "\\" and i + 1 < len(raw):
            n = raw[i + 1]
            if n == "\n":
                i += 2
                while i < len(raw) and raw[i] in " \t":
                    i += 1
                continue
            out.append({"n": "\n", "t": "\t", "r": "\r", "0": "\0"}.get(n, n))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def from_source(add, text, where, quotes):
    """String literals that begin a statement, and the comments' code spans."""
    rust = where.endswith(".rs")
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        # Comments: a line's, or a block's; their code spans are texts too.
        if text.startswith("//", i) or (c == "#" and where.endswith(".py")):
            end = text.find("\n", i)
            end = n if end < 0 else end
            spans(add, text[i:end], where)
            i = end
            continue
        if text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            spans(add, text[i:end], where)
            i = end
            continue
        # Rust's raw strings: r"..", r#".."#.
        if rust and c == "r" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            m = re.match(r'r(#*)"', text[i:])
            if m:
                close = '"' + m.group(1)
                start = i + len(m.group(0))
                end = text.find(close, start)
                if end < 0:
                    break
                body = text[start:end]
                if begins_statement(body):
                    block(add, body, where)
                i = end + len(close)
                continue
        # Rust's character literals and lifetimes hold no statement.
        if c in quotes:
            triple = text.startswith(c * 3, i) and c != "`"
            close = c * 3 if triple else c
            j = i + len(close)
            while j < n:
                if text[j] == "\\" and c != "`":
                    j += 2
                    continue
                if text.startswith(close, j):
                    break
                if not triple and c != "`" and text[j] == "\n" and not rust:
                    break
                j += 1
            body = text[i + len(close):j]
            if begins_statement(body):
                body = body if c == "`" else unescape(body, c)
                block(add, body, where)
            i = j + len(close)
            continue
        i += 1


def harvest(root=ROOT, paths=None):
    """The texts of the files `paths` names under `root`: by default this
    checkout's, as git tracks them."""
    texts, seen = [], set()

    def add(text, where):
        t = text.strip()
        if t and t not in seen:
            seen.add(t)
            texts.append({"text": t, "from": where})

    for path in tracked() if paths is None else paths:
        ext = os.path.splitext(path)[1]
        if path.startswith(("target/", "node_modules/")) or "/node_modules/" in path:
            continue
        if ext == ".html" and path.startswith("site/"):
            body = read(root, path)
            if body:
                from_html(add, body, path)
        elif ext == ".md":
            body = read(root, path)
            if body:
                from_markdown(add, body, path)
        elif ext == ".json" and path.startswith("integrations/"):
            body = read(root, path)
            try:
                from_json(add, json.loads(body), path) if body else None
            except ValueError:
                pass
        elif ext in SOURCES and not path.endswith(".min.js"):
            body = read(root, path)
            if body:
                from_source(add, body, path, SOURCES[ext])
    return texts


# ------------------------------------------------------------------ reading


def extract(ref, into, paths=None):
    """`ref`'s tree -- or those of `paths` it holds -- written under `into`
    from `git archive`: no worktree registered, nothing of git's left."""
    if paths is not None:
        held = set(subprocess.run(["git", "ls-tree", "--name-only", ref], cwd=ROOT, check=True,
                                  stdout=subprocess.PIPE).stdout.decode().split("\n"))
        paths = [p for p in paths if p in held]
    out = subprocess.run(["git", "archive", "--format=tar", ref, "--", *(paths or [])],
                         cwd=ROOT, check=True, stdout=subprocess.PIPE).stdout
    with tarfile.open(fileobj=io.BytesIO(out)) as tar:
        tar.extractall(into, filter="data")


def toolchain():
    """This tree's pinned compiler, which every tree is built with: a tag
    naming `stable` would otherwise build with whatever that is today."""
    text = read(ROOT, "rust-toolchain.toml") or ""
    m = re.search(r'^channel\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else None


def build(tree, target):
    """`tree`'s fennec-ql with this tree's `corpus` example, unoptimised --
    two trees build in a minute, where `release`'s LTO took several -- with
    overflow checks and debug assertions off, so a text reads as the
    released parser reads it."""
    if os.path.abspath(tree) != ROOT:
        os.makedirs(os.path.join(tree, os.path.dirname(EXAMPLE)), exist_ok=True)
        shutil.copy(os.path.join(ROOT, EXAMPLE), os.path.join(tree, EXAMPLE))
    env = dict(os.environ, CARGO_TARGET_DIR=target, CARGO_PROFILE_DEV_DEBUG="0",
               CARGO_PROFILE_DEV_OVERFLOW_CHECKS="false",
               CARGO_PROFILE_DEV_DEBUG_ASSERTIONS="false")
    channel = toolchain()
    if channel:
        env["RUSTUP_TOOLCHAIN"] = channel
    subprocess.run([CARGO, "build", "-q", "-p", "fennec-ql", "--example", "corpus"],
                   cwd=tree, env=env, check=True)
    return os.path.join(target, "debug", "examples", "corpus")


def run(exe, texts):
    """The grammar's epoch `exe` was built under, and each text's three
    readings by it, in order."""
    data = "\n".join(json.dumps(t["text"], ensure_ascii=False) for t in texts) + "\n"
    out = subprocess.run([exe], input=data.encode("utf-8"), stdout=subprocess.PIPE, check=True).stdout
    lines = out.decode().split("\n")
    epoch = int(lines[0].split()[1])
    rows = [tuple(line.split("\t")) for line in lines[1:len(texts) + 1]]
    assert len(rows) == len(texts) and all(len(r) == 3 for r in rows), (len(rows), len(texts))
    return epoch, rows


# ------------------------------------------------------------------ a frozen corpus


def frozen_path(name, root=ROOT):
    return os.path.join(root, FROZEN, f"{name}.jsonl")


def default_stored(parse):
    """A reading's stored one where the file leaves it out: the definition
    itself, where the text is one alone, else none (`-`)."""
    words = parse.split()
    one = parse.startswith("ok ") and len(words) == 2 and words[1].startswith("s")
    return parse if one else "-"


def frozen_rows(texts, rows):
    """The lines of a frozen corpus: each text a reading of which parsed,
    a refusal as `err` alone (its words are free to change), `exact` and
    `stored` only where they are not what `parse` makes them."""
    out = []
    for t, (parse, exact, stored) in zip(texts, rows):
        parse, exact, stored = (r if r.startswith(("ok", "-")) else "err" for r in (parse, exact, stored))
        if not any(r.startswith("ok") for r in (parse, exact, stored)):
            continue
        line = {"text": t["text"], "from": t["from"], "parse": parse}
        if exact != parse:
            line["exact"] = exact
        if stored != default_stored(parse):
            line["stored"] = stored
        out.append(line)
    return out


def read_frozen(path):
    """A frozen corpus' head, texts and readings."""
    with open(path, encoding="utf-8") as f:
        lines = [json.loads(line) for line in f if line.strip()]
    head, texts, rows = lines[0], [], []
    for line in lines[1:]:
        texts.append({"text": line["text"], "from": line["from"]})
        parse = line["parse"]
        rows.append((parse, line.get("exact", parse), line.get("stored", default_stored(parse))))
    return head, texts, rows


def freeze(tag):
    """The texts `tag`'s tree holds, read by `tag`'s own parser, written as
    the corpus frozen at it."""
    commit = subprocess.run(["git", "rev-parse", f"{tag}^{{commit}}"], cwd=ROOT, check=True,
                            stdout=subprocess.PIPE).stdout.decode().strip()
    scratch = tempfile.mkdtemp(prefix="fennec-corpus-freeze-")
    try:
        tree = os.path.join(scratch, "tree")
        extract(commit, tree)
        texts = harvest(tree, tracked(commit))
        exe = build(tree, os.path.join(ROOT, "target", "fennecql-corpus", "freeze"))
        epoch, rows = run(exe, texts)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    kept = frozen_rows(texts, rows)
    head = {"tag": tag, "commit": commit, "epoch": epoch, "texts": len(texts), "kept": len(kept),
            "written": f"python3 tools/fennecql_corpus.py --freeze {tag}: the tag's tree's texts, "
                       "each read by the tag's fennec-ql with this tree's corpus example; a text "
                       "every reading refused is left out"}
    path = frozen_path(tag)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        for line in [head] + kept:
            f.write(json.dumps(line, ensure_ascii=False, sort_keys=False) + "\n")
    print(f"{path}: {len(kept)} of {len(texts)} texts, epoch {epoch}, {tag} at {commit[:12]}")


# ------------------------------------------------------------------ judging


DIGESTS = ("c", "e", "s")
READINGS = ("parse", "parse_exact", "stored_definition")


def judge(head, base):
    """One reading against the other's: `same`, `changed` (both parsed),
    `refused` (it parsed before), `parses` (it was refused) or `reworded`."""
    if head == base:
        return "same"
    h_ok, b_ok = head.startswith("ok"), base.startswith("ok")
    if h_ok and b_ok:
        return "changed"
    if b_ok:
        return "refused"
    if h_ok:
        return "parses"
    # A frozen corpus keeps no refusal's words: any refusal is the same.
    if "err" in (head, base) and head.startswith("err") and base.startswith("err"):
        return "same"
    return "reworded"


def digested(reading):
    """Whether a reading holds a word a stored definition's digest is."""
    return reading.startswith("ok") and any(w[:1] in DIGESTS for w in reading.split()[1:])


def moves_digest(head_row, base_row):
    """Whether the change between two rows moves a digest: a `c`, `e` or
    `s` word read otherwise, or a definition's stored reading changed or
    refused -- what an open would mark broken."""
    for i, (h, b) in enumerate(zip(head_row, base_row)):
        verdict = judge(h, b)
        if i == 2 and verdict in ("changed", "refused"):
            return True
        if verdict == "changed" and (digested(h) or digested(b)):
            return True
    return False


def compare(texts, head_rows, base_rows, head_epoch, base_epoch, accepted=()):
    """Each text's readings against BASE's, grouped: `changed` and
    `refused` fail the check unless `accepted` lets the text through,
    `parses` and `reworded` are told."""
    entries = {a["text"]: a for a in accepted}
    groups = {k: [] for k in ("changed", "refused", "parses", "reworded", "accepted", "held")}
    same = 0
    for t, h, b in zip(texts, head_rows, base_rows):
        verdicts = [judge(x, y) for x, y in zip(h, b)]
        same += verdicts.count("same")
        if "changed" in verdicts or "refused" in verdicts:
            entry = entries.get(t["text"])
            if entry and not moves_digest(h, b):
                groups["accepted"].append(t)
                continue
            if entry and head_epoch > base_epoch and entry.get("epoch") == head_epoch:
                groups["accepted"].append(t)
                continue
            if entry:
                # Listed, but a digest moved under the epoch BASE read it
                # with: a stored definition would be marked broken.
                groups["held"].append(t)
            groups["changed" if "changed" in verdicts else "refused"].append(t)
        elif "parses" in verdicts:
            groups["parses"].append(t)
        elif "reworded" in verdicts:
            groups["reworded"].append(t)
    return same, groups


def report(base_name, how, texts, same, groups, out=sys.stdout):
    readings = len(texts) * len(READINGS)
    print(f"## FennecQL's corpus, against `{base_name}`\n", file=out)
    print(f"{how}: {len(texts)} texts, {readings} readings (`parse`, `parse_exact`, "
          f"`stored_definition`): {same} read alike.\n", file=out)
    print(f"- read otherwise: {len(groups['changed'])}", file=out)
    print(f"- refused now, parsed before: {len(groups['refused'])}", file=out)
    print(f"- accepted (ACCEPTED): {len(groups['accepted'])}", file=out)
    print(f"- parse now, refused before: {len(groups['parses'])}", file=out)
    print(f"- refused in other words: {len(groups['reworded'])}", file=out)
    titles = (("changed", "Read otherwise"), ("refused", "Refused now, parsed before"),
              ("held", "In ACCEPTED, but a digest moved with no grammar epoch raised past BASE's"),
              ("accepted", "Accepted"), ("parses", "Parse now, refused before"),
              ("reworded", "Refused in other words"))
    for key, title in titles:
        if groups[key]:
            print(f"\n### {title}\n", file=out)
            for t in groups[key]:
                print(f"- `{t['from']}`: {json.dumps(t['text'], ensure_ascii=False)[:200]}", file=out)


def main():
    args = sys.argv[1:]
    if args and args[0] == "--list":
        for t in harvest():
            print(json.dumps(t, ensure_ascii=False))
        return
    if args and args[0] == "--freeze":
        freeze(args[1])
        return
    base_name = args[0] if args else "origin/main"
    target = os.path.join(ROOT, "target", "fennecql-corpus")
    head_exe = build(ROOT, os.path.join(target, "head"))
    if os.path.exists(frozen_path(base_name)):
        head_info, texts, base_rows = read_frozen(frozen_path(base_name))
        base_epoch = head_info["epoch"]
        how = (f"frozen at {head_info['tag']} ({head_info['commit'][:12]}), as its own parser "
               f"read them under grammar epoch {base_epoch}")
    else:
        texts = harvest()
        scratch = tempfile.mkdtemp(prefix="fennec-corpus-base-")
        try:
            tree = os.path.join(scratch, "tree")
            extract(base_name, tree, BUILT)
            base_epoch, base_rows = run(build(tree, os.path.join(target, "base")), texts)
        finally:
            shutil.rmtree(scratch, ignore_errors=True)
        how = "this tree's texts, read by both"
    head_epoch, head_rows = run(head_exe, texts)
    same, groups = compare(texts, head_rows, base_rows, head_epoch, base_epoch, ACCEPTED)
    report(base_name, how, texts, same, groups)
    if groups["changed"] or groups["refused"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
