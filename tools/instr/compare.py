#!/usr/bin/env python3
"""Two builds' instructions an op on macOS, the allocator's draw averaged
out: `make instr-compare` (docs/design/benchmarks.md#instruction-counts).

macOS 26's allocator, xzone malloc, sends a binary's allocations and frees
down paths its identity picks, so the same code built twice, or one build
with only its Mach-O UUID rewritten, runs one request of `replays.rs` some
500 to 1 100 instructions apart, all of them the allocator's (#513), and
`MallocNanoZone=0` no longer holds that still. A main-against-branch difference under ~500 instructions a request
read from one binary a side is the draw, not the change. This reads it over
several draws:

  * Each build -- this tree as it stands and BASE's (`origin/main`, built in
    a scratch worktree), or the binaries `--bin NAME=PATH` names, the first
    the reference -- is copied under UUIDS random UUIDs and each copy signed
    again (`setuuid.py`): each copy is another draw of the allocator's paths.
  * Every copy runs `BIN MODE ARGS` ROUNDS times, all of them in one order a
    round, the order turning each round; a build's figure is the mean of its
    copies' medians, beside its standard error and its copies' range. The
    range of one copy a side against one copy of the other is what a plain
    pair would have read. `--uuids 0` runs each build's own binary alone:
    plain pairs.
  * `--trace FUNCTION` also counts one call of FUNCTION in each build by
    function (`trace.py`, lldb): the program's own code's delta to the
    instruction, the allocator's apart.

The measured program is an example of PKG (`replays`, `hook_events`, any
printing a line PATTERN reads: `a request: <n> instructions` by default),
built with `cargo rustc --release ... -- -C strip=none` -- the release
profile's code, its symbols kept for the trace. A run starts once the
1-minute load is under QUIET and is made again (three tries) where the load
stood past RERUN as it ended: a run beside someone else's work measures
that work. Results go to OUT (WORK/results.txt), rewritten after every run;
progress to OUT's `.log`. The binaries and their copies are scratch under
WORK (`target/instr`), removed at the end unless `--keep`; BASE's worktree
and its build too, once its binary is copied, unless `--keep-base`, where
the next run builds on them.

macOS-only: the counts are `proc_pid_rusage`'s and the signing `codesign`'s;
elsewhere this says so and exits 0.
"""

import argparse
import datetime
import importlib.util
import os
import platform
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import setuuid  # noqa: E402


def tracing():
    """trace.py as a module (`trace` alone would be the standard library's)."""
    spec = importlib.util.spec_from_file_location("instr_trace", os.path.join(HERE, "trace.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def quiet_run(cmd, quiet, rerun, log, env=None, timeout=7200, what=None):
    """Runs `cmd` once the 1-minute load is under `quiet`, made again (three
    tries) where the load stood at `rerun` or past as it ended. Returns
    (stdout, load before, load after); a failed run ends the script."""
    what = what or " ".join(os.path.basename(c) if i == 0 else c for i, c in enumerate(cmd))
    env = dict(os.environ if env is None else env, MallocNanoZone="0")
    for attempt in range(1, 4):
        while (load := os.getloadavg()[0]) >= quiet:
            log(f"load {load:.2f}, waiting for it under {quiet}")
            time.sleep(30)
        p = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)
        after = os.getloadavg()[0]
        if p.returncode != 0:
            sys.exit(f"{what} failed:\n{p.stdout}\n{p.stderr[-4000:]}")
        if after >= rerun and attempt < 3:
            log(f"{what}: the load rose to {after:.2f}, discarded")
            continue
        return p.stdout, load, after


def mean_of_medians(runs_by_identity):
    """A build's figure over its identities: (mean of their medians, its
    standard error, the lowest and the highest median)."""
    meds = [statistics.median(v) for v in runs_by_identity if v]
    mean = statistics.mean(meds)
    se = statistics.stdev(meds) / len(meds) ** 0.5 if len(meds) > 1 else float("nan")
    return mean, se, min(meds), max(meds), meds


def against(ref, other):
    """`other` against `ref`, each mean_of_medians' answer: (difference, its
    standard error, and the lowest and highest difference one identity a
    side reads -- what a plain pair would have)."""
    d = other[0] - ref[0]
    se = (ref[1] ** 2 + other[1] ** 2) ** 0.5
    singles = [y - x for x in ref[4] for y in other[4]]
    return d, se, min(singles), max(singles)


def report(builds, modes, res, uuids, rounds, pattern_what="instructions an op"):
    """The summary lines: each mode's builds and each against the first.
    `res[(mode, build, identity)]` is that identity's runs."""
    out = []
    n = max(uuids, 1)
    for mode in modes:
        if uuids:
            out.append(f"## {mode}: {pattern_what}, the mean of each build's {uuids} identities' medians "
                       f"({rounds} runs each)\n")
        else:
            out.append(f"## {mode}: {pattern_what}, the median of each build's {rounds} runs (plain pairs)\n")
        figures = {}
        for name, _ in builds:
            runs = [res.get((mode, name, i), []) for i in range(n)]
            if not all(runs):
                continue
            f = figures[name] = mean_of_medians(runs)
            if uuids:
                out.append(f"{name:10} {f[0]:12.1f}  SE {f[1]:6.1f}  identities {f[2]:.1f} to {f[3]:.1f}")
            else:
                out.append(f"{name:10} {f[0]:12.1f}  runs {min(runs[0]):.1f} to {max(runs[0]):.1f}")
        first = builds[0][0]
        if first not in figures:
            out.append("")
            continue
        ref = figures[first]
        for name, _ in builds[1:]:
            if name not in figures:
                continue
            if uuids:
                d, se, lo, hi = against(ref, figures[name])
                pct = 100 * d / ref[0]
                out.append(f"{name} against {first}: {d:+.1f} ({pct:+.2f}%), SE {se:.1f} -- "
                           f"{'inside' if abs(d) <= 2 * se else 'outside'} 2 SE (±{2 * se:.0f}); "
                           f"one identity a side reads {lo:+.0f} to {hi:+.0f}")
            else:
                xs, ys = res[(mode, first, 0)], res[(mode, name, 0)]
                diffs = [y - x for x, y in zip(xs, ys)]
                d = figures[name][0] - ref[0]
                out.append(f"{name} against {first}: {d:+.1f} ({100 * d / ref[0]:+.2f}%), the rounds' "
                           f"differences a median of {statistics.median(diffs):+.1f}, {sum(x > 0 for x in diffs)} "
                           f"of {len(diffs)} above, from {min(diffs):+.0f} to {max(diffs):+.0f}")
        out.append("")
    return out


def git(root, *a):
    return subprocess.run(["git", *a], cwd=root, check=True, capture_output=True, text=True).stdout.strip()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--cargo", default=os.environ.get("CARGO") or os.path.expanduser("~/.cargo/bin/cargo"))
    ap.add_argument("--base", default="origin/main", help="the ref this tree is held to")
    ap.add_argument("--bin", action="append", default=[], metavar="NAME=PATH",
                    help="a built binary instead of building (twice or more; the first the reference)")
    ap.add_argument("--package", default="fennec-http")
    ap.add_argument("--example", default="replays")
    ap.add_argument("--modes", default="put,keyed", help="the program's first argument, a run each")
    ap.add_argument("--args", default="", help="the program's arguments after the mode")
    ap.add_argument("--pattern", default=r"^a request: ([\d.]+) instructions",
                    help="a regular expression whose first group is the figure")
    ap.add_argument("--uuids", type=int, default=8, help="identities a build (0: its own binary alone, plain pairs)")
    ap.add_argument("--rounds", type=int, default=7, help="runs of each identity a mode")
    ap.add_argument("--quiet", type=float, default=4.0, help="the 1-minute load a run starts under")
    ap.add_argument("--rerun", type=float, help="a run ending at this load or past is made again (QUIET + 1.5)")
    ap.add_argument("--trace", metavar="FUNCTION", help="count one call of FUNCTION in each build by function")
    ap.add_argument("--hit", type=int, default=30, help="which call --trace counts")
    ap.add_argument("--work", help="scratch: copies, BASE's worktree and build (target/instr)")
    ap.add_argument("--out", help="results (WORK/results.txt)")
    ap.add_argument("--keep", action="store_true", help="keep the builds' binaries (WORK/bin) and their copies")
    ap.add_argument("--keep-base", action="store_true",
                    help="keep BASE's worktree and build, for the next run to build on")
    a = ap.parse_args()
    if sys.platform != "darwin" or not setuuid.signs():
        print(f"compare.py: the allocator whose draw this averages, the counts and the signing are macOS's; "
              f"nothing to do on {sys.platform}", file=sys.stderr)
        return
    rerun = a.rerun if a.rerun is not None else a.quiet + 1.5
    modes = [m for m in a.modes.split(",") if m]
    extra = shlex.split(a.args)
    pattern = re.compile(a.pattern, re.M)
    root = git(os.getcwd(), "rev-parse", "--show-toplevel")
    work = os.path.abspath(a.work or os.path.join(root, "target", "instr"))
    os.makedirs(work, exist_ok=True)
    out = os.path.abspath(a.out or os.path.join(work, "results.txt"))
    os.makedirs(os.path.dirname(out), exist_ok=True)
    logf = open(out + ".log", "a", buffering=1)

    def log(msg):
        line = f"{datetime.datetime.now():%H:%M:%S} {msg}"
        print(line, flush=True)
        logf.write(line + "\n")

    bindir = os.path.join(work, "bin")
    os.makedirs(bindir, exist_ok=True)
    base_tree, base_target = os.path.join(work, "base"), os.path.join(work, "base-target")
    made_tree = False
    made = []
    builds = []  # (name, binary)
    described = {}
    try:
        if a.bin:
            if len(a.bin) < 2:
                sys.exit("compare.py: --bin twice or more")
            for nb in a.bin:
                name, _, path = nb.partition("=")
                dst = os.path.join(bindir, name)
                shutil.copy2(path, dst)
                made.append(dst)
                builds.append((name, dst))
                described[name] = path
        else:
            env = dict(os.environ, CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "4"))
            cmd = [a.cargo, "rustc", "--release", "-p", a.package, "--example", a.example, "--", "-C", "strip=none"]
            base_sha = git(root, "rev-parse", "--short", a.base)
            log(f"building {a.base} ({base_sha}) in {base_tree}")
            made_tree = True
            if os.path.exists(os.path.join(base_tree, ".git")):
                # A worktree an earlier run kept (--keep-base): its build is
                # reused, only what BASE changed built again.
                subprocess.run(["git", "-C", base_tree, "checkout", "--detach", "--force", base_sha], check=True)
            else:
                if os.path.exists(base_tree):
                    shutil.rmtree(base_tree)
                subprocess.run(["git", "worktree", "prune"], cwd=root)
                subprocess.run(["git", "worktree", "add", "--detach", base_tree, a.base], cwd=root, check=True)
            subprocess.run(cmd, cwd=base_tree, env=dict(env, CARGO_TARGET_DIR=base_target), check=True)
            head = git(root, "rev-parse", "--short", "HEAD")
            dirty = " with uncommitted changes" if git(root, "status", "--porcelain", "--untracked-files=no") else ""
            log(f"building this tree ({head}{dirty})")
            subprocess.run(cmd, cwd=root, env=env, check=True)
            target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(root, "target")
            for name, src, what in (("base", os.path.join(base_target, "release", "examples", a.example),
                                     f"{a.base} {base_sha}"),
                                    ("head", os.path.join(target, "release", "examples", a.example),
                                     f"this tree, {head}{dirty}")):
                dst = os.path.join(bindir, name)
                shutil.copy2(src, dst)
                made.append(dst)
                builds.append((name, dst))
                described[name] = what
            if not a.keep_base:
                subprocess.run(["git", "worktree", "remove", "--force", base_tree], cwd=root)
                shutil.rmtree(base_target, ignore_errors=True)
                log("removed BASE's worktree and build")
            made_tree = False

        n = max(a.uuids, 1)
        variants = []  # (name, identity, binary)
        for name, binary in builds:
            if a.uuids:
                ids = setuuid.identities(binary, a.uuids, os.path.join(work, "ids"), name)
                for i, (path, u) in enumerate(ids):
                    made.append(path)
                    variants.append((name, i, path))
                    log(f"{name} identity {i}: {u}")
            else:
                variants.append((name, 0, binary))

        res, runs = {}, []

        def write(final=()):
            with open(out, "w") as f:
                f.write(f"# instr-compare, {datetime.datetime.now():%Y-%m-%d %H:%M}; {platform.platform()}, "
                        f"{os.cpu_count()} CPUs\n")
                for name, _ in builds:
                    f.write(f"# {name}: {described[name]}\n")
                ran = "BIN" if a.bin else f"{a.package}'s example {a.example}"
                f.write(f"# {ran} MODE {' '.join(extra)}; {a.uuids} identities a build, "
                        f"{a.rounds} rounds; a run started under a load of {a.quiet}, made again past {rerun}\n\n")
                f.write("\n".join(report(builds, modes, res, a.uuids, a.rounds)) + "\n")
                if final:
                    f.write("\n".join(final) + "\n")
                f.write("\n## Runs\n\nround mode       build      id      figure  load before/after\n")
                for r in runs:
                    f.write(f"{r[0]:5} {r[1]:10} {r[2]:10} {r[3]:2} {r[4]:11.1f}  {r[5]:5.2f}/{r[6]:5.2f}\n")

        log(f"{len(variants)} binaries, {a.rounds} rounds, modes {modes}; to {out}")
        for rnd in range(a.rounds):
            k = rnd % len(variants)
            for mode in modes:
                for name, i, binary in variants[k:] + variants[:k]:
                    stdout, l0, l1 = quiet_run([binary, mode, *extra], a.quiet, rerun, log,
                                               what=f"{name} #{i} {mode}")
                    m = pattern.search(stdout)
                    if not m:
                        sys.exit(f"compare.py: {name} {mode} printed nothing {a.pattern!r} reads:\n{stdout}")
                    v = float(m.group(1))
                    res.setdefault((mode, name, i), []).append(v)
                    runs.append((rnd, mode, name, i, v, l0, l1))
                    write()
            log(f"round {rnd + 1} of {a.rounds} done")

        final = []
        if a.trace:
            tr = tracing()
            tdir = os.path.join(work, "traces")
            os.makedirs(tdir, exist_ok=True)
            paths = {}
            for mode in modes:
                for name, binary in builds:
                    p = paths[(mode, name)] = os.path.join(tdir, f"{name}-{mode}.trace")
                    if os.path.exists(p):
                        os.remove(p)
                    log(f"tracing the {a.hit}th call of {a.trace} in {name} {mode}")
                    t = subprocess.run([sys.executable, os.path.join(HERE, "trace.py"), "run", "--function",
                                        a.trace, "--hit", str(a.hit), "--out", p, binary, mode, *extra],
                                       capture_output=True, text=True)
                    if t.returncode != 0 or not os.path.exists(p):
                        log(f"the trace of {name} {mode} failed: {t.stderr.strip()[-2000:]}")
                first = builds[0][0]
                for name, _ in builds[1:]:
                    if not (os.path.exists(paths[(mode, first)]) and os.path.exists(paths[(mode, name)])):
                        continue
                    final.append(f"## {mode}: the {a.hit}th call of {a.trace}, {name} (B) against {first} (A), "
                                 f"traced\n")
                    final.extend(tr.diff(paths[(mode, first)], paths[(mode, name)], top=25))
                    final.append("")
        write(final)
        with open(out) as f:
            summary = f.read().split("\n## Runs")[0]
        print("\n" + summary)
        log(f"done: {out}")
    finally:
        if made_tree and not a.keep_base:
            subprocess.run(["git", "worktree", "remove", "--force", base_tree], cwd=root)
            shutil.rmtree(base_target, ignore_errors=True)
        if not a.keep:
            for p in made:
                if os.path.exists(p):
                    os.remove(p)
            for d in (os.path.join(work, "ids"), bindir):
                if os.path.isdir(d) and not os.listdir(d):
                    os.rmdir(d)


if __name__ == "__main__":
    main()
