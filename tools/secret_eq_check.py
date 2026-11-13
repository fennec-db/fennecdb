#!/usr/bin/env python3
"""No secret is compared with `==` or `!=`: `python3 tools/secret_eq_check.py`.

docs/design/conventions.md#secrets-compared. A token, a secret, a mark, a
signature or a password is compared by `fennec_http::crypto::ct_eq` alone,
which reads every byte whatever differs: `==` on a slice or a string leaves
at the first byte that differs, so the time an answer takes says how much
of a guess was right, and a guess can be grown a byte at a time. The check
goes by names, a tripwire: a secret under another name (`got`, `want`) is
its reviewer's to see.

This reads every `.rs` file under `crates/*/src`, its comments, strings and
characters blanked and its `#[cfg(test)]` items (`cfg(all(test, ..))` too)
left out, and finds each `==` and `!=` an operand of which names a value
whose name holds `token`, `secret`, `mark`, `signature` or `password`,
whatever its case: `given_token`, `self.secret`, `marks[i]`,
`hmac(secret, msg)`. A name with a capital first -- a type, a variant, a
constant (`Token::Comma`, `OP_MARK`) -- is passed over, and so is an
operand ending in `.len()`, `.is_empty()`, `.is_some()` or `.is_none()`:
a length or a presence is no secret's bytes. A comparison one side of
which is public -- a literal, a constant (`Some("")`, `0`,
`Some(&stored::OP_MARK)`) -- is passed over too: the other side is held
to a value the binary carries, never to a guess someone grows.

What is meant -- a lexer's token, a mark that is a position -- goes in
ALLOWED with why, by file and the comparison as written (its spaces made
one), and an entry the code no longer has fails the check, so the list
keeps no stale pass. `--self-test` holds the reader to CASES below. CI's
fmt job runs both."""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

NAMES = ("token", "secret", "mark", "signature", "password")

# A comparison the check finds that compares no secret: by file, the
# comparison as the source writes it (its spaces made one), and why.
ALLOWED = {
    "crates/fennec-core/src/engine.rs": {
        "sel.marks[i].label() == *col": "a select list's items (`marks`), found by their labels",
    },
    "crates/fennec-core/src/highlight.rs": {
        "tokens == n": "a snippet's count of words",
    },
    "crates/fennec-core/src/engine/maintenance.rs": {
        "t.token == token": "the number a maintenance's tail is told apart by",
        "x.token == token": "the number a maintenance's tail is told apart by",
    },
    "crates/fennec-http/src/schedule.rs": {
        "*m == mark": "a scheduled statement's mark: the time a run ahead of the clock was told for",
    },
    "crates/fennec-shard/src/upstream.rs": {
        "bucket(mark_of(a)) == bucket(mark_of(b))": "a tenant's mark, a keyed hash of its name, by bucket",
        "self.moved(flying.mark) != flying.seen": "a failover count, read by the tenant's mark",
        "self.moved[bucket(flying.mark)].load(Ordering::Relaxed) != flying.seen": "a failover count, read by the tenant's mark",
        "pool.moved(mark) != seen": "a failover count, read by the tenant's mark",
    },
}

# The last call of an operand that leaves no secret's bytes in it.
HARMLESS_ENDS = (".len()", ".is_empty()", ".is_some()", ".is_none()")

# Where a comment, a string or a character may begin: a name's letters
# before `b`, `c` or `r` make them its end instead (`sub"` is no byte
# string, `r#type` no raw one). The scan jumps from one to the next rather
# than stepping a character at a time: the crates' 8.4 MB of sources in
# about two seconds of a core.
OPENING = re.compile(r"""//|/\*|(?<![A-Za-z0-9_])[bc]?r(#*)"|(?<![A-Za-z0-9_])[bc]"|"|(?<![A-Za-z0-9_])b'|'""")
COMMENT_EDGE = re.compile(r"/\*|\*/")
STRING_REST = re.compile(r'(?:[^"\\]|\\.)*"', re.S)
CHARACTER = re.compile(r"'(?:[^'\\\n]|\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.))'")
NOT_NEWLINE = re.compile(r"[^\n]")


def blank(src):
    """`src` with its comments, strings and characters made spaces, every
    newline kept: what is left is code alone, at the places it had."""
    out, i, n = [], 0, len(src)
    while m := OPENING.search(src, i):
        a, token = m.start(), m.group()
        if token == "//":
            j = src.find("\n", a)
            j = n if j < 0 else j
        elif token == "/*":
            depth, j = 0, n
            for e in COMMENT_EDGE.finditer(src, a):
                depth += 1 if e.group() == "/*" else -1
                if depth == 0:
                    j = e.end()
                    break
        elif token.endswith('"') and "r" in token:
            end = '"' + m.group(1)
            j = src.find(end, m.end())
            j = n if j < 0 else j + len(end)
        elif token.endswith('"'):
            rest = STRING_REST.match(src, m.end())
            j = rest.end() if rest else n
        else:
            # A character ('a', '\n', '\u{2028}', b'x') or a lifetime ('a).
            c = CHARACTER.match(src, m.end() - 1)
            if not c:
                out.append(src[i : m.end()])
                i = m.end()
                continue
            j = c.end()
        out.append(src[i:a])
        out.append(NOT_NEWLINE.sub(" ", src[a:j]))
        i = j
    out.append(src[i:])
    return "".join(out)


TEST_ATTR = re.compile(r"#\[cfg\(\s*(?:test\s*\)|all\(\s*test\s*,)")
BRACKETS = re.compile(r"[()\[\]{};]")
BRACES = re.compile(r"[{}]")


def without_tests(code):
    """`code` with every `#[cfg(test)]` item blanked: the attribute, any
    after it, and the item to its `;` or its block's closing brace."""
    out, last = [], 0
    for m in TEST_ATTR.finditer(code):
        if m.start() < last:
            continue
        depth, end = 0, len(code)
        for b in BRACKETS.finditer(code, m.end()):
            ch = b.group()
            if ch in "([":
                depth += 1
            elif ch in ")]":
                depth -= 1
            elif ch == ";" and depth <= 0:
                end = b.end()
                break
            elif ch == "{" and depth <= 0:
                braces = 0
                for c in BRACES.finditer(code, b.start()):
                    braces += 1 if c.group() == "{" else -1
                    if braces == 0:
                        end = c.end()
                        break
                break
        out.append(code[last : m.start()])
        out.append(NOT_NEWLINE.sub(" ", code[m.start() : end]))
        last = end
    out.append(code[last:])
    return "".join(out)


IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
PAIR = {")": "(", "]": "[", "(": ")", "[": "]"}


def matching(code, at, step):
    """Where the bracket at `at` is closed, read in `step`'s direction."""
    depth, j = 0, at
    while 0 <= j < len(code):
        if code[j] == code[at]:
            depth += 1
        elif code[j] == PAIR[code[at]]:
            depth -= 1
            if depth == 0:
                return j
        j += step
    return j


def left_operand(code, at):
    """Where the operand ending before `at` starts and ends: a path of
    names, calls and indexes (`self.cfg.token`, `hmac(k, m)`, `marks[i]`,
    `&given`), read backwards."""
    i = at
    while i > 0 and code[i - 1].isspace():
        i -= 1
    end = i
    while i > 0:
        ch = code[i - 1]
        if ch in ")]":
            i = matching(code, i - 1, -1)
        elif ch.isalnum() or ch in "_.":
            i -= 1
        elif ch == ":" and i > 1 and code[i - 2] == ":":
            i -= 2
        elif ch in "&*" and (i < 2 or code[i - 2].isspace() or code[i - 2] in "(,=!|&"):
            i -= 1
        else:
            break
    return i, end


def right_operand(code, at):
    """Where the operand starting after `at` starts and ends, read forwards
    as `left_operand` reads backwards."""
    i, n = at, len(code)
    while i < n and code[i].isspace():
        i += 1
    start = i
    while i < n and code[i] in "&*-!":
        i += 1
    while i < n:
        ch = code[i]
        if ch in "([":
            i = matching(code, i, 1) + 1
        elif ch.isalnum() or ch in "_.":
            i += 1
        elif code.startswith("::", i):
            i += 2
        else:
            break
    return start, i


def values(text):
    """The names in `text` that name a value: never a path's module
    (`stored::`), nor a name with a capital first -- a type, a variant, a
    constant."""
    for m in IDENT.finditer(text):
        name = m.group()
        if name[0].isupper() or text.startswith("::", m.end()):
            continue
        yield name


def secret_named(operand):
    """Whether `operand` names a value whose name holds a secret's word,
    not its length or presence."""
    text = operand.strip()
    if text.endswith(HARMLESS_ENDS):
        return False
    return any(any(word in name.lower() for word in NAMES) for name in values(text))


def public(operand):
    """Whether `operand` is a value the binary carries: literals and
    constants alone, its strings already blanked (`Some("")` is `Some(  )`
    here)."""
    return all(name in ("true", "false") for name in values(operand))


OPERATOR = re.compile(r"(?<![=!<>])(?:==|!=)(?!=)")


def findings(src):
    """Each comparison in `src` an operand of which names a secret: its
    line and the comparison as written, its spaces made one."""
    code = without_tests(blank(src))
    found = []
    for m in OPERATOR.finditer(code):
        a, b = left_operand(code, m.start())
        c, d = right_operand(code, m.end())
        left, right = code[a:b], code[c:d]
        if not (secret_named(left) or secret_named(right)) or public(left) or public(right):
            continue
        line = code.count("\n", 0, m.start()) + 1
        found.append((line, " ".join(src[a:d].split())))
    return found


def sources():
    crates = os.path.join(ROOT, "crates")
    for crate in sorted(os.listdir(crates)):
        src = os.path.join(crates, crate, "src")
        for base, dirs, files in os.walk(src):
            dirs.sort()
            for f in sorted(files):
                if f.endswith(".rs"):
                    path = os.path.join(base, f)
                    yield os.path.relpath(path, ROOT).replace(os.sep, "/"), path


def check():
    failed = False
    seen = set()
    for rel, path in sources():
        with open(path, encoding="utf-8") as fh:
            src = fh.read()
        allowed = ALLOWED.get(rel, {})
        for line, written in findings(src):
            if written in allowed:
                seen.add((rel, written))
                continue
            failed = True
            print(
                f"{rel}:{line}: `{written}` compares a secret with == or !=: compare it "
                "with crypto::ct_eq, or allow it in tools/secret_eq_check.py's ALLOWED, saying why"
            )
    for rel, entries in ALLOWED.items():
        for written in entries:
            if (rel, written) not in seen:
                failed = True
                print(f"{rel}: `{written}` is in ALLOWED but no longer in the code: remove it")
    if failed:
        sys.exit(1)
    print("no secret compared with == or != in crates/*/src")


# (source, the comparisons the check finds in it).
CASES = [
    ("if given == token { ok() }", ["given == token"]),
    ("if self.cfg.http_token != given.as_bytes() {}", ["self.cfg.http_token != given.as_bytes()"]),
    ("let ok = &hmac(secret, msg) == sig;", ["&hmac(secret, msg) == sig"]),
    ("if marks[i] == want {}", ["marks[i] == want"]),
    ("if got == entry.signature {}", ["got == entry.signature"]),
    ("if a ==\n    secret_of(b) {}", ["a == secret_of(b)"]),
    # Types and variants, lengths and presence name no secret's bytes.
    ("if t == Token::Comma || kind != MarkKind::Held {}", []),
    ("if self.password == given || entry.token == given {}",
     ["self.password == given", "entry.token == given"]),
    ("if token.len() != 32 || secret.is_empty() == false {}", []),
    ("if self.token.is_some() == other.is_some() {}", []),
    # Comments, strings and characters are no code.
    ('// token == x\nlet s = "token == x"; let r = r#"secret != y"#;', []),
    ("/* a /* nested */ mark == b */ let c = b'm' == x;", []),
    ("let q = '\\'' == c; let l: &'static str = \"\";", []),
    ('let s = br##"x "# token == y"##; let ok = c"token" == z;', []),
    ("fn f<'a>(token: &'a str, given: &'a str) -> bool { token == given }", ["token == given"]),
    # A test's items are left out, every other item read.
    ("#[cfg(test)]\nmod tests {\n    fn f() { assert!(token == x); }\n}\nfn g() -> bool { secret == y }",
     ["secret == y"]),
    ("#[cfg(all(test, feature = \"x\"))]\nfn t() { mark == 1; }\nfn u() { 1 == 2 }", []),
    ("#[cfg(test)]\nconst X: &str = \"x\";\nfn v(a: &str) -> bool { a == token_of(X) }", ["a == token_of(X)"]),
    # A literal or a constant is public: what it is held to is no guess.
    ('if token.as_deref() == Some("") || secret != 0 || mark == stored::OP_MARK {}', []),
    ("if body.first() == Some(&OP_MARK) || n.secrets == MAX_SECRETS || mark != -1 {}", []),
    # Not comparisons.
    ("if token <= x || secret >= y { let m = |mark| mark; match a { _ => {} } }", []),
]


def self_test():
    failed = 0
    for src, want in CASES:
        got = [w for _, w in findings(src)]
        if got != want:
            failed += 1
            print(f"FAIL {src!r}: found {got}, want {want}")
    if failed:
        sys.exit(1)
    print(f"{len(CASES)} cases read as meant")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        check()
