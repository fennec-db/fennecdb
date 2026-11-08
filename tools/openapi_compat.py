#!/usr/bin/env python3
"""The REST API only grows (docs/design/http.md#compatibility).

fennec-server's OpenAPI documents -- a file's server, a tenant node and a
router, as `GET /_openapi.json` serves them -- are held to the baseline
committed in integrations/openapi/. An addition passes, and the same pull
request writes the baseline again (`make openapi-golden`). A change a
client could have relied on fails:

  * an operation removed, or its method or path changed; its
    `operationId` or a path parameter renamed (a generated client names
    its method and arguments by them); a flag it is served under added
    (`x-fennec-flag`: a server started without it answers 404);
  * a parameter, a request body or a body field that became required, or a
    required one added; a parameter or a request field removed;
  * a request type narrowed, a path parameter's among them: a type, an
    enum, a bound, a format but int32 -> int64 and float -> double, a
    multipleOf but a divisor of the old, an alternative of a oneOf no
    longer taken, `additionalProperties` closed or given a schema, every
    part of an `allOf` held; a `pattern` changed at all, since whether one
    takes less cannot be told: a pattern loosened is named in BREAKS too;
  * a `default` changed or dropped. The documents write none today -- what
    a parameter left out means is its description's words, which nothing
    here reads -- so a default changing is a break named by hand;
  * a documented response status removed (one a range such as `2XX` covers
    is not), a response field, header or media type removed, a response
    field's or header's type changed, or one that was required no longer;
  * a security requirement added to an operation that took none, a way of
    authenticating it took no longer taken or asking more scopes, or a
    security scheme it uses defined otherwise (its type, scheme, `in` or
    name, an OAuth flow's URL or scope).

A break made on purpose is named in BREAKS below -- the kind, the
operation, the change, the reason and the document version it went out
in -- and moves the document's version (`DOCUMENT_VERSION` in
crates/fennec-http/src/openapi.rs), a whole number, forward: past the
baseline's, never back. The baseline is never the way: CI also holds the
documents to the baseline main has (`--base-ref`) with main's copy of this
checker, BREAKS read from the pull request's as data (`--breaks-from`), so
neither a baseline written again past a break nor a checker made to see
less lets one through.

    python3 tools/openapi_compat.py                      # the committed baseline
    python3 tools/openapi_compat.py --write              # make openapi-golden
    python3 -I main_copy.py --root . --base-ref FETCH_HEAD \\
        --breaks-from tools/openapi_compat.py           # CI: main's checker

The documents are read where fennec-server's `tests/openapi.rs` leaves
them (target/tmp/openapi/, `--built`), the router's beside them.
"""

import argparse
import ast
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
KINDS = ("file", "node", "router")
BASELINE = os.path.join("integrations", "openapi")
BUILT = os.path.join("target", "tmp", "openapi")
METHODS = ("get", "put", "post", "delete", "options", "head", "patch", "trace")

# Every break made on purpose, in the order they went out: never removed, so
# the list is the API's history of them. An entry allows its break only
# while the built document's `info.version` is the entry's `version`, and
# that version is past the baseline's: a later break needs a version, and
# an entry, of its own. Each says why, and what a caller does instead; an
# operation removed was deprecated first (RFC 9745's `Deprecation`, RFC
# 8594's `Sunset`: crates/fennec-http/src/openapi.rs's `Deprecated`) unless
# the reason says why it could not wait. A literal: main's checker reads it
# from a pull request's copy as data, never running it (`--breaks-from`).
#
#   {"version": "2", "kind": "file", "operation": "DELETE /{collection}/all",
#    "change": "operation removed",
#    "reason": "deprecated in 1 (sunset 2027-05-08): DELETE /{collection}?all"}
BREAKS = []


# ------------------------------------------------------------------ reading


def deref(node, doc, depth=0):
    """`node` with its `$ref` followed, within `doc`."""
    while isinstance(node, dict) and "$ref" in node and depth < 32:
        ref = node["$ref"]
        if not ref.startswith("#/"):
            return node
        target = doc
        for part in ref[2:].split("/"):
            part = part.replace("~1", "/").replace("~0", "~")
            if not isinstance(target, dict) or part not in target:
                return node
            target = target[part]
        node, depth = target, depth + 1
    return node


def placeholder(segment):
    return segment.startswith("{") and segment.endswith("}")


def normal_path(path):
    """A path with its placeholders unnamed: a placeholder renamed is the
    same route, its rename a break of its own (`renamed`)."""
    return "/".join("{}" if placeholder(p) else p for p in path.split("/"))


def operations(doc):
    """Every operation of `doc`, by (method, path unnamed): its display
    name, the operation, its path item's parameters and its path."""
    out = {}
    for path, item in (doc.get("paths") or {}).items():
        shared = item.get("parameters") or []
        for method, op in item.items():
            if method not in METHODS or not isinstance(op, dict):
                continue
            out[(method, normal_path(path))] = (f"{method.upper()} {path}", op, shared, path)
    return out


def parameters(op, shared, doc):
    """An operation's query, header and cookie parameters by (in, name), its
    own over its path item's; path parameters are `path_parameters`'."""
    out = {}
    for p in list(shared) + list(op.get("parameters") or []):
        p = deref(p, doc)
        where, name = p.get("in"), p.get("name", "")
        if where == "path":
            continue
        # A header's name is not case sensitive.
        out[(where, name.lower() if where == "header" else name)] = p
    return out


def path_parameters(path, op, shared, doc):
    """An operation's path parameters in its placeholders' order, as (name,
    parameter): a client fills them by place, a generated one names them."""
    declared = {}
    for p in list(shared) + list(op.get("parameters") or []):
        p = deref(p, doc)
        if p.get("in") == "path":
            declared[p.get("name", "")] = p
    names = [s[1:-1] for s in path.split("/") if placeholder(s)]
    return [(n, declared.get(n, {})) for n in names]


def flags(op):
    """The flags a server serves `op` under (`x-fennec-flag`), every one
    of them needed."""
    return set(str(op.get("x-fennec-flag") or "").split())


def security(op, doc):
    """The ways an operation takes a caller, each a set of (scheme, the
    scopes it asks): the empty set is a caller with nothing."""
    reqs = op.get("security", doc.get("security", []))
    if not reqs:
        return {frozenset()}
    return {frozenset((name, frozenset(scopes or [])) for name, scopes in r.items()) for r in reqs}


def takes(way, caller):
    """Whether `way` takes a caller presenting `caller`'s schemes and
    scopes: it asks some of them, each for some of its scopes, or none."""
    has = dict(caller)
    return all(name in has and scopes <= has[name] for name, scopes in way)


def schemes_of(way):
    return "+".join(sorted(name for name, _ in way))


def json_type(v):
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, int):
        return "integer"
    if isinstance(v, float):
        return "number"
    if isinstance(v, str):
        return "string"
    if v is None:
        return "null"
    return "array" if isinstance(v, list) else "object"


def types(s):
    """The JSON types `s` takes, `None` for any: a number takes an
    integer."""
    t = s.get("type")
    if t is None:
        if "const" in s:
            t = [json_type(s["const"])]
        elif "enum" in s:
            t = [json_type(v) for v in s["enum"]]
        else:
            return None
    t = set(t) if isinstance(t, list) else {t}
    if "number" in t:
        t.add("integer")
    return t


def named(ts):
    return "any" if ts is None else "|".join(sorted(ts - {"integer"} if "number" in ts else ts))


LOWER = ("minimum", "exclusiveMinimum", "minLength", "minItems", "minProperties")
UPPER = ("maximum", "exclusiveMaximum", "maxLength", "maxItems", "maxProperties")


def anything(s):
    """Whether a subschema (`items`, `additionalProperties`) takes every
    value: absent, `true` or `{}`."""
    return s is None or s is True or s == {}


def meet_types(a, b):
    """The types both `type`s take: a number meeting an integer is one."""
    a = set(a if isinstance(a, list) else [a])
    b = set(b if isinstance(b, list) else [b])
    both = a & b
    if ("number" in a and "integer" in b) or ("integer" in a and "number" in b):
        both.add("integer")
    both = sorted(both)
    return both[0] if len(both) == 1 else both


def meet(a, b):
    """One schema holding a value to both `a` and `b`, as an `allOf` of the
    two does: the properties of each (one both name held to both), the
    required fields together, the tighter bound of each kind, the types
    and values both take, a pattern or a format of each kept."""
    out = dict(a)
    for k, v in b.items():
        if k not in out:
            out[k] = v
            continue
        w = out[k]
        if k == "properties":
            props = dict(w)
            for name, p in v.items():
                props[name] = {"allOf": [props[name], p]} if name in props else p
            out[k] = props
        elif k == "required":
            out[k] = sorted(set(w) | set(v))
        elif k in LOWER:
            out[k] = max(w, v)
        elif k in UPPER:
            out[k] = min(w, v)
        elif k == "type":
            out[k] = meet_types(w, v)
        elif k == "enum":
            out[k] = [x for x in w if x in v]
        elif k in ("items", "additionalProperties"):
            if w is False or v is False:
                out[k] = False
            elif anything(w):
                out[k] = v
            elif not anything(v):
                out[k] = {"allOf": [w, v]}
        elif k == "uniqueItems":
            out[k] = bool(w or v)
        elif k in ("pattern", "format", "multipleOf") and w != v:
            # Each must hold; one value cannot say both, so both are kept,
            # which differs from what either side had alone.
            out[k] = (w if isinstance(w, list) else [w]) + [v]
    return out


def merged(s, doc, depth=0):
    """`s` with its `allOf` laid into one schema, every part met
    (`meet`)."""
    s = deref(s, doc)
    if not isinstance(s, dict) or "allOf" not in s or depth > 24:
        return s
    out = {k: v for k, v in s.items() if k != "allOf"}
    for part in s["allOf"]:
        part = merged(part, doc, depth + 1)
        if isinstance(part, dict):
            out = meet(out, part)
    return out


def alternatives(s, doc):
    """The schemas `s` is one of (`oneOf`, `anyOf`), else `s` alone."""
    s = merged(s, doc)
    alts = s.get("oneOf") or s.get("anyOf") if isinstance(s, dict) else None
    if not alts:
        return [s]
    out = []
    for a in alts:
        out.extend(alternatives(a, doc))
    return out


# ------------------------------------------------------------ what narrows

# A format the new side takes everything the old one's did under.
WIDER_FORMATS = {("int32", "int64"), ("float", "double")}
# What a default left out of a schema is: nothing, never `None`, which is
# JSON's null.
UNSET = object()


def narrowed(old, new, od, nd, where, depth=0):
    """What a request `new` refuses that `old` took, as (where, change,
    detail): a request schema's narrowing, field by field."""
    if depth > 24:
        return []
    olds, news = alternatives(old, od), alternatives(new, nd)
    if len(olds) > 1 or len(news) > 1:
        out = []
        for o in olds:
            # An old alternative still taken whole by some new one is no
            # break; otherwise the closest by type says what narrowed.
            tried = [narrowed(o, n, od, nd, where, depth + 1) for n in news]
            if any(not t for t in tried):
                continue
            ot = types(o)
            near = [t for n, t in zip(news, tried) if ot is None or types(n) is None or ot & types(n)]
            if near:
                out.extend(min(near, key=len))
            else:
                out.append((where, "narrowed", f"a {named(ot)} is no longer taken"))
        return out
    old, new = olds[0], news[0]
    if not isinstance(old, dict) or not isinstance(new, dict):
        return []
    out = []
    ot, nt = types(old), types(new)
    if nt is not None and (ot is None or not ot <= nt):
        out.append((where, "narrowed", f"type {named(ot)} -> {named(nt)}"))
    oe = old.get("enum", [old["const"]] if "const" in old else None)
    ne = new.get("enum", [new["const"]] if "const" in new else None)
    if ne is not None:
        gone = [v for v in oe if v not in ne] if oe is not None else None
        if oe is None or gone:
            what = "a value list where any was taken" if oe is None else f"{gone} no longer taken"
            out.append((where, "narrowed", what))
    for k in LOWER:
        if k in new and (k not in old or new[k] > old[k]):
            out.append((where, "narrowed", f"{k} {old.get(k, 'none')} -> {new[k]}"))
    for k in UPPER:
        if k in new and (k not in old or new[k] < old[k]):
            out.append((where, "narrowed", f"{k} {old.get(k, 'none')} -> {new[k]}"))
    for k in ("pattern", "format", "multipleOf"):
        if k not in new or new[k] == old.get(k):
            continue
        was, now = old.get(k, "none"), new[k]
        if k == "format" and (was, now) in WIDER_FORMATS:
            continue
        numbers = all(isinstance(x, (int, float)) and not isinstance(x, bool) for x in (was, now))
        if k == "multipleOf" and numbers and now and (was / now) == int(was / now):
            continue
        detail = f"{k} {was!r} -> {now!r}"
        if k == "pattern":
            detail += ": whether it takes less cannot be told, so one loosened is named in BREAKS too"
        out.append((where, "narrowed", detail))
    if new.get("uniqueItems") and not old.get("uniqueItems"):
        out.append((where, "narrowed", "items must be unique"))
    if "default" in old and old["default"] != new.get("default", UNSET):
        now = "none" if "default" not in new else json.dumps(new["default"])
        out.append((where, "default changed", f"{json.dumps(old['default'])} -> {now}: a request leaving it out means another thing"))
    # An object's fields: one newly required, one gone, one narrowed.
    oprops, nprops = old.get("properties") or {}, new.get("properties") or {}
    oreq, nreq = set(old.get("required") or []), set(new.get("required") or [])
    for f in sorted(nreq - oreq):
        what = "a required field added" if f not in oprops else "an optional field became required"
        out.append((f"{where}.{f}", "required", what))
    for f, s in oprops.items():
        if f not in nprops:
            out.append((f"{where}.{f}", "removed", "a field the request took"))
        else:
            out.extend(narrowed(s, nprops[f], od, nd, f"{where}.{f}", depth + 1))
    # A field beyond those listed: refused, or held to a schema where any
    # was taken.
    oa, na = old.get("additionalProperties"), new.get("additionalProperties")
    if na is False and oa is not False:
        out.append((where, "narrowed", "no field beyond those listed is taken"))
    elif not anything(na) and oa is not False:
        out.extend(narrowed({} if anything(oa) else oa, na, od, nd, f"{where}.*", depth + 1))
    if "items" in new and new["items"] is not True:
        out.extend(narrowed(old.get("items", {}), new["items"], od, nd, f"{where}[]", depth + 1))
    return out


def fields(s, doc, at="", out=None, required=None, depth=0):
    """Every field an answer of schema `s` may hold, by its path (`` the
    body, `.rows[].id` within it), with the types it may have: a oneOf's
    alternatives together. `required`, where given, gathers the paths an
    object lists as `required`: where its parent is, it is."""
    out = {} if out is None else out
    if depth > 24:
        return out
    s = merged(s, doc)
    if not isinstance(s, dict):
        return out
    for alt in s.get("oneOf") or s.get("anyOf") or []:
        fields(alt, doc, at, out, required, depth + 1)
    t = types(s)
    have = out.setdefault(at, set())
    if have is not None:
        if t is None and not (s.get("oneOf") or s.get("anyOf")):
            out[at] = None
        elif t is not None:
            have |= t
    if required is not None:
        required.update(f"{at}.{name}" for name in s.get("required") or [])
    for name, p in (s.get("properties") or {}).items():
        fields(p, doc, f"{at}.{name}", out, required, depth + 1)
    if isinstance(s.get("items"), dict):
        fields(s["items"], doc, f"{at}[]", out, required, depth + 1)
    if isinstance(s.get("additionalProperties"), dict):
        fields(s["additionalProperties"], doc, f"{at}.*", out, required, depth + 1)
    return out


def shown(path):
    return path.lstrip(".") or "(the body)"


def answer_breaks(what, old, new, od, nd):
    """What an answer of schema `new` no longer holds that `old` did, as
    (change, detail): `what` names it (`response 200 field`, `response
    503 header Retry-After`)."""
    out = []
    oreq, nreq = set(), set()
    was, now = fields(old, od, required=oreq), fields(new, nd, required=nreq)
    for at, ts in was.items():
        if at not in now:
            out.append((f"{what} {shown(at)} removed", "a field the answer held"))
        elif ts is not None and ts and (now[at] is None or not now[at] <= ts):
            out.append((f"{what} {shown(at)} type changed", f"{named(ts)} -> {named(now[at])}"))
    for at in sorted(oreq - nreq):
        if at in now:
            out.append((f"{what} {shown(at)} no longer required", "an answer may leave it out"))
    return out


def answering(status, responses):
    """The new answers `status` is among: its own, else (a code) those of
    its range -- `2XX`, or a code of it where the old was a range."""
    if status in responses:
        return [responses[status]]
    upper = {k.upper(): v for k, v in responses.items()}
    if len(status) == 3 and status[0] in "12345":
        if status.upper().endswith("XX"):
            return [v for k, v in responses.items() if k[0] == status[0] and k.isdigit()]
        if f"{status[0]}XX" in upper:
            return [upper[f"{status[0]}XX"]]
    return []


# ------------------------------------------------------------ what breaks


def breaks(old, new):
    """Every break from document `old` to `new`, as (operation, change,
    detail): `operation` and `change` are what a BREAKS entry names."""
    out = []
    olds, news = operations(old), operations(new)
    by_id = {op.get("operationId"): name for name, op, _, _ in news.values()}
    for key, (name, op, shared, path) in olds.items():
        if key not in news:
            moved = by_id.get(op.get("operationId"))
            detail = f"moved to {moved}" if moved else "no operation at its method and path"
            if op.get("deprecated"):
                sunset = op.get("x-fennec-sunset")
                detail += f"; deprecated{', sunset ' + sunset if sunset else ', no sunset given'}"
            else:
                detail += "; never deprecated: deprecate it first (Deprecation, Sunset)"
            out.append((name, "operation removed", detail))
            continue
        _, nop, nshared, npath = news[key]
        found = operation_breaks((path, op, shared), (npath, nop, nshared), old, new)
        out.extend((name, c, d) for c, d in found)
    out.extend(scheme_breaks(old, new))
    return out


def parameter_breaks(label, p, q, od, nd):
    """What parameter `q` refuses that `p` took, as (change, detail)."""
    out = []
    if q.get("required") and not p.get("required"):
        out.append((f"{label} required", "an optional parameter became required"))
    for k in ("style", "explode"):
        if k in q and q.get(k) != p.get(k):
            out.append((f"{label} narrowed", f"{k} {p.get(k)} -> {q[k]}"))
    if p.get("allowEmptyValue") and not q.get("allowEmptyValue"):
        out.append((f"{label} narrowed", "an empty value is no longer taken"))
    for at, change, what in narrowed(p.get("schema", {}), q.get("schema", {}), od, nd, ""):
        out.append((f"{label}{at and ' ' + shown(at)} {change}", what))
    return out


def operation_breaks(was, now, od, nd):
    (path, op, shared), (npath, nop, nshared) = was, now
    out = []
    # What a generated client names: its method by the id, its arguments
    # by the placeholders.
    oid, nid = op.get("operationId"), nop.get("operationId")
    if oid and oid != nid:
        out.append(("operationId renamed", f"{oid} -> {nid or 'none'}: a generated client's method is named by it"))
    # Who serves it: a flag more is a server started without it answering
    # 404, which is the operation removed for every server but those.
    gained = sorted(flags(nop) - flags(op))
    if gained:
        before = " ".join(sorted(flags(op))) or "every server"
        out.append(
            ("flag added", f"served under {before}, now only under {' '.join(sorted(flags(nop)))}: one started without {' '.join(gained)} answers 404")
        )
    # Path parameters, by place.
    for (oname, p), (nname, q) in zip(path_parameters(path, op, shared, od), path_parameters(npath, nop, nshared, nd)):
        label = f"parameter path {oname}"
        if oname != nname:
            out.append((f"{label} renamed", f"{{{oname}}} -> {{{nname}}}: a generated client's argument is named by it"))
        out.extend(parameter_breaks(label, p, q, od, nd))
    # Query, header and cookie parameters, by name.
    ops, nps = parameters(op, shared, od), parameters(nop, nshared, nd)
    for (where, pname), p in ops.items():
        label = f"parameter {where} {p.get('name', pname)}"
        if (where, pname) not in nps:
            out.append((f"{label} removed", "a parameter the operation took"))
            continue
        out.extend(parameter_breaks(label, p, nps[(where, pname)], od, nd))
    for (where, pname), q in nps.items():
        if (where, pname) not in ops and q.get("required"):
            out.append((f"parameter {where} {q.get('name', pname)} required", "a required parameter added"))
    # The request's body.
    ob, nb = deref(op.get("requestBody"), od), deref(nop.get("requestBody"), nd)
    if ob is None and nb is not None and nb.get("required"):
        out.append(("request body required", "a body where none was asked"))
    elif ob is not None and nb is None:
        out.append(("request body removed", "the operation takes a body no longer"))
    elif ob is not None:
        if nb.get("required") and not ob.get("required"):
            out.append(("request body required", "an optional body became required"))
        ncontent = nb.get("content") or {}
        for media, m in (ob.get("content") or {}).items():
            if media not in ncontent:
                out.append((f"request {media} removed", "a media type the body took"))
                continue
            for at, change, what in narrowed(m.get("schema", {}), ncontent[media].get("schema", {}), od, nd, ""):
                where = f"request field {shown(at)}" if at else "request body"
                out.append((f"{where} {change}", what))
    # The answers.
    oresp, nresp = op.get("responses") or {}, nop.get("responses") or {}
    for status, r in oresp.items():
        ns = answering(status, nresp)
        if not ns:
            out.append((f"response {status} removed", "a documented answer"))
            continue
        r = deref(r, od)
        for n in ns:
            out.extend(response_breaks(status, r, deref(n, nd), od, nd))
    # Who it takes: a caller presenting an old way's schemes and scopes is
    # still taken where a new way asks some of them, or none.
    was_ways, now_ways = security(op, od), security(nop, nd)
    for way in sorted((w for w in was_ways if not any(takes(n, w) for n in now_ways)), key=sorted):
        asks = " or ".join(schemes_of(w) or "nothing" for w in sorted(now_ways, key=sorted))
        if not way:
            out.append(("security added", f"it took a caller with no token; now it asks {asks}"))
        elif any({n for n, _ in w} == {n for n, _ in way} for w in now_ways):
            out.append((f"security {schemes_of(way)} scopes added", "a way it took now asks more scopes"))
        else:
            out.append((f"security {schemes_of(way)} removed", "a way of authenticating it took"))
    # A response's duplicates (one status against a range's several) said once.
    seen, kept = set(), []
    for item in out:
        if item not in seen:
            seen.add(item)
            kept.append(item)
    return kept


def response_breaks(status, r, n, od, nd):
    out = []
    nheaders = {h.lower(): deref(v, nd) for h, v in (n.get("headers") or {}).items()}
    for h, hv in (r.get("headers") or {}).items():
        hv, hn = deref(hv, od), nheaders.get(h.lower())
        if hn is None:
            out.append((f"response {status} header {h} removed", "a header the answer carried"))
            continue
        if hv.get("required") and not hn.get("required"):
            out.append((f"response {status} header {h} no longer required", "an answer may leave it out"))
        for change, detail in answer_breaks(f"response {status} header {h}", hv.get("schema", {}), hn.get("schema", {}), od, nd):
            # A header is its value: no path within it to name.
            out.append((change.replace(" (the body)", ""), detail))
    ncontent = n.get("content") or {}
    for media, m in (r.get("content") or {}).items():
        if media not in ncontent:
            out.append((f"response {status} {media} removed", "a media type the answer had"))
            continue
        out.extend(answer_breaks(f"response {status} field", m.get("schema", {}), ncontent[media].get("schema", {}), od, nd))
    return out


def scheme_breaks(od, nd):
    """A security scheme an operation of `od` uses, defined otherwise in
    `nd`: a client authenticates by its definition, not its name. One
    entry a scheme (`securitySchemes <name>`), whichever operations ask
    it."""
    out = []
    used = {name for _, op, _, _ in operations(od).values() for way in security(op, od) for name, _ in way}
    old = (od.get("components") or {}).get("securitySchemes") or {}
    new = (nd.get("components") or {}).get("securitySchemes") or {}
    for name in sorted(used & set(old) & set(new)):
        s, n = deref(old[name], od), deref(new[name], nd)
        what = f"securitySchemes {name}"
        for k in ("type", "scheme", "in", "name", "openIdConnectUrl"):
            a, b = s.get(k), n.get(k)
            # An HTTP authentication scheme's name is not case sensitive.
            if k == "scheme" and isinstance(a, str) and isinstance(b, str):
                a, b = a.lower(), b.lower()
            if a != b:
                out.append((what, "definition changed", f"{k} {s.get(k)!r} -> {n.get(k)!r}"))
        nflows = n.get("flows") or {}
        for flow, f in (s.get("flows") or {}).items():
            g = nflows.get(flow)
            if g is None:
                out.append((what, "definition changed", f"flow {flow} removed"))
                continue
            for k in ("authorizationUrl", "tokenUrl", "refreshUrl"):
                if f.get(k) != g.get(k):
                    out.append((what, "definition changed", f"flow {flow} {k} {f.get(k)!r} -> {g.get(k)!r}"))
            gone = sorted(set(f.get("scopes") or {}) - set(g.get("scopes") or {}))
            if gone:
                out.append((what, "definition changed", f"flow {flow} scopes {gone} removed"))
    return out


# ------------------------------------------------------------ the gate


def version(doc):
    return str((doc.get("info") or {}).get("version", ""))


def whole(v):
    """A version as its whole number, `None` where it is none."""
    return int(v) if v.isdigit() else None


def past(nv, ov):
    """Whether version `nv` comes after `ov`: whole numbers, and greater."""
    return whole(nv) is not None and whole(ov) is not None and whole(nv) > whole(ov)


def next_version(*vs):
    nums = [whole(v) for v in vs if whole(v) is not None]
    return str(max(nums) + 1) if nums else "2"


def judge(kind, old, new, allowed):
    """The breaks from `old` to `new` of document `kind`, split: those an
    entry of `allowed` names (with its reason), and the rest (with what an
    entry would be, or why the one there does not apply)."""
    passed, failed = [], []
    ov, nv = version(old), version(new)
    for operation, change, detail in breaks(old, new):
        named_by = [
            e for e in allowed if e["kind"] == kind and e["operation"] == operation and e["change"] == change
        ]
        current = [e for e in named_by if e["version"] == nv]
        if current and past(nv, ov):
            passed.append((operation, change, detail, current[0]["reason"]))
            continue
        if current:
            why = (
                f"BREAKS names it for version {nv}, which is not past the baseline's {ov}: a break on "
                f"purpose moves DOCUMENT_VERSION (crates/fennec-http/src/openapi.rs) forward, to "
                f"{next_version(ov, nv)}, a whole number"
            )
        else:
            entry = {
                "version": nv if past(nv, ov) else next_version(ov, nv),
                "kind": kind,
                "operation": operation,
                "change": change,
                "reason": "<why, and what a caller does instead>",
            }
            why = (
                "if on purpose: deprecate it first where it is an operation, move DOCUMENT_VERSION, "
                f"and name it in BREAKS (tools/openapi_compat.py):\n      {json.dumps(entry)}"
            )
        failed.append((operation, change, detail, why))
    return passed, failed


def backwards(old, new):
    """What is wrong with `new`'s version against `old`'s, if anything: a
    version is a whole number, and never goes back, or a BREAKS entry of
    a version past would allow its break again."""
    ov, nv = version(old), version(new)
    if whole(nv) is None:
        return f"info.version {nv!r} is not a whole number: DOCUMENT_VERSION counts breaks"
    if whole(ov) is not None and whole(nv) < whole(ov):
        return f"info.version went back from {ov} to {nv}: DOCUMENT_VERSION only moves forward"
    return None


def well_formed(entries):
    """What is wrong with BREAKS's entries, if anything."""
    wrong = []
    for i, e in enumerate(entries):
        keys = {"version", "kind", "operation", "change", "reason"}
        if not isinstance(e, dict) or set(e) != keys:
            wrong.append(f"BREAKS[{i}]: keys {sorted(e) if isinstance(e, dict) else e!r}, not {sorted(keys)}")
        elif e["kind"] not in KINDS:
            wrong.append(f"BREAKS[{i}]: kind {e['kind']!r} is none of {KINDS}")
        elif not isinstance(e["version"], str) or whole(e["version"]) is None:
            wrong.append(f"BREAKS[{i}]: version {e['version']!r} is not a whole number's text")
        elif not e["reason"].strip() or e["reason"].startswith("<"):
            wrong.append(f"BREAKS[{i}]: no reason given")
    return wrong


def added(old, new):
    """The operations `new` lists that `old` does not."""
    olds = operations(old)
    return [name for key, (name, _, _, _) in operations(new).items() if key not in olds]


# ------------------------------------------------------------ running


def read(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def breaks_in(path):
    """BREAKS as the checker at `path` writes it, read as data -- a literal,
    never run: what main's copy holds a pull request's breaks by."""
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read(), path)
    for node in tree.body:
        targets = node.targets if isinstance(node, ast.Assign) else [getattr(node, "target", None)]
        if any(isinstance(t, ast.Name) and t.id == "BREAKS" for t in targets):
            return ast.literal_eval(node.value)
    raise ValueError(f"{path} assigns no BREAKS")


def at_ref(ref, path, root):
    """The JSON file `path` at git `ref`, or None where there is none."""
    p = subprocess.run(
        ["git", "show", f"{ref}:{path}"], cwd=root, capture_output=True, text=True
    )
    return json.loads(p.stdout) if p.returncode == 0 else None


def written(doc):
    """A baseline as committed: indented, in the document's own order, so a
    pull request's diff shows which operation moved."""
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def check(built, baseline, base=None, allowed=BREAKS, out=print):
    """Holds each built document to its baseline, and to `base`'s where
    given (kind -> document, or None where that ref had none). Returns the
    number of failures, having said each."""
    failures = 0
    for line in well_formed(allowed):
        out(line)
        failures += 1
    for kind in KINDS:
        new = built[kind]
        against = [("the committed baseline", baseline.get(kind))]
        if base is not None:
            against.append(("the base branch's baseline", base.get(kind)))
        # A break the committed baseline shows is told once, against it:
        # one only the base's shows is one the committed baseline was
        # written again past.
        told = set()
        for what, old in against:
            if old is None:
                out(f"{kind}: no document in {what}: nothing to hold it to")
                continue
            wrong = backwards(old, new)
            if wrong and wrong not in told:
                told.add(wrong)
                failures += 1
                out(f"{kind}: against {what}: {wrong}")
            passed, failed = judge(kind, old, new, allowed)
            for operation, change, detail, reason in passed:
                if (operation, change) not in told:
                    told.add((operation, change))
                    out(f"{kind}: {operation}: {change} ({detail}): allowed by BREAKS: {reason}")
            for operation, change, detail, why in failed:
                if (operation, change) in told:
                    continue
                told.add((operation, change))
                failures += 1
                head = f"BREAKING against {what} (integrations/openapi/{kind}.json)"
                if what != "the committed baseline":
                    head += (
                        ": the committed baseline was written again past this break; a break on "
                        "purpose is named in BREAKS, never by writing the baseline alone"
                    )
                out(f"{head}\n  {kind}: {operation}: {change} ({detail})\n    {why}")
        old = baseline.get(kind)
        if old is not None and old != new and not judge(kind, old, new, allowed)[1]:
            failures += 1
            grew = added(old, new)
            more = f": {', '.join(grew)} added" if grew else " with nothing broken"
            out(
                f"{kind}: the document changed{more}, and integrations/openapi/{kind}.json was not "
                "written again: run make openapi-golden and commit it"
            )
    return failures


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", default=ROOT, help="the checkout: where the defaults below are, and git runs")
    ap.add_argument("--built", help="the served documents (<root>/target/tmp/openapi)")
    ap.add_argument("--baseline", help="the committed baseline (<root>/integrations/openapi)")
    ap.add_argument("--base-ref", help="also hold them to the baseline at this git ref (CI: main)")
    ap.add_argument(
        "--breaks-from",
        help="BREAKS read as data from this checker, not this one's own (CI: main's copy run with the "
        "pull request's breaks)",
    )
    ap.add_argument("--write", action="store_true", help="write the baseline again, refusing a break BREAKS names not")
    a = ap.parse_args(argv)
    root = os.path.abspath(a.root)
    built_dir = a.built or os.path.join(root, BUILT)
    baseline_dir = a.baseline or os.path.join(root, BASELINE)
    allowed = BREAKS
    if a.breaks_from:
        try:
            allowed = breaks_in(a.breaks_from)
        except (OSError, SyntaxError, ValueError) as e:
            print(f"--breaks-from {a.breaks_from}: {e}", file=sys.stderr)
            return 2
    built = {}
    for kind in KINDS:
        path = os.path.join(built_dir, f"{kind}.json")
        if not os.path.exists(path):
            print(
                f"{path} is missing: `cargo test -p fennec-server --test all openapi::` writes it "
                "(make openapi-check does)",
                file=sys.stderr,
            )
            return 2
        built[kind] = read(path)
    baseline = {}
    for kind in KINDS:
        path = os.path.join(baseline_dir, f"{kind}.json")
        baseline[kind] = read(path) if os.path.exists(path) else None
    if a.write:
        failed = 0
        for kind in KINDS:
            if baseline[kind] is not None:
                wrong = backwards(baseline[kind], built[kind])
                if wrong:
                    failed += 1
                    print(f"{kind}: {wrong}")
                for operation, change, detail, why in judge(kind, baseline[kind], built[kind], allowed)[1]:
                    failed += 1
                    print(f"BREAKING: {kind}: {operation}: {change} ({detail})\n    {why}")
        if failed:
            print(f"{failed} failure(s): the baseline is left as it was")
            return 1
        os.makedirs(baseline_dir, exist_ok=True)
        for kind in KINDS:
            text = written(built[kind])
            path = os.path.join(baseline_dir, f"{kind}.json")
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)
            shown_path = os.path.relpath(path, root) if path.startswith(root + os.sep) else path
            print(f"wrote {shown_path}: {len(operations(built[kind]))} operations")
        return 0
    base = None
    if a.base_ref:
        # A ref that names no commit is a mistake, never a base with no
        # baseline (which the first pull request with one has).
        known = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"{a.base_ref}^{{commit}}"],
            cwd=root,
            capture_output=True,
        )
        if known.returncode != 0:
            print(f"--base-ref {a.base_ref} names no commit (fetch it first)", file=sys.stderr)
            return 2
        base = {kind: at_ref(a.base_ref, f"{BASELINE}/{kind}.json", root) for kind in KINDS}
    failures = check(built, baseline, base, allowed)
    if failures:
        print(f"\n{failures} failure(s): docs/design/http.md#compatibility says what may change and how")
        return 1
    print(f"the REST API only grew: {', '.join(f'{k} {len(operations(built[k]))} operations' for k in KINDS)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
