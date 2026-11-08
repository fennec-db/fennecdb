#!/usr/bin/env python3
"""The REST API only grows (docs/design/http.md#compatibility).

fennec-server's OpenAPI documents -- a file's server, a tenant node and a
router, as `GET /_openapi.json` serves them -- are held to the baseline
committed in integrations/openapi/. An addition passes, and the same pull
request writes the baseline again (`make openapi-golden`). A change a
client could have relied on fails:

  * an operation removed, or its method or path changed;
  * a parameter, a request body or a body field that became required, or a
    required one added; a parameter or a request field removed;
  * a request type narrowed: a type, an enum, a bound, a pattern, an
    alternative of a oneOf no longer taken;
  * a documented response status removed, a response field, header or
    media type removed, or a response field's type changed;
  * a security requirement added to an operation that took none, or a way
    of authenticating it took no longer taken.

A break made on purpose is named in BREAKS below -- the kind, the
operation, the change, the reason and the document version it went out
in -- and moves the document's version (`DOCUMENT_VERSION` in
crates/fennec-http/src/openapi.rs). The baseline is never the way: CI also
holds the documents to the baseline main has (`--base-ref`), so one
written again past a break fails there, saying so.

    python3 tools/openapi_compat.py                      # make openapi-check
    python3 tools/openapi_compat.py --base-ref FETCH_HEAD  # CI, main fetched
    python3 tools/openapi_compat.py --write              # make openapi-golden

The documents are read where fennec-server's `tests/openapi.rs` leaves
them (target/tmp/openapi/, `--built`), the router's beside them.
"""

import argparse
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
# that version is not the baseline's: a later break needs a version, and
# an entry, of its own. Each says why, and what a caller does instead; an
# operation removed was deprecated first (RFC 9745's `Deprecation`, RFC
# 8594's `Sunset`: crates/fennec-http/src/openapi.rs's `Deprecated`) unless
# the reason says why it could not wait.
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


def normal_path(path):
    """A path with its placeholders unnamed: `{collection}` renamed is the
    same route to a client."""
    return "/".join("{}" if p.startswith("{") and p.endswith("}") else p for p in path.split("/"))


def operations(doc):
    """Every operation of `doc`, by (method, path unnamed): its display
    name, the operation and its path item's parameters."""
    out = {}
    for path, item in (doc.get("paths") or {}).items():
        shared = item.get("parameters") or []
        for method, op in item.items():
            if method not in METHODS or not isinstance(op, dict):
                continue
            out[(method, normal_path(path))] = (f"{method.upper()} {path}", op, shared)
    return out


def parameters(op, shared, doc):
    """An operation's query, header and cookie parameters by (in, name), its
    own over its path item's; path parameters are its path's."""
    out = {}
    for p in list(shared) + list(op.get("parameters") or []):
        p = deref(p, doc)
        where, name = p.get("in"), p.get("name", "")
        if where == "path":
            continue
        # A header's name is not case sensitive.
        out[(where, name.lower() if where == "header" else name)] = p
    return out


def security(op, doc):
    """The ways an operation takes a caller, each a set of scheme names:
    the empty set is a caller with nothing."""
    reqs = op.get("security", doc.get("security", []))
    if not reqs:
        return {frozenset()}
    return {frozenset(r.keys()) for r in reqs}


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


def merged(s, doc):
    """`s` with its `allOf` laid into one schema: the parts' properties,
    required fields and bounds together."""
    s = deref(s, doc)
    if not isinstance(s, dict) or "allOf" not in s:
        return s
    out = {k: v for k, v in s.items() if k != "allOf"}
    for part in s["allOf"]:
        part = merged(part, doc)
        for k, v in part.items():
            if k == "properties":
                out.setdefault("properties", {}).update(v)
            elif k == "required":
                out["required"] = sorted(set(out.get("required", [])) | set(v))
            else:
                out.setdefault(k, v)
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

LOWER = ("minimum", "exclusiveMinimum", "minLength", "minItems", "minProperties")
UPPER = ("maximum", "exclusiveMaximum", "maxLength", "maxItems", "maxProperties")


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
        if k in new and new.get(k) != old.get(k):
            out.append((where, "narrowed", f"{k} {old.get(k, 'none')!r} -> {new[k]!r}"))
    if new.get("uniqueItems") and not old.get("uniqueItems"):
        out.append((where, "narrowed", "items must be unique"))
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
    oa, na = old.get("additionalProperties", True), new.get("additionalProperties", True)
    if na is False and oa is not False:
        out.append((where, "narrowed", "no field beyond those listed is taken"))
    elif isinstance(oa, dict) and isinstance(na, dict):
        out.extend(narrowed(oa, na, od, nd, f"{where}.*", depth + 1))
    if "items" in old and "items" in new:
        out.extend(narrowed(old["items"], new["items"], od, nd, f"{where}[]", depth + 1))
    elif "items" in new and "items" not in old:
        out.extend(narrowed({}, new["items"], od, nd, f"{where}[]", depth + 1))
    return out


def fields(s, doc, at="", out=None, depth=0):
    """Every field an answer of schema `s` may hold, by its path (`` the
    body, `.rows[].id` within it), with the types it may have: a oneOf's
    alternatives together."""
    out = {} if out is None else out
    if depth > 24:
        return out
    s = merged(s, doc)
    if not isinstance(s, dict):
        return out
    for alt in s.get("oneOf") or s.get("anyOf") or []:
        fields(alt, doc, at, out, depth + 1)
    t = types(s)
    have = out.setdefault(at, set())
    if have is not None:
        if t is None and not (s.get("oneOf") or s.get("anyOf")):
            out[at] = None
        elif t is not None:
            have |= t
    for name, p in (s.get("properties") or {}).items():
        fields(p, doc, f"{at}.{name}", out, depth + 1)
    if isinstance(s.get("items"), dict):
        fields(s["items"], doc, f"{at}[]", out, depth + 1)
    if isinstance(s.get("additionalProperties"), dict):
        fields(s["additionalProperties"], doc, f"{at}.*", out, depth + 1)
    return out


def shown(path):
    return path.lstrip(".") or "(the body)"


# ------------------------------------------------------------ what breaks


def breaks(old, new):
    """Every break from document `old` to `new`, as (operation, change,
    detail): `operation` and `change` are what a BREAKS entry names."""
    out = []
    olds, news = operations(old), operations(new)
    by_id = {op.get("operationId"): name for name, op, _ in news.values()}
    for key, (name, op, shared) in olds.items():
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
        _, nop, nshared = news[key]
        out.extend((name, c, d) for c, d in operation_breaks(op, shared, nop, nshared, old, new))
    return out


def operation_breaks(op, shared, nop, nshared, od, nd):
    out = []
    # Parameters.
    ops, nps = parameters(op, shared, od), parameters(nop, nshared, nd)
    for (where, pname), p in ops.items():
        label = f"parameter {where} {p.get('name', pname)}"
        if (where, pname) not in nps:
            out.append((f"{label} removed", "a parameter the operation took"))
            continue
        q = nps[(where, pname)]
        if q.get("required") and not p.get("required"):
            out.append((f"{label} required", "an optional parameter became required"))
        for k in ("style", "explode"):
            if k in q and q.get(k) != p.get(k):
                out.append((f"{label} narrowed", f"{k} {p.get(k)} -> {q[k]}"))
        if p.get("allowEmptyValue") and not q.get("allowEmptyValue"):
            out.append((f"{label} narrowed", "an empty value is no longer taken"))
        for at, change, what in narrowed(p.get("schema", {}), q.get("schema", {}), od, nd, ""):
            out.append((f"{label}{at and ' ' + shown(at)} {change}", what))
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
        if status not in nresp:
            out.append((f"response {status} removed", "a documented answer"))
            continue
        r, n = deref(r, od), deref(nresp[status], nd)
        nheaders = {h.lower() for h in (n.get("headers") or {})}
        for h in r.get("headers") or {}:
            if h.lower() not in nheaders:
                out.append((f"response {status} header {h} removed", "a header the answer carried"))
        ncontent = n.get("content") or {}
        for media, m in (r.get("content") or {}).items():
            if media not in ncontent:
                out.append((f"response {status} {media} removed", "a media type the answer had"))
                continue
            was = fields(m.get("schema", {}), od)
            now = fields(ncontent[media].get("schema", {}), nd)
            for at, ts in was.items():
                if at not in now:
                    out.append((f"response {status} field {shown(at)} removed", "a field the answer held"))
                elif ts is not None and ts and (now[at] is None or not now[at] <= ts):
                    out.append(
                        (f"response {status} field {shown(at)} type changed", f"{named(ts)} -> {named(now[at])}")
                    )
    # Who it takes: a caller presenting an old way's schemes is still taken
    # where a new way asks some of them, or none.
    was, now = security(op, od), security(nop, nd)
    for way in sorted((w for w in was if not any(n <= w for n in now)), key=sorted):
        if not way:
            asks = " or ".join("+".join(sorted(w)) for w in sorted(now, key=sorted))
            out.append(("security added", f"it took a caller with no token; now it asks {asks}"))
        else:
            out.append((f"security {'+'.join(sorted(way))} removed", "a way of authenticating it took"))
    return out


# ------------------------------------------------------------ the gate


def version(doc):
    return str((doc.get("info") or {}).get("version", ""))


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
        if current and nv != ov:
            passed.append((operation, change, detail, current[0]["reason"]))
            continue
        if current:
            why = (
                f"BREAKS names it for version {nv}, which is the baseline's too: a break on purpose "
                f"moves DOCUMENT_VERSION (crates/fennec-http/src/openapi.rs) past {ov}"
            )
        else:
            entry = {
                "version": next_version(ov) if nv == ov else nv,
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


def next_version(v):
    return str(int(v) + 1) if v.isdigit() else f"{v}+1"


def well_formed(entries):
    """What is wrong with BREAKS's entries, if anything."""
    wrong = []
    for i, e in enumerate(entries):
        keys = {"version", "kind", "operation", "change", "reason"}
        if set(e) != keys:
            wrong.append(f"BREAKS[{i}]: keys {sorted(e)}, not {sorted(keys)}")
        elif e["kind"] not in KINDS:
            wrong.append(f"BREAKS[{i}]: kind {e['kind']!r} is none of {KINDS}")
        elif not e["reason"].strip() or e["reason"].startswith("<"):
            wrong.append(f"BREAKS[{i}]: no reason given")
    return wrong


def added(old, new):
    """The operations `new` lists that `old` does not."""
    olds = operations(old)
    return [name for key, (name, _, _) in operations(new).items() if key not in olds]


# ------------------------------------------------------------ running


def read(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


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
    ap.add_argument("--built", default=os.path.join(ROOT, BUILT), help="the served documents (target/tmp/openapi)")
    ap.add_argument("--baseline", default=os.path.join(ROOT, BASELINE), help="integrations/openapi")
    ap.add_argument("--base-ref", help="also hold them to the baseline at this git ref (CI: main)")
    ap.add_argument("--write", action="store_true", help="write the baseline again, refusing a break BREAKS names not")
    a = ap.parse_args(argv)
    built = {}
    for kind in KINDS:
        path = os.path.join(a.built, f"{kind}.json")
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
        path = os.path.join(a.baseline, f"{kind}.json")
        baseline[kind] = read(path) if os.path.exists(path) else None
    if a.write:
        failed = 0
        for kind in KINDS:
            if baseline[kind] is not None:
                for operation, change, detail, why in judge(kind, baseline[kind], built[kind], BREAKS)[1]:
                    failed += 1
                    print(f"BREAKING: {kind}: {operation}: {change} ({detail})\n    {why}")
        if failed:
            print(f"{failed} break(s) BREAKS does not name: the baseline is left as it was")
            return 1
        os.makedirs(a.baseline, exist_ok=True)
        for kind in KINDS:
            text = written(built[kind])
            path = os.path.join(a.baseline, f"{kind}.json")
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)
            shown_path = os.path.relpath(path, ROOT) if path.startswith(ROOT + os.sep) else path
            print(f"wrote {shown_path}: {len(operations(built[kind]))} operations")
        return 0
    base = None
    if a.base_ref:
        # A ref that names no commit is a mistake, never a base with no
        # baseline (which the first pull request with one has).
        known = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"{a.base_ref}^{{commit}}"],
            cwd=ROOT,
            capture_output=True,
        )
        if known.returncode != 0:
            print(f"--base-ref {a.base_ref} names no commit (fetch it first)", file=sys.stderr)
            return 2
        base = {kind: at_ref(a.base_ref, f"{BASELINE}/{kind}.json", ROOT) for kind in KINDS}
    failures = check(built, baseline, base)
    if failures:
        print(f"\n{failures} failure(s): docs/design/http.md#compatibility says what may change and how")
        return 1
    print(f"the REST API only grew: {', '.join(f'{k} {len(operations(built[k]))} operations' for k in KINDS)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
