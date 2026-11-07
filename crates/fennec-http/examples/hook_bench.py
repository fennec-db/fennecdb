#!/usr/bin/env python3
"""What a webhook event costs, every way slice 3 owes it measured, in turns:
`make hook-bench` (`docs/design/named-statements.md`, *What a hook costs*).

Three groups, each run a process of its own, `MallocNanoZone=0` (a
rebuild's binary moves microsecond timings by the nano zone's layout):

  * `paired` -- what a request that is no event pays: `replays.rs`'s `put`
    and `keyed` (a write by id through `/query`, buffered, without and with
    an `Idempotency-Key`), this tree's build against BASE's (origin/main
    unless given), PAIRS pairs a mode, the order within a pair alternating.
    The one group main can run: `hook_events.rs` exists on this branch
    alone.
  * `hooks` -- `hook_events.rs`'s modes on this tree, a round each in a
    rotated order, PAIRS rounds: `event` (a Stripe event verified, taken
    and fsynced), `keyed` and `put` (the same `set` under an
    `Idempotency-Key`, and plain, both under `--sync always`), `buffered`
    (`--sync 250`), `duplicate` (answered from its key), `forged` (the one
    401), `verify4k` and `verify64k` (`Hooks::verify` alone).
  * `beside` -- `beside0`, `beside10` and `beside100`: buffered writes'
    median and p99 beside 0, 10 and 100 events a second, each fsynced.

Every run starts once the 1-minute load is under QUIET, and is discarded
and made again (three tries) where the load stood past RERUN as it ended:
a run beside someone else's work measures that work. The binaries are
built once, before the first run, and copied aside, so nothing rebuilds
them midway; BASE's in a scratch worktree under `target/hook-bench/`,
removed with its build at the end unless `--keep`.

OUT (`target/hook-bench/results.txt`) is rewritten after every run: the
medians of each mode and build -- ns a request, and on macOS the
instructions and cycles the loop retired a request (`proc_pid_rusage`) --
the paired group's branch against BASE by median and by the median of the
pairs' differences, then every run. Progress goes to OUT's `.log`. Not
run in CI; an hour or so on an idle machine.
"""

import argparse
import datetime
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time

HOOKS = ["event", "keyed", "put", "buffered", "duplicate", "forged", "verify4k", "verify64k"]
BESIDE = ["beside0", "beside10", "beside100"]
PAIRED = ["put", "keyed"]
GROUPS = ["paired", "hooks", "beside"]


def sh(cmd, cwd, env=None, log=None):
    if log:
        log("$ " + " ".join(cmd))
    subprocess.run(cmd, cwd=cwd, env=env, check=True)


def git(root, *a):
    return subprocess.run(["git", *a], cwd=root, check=True, capture_output=True, text=True).stdout.strip()


def parse(out):
    r = {}
    m = re.search(r"^ns a request (\d+)", out, re.M)
    if m:
        r["ns"] = float(m.group(1))
    m = re.search(r"^a request: ([\d.]+) instructions, ([\d.]+) cycles", out, re.M)
    if m:
        r["instr"], r["cycles"] = float(m.group(1)), float(m.group(2))
    m = re.search(r"^buffered writes: median (\d+) ns, p99 (\d+) ns, max (\d+) ns; (\d+) events", out, re.M)
    if m:
        r["median"], r["p99"], r["max"], r["events"] = (float(g) for g in m.groups())
    return r


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--cargo", default=os.environ.get("CARGO") or os.path.expanduser("~/.cargo/bin/cargo"))
    ap.add_argument("--base", default="origin/main", help="the ref the paired group is held to")
    ap.add_argument("--pairs", type=int, default=7, help="pairs, and rounds, of each mode")
    ap.add_argument("--quiet", type=float, default=2.5, help="the 1-minute load a run starts under")
    ap.add_argument("--rerun", type=float, help="a run ending past this load is made again (QUIET + 1.5)")
    ap.add_argument("--only", default=",".join(GROUPS), help="groups, comma-separated: " + ", ".join(GROUPS))
    ap.add_argument("--out", default="target/hook-bench/results.txt")
    ap.add_argument("--keep", action="store_true", help="keep BASE's worktree and build")
    a = ap.parse_args()
    rerun = a.rerun if a.rerun is not None else a.quiet + 1.5
    groups = [g.strip() for g in a.only.split(",") if g.strip()]
    for g in groups:
        if g not in GROUPS:
            sys.exit(f"no group {g}: " + ", ".join(GROUPS))

    root = git(os.getcwd(), "rev-parse", "--show-toplevel")
    work = os.path.join(root, "target", "hook-bench")
    os.makedirs(work, exist_ok=True)
    out = os.path.join(root, a.out) if not os.path.isabs(a.out) else a.out
    os.makedirs(os.path.dirname(out), exist_ok=True)
    logf = open(out + ".log", "a", buffering=1)

    def log(msg):
        line = f"{datetime.datetime.now():%H:%M:%S} {msg}"
        print(line, flush=True)
        logf.write(line + "\n")

    head = git(root, "rev-parse", "--short", "HEAD")
    dirty = " (with uncommitted changes)" if git(root, "status", "--porcelain", "--untracked-files=no") else ""
    base_sha = git(root, "rev-parse", "--short", a.base) if "paired" in groups else "-"
    env = dict(os.environ, CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "4"))
    bins = {}
    base_tree = os.path.join(work, "base")
    base_target = os.path.join(work, "base-target")
    made_tree = False
    try:
        log(f"building this tree ({head}{dirty})")
        examples = (["--example", "hook_events"] if {"hooks", "beside"} & set(groups) else []) + (
            ["--example", "replays"] if "paired" in groups else [])
        sh([a.cargo, "build", "--release", "-p", "fennec-http", *examples], root, env, log)
        for ex in ("hook_events", "replays"):
            src = os.path.join(root, "target", "release", "examples", ex)
            if ex in examples:
                dst = os.path.join(work, f"{ex}-branch")
                shutil.copy2(src, dst)
                bins[(ex, "branch")] = dst
        if "paired" in groups:
            log(f"building {a.base} ({base_sha}) in {base_tree}")
            if os.path.exists(base_tree):
                subprocess.run(["git", "worktree", "remove", "--force", base_tree], cwd=root)
            sh(["git", "worktree", "add", "--detach", base_tree, a.base], root, None, log)
            made_tree = True
            benv = dict(env, CARGO_TARGET_DIR=base_target)
            sh([a.cargo, "build", "--release", "-p", "fennec-http", "--example", "replays"], base_tree, benv, log)
            dst = os.path.join(work, "replays-base")
            shutil.copy2(os.path.join(base_target, "release", "examples", "replays"), dst)
            bins[("replays", "base")] = dst

        runs = []

        def write():
            with open(out, "w") as f:
                f.write(f"# make hook-bench, {datetime.datetime.now():%Y-%m-%d %H:%M}: this tree {head}{dirty}")
                f.write(f", BASE {a.base} {base_sha}; {platform.platform()}, {os.cpu_count()} CPUs\n")
                f.write(f"# pairs {a.pairs}, a run started under a load of {a.quiet}, made again past {rerun}\n\n")
                f.write("## Medians\n\n")
                f.write(f"{'group':8} {'mode':10} {'build':7} {'runs':>4} {'ns/req':>10} {'instr/req':>11} "
                        f"{'cycles/req':>11} {'buf.median':>10} {'buf.p99':>9} {'events':>7}\n")
                keys = []
                for r in runs:
                    k = (r["group"], r["mode"], r["build"])
                    if k not in keys:
                        keys.append(k)
                med = {}
                for k in keys:
                    rs = [r for r in runs if (r["group"], r["mode"], r["build"]) == k]
                    m = {f: statistics.median([r[f] for r in rs]) for f in
                         ("ns", "instr", "cycles", "median", "p99", "events") if all(f in r for r in rs)}
                    med[k] = m
                    cell = lambda f, w, p=0: f"{m[f]:{w}.{p}f}" if f in m else f"{'-':>{w}}"
                    f.write(f"{k[0]:8} {k[1]:10} {k[2]:7} {len(rs):>4} {cell('ns', 10)} {cell('instr', 11, 1)} "
                            f"{cell('cycles', 11, 1)} {cell('median', 10)} {cell('p99', 9)} {cell('events', 7)}\n")
                paired = [m for m in PAIRED if ("paired", m, "base") in med and ("paired", m, "branch") in med]
                if paired:
                    f.write(f"\n## This tree against {a.base} ({base_sha}), the paired group\n\n")
                    for mode in paired:
                        b, t = med[("paired", mode, "base")], med[("paired", mode, "branch")]
                        for fld in ("ns", "instr"):
                            if fld not in b or fld not in t:
                                continue
                            diffs = []
                            for p in range(a.pairs):
                                x = [r[fld] for r in runs if (r["mode"], r["pair"], r["group"]) == (mode, p, "paired")
                                     and r["build"] == "base"]
                                y = [r[fld] for r in runs if (r["mode"], r["pair"], r["group"]) == (mode, p, "paired")
                                     and r["build"] == "branch"]
                                if x and y:
                                    diffs.append(y[0] - x[0])
                            d = statistics.median(diffs) if diffs else float("nan")
                            f.write(f"{mode:6} {fld:6} base {b[fld]:12.1f}  branch {t[fld]:12.1f}  "
                                    f"{100 * (t[fld] - b[fld]) / b[fld]:+6.2f}%  pairs' differences: median "
                                    f"{d:+.1f}, each {', '.join(f'{x:+.0f}' for x in diffs)}\n")
                f.write("\n## Runs\n\n")
                f.write("group    mode       build   pair    ns/req   instr/req  cycles/req  buf.median  buf.p99  "
                        "events  load before/after  secs\n")
                for r in runs:
                    g = lambda fld, w, p=0: f"{r[fld]:{w}.{p}f}" if fld in r else f"{'-':>{w}}"
                    f.write(f"{r['group']:8} {r['mode']:10} {r['build']:7} {r['pair']:>4} {g('ns', 9)} {g('instr', 11, 1)} "
                            f"{g('cycles', 11, 1)} {g('median', 11)} {g('p99', 8)} {g('events', 7)}  "
                            f"{r['before']:5.2f}/{r['after']:5.2f}  {r['secs']:6.1f}\n")

        def run(group, ex, mode, build, pair):
            for attempt in range(1, 4):
                while (load := os.getloadavg()[0]) >= a.quiet:
                    log(f"load {load:.2f}, waiting for it under {a.quiet}")
                    time.sleep(30)
                t = time.time()
                p = subprocess.run([bins[(ex, build)], mode], capture_output=True, text=True,
                                   env=dict(os.environ, MallocNanoZone="0"), timeout=7200)
                secs, after = time.time() - t, os.getloadavg()[0]
                if p.returncode != 0:
                    sys.exit(f"{ex} {mode} ({build}) failed:\n{p.stdout}\n{p.stderr[-4000:]}")
                r = dict(parse(p.stdout), group=group, mode=mode, build=build, pair=pair, before=load,
                         after=after, secs=secs)
                if after >= rerun and attempt < 3:
                    log(f"{group} {mode} {build} #{pair}: the load rose to {after:.2f}, discarded")
                    continue
                runs.append(r)
                write()
                log(f"{group} {mode} {build} #{pair}: {r.get('ns', 0):.0f} ns, "
                    f"{r.get('instr', 0):.1f} instructions a request, load {load:.2f}/{after:.2f}, {secs:.1f} s")
                return

        for pair in range(a.pairs):
            if "paired" in groups:
                order = ["base", "branch"] if pair % 2 == 0 else ["branch", "base"]
                for mode in PAIRED:
                    for build in order:
                        run("paired", "replays", mode, build, pair)
            if "hooks" in groups:
                k = pair % len(HOOKS)
                for mode in HOOKS[k:] + HOOKS[:k]:
                    run("hooks", "hook_events", mode, "branch", pair)
            if "beside" in groups:
                k = pair % len(BESIDE)
                for mode in BESIDE[k:] + BESIDE[:k]:
                    run("beside", "hook_events", mode, "branch", pair)
        log(f"done: {out}")
    finally:
        if made_tree and not a.keep:
            subprocess.run(["git", "worktree", "remove", "--force", base_tree], cwd=root)
            shutil.rmtree(base_target, ignore_errors=True)
            log("removed BASE's worktree and build")


if __name__ == "__main__":
    main()
