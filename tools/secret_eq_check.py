#!/usr/bin/env python3
"""No secret is compared with `==` or `!=`: `python3 tools/secret_eq_check.py`.

docs/design/conventions.md#secrets-compared. A token, a secret, a mark, a
signature, a MAC, a proof or a password is compared by
`fennec_http::crypto::ct_eq` alone, which reads every byte whatever
differs: `==` on a slice or a string leaves at the first byte that differs,
so the time an answer takes says how much of a guess was right, and a
guess can be grown a byte at a time. The check goes by names, a tripwire:
a secret under another name (`got`, `want`) is its reviewer's to see.

This reads every `.rs` file under `crates/*/src`, its comments blanked, its
strings and characters made literals of their length, and every item a
`#[cfg(test)]` is on left out -- an item, a statement, a field, a variant,
a match arm or an argument, to its `;`, its `,`, its block's `}` or the
bracket closing around it, and all a `#![cfg(test)]` is inside -- the cfg
read as Rust reads it (`all(feature = "x", test)` is a test's,
`any(test, unix)` is not). Each `==` and `!=` is read with both its
operands whole, as Rust binds them: paths, fields, calls, indexes, `?`,
`::<..>`, macros, prefixes and the operators binding tighter (`+`, `^`,
`as`, ...). It is refused where an operand names a value whose name holds
`token`, `secret`, `mark`, `signature` or `password`, or has `sig`, `mac`,
`hmac` or `proof` (or its plural) as a whole part between its `_`s,
whatever its case: `given_token`, `self.secret`, `marks[i]`, `given_sig`,
`hmac(k, msg)`, `ADMIN_TOKEN.get()` -- never `signal` or `machine`. A type
or a variant (`Token::Comma`) names no value, and a part of an operand
ending in `.len()`, `.is_empty()`, `.is_some()` or `.is_none()` no secret's
bytes. A comparison one side of which is public -- literals and constants
alone (`Some("")`, `0`, `Some(&stored::OP_MARK)`, `MAX_SECRETS`) -- is
passed over: the other side is held to a value the binary carries, never
to a guess someone grows. A static read through a method
(`ADMIN_TOKEN.get()`) is no constant, and an operand the reader cannot read
whole is never public.

What is meant -- a lexer's token, a mark that is a position -- goes in
ALLOWED with why and how many times its file has it, the comparison as
written (its spaces made one): one more is a comparison nobody weighed,
refused, and one fewer a stale pass, refused too. `--self-test` holds the
reader to CASES and COUNTED below. CI's fmt job runs both."""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# A secret's words: one of the first anywhere in a name (`http_token`,
# `secrets`); one of the second only as a whole part between the name's
# `_`s, its plural too (`given_sig`, `hmac_key`, `macs`), for they are
# inside `signal`, `design`, `machine` and `proofread`.
WITHIN = ("token", "secret", "mark", "signature", "password")
PARTS = ("sig", "mac", "hmac", "proof")
PART_WORDS = frozenset(PARTS + tuple(w + "s" for w in PARTS))

# A comparison the check finds that compares no secret: by file, the
# comparison as the source writes it (its spaces made one), how many times
# the file has it, and why. A second one of the same text is a comparison
# nobody weighed, so a count, never the text alone, lets one through.
ALLOWED = {
    "crates/fennec-core/src/engine.rs": {
        "sel.marks[i].label() == *col": (1, "a select list's items (`marks`), found by their labels"),
    },
    "crates/fennec-core/src/highlight.rs": {
        "at + tokens == n": (1, "a snippet's window of words (`tokens` a count) reaching the text's end"),
    },
    "crates/fennec-core/src/engine/maintenance.rs": {
        "t.token == token": (1, "the number a maintenance's tail is told apart by"),
        "x.token == token": (1, "the number a maintenance's tail is told apart by"),
    },
    "crates/fennec-http/src/schedule.rs": {
        "*m == mark": (1, "a scheduled statement's mark: the time a run ahead of the clock was told for"),
    },
    "crates/fennec-shard/src/upstream.rs": {
        "bucket(mark_of(a)) == bucket(mark_of(b))": (1, "a tenant's mark, a keyed hash of its name, by bucket"),
        "self.moved(flying.mark) != flying.seen": (1, "a failover count, read by the tenant's mark"),
        "self.moved[bucket(flying.mark)].load(Ordering::Relaxed) != flying.seen": (
            1,
            "a failover count, read by the tenant's mark",
        ),
        "pool.moved(mark) != seen": (1, "a failover count, read by the tenant's mark"),
    },
}

# The last call of a part of an operand that leaves no secret's bytes in it.
HARMLESS_ENDS = (".len()", ".is_empty()", ".is_some()", ".is_none()")

# Where a comment, a string or a character may begin: a name's letters
# before `b`, `c` or `r` make them its end instead (`sub"` is no byte
# string, `r#type` no raw one). The scan jumps from one to the next rather
# than stepping a character at a time: the crates' 8.4 MB of sources read
# in a few seconds of a core.
OPENING = re.compile(r"""//|/\*|(?<![A-Za-z0-9_])[bc]?r(#*)"|(?<![A-Za-z0-9_])[bc]"|"|(?<![A-Za-z0-9_])b'|'""")
COMMENT_EDGE = re.compile(r"/\*|\*/")
STRING_REST = re.compile(r'(?:[^"\\]|\\.)*"', re.S)
CHARACTER = re.compile(r"'(?:[^'\\\n]|\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.))'")
NOT_NEWLINE = re.compile(r"[^\n]")


def spaces(text):
    """`text` made spaces, its newlines kept: by a multiplication where it
    has none, as most comments and literals do, a third of `blank`'s time."""
    return NOT_NEWLINE.sub(" ", text) if "\n" in text else " " * len(text)


def blank(src):
    """`src` with its comments made spaces and each string or character
    made a literal of its length -- `"`, spaces and `"`, which the operand
    reader takes for the literal it was -- every newline kept: what is left
    is code alone, at the places it had."""
    out, i, n = [], 0, len(src)
    while m := OPENING.search(src, i):
        a, opening = m.start(), m.group()
        literal = True
        if opening == "//":
            literal = False
            j = src.find("\n", a)
            j = n if j < 0 else j
        elif opening == "/*":
            literal = False
            depth, j = 0, n
            for e in COMMENT_EDGE.finditer(src, a):
                depth += 1 if e.group() == "/*" else -1
                if depth == 0:
                    j = e.end()
                    break
        elif opening.endswith('"') and "r" in opening:
            end = '"' + m.group(1)
            j = src.find(end, m.end())
            j = n if j < 0 else j + len(end)
        elif opening.endswith('"'):
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
        if literal and j - a >= 2:
            out.append('"' + spaces(src[a + 1 : j - 1]) + '"')
        else:
            out.append(spaces(src[a:j]))
        i = j
    out.append(src[i:])
    return "".join(out)


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


ATTRIBUTE = re.compile(r"#(!?)\[")
CFG = re.compile(r"\s*cfg\s*\((.*)\)\s*", re.S)
ALL_ANY = re.compile(r"(all|any)\s*\((.*)\)", re.S)
EDGES = re.compile(r"[()\[\]{};,]")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

# An item's first words a `,` never ends -- its generics, a `where` clause
# or a closure's parameters hold one -- and, of them, those whose `}` does
# not end them either (`let x = S { .. };`): a `;` always does. A field of
# such a name (`union: u32`) is a field.
ITEMS = frozenset(
    ("fn", "impl", "struct", "enum", "trait", "union", "mod", "extern", "unsafe", "async", "macro_rules", "const")
    + ("let", "static", "use", "type")
)
ENDED_BY_SEMICOLON = frozenset(("let", "static", "use", "type"))


def split_terms(text):
    """A cfg predicate's terms, apart at its top level's commas."""
    terms, depth, last = [], 0, 0
    for i, ch in enumerate(text):
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "," and depth == 0:
            terms.append(text[last:i])
            last = i + 1
    terms.append(text[last:])
    return [t for t in terms if t.strip()]


def test_only(predicate):
    """Whether a cfg predicate holds in a test build alone: `test`, an
    `all` one of whose terms does, an `any` all of whose terms do."""
    p = predicate.strip()
    if p == "test":
        return True
    m = ALL_ANY.fullmatch(p)
    if not m:
        return False
    terms = split_terms(m.group(2))
    if m.group(1) == "all":
        return any(test_only(t) for t in terms)
    return bool(terms) and all(test_only(t) for t in terms)


def first_word(code, i):
    """The first word of what begins at `i`, past its other attributes and
    its visibility, and whether a lone `:` follows it: a field's name."""
    n = len(code)
    while True:
        while i < n and code[i].isspace():
            i += 1
        a = ATTRIBUTE.match(code, i)
        if a:
            i = matching(code, a.end() - 1, 1) + 1
            continue
        m = IDENT.match(code, i)
        if not m:
            return "", False
        i = m.end()
        while i < n and code[i].isspace():
            i += 1
        if m.group() == "pub":
            if code.startswith("(", i):
                i = matching(code, i, 1) + 1
            continue
        return m.group(), code.startswith(":", i) and not code.startswith("::", i)


def item_end(code, start):
    """Where what an outer attribute ending at `start` is on ends: its `;`
    or its `,` at its own depth -- an item's `,` none -- the `}` of a block
    it opened, or before the bracket closing around it (a last field, arm
    or argument). Read too short, a test's code is read as the crates'; it
    is never read too long, which would leave the crates' code unread."""
    word, field = first_word(code, start)
    commas = field or word not in ITEMS
    braces = field or word not in ENDED_BY_SEMICOLON
    depth = 0
    for b in EDGES.finditer(code, start):
        ch = b.group()
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth < 0:
                return b.start()
            if depth == 0 and ch == "}" and braces:
                return b.end()
        elif depth == 0 and (ch == ";" or commas):
            return b.end()
    return len(code)


def enclosing_end(code, start):
    """Where the block an inner attribute ending at `start` is in closes,
    or the file's end."""
    depth = 0
    for b in EDGES.finditer(code, start):
        ch = b.group()
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth < 0:
                return b.start()
    return len(code)


def without_tests(code):
    """`code` with what every `#[cfg(test)]` is on blanked, the attribute
    and any after it with it, and all a `#![cfg(test)]` is inside."""
    out, last = [], 0
    for m in ATTRIBUTE.finditer(code):
        if m.start() < last:
            continue
        close = matching(code, m.end() - 1, 1)
        if close >= len(code):
            continue
        cfg = CFG.fullmatch(code, m.end(), close)
        if not cfg or not test_only(cfg.group(1)):
            continue
        end = enclosing_end(code, close + 1) if m.group(1) else item_end(code, close + 1)
        out.append(code[last : m.start()])
        out.append(spaces(code[m.start() : end]))
        last = end
    out.append(code[last:])
    return "".join(out)


TOKEN = re.compile(
    r'"[^"]*"'  # a literal, as `blank` left it
    r"|[A-Za-z_][A-Za-z0-9_]*"  # a name or a keyword
    r"|[0-9][0-9A-Za-z_]*(?:\.[0-9][0-9A-Za-z_]*)?"  # a number
    r"|'[A-Za-z_][A-Za-z0-9_]*"  # a lifetime or a label
    r"|::|==|!=|<=|>=|&&|\|\||->|=>|\.\.=|\.\.\.|\.\.|<<=|>>=|<<|>>|[-+*/%^&|]="
    r"|\S"
)

OPENERS = {"(": ")", "[": "]", "{": "}"}
CLOSERS = {")": "(", "]": "[", "}": "{"}
# Words no operand is nor ends with: where one begins or stops, or a
# type's or a pattern's word.
KEYWORDS = frozenset(
    "as async break const continue dyn else enum extern fn for if impl in let loop match mod move mut pub ref "
    "return static struct trait type unsafe use where while yield".split()
)
# A prefix an operand may begin with (`&mut x` its two); one following the
# end of an operand is the binary operator it is spelt as.
PREFIX = frozenset(("&", "&&", "*", "-", "!", "mut"))
# The operators binding tighter than `==`: an operand goes on across them.
BINARY = frozenset(("+", "-", "*", "/", "%", "<<", ">>", "&", "^", "|", "as"))
ASSIGNING = ("=", "+=", "-=", "*=", "/=", "%=", "^=", "&=", "|=", "<<=", ">>=")
# The tokens an operand read whole may follow, and may be followed by.
LEFT_ENDS = frozenset((";", ",", "(", "[", "{", "}", "=>", ":", "&&", "||", "..", "..=") + ASSIGNING)
RIGHT_ENDS = frozenset((";", ",", ")", "]", "}", "{", "=>", "&&", "||", "..", "..=", "else"))
# Where an expression may begin: a `|` there opens a closure's parameters.
EXPRESSION_STARTS = frozenset(
    ("(", "[", "{", ",", ";", "=>", ":", "&&", "||", "return", "move", "break", "async") + ASSIGNING
)
# What no generic argument list holds: a `<` or `>` met before its partner
# is a comparison's, or a shift's.
ANGLE_STOPS = frozenset((";", "{", "}", "(", ")", "[", "]", "==", "!=", "&&", "||", "=", "=>", "<=", ">="))


def is_name(t):
    return (t[0].isalpha() or t[0] == "_") and t not in KEYWORDS


def is_atom(t):
    """A name, a number or a literal: what an operand's path begins with."""
    return t[0] == '"' or t[0].isdigit() or is_name(t)


def partners(texts):
    """A function giving the token a bracket's partner is, or None: found
    as it is asked for, the comparisons being few and their brackets near
    them, where pairing every bracket of a file took a pass in Python over
    all its tokens."""
    found, n = {}, len(texts)

    def partner(i):
        if i in found:
            return found[i]
        step = 1 if texts[i] in OPENERS else -1
        depth, j = 0, i
        while 0 <= j < n:
            t = texts[j]
            if t in OPENERS:
                depth += step
            elif t in CLOSERS:
                depth -= step
            if depth == 0:
                found[i], found[j] = j, i
                return j
            j += step
        found[i] = None
        return None

    return partner


# What a closure's parameters never hold: met before the `|` opening them,
# the `|` read closes none.
NO_PARAMETERS = frozenset((";", "||", "&&", "==", "!=", "=", "=>", "(", "[", "{")) | (KEYWORDS - {"mut", "ref"})


def closes_parameters(texts, j, pair):
    """Whether the `|` at token `j` closes a closure's parameters, which
    end no operand: the `|` before it at its depth opens them, where an
    expression may begin (`.any(|t| *t == x)`)."""
    k = j - 1
    while k >= 0:
        t = texts[k]
        if t in CLOSERS:
            o = pair(k)
            if o is None:
                return False
            k = o - 1
        elif t == "|":
            return k == 0 or texts[k - 1] in EXPRESSION_STARTS
        elif t in NO_PARAMETERS:
            return False
        else:
            k -= 1
    return False


def turbofish_start(texts, j, pair):
    """The `::` of the `::<..>` the `>` (or `>>`) at token `j` closes, or
    None where it closes none: a comparison's, a shift's, a type's."""
    depth, k = 0, j
    while k >= 0:
        t = texts[k]
        if t in CLOSERS:
            o = pair(k)
            if o is None:
                return None
            k = o - 1
            continue
        if t == ">":
            depth += 1
        elif t == ">>":
            depth += 2
        elif t == "<":
            depth -= 1
        elif t == "<<":
            depth -= 2
        elif t in ANGLE_STOPS:
            return None
        if depth <= 0:
            return k - 1 if k >= 1 and texts[k - 1] == "::" else None
        k -= 1
    return None


def angle_end(texts, i, pair):
    """The `>` closing the `<` at token `i`, or None."""
    depth, k, n = 0, i, len(texts)
    while k < n:
        t = texts[k]
        if t in OPENERS:
            o = pair(k)
            if o is None:
                return None
            k = o + 1
            continue
        if t == "<":
            depth += 1
        elif t == "<<":
            depth += 2
        elif t == ">":
            depth -= 1
        elif t == ">>":
            depth -= 2
        elif t in ANGLE_STOPS:
            return None
        if depth <= 0:
            return k
        k += 1
    return None


def ends_operand(texts, k, pair):
    """Whether an operand may end at token `k`: a `&` after it is `and`."""
    t = texts[k]
    if t in (")", "]", "?") or is_atom(t):
        return True
    return t in (">", ">>") and turbofish_start(texts, k, pair) is not None


def unary_left(texts, i, pair):
    """Where the unary expression ending at token `i` begins, read back
    through its `?`, calls, indexes, fields, paths, `::<..>` and macros to
    its first name, literal or bracket, then its prefixes; None where no
    operand ends there."""
    j = i
    while True:
        if j < 0:
            return None
        t = texts[j]
        if t == "?":
            j -= 1
        elif t in CLOSERS:
            o = pair(j)
            if o is None:
                return None
            if o >= 2 and texts[o - 1] == "!" and is_name(texts[o - 2]):
                j = o - 2  # a macro's name: `format!(..)`
            elif t == "}":
                return None  # a block or a struct's literal: not read
            elif o >= 1 and ends_operand(texts, o - 1, pair):
                j = o - 1  # a call or an index, of what ends before it
            else:
                j = o  # a parenthesised expression, a tuple or an array
                break
        elif t in (">", ">>"):
            s = turbofish_start(texts, j, pair)
            if s is None:
                return None
            j = s - 1
        elif is_atom(t):
            if j >= 1 and texts[j - 1] in (".", "::"):
                j -= 2
            else:
                break
        else:
            return None
    while j >= 1 and texts[j - 1] in PREFIX and not (j >= 2 and ends_operand(texts, j - 2, pair)):
        j -= 1
    return j


def unary_right(texts, i, pair):
    """Where the unary expression beginning at token `i` ends, one past its
    last token: its prefixes, then a name, a literal or a bracket, read on
    through its fields, paths, `::<..>`, `?`, calls, indexes and macros;
    None where no operand begins there."""
    n, j = len(texts), i
    while j < n and texts[j] in PREFIX:
        j += 1
    if j >= n:
        return None
    if texts[j] in ("(", "["):
        o = pair(j)
        if o is None:
            return None
        j = o + 1
    elif is_atom(texts[j]):
        j += 1
    else:
        return None
    while j < n:
        t = texts[j]
        if t == "::" and j + 1 < n and texts[j + 1] == "<":
            e = angle_end(texts, j + 1, pair)
            if e is None:
                return None
            j = e + 1
        elif t in (".", "::"):
            if j + 1 < n and (texts[j + 1][0].isalnum() or texts[j + 1][0] == "_"):
                j += 2
            else:
                return None
        elif t == "?":
            j += 1
        elif t in ("(", "[") or (t == "!" and j + 1 < n and texts[j + 1] in OPENERS and is_name(texts[j - 1])):
            # A call, an index, or a macro's brackets after its name's `!`.
            o = pair(j if t != "!" else j + 1)
            if o is None:
                return None
            j = o + 1
        else:
            break
    return j


def operand_left(texts, k, pair):
    """The operand before the comparison at token `k`: its unary
    expressions [(first, last)], and whether it was read whole, back to a
    token an operand may follow."""
    units, last = [], k - 1
    while True:
        first = unary_left(texts, last, pair) if last >= 0 else None
        if first is None:
            return units[::-1], False
        units.append((first, last))
        op = first - 1
        bar = op >= 1 and texts[op] == "|" and closes_parameters(texts, op, pair)
        if op >= 1 and texts[op] in BINARY and not bar:
            last = op - 1
            continue
        whole = op < 0 or bar or texts[op] in LEFT_ENDS or texts[op] in KEYWORDS
        return units[::-1], whole


def operand_right(texts, k, pair):
    """The operand after the comparison at token `k`, as `operand_left`."""
    units, first, n = [], k + 1, len(texts)
    while True:
        end = unary_right(texts, first, pair) if first < n else None
        if end is None:
            return units, False
        units.append((first, end - 1))
        if end < n and texts[end] in BINARY:
            first = end + 1
            continue
        return units, end >= n or texts[end] in RIGHT_ENDS


def names_secret(name):
    low = name.lower()
    return any(w in low for w in WITHIN) or any(p in PART_WORDS for p in low.split("_"))


def secret_named(texts, units):
    """Whether an operand names a value whose name holds a secret's word,
    not its length or presence: a part after `as` is a type."""
    for first, last in units:
        if first > 0 and texts[first - 1] == "as":
            continue
        if "".join(texts[first : last + 1]).endswith(HARMLESS_ENDS):
            continue
        for k in range(first, last + 1):
            t = texts[k]
            if not (t[0].isalpha() or t[0] == "_") or (t[0].isupper() and not t.isupper()):
                continue  # no name, or a type's or a variant's
            if k + 1 < len(texts) and texts[k + 1] == "::":
                continue  # a module of a path
            if names_secret(t):
                return True
    return False


def public(texts, units, whole):
    """Whether an operand read whole is literals and constants alone: a
    type, a variant, a constant, a module of a path, a macro's name, a
    literal or `true` -- never a value's name, nor a static's
    (`ADMIN_TOKEN.get()`), which a method reads."""
    if not whole or not units:
        return False
    for first, last in units:
        if first > 0 and texts[first - 1] == "as":
            continue
        for k in range(first, last + 1):
            t = texts[k]
            if not (t[0].isalpha() or t[0] == "_") or t in ("true", "false"):
                continue
            after = texts[k + 1] if k + 1 < len(texts) else ""
            if after in ("::", "!"):
                continue
            if t[0].isupper() and not (t.isupper() and after == "."):
                continue
            return False
    return True


def findings(src):
    """Each comparison in `src` an operand of which names a secret and
    neither of which is public: its line and the comparison as written,
    its spaces made one."""
    code = without_tests(blank(src))
    if "==" not in code and "!=" not in code:
        return []
    # The tokens' texts alone, read at C's speed: a token's place is asked
    # of its match only where a comparison is refused.
    texts, pair, places, found = TOKEN.findall(code), None, None, []
    for k in sorted(at(texts, "==") + at(texts, "!=")):
        pair = pair or partners(texts)
        left, left_whole = operand_left(texts, k, pair)
        right, right_whole = operand_right(texts, k, pair)
        if not (secret_named(texts, left) or secret_named(texts, right)):
            continue
        if public(texts, left, left_whole) or public(texts, right, right_whole):
            continue
        places = places or list(TOKEN.finditer(code))
        a = places[left[0][0]].start() if left else places[k].start()
        d = places[right[-1][1]].end() if right else places[k].end()
        found.append((code.count("\n", 0, places[k].start()) + 1, " ".join(src[a:d].split())))
    return found


def at(texts, token):
    """Where `token` is among `texts`, found by `list.index`'s C loop."""
    out, k = [], -1
    try:
        while True:
            k = texts.index(token, k + 1)
            out.append(k)
    except ValueError:
        return out


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


def verdicts(found, allowed):
    """What fails, from each file's findings [(line, written)] and an
    ALLOWED: a comparison it does not name, or names a number of times
    other than the file has it."""
    failed = []
    for rel in sorted(set(found) | set(allowed)):
        entries, lines = allowed.get(rel, {}), {}
        for line, written in found.get(rel, []):
            lines.setdefault(written, []).append(line)
        for written, at in lines.items():
            if written not in entries:
                failed += [
                    f"{rel}:{line}: `{written}` compares a secret with == or !=: compare it with crypto::ct_eq, "
                    "or allow it in tools/secret_eq_check.py's ALLOWED, saying why"
                    for line in at
                ]
        for written, (count, _why) in entries.items():
            at = lines.get(written, [])
            if len(at) > count:
                failed.append(
                    f"{rel}: `{written}` is in ALLOWED {count} time(s) and in the code {len(at)}, lines "
                    f"{', '.join(map(str, at))}: compare the new one with crypto::ct_eq, or count it, saying why"
                )
            elif len(at) < count:
                failed.append(
                    f"{rel}: `{written}` is in ALLOWED {count} time(s) and in the code {len(at)}: "
                    "lower its count, or remove it"
                )
    return failed


def check():
    found = {}
    for rel, path in sources():
        with open(path, encoding="utf-8") as fh:
            got = findings(fh.read())
        if got:
            found[rel] = got
    failed = verdicts(found, ALLOWED)
    for message in failed:
        print(message)
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
    ("if self.password == given || entry.token == given {}", ["self.password == given", "entry.token == given"]),
    ("if token.len() != 32 || secret.is_empty() == false {}", []),
    ("if self.token.is_some() == other.is_some() {}", []),
    ("if token.len() + 1 == n {}", []),
    # Comments, strings and characters are no code.
    ('// token == x\nlet s = "token == x"; let r = r#"secret != y"#;', []),
    ("/* a /* nested */ mark == b */ let c = b'm' == x;", []),
    ("let q = '\\'' == c; let l: &'static str = \"\";", []),
    ('let s = br##"x "# token == y"##; let ok = c"token" == z;', []),
    ("fn f<'a>(token: &'a str, given: &'a str) -> bool { token == given }", ["token == given"]),
    # A test's items are left out, every other item read.
    (
        "#[cfg(test)]\nmod tests {\n    fn f() { assert!(token == x); }\n}\nfn g() -> bool { secret == y }",
        ["secret == y"],
    ),
    ('#[cfg(all(test, feature = "x"))]\nfn t() { mark == 1; }\nfn u() { 1 == 2 }', []),
    ('#[cfg(test)]\nconst X: &str = "x";\nfn v(a: &str) -> bool { a == token_of(X) }', ["a == token_of(X)"]),
    # ... to its `,` or the `}` around it, never on past it: a field, the
    # last field, a field of a literal, a match arm, an argument.
    (
        "struct S {\n    #[cfg(test)]\n    from: &'static str,\n    token: u8,\n}\n"
        "fn f(s: &S, given: u8) -> bool { s.token == given }",
        ["s.token == given"],
    ),
    ("struct S { a: u8, #[cfg(test)] b: u8 }\nfn g(token: &str, x: &str) -> bool { token == x }", ["token == x"]),
    ("let c = Calls { #[cfg(test)] from, ok: given == token };", ["given == token"]),
    (
        "fn f() -> Calls {\n    Calls { alloc, #[cfg(test)] from }\n}\n"
        "fn g(c: &Calls, given: u8) -> bool { c.secret == given }",
        ["c.secret == given"],
    ),
    (
        "match k {\n    #[cfg(test)]\n    Kind::Probe => 0,\n    Kind::Real => (token == given) as u8,\n}",
        ["token == given"],
    ),
    ("f(#[cfg(test)] probe, mark != given);", ["mark != given"]),
    # An item's `,`s, in its generics and its `where`, are its own, and a
    # `let`'s `}` too.
    (
        "#[cfg(test)]\nfn t<A, B>(a: A) where A: X, B: Y { assert!(token == a); }\n"
        "fn u(secret: u8, y: u8) -> bool { secret == y }",
        ["secret == y"],
    ),
    ("fn f() {\n    #[cfg(test)]\n    let x = S { a: token == y };\n    if token == y {}\n}", ["token == y"]),
    # The cfg as Rust reads it, and all an inner one is inside.
    ('#[cfg(all(feature = "x", test))]\nfn t() { token == x; }', []),
    ("#[cfg(any(test, unix))]\nfn u() { token == x; }", ["token == x"]),
    ("#[cfg(not(test))]\nfn v() { secret == y; }", ["secret == y"]),
    ("mod m {\n    #![cfg(test)]\n    fn f() { token == x; }\n}\nfn g() { secret == y; }", ["secret == y"]),
    # A literal or a constant is public: what it is held to is no guess.
    ('if token.as_deref() == Some("") || secret != 0 || mark == stored::OP_MARK {}', []),
    ("if body.first() == Some(&OP_MARK) || n.secrets == MAX_SECRETS || mark != -1 {}", []),
    ("if x == MAX_TOKEN_LEN || OP_MARK == mark || token == vec![0u8; 32] {}", []),
    # A static is no constant where a method reads it.
    ("if ADMIN_TOKEN.get() == Some(given) {}", ["ADMIN_TOKEN.get() == Some(given)"]),
    ("if KEYS.lock().unwrap().secret == given {}", ["KEYS.lock().unwrap().secret == given"]),
    # An operand is read whole: through `?` and `::<..>`, and across the
    # operators binding tighter; one it cannot read whole is not public.
    ("if secret.parse::<u64>()? == given {}", ["secret.parse::<u64>()? == given"]),
    ("if given == token.parse::<u64>().unwrap() {}", ["given == token.parse::<u64>().unwrap()"]),
    ("let ok = std::str::from_utf8(&token)? != given;", ["std::str::from_utf8(&token)? != given"]),
    (
        "if secret ^ mask == given || x as u64 != token as u64 {}",
        ["secret ^ mask == given", "x as u64 != token as u64"],
    ),
    ("let ok = given_token == match x { _ => y };", ["given_token =="]),
    ("let ok = S { a } == token;", ["== token"]),
    ("if xs.iter().any(|t| *t == token) {}", ["*t == token"]),
    # `sig`, `mac`, `hmac` and `proof` are a name's parts, never inside one.
    (
        "if given_sig == sig_of(b) || self.mac != tag || hmac(k, m) == given || proof == want || macs[i] == m {}",
        ["given_sig == sig_of(b)", "self.mac != tag", "hmac(k, m) == given", "proof == want", "macs[i] == m"],
    ),
    ("if signal == x || machine == m || design != d || proofread == p {}", []),
    # Not comparisons.
    ("if token <= x || secret >= y { let m = |mark| mark; match a { _ => {} } }", []),
]

# (source, its file's ALLOWED, how many failures the check reports): an
# entry lets as many through as it counts.
ONCE = {"t.token == token": (1, "a number")}
COUNTED = [
    ("fn f() { t.token == token; }", ONCE, 0),
    ("fn f() { t.token == token; }\nfn g() { t.token == token; }", ONCE, 1),
    ("fn f() {}", ONCE, 1),
    ("fn f() { t.token == token; x.token == token; }", ONCE, 1),
]


def self_test():
    failed = 0
    for src, want in CASES:
        got = [w for _, w in findings(src)]
        if got != want:
            failed += 1
            print(f"FAIL {src!r}: found {got}, want {want}")
    for src, entries, want in COUNTED:
        got = verdicts({"f.rs": findings(src)}, {"f.rs": entries})
        if len(got) != want:
            failed += 1
            print(f"FAIL {src!r} under {entries}: {got}, want {want} failure(s)")
    if failed:
        sys.exit(1)
    print(f"{len(CASES) + len(COUNTED)} cases read as meant")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        check()
