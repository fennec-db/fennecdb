#!/usr/bin/env python3
"""Every FennecQL text the repository holds, parsed by this tree and by
another, and the texts a release's own parser read, by this tree:
`make fennecql-corpus BASE=origin/main`, `make fennecql-corpus
BASE=frozen` (docs/design/fennecql.md#the-corpus).

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
  their comments that does;
- each line of `integrations/fennecql-corpus/kinds.txt`, written by hand,
  at least five texts of every kind of statement and every SQL spelling the
  parser reads, and two of every SQL word it refuses by name.

A block is a text whole, and each of its lines that begins a statement a
text of its own. Each text is read by fennec-ql's `corpus` example with
`parse`, `parse_exact` and `stored_definition` -- as an open reads one --
each statement written as a word: a statement a named statement may hold,
and a definition, as its canonical tree's digest (`Definition::digest`);
any other as a hash of its form, its pretty `Debug` with every named field
of a struct holding an empty value left out. Either way a field added
since BASE that a text does not use moves nothing.

BASE is a git ref -- its tree built from `git archive` with this tree's
example copied in, both trees' texts read, and every corpus frozen in it
held to be in this checkout as it is there -- or the name of a corpus
frozen at a tag (`integrations/fennecql-corpus/<tag>.jsonl`): the texts the
tag's tree held, the list's and those of the corpora frozen before it, as
the tag's own parser read them, written once by `--freeze <tag>` and never
replaced. `frozen` is every one of them. The report counts the texts, by
kind, those whose reading changed and those refused now that parsed --
which must be none but those ACCEPTED names -- those the parser panics on,
those that parse now and were refused, and those refused in other words,
and prints every `same_meaning` an entry claims; and for each failing text
what changed, and its ACCEPTED entry ready to paste, every one of them
written to a file beside the builds.

    python3 tools/fennecql_corpus.py origin/main       # against a tree
    python3 tools/fennecql_corpus.py v0.2.0            # against the corpus frozen at it
    python3 tools/fennecql_corpus.py --frozen          # against every frozen corpus
    python3 tools/fennecql_corpus.py --freeze v1.0.0   # write a tag's corpus, by the tag's code
    python3 tools/fennecql_corpus.py --list            # the texts, as JSON lines

The builds go under `target/fennecql-corpus/`, or `$FENNECQL_CORPUS_TARGET`
(CI's, outside the directory its cache saves): this checkout's
`head-<its path's hash>`, a base commit's `base-<commit>`, a tag's
`freeze-<commit>`; the trees unpacked, `trees/<role>-<pid>`; a failure's
entries, `accepted-<BASE>.txt`.
"""

import collections
import difflib
import hashlib
import html
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
HOME_CARGO = os.path.expanduser("~/.cargo/bin/cargo")
CARGO = os.environ.get("CARGO") or (HOME_CARGO if os.path.exists(HOME_CARGO) else "cargo")
EXAMPLE = os.path.join("crates", "fennec-ql", "examples", "corpus.rs")
FROZEN = os.path.join("integrations", "fennecql-corpus")
# Written by hand, beside the frozen files: the kinds of statement the
# harvest holds few of, five texts each at least (`REQUIRED_KINDS`).
LISTED = "integrations/fennecql-corpus/kinds.txt"

# A change made on purpose, let through by its entry: one change of one
# text, named by the text as harvested and both readings -- `base` and
# `head`, each `[parse, parse_exact, stored_definition]` with a refusal's
# words written `err`, as a failure prints them ready to paste -- the pull
# request and why. An entry lets that change through and no other: a later
# change of the same text fails again, and an entry matching no difference
# approves nothing (it may stay, as the history of a change).
#
# Where a digest moves -- a `c`, `e` or `s` word read otherwise, or a
# definition's stored reading changed or refused, what an open marks broken
# -- and where a text a 1.x BASE read is refused, the entry also names the
# grammar epoch it raised `GRAMMAR_EPOCH` to, past BASE's: a major version
# (docs/design/compatibility.md#the-grammar-epoch), and the epoch's comment
# in crates/fennec-core/src/named.rs names its texts as 2 to 4 did. Against
# a 0.x BASE -- a 0.x tag's frozen corpus, or main before 1.0 -- a refusal
# whose stored reading holds needs no epoch: 0.x promised nothing.
#
# A statement no definition holds read otherwise against a 1.x BASE -- a
# `d` word moved -- is either: the word hashes a tree's form, which moves
# where the tree changed shape and the meaning did not (a field renamed) as
# where the meaning changed (`drop collection if exists t` read without its
# `if exists`), and nothing here tells the two apart. So it passes with the
# epoch raised, as a moved digest does, or with `same_meaning`: why the
# statement means what it meant, a claim of its own -- never `why` -- which
# the summary prints, every one, for a reviewer to read. An empty one is
# refused (`entry_errors`). Against a 0.x BASE an entry alone is enough.
#
#   {"text": "get t where x < 1e400", "base": ["ok c...", "ok c...", "-"],
#    "head": ["err", "err", "-"], "pr": "#632",
#    "why": "an infinity, which JSON writes as null: refused now"}
#   {"text": "drop collection if exists t", "base": ["ok d...", "ok d...", "-"],
#    "head": ["ok d...", "ok d...", "-"], "pr": "#700", "why": "...",
#    "same_meaning": "`if_exists` renamed `exists`: the tree's field, read as before"}
ACCEPTED = [
    {"text": "put t {fs: [1e-400]}", "base": ["ok c4051d1fedcec68ee", "ok c05485bf748ced6ad", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "create statement big() as get t where x < 1e400 and y > -1e-400 select id", "base": ["ok s5aefe2bf1e46f4cf", "ok s5aefe2bf1e46f4cf", "ok s5aefe2bf1e46f4cf"], "head": ["err", "err", "ok s5aefe2bf1e46f4cf"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range; the stored definition reads as it did"},
    {"text": "create statement big() as get t where x < 1e400 select id", "base": ["ok s11545a8501530bd3", "ok s11545a8501530bd3", "ok s11545a8501530bd3"], "head": ["err", "err", "ok s11545a8501530bd3"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range; the stored definition reads as it did"},
    {"text": "create statement lists() public as insert t {xs: [1e-400, 1], ys: [2.5e999], z: -1e400}", "base": ["ok s67068334dc466c11", "ok s258f2c1294ef4ba2", "ok s67068334dc466c11"], "head": ["err", "err", "ok s67068334dc466c11"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range; the stored definition reads as it did"},
    {"text": "create statement q() as SELECT id FROM t ORDER BY v <#> '[1, 1e400]' LIMIT 2", "base": ["ok se3ae2fdcbd41a508", "ok se3ae2fdcbd41a508", "ok se3ae2fdcbd41a508"], "head": ["err", "err", "ok se3ae2fdcbd41a508"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range; the stored definition reads as it did"},
    {"text": "get t where x < 1e400", "base": ["ok cad014cb9665d33d1", "ok cad014cb9665d33d1", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "get t where x > -1e-400", "base": ["ok cc90141440e5b81dd", "ok cc90141440e5b81dd", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "put t {x: 2.5E+999}", "base": ["ok ca33cbe334e2c93d8", "ok ca33cbe334e2c93d8", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "put t {v: [0.5, 1e-999]}", "base": ["ok c0675610c35906d9c", "ok ce100a99b0dc7929e", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "get t near v [1, -1.8e308] limit 1", "base": ["ok c8ed5902faadf6532", "ok c44cb45bedaa3bc32", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "get t where x in [1, 1_0e400]", "base": ["ok c69d969548c1b9be4", "ok c69d969548c1b9be4", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "SELECT id FROM t ORDER BY v <#> '[1, 1e400]' LIMIT 2", "base": ["ok c37a5377a8c3da0a3", "ok c37a5377a8c3da0a3", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "put t {x: 1e400}", "base": ["ok ca33cbe334e2c93d8", "ok ca33cbe334e2c93d8", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "put t {x: -1e-400}", "base": ["ok c54855b06b0f291ae", "ok c54855b06b0f291ae", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "put t {xs: [0.5, 2e999]}", "base": ["ok cb849c38c41074a00", "ok cf71828d404f330a1", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
    {"text": "get t where x > 1e309", "base": ["ok c0b69f2627a9549bc", "ok c0b69f2627a9549bc", "-"], "head": ["err", "err", "-"], "pr": "#632", "why": "a number past a float's range, read before as an infinity or as 0: refused now naming the range"},
]

# The words a statement begins with: FennecQL's, SQL's that it reads, and
# those that are refused naming what to write. A text whose first word is
# another is no statement.
WORDS = {
    "get", "select", "put", "insert", "set", "update", "del", "delete", "create",
    "drop", "alter", "explain", "collections", "describe", "compact", "truncate",
    "begin", "start", "commit", "rollback", "call", "statements",
}

# A text's kind, its first statement's leading words: the longest of these
# it begins with, else its first word (`kind`).
KINDS = (
    "create or replace statement", "create unique index", "create statement",
    "create collection", "create table", "create index", "drop statement",
    "drop collection", "drop table", "drop index", "alter collection",
    "alter table", "explain get", "explain select", "describe statement",
    "insert into", "delete from", "start transaction",
)

# The kinds the hand-written list holds five texts of at least: every
# statement fennec-ql reads, every SQL spelling of one, and every SQL word
# it refuses naming what to write.
REQUIRED_KINDS = (
    "get", "put", "insert", "set", "del", "explain get", "collections", "describe",
    "compact", "create collection", "drop collection", "create index",
    "create unique index", "drop index", "alter collection", "create statement",
    "create or replace statement", "drop statement", "statements",
    "describe statement", "call",
    "select", "insert into", "update", "delete from", "explain select",
    "create table", "alter table", "drop table", "call (sql)", "truncate",
    "begin", "start transaction", "commit", "rollback",
)

# The other statements fennec-ql's parser refuses by name, naming what to
# write (`Parser::statement`, `create`, `drop`, `alter`, `explain`): the
# hand-written list holds two texts at least of each path of a refusal,
# where a kind read has many paths to hold. Matched from a text's start.
REFUSED_KINDS = (
    ("create function", r"create\s+function\b"),
    ("create procedure", r"create\s+procedure\b"),
    ("create or replace function", r"create\s+or\s+replace\s+(function|procedure)\b"),
    ("create or replace, no statement", r"create\s+or\s+replace\s+(?!statement\b|function\b|procedure\b)\w"),
    ("create trigger", r"create\s+trigger\b"),
    ("create extension", r"create\s+extension\b"),
    ("create temp table", r"create\s+(temp|temporary|unlogged)\b"),
    ("drop function", r"drop\s+function\b"),
    ("drop procedure", r"drop\s+procedure\b"),
    ("drop trigger", r"drop\s+trigger\b"),
    ("a leading join", r"(join|inner|left|right|full|cross)\b"),
    ("a name, then join", r"(?!(join|inner|left|right|full|cross|outer)\b)[A-Za-z_]\w*\s+join\b"),
    ("alter ... modify", r"alter\s+collection\s+\S+\s+(alter|modify|set|change|retype)\s+(?!(field\s+)?\S+\s*@)"),
    ("alter field @other", r"alter\s+collection\s+\S+\s+alter\s+(field\s+)?\S+\s*@(?!ttl\b|sorted\b)"),
    ("alter collection, another change",
     r"alter\s+collection\s+\S+\s+(?!(add|drop|rename|alter|modify|set|change|retype)\b)\w"),
    ("explain, no get", r"explain\s+(?!get\b|select\b)\w"),
)

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

# How much of a failure is printed: the texts shown whole, and the lines of
# each reading's forms that differ.
SHOWN_TEXTS = 20
SHOWN_LINES = 12


def begins_statement(text):
    word = re.match(r"\s*([A-Za-z_]+)", text)
    return bool(word) and word.group(1).lower() in WORDS


def kind(text):
    """A text's kind: the leading words of its first statement, lowered --
    SQL's `CALL f(...)` apart from FennecQL's `call`."""
    words = [w.lower() for w in re.findall(r"[A-Za-z_]+", text)[:4]]
    lead = " ".join(words)
    for k in KINDS:
        if lead == k or lead.startswith(k + " "):
            return k
    if words[:1] == ["call"] and re.match(r"\s*call\s+[A-Za-z_][\w.]*\s*\(", text, re.I):
        return "call (sql)"
    return words[0] if words else ""


def tracked(ref=None, repo=ROOT):
    """The paths git tracks: this checkout's, or a ref's tree."""
    cmd = ["git", "ls-tree", "-r", "-z", "--name-only", ref] if ref else ["git", "ls-files", "-z"]
    out = subprocess.run(cmd, cwd=repo, check=True, stdout=subprocess.PIPE).stdout.decode()
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


def from_list(add, text, where):
    """The hand-written list: a text a line, `#` a comment's line. A text
    is kept as written, a refusal too: each line is one."""
    for line in text.splitlines():
        if line.strip() and not line.lstrip().startswith("#"):
            add(line, where)


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
        if path == LISTED:
            body = read(root, path)
            if body:
                from_list(add, body, path)
        elif ext == ".html" and path.startswith("site/"):
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


def merged(*lists):
    """Texts from several harvests, each once, the first's `from` kept."""
    out, seen = [], set()
    for texts in lists:
        for t in texts:
            if t["text"] not in seen:
                seen.add(t["text"])
                out.append(t)
    return out


# ------------------------------------------------------------------ reading


def commit_of(ref, repo=ROOT):
    return subprocess.run(["git", "rev-parse", f"{ref}^{{commit}}"], cwd=repo, check=True,
                          stdout=subprocess.PIPE).stdout.decode().strip()


def extract(ref, into, paths=None, repo=ROOT):
    """`ref`'s tree -- or those of `paths` it holds -- written under `into`
    from `git archive`: no worktree registered, nothing of git's left. Its
    files keep the commit's time, older than any build since: a target is
    a commit's own (`target_for`), never another tree's reused."""
    if paths is not None:
        held = set(subprocess.run(["git", "ls-tree", "--name-only", ref], cwd=repo, check=True,
                                  stdout=subprocess.PIPE).stdout.decode().split("\n"))
        paths = [p for p in paths if p in held]
    out = subprocess.run(["git", "archive", "--format=tar", ref, "--", *(paths or [])],
                         cwd=repo, check=True, stdout=subprocess.PIPE).stdout
    with tarfile.open(fileobj=io.BytesIO(out)) as tar:
        tar.extractall(into, filter="data")


def unpacked(root, role):
    """A directory under the builds' root to unpack a tree into,
    `trees/<role>-<pid>`, empty: a run killed before it removed its own
    leaves it for the next run's `clear_leftovers`, where one in the
    system's temporary directory stayed for good."""
    path = os.path.join(root, "trees", f"{role}-{os.getpid()}")
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path)
    return path


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def clear_leftovers(root):
    """The trees runs that are gone left under the builds' root, removed;
    a live run's -- another checkout's, sharing the root -- kept."""
    d = os.path.join(root, "trees")
    for name in os.listdir(d) if os.path.isdir(d) else []:
        m = re.fullmatch(r"[a-z]+-(\d+)", name)
        if m and not alive(int(m.group(1))):
            shutil.rmtree(os.path.join(d, name), ignore_errors=True)


def targets():
    """Where the builds go: `$FENNECQL_CORPUS_TARGET` -- CI's, outside
    the `target/` its cache saves, where two builds were 306 MB more of
    it -- or this checkout's `target/fennecql-corpus`."""
    return os.environ.get("FENNECQL_CORPUS_TARGET") or os.path.join(ROOT, "target", "fennecql-corpus")


def head_target(root, checkout=ROOT):
    """The target a checkout's own tree is built into: `head-<key>`, the
    key a hash of the checkout's absolute path, so no other checkout
    shares it. Two checkouts sharing one `$FENNECQL_CORPUS_TARGET` and one
    `head`, the second written before the first was built, the second was
    taken as built -- cargo's freshness is by file time and by paths within
    the workspace, alike in both -- and a change failing 6 texts passed
    (`test_two_checkouts_sharing_a_target_are_each_built`). One checkout's
    own changes are newer than its build, as cargo needs. Another
    checkout's is left alone: it may be building now."""
    where = os.path.realpath(os.path.abspath(checkout))
    return os.path.join(root, "head-" + hashlib.sha256(where.encode()).hexdigest()[:12])


def target_for(root, role, commit):
    """The target a commit's tree is built into, `<role>-<commit>`, the
    role's others removed. A tree unpacked from `git archive` keeps its
    commit's file times and sits at the path of the last one in cargo's
    eyes (a workspace's members are hashed by their path within it), so
    built into one target after another tree, it was taken as built: the
    last tree's reading, in a tenth of a second."""
    mine = f"{role}-{commit[:12]}"
    if os.path.isdir(root):
        for name in os.listdir(root):
            if name.startswith(role + "-") and name != mine:
                shutil.rmtree(os.path.join(root, name), ignore_errors=True)
    return os.path.join(root, mine)


def toolchain():
    """This tree's pinned compiler, which every tree is built with: a tag
    naming `stable` would otherwise build with whatever that is today."""
    text = read(ROOT, "rust-toolchain.toml") or ""
    m = re.search(r'^channel\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else None


def build_env(target):
    """A build's environment: unoptimised -- two trees build in a minute,
    where `release`'s LTO took several -- with overflow checks and debug
    assertions off, so a text reads as the released parser reads it; with
    this tree's compiler; and offline, so a tree's build reaches no
    network, whatever its lock file names."""
    env = dict(os.environ, CARGO_TARGET_DIR=target, CARGO_PROFILE_DEV_DEBUG="0",
               CARGO_PROFILE_DEV_OVERFLOW_CHECKS="false",
               CARGO_PROFILE_DEV_DEBUG_ASSERTIONS="false", CARGO_NET_OFFLINE="true")
    channel = toolchain()
    if channel:
        env["RUSTUP_TOOLCHAIN"] = channel
    return env


def build(tree, target, example=None):
    """`tree`'s fennec-ql with this tree's `corpus` example (or `example`),
    the path of the program built."""
    if os.path.abspath(tree) != ROOT or example:
        os.makedirs(os.path.join(tree, os.path.dirname(EXAMPLE)), exist_ok=True)
        shutil.copy(example or os.path.join(ROOT, EXAMPLE), os.path.join(tree, EXAMPLE))
    subprocess.run([CARGO, "build", "-q", "-p", "fennec-ql", "--example", "corpus"],
                   cwd=tree, env=build_env(target), check=True)
    return os.path.join(target, "debug", "examples", "corpus")


def run(exe, texts, forms=False):
    """The grammar's epoch `exe` was built under, and each text's three
    readings by it, in order: words, or (`forms`) each statement's form."""
    data = "\n".join(json.dumps(t["text"], ensure_ascii=False) for t in texts) + "\n"
    args = [exe, "forms"] if forms else [exe]
    out = subprocess.run(args, input=data.encode("utf-8"), stdout=subprocess.PIPE, check=True).stdout
    lines = out.decode().split("\n")
    epoch = int(lines[0].split()[1])
    rows = [tuple(line.split("\t")) for line in lines[1:len(texts) + 1]]
    assert len(rows) == len(texts) and all(len(r) == 3 for r in rows), (len(rows), len(texts))
    return epoch, rows


def unescape_line(s):
    """A reading as the example wrote it on one line, read back."""
    out, i = [], 0
    while i < len(s):
        if s[i] == "\\" and i + 1 < len(s):
            out.append({"t": "\t", "n": "\n", "r": "\r"}.get(s[i + 1], s[i + 1]))
            i += 2
        else:
            out.append(s[i])
            i += 1
    return "".join(out)


def forms_of(reading):
    """A `forms` reading's statements' forms, or None for a refusal."""
    r = unescape_line(reading)
    return r.split("\x1e")[1:] if r.startswith("ok") else None


def cargo_version(tree):
    """The workspace's version in `tree`'s Cargo.toml."""
    m = re.search(r'^version\s*=\s*"([^"]+)"', read(tree, "Cargo.toml") or "", re.M)
    return m.group(1) if m else "0"


def major(version):
    """A version's or a tag's major: `v0.2.0` 0, `1.0.0-rc.1` 1 -- a
    release candidate of 1.0 is held to 1.0's contract."""
    m = re.match(r"v?(\d+)", version)
    return int(m.group(1)) if m else 0


# ------------------------------------------------------------------ a frozen corpus


def frozen_path(name, root=ROOT):
    return os.path.join(root, FROZEN, f"{name}.jsonl")


def frozen_names(root=ROOT):
    """Every corpus frozen under `integrations/fennecql-corpus/`, by name:
    each a tag's, kept for good -- a text a later tree no longer holds is
    still held to the reading the tag gave it."""
    d = os.path.join(root, FROZEN)
    return sorted(f[:-len(".jsonl")] for f in os.listdir(d) if f.endswith(".jsonl")) if os.path.isdir(d) else []


def default_stored(parse):
    """A reading's stored one where the file leaves it out: the definition
    itself, where the text is one alone, else none (`-`)."""
    words = parse.split()
    one = parse.startswith("ok ") and len(words) == 2 and words[1].startswith("s")
    return parse if one else "-"


def norm(reading):
    """A reading as a frozen corpus and an entry keep it: a refusal's words
    are free to change, so any refusal is `err`."""
    return reading if reading.startswith(("ok", "-")) or reading == "panic" else "err"


def frozen_rows(texts, rows, keep=()):
    """The lines of a frozen corpus: each text a reading of which parsed,
    and each text of `keep` (the hand-written list) whatever it read, a
    refusal as `err` alone, `exact` and `stored` only where they are not
    what `parse` makes them."""
    out = []
    for t, (parse, exact, stored) in zip(texts, rows):
        parse, exact, stored = (r if r.startswith(("ok", "-")) else "err" for r in (parse, exact, stored))
        if not any(r.startswith("ok") for r in (parse, exact, stored)) and t["text"] not in keep:
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


def version_key(name):
    """A tag's place among releases -- `v0.2.0` before `v0.10.0`, a
    release candidate before its release -- or None for a name that is no
    version."""
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?", name)
    if not m:
        return None
    pre = m.group(4)
    ids = tuple((0, int(x), "") if x.isdigit() else (1, 0, x) for x in pre.split(".")) if pre else ()
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)), 0 if pre else 1, ids)


def earlier_corpora(tag, root=ROOT):
    """The corpora frozen at tags before `tag`, oldest first: a later
    tag's would hold texts read by a later grammar, which the tag's reading
    of them would then hold back."""
    mine = version_key(tag)
    named = [(version_key(n), n) for n in frozen_names(root)]
    return [n for k, n in sorted(x for x in named if x[0] is not None) if mine is not None and k < mine]


def freeze_texts(tree_texts, listed, earlier, root=ROOT):
    """What a freeze reads: the tag's tree's texts, the hand-written list's,
    and the texts each earlier frozen corpus holds that neither does -- the
    texts deleted since, held from now on to the tag's reading too, not to
    an older release's alone. With how many each earlier one added."""
    texts = merged(tree_texts, listed)
    seen = {t["text"] for t in texts}
    added = {}
    for name in earlier:
        _, held, _ = read_frozen(frozen_path(name, root))
        new = [t for t in held if t["text"] not in seen]
        seen.update(t["text"] for t in new)
        texts += new
        added[name] = len(new)
    return texts, added


def may_write(path, replace):
    """A frozen corpus is added, never replaced: once landed, its tag's
    reading is the one thing no later tree can give again."""
    if os.path.exists(path) and not replace:
        sys.exit(f"{path} is frozen: a frozen corpus is added, never replaced "
                 "(--replace only before it lands)")


def freeze(tag, replace=False, into=None):
    """The texts `tag`'s tree holds, the hand-written list's and those of
    the corpora frozen before it, read by `tag`'s own parser, written as
    the corpus frozen at it -- or into `into`, to see a freeze give a
    committed file again."""
    path = into or frozen_path(tag)
    may_write(path, replace)
    commit = commit_of(tag)
    target_root = targets()
    scratch = unpacked(target_root, "freeze")
    try:
        tree = os.path.join(scratch, "tree")
        extract(commit, tree)
        listed = harvest(ROOT, [LISTED])
        texts, earlier = freeze_texts(harvest(tree, tracked(commit)), listed, earlier_corpora(tag))
        exe = build(tree, target_for(target_root, "freeze", commit))
        epoch, rows = run(exe, texts)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    kept = frozen_rows(texts, rows, {t["text"] for t in listed})
    head = {"tag": tag, "commit": commit, "epoch": epoch, "texts": len(texts), "kept": len(kept),
            "listed": len(listed),
            "written": f"python3 tools/fennecql_corpus.py --freeze {tag}: the tag's tree's texts "
                       f"and {LISTED}'s, each read by the tag's fennec-ql with this tree's corpus "
                       "example; a harvested text every reading refused is left out"}
    if earlier:
        # The texts each earlier corpus added that the tag's tree and the
        # list did not hold: v0.2.0's, the first, has none to name.
        head["earlier"] = earlier
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        for line in [head] + kept:
            f.write(json.dumps(line, ensure_ascii=False, sort_keys=False) + "\n")
    print(f"{path}: {len(kept)} of {len(texts)} texts, epoch {epoch}, {tag} at {commit[:12]}")


# ------------------------------------------------------------------ judging


DIGESTS = ("c", "e", "s")
READINGS = ("parse", "parse_exact", "stored_definition")


def judge(head, base):
    """One reading against the other's: `panic` (it panics, whatever BASE
    did), `same`, `changed` (both parsed), `refused` (it parsed before),
    `parses` (it was refused) or `reworded`."""
    if head == "panic":
        return "panic"
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
    if "err" in (head, base) and norm(head) == norm(base):
        return "same"
    return "reworded"


def digest_word(word):
    return word[:1] in DIGESTS


def moves_digest(head_row, base_row):
    """Whether the change between two rows moves a digest: a `c`, `e` or
    `s` word read otherwise -- word by word, a statement no definition
    holds read otherwise beside one moving nothing -- or a definition's
    stored reading changed or refused: what an open would mark broken."""
    for i, (h, b) in enumerate(zip(head_row, base_row)):
        verdict = judge(h, b)
        if i == 2 and verdict in ("changed", "refused"):
            return True
        if verdict != "changed":
            continue
        hw, bw = h.split()[1:], b.split()[1:]
        if len(hw) != len(bw):
            # Read as another number of statements: none is the one it was.
            if any(digest_word(w) for w in hw + bw):
                return True
            continue
        if any(x != y and (digest_word(x) or digest_word(y)) for x, y in zip(hw, bw)):
            return True
    return False


def entry_key(text, base_row, head_row):
    return (text, tuple(norm(r) for r in base_row), tuple(norm(r) for r in head_row))


def claim(entry):
    """An entry's `same_meaning`, where it says something."""
    said = entry.get("same_meaning")
    return said.strip() if isinstance(said, str) and said.strip() else None


def entry_errors(accepted):
    """What is wrong with entries of ACCEPTED, refused before anything
    runs: a `same_meaning` that says nothing claims nothing."""
    return [f"an ACCEPTED entry's same_meaning says nothing: {json.dumps(a['text'], ensure_ascii=False)[:120]} "
            "-- say why the statement means what it meant, or raise GRAMMAR_EPOCH and name it (`epoch`)"
            for a in accepted if "same_meaning" in a and not claim(a)]


def needs(head_row, base_row, base_major):
    """What lets a change of a text through beside its entry: `epoch` --
    `GRAMMAR_EPOCH` raised past BASE's, where a digest moved or a text a
    1.x BASE read is refused -- `meaning` -- the epoch, or `same_meaning`,
    where a 1.x BASE's statement no definition holds is read otherwise --
    or None, the entry alone."""
    verdicts = [judge(x, y) for x, y in zip(head_row, base_row)]
    if moves_digest(head_row, base_row) or (base_major >= 1 and "refused" in verdicts):
        return "epoch"
    if base_major >= 1 and "changed" in verdicts:
        return "meaning"
    return None


def entry_for(text, head_row, base_row, base_major=0):
    """The ACCEPTED entry naming this change, ready to paste: against a 1.x
    BASE, a statement read otherwise asks for its `same_meaning`, which an
    entry may hold only where it says something."""
    entry = {"text": text, "base": [norm(r) for r in base_row], "head": [norm(r) for r in head_row],
             "pr": "#", "why": ""}
    if needs(head_row, base_row, base_major) == "meaning":
        entry["same_meaning"] = ""
    return entry


def compare(texts, head_rows, base_rows, head_epoch, base_epoch, accepted=(), base_major=0):
    """Each text's readings against BASE's, grouped, each as (text, head's
    row, BASE's): `changed`, `refused` and `panics` fail the check unless
    an entry of `accepted` names the change, `parses` and `reworded` are
    told; `unmatched` holds the entries naming no change here, and
    `claimed` (text, claim) every accepted entry's `same_meaning`."""
    entries = {entry_key(a["text"], a["base"], a["head"]): a for a in accepted}
    groups = {k: [] for k in ("changed", "refused", "panics", "parses", "reworded", "accepted", "held",
                              "claimed")}
    used = set()
    same = 0
    for t, h, b in zip(texts, head_rows, base_rows):
        verdicts = [judge(x, y) for x, y in zip(h, b)]
        same += verdicts.count("same")
        if "panic" in verdicts:
            groups["panics"].append((t, h, b))
        elif "changed" in verdicts or "refused" in verdicts:
            key = entry_key(t["text"], b, h)
            entry = entries.get(key)
            if entry:
                used.add(key)
                # A refusal of what a 1.x release read is a break of the
                # contract (compatibility.md): a major version, as a
                # digest moved is; a 1.x statement read otherwise is one
                # unless its meaning is said to be the same.
                need = needs(h, b, base_major)
                raised = head_epoch > base_epoch and entry.get("epoch") == head_epoch
                if need is None or raised or (need == "meaning" and claim(entry)):
                    groups["accepted"].append((t, h, b))
                    if claim(entry):
                        groups["claimed"].append((t, claim(entry)))
                    continue
                groups["held"].append((t, h, b))
            groups["changed" if "changed" in verdicts else "refused"].append((t, h, b))
        elif "parses" in verdicts:
            groups["parses"].append((t, h, b))
        elif "reworded" in verdicts:
            groups["reworded"].append((t, h, b))
    groups["unmatched"] = [a for k, a in entries.items() if k not in used]
    return same, groups


FAILING = ("changed", "refused", "panics")


def failing(groups):
    return [x for k in FAILING for x in groups[k]]


def report(base_name, how, texts, same, groups, out=sys.stdout):
    readings = len(texts) * len(READINGS)
    print(f"## FennecQL's corpus, against `{base_name}`\n", file=out)
    print(f"{how}: {len(texts)} texts, {readings} readings (`parse`, `parse_exact`, "
          f"`stored_definition`): {same} read alike.\n", file=out)
    kinds = collections.Counter(kind(t["text"]) for t in texts)
    print("- by kind: " + ", ".join(f"`{k}` {n}" for k, n in sorted(kinds.items(), key=lambda x: (-x[1], x[0]))),
          file=out)
    print(f"- read otherwise: {len(groups['changed'])}", file=out)
    print(f"- refused now, parsed before: {len(groups['refused'])}", file=out)
    print(f"- the parser panics: {len(groups['panics'])}", file=out)
    print(f"- accepted (ACCEPTED): {len(groups['accepted'])}", file=out)
    if groups["claimed"]:
        print(f"- of them saying the meaning is the same (`same_meaning`): {len(groups['claimed'])}",
              file=out)
    print(f"- parse now, refused before: {len(groups['parses'])}", file=out)
    print(f"- refused in other words: {len(groups['reworded'])}", file=out)
    if groups["unmatched"]:
        print(f"- ACCEPTED entries naming no change here, which approve nothing: "
              f"{len(groups['unmatched'])}", file=out)
    titles = (("changed", "Read otherwise"), ("refused", "Refused now, parsed before"),
              ("panics", "The parser panics"),
              ("held", "In ACCEPTED, but a digest moved, or a text a 1.x release read is refused, "
                       "with no grammar epoch raised past BASE's -- or a 1.x release's statement read "
                       "otherwise with neither the epoch raised nor its `same_meaning` said"),
              ("accepted", "Accepted"), ("parses", "Parse now, refused before"),
              ("reworded", "Refused in other words"))
    for key, title in titles:
        if groups[key]:
            print(f"\n### {title}\n", file=out)
            for t, _, _ in groups[key]:
                print(f"- `{t['from']}`: {json.dumps(t['text'], ensure_ascii=False)[:200]}", file=out)
    # Every claim whole: what a reviewer of the change reads and judges.
    if groups["claimed"]:
        print("\n### The same meaning, as each entry says\n", file=out)
        for t, said in groups["claimed"]:
            print(f"- `{t['from']}`: {json.dumps(t['text'], ensure_ascii=False)[:200]}", file=out)
            print(f"    same_meaning: {said}", file=out)


def differing(was, now, limit=SHOWN_LINES):
    """The lines of two forms that differ, a line of context about them,
    at most `limit`."""
    lines = list(difflib.unified_diff(was.split("\n"), now.split("\n"), lineterm="", n=1))[2:]
    if len(lines) > limit:
        lines = lines[:limit] + [f"... {len(lines) - limit} lines more"]
    return lines


def entries_path(target_root, base_name):
    """Where a failure against `base_name` writes every failing text's
    entry: beside the builds, named by the base."""
    return os.path.join(target_root, "accepted-" + re.sub(r"[^A-Za-z0-9._-]+", "-", base_name) + ".txt")


def explain(fails, head_forms, base_forms, why_no_base=None, out=sys.stdout, entries_to=None, base_major=0):
    """For each failing text, at most `SHOWN_TEXTS`: its entry ready to
    paste, and per reading what changed -- the lines of the statements'
    forms that differ, a refusal's words, a panic. `head_forms` and
    `base_forms` are the texts' `forms` readings (BASE's None where its
    build cannot be had, `why_no_base` saying why). Every failing text's
    entry, those past the screen's too, is written to `entries_to`, a line
    each, ready to paste."""
    print("\n### What changed\n", file=out)
    print("Each failing text, its ACCEPTED entry -- were the change made on purpose -- "
          "and what each reading reads otherwise.\n", file=out)
    if entries_to:
        os.makedirs(os.path.dirname(entries_to) or ".", exist_ok=True)
        with open(entries_to, "w", encoding="utf-8") as f:
            for t, h, b in fails:
                f.write(f"    {json.dumps(entry_for(t['text'], h, b, base_major), ensure_ascii=False)},\n")
        print(f"The entries of every one of the {len(fails)} failing texts, ready to paste: {entries_to}\n",
              file=out)
    for n, (t, h, b) in enumerate(fails[:SHOWN_TEXTS]):
        print(f"- `{t['from']}`: {json.dumps(t['text'], ensure_ascii=False)[:200]}", file=out)
        print(f"    {json.dumps(entry_for(t['text'], h, b, base_major), ensure_ascii=False)},", file=out)
        for i, name in enumerate(READINGS):
            verdict = judge(h[i], b[i])
            if verdict in ("same", "parses", "reworded"):
                continue
            if verdict == "panic":
                print(f"  {name}: the parser panics", file=out)
                continue
            if verdict == "refused":
                print(f"  {name}: refused: {unescape_line(h[i])[len('err '):][:300]}", file=out)
                continue
            now = forms_of(head_forms[n][i]) or []
            was = forms_of(base_forms[n][i]) if base_forms else None
            print(f"  {name}: read otherwise, `{b[i]}` -> `{h[i]}`", file=out)
            if was is None:
                print(f"      BASE's forms not built: {why_no_base}", file=out)
                for line in "\n".join(now).split("\n")[:SHOWN_LINES]:
                    print(f"      {line}", file=out)
                continue
            sep = "\n----- the next statement\n"
            lines = differing(sep.join(was), sep.join(now))
            if not lines:
                print("      the forms are alike: what moved is the canonical encoding "
                      "(`Definition::canonical`)", file=out)
            for line in lines:
                print(f"      {line}", file=out)
    if len(fails) > SHOWN_TEXTS:
        print(f"\n... and {len(fails) - SHOWN_TEXTS} texts more.", file=out)


def tag_exe(head_info, target_root):
    """A frozen corpus' tag's build, to print its forms: its commit, from
    this clone or fetched (a shallow clone lacks it), unpacked and built.
    (None, why) where it cannot be had."""
    commit = head_info["commit"]
    have = subprocess.run(["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=ROOT,
                          stderr=subprocess.DEVNULL).returncode == 0
    if not have:
        try:
            have = subprocess.run(["git", "fetch", "-q", "--no-tags", "--depth=1", "origin", commit],
                                  cwd=ROOT, timeout=300, stderr=subprocess.DEVNULL).returncode == 0
        except subprocess.TimeoutExpired:
            have = False
    if not have:
        return None, f"{head_info['tag']}'s commit {commit[:12]} is not in this clone"
    scratch = unpacked(target_root, "tag")
    try:
        tree = os.path.join(scratch, "tree")
        extract(commit, tree)
        return build(tree, target_for(target_root, "base", commit)), None
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def judged(base_name, how, texts, head, base, base_major, head_exe, base_exe, accepted=None,
           target_root=None):
    """`texts` read by both, reported; whether they fail. `base_exe` is
    BASE's build, or a function giving (build, why not) where it is built
    only to show a failure. A failure's entries go beside the builds."""
    (head_epoch, head_rows), (base_epoch, base_rows) = head, base
    same, groups = compare(texts, head_rows, base_rows, head_epoch, base_epoch,
                           ACCEPTED if accepted is None else accepted, base_major)
    report(base_name, how, texts, same, groups)
    fails = failing(groups)
    entries = entries_path(target_root or targets(), base_name)
    if fails:
        shown = [t for t, _, _ in fails[:SHOWN_TEXTS]]
        _, head_forms = run(head_exe, shown, forms=True)
        exe, why = base_exe() if callable(base_exe) else (base_exe, None)
        base_forms = run(exe, shown, forms=True)[1] if exe else None
        explain(fails, head_forms, base_forms, why, entries_to=entries, base_major=base_major)
    elif os.path.exists(entries):
        # An earlier run's, which no longer names a failure.
        os.remove(entries)
    return bool(fails)


def against_frozen(name, head_exe, target_root):
    head_info, texts, base_rows = read_frozen(frozen_path(name))
    how = (f"frozen at {head_info['tag']} ({head_info['commit'][:12]}), as its own parser "
           f"read them under grammar epoch {head_info['epoch']}")
    return judged(name, how, texts, run(head_exe, texts), (head_info["epoch"], base_rows),
                  major(head_info["tag"]), head_exe, lambda: tag_exe(head_info, target_root),
                  target_root=target_root)


def frozen_changed(commit, root=ROOT):
    """The corpora frozen in `commit` that this checkout holds otherwise or
    not at all, each `(path, "changed" | "removed")`: once landed, a frozen
    corpus is added, never replaced -- `--freeze` refuses to write over one,
    and this a pull request editing or deleting one by hand. Compared by
    git's object id, the checkout's filters applied."""
    out = subprocess.run(["git", "ls-tree", "-r", "-z", commit, "--", FROZEN + "/"], cwd=root, check=True,
                         stdout=subprocess.PIPE).stdout.decode()
    found = []
    for entry in filter(None, out.split("\0")):
        meta, path = entry.split("\t", 1)
        if not path.endswith(".jsonl"):
            continue
        if not os.path.isfile(os.path.join(root, path)):
            found.append((path, "removed"))
            continue
        mine = subprocess.run(["git", "hash-object", "--", path], cwd=root, check=True,
                              stdout=subprocess.PIPE).stdout.decode().strip()
        if mine != meta.split()[2]:
            found.append((path, "changed"))
    return found


def against_tree(base_name, head_exe, target_root):
    """This tree's texts and BASE's -- a change that edits away the texts
    showing it is still read -- by both trees; and every corpus frozen in
    BASE, as BASE holds it."""
    commit = commit_of(base_name)
    kept_otherwise = frozen_changed(commit)
    scratch = unpacked(target_root, "base")
    try:
        tree = os.path.join(scratch, "tree")
        extract(commit, tree)
        texts = merged(harvest(), harvest(tree, tracked(commit)))
        base_exe = build(tree, target_for(target_root, "base", commit))
        version = cargo_version(tree)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    how = f"this tree's texts and {base_name}'s ({commit[:12]}, version {version}), read by both"
    failed = judged(base_name, how, texts, run(head_exe, texts), run(base_exe, texts), major(version),
                    head_exe, base_exe, target_root=target_root)
    if kept_otherwise:
        print(f"\n### Frozen corpora {base_name} holds, changed or removed\n")
        print("A frozen corpus is added, never changed or removed: its tag's reading is the one thing "
              "no later tree can give again (a new tag's goes in a file of its own).\n")
        for path, how in kept_otherwise:
            print(f"- `{path}`: {how}")
    return failed or bool(kept_otherwise)


def main():
    args = sys.argv[1:]
    if args and args[0] == "--list":
        for t in harvest():
            print(json.dumps(t, ensure_ascii=False))
        return
    errors = entry_errors(ACCEPTED)
    if errors:
        sys.exit("\n".join(errors))
    target_root = targets()
    clear_leftovers(target_root)
    if args and args[0] == "--freeze":
        freeze(args[1], replace="--replace" in args[2:])
        return
    head_exe = build(ROOT, head_target(target_root))
    if args and args[0] == "--frozen":
        names = frozen_names()
        if not names:
            sys.exit(f"no corpus frozen under {FROZEN}")
        failed = [n for n in names if against_frozen(n, head_exe, target_root)]
    else:
        base_name = args[0] if args else "origin/main"
        if os.path.exists(frozen_path(base_name)):
            failed = [base_name] if against_frozen(base_name, head_exe, target_root) else []
        else:
            failed = [base_name] if against_tree(base_name, head_exe, target_root) else []
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
