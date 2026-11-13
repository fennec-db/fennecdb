#!/usr/bin/env python3
"""A pull request held to its base in instructions an op: CI's `perf
(instructions)` job and `make perf-gate` (docs/design/benchmarks.md#the-gate).

The workloads are crates/fennec-http/examples/gate.rs's: one program, built
in this tree and in BASE's -- this tree's source copied in, so both sides
run the same work -- for the static musl build that ships, in the release
profile. Each workload runs RUNS (5) times a build under callgrind, its
setup uncounted: the program asks callgrind to count as its loop starts and
to stop as it ends. A build's figure for a workload is the median of its
runs' instructions. A workload whose figure in this tree is THRESHOLD (1%)
or more above BASE's fails the gate, unless the pull request adds an entry
to PERF_ACCEPTED below naming it, the most it lets through and why.

Why a median: std's hash tables draw their keys at random in each process
(`RandomState`), and a hundred or so sit on the engine's paths. Whether two
keys of a small table share a probe is the draw's, so one run's count
stands some 80 instructions an op from the next -- 0.4-0.6% of a request of
14 000 -- in a few steps. The median of five takes the commonest step, and
the threshold stands above it: the control and the calibration over merged
pull requests that set it are in the design document.

    python3 tools/perf_gate.py run --base origin/main     # make perf-gate
    python3 tools/perf_gate.py measure --base-bin B --head-bin H --out R.json
    python3 tools/perf_gate.py check R.json               # this tree's entries
    python3 -I base_copy.py check R.json --accepted-from tools/perf_gate.py \\
        --accepted-base base_copy.py                      # CI: the base's checker

CI judges with the base's copy of this checker, reading PERF_ACCEPTED from
the pull request's as data (`--accepted-from`): a pull request lowering the
bar or rewriting the rule is judged by the rule it would replace. An entry
lets its workload through only in the pull request that adds it -- one the
base holds already (`--accepted-base`) lets nothing through -- so the list
is the history of what was let through, never removed.

`run` and `measure` count with valgrind, which runs on Linux: elsewhere they
say so and exit 0 (on macOS, in Docker: docs/design/benchmarks.md#the-gate).
`check` runs anywhere. The I/O counts (P50, under strace) are another
metric of the same results file, to come: `metric` names what a file holds.
"""

import argparse
import ast
import concurrent.futures
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PROGRAM = os.path.join("crates", "fennec-http", "examples", "gate.rs")

# A workload this much or more above its base's figure fails: +1% after the
# calibration (docs/design/benchmarks.md#the-gate). A fraction.
THRESHOLD = 0.01
# Runs a build of each workload: the figure is their median.
RUNS = 5

# Every workload a pull request made slower on purpose, in the order they
# went out, never removed: the history of what the gate let through. An
# entry lets its workload through only in the pull request that adds it, up
# to `most`, the measured delta with room for the merge queue's rerun; a
# later regression of the same workload needs an entry of its own. `reason`
# says what the instructions buy and how they were measured (`make
# perf-gate`, `make instr-compare`'s figure on macOS). A literal: the base's
# checker reads it from a pull request's copy as data, never running it
# (`--accepted-from`).
#
#   {"workload": "http-put-keyed", "most": "+1.8%",
#    "reason": "a key's row now holds its request's digest, which a replay
#               checks: +260 instructions a keyed put (make perf-gate),
#               +1.5% by make instr-compare MODES=keyed on an M1"}
PERF_ACCEPTED = []

# What an entry must say, and how long its reason is at the least: a
# written justification, not a word.
ENTRY_KEYS = {"workload", "most", "reason"}
REASON_AT_LEAST = 40


# ------------------------------------------------------------------ entries


def accepted_in(path):
    """PERF_ACCEPTED as the checker at `path` writes it, read as data -- a
    literal, never run: what the base's copy holds a pull request's entries
    by."""
    with open(path, encoding="utf-8") as f:
        tree = ast.parse(f.read(), path)
    for node in tree.body:
        targets = node.targets if isinstance(node, ast.Assign) else [getattr(node, "target", None)]
        if any(isinstance(t, ast.Name) and t.id == "PERF_ACCEPTED" for t in targets):
            return ast.literal_eval(node.value)
    raise ValueError(f"{path} assigns no PERF_ACCEPTED")


def percent(text):
    """`+1.8%` as 0.018; anything else a ValueError."""
    m = re.fullmatch(r"\+(\d+(?:\.\d+)?)%", str(text).strip())
    if not m:
        raise ValueError(f"`most` is a percentage above the base, as \"+1.8%\": {text!r}")
    return float(m.group(1)) / 100


def checked(entries):
    """`entries` held to the form an entry takes: each a dict of exactly
    ENTRY_KEYS, a workload's name, a `most` above the threshold and a
    reason in words. A malformed entry is the gate's failure, never one
    passed over."""
    if not isinstance(entries, list):
        raise ValueError("PERF_ACCEPTED is a list of entries")
    for e in entries:
        if not isinstance(e, dict) or set(e) != ENTRY_KEYS:
            raise ValueError(f"an entry names exactly {sorted(ENTRY_KEYS)}: {e!r}")
        if not isinstance(e["workload"], str) or not e["workload"]:
            raise ValueError(f"an entry's workload is a name: {e!r}")
        most = percent(e["most"])
        if most < THRESHOLD:
            raise ValueError(f"{e['workload']}: `most` {e['most']} is under the threshold, which needs no entry")
        if not isinstance(e["reason"], str) or len(e["reason"].strip()) < REASON_AT_LEAST:
            raise ValueError(f"{e['workload']}: the reason is a written justification, {REASON_AT_LEAST} characters at the least")
    return entries


def added(head, base):
    """The entries `head` holds and `base` does not: the pull request's own.
    Entries are compared whole, so one edited counts as added."""
    seen = [json.dumps(e, sort_keys=True) for e in base]
    return [e for e in head if json.dumps(e, sort_keys=True) not in seen]


# ------------------------------------------------------------------ judging


def figure(runs):
    """A build's figure for a workload: the median of its runs, an int."""
    return int(statistics.median_low(runs))


def judge(results, entries=(), threshold=THRESHOLD):
    """Each workload of `results` judged: (rows, failed), a row (name, ops,
    base, head, delta, verdict) -- base and head figures for the whole loop,
    delta a fraction or None. `entries` are the pull request's own."""
    if results.get("metric") != "instructions":
        raise ValueError(f"these results hold {results.get('metric')!r}, not instructions")
    names = set(results["workloads"])
    for e in entries:
        if e["workload"] not in names:
            raise ValueError(f"PERF_ACCEPTED names {e['workload']!r}, which no workload is")
    allowed = {}
    for e in entries:
        allowed[e["workload"]] = max(allowed.get(e["workload"], 0.0), percent(e["most"]))
    rows, failed = [], False
    for name in sorted(names):
        w = results["workloads"][name]
        base, head = w.get("base") or [], w.get("head") or []
        if not head:
            rows.append((name, w["ops"], figure(base) if base else None, None, None, "not in this tree"))
            continue
        if not base:
            why = "the base's build did not run it" if w.get("base_error") else "new"
            rows.append((name, w["ops"], None, figure(head), None, f"{why}: held to nothing"))
            continue
        if min(len(base), len(head)) < RUNS:
            # Fewer runs are a median the seeds move: a figure the base's
            # checker does not take, whatever the pull request's counting.
            rows.append((name, w["ops"], figure(base), figure(head), None, f"FAILED: fewer than {RUNS} runs a side"))
            failed = True
            continue
        b, h = figure(base), figure(head)
        delta = h / b - 1
        if delta < threshold:
            verdict = "ok"
        elif name in allowed and delta <= allowed[name]:
            verdict = f"accepted (PERF_ACCEPTED, up to +{allowed[name] * 100:.1f}%)"
        elif name in allowed:
            verdict = f"FAILED: past its PERF_ACCEPTED entry's +{allowed[name] * 100:.1f}%"
            failed = True
        else:
            verdict = f"FAILED: +{threshold * 100:g}% or more"
            failed = True
        rows.append((name, w["ops"], b, h, delta, verdict))
    return rows, failed


def spread(runs):
    """A side's runs' range against their median, a fraction."""
    m = statistics.median(runs)
    return (max(runs) - min(runs)) / m if m else 0.0


def table(results, rows):
    """The judgement as Markdown: a step summary's, a pull request's."""
    out = [
        f"### Instructions an op, base against this tree (median of {results.get('runs', '?')} callgrind runs)",
        "",
        "| workload | ops | base | this tree | delta | runs' spread | verdict |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    for name, ops, b, h, delta, verdict in rows:
        w = results["workloads"][name]
        per = lambda x: f"{x / ops:,.0f}" if x is not None else "-"
        d = f"{delta * 100:+.2f}%" if delta is not None else "-"
        sp = " / ".join(f"{spread(w[s]) * 100:.2f}%" for s in ("base", "head") if w.get(s))
        out.append(f"| {name} | {ops} | {per(b)} | {per(h)} | {d} | {sp} | {verdict} |")
    out.append("")
    out.append(
        f"A workload fails at +{THRESHOLD * 100:g}% or more, unless the pull request adds a "
        "PERF_ACCEPTED entry (tools/perf_gate.py) naming it: docs/design/benchmarks.md#the-gate."
    )
    return "\n".join(out) + "\n"


def check(args):
    try:
        with open(args.results, encoding="utf-8") as f:
            results = json.load(f)
        head = checked(accepted_in(args.accepted_from or os.path.abspath(__file__)))
        base = checked(accepted_in(args.accepted_base)) if args.accepted_base else []
        rows, failed = judge(results, added(head, base))
        text = table(results, rows)
    except ValueError as e:
        # Said where the pull request's author reads the gate's table.
        text, failed = f"### The perf gate refused its input\n\n{e}\n", True
    print(text)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as f:
            f.write(text)
    return 1 if failed else 0


# ------------------------------------------------------------------ counting


def counting():
    """Whether valgrind can count here: Linux with valgrind on the PATH."""
    if platform.system() != "Linux" or shutil.which("valgrind") is None:
        print(
            "perf_gate: callgrind counts on Linux with valgrind installed; on macOS run it in "
            "Docker (docs/design/benchmarks.md#the-gate). Nothing measured.",
            file=sys.stderr,
        )
        return False
    return True


def listed(binary):
    """The workloads `binary` runs: [(name, ops)]."""
    out = subprocess.run([binary, "--list"], capture_output=True, text=True, check=True).stdout
    return [(name, int(ops)) for name, ops in (line.split() for line in out.splitlines() if line.strip())]


def totals(path):
    """The instructions a callgrind output file counted: its `totals:` line,
    or `summary:` where an older callgrind wrote that alone."""
    found = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            for key in ("totals:", "summary:"):
                if line.startswith(key):
                    found[key] = int(line.split()[1])
    n = found.get("totals:", found.get("summary:"))
    if n is None:
        raise ValueError(f"{path}: no totals")
    return n


def one_run(binary, name, ops, scratch):
    """Runs workload `name` once under callgrind: the instructions its loop
    counted, and the profile callgrind wrote, by function. Nothing counted
    means valgrind passed the program's requests by -- a sequence of
    `callgrind::request` not valgrind's -- and is refused."""
    fd, out = tempfile.mkstemp(prefix=f"{name}.", suffix=".callgrind", dir=scratch)
    os.close(fd)
    cmd = [
        "valgrind", "--tool=callgrind", "--instr-atstart=no", "--collect-atstart=no",
        "--dump-instr=no", "--dump-line=no", f"--callgrind-out-file={out}", binary, name,
    ]
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"{name}: exit {p.returncode}\n{p.stdout}\n{p.stderr[-3000:]}")
    if f"{name} {ops} ops" not in p.stdout:
        raise RuntimeError(f"{name}: the program did not run its {ops} ops:\n{p.stdout}")
    n = totals(out)
    if n <= 0:
        raise RuntimeError(f"{name}: callgrind counted nothing -- were the client requests seen?")
    return n, out


def measure_bins(base_bin, head_bin, runs, jobs, scratch, log=print, profiles=None, only=None):
    """Every workload of this tree's build counted `runs` times a build, the
    runs of all of them spread over `jobs` processes, each round of every
    workload and build before the next: a count does not move with how
    many run beside it, the workloads keeping out what runs on the clock.
    With `profiles`, the profile of each side's median run of each workload
    is kept there (`<workload>.<side>.callgrind`), for `callgrind_annotate`
    to tell where a difference is; the rest are let go of."""
    sides = {"head": dict(listed(head_bin))}
    if base_bin:
        sides["base"] = dict(listed(base_bin))
    names = sorted(n for n in sides["head"] if not only or n in only)
    work = {"head": head_bin, "base": base_bin}
    results = {"metric": "instructions", "tool": "callgrind", "runs": runs, "workloads": {}}
    failed, files = {}, {}
    for name in names:
        results["workloads"][name] = {"ops": sides["head"][name], "base": [], "head": []}
        if base_bin and name not in sides["base"]:
            failed[("base", name)] = f"{name}: the base's program has no such workload"
        elif base_bin and sides["base"][name] != sides["head"][name]:
            # The base's own program, where this tree's did not build there:
            # another loop, held to nothing rather than to another's count.
            failed[("base", name)] = f"{name}: {sides['base'][name]} ops in the base's program, {sides['head'][name]} here"
    tasks = [
        (side, name)
        for _ in range(runs)
        for name in names
        for side in ("base", "head")
        if side in sides and (side, name) not in failed
    ]
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {
            pool.submit(one_run, work[side], name, sides["head"][name], scratch): (side, name)
            for side, name in tasks
        }
        for fut in concurrent.futures.as_completed(futures):
            side, name = futures[fut]
            try:
                n, path = fut.result()
                results["workloads"][name][side].append(n)
                files.setdefault((side, name), []).append((n, path))
            except RuntimeError as e:
                failed.setdefault((side, name), str(e))
            done += 1
            if done % 20 == 0 or done == len(tasks):
                log(f"  {done}/{len(tasks)} runs")
    for (side, name), kept in files.items():
        kept.sort()
        median = figure([n for n, _ in kept])
        for n, path in kept:
            if profiles and n == median:
                os.makedirs(profiles, exist_ok=True)
                shutil.move(path, os.path.join(profiles, f"{name}.{side}.callgrind"))
                median = None
            elif os.path.exists(path):
                os.unlink(path)
    heads = [msg for (side, _), msg in sorted(failed.items()) if side == "head"]
    if heads:
        raise RuntimeError("a workload failed in this tree's build:\n" + "\n".join(heads))
    for w in results["workloads"].values():
        w["base"].sort()
        w["head"].sort()
    # A workload the base cannot run -- a statement only this tree knows,
    # its first workload over a new feature -- is held to nothing, and says
    # why: none of its base runs count.
    for (_, name), msg in failed.items():
        results["workloads"][name]["base"] = []
        results["workloads"][name]["base_error"] = msg[-600:]
    return results


def measure(args):
    if not counting():
        return 0
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    scratch = tempfile.mkdtemp(prefix="perf-gate-")
    try:
        results = measure_bins(
            args.base_bin, args.head_bin, args.runs, args.jobs, scratch,
            profiles=args.profiles, only=args.only.split(",") if args.only else None,
        )
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(results, f, indent=1)
        f.write("\n")
    print(f"results: {args.out}")
    return 0


# ------------------------------------------------------------------ building


def build(tree, target, target_dir, cargo, log):
    """Builds the gate program in `tree`: the binary's path, or None where
    it does not build there."""
    cmd = [cargo, "build", "--locked", "--release", "-p", "fennec-http", "--example", "gate"]
    if target:
        cmd += ["--target", target]
    # The release profile's code with its symbols kept, which the profile
    # strips: a stripped binary's code is the same, and a profile of one
    # names no function a regression is in.
    env = dict(os.environ, CARGO_TARGET_DIR=target_dir, CARGO_PROFILE_RELEASE_STRIP="false")
    p = subprocess.run(cmd, cwd=tree, env=env, capture_output=True, text=True)
    if p.returncode != 0:
        log(f"the gate program did not build in {tree}:\n{p.stderr[-4000:]}")
        return None
    return os.path.join(target_dir, *([target] if target else []), "release", "examples", "gate")


def git(*args, cwd=ROOT):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()


def worktree(ref, where):
    """A detached worktree of `ref` at `where`, made anew: its commit."""
    if os.path.exists(where):
        subprocess.run(["git", "worktree", "remove", "--force", where], cwd=ROOT, capture_output=True)
    sha = git("rev-parse", "--verify", f"{ref}^{{commit}}")
    git("worktree", "add", "--detach", where, sha)
    return sha


def run(args):
    """`make perf-gate`: this tree -- or HEAD's, a replay of two commits --
    and BASE's built, counted and judged."""
    if not counting():
        return 0
    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)
    base_tree = os.path.join(work, "base")
    sha = worktree(args.base, base_tree)
    head_tree = ROOT
    if args.head:
        head_tree = os.path.join(work, "head")
        worktree(args.head, head_tree)
    log = lambda m: print(m, flush=True)
    try:
        # The same program on both sides: this tree's, copied into the
        # base's. One the base's crates do not build -- an interface the
        # pull request changed -- falls back to the base's own copy, whose
        # workloads are then held to these by name, said in the summary.
        own = os.path.join(base_tree, PROGRAM)
        had = os.path.exists(own)
        for tree in {base_tree, head_tree} - {ROOT}:
            shutil.copyfile(os.path.join(ROOT, PROGRAM), os.path.join(tree, PROGRAM))
        log(f"building the gate program in {head_tree} and at {args.base} ({sha[:10]}), side by side")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            head_f = pool.submit(build, head_tree, args.target, os.path.join(work, "head-target"), args.cargo, log)
            base_f = pool.submit(build, base_tree, args.target, os.path.join(work, "base-target"), args.cargo, log)
            head_bin, base_bin = head_f.result(), base_f.result()
        if head_bin is None:
            return 1
        programs = "this tree's"
        if base_bin is None and had:
            git("checkout", "--", PROGRAM, cwd=base_tree)
            base_bin = build(base_tree, args.target, os.path.join(work, "base-target"), args.cargo, log)
            programs = "each tree's own"
        if base_bin is None:
            log("the base builds no gate program: nothing to hold this tree to")
            return 1
        log(f"counting {args.runs} runs a build of each workload, {args.jobs} at a time")
        scratch = tempfile.mkdtemp(prefix="scratch-", dir=work)
        try:
            results = measure_bins(
                base_bin, head_bin, args.runs, args.jobs, scratch, log, os.path.join(work, "profiles")
            )
        finally:
            shutil.rmtree(scratch, ignore_errors=True)
        results["base"] = sha
        results["programs"] = programs
        out = args.out or os.path.join(work, "results.json")
        with open(out, "w", encoding="utf-8") as f:
            json.dump(results, f, indent=1)
            f.write("\n")
        log(f"results: {out}")
    finally:
        if not args.keep:
            for tree in {base_tree, head_tree} - {ROOT}:
                subprocess.run(["git", "worktree", "remove", "--force", tree], cwd=ROOT, capture_output=True)
    if args.no_check:
        return 0
    base_copy = os.path.join(work, "base_perf_gate.py")
    have = subprocess.run(["git", "show", f"{sha}:tools/perf_gate.py"], cwd=ROOT, capture_output=True, text=True)
    accepted_base = None
    if have.returncode == 0:
        with open(base_copy, "w", encoding="utf-8") as f:
            f.write(have.stdout)
        accepted_base = base_copy
    ns = argparse.Namespace(
        results=out, accepted_from=os.path.abspath(__file__), accepted_base=accepted_base, summary=args.summary
    )
    return check(ns)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="command", required=True)

    r = sub.add_parser("run", help="build this tree and BASE's, count both, judge")
    r.add_argument("--base", default="origin/main", help="the git ref held to (origin/main)")
    r.add_argument("--head", help="a git ref held to it in place of this tree: a replay of two commits")
    r.add_argument("--target", help="the build's target triple (CI: x86_64-unknown-linux-musl)")
    r.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    r.add_argument("--runs", type=int, default=RUNS)
    r.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    r.add_argument("--work", default=os.path.join(ROOT, "target", "perf-gate"), help="scratch: builds, results")
    r.add_argument("--out", help="the results file (WORK/results.json)")
    r.add_argument("--keep", action="store_true", help="keep the base's worktree")
    r.add_argument("--no-check", action="store_true", help="count only: CI judges with the base's checker")
    r.add_argument("--summary", help="append the judgement's table to this file (a step summary)")

    m = sub.add_parser("measure", help="count two built programs")
    m.add_argument("--base-bin", help="the base's gate program (none: this tree's alone)")
    m.add_argument("--head-bin", required=True)
    m.add_argument("--runs", type=int, default=RUNS)
    m.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    m.add_argument("--out", required=True)
    m.add_argument("--profiles", help="keep each side's median run's callgrind profile here")
    m.add_argument("--only", help="these workloads alone, a comma between: a look at one")

    c = sub.add_parser("check", help="judge a results file")
    c.add_argument("results")
    c.add_argument("--accepted-from", help="the checker whose PERF_ACCEPTED is the pull request's (this one)")
    c.add_argument("--accepted-base", help="the checker whose PERF_ACCEPTED the base holds already")
    c.add_argument("--summary", help="append the table to this file (a step summary)")

    args = ap.parse_args(argv)
    try:
        return {"run": run, "measure": measure, "check": check}[args.command](args)
    except (ValueError, RuntimeError) as e:
        print(f"perf_gate: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
