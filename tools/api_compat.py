#!/usr/bin/env python3
"""The C header and the JS types only grow (docs/design/compatibility.md).

`crates/fennec-ffi/include/fennec.h`, `web/fennec.d.ts` and
`web/client.d.ts` are read into dumps and held to the baselines committed in
integrations/api-golden/. An addition passes. What a caller could have
relied on fails unless BREAKS names it:

  * fennec.h: a function removed, its return type, a parameter's type or
    name, or the number or order of its parameters changed; a macro
    (`FENNEC_*`) removed or given another value. A parameter's name counts
    because two parameters of one type are told apart by it alone, so a
    rename is read as a reorder.
  * fennec.d.ts: an export removed or of another kind (a class made a type,
    a function a constant); a function's, constant's or type alias's
    declaration changed; an overload dropped; a class's or interface's
    heading (its type parameters, `extends`, `implements`) changed; a
    member removed, changed or one of its overloads dropped. A declaration
    is compared as written, whitespace and comments aside, so a narrowed
    parameter, a widened return, an optional member made required and a
    new required parameter all change it -- and so does a parameter
    widened or a return narrowed, which is no break: that, and a type's
    own words moved with the same meaning, are named in BREAKS too, as a
    pattern loosened is in tools/openapi_compat.py (a type checker alone
    tells which). What it cannot see: a non-exported helper type's body, a
    new required member of an interface a caller implements, an overload
    added in front of another, a type alias's meaning through the names it
    uses, and whether a declaration compiles at all (`make types-check`
    does).
  * client.d.ts: a name it re-exports no longer exported, or a value
    (`export {..}`) become a type (`export type {..}`).

  * order: a declaration's overloads, and a member's, are held in the order
    written, since the first that fits is the one a caller resolves; one
    reordered is "overload order changed". An overload added anywhere
    passes, though one added in front of another can change which a caller
    resolves (a type checker alone tells).

A break made on purpose is named in BREAKS below: the surface, the symbol,
what changed, the declaration before and after as digests (printed ready to
paste), the release it goes out in and why. An entry names one change, not
a kind of change: another change to the same symbol is a break of its own.
The baselines are dumps of the sources, always: a dump that is not what the
sources dump to fails, naming `make api-golden`, and a pull request's
dumps are held to main's, every entry of main's in the pull request's
unless a BREAKS entry names that difference. What 1.0 changes is the rule
for BREAKS: until `--freeze 1.0` (integrations/api-golden/frozen.json) an
entry's release is "1.0", the changes made on the way there; after it, a
major version above the frozen one, and `make version` refuses 1.0.0 or
later while the baselines are not frozen (RELEASING.md).

What the scanner does not know, it refuses (exit 2), never skips: in the
.d.ts files `export { a as b }`, `export * from`, `export default`, and a
class and a namespace or function of one name (declaration merging, but for
interfaces); in fennec.h a `typedef`, `struct`, `union` or `enum`, a
function-like macro, and a prototype under an `#if` (a macro under one is
recorded with its condition; the include guard and `__cplusplus` are not
conditions). A change that needs one of these teaches the scanner in a pull
request of its own first: main's copy of the checker judges the next one,
and would refuse the form.

    python3 tools/api_compat.py                       # the committed baselines
    python3 tools/api_compat.py --write               # make api-golden: the baselines again
    python3 tools/api_compat.py --freeze 1.0          # the 1.0 tag's, once (RELEASING.md)
    python3 -I main_copy.py --root . --base-ref FETCH_HEAD \\
        --breaks-from tools/api_compat.py             # CI: main's checker

No dependency: the declarations are read by the scanner below, which knows
this repository's own formatting (a top-level declaration starts in column
0), not TypeScript's grammar. A line in column 0 it does not know is an
error, never skipped.
"""

import argparse
import ast
import hashlib
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BASELINE = os.path.join("integrations", "api-golden")
SOURCES = {
    "fennec.h": os.path.join("crates", "fennec-ffi", "include", "fennec.h"),
    "fennec.d.ts": os.path.join("web", "fennec.d.ts"),
    "client.d.ts": os.path.join("web", "client.d.ts"),
}
SURFACES = tuple(SOURCES)
FROZEN = "frozen.json"


def baseline_name(surface):
    return surface.replace(".", "-") + ".json"


# Every break made on purpose, in the order they went out: never removed, so
# the list is the API's history of them. Each says why, and what a caller
# does instead; a break before the 1.0 tag is named with release "1.0", and
# its entry stays as history once the baselines are frozen there. `before`
# and `after` are the digests the failure prints (`digest`): the entry names
# that one change. A
# literal: main's checker reads it from a pull request's copy as data, never
# running it (`--breaks-from`).
#
#   {"surface": "fennec.d.ts", "symbol": "Query.where", "change": "member changed",
#    "before": "0123456789ab", "after": "ba9876543210", "release": "2.0", "reason": "takes a Where only: pass { ... } instead of the builder"}
BREAKS = []


# ------------------------------------------------------------ comments


def strip_comments(text, line_comments=True):
    """`text` without its comments, a block comment as one space, a line
    comment as nothing: strings and template literals are walked past, so a
    `//` in one stays."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c in "'\"`":
            j = i + 1
            while j < n and text[j] != c:
                j += 2 if text[j] == "\\" else 1
            out.append(text[i : j + 1])
            i = j + 1
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            if j < 0:
                raise ValueError("a comment that never ends")
            # Keep the newlines: a statement still starts on its line.
            out.append(" " + "\n" * text.count("\n", i, j))
            i = j + 2
        elif line_comments and text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def squeeze(text):
    """Whitespace as one space and no space inside or before a bracket, a
    comma or a semicolon, no trailing comma: what a formatter may change."""
    t = re.sub(r"\s+", " ", text).strip()
    t = re.sub(r"([(\[{<]) ", r"\1", t)
    t = re.sub(r" ([)\]}>,;])", r"\1", t)
    t = re.sub(r",(?=[)\]}>])", "", t)
    t = re.sub(r";(?=\})", "", t)
    return t


# ------------------------------------------------------------ fennec.h


def c_type(tokens):
    """A C type's tokens as one text: a space between words, none around `*`."""
    out = ""
    for t in tokens:
        if out and re.match(r"\w", t) and re.match(r"\w", out[-1]):
            out += " "
        out += t
    return out


def c_prototype(text):
    """`{"name", "returns", "params": [{"type", "name"}]}` of one declaration."""
    toks = re.findall(r"\w+|\*|,|\(|\)|\.\.\.", text)
    if "(" not in toks or toks[-1] != ")":
        raise ValueError(f"not a prototype: {text!r}")
    open_at = toks.index("(")
    head = toks[:open_at]
    if len(head) < 2 or not re.match(r"\w", head[-1]):
        raise ValueError(f"no name before the parameters: {text!r}")
    inner = toks[open_at + 1 : -1]
    params = []
    if inner and inner != ["void"]:
        piece = []
        for t in inner + [","]:
            if t == ",":
                words = [x for x in piece if re.match(r"\w", x)]
                if piece == ["..."]:
                    params.append({"type": "...", "name": ""})
                elif len(words) < 2 or not re.match(r"\w", piece[-1]):
                    raise ValueError(f"a parameter with no name in {text!r}")
                else:
                    params.append({"type": c_type(piece[:-1]), "name": piece[-1]})
                piece = []
            else:
                piece.append(t)
    return {"name": head[-1], "returns": c_type(head[:-1]), "params": params}


def dump_header(text):
    """The macros and the functions of fennec.h. A macro under an `#if` is
    recorded with its condition (`value [if COND]`); a prototype under one,
    a function-like macro and a typedef, struct, union or enum are refused."""
    body = strip_comments(text)
    macros, functions = {}, {}
    rest = []
    conds = []  # one per open #if: its condition, or None for a guard
    first_if = True
    pending_guard = None
    for line in body.split("\n"):
        s = line.strip()
        if s.startswith("#"):
            d = re.match(r"#\s*(\w+)\s*(.*)$", s)
            word, arg = (d.group(1), d.group(2).strip()) if d else ("", "")
            if word in ("if", "ifdef", "ifndef"):
                cond = {"if": arg, "ifdef": f"defined({arg})", "ifndef": f"!defined({arg})"}[word]
                # The include guard (`#ifndef X` then `#define X`) and
                # C++'s `extern "C"` are no condition on what is declared.
                if word == "ifndef" and first_if:
                    pending_guard = arg
                conds.append(None if arg == "__cplusplus" and word == "ifdef" else cond)
                first_if = False
            elif word in ("else", "elif"):
                if not conds:
                    raise ValueError(f"#{word} with no #if")
                if conds[-1] is not None:
                    conds[-1] = f"!({conds[-1]})" if word == "else" else f"!({conds[-1]}) && ({arg})"
            elif word == "endif":
                if not conds:
                    raise ValueError("#endif with no #if")
                conds.pop()
            elif word == "define":
                m = re.match(r"(\w+)(?:[ \t]+(.*))?$", arg)
                if not m:
                    raise ValueError(f"a macro the scanner does not know (function-like?): {s!r}")
                if pending_guard is not None:
                    if m.group(1) == pending_guard:
                        conds[0] = None
                    pending_guard = None
                if m.group(2) and m.group(2).strip():
                    value = squeeze(m.group(2))
                    live = [c for c in conds if c is not None]
                    if live:
                        value += " [if " + " && ".join(live) + "]"
                    macros[m.group(1)] = value
            elif word == "undef":
                raise ValueError(f"#undef is not known to the scanner: {s!r}")
            continue
        if s and any(c is not None for c in conds):
            raise ValueError(f"a declaration under an #if the scanner does not know: {s!r}")
        rest.append(line)
    code = "\n".join(rest)
    if re.search(r"\b(typedef|struct|union|enum)\b", code):
        raise ValueError("a typedef, struct, union or enum in fennec.h: the scanner does not know it")
    code = re.sub(r'extern\s+"C"\s*\{', "", code)
    code = code.replace("}", "")
    for decl in code.split(";"):
        decl = squeeze(decl)
        if not decl or decl.startswith(("#include",)):
            continue
        p = c_prototype(decl)
        if p["name"] in functions:
            raise ValueError(f"{p['name']} declared twice")
        functions[p["name"]] = {"returns": p["returns"], "params": p["params"]}
    return {"macros": macros, "functions": functions}


# ------------------------------------------------------------ the .d.ts files


STARTERS = re.compile(r"^(export|declare|type|interface|function|class|const|import|abstract|enum|namespace)\b")


def statements(text):
    """The top-level statements of a .d.ts file, comments gone, each one's
    lines joined: a statement starts at a line in column 0 that opens with a
    declaration's keyword and runs to the next such line."""
    out = []
    for line in strip_comments(text).split("\n"):
        if not line.strip():
            continue
        if line[0] in " \t":
            if not out:
                raise ValueError(f"indented text before any statement: {line!r}")
            out[-1].append(line)
        elif STARTERS.match(line):
            out.append([line])
        elif line[0] in "}])>|&?:.=" or line.startswith("..."):
            if not out:
                raise ValueError(f"a continuation before any statement: {line!r}")
            out[-1].append(line)
        else:
            raise ValueError(f"a line in column 0 the scanner does not know: {line!r}")
    return [squeeze(" ".join(s)).rstrip(";") for s in out]


def walk(text, start=0):
    """Yields (index, char, depth) over `text` from `start`, depth counting
    (), [], {} and <>, the arrow `=>` and strings not counted; the depth is
    that before the char for an opener's own char and after for a closer's."""
    depth = 0
    i, n = start, len(text)
    while i < n:
        c = text[i]
        if c in "'\"`":
            j = i + 1
            while j < n and text[j] != c:
                j += 2 if text[j] == "\\" else 1
            i = j + 1
            continue
        if c in "([{<":
            yield i, c, depth
            depth += 1
        elif c in ")]}":
            depth -= 1
            yield i, c, depth
        elif c == ">":
            if i > 0 and text[i - 1] == "=":
                yield i, c, depth
            else:
                depth -= 1
                yield i, c, depth
        else:
            yield i, c, depth
        i += 1


def split_depth0(text, seps):
    parts, last = [], 0
    for i, c, depth in walk(text):
        if depth == 0 and c in seps:
            parts.append(text[last:i])
            last = i + 1
    parts.append(text[last:])
    return [p.strip() for p in parts if p.strip()]


MODIFIERS = r"(?:(?:readonly|static|public|private|protected|abstract|declare|override|get|set|async)\s+)*"


def member_name(member):
    """The name a class's or interface's member is grouped by, or None where
    it is not API (a private one other than the constructor)."""
    m = re.match(r"(" + MODIFIERS + r")(.*)$", member)
    mods, rest = m.group(1), m.group(2)
    if rest.startswith("["):
        end = next(i for i, c, d in walk(rest) if c == "]" and d == 0)
        inner = rest[1 : end]
        return "[index]" if ":" in inner else "[" + inner + "]"
    if rest.startswith(("(", "<")):
        return "()"
    name = re.match(r"new\b|[A-Za-z_$][\w$]*|'[^']*'|\"[^\"]*\"|#\w+", rest)
    if not name:
        raise ValueError(f"no name in the member {member!r}")
    n = name.group(0)
    if n.startswith("#") or ("private" in mods and n != "constructor"):
        return None
    return n


NS_KEYWORDS = r"(?:export\s+)?(?:declare\s+)?(?:abstract\s+)?(?:const\s+enum|class|interface|function|const|let|var|type|enum|namespace)\s+"
NS_BODIED = re.compile(r"(?:export\s+)?(?:declare\s+)?(?:abstract\s+)?(?:const\s+enum|class|interface|enum|namespace)\b")


def namespace_pieces(inner):
    """A namespace body's declarations: split at `;`, and after a `}` that
    closes a class's, interface's, enum's or namespace's own body, which
    ends no statement with a `;`."""
    pieces, last = [], 0
    for i, c, depth in walk(inner):
        if depth != 0:
            continue
        if c == ";" or (c == "}" and NS_BODIED.match(inner[last:].lstrip())):
            pieces.append(inner[last : i + (c == "}")])
            last = i + 1
    pieces.append(inner[last:])
    return [p.strip() for p in pieces if p.strip()]


def namespace_member_name(member):
    m = re.match(NS_KEYWORDS + r"([\w$]+)", member)
    if not m:
        raise ValueError(f"no name in the namespace member {member!r}")
    return m.group(1)


def declaration(stmt):
    """(name, kind, rest) of an exported statement, or None for one that is
    not exported."""
    if not stmt.startswith("export "):
        return None
    s = stmt[len("export ") :]
    s = re.sub(r"^declare\s+", "", s)
    m = re.match(r"(abstract\s+class|class|interface|function|const\s+enum|const|let|var|type|enum|namespace)\s+([\w$]+)", s)
    if not m:
        raise ValueError(f"an export the scanner does not know: {stmt[:80]!r}")
    kind = m.group(1).split()[-1]
    return m.group(2), {"let": "const", "var": "const"}.get(kind, kind), s


def dump_declarations(text):
    exports = {}
    for stmt in statements(text):
        d = declaration(stmt)
        if d is None:
            continue
        name, kind, s = d
        entry = exports.setdefault(name, {"kind": kind})
        if entry["kind"] != kind:
            raise ValueError(f"{name} is a {entry['kind']} and a {kind}")
        if kind in ("class", "interface", "enum", "namespace"):
            # An interface declared twice merges (TypeScript's declaration
            # merging); a class, an enum and a namespace here cannot be.
            if "members" in entry and kind != "interface":
                raise ValueError(f"{name} declared twice")
            body_at = next((i for i, c, d in walk(s) if c == "{" and d == 0), None)
            if body_at is None:
                raise ValueError(f"{name} has no body")
            close = next(i for i, c, d in walk(s, body_at) if c == "}" and d == 0)
            head = s[:body_at].strip()
            entry["head"] = entry["head"] + " + " + head if "head" in entry else head
            members = {k: list(v) for k, v in entry.get("members", {}).items()}
            inner = s[body_at + 1 : close]
            pieces = namespace_pieces(inner) if kind == "namespace" else split_depth0(inner, ";,")
            for member in pieces:
                key = namespace_member_name(member) if kind == "namespace" else member_name(member)
                if key is not None:
                    members.setdefault(key, []).append(member)
            entry["members"] = {k: members[k] for k in sorted(members)}
        else:
            entry.setdefault("declarations", []).append(s)
    return {"exports": {k: exports[k] for k in sorted(exports)}}


def dump_client(text):
    """The names client.d.ts re-exports, each a `value` or a `type`."""
    out = {}
    for stmt in statements(text):
        m = re.match(r"export (type )?\{(.*)\} from '[^']*'$", stmt)
        if not m:
            raise ValueError(f"client.d.ts holds only re-exports; not {stmt[:80]!r}")
        for item in m.group(2).split(","):
            item = item.strip()
            if not item:
                continue
            name = item.split(" as ")[-1].strip()
            if name in out:
                raise ValueError(f"{name} exported twice")
            out[name] = "type" if m.group(1) else "value"
    return {"exports": {k: out[k] for k in sorted(out)}}


def dump(surface, text):
    if surface == "fennec.h":
        return dump_header(text)
    if surface == "fennec.d.ts":
        return dump_declarations(text)
    return dump_client(text)


# ------------------------------------------------------------ comparing


def digest(text):
    """Twelve hex digits naming a declaration's text: what a BREAKS entry
    holds of what changed, short enough to paste."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:12]


def prototype(name, f):
    return f"{f['returns']} {name}({', '.join((p['type'] + ' ' + p['name']).strip() for p in f['params'])})"


def header_breaks(old, new):
    """(symbol, change, before, after): the text of each side of a change."""
    out = []
    for name, value in old["macros"].items():
        if name not in new["macros"]:
            out.append((name, "macro removed", value, ""))
        elif new["macros"][name] != value:
            out.append((name, "macro value changed", value, new["macros"][name]))
    for name, f in old["functions"].items():
        g = new["functions"].get(name)
        if g is None:
            out.append((name, "function removed", prototype(name, f), ""))
            continue
        if f["returns"] != g["returns"]:
            out.append((name, "return type changed", f["returns"], g["returns"]))
        if len(f["params"]) != len(g["params"]):
            out.append((name, "parameter count changed", prototype(name, f), prototype(name, g)))
            continue
        for i, (p, q) in enumerate(zip(f["params"], g["params"])):
            if p["type"] != q["type"]:
                out.append((name, "parameter type changed", f"#{i + 1} {p['name']}: {p['type']}", f"#{i + 1} {q['name']}: {q['type']}"))
            elif p["name"] != q["name"]:
                out.append((name, "parameter name changed", f"#{i + 1}: {p['name']}", f"#{i + 1}: {q['name']}"))
    return out


def in_order(old, new):
    """Whether `old`'s entries are all in `new`, in the order they have in
    `old` (others may lie between them)."""
    it = iter(new)
    return all(any(s == t for t in it) for s in old)


def lost(old, new, changed):
    """The change from the signatures `old` to `new`, as (change, before,
    after) or None: one gone is an overload removed where `new` has fewer,
    `changed` otherwise; all there but in another order is a reorder."""
    before, after = "\n".join(old), "\n".join(new)
    if [s for s in old if s not in new]:
        return ("overload removed" if len(new) < len(old) else changed, before, after)
    if not in_order(old, new):
        return ("overload order changed", before, after)
    return None


def declaration_breaks(old, new):
    out = []
    for name, e in old["exports"].items():
        n = new["exports"].get(name)
        if n is None:
            out.append((name, "export removed", e["kind"], ""))
            continue
        if e["kind"] != n["kind"]:
            out.append((name, "kind changed", e["kind"], n["kind"]))
            continue
        if "members" in e:
            if e["head"] != n["head"]:
                out.append((name, "head changed", e["head"], n["head"]))
            for m, sigs in e["members"].items():
                at = f"{name}.{m}"
                if m not in n["members"]:
                    out.append((at, "member removed", "\n".join(sigs), ""))
                    continue
                got = lost(sigs, n["members"][m], "member changed")
                if got:
                    out.append((at, *got))
        else:
            got = lost(e["declarations"], n["declarations"], "declaration changed")
            if got:
                out.append((name, *got))
    return out


def client_breaks(old, new):
    out = []
    for name, kind in old["exports"].items():
        got = new["exports"].get(name)
        if got is None:
            out.append((name, "export removed", kind, ""))
        elif kind == "value" and got == "type":
            out.append((name, "kind changed", "value", "type"))
    return out


def breaks(surface, old, new):
    """(symbol, change, before, after) for each way `new` breaks `old`."""
    if surface == "fennec.h":
        return header_breaks(old, new)
    if surface == "fennec.d.ts":
        return declaration_breaks(old, new)
    return client_breaks(old, new)


def additions(surface, old, new):
    """What `new` has that `old` has not: a count by the surface's own unit."""
    if surface == "fennec.h":
        return [k for k in new["macros"] if k not in old["macros"]] + [
            k for k in new["functions"] if k not in old["functions"]
        ]
    return [k for k in new["exports"] if k not in old["exports"]]


def major(release):
    return int(release.split(".")[0])


def well_formed(entries, frozen=None):
    """What is wrong with BREAKS. `frozen` is the release the baselines were
    frozen at, or None: until then every entry is for "1.0"; after, an entry
    for a release up to the frozen one is history (it approves nothing, see
    `approves`) and any other is a major version above it."""
    wrong = []
    keys = {"surface", "symbol", "change", "before", "after", "release", "reason"}
    for i, e in enumerate(entries):
        if not isinstance(e, dict) or set(e) != keys:
            wrong.append(f"BREAKS[{i}]: keys {sorted(e) if isinstance(e, dict) else e!r}, not {sorted(keys)}")
        elif e["surface"] not in SURFACES:
            wrong.append(f"BREAKS[{i}]: surface {e['surface']!r} is none of {SURFACES}")
        elif not all(isinstance(e[k], str) for k in keys) or not all(e[k].strip() for k in keys - {"before", "after"}):
            wrong.append(f"BREAKS[{i}]: every key holds text")
        elif not all(re.fullmatch(r"[0-9a-f]{12}", e[k]) for k in ("before", "after")):
            wrong.append(f"BREAKS[{i}]: before and after are the 12 hex digits the failure printed")
        elif e["reason"].startswith("<"):
            wrong.append(f"BREAKS[{i}]: no reason given")
        elif not re.fullmatch(r"\d+\.\d+", e["release"]):
            wrong.append(f"BREAKS[{i}]: release {e['release']!r} is not a MAJOR.MINOR version")
        elif frozen is None and e["release"] != "1.0":
            wrong.append(f"BREAKS[{i}]: release {e['release']!r}: until the baselines are frozen at 1.0 every break goes out in \"1.0\"")
    return wrong


def approves(entry, frozen):
    """Whether an entry may let a break through: before the freeze one for
    "1.0", after it one for a major version above the frozen one."""
    return entry["release"] == "1.0" if frozen is None else major(entry["release"]) > major(frozen)


def judge(surface, old, new, allowed, frozen=None):
    """The breaks from `old` to `new`, split: (symbol, change, detail,
    reason) those BREAKS names, and (symbol, change, detail, why) the rest."""
    passed, failed = [], []
    for symbol, change, before, after in breaks(surface, old, new):
        detail = f"{before} -> {after}" if after else before
        d_before, d_after = digest(before), digest(after)
        named = [
            e
            for e in allowed
            if isinstance(e, dict)
            and (e.get("surface"), e.get("symbol"), e.get("change"), e.get("before"), e.get("after"))
            == (surface, symbol, change, d_before, d_after)
            and approves(e, frozen)
        ]
        if named:
            passed.append((symbol, change, detail, named[0]["reason"]))
        else:
            entry = {
                "surface": surface,
                "symbol": symbol,
                "change": change,
                "before": d_before,
                "after": d_after,
                "release": "<the release it goes out in>",
                "reason": "<why, and what a caller does instead>",
            }
            failed.append((symbol, change, detail, f"if on purpose, name it in BREAKS (tools/api_compat.py):\n      {json.dumps(entry)}"))
    return passed, failed


# ------------------------------------------------------------ running


def read_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def written(doc):
    """A baseline as committed: indented, in the dump's own order."""
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def breaks_in(path):
    """BREAKS as the checker at `path` writes it, read as data."""
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read(), path)
    for node in tree.body:
        targets = node.targets if isinstance(node, ast.Assign) else [getattr(node, "target", None)]
        if any(isinstance(t, ast.Name) and t.id == "BREAKS" for t in targets):
            return ast.literal_eval(node.value)
    raise ValueError(f"{path} assigns no BREAKS")


def at_ref(ref, path, root):
    p = subprocess.run(["git", "show", f"{ref}:{path}"], cwd=root, capture_output=True, text=True)
    return json.loads(p.stdout) if p.returncode == 0 else None


def read_frozen(baseline_dir):
    """The release the baselines were frozen at ("1.0"), or None before."""
    path = os.path.join(baseline_dir, FROZEN)
    if not os.path.exists(path):
        return None
    doc = read_json(path)
    if not isinstance(doc, dict) or set(doc) != {"frozen"} or not (
        doc["frozen"] is None or (isinstance(doc["frozen"], str) and re.fullmatch(r"\d+\.\d+", doc["frozen"]))
    ):
        raise ValueError(f"{path}: expected {{\"frozen\": null}} or {{\"frozen\": \"1.0\"}}")
    return doc["frozen"]


def check(built, baseline, base=None, allowed=BREAKS, out=print, frozen=None, base_frozen=None):
    """Holds each built dump to its baseline -- equal, the baseline being a
    dump of the sources -- and the baseline to `base`'s where given
    (surface -> dump, or None): dump against dump. Returns the number of
    failures, said."""
    failures = 0
    for line in well_formed(allowed, frozen):
        out(line)
        failures += 1
    if base_frozen is not None and frozen != base_frozen:
        out(f"integrations/api-golden/{FROZEN} says {frozen!r}, the base branch's {base_frozen!r}: a freeze is not undone")
        failures += 1
    for surface in SURFACES:
        new = built[surface]
        name = baseline_name(surface)
        against = [("the committed baseline", baseline.get(surface), new)]
        if base is not None:
            against.append(("the base branch's baseline", base.get(surface), baseline.get(surface)))
        told = set()
        for what, old, now in against:
            if old is None or now is None:
                if what == "the committed baseline":
                    out(f"{surface}: integrations/api-golden/{name} is missing: run `make api-golden`")
                    failures += 1
                else:
                    out(f"{surface}: no baseline in {what}: nothing to hold it to")
                continue
            passed, failed = judge(surface, old, now, allowed, frozen)
            for symbol, change, detail, reason in passed:
                if (symbol, change, detail) not in told:
                    told.add((symbol, change, detail))
                    out(f"{surface}: {symbol}: {change} ({detail}): allowed by BREAKS: {reason}")
            for symbol, change, detail, why in failed:
                # Folded only where the whole line repeats (the committed pass
                # and the base pass finding one break): breaks that share a
                # symbol and a change differ in their text and are each judged.
                if (symbol, change, detail) in told:
                    continue
                told.add((symbol, change, detail))
                failures += 1
                head = f"BREAKING against {what} (integrations/api-golden/{name})"
                if what != "the committed baseline":
                    head += (
                        ": the committed baseline was written again past this break, or edited by hand; a "
                        "break on purpose is named in BREAKS, never by writing the baseline alone"
                    )
                out(f"{head}\n  {surface}: {symbol}: {change} ({detail})\n    {why}")
        if baseline.get(surface) is not None and baseline[surface] != new:
            failures += 1
            out(
                f"{surface}: integrations/api-golden/{name} is not what {SOURCES[surface]} dumps to: "
                "run `make api-golden` and commit it"
            )
    return failures


def freeze(version, built, baseline, allowed, baseline_dir, frozen, out=print):
    """Writes the freeze, or returns what stops it: the dumps are the
    sources', every BREAKS entry is for a release already made (not above
    `version`), and `version` is above any freeze before."""
    wrong = []
    # One value only: a freeze is a major version's, so 1.0 first and then
    # the next major's .0, never a minor or a skipped major.
    takes = "1.0" if frozen is None else f"{major(frozen) + 1}.0"
    if version != takes:
        wrong.append(f"{version!r}: a freeze at this point takes {takes!r} alone")
    for surface in SURFACES:
        if baseline[surface] != built[surface]:
            wrong.append(f"{surface}: the baseline is not what the sources dump to: run `make api-golden` first")
    for i, e in enumerate(allowed):
        if isinstance(e, dict) and isinstance(e.get("release"), str) and re.fullmatch(r"\d+\.\d+", e["release"]):
            if not wrong and major(e["release"]) > major(version):
                wrong.append(f"BREAKS[{i}] is for release {e['release']}, which is not made yet")
    for line in wrong:
        out(line)
    if wrong:
        return 1
    with open(os.path.join(baseline_dir, FROZEN), "w", encoding="utf-8") as f:
        f.write(written({"frozen": version}))
    out(f"froze the baselines at {version}: from here a break is a major version above it")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", default=ROOT, help="the checkout: where the sources and baselines are, and git runs")
    ap.add_argument("--baseline", help="the committed baselines (<root>/integrations/api-golden)")
    ap.add_argument("--base-ref", help="also hold the baselines to those at this git ref (CI: main)")
    ap.add_argument(
        "--breaks-from",
        help="BREAKS read as data from this checker, not this one's own (CI: main's copy run with the "
        "pull request's breaks)",
    )
    ap.add_argument("--write", action="store_true", help="write the baselines again, refusing a break BREAKS names not")
    ap.add_argument("--freeze", metavar="VERSION", help="freeze the baselines at VERSION (1.0), once: the tag's step")
    ap.add_argument("--require-frozen", action="store_true", help="exit 1 unless the baselines are frozen (make version)")
    a = ap.parse_args(argv)
    root = os.path.abspath(a.root)
    baseline_dir = a.baseline or os.path.join(root, BASELINE)
    try:
        frozen = read_frozen(baseline_dir)
    except (OSError, ValueError) as e:
        print(e, file=sys.stderr)
        return 2
    if a.require_frozen:
        if frozen is None:
            print(
                "the API baselines are not frozen: a 1.0.0 or later release needs `python3 tools/api_compat.py "
                "--freeze 1.0` first (RELEASING.md)",
                file=sys.stderr,
            )
            return 1
        return 0
    allowed = BREAKS
    if a.breaks_from:
        try:
            allowed = breaks_in(a.breaks_from)
        except (OSError, SyntaxError, ValueError) as e:
            print(f"--breaks-from {a.breaks_from}: {e}", file=sys.stderr)
            return 2
    built = {}
    for surface, rel in SOURCES.items():
        try:
            with open(os.path.join(root, rel), encoding="utf-8") as f:
                built[surface] = dump(surface, f.read())
        except (OSError, ValueError) as e:
            print(f"{rel}: {e}", file=sys.stderr)
            return 2
    baseline = {}
    for surface in SURFACES:
        path = os.path.join(baseline_dir, baseline_name(surface))
        baseline[surface] = read_json(path) if os.path.exists(path) else None
    if a.freeze:
        return freeze(a.freeze, built, baseline, allowed, baseline_dir, frozen)
    if a.write:
        failed = 0
        for line in well_formed(allowed, frozen):
            failed += 1
            print(line)
        for surface in SURFACES:
            if baseline[surface] is not None:
                for symbol, change, detail, why in judge(surface, baseline[surface], built[surface], allowed, frozen)[1]:
                    failed += 1
                    print(f"BREAKING: {surface}: {symbol}: {change} ({detail})\n    {why}")
        if failed:
            print(f"{failed} failure(s): the baselines are left as they were")
            return 1
        os.makedirs(baseline_dir, exist_ok=True)
        for surface in SURFACES:
            with open(os.path.join(baseline_dir, baseline_name(surface)), "w", encoding="utf-8") as f:
                f.write(written(built[surface]))
            print(f"wrote {BASELINE}/{baseline_name(surface)}")
        if not os.path.exists(os.path.join(baseline_dir, FROZEN)):
            with open(os.path.join(baseline_dir, FROZEN), "w", encoding="utf-8") as f:
                f.write(written({"frozen": None}))
        return 0
    base, base_frozen = None, None
    if a.base_ref:
        known = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"{a.base_ref}^{{commit}}"], cwd=root, capture_output=True
        )
        if known.returncode != 0:
            print(f"--base-ref {a.base_ref} names no commit (fetch it first)", file=sys.stderr)
            return 2
        base = {s: at_ref(a.base_ref, f"{BASELINE}/{baseline_name(s)}", root) for s in SURFACES}
        got = at_ref(a.base_ref, f"{BASELINE}/{FROZEN}", root)
        base_frozen = got.get("frozen") if isinstance(got, dict) else None
    failures = check(built, baseline, base, allowed, frozen=frozen, base_frozen=base_frozen)
    if failures:
        print(f"\n{failures} failure(s): docs/design/compatibility.md says what may change and how")
        return 1
    grew = ""
    if base is not None:
        n = sum(len(additions(s, base[s], baseline[s])) for s in SURFACES if base[s] is not None)
        grew = f"; {n} added since the base branch's"
    print(f"fennec.h and the JS types only grow: the baselines are what the sources dump to{grew}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
