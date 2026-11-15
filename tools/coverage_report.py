#!/usr/bin/env python3
"""tools/coverage_report.py: the weekly coverage report's tables, written
from the JSON `llvm-cov export` makes (.github/workflows/coverage.yml).

    python3 -I tools/coverage_report.py EXPORT.json --markdown report.md --summary summary.json
    python3 -I tools/coverage_report.py --check-paths

The figures are information, never a gate: nothing here compares a number to
a threshold, and the tool's exit status says only whether it could read its
input (docs/design/conventions.md#coverage). Standard library only.
"""

import argparse
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The trust boundaries: a module that reads bytes from outside the process.
# `reads` is the one line saying which; `functions`, where a file mixes the
# load path with much else, names the functions whose own row is shown beside
# the file's (the export gives a function's regions and whether it ran, not
# its lines). A path that no longer exists fails the tool, so a rename cannot
# drop a row unseen. A new parser of untrusted bytes is added here, beside
# its fuzz target.
BOUNDARIES = [
    {"path": "crates/fennec-ql/src/parser.rs",
     "reads": "FennecQL statements from any client, the shell and a named statement's stored text"},
    {"path": "crates/fennec-ql/src/lexer.rs",
     "reads": "the characters of a FennecQL statement, before the parser"},
    {"path": "crates/fennec-core/src/json.rs",
     "reads": "request bodies, query parameters and pages of documents as JSON text"},
    {"path": "crates/fennec-core/src/codec.rs",
     "reads": "a value's and a type's encoding in a file's records and a replica's stream"},
    {"path": "crates/fennec-http/src/http.rs",
     "reads": "an HTTP request's head and body, by length or chunked, off a socket"},
    {"path": "crates/fennec-import/src/sqlite.rs",
     "reads": "a SQLite file's pages, as `fennec import` opens it"},
    {"path": "crates/fennec-wire/src/proto.rs",
     "reads": "PostgreSQL protocol messages from a server a mirror or import dials"},
    {"path": "crates/fennec-http/src/crypto.rs",
     "reads": "signatures, keys, MACs and tokens presented by a caller or an issuer"},
    {"path": "crates/fennec-http/src/crypto/p256.rs",
     "reads": "P-256 points and signatures inside a token or a webhook's proof"},
    {"path": "crates/fennec-http/src/seal.rs",
     "reads": "a sealed archive or backup's bytes, authenticated before they are read"},
    # The engine's load path is a file row and, within it, the functions that
    # read a file's or an image's bytes at open: a file-level figure alone
    # would be mostly the engine's write path.
    {"path": "crates/fennec-core/src/store.rs",
     "reads": "a file's records and an image's frames replayed at open",
     "functions": ["replay", "replay_mapped", "adopt_index"]},
    {"path": "crates/fennec-core/src/fs.rs",
     "reads": "the file a database is opened from, mapped or read whole",
     "functions": ["open", "open_with"]},
]

# Never counted: the registry's and the toolchain's sources, and the tests'
# own files, which would only count themselves.
IGNORED = re.compile(r"(/\.cargo/registry/|/\.cargo/git/|/rustc/|/(tests|examples|benches)/)")
CRATE = re.compile(r"(?:^|/)crates/([^/]+)/(.+)$")


def missing_paths(root=ROOT, boundaries=None):
    """The listed paths that are not files under `root`."""
    boundaries = BOUNDARIES if boundaries is None else boundaries
    return [b["path"] for b in boundaries if not os.path.isfile(os.path.join(root, b["path"]))]


def percent(covered, total):
    if not total:
        return "-"
    return "%.1f%%" % (100.0 * covered / total)


def cell(kind):
    return "%s / %s (%s)" % (kind["covered"], kind["count"], percent(kind["covered"], kind["count"]))


def zero():
    return {"count": 0, "covered": 0}


def add(into, kind):
    into["count"] += kind["count"]
    into["covered"] += kind["covered"]


def kinds(summary):
    return {k: {"count": summary[k]["count"], "covered": summary[k]["covered"]}
            for k in ("lines", "regions", "functions")}


def counted(files):
    """(crate, path inside the repository, summary) of each file counted."""
    for f in files:
        name = f["filename"].replace("\\", "/")
        if IGNORED.search(name):
            continue
        m = CRATE.search(name)
        if not m:
            continue
        yield m.group(1), "crates/" + m.group(1) + "/" + m.group(2), f["summary"]


def function_row(functions, path, names):
    """Functions and regions of the named functions of one file. A name is
    found in a mangled symbol by its length and spelling, so `open` is not
    `reopen` nor `open_with`."""
    patterns = [re.compile(r"(?<!\d)%d%s" % (len(n), re.escape(n))) for n in names]
    row = {"functions": zero(), "regions": zero()}
    for fn in functions:
        files = [x.replace("\\", "/") for x in fn.get("filenames", [])]
        if not files or not files[0].endswith(path):
            continue
        if not any(p.search(fn["name"]) for p in patterns):
            continue
        row["functions"]["count"] += 1
        row["functions"]["covered"] += 1 if fn["count"] > 0 else 0
        for r in fn.get("regions", []):
            if r[7] == 0:  # a code region, not a gap or an expansion
                row["regions"]["count"] += 1
                row["regions"]["covered"] += 1 if r[4] > 0 else 0
    return row


def build(export, boundaries=None):
    boundaries = BOUNDARIES if boundaries is None else boundaries
    data = export["data"][0]
    crates, by_path = {}, {}
    for crate, path, summary in counted(data.get("files", [])):
        row = crates.setdefault(crate, {k: zero() for k in ("lines", "regions", "functions")})
        for k, v in kinds(summary).items():
            add(row[k], v)
        by_path[path] = kinds(summary)
    rows = []
    for b in boundaries:
        row = {"path": b["path"], "reads": b["reads"], "file": by_path.get(b["path"]), "functions": None}
        if b.get("functions"):
            row["function_names"] = list(b["functions"])
            row["functions"] = function_row(data.get("functions", []), b["path"], b["functions"])
        rows.append(row)
    return {"crates": dict(sorted(crates.items())), "boundaries": rows}


def render(report):
    out = ["# Coverage report", "",
           "Figures from `cargo test` under `-C instrument-coverage`, published as",
           "information: no threshold is applied anywhere. Counts include a file's own",
           "inline test modules; the tests' files, examples, benches and the registry's",
           "crates are left out.", "",
           "## Crates", "",
           "| Crate | Lines | Regions | Functions |", "| --- | --- | --- | --- |"]
    for name, row in report["crates"].items():
        out.append("| %s | %s | %s | %s |" % (name, cell(row["lines"]), cell(row["regions"]), cell(row["functions"])))
    out += ["", "## Trust boundaries", "",
            "Modules that parse bytes from outside the process, listed in",
            "`tools/coverage_report.py`. A `fn` row is the named functions of the file above it.", "",
            "| Module | Reads | Lines | Regions | Functions |", "| --- | --- | --- | --- | --- |"]
    for b in report["boundaries"]:
        f = b["file"]
        if f is None:
            out.append("| `%s` | %s | not in the export | - | - |" % (b["path"], b["reads"]))
        else:
            out.append("| `%s` | %s | %s | %s | %s |" % (b["path"], b["reads"], cell(f["lines"]), cell(f["regions"]), cell(f["functions"])))
        fn = b["functions"]
        if fn is not None:
            out.append("| fn `%s` | the load path within | - | %s | %s |" % (", ".join(b["function_names"]), cell(fn["regions"]), cell(fn["functions"])))
    out.append("")
    return "\n".join(out)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("export", nargs="?", help="the JSON `llvm-cov export` wrote")
    ap.add_argument("--markdown", help="where the tables are written (stdout when absent)")
    ap.add_argument("--summary", help="where the JSON summary is written")
    ap.add_argument("--check-paths", action="store_true", help="only check the boundary list's paths")
    args = ap.parse_args(argv)
    gone = missing_paths()
    if gone:
        print("coverage_report: boundary paths no longer in the tree: " + ", ".join(gone), file=sys.stderr)
        return 1
    if args.check_paths:
        print("coverage_report: all %d boundary paths exist" % len(BOUNDARIES))
        return 0
    if not args.export:
        ap.error("an export file or --check-paths")
    with open(args.export) as f:
        report = build(json.load(f))
    text = render(report)
    if args.markdown:
        with open(args.markdown, "w") as f:
            f.write(text)
    else:
        print(text)
    if args.summary:
        with open(args.summary, "w") as f:
            json.dump(report, f, indent=2, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
