#!/usr/bin/env python3
"""Appends measured values to a family's results file, in the schema every
bench shares (crates/fennec-bench/results/<family>.tsv: run, commit,
machine, date, family, cell, metric, value, a line a value), which
site/build.py takes the medians of and holds the site's figures to.

    tools/bench_record.py requests --run n1 --cell 'docker:read' \\
        round_trip_us=288 inside_us=23

A bench target that prints its figures pipes them here rather than leaving
them to be copied by hand into a page. The commit is the tree's HEAD
(`-dirty` with uncommitted changes), the machine `FENNEC_BENCH_MACHINE` or
the host's name; `--machine` and `--date` override them. The file is only
appended to: a run measured again is a new run id, a replaced one is named
in superseded.tsv.
"""
import argparse
import datetime
import os
import socket
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RESULTS = os.path.join(REPO, "crates", "fennec-bench", "results")
COLUMNS = ("run", "commit", "machine", "date", "family", "cell", "metric", "value")


def commit():
    def git(*args):
        return subprocess.run(("git", "-C", REPO) + args, capture_output=True, text=True).stdout.strip()
    sha = git("rev-parse", "--short=8", "HEAD")
    return sha + ("-dirty" if git("status", "--porcelain", "--untracked-files=no") else "")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("family", help="the file's name: results/<family>.tsv")
    ap.add_argument("--run", required=True, help="a run id, new for each measurement")
    ap.add_argument("--cell", required=True, help="what was measured, colon-separated parts")
    ap.add_argument("--machine", default=os.environ.get("FENNEC_BENCH_MACHINE") or socket.gethostname().split(".")[0])
    ap.add_argument("--date", default=datetime.date.today().isoformat())
    ap.add_argument("values", nargs="+", metavar="metric=value")
    args = ap.parse_args()
    if args.family in ("superseded",) or "/" in args.family:
        sys.exit("a family is a file's name under results/, not superseded or a path")
    rows = []
    for item in args.values:
        metric, sep, value = item.partition("=")
        if not sep:
            sys.exit(f"{item!r} is not metric=value")
        try:
            float(value)
        except ValueError:
            sys.exit(f"{metric}: {value!r} is not a number")
        rows.append((args.run, commit(), args.machine, args.date, args.family, args.cell, metric, value))
    for row in rows:
        if any("\t" in f or "\n" in f for f in row):
            sys.exit("a field holds a tab or a newline")
    path = os.path.join(RESULTS, args.family + ".tsv")
    new = not os.path.exists(path)
    os.makedirs(RESULTS, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        if new:
            f.write("\t".join(COLUMNS) + "\n")
        for row in rows:
            f.write("\t".join(row) + "\n")
    print(f"{len(rows)} lines to {os.path.relpath(path, REPO)}")


if __name__ == "__main__":
    main()
