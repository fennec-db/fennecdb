#!/usr/bin/env python3
"""The browser module's size with each of the features it may go without,
as it is served: `make wasm-sizes`.

The module is built with every feature (`make wasm`), with none -- the
smallest module there is, what every page loads whatever it declares --
with each feature alone added to none, and with every feature but each,
into target/wasm-sizes. Each is measured raw, under gzip -9, and under
brotli -q 11 when the `brotli` tool is there. A feature's cost is given
both ways, since they differ: what it adds to the smallest module, and
what leaving it out of the full one saves -- code two features share is
counted in each one's first and in neither one's second.

    make wasm-sizes                  # every feature, both ways
    make wasm-sizes ONLY="geo collate"  # those features alone, both ways
    make wasm-sizes JSON=1           # also crates/fennec-wasm/sizes.json, in bytes

sizes.json is what site/build.py holds every size the docs quote of a
feature or of the two ends to, with no drift: a page's figure is this file's
rounded, and the file is regenerated at each release (RELEASING.md), since
CI measures the module only against a pull request's base.

What each feature is, and what a build without it does with a file or a
statement that needs it, is written beside it in fennec-core's Cargo.toml.
"""

import gzip
import json
import os
import shutil
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
HOME_CARGO = os.path.expanduser("~/.cargo/bin/cargo")
CARGO = os.environ.get("CARGO") or (HOME_CARGO if os.path.exists(HOME_CARGO) else "cargo")
TARGET = os.path.join(ROOT, "target", "wasm-sizes")
JSON_OUT = os.path.join(ROOT, "crates", "fennec-wasm", "sizes.json")
OUT = os.path.join(TARGET, "wasm32-unknown-unknown", "wasm", "fennec_wasm.wasm")
# Every feature of fennec-wasm a module may go without, in the order the
# report lists them: the indexes, the rest of the language, the schema check.
FEATURES = ["vector", "text", "sparse", "sorted", "composite", "geo", "nocase", "facets",
            "highlight", "analytics", "collate", "schema"]
# What a feature is measured with when it is added to the smallest module:
# an index over several fields keeps its keys in the ordered index's chunks,
# and the marks are of what `match` found, which needs the text index.
NEEDS = {"composite": ["sorted"], "highlight": ["text"]}
# What Cargo turns on with a feature: left out of the full module, a
# feature takes out each one that requires it.
REQUIRES = {"composite": ["sorted"]}


def brotli(data):
    if not shutil.which("brotli"):
        return None
    done = subprocess.run(["brotli", "-c", "-q", "11"], input=data, capture_output=True, check=True)
    return len(done.stdout)


def kb(n):
    return "--" if n is None else "%.1f" % (n / 1024)


def signed_kb(n):
    return "--" if n is None else "%+.1f" % (n / 1024)


def build(features):
    """`(raw, gzip, brotli)` of the module built with `features`."""
    args = [CARGO, "build", "-q", "-p", "fennec-wasm", "--target", "wasm32-unknown-unknown",
            "--profile", "wasm", "--no-default-features"]
    if features:
        args += ["--features", ",".join(features)]
    subprocess.run(args, check=True, cwd=ROOT, env=dict(os.environ, CARGO_TARGET_DIR=TARGET))
    data = open(OUT, "rb").read()
    return len(data), len(gzip.compress(data, 9, mtime=0)), brotli(data)


def minus(a, b):
    return tuple(None if x is None or y is None else x - y for x, y in zip(a, b))


def sizes(t):
    return {"raw": t[0], "gzip": t[1], "brotli": t[2]}


def main():
    want_json = "--json" in sys.argv[1:]
    only = [f for f in os.environ.get("ONLY", "").split() if f] or FEATURES
    unknown = [f for f in only if f not in FEATURES]
    if unknown:
        sys.exit("unknown features: %s (of %s)" % (" ".join(unknown), " ".join(FEATURES)))
    full = build(FEATURES)
    least = build([])
    record = {"full": sizes(full), "none": sizes(least), "features": {}}
    print("| module | raw KB | gzip KB | brotli KB |")
    print("| --- | ---: | ---: | ---: |")
    print("| every feature (`make wasm`) | %s |" % " | ".join(kb(n) for n in full))
    print("| none (`make wasm FEATURES=none SCHEMA=0`) | %s |" % " | ".join(kb(n) for n in least))
    print()
    print("| feature | added to none: raw KB | brotli KB | left out of every one: raw KB | brotli KB |")
    print("| --- | ---: | ---: | ---: | ---: |")
    for f in only:
        with_needs = NEEDS.get(f, [])
        alone = build(with_needs + [f])
        base = build(with_needs) if with_needs else least
        out = [f] + [g for g, req in REQUIRES.items() if f in req]
        without = build([g for g in FEATURES if g not in out])
        name = f"`{f}`" + (f", beside `{'`, `'.join(with_needs)}`" if with_needs else "")
        name += f" (and `{'`, `'.join(out[1:])}`)" if out[1:] else ""
        added, saved = minus(alone, base), minus(full, without)
        print("| %s | %s | %s | %s | %s |" % (name, signed_kb(added[0]), signed_kb(added[2]),
                                            kb(saved[0]), kb(saved[2])), flush=True)
        record["features"][f] = {"added": sizes(added), "saved": sizes(saved)}
    if want_json:
        # Only a run of every feature may replace the committed file.
        if only != FEATURES:
            sys.exit("--json wants every feature, not ONLY")
        # The code by where it is written, which the docs' table of the
        # module's bytes quotes: the report's own measure, names kept.
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import size_report
        m = size_report.measure(ROOT, os.path.join(TARGET, "names"))
        record["code"] = {"code": m["code"], "data": m["data"], "parts": dict(sorted(m["parts"].items()))}
        with open(JSON_OUT, "w") as out:
            json.dump(record, out, indent=1)
            out.write("\n")


if __name__ == "__main__":
    main()
