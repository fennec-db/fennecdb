#!/usr/bin/env python3
"""The zero-dependency rule, held: `python3 tools/deps_check.py`.

docs/design/crates.md#zero-dependencies. Every crate of the workspace but
fennec-tls and fennec-bench names only crates of the workspace, as a
dependency of any kind (normal, build or dev). fennec-tls names rustls and
webpki-roots, and rcgen to test with, and nothing else: TLS is the one door,
and nothing else comes in by it. A crate taking fennec-tls takes it
optional, behind a feature named `tls`, so a build without its default
features takes nothing from outside the workspace. And the browser module
and the crates beneath it -- fennec-core, fennec-ql, fennec-abi,
fennec-wasm -- reach nothing from outside the workspace, and not
fennec-tls, with every feature of theirs on.

CI's fmt job runs it. `cargo deny` would be a dependency of its own, and
this is the one question asked."""

import json
import subprocess
import sys

# What each exempt crate may name; None, anything (a measurement harness,
# `publish = false`).
EXEMPT = {
    "fennec-tls": {"rustls", "webpki-roots", "rcgen"},
    "fennec-bench": None,
}
# Never fennec-tls, nor anything from outside the workspace, by any path.
CLOSED = ("fennec-core", "fennec-ql", "fennec-abi", "fennec-wasm")


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def check(meta):
    """Each breach of the rule, as a line."""
    members = {p["id"]: p for p in meta["packages"] if p["id"] in meta["workspace_members"]}
    names = {p["name"] for p in members.values()}
    by_name = {p["name"]: p for p in members.values()}
    errors = []
    for p in sorted(members.values(), key=lambda p: p["name"]):
        allowed = EXEMPT.get(p["name"], set())
        for d in p["dependencies"]:
            kind = d["kind"] or "normal"
            inside = d["source"] is None and d["name"] in names
            if inside:
                if d["name"] == "fennec-tls" and kind == "normal":
                    tls = p["features"].get("tls", [])
                    if not d["optional"] or not any(
                        f in ("dep:fennec-tls", "fennec-tls") for f in tls
                    ):
                        errors.append(
                            f"{p['name']}: takes fennec-tls other than as an optional "
                            "dependency behind a feature named `tls`"
                        )
                continue
            if allowed is None or d["name"] in allowed:
                continue
            errors.append(
                f"{p['name']}: {kind} dependency `{d['name']}` comes from outside the "
                "workspace (only fennec-tls and fennec-bench take any)"
            )
    # The trees of the closed crates, normal and build edges, any target.
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    for root in CLOSED:
        start = by_name[root]["id"]
        seen, todo, outside = {start}, [start], []
        while todo:
            node = nodes[todo.pop()]
            for d in node["deps"]:
                if not any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
                    continue
                if d["pkg"] in seen:
                    continue
                seen.add(d["pkg"])
                todo.append(d["pkg"])
                dep = members.get(d["pkg"])
                if dep is None:
                    outside.append(d["name"])
                elif dep["name"] == "fennec-tls":
                    errors.append(f"{root}: reaches fennec-tls")
        if outside:
            errors.append(f"{root}: reaches from outside the workspace: {', '.join(sorted(outside))}")
    for name in EXEMPT:
        if name not in names:
            errors.append(f"{name}: exempt, and not in the workspace")
    return errors


def main():
    errors = check(metadata())
    for e in errors:
        print(e, file=sys.stderr)
    if errors:
        sys.exit("the zero-dependency rule is broken: docs/design/crates.md#zero-dependencies")
    print("deps_check: only fennec-tls and fennec-bench take dependencies from outside the workspace")


if __name__ == "__main__":
    main()
