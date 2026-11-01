#!/usr/bin/env python3
"""The zero-dependency rule, held: `python3 tools/deps_check.py`.

docs/design/crates.md#zero-dependencies. Every crate of the workspace but
fennec-tls and fennec-bench names only crates of the workspace, as a
dependency of any kind (normal, build or dev). fennec-tls names rustls and
webpki-roots, and rcgen to test with, and nothing else: TLS is the one door,
and nothing else comes in by it. What comes in by it is pinned whole, every
crate of its tree by name and version (TLS_TREE), so a rustls feature turned
on or a version moved is a review, never a silent addition. A crate taking
fennec-tls takes it optional, behind a feature named `tls`, so a build
without its default features takes nothing from outside the workspace. And
the browser module and the crates beneath it -- fennec-core, fennec-ql,
fennec-abi, fennec-wasm -- and the native library apps embed, fennec-ffi,
reach nothing from outside the workspace, and not fennec-tls, with every
feature of theirs on.

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
CLOSED = ("fennec-core", "fennec-ql", "fennec-abi", "fennec-wasm", "fennec-ffi")

# fennec-tls's whole tree, normal and build edges, as Cargo.lock resolves it
# for the targets the binaries ship for (TLS_TARGETS; Windows takes the same
# less libc). Its Cargo.toml names two crates; these are what they bring, and
# a rustls feature turned on (aws-lc-rs, brotli), a default moved or a
# `cargo update` changes them without a line of any Cargo.toml changing. A
# difference fails the check: read what came in or moved (what it is, who
# publishes it, its build.rs), then write the list again. A build script
# here is one tools/image-cargo.sh names by version too.
TLS_TREE = {
    "cc@1.4.6",
    "cfg-if@1.0.5",
    "find-msvc-tools@0.1.12",
    "getrandom@0.2.17",
    "libc@0.2.189",
    "log@0.4.34",
    "once_cell@1.21.4",
    "ring@0.17.14",
    "rustls@0.23.46",
    "rustls-pki-types@1.15.1",
    "rustls-webpki@0.103.15",
    "shlex@2.0.1",
    "subtle@2.6.1",
    "untrusted@0.9.0",
    "webpki-roots@1.0.11",
    "zeroize@1.9.0",
}
TLS_TARGETS = (
    "x86_64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "aarch64-linux-android",
    "aarch64-apple-ios",
)


def metadata(*extra):
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features", *extra],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def tls_tree(meta):
    """The crates from outside the workspace fennec-tls reaches by normal and
    build edges, as name@version."""
    members = set(meta["workspace_members"])
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    start = next(i for i in members if packages[i]["name"] == "fennec-tls")
    seen, todo = {start}, [start]
    while todo:
        for d in nodes[todo.pop()]["deps"]:
            if any(k["kind"] in (None, "build") for k in d["dep_kinds"]) and d["pkg"] not in seen:
                seen.add(d["pkg"])
                todo.append(d["pkg"])
    return {f"{packages[i]['name']}@{packages[i]['version']}" for i in seen if i not in members}


def check_tls(took):
    """Each way fennec-tls's tree differs from TLS_TREE, as a line."""
    errors = []
    pinned = {t.split("@")[0]: t for t in TLS_TREE}
    for t in sorted(took - TLS_TREE):
        name = t.split("@")[0]
        was = f" where TLS_TREE names {pinned[name]}" if name in pinned else ""
        errors.append(
            f"fennec-tls: its tree takes {t}{was}: review it (what it is, who publishes it, "
            "its build.rs), then update TLS_TREE in tools/deps_check.py"
        )
    names = {t.split("@")[0] for t in took}
    for t in sorted(TLS_TREE - took):
        if t.split("@")[0] not in names:
            errors.append(f"fennec-tls: its tree no longer takes {t}: take it off TLS_TREE")
    return errors


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
    took = set()
    for target in TLS_TARGETS:
        took |= tls_tree(metadata("--filter-platform", target))
    errors += check_tls(took)
    for e in errors:
        print(e, file=sys.stderr)
    if errors:
        sys.exit("the zero-dependency rule is broken: docs/design/crates.md#zero-dependencies")
    print("deps_check: only fennec-tls and fennec-bench take dependencies from outside the workspace")


if __name__ == "__main__":
    main()
