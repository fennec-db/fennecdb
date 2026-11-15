#!/usr/bin/env python3
"""A pull request held to its base in instructions and heap bytes an op:
CI's `perf (instructions)` job and `make perf-gate`
(docs/design/benchmarks.md#the-gate).

The workloads are crates/fennec-http/examples/gate.rs's: one program, built
in this tree and in BASE's -- this tree's source copied in, so both sides
run the same work -- for the static musl build that ships, in the release
profile. Each workload runs RUNS (5) times a build under callgrind, its
setup uncounted: the program asks callgrind to count as its loop starts and
to stop as it ends. A run's figure is its instructions with the allocator's
calls priced at the base's cost a call (below), and a build's figure for a
workload the median of its runs'. Beside it the program tallies what its
loop asks of the allocator -- the calls, and the bytes they ask for -- and a
build's bytes are the median of its runs'. A workload whose figure or bytes
in this tree are THRESHOLD (1%) or more above BASE's fails the gate, unless
the pull request adds an entry to PERF_ACCEPTED below naming it, what it
lets through, the most and why. So does a workload the base's program runs
that this tree's dropped, and one the base's program has that the base's
build did not run: neither is held to nothing.

Why a median: std's hash tables draw their keys at random in each process
(`RandomState`), and a hundred or so sit on the engine's paths. Whether two
keys of a small table share a probe is the draw's, so one run's count
stands some 80 instructions an op from the next -- 0.4-0.6% of a request of
14 000 -- in a few steps. The median of five takes the commonest step, and
the threshold stands above it: the control and the calibration over merged
pull requests that set it are in the design document.

Why the allocator is priced: musl's allocator takes a slot from a group of
its size class, and gives the group back once its last slot is freed, so an
op whose allocations fill and empty a group of their own pays for making
and freeing the group each time -- a few hundred instructions more, with
the same calls. Which ops do is the heap's layout, which any change to what
a program keeps moves: the calibration's two merged pull requests that the
plain count failed ran their own code within 0.34% of their base's, and
paid musl's allocator 360 to 1 170 instructions more an op (+2 to +6%) with
the same calls. So each run's instructions in the allocator's own functions
(ALLOCATOR, their self cost) are counted at what the base's runs paid a
call, the calls the program's own tally: an allocation more or fewer moves
the figure at the base's price, a group's making and freeing does not. The
plain count stays in the table beside it. No call's inclusive cost is read:
callgrind's tracking of calls and returns lost returns from musl's
allocator, charging a `malloc` with ten thousand instructions of its
caller's, and the heap-shift control moved such a figure by 18%.

Why the bytes: a call priced at the base's cost hides what grows inside it
while the calls stay the same -- an allocation past its size class, a
buffer grown by `realloc`'s copies, `calloc`'s zeroing. The bytes a loop
asks for are the code's alone, the same whatever the heap's layout (the
heap-shift control, `measure --head-leak`, holds both to that), so they are
held to the same 1%.

    python3 tools/perf_gate.py run --base origin/main     # make perf-gate
    python3 tools/perf_gate.py measure --base-bin B --head-bin H --out R.json
    python3 tools/perf_gate.py measure --base-bin B --head-bin B \\
        --head-leak 65536 --out R.json                    # the heap-shift control
    python3 tools/perf_gate.py check R.json               # this tree's entries
    python3 tools/perf_gate.py reaches --base HEAD^1      # whether a change is counted
    python3 tools/perf_gate.py edits --base HEAD^1        # the gate's own files it edits

CI counts and judges with the base's copy of this checker: the count job
writes it under tools/ -- so ROOT is the checkout -- and runs it, and the
judge job, apart, runs it again on the results, reading PERF_ACCEPTED from
the pull request's copy as data (`--accepted-from`) and the workloads the
base had from the base's own gate.rs (`--base-program`): a pull request
lowering the bar, pricing a function away, cutting the runs or rewriting
the rule is counted and judged by the rule it would replace. An entry lets
its workload through only in the pull request that adds it -- one the base
holds already (`--accepted-base`) lets nothing through -- so the list is
the history of what was let through, never removed. The owner approves
each entry, and each edit of the gate's own files, which `edits` names.

`run` and `measure` count with valgrind, which runs on Linux: elsewhere they
say so and exit 0 (on macOS, in Docker: docs/design/benchmarks.md#the-gate).
`check`, `reaches` and `edits` run anywhere. The I/O counts (P50, under
strace) are another metric of the same results file, to come: `metric`
names what a file holds.
"""

import argparse
import ast
import concurrent.futures
import json
import math
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
PROGRAM = "crates/fennec-http/examples/gate.rs"
CHECKER = "tools/perf_gate.py"

# A workload this much or more above its base's figure, or its bytes, fails:
# +1% after the calibration (docs/design/benchmarks.md#the-gate). A fraction.
THRESHOLD = 0.01
# Runs a build of each workload: the figure is their median.
RUNS = 5
# The allocator's own functions, as the static musl build's profile names
# them: the entry points, musl's mallocng behind them, its lock, and the
# system calls it makes and frees its groups with -- mmap, munmap, madvise,
# mremap, brk and the wrappers they share. Their instructions -- each
# function's self cost, which callgrind charges by where an instruction
# is, never by its tracking of calls -- are the allocator's, priced at the
# base's cost a call, the calls gate.rs's own tally. The program's own
# system calls pass through the same wrappers, a few instructions each,
# priced with them: the gate counts no system call's work anyway. memcpy
# and memset, which realloc and calloc call and the program does too,
# count as they ran. A name missing here counts as the program's and moves
# with the heap, which the heap-shift control shows. The count runs the
# base's copy of this list, so a name added here prices nothing away in
# the pull request that adds it.
ALLOCATOR = frozenset({
    "malloc", "free", "realloc", "calloc", "aligned_alloc", "posix_memalign", "memalign",
    "malloc_usable_size", "__libc_malloc", "__libc_malloc_impl", "__libc_free", "__libc_realloc",
    "__libc_calloc", "alloc_slot", "alloc_group", "try_avail", "nontrivial_free", "__malloc_alloc_meta",
    "__malloc_donate", "get_meta", "enframe", "size_to_class", "__lock", "__unlock",
    "mmap", "__mmap", "munmap", "__munmap", "madvise", "__madvise", "mremap", "__mremap", "brk",
    "__vm_wait", "__vm_lock", "__vm_unlock", "__syscall_ret", "__errno_location",
})
# A replica's sync (`sync-apply`), which only the native library builds
# otherwise, asked for on the command line: a base whose fennec-http
# manifest predates its dev-dependency builds the program too.
FEATURES = "fennec-abi/sync"
# What no build of the program reads: a change whose every path is under
# one of these, or is Markdown, is the base's program to the byte. Anything
# else is counted -- the root manifest's profiles, .cargo/, the lock, the
# toolchain, a crate, a tool, a workflow -- so a place a build reads is
# counted from the day it appears, never once someone lists it.
UNREAD = ("site/", "docs/", "integrations/", "web/", "studio/", "deploy/")
# The workflow's jobs that count and judge: an edit to one is named for the
# owner (`edits`), compared by their lines, comments and blank ones aside.
WORKFLOW = ".github/workflows/ci.yml"
JOBS = ("perf-count", "perf")
# What an entry lets through: the figure (instructions an op, the
# allocator priced), or the heap bytes an op.
METRICS = ("instructions", "bytes")

# Every workload a pull request made slower on purpose, in the order they
# went out, never removed: the history of what the gate let through. An
# entry lets its workload through only in the pull request that adds it, up
# to `most`, the measured delta with room for the merge queue's rerun; a
# later regression of the same workload needs an entry of its own. `metric`
# says what it lets through, "bytes" for the heap's bytes and the figure
# where it is left out; one entry a metric. `reason` says what the cost
# buys and how it was measured (`make perf-gate`, `make instr-compare`'s
# figure on macOS). The owner approves an entry before its pull request
# merges. A literal: the base's checker reads it from a pull request's
# copy as data, never running it (`--accepted-from`).
#
#   {"workload": "http-put-keyed", "most": "+1.8%",
#    "reason": "a key's row now holds its request's digest, which a replay
#               checks: +260 instructions a keyed put (make perf-gate),
#               +1.5% by make instr-compare MODES=keyed on an M1"}
PERF_ACCEPTED = []

# What an entry must say, what it may, and how long its reason is at the
# least: a written justification, not a word.
ENTRY_KEYS = {"workload", "most", "reason"}
ENTRY_OPTIONAL = {"metric"}
REASON_AT_LEAST = 40


# ------------------------------------------------------------------ entries


def accepted_text(text, where):
    """PERF_ACCEPTED as the checker `text` writes it, read as data -- a
    literal, never run: what the base's copy holds a pull request's entries
    by."""
    tree = ast.parse(text, where)
    for node in tree.body:
        targets = node.targets if isinstance(node, ast.Assign) else [getattr(node, "target", None)]
        if any(isinstance(t, ast.Name) and t.id == "PERF_ACCEPTED" for t in targets):
            return ast.literal_eval(node.value)
    raise ValueError(f"{where} assigns no PERF_ACCEPTED")


def accepted_in(path):
    """PERF_ACCEPTED as the checker at `path` writes it (`accepted_text`)."""
    with open(path, encoding="utf-8") as f:
        return accepted_text(f.read(), path)


def percent(text):
    """`+1.8%` as 0.018; anything else a ValueError."""
    m = re.fullmatch(r"\+(\d+(?:\.\d+)?)%", str(text).strip())
    if not m:
        raise ValueError(f"`most` is a percentage above the base, as \"+1.8%\": {text!r}")
    return float(m.group(1)) / 100


def metric_of(e):
    """What entry `e` lets through: its `metric`, the figure's where none."""
    return e.get("metric", "instructions")


def checked(entries):
    """`entries` held to the form an entry takes: each a dict of ENTRY_KEYS
    and at most ENTRY_OPTIONAL, a workload's name, a metric, a `most` above
    the threshold and a reason in words. A malformed entry is the gate's
    failure, never one passed over."""
    if not isinstance(entries, list):
        raise ValueError("PERF_ACCEPTED is a list of entries")
    for e in entries:
        if not isinstance(e, dict) or not ENTRY_KEYS <= set(e) <= ENTRY_KEYS | ENTRY_OPTIONAL:
            raise ValueError(f"an entry names {sorted(ENTRY_KEYS)}, and `metric` where it lets bytes through: {e!r}")
        if not isinstance(e["workload"], str) or not e["workload"]:
            raise ValueError(f"an entry's workload is a name: {e!r}")
        if metric_of(e) not in METRICS:
            raise ValueError(f"{e['workload']}: `metric` is one of {', '.join(METRICS)}: {e['metric']!r}")
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


def workload_names(source):
    """The workloads a gate.rs `source` lists, {name: ops}: what the base's
    own program ran, which this tree's is held to -- none where there is no
    source (a base before the program). A source whose WORKLOADS this
    cannot read whole is refused, never read as fewer."""
    if not source.strip():
        return {}
    m = re.search(r"const WORKLOADS\s*:[^=]*=\s*&\[(.*?)\n\];", source, re.S)
    if not m:
        raise ValueError("the base's gate.rs holds no WORKLOADS table this checker can read")
    body = m.group(1)
    found = re.findall(r'\(\s*"([^"]+)"\s*,\s*([0-9_]+)\s*,\s*[A-Za-z_][A-Za-z0-9_]*\s*,?\s*\)', body)
    if not found or len(found) != len(re.findall(r'\(\s*"', body)):
        raise ValueError("the base's gate.rs lists workloads this checker cannot read whole")
    return {name: int(ops.replace("_", "")) for name, ops in found}


# ------------------------------------------------------------------ judging


def figure(runs):
    """A build's figure for a workload: the median of its runs, an int."""
    return int(statistics.median_low(runs))


def price_of(runs):
    """The base's cost a call into the allocator over `runs` -- a side's
    [total, the allocator's instructions, calls] each -- pooled."""
    calls = sum(c for _, _, c in runs)
    return sum(a for _, a, _ in runs) / calls if calls else 0.0


def priced(runs, price):
    """Each of `runs`' instructions with what it ran in the allocator
    counted as its calls at `price` a call."""
    return [t - a + c * price for t, a, c in runs]


def figures_of(w):
    """Workload `w`'s figures (base, head): each side's runs with the
    allocator's instructions counted as its calls at the base's price, or
    as counted where the results hold no allocator's counts (a file
    written before they were kept)."""
    alloc = w.get("allocator") or {}
    if not (alloc.get("base") and alloc.get("head")):
        return figure(w["base"]), figure(w["head"])
    for side in ("base", "head"):
        if sorted(r[0] for r in alloc[side]) != sorted(w[side]) or any(len(r) != 3 for r in alloc[side]):
            raise ValueError("the allocator's counts are not those of the runs they stand beside")
    price = price_of(alloc["base"])
    return figure(priced(alloc["base"], price)), figure(priced(alloc["head"], price))


def heap_figures(name, w):
    """Workload `name`'s heap tallies, each side's median: {"calls": (base,
    head), "bytes": (base, head), "delta": the bytes' change, a fraction}.
    None for results that hold no tallies (a file written before they were
    kept); one side's missing, or tallies not of the runs beside them, are
    refused: where they were counted, the bytes are judged."""
    heap = w.get("heap")
    if not heap:
        return None
    base, head = heap.get("base") or [], heap.get("head") or []
    if len(base) != len(w["base"]) or len(head) != len(w["head"]):
        raise ValueError(f"{name}: the heap's tallies are not those of the runs they stand beside")

    def med(runs, key):
        return int(statistics.median_low(r[key] for r in runs))

    bb, hb = med(base, "bytes"), med(head, "bytes")
    delta = hb / bb - 1 if bb else (0.0 if not hb else math.inf)
    return {"calls": (med(base, "calls"), med(head, "calls")), "bytes": (bb, hb), "delta": delta}


def judge(results, entries=(), threshold=THRESHOLD, base_names=None):
    """Each workload of `results` judged: (rows, failed), a row (name, ops,
    base, head, delta, verdict, counted, heap) -- base and head figures for
    the whole loop, the allocator's instructions counted as its calls at the
    base's cost a call, delta their change, a fraction or None, counted the
    plain count's, and
    heap the allocator's tallies (`heap_figures`) or None. `entries` are
    the pull request's own; `base_names` the workloads the base's own
    program lists ({name: ops}), or None where unknown: a workload of them
    this tree dropped fails, and so does one of them the base's build did
    not run, where a workload new to this tree is held to nothing."""
    if results.get("metric") != "instructions":
        raise ValueError(f"these results hold {results.get('metric')!r}, not instructions")
    names = set(results["workloads"])
    known = names | set(base_names or ())
    for e in entries:
        if e["workload"] not in known:
            raise ValueError(f"PERF_ACCEPTED names {e['workload']!r}, which no workload is")
    allowed = {}
    for e in entries:
        key = (e["workload"], metric_of(e))
        allowed[key] = max(allowed.get(key, 0.0), percent(e["most"]))

    def held(name, metric, delta, failures, accepted):
        if delta < threshold:
            return
        most = allowed.get((name, metric))
        if most is None:
            failures.append(f"{metric} +{threshold * 100:g}% or more")
        elif delta <= most:
            accepted.append(f"{metric} up to +{most * 100:.1f}%")
        else:
            failures.append(f"{metric} past its PERF_ACCEPTED entry's +{most * 100:.1f}%")

    rows, failed = [], False
    for name, ops in sorted((base_names or {}).items()):
        if name not in names:
            rows.append((name, ops, None, None, None, "FAILED: dropped -- the base's program runs it, this tree's does not", None, None))
            failed = True
    for name in sorted(names):
        w = results["workloads"][name]
        base, head = w.get("base") or [], w.get("head") or []
        if not head:
            rows.append((name, w["ops"], figure(base) if base else None, None, None, "FAILED: this tree's build did not run it", None, None))
            failed = True
            continue
        if not base:
            had = name in base_names if base_names is not None else bool(w.get("base_error"))
            if had:
                # The base had it: a run that crashed, or a loop the change
                # rewrote into one the base cannot run, is no figure to
                # pass on -- never held to nothing.
                rows.append((name, w["ops"], None, figure(head), None, "FAILED: the base's build did not run it", None, None))
                failed = True
            else:
                rows.append((name, w["ops"], None, figure(head), None, "new: held to nothing", None, None))
            continue
        if min(len(base), len(head)) < RUNS:
            # Fewer runs are a median the seeds move: a figure the base's
            # checker does not take, whatever the pull request's counting.
            rows.append(
                (name, w["ops"], figure(base), figure(head), None, f"FAILED: fewer than {RUNS} runs a side", None, None)
            )
            failed = True
            continue
        counted = figure(head) / figure(base) - 1
        b, h = figures_of(w)
        delta = h / b - 1
        heap = heap_figures(name, w)
        failures, accepted = [], []
        held(name, "instructions", delta, failures, accepted)
        if heap:
            held(name, "bytes", heap["delta"], failures, accepted)
        if failures:
            verdict = "FAILED: " + "; ".join(failures)
            failed = True
        elif accepted:
            verdict = "accepted (PERF_ACCEPTED): " + "; ".join(accepted)
        else:
            verdict = "ok"
        rows.append((name, w["ops"], b, h, delta, verdict, counted, heap))
    rows.sort(key=lambda r: r[0])
    return rows, failed


def spread(runs):
    """A side's runs' range against their median, a fraction."""
    m = statistics.median(runs)
    return (max(runs) - min(runs)) / m if m else 0.0


def table(results, rows):
    """The judgement as Markdown: a step summary's, a pull request's."""
    out = [
        f"### Instructions and heap bytes an op, base against this tree (median of {results.get('runs', '?')} callgrind runs)",
        "",
    ]
    programs = results.get("programs")
    if programs == "each tree's own":
        out += [
            "**Each tree ran its own program.** This tree's gate.rs did not build against the base's crates, "
            "so the base ran its own copy, its workloads held to these by name: a loop the change "
            "rewrote is held to the base's other loop.",
            "",
        ]
    elif programs:
        out += [f"The program: {programs} `{PROGRAM}`, built on both sides.", ""]
    control = results.get("control")
    if control:
        out += [
            f"**A control**: one build on both sides, this tree's runs with {control['head_leak']} bytes "
            "leaked before the setup (`FENNEC_GATE_LEAK`): the heap's layout moved, the code did not.",
            "",
        ]
    out += [
        "| workload | ops | base | this tree | delta | as counted | calls/op | bytes/op | bytes delta | runs' spread | verdict |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for name, ops, b, h, delta, verdict, counted, heap in rows:
        w = results["workloads"].get(name, {})
        per = lambda x, d=0: f"{x / ops:,.{d}f}" if x is not None else "-"
        pct = lambda x: f"{x * 100:+.2f}%" if x is not None else "-"
        calls = f"{per(heap['calls'][0], 1)} → {per(heap['calls'][1], 1)}" if heap else "-"
        nbytes = f"{per(heap['bytes'][0])} → {per(heap['bytes'][1])}" if heap else "-"
        sp = " / ".join(f"{spread(w[s]) * 100:.2f}%" for s in ("base", "head") if w.get(s)) or "-"
        out.append(
            f"| {name} | {ops} | {per(b)} | {per(h)} | {pct(delta)} | {pct(counted)} | {calls} | {nbytes} "
            f"| {pct(heap['delta'] if heap else None)} | {sp} | {verdict} |"
        )
    out.append("")
    out.append(
        f"A workload fails at +{THRESHOLD * 100:g}% or more in instructions or in heap bytes an op, unless "
        "the pull request adds a PERF_ACCEPTED entry (tools/perf_gate.py) naming it, which the owner "
        "approves: docs/design/benchmarks.md#the-gate. Its figures count each call into the allocator "
        "at what the base's runs paid a call; *as counted* is the plain count's delta, the difference "
        "musl's groups made: an allocation's cost moves with the heap's layout, its calls and bytes "
        "with the code. A workload the base's program runs fails when this tree dropped it or the "
        "base's build did not run it."
    )
    errors = [(n, w["base_error"]) for n, w in sorted(results["workloads"].items()) if w.get("base_error")]
    if errors:
        out += ["", "<details><summary>What the base's build said of the workloads it did not run</summary>", ""]
        for n, e in errors:
            tail = " / ".join(line.strip() for line in e.strip().splitlines()[-3:] if line.strip())
            out.append(f"- `{n}`: {tail.replace('`', chr(39))}")
        out += ["", "</details>"]
    return "\n".join(out) + "\n"


def check(args):
    try:
        with open(args.results, encoding="utf-8") as f:
            results = json.load(f)
        head = checked(accepted_in(args.accepted_from or os.path.abspath(__file__)))
        base = checked(accepted_in(args.accepted_base)) if args.accepted_base else []
        names = None
        if args.base_program:
            with open(args.base_program, encoding="utf-8") as f:
                names = workload_names(f.read())
        rows, failed = judge(results, added(head, base), base_names=names)
        text = table(results, rows)
    except ValueError as e:
        # Said where the pull request's author reads the gate's table.
        text, failed = f"### The perf gate refused its input\n\n{e}\n", True
    print(text)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as f:
            f.write(text)
    return 1 if failed else 0


# ------------------------------------------------------------------ a change


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd or ROOT, capture_output=True, text=True, check=True).stdout.strip()


def show(ref, path):
    """`path` at `ref`, or "" where `ref` has no such file; a ref that is
    no commit is refused."""
    git("rev-parse", "--verify", f"{ref}^{{commit}}")
    p = subprocess.run(["git", "show", f"{ref}:{path}"], cwd=ROOT, capture_output=True, text=True)
    return p.stdout if p.returncode == 0 else ""


def diff_paths(base, head):
    """Every path a change from `base` to `head` touches, a rename as both
    of its paths."""
    out = git("diff", "--name-only", "--no-renames", base, head)
    return [p for p in out.splitlines() if p.strip()]


def reaches(paths):
    """Whether a change of `paths` may reach what the gate builds: anything
    but the places UNREAD names and Markdown does, and so does a change
    whose paths could not be read (none)."""
    if not paths:
        return True
    return any(not (p.startswith(UNREAD) or p.endswith(".md")) for p in paths)


def jobs_of(text, ids=JOBS):
    """The jobs `ids` of a workflow's `text`, each its lines but comments
    and blank ones, joined: {id: lines}, a job the text lacks absent."""
    out, job, in_jobs = {}, None, False
    for line in text.splitlines():
        if re.match(r"^jobs:\s*(#.*)?$", line):
            in_jobs = True
            continue
        if line[:1] not in ("", " ", "#"):
            in_jobs, job = False, None
            continue
        if not in_jobs:
            continue
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*(#.*)?$", line)
        if m:
            job = m.group(1) if m.group(1) in ids else None
            if job:
                out[job] = []
            continue
        if job and line.strip() and not line.lstrip().startswith("#"):
            out[job].append(line.rstrip())
    return {k: "\n".join(v) for k, v in out.items()}


def gate_edits(base, head):
    """What a change from `base` to `head` does to the gate's own files, a
    line each for the owner: the program or the checker edited, a job of
    the workflow's that counts or judges edited, and every PERF_ACCEPTED
    entry added, edited or removed."""
    changed = set(diff_paths(base, head))
    notes = [
        f"`{path}` is edited: one of the gate's own files, which the owner approves"
        for path in (PROGRAM, CHECKER)
        if path in changed
    ]
    was, now = jobs_of(show(base, WORKFLOW)), jobs_of(show(head, WORKFLOW))
    for job in JOBS:
        if was.get(job) != now.get(job):
            how = "added" if job not in was else "removed or renamed" if job not in now else "edited"
            notes.append(f"the `{job}` job of `{WORKFLOW}` is {how}: the gate's own job, which the owner approves")
    old, new = show(base, CHECKER), show(head, CHECKER)
    old = accepted_text(old, f"{base}:{CHECKER}") if old else []
    new = accepted_text(new, f"{head}:{CHECKER}") if new else []
    for e in added(new, old):
        notes.append(
            f"PERF_ACCEPTED gains an entry: `{e.get('workload')}`, {e.get('metric', 'instructions')} "
            f"up to {e.get('most')} -- {e.get('reason')}"
        )
    for e in added(old, new):
        notes.append(
            f"PERF_ACCEPTED loses an entry, the list's history cut or an entry edited: `{e.get('workload')}` "
            f"up to {e.get('most')}"
        )
    return notes


def annotation(text):
    """`text` as a workflow command's message: one line, its `%` escaped."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def edits(args):
    notes = gate_edits(args.base, args.head)
    for n in notes:
        print(f"::warning title=perf gate::{annotation(n)}")
    if notes:
        text = "### The gate's own files\n\nThis change edits what counts and judges it, or what it lets through: " \
               "the owner approves it before it merges (docs/design/benchmarks.md#the-gate).\n\n" + \
               "".join(f"- {n}\n" for n in notes)
    else:
        text = "The change edits none of the gate's own files and adds no PERF_ACCEPTED entry.\n"
    print(text)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as f:
            f.write(text + "\n")
    return 0


def reach(args):
    built = reaches(diff_paths(args.base, args.head))
    line = f"built={'true' if built else 'false'}"
    print(line)
    if args.output:
        with open(args.output, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    if not built and args.summary:
        with open(args.summary, "a", encoding="utf-8") as f:
            f.write(
                "Nothing to count: every path the change touches is the site's, the docs', an "
                "integration's, the browser client's, the studio's, deploy/'s or Markdown, which no "
                "build of the gate's program reads.\n"
            )
    return 0


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


def allocator(path):
    """The instructions a callgrind output file counts in the allocator's
    own functions (ALLOCATOR): their self cost, the cost lines under each
    `fn=` but those after a `calls=`, which are a call's inclusive cost. A
    name is read once given, by its compressed id after, and a recursion's
    level (`'2`) as its function."""
    names, total = {}, 0
    fn, after_call = None, False

    def name(text):
        m = re.match(r"\((\d+)\)(?:\s+(.*))?$", text)
        if not m:
            return text
        if m.group(2) is not None:
            names[m.group(1)] = m.group(2)
        return names.get(m.group(1))

    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if line.startswith("fn="):
                fn = re.sub(r"'\d+$", "", name(line[3:]) or "")
            elif line.startswith("cfn="):
                name(line[4:])
            elif line.startswith("calls="):
                after_call = True
            elif line[:1] in tuple("0123456789+-*"):
                if after_call:
                    after_call = False
                elif fn in ALLOCATOR:
                    total += int(line.split()[1])
    return total


# The program's tally of what its loop asked of the allocator (gate.rs's
# `Counting`), a line of its output.
HEAP = re.compile(r"^heap: calls=(\d+) bytes=(\d+) zeroed=(\d+) grown=(\d+)$", re.M)


def heap_of(output):
    """The `heap:` line of a run's output, {calls, bytes, zeroed, grown}, or
    None where it printed none."""
    m = HEAP.search(output)
    if not m:
        return None
    return dict(zip(("calls", "bytes", "zeroed", "grown"), map(int, m.groups())))


def one_run(binary, name, ops, scratch, leak=None):
    """Runs workload `name` once under callgrind: the instructions its loop
    counted, those of them the allocator's (`allocator`), its heap's tally
    (`heap_of`), and the profile callgrind wrote, by function. Nothing
    counted means valgrind passed the program's requests by -- a sequence of
    `callgrind::request` not valgrind's -- and is refused; so is a run that
    tallied no heap, whose bytes could not be judged. With `leak`, the
    program holds that many bytes before its setup (the heap-shift
    control), and says so."""
    fd, out = tempfile.mkstemp(prefix=f"{name}.", suffix=".callgrind", dir=scratch)
    os.close(fd)
    cmd = [
        "valgrind", "--tool=callgrind", "--instr-atstart=no", "--collect-atstart=no",
        "--dump-instr=no", "--dump-line=no", f"--callgrind-out-file={out}", binary, name,
    ]
    env = {k: v for k, v in os.environ.items() if k != "FENNEC_GATE_LEAK"}
    if leak:
        env["FENNEC_GATE_LEAK"] = str(leak)
    p = subprocess.run(cmd, capture_output=True, text=True, env=env)
    if p.returncode != 0:
        raise RuntimeError(f"{name}: exit {p.returncode}\n{p.stdout}\n{p.stderr[-3000:]}")
    if f"{name} {ops} ops" not in p.stdout:
        raise RuntimeError(f"{name}: the program did not run its {ops} ops:\n{p.stdout}")
    if leak and f"leaked {leak} bytes before the setup" not in p.stdout:
        raise RuntimeError(f"{name}: the program did not leak the {leak} bytes asked of it:\n{p.stdout}")
    heap = heap_of(p.stdout)
    if heap is None:
        raise RuntimeError(f"{name}: the program tallied no heap (gate.rs's `Counting`):\n{p.stdout}")
    n = totals(out)
    if n <= 0:
        raise RuntimeError(f"{name}: callgrind counted nothing -- were the client requests seen?")
    return n, allocator(out), heap, out


def measure_bins(base_bin, head_bin, runs, jobs, scratch, log=print, profiles=None, only=None, head_leak=None):
    """Every workload of this tree's build counted `runs` times a build, the
    runs of all of them spread over `jobs` processes, each round of every
    workload and build before the next: a count does not move with how
    many run beside it, the workloads keeping out what runs on the clock.
    With `profiles`, the profile of each side's median run of each workload
    is kept there (`<workload>.<side>.callgrind`), for `callgrind_annotate`
    to tell where a difference is; the rest are let go of. With
    `head_leak`, this tree's runs hold that many bytes before their setup:
    the heap-shift control, one build on both sides."""
    sides = {"head": dict(listed(head_bin))}
    if base_bin:
        sides["base"] = dict(listed(base_bin))
    names = sorted(n for n in sides["head"] if not only or n in only)
    work = {"head": head_bin, "base": base_bin}
    results = {"metric": "instructions", "tool": "callgrind", "runs": runs, "workloads": {}}
    if head_leak:
        results["control"] = {"head_leak": head_leak}
    failed, files = {}, {}
    for name in names:
        # `allocator`: each run's total beside its instructions in the
        # allocator and its calls, which the judgement prices at the base's
        # cost a call; `heap`, each run's tally of what it asked of the
        # allocator.
        results["workloads"][name] = {
            "ops": sides["head"][name], "base": [], "head": [],
            "allocator": {"base": [], "head": []}, "heap": {"base": [], "head": []},
        }
        if base_bin and name not in sides["base"]:
            failed[("base", name)] = f"{name}: the base's program has no such workload"
        elif base_bin and sides["base"][name] != sides["head"][name]:
            # The base's own program, where this tree's did not build there:
            # another loop, never held to another's count.
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
            pool.submit(
                one_run, work[side], name, sides["head"][name], scratch, head_leak if side == "head" else None
            ): (side, name)
            for side, name in tasks
        }
        for fut in concurrent.futures.as_completed(futures):
            side, name = futures[fut]
            try:
                n, alloc, heap, path = fut.result()
                results["workloads"][name][side].append(n)
                results["workloads"][name]["allocator"][side].append([n, alloc, heap["calls"]])
                results["workloads"][name]["heap"][side].append(heap)
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
        # Each list in run order, the totals sorted with their tallies.
        for side in ("base", "head"):
            order = sorted(range(len(w[side])), key=lambda i: w[side][i])
            w[side] = [w[side][i] for i in order]
            w["heap"][side] = [w["heap"][side][i] for i in order]
    # A workload the base did not run -- a statement only this tree knows,
    # a run that crashed -- keeps none of its base runs, and says why: the
    # judgement holds it to nothing only where the base's program had no
    # such workload (`judge`'s `base_names`).
    for (_, name), msg in failed.items():
        w = results["workloads"][name]
        w["base"], w["allocator"]["base"], w["heap"]["base"] = [], [], []
        w["base_error"] = msg[-600:]
    return results


def measure(args):
    if not counting():
        return 0
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    scratch = tempfile.mkdtemp(prefix="perf-gate-")
    try:
        results = measure_bins(
            args.base_bin, args.head_bin, args.runs, args.jobs, scratch,
            profiles=args.profiles, only=args.only.split(",") if args.only else None, head_leak=args.head_leak,
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
    cmd = [cargo, "build", "--locked", "--release", "-p", "fennec-http", "--example", "gate", "--features", FEATURES]
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
            log(f"replay {args.base} -> {args.head or 'this tree'}: the gate program does not build at the head")
            return 1
        programs = "this tree's"
        if base_bin is None and had:
            git("checkout", "--", PROGRAM, cwd=base_tree)
            base_bin = build(base_tree, args.target, os.path.join(work, "base-target"), args.cargo, log)
            programs = "each tree's own"
        if base_bin is None:
            log(f"replay {args.base} -> {args.head or 'this tree'}: the base builds no gate program, "
                "neither with this tree's nor its own: nothing to hold the head to")
            return 1
        log(f"counting {args.runs} runs a build of each workload, {args.jobs} at a time ({programs} program)")
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
    accepted_base = None
    text = show(sha, CHECKER)
    if text:
        with open(base_copy, "w", encoding="utf-8") as f:
            f.write(text)
        accepted_base = base_copy
    base_program = os.path.join(work, "base_gate.rs")
    with open(base_program, "w", encoding="utf-8") as f:
        f.write(show(sha, PROGRAM))
    ns = argparse.Namespace(
        results=out, accepted_from=os.path.abspath(__file__), accepted_base=accepted_base,
        base_program=base_program, summary=args.summary,
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
    m.add_argument("--head-leak", type=int, help="the heap-shift control: the head's runs leak this many bytes first")

    c = sub.add_parser("check", help="judge a results file")
    c.add_argument("results")
    c.add_argument("--accepted-from", help="the checker whose PERF_ACCEPTED is the pull request's (this one)")
    c.add_argument("--accepted-base", help="the checker whose PERF_ACCEPTED the base holds already")
    c.add_argument("--base-program", help="the base's own gate.rs, whose workloads this tree's are held to (empty: none)")
    c.add_argument("--summary", help="append the table to this file (a step summary)")

    h = sub.add_parser("reaches", help="whether a change reaches what the gate builds: built=true|false")
    h.add_argument("--base", required=True)
    h.add_argument("--head", default="HEAD")
    h.add_argument("--output", help="append built=... to this file ($GITHUB_OUTPUT)")
    h.add_argument("--summary", help="say so in this file where nothing is counted")

    e = sub.add_parser("edits", help="name each edit of the gate's own files and each PERF_ACCEPTED entry added")
    e.add_argument("--base", required=True)
    e.add_argument("--head", default="HEAD")
    e.add_argument("--summary", help="append the list to this file (a step summary)")

    args = ap.parse_args(argv)
    try:
        commands = {"run": run, "measure": measure, "check": check, "reaches": reach, "edits": edits}
        return commands[args.command](args)
    except (ValueError, RuntimeError, subprocess.CalledProcessError) as e:
        print(f"perf_gate: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
