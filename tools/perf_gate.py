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
    python3 tools/perf_gate.py check R.json --expect "core-put:both" \\
        --cannot-run "http-scoped-set=<its error's words>"   # a holdout pair
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

The holdout (.github/workflows/perf-holdout.yml) replays merged pairs and
knows what each must do: `--expect` names the workloads that fail and by
which metric, and the check passes only when exactly those fail -- `pass`,
when none does; a metric marked `?` may fail or not, for a figure its runs
put within their spread of the bar. A pair whose base predates what a
workload's setup asks for names it with `--cannot-run` and the words the
base's error says it in, WHY_AT_LEAST characters at the least: held to
nothing then, where a workload so named that the base ran, or did not for
another reason, or this tree ran fewer than RUNS times, fails, as does any
other the base did not run. Both judge instructions alone: the holdout
replays no I/O, and a check of I/O results given either refuses it.

The I/O a write costs, which callgrind does not see -- it counts no system
call -- is another metric of the same checker (`"metric": "io"` in a results
file of its own): each of gate.rs's IO_WORKLOADS sent by that program to a
real fennec-server of each side under `--sync always`, the server alone run
under `strace -f`, IO_RUNS (3) times a side. Counted between the server
taking the workload's connection and its taking the driver's next one: the
fsync family's calls, renames, the calls writing to files and the bytes
they wrote, and the calls of those that failed -- each to the call, so
every run of a side must count the same, and any count of this tree's that
is not the base's fails, up or down (fewer fsyncs a durable write is
durability lost), unless the pull request adds a PERF_ACCEPTED entry naming
the workload and the count (`"metric": "fsyncs"`, a `most` of `+100%` or
`+1`).

    python3 tools/perf_gate.py io --base origin/main      # make perf-gate-io
    python3 tools/perf_gate.py io-measure --base-server B --head-server H \\
        --client G --out IO.json                          # two built servers
    python3 tools/perf_gate.py check IO.json              # judged as above

`run` and `measure` count with valgrind, `io` and `io-measure` with strace,
both on Linux: elsewhere they say so and exit 0 (on macOS, in Docker:
docs/design/benchmarks.md#the-gate). `check`, `reaches` and `edits` run
anywhere.
"""

import argparse
import ast
import concurrent.futures
import http.client
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
import threading

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
# allocator priced), or the heap bytes an op; or one of an I/O workload's
# counts (IO_METRICS, below).
INSTRUCTION_METRICS = ("instructions", "bytes")
# An I/O workload's counts, each judged to the call: the fsync family's
# calls, renames, the calls writing to files, the bytes they wrote, and
# the calls of those that failed.
IO_METRICS = ("fsyncs", "renames", "writes", "written", "failed")
METRICS = INSTRUCTION_METRICS + IO_METRICS

# Every workload a pull request made slower on purpose, in the order they
# went out, never removed: the history of what the gate let through. An
# entry lets its workload through only in the pull request that adds it, up
# to `most`, the measured delta with room for the merge queue's rerun; a
# later regression of the same workload needs an entry of its own. `metric`
# says what it lets through, "bytes" for the heap's bytes and the figure
# where it is left out; one entry a metric. An I/O workload's entry names
# its count (IO_METRICS) and lets it move only in its `most`'s direction,
# by at most that share of the base's count or that many: "+100%", "+1",
# "-50%". `reason` says what the cost buys and how it was measured (`make
# perf-gate`, `make perf-gate-io`, `make instr-compare`'s figure on macOS).
# The owner approves an entry before its pull request merges. A literal:
# the base's checker reads it from a pull request's copy as data, never
# running it (`--accepted-from`).
#
#   {"workload": "http-put-keyed", "most": "+1.8%",
#    "reason": "a key's row now holds its request's digest, which a replay
#               checks: +260 instructions a keyed put (make perf-gate),
#               +1.5% by make instr-compare MODES=keyed on an M1"}
#   {"workload": "io-compact", "metric": "fsyncs", "most": "+1",
#    "reason": "the directory is synced after the rename too, or a power
#               cut could bring the old file back: one fsync a compact"}
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


def io_most(text):
    """An I/O entry's `most`: ("share", +1.0) for `+100%`, ("count", -2) for
    `-2` -- signed, the direction the count may move, never 0; anything else
    a ValueError."""
    s = str(text).strip()
    m = re.fullmatch(r"([+-])(\d+(?:\.\d+)?)(%?)", s)
    if not m or float(m.group(2)) == 0 or (not m.group(3) and "." in m.group(2)):
        raise ValueError(
            f"an I/O entry's `most` is a signed share of the base's count or a signed whole count, "
            f"as \"+100%\", \"-50%\" or \"+1\": {text!r}"
        )
    sign = 1 if m.group(1) == "+" else -1
    if m.group(3):
        return "share", sign * float(m.group(2)) / 100
    return "count", sign * int(m.group(2))


def io_lets(most, base, head):
    """Whether an I/O entry's `most` lets a count move from `base` to
    `head`: the same direction, and no further."""
    kind, v = io_most(most)
    d = head - base
    if d == 0 or (d > 0) != (v > 0):
        return False
    if kind == "count":
        return abs(d) <= abs(v)
    return base != 0 and abs(d / base) <= abs(v)


def metric_of(e):
    """What entry `e` lets through: its `metric`, the figure's where none."""
    return e.get("metric", "instructions")


def checked(entries):
    """`entries` held to the form an entry takes: each a dict of ENTRY_KEYS
    and at most ENTRY_OPTIONAL, a workload's name, a metric, a `most` above
    the threshold -- an I/O count's signed (`io_most`) -- and a reason in
    words. A malformed entry is the gate's failure, never one passed over."""
    if not isinstance(entries, list):
        raise ValueError("PERF_ACCEPTED is a list of entries")
    for e in entries:
        if not isinstance(e, dict) or not ENTRY_KEYS <= set(e) <= ENTRY_KEYS | ENTRY_OPTIONAL:
            raise ValueError(
                f"an entry names {sorted(ENTRY_KEYS)}, and `metric` where it lets bytes or an I/O count through: {e!r}"
            )
        if not isinstance(e["workload"], str) or not e["workload"]:
            raise ValueError(f"an entry's workload is a name: {e!r}")
        if metric_of(e) not in METRICS:
            raise ValueError(f"{e['workload']}: `metric` is one of {', '.join(METRICS)}: {e['metric']!r}")
        if metric_of(e) in IO_METRICS:
            # Counted to the call: any move is one, and its direction is
            # named -- fewer fsyncs a durable write is durability lost.
            io_most(e["most"])
        elif percent(e["most"]) < THRESHOLD:
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


def io_workload_names(source):
    """The I/O workloads a gate.rs `source` lists, {name: requests}: none
    where it has no IO_WORKLOADS table -- a base before the I/O gate. A
    table this cannot read whole is refused, never read as fewer."""
    m = re.search(r"const IO_WORKLOADS\s*:[^=]*=\s*&\[(.*?)\n\];", source, re.S)
    if not m:
        return {}
    body = m.group(1)
    ident = r"[A-Za-z_][A-Za-z0-9_]*"
    found = re.findall(rf'\(\s*"([^"]+)"\s*,\s*([0-9_]+)\s*,\s*{ident}\s*,\s*{ident}\s*,?\s*\)', body)
    if not found or len(found) != len(re.findall(r'\(\s*"', body)):
        raise ValueError("the base's gate.rs lists I/O workloads this checker cannot read whole")
    return {name: int(n.replace("_", "")) for name, n in found}


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


def judge(results, entries=(), threshold=THRESHOLD, base_names=None, cannot_run=None):
    """Each workload of `results` judged: (rows, failed), a row (name, ops,
    base, head, delta, verdict, counted, heap) -- base and head figures for
    the whole loop, the allocator's instructions counted as its calls at the
    base's cost a call, delta their change, a fraction or None, counted the
    plain count's, and
    heap the allocator's tallies (`heap_figures`) or None. `entries` are
    the pull request's own; `base_names` the workloads the base's own
    program lists ({name: ops}), or None where unknown: a workload of them
    this tree dropped fails, and so does one of them the base's build did
    not run, where a workload new to this tree is held to nothing.
    `cannot_run`, {name: why}, the holdout's: workloads a replayed base is
    known not to run, the words `why` in its error -- held to nothing where
    the base did not run one and said so and this tree ran it RUNS times,
    failed where the base ran one or did not for another reason, or this
    tree ran it fewer times."""
    if results.get("metric") != "instructions":
        raise ValueError(f"these results hold {results.get('metric')!r}, not instructions")
    # An I/O workload's entries are the I/O judgement's (`judge_io`).
    entries = [e for e in entries if metric_of(e) in INSTRUCTION_METRICS]
    names = set(results["workloads"])
    known = names | set(base_names or ())
    for e in entries:
        if e["workload"] not in known:
            raise ValueError(f"PERF_ACCEPTED names {e['workload']!r}, which no workload is")
    cannot_run = cannot_run or {}
    for name in sorted(cannot_run):
        if name not in names:
            raise ValueError(f"{name!r} is named as a workload the base cannot run, which no workload is")
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
        why = cannot_run.get(name)
        if why is not None:
            # A base older than what the workload's setup asks for, said
            # by the error it gave: one it ran, or did not for another
            # reason -- a crash -- is no workload it cannot run.
            if base:
                verdict = f"FAILED: named as one the base cannot run ({why}), and the base ran it"
            elif why not in (w.get("base_error") or ""):
                verdict = f"FAILED: the base did not run it, its error not the one named ({why})"
            elif len(head) < RUNS:
                # Held to nothing on its base's side, it is held to its
                # count on this tree's: a build that ran it fewer times
                # crashed in a run, which no other row would say.
                verdict = f"FAILED: the base cannot run it, as named, and this tree's ran it fewer than {RUNS} times"
            else:
                verdict = None
            if verdict:
                rows.append((name, w["ops"], figure(base) if base else None, figure(head), None, verdict, None, None))
                failed = True
            else:
                rows.append((name, w["ops"], None, figure(head), None, f"the base cannot run it, as named: {why}", None, None))
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


# ------------------------------------------------------------------ the holdout
#
# The holdout replays pairs in instructions and bytes alone: its metrics
# are INSTRUCTION_METRICS, `both` the two of them, never an I/O count.


def failing(rows):
    """What failed in `rows` (`judge`'s), {name: kinds}: the metrics a
    workload failed by, `instructions` and `bytes`, or `other` for a failure
    no metric is -- dropped, not run on a side, too few runs, named as one
    the base cannot run and run."""
    found = {}
    for name, *_, verdict, _counted, _heap in rows:
        if verdict.startswith("FAILED: "):
            parts = verdict[len("FAILED: "):].split("; ")
            found[name] = {p.split(" ", 1)[0] if p.split(" ", 1)[0] in INSTRUCTION_METRICS else "other" for p in parts}
    return found


def failures_said(failures):
    """{name: kinds} as a line: "`a` both, `b` bytes", or "none"."""
    said = lambda k: "both" if k == set(INSTRUCTION_METRICS) else ", ".join(sorted(k))
    return ", ".join(f"`{n}` {said(k)}" for n, k in sorted(failures.items())) or "none"


# A cannot-run's words, the least: enough to tell the base's error from
# another -- a crash, a panic -- where a word or two is in every error.
WHY_AT_LEAST = 16


def expected_of(text):
    """`--expect`'s text read, {name: (must, may)}: `pass`, no workload
    failing, or each workload named as `name:metrics` -- `instructions`,
    `bytes` or `both`, two joined by `+` -- apart by spaces or commas, the
    metrics it must fail by in `must`, those marked `?` in `may`: they may
    fail or not, for a figure its runs put within their spread of the bar
    (`a:bytes+instructions?`, `a:instructions?`). Anything else is refused:
    an expectation left empty is no `pass`, a metric named twice no
    expectation."""
    text = text.strip()
    if text == "pass":
        return {}
    usage = (
        "an expectation is `pass` or `<workload>:<metric>[+<metric>]`, "
        "each `instructions`, `bytes` or `both`, `?` after one that may fail"
    )
    out = {}
    for item in re.split(r"[\s,]+", text) if text else []:
        name, _, metrics = item.rpartition(":")
        if not name:
            raise ValueError(f"{usage}: {item!r}")
        must, may = set(), set()
        for term in metrics.split("+"):
            optional = term.endswith("?")
            word = term[:-1] if optional else term
            kinds = set(INSTRUCTION_METRICS) if word == "both" else {word}
            if not kinds <= set(INSTRUCTION_METRICS):
                raise ValueError(f"{usage}: {item!r}")
            if kinds & (must | may):
                raise ValueError(f"{item!r} names a metric twice")
            (may if optional else must).update(kinds)
        if name in out:
            raise ValueError(f"{name!r} is expected twice")
        out[name] = (frozenset(must), frozenset(may))
    if not out:
        raise ValueError(f"{usage}: the workloads that fail, or `pass`")
    return out


def expected_said(expected):
    """{name: (must, may)} as a line, each as `--expect` spells it:
    "`a` both, `b` bytes+instructions?", or "none"."""

    def said(kinds, mark):
        return ["both" + mark] if kinds == set(INSTRUCTION_METRICS) else [k + mark for k in sorted(kinds)]

    return ", ".join(
        f"`{n}` {'+'.join(said(must, '') + said(may, '?'))}" for n, (must, may) in sorted(expected.items())
    ) or "none"


def cannot_run_of(text):
    """`--cannot-run`'s text read, {name: why}: `name[,name...]=why`, the
    workloads a replayed base cannot run and the words its error says why
    in, WHY_AT_LEAST characters at the least; none where the text is
    empty."""
    text = text.strip()
    if not text:
        return {}
    names, _, why = text.partition("=")
    why = why.strip()
    listed = [n for n in re.split(r"[\s,]+", names.strip()) if n]
    if not why or not listed:
        raise ValueError(f"a workload the base cannot run is named `<workload>[,<workload>...]=<its error's words>`: {text!r}")
    if len(why) < WHY_AT_LEAST:
        raise ValueError(
            f"the words of the base's error tell it from another error, {WHY_AT_LEAST} characters at the least: {why!r}"
        )
    return {n: why for n in listed}


def unexpected(rows, expected):
    """How what failed in `rows` differs from `expected` (`expected_of`): a
    line for each workload that failed unnamed, by a metric not named, for
    no metric's reason, or that was named and did not fail by every metric
    it must. None when each failed by those it must, and by none but those
    it may besides."""
    found, verdicts = failing(rows), {r[0]: r[5] for r in rows}
    said = lambda k: " and ".join(sorted(k)) if k else "nothing"
    out = []
    for n in sorted(set(found) | set(expected)):
        got = found.get(n, set())
        must, may = expected.get(n, (frozenset(), frozenset()))
        if "other" in got or not must <= got <= must | may:
            how = verdicts[n] if "other" in got else f"failed by {said(got)}"
            want = said(must) + (f" (maybe {said(may)})" if may else "")
            out.append(f"`{n}`: expected to fail by {want}, {how}")
    return out


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


def io_side(runs):
    """A side's runs of an I/O workload: the one run they agree on, and the
    counts they differ in. Counted to the call, a count that moves from one
    run to the next is the clock's or a draw's, and no count to judge by."""
    first = runs[0]
    return first, [k for k in IO_METRICS + IO_SENT if any(r.get(k) != first.get(k) for r in runs)]


def io_moved(b, h):
    """A count's move from `b` to `h`, as the verdict says it."""
    share = f"{(h - b) / b * 100:+.1f}%" if b else "from none"
    return f"{b:,} → {h:,} ({share})"


def judge_io(results, entries=(), base_names=None):
    """Each I/O workload of `results` judged: (rows, failed), a row (name,
    requests, base, head, verdict, moved) -- base and head the run each
    side's runs agree on, or None, and moved {count: (base, head)} for each
    count of IO_METRICS that is not the base's. Any such move fails unless
    one of `entries`, the pull request's own, names the workload and the
    count and lets it so far in its direction (`io_lets`). A side whose
    runs differ fails, and so do sides sent different requests. As the
    instructions' (`judge`): a workload of `base_names` -- the base's own
    program's IO_WORKLOADS -- this tree dropped, or the base's build did not
    run, fails, where one new to this tree is held to nothing."""
    if results.get("metric") != "io":
        raise ValueError(f"these results hold {results.get('metric')!r}, not I/O counts")
    # The instructions' entries are `judge`'s.
    entries = [e for e in entries if metric_of(e) in IO_METRICS]
    names = set(results["workloads"])
    known = names | set(base_names or ())
    for e in entries:
        if e["workload"] not in known:
            raise ValueError(f"PERF_ACCEPTED names {e['workload']!r}, which no I/O workload is")
    allowed = {}
    for e in entries:
        allowed.setdefault((e["workload"], metric_of(e)), []).append(e["most"])

    rows, failed = [], False
    for name, n in sorted((base_names or {}).items()):
        if name not in names:
            rows.append((name, n, None, None, "FAILED: dropped -- the base's program runs it, this tree's does not", {}))
            failed = True
    for name in sorted(names):
        w = results["workloads"][name]
        n, base, head = w["requests"], w.get("base") or [], w.get("head") or []

        def fail(why, b=None, h=None):
            rows.append((name, n, b, h, "FAILED: " + why, {}))
            return True

        if not head:
            failed |= fail("this tree's build did not run it")
            continue
        h, differ = io_side(head)
        if differ:
            failed |= fail(f"this tree's runs differ in {', '.join(differ)}: a count no run repeats", h=h)
            continue
        if not base:
            had = name in base_names if base_names is not None else bool(w.get("base_error"))
            if had:
                # The base had it: a run that crashed is no count to pass on.
                failed |= fail("the base's build did not run it", h=h)
            else:
                rows.append((name, n, None, h, "new: held to nothing", {}))
            continue
        if min(len(base), len(head)) < IO_RUNS:
            failed |= fail(f"fewer than {IO_RUNS} runs a side")
            continue
        b, differ = io_side(base)
        if differ:
            failed |= fail(f"the base's runs differ in {', '.join(differ)}: a count no run repeats", b, h)
            continue
        sent = [k for k in IO_SENT if b.get(k) != h.get(k)]
        if sent:
            failed |= fail(f"the two sides were not sent the same requests ({', '.join(sent)})", b, h)
            continue
        moved = {k: (b[k], h[k]) for k in IO_METRICS if b[k] != h[k]}
        failures, accepted = [], []
        for k, (was, now) in moved.items():
            said = f"{k} {io_moved(was, now)}"
            if any(io_lets(most, was, now) for most in allowed.get((name, k), [])):
                accepted.append(said)
            else:
                failures.append(said)
        if failures:
            verdict = "FAILED: " + "; ".join(failures)
            failed = True
        elif accepted:
            verdict = "accepted (PERF_ACCEPTED): " + "; ".join(accepted)
        else:
            verdict = "ok"
        rows.append((name, n, b, h, verdict, moved))
    rows.sort(key=lambda r: r[0])
    return rows, failed


def io_table(results, rows):
    """The I/O judgement as Markdown, a step summary's: each workload's
    counts, `base → this tree` where they differ, the ratios a reader
    weighs them by, and each file's share."""
    out = [
        f"### I/O a workload, base against this tree ({results.get('runs', '?')} runs a side of each "
        "side's fennec-server under `strace -f`, every run of a side alike)",
        "",
        "| workload | answered | rows | bytes sent | fsyncs | fsyncs an answer | renames | write calls "
        "| bytes written | written a byte sent | file before | written a byte of it | verdict |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for name, n, b, h, verdict, _ in rows:
        sides = [r for r in (b, h) if r]

        def cell(key):
            vals = [r[key] for r in sides]
            if not vals:
                return "-"
            return " → ".join(f"{v:,}" for v in dict.fromkeys(vals))

        def ratio(num, den, digits):
            vals = [f"{r[num] / r[den]:,.{digits}f}" if r[den] else "-" for r in sides]
            return " → ".join(dict.fromkeys(vals)) or "-"

        out.append(
            f"| {name} | {cell('answered')} | {cell('rows')} | {cell('sent')} | {cell('fsyncs')} "
            f"| {ratio('fsyncs', 'answered', 2)} | {cell('renames')} | {cell('writes')} | {cell('written')} "
            f"| {ratio('written', 'sent', 3)} | {cell('file_before')} | {ratio('written', 'file_before', 3)} | {verdict} |"
        )
    out += [
        "",
        f"Any count of this tree's that is not the base's fails, up or down -- fewer fsyncs a durable "
        "write is durability lost -- unless the pull request adds a PERF_ACCEPTED entry (tools/perf_gate.py) "
        "naming the workload and the count, which the owner approves: docs/design/benchmarks.md#the-gate. "
        "Counted from the server taking the workload's connection to its taking the driver's next: the "
        "fsync family's calls (fsync, fdatasync, sync_file_range, msync, syncfs) on files, renames, and "
        "the calls writing to files and the bytes they returned -- never a socket's or a pipe's, the log "
        "being a pipe; *answered* and *bytes sent* are the client's, *file before* the database's size "
        "once the setup was answered.",
    ]
    detail = []
    for name, _, b, h, _, _ in rows:
        for side, r in (("base", b), ("this tree", h)):
            if not r or (side == "base" and h and h.get("files") == r.get("files")):
                continue
            label = "both" if side == "this tree" and b and b.get("files") == r.get("files") else side
            for f, c in sorted((r.get("files") or {}).items()):
                detail.append(
                    f"| {name} | {label} | `{f}` | {c.get('writes', 0):,} | {c.get('written', 0):,} "
                    f"| {c.get('fsyncs', 0):,} | {c.get('renames', 0):,} |"
                )
    if detail:
        out += [
            "",
            "<details><summary>Each file's share, as the server named it in the data directory</summary>",
            "",
            "| workload | side | file | write calls | bytes written | fsyncs | renamed to it |",
            "|---|---|---|---:|---:|---:|---:|",
            *detail,
            "",
            "</details>",
        ]
    errors = [(n, w["base_error"]) for n, w in sorted(results["workloads"].items()) if w.get("base_error")]
    if errors:
        out += ["", "<details><summary>What the base's build said of the I/O workloads it did not run</summary>", ""]
        for n, e in errors:
            tail = " / ".join(line.strip() for line in e.strip().splitlines()[-3:] if line.strip())
            out.append(f"- `{n}`: {tail.replace('`', chr(39))}")
        out += ["", "</details>"]
    return "\n".join(out) + "\n"


def check_options(c):
    """The check's options, on its parser `c`: the command line's, and
    `check_args`'s, which `run` and `io` judge through."""
    c.add_argument("results")
    c.add_argument("--accepted-from", help="the checker whose PERF_ACCEPTED is the pull request's (this one)")
    c.add_argument("--accepted-base", help="the checker whose PERF_ACCEPTED the base holds already")
    c.add_argument("--base-program", help="the base's own gate.rs, whose workloads this tree's are held to (empty: none)")
    c.add_argument("--summary", help="append the table to this file (a step summary)")
    c.add_argument("--expect", help="a holdout pair: `pass`, or the workloads that fail, `name:instructions|bytes|both` "
                   "each, two joined by `+`, `?` after one that may fail or not; exit 0 when exactly those fail "
                   "(instruction results alone)")
    c.add_argument("--cannot-run", action="append", help="a holdout pair: `name[,name...]=why`, workloads its base "
                   f"cannot run, the words of the base's error, {WHY_AT_LEAST} characters at the least (empty: none; "
                   "instruction results alone)")


def check_args(results, **given):
    """`check`'s arguments as its command line would make them: every
    option at its default but those `given`. `run` and `io` judge through
    this, so an option added to the check is one no caller can leave out
    -- a namespace made by hand lacked the holdout's `expect` and
    `cannot_run`, and `make perf-gate-io` died on it. A name the check has
    no option for is refused."""
    p = argparse.ArgumentParser(prog="perf_gate.py check", add_help=False)
    check_options(p)
    ns = p.parse_args(["--", results])
    for k, v in given.items():
        if not hasattr(ns, k):
            raise TypeError(f"the check has no option {k!r}")
        setattr(ns, k, v)
    return ns


def check(args):
    try:
        with open(args.results, encoding="utf-8") as f:
            results = json.load(f)
        head = checked(accepted_in(args.accepted_from or os.path.abspath(__file__)))
        base = checked(accepted_in(args.accepted_base)) if args.accepted_base else []
        of_io = results.get("metric") == "io"
        names = None
        if args.base_program:
            with open(args.base_program, encoding="utf-8") as f:
                names = (io_workload_names if of_io else workload_names)(f.read())
        if of_io:
            if args.expect is not None or args.cannot_run:
                # The holdout's options: a pair's expectation is of
                # instructions and bytes, and a base that cannot run a
                # workload is said by its error -- an I/O count is held to
                # the base's to the call, with nothing to expect in its place.
                raise ValueError(
                    "`--expect` and `--cannot-run` judge instruction results, the holdout's; these results "
                    "hold I/O counts, which are held to the base's to the call and expect nothing"
                )
            rows, failed = judge_io(results, added(head, base), base_names=names)
            text = io_table(results, rows)
        else:
            cannot = {}
            for text in args.cannot_run or []:
                cannot.update(cannot_run_of(text))
            rows, failed = judge(results, added(head, base), base_names=names, cannot_run=cannot)
            text = table(results, rows)
            if args.expect is not None:
                # A holdout pair: as expected when exactly the workloads named
                # fail, each by the metrics named -- one more or one fewer is
                # not, nor one failing for no metric's reason.
                off = unexpected(rows, expected_of(args.expect))
                if off:
                    text += "\n**Not as expected**:\n\n" + "".join(f"- {line}\n" for line in off)
                else:
                    text += f"\n**As expected**: failed, {failures_said(failing(rows))}.\n"
                failed = bool(off)
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


# ------------------------------------------------------------------ I/O

# Runs of each I/O workload a side. Its counts are exact -- the same to the
# call run after run, 20 runs of one build held to each other in the
# design document -- so three are no median: every run of a side must
# count the same (`io_side`), and a count that moves fails as a count
# should not.
IO_RUNS = 3
# What strace is asked for: the connections the server takes, which mark
# the counted stretch, and every call that syncs, renames or writes. Each
# goes as `?name`, which strace passes over where the architecture has no
# such call (aarch64 has no `rename`, x86_64's musl calls it).
IO_CALLS = (
    "accept", "accept4", "write", "pwrite64", "writev", "pwritev", "pwritev2", "copy_file_range", "sendfile",
    "fsync", "fdatasync", "sync_file_range", "msync", "syncfs", "rename", "renameat", "renameat2",
)
IO_ACCEPTS = frozenset({"accept", "accept4"})
# The fsync family. `msync` names no descriptor, and is a mapped file's.
IO_SYNCS = frozenset({"fsync", "fdatasync", "sync_file_range", "msync", "syncfs"})
IO_RENAMES = frozenset({"rename", "renameat", "renameat2"})
# A call writing bytes, and which of its descriptors (`-y` shows each with
# its path) is the one written to: copy_file_range's second.
IO_WRITES = {"write": 0, "pwrite64": 0, "writev": 0, "pwritev": 0, "pwritev2": 0, "copy_file_range": 1, "sendfile": 0}
# What else each run of a side must agree on, and both sides: the client's
# requests answered, rows and bytes sent in the run (and in the setup), and
# the database's size once the setup was answered.
IO_SENT = ("answered", "rows", "sent", "setup_sent", "file_before")
# The server: the workload's file, a port the system picks (its `listening
# on` line names it), every write durable before it is answered, no
# compaction of its own -- looked for every 5 s, by the clock -- and the one
# shutdown once its stdin closes, which also ends a server whose driver
# died, so none is left behind.
IO_SERVER = ("--http", "127.0.0.1:0", "--sync", "always", "--auto-compact", "off", "--exit-on-stdin-close")
# A phase that takes longer has hung.
IO_TIMEOUT = 300

STRACE_LINE = re.compile(r"^(?:\[pid\s+)?(\d+)\]?\s+(.*)$")
STRACE_RESUMED = re.compile(r"^<\.\.\. (\w+) resumed>(.*)$")
STRACE_CALL = re.compile(r"^(\w+)\((.*)$")
# A call's arguments and what it returned, after the last `) =`: an int,
# `-1 EBADF (...)`, a descriptor with its path, or `?` where the process
# ended inside the call.
STRACE_RETURN = re.compile(r"^(.*)\)\s+=\s+(\S.*)$")
# A descriptor as `-y` shows it, `3</data/io.fennec>`, `5<socket:[812]>`,
# and the working directory a relative path of renameat's is in,
# `AT_FDCWD</data>`.
STRACE_FD = re.compile(r"(-?\d+|AT_FDCWD)<([^<>]*)>")
UNFINISHED = "<unfinished ...>"


def strace_calls(lines):
    """The calls a `strace -f -y` trace holds, (name, arguments, return) --
    the return an int, None where the process ended inside the call -- in
    the order they returned: a call another thread's interleaved, written
    `<unfinished ...>` where it began and `<... name resumed>` where it
    returned, is joined by its thread's id and yielded where it returned.
    Exits and signals are passed over, and so is a call resumed whose
    beginning the trace does not hold."""
    begun = {}
    for raw in lines:
        m = STRACE_LINE.match(raw.rstrip("\n"))
        if not m:
            continue
        tid, body = m.groups()
        if body.startswith(("+++", "---")):
            continue
        r = STRACE_RESUMED.match(body)
        if r:
            start = begun.pop(tid, None)
            if start is None or start[0] != r.group(1):
                continue
            name, text = start[0], start[1] + r.group(2)
        else:
            c = STRACE_CALL.match(body)
            if not c:
                continue
            name, text = c.groups()
            if text.endswith(UNFINISHED):
                begun[tid] = (name, text[: -len(UNFINISHED)].rstrip())
                continue
        # strace pads before the `=` where a call was resumed or its
        # arguments were short: `<... write resumed>)        = 100`.
        done = STRACE_RETURN.match(text)
        if not done:
            continue
        v = re.match(r"-?\d+", done.group(2))
        yield name, done.group(1), int(v.group()) if v else None


def io_counts(lines, data_dir, start=2, end=3):
    """What a server's trace (`lines`) did to files between the `start`th
    connection it took and the `end`th: {fsyncs, renames, writes, written,
    failed, files}, and the connections it took in all. A call counts where
    it returned. A descriptor counts as a file where `-y` names it by a path
    outside /dev, /proc and /sys -- never a socket, a pipe (the server's
    log, its stderr, is the driver's pipe) or /dev/null. A call that failed
    counts in `failed` alone, its bytes none; `files` gives each file's
    share, named within `data_dir`."""
    data_dir = data_dir.rstrip("/")

    def file_of(path):
        path = path.removesuffix(" (deleted)")
        if not path.startswith("/") or path.startswith(("/dev/", "/proc/", "/sys/")):
            return None
        if path == data_dir:
            return "."
        if path.startswith(data_dir + "/"):
            return path[len(data_dir) + 1:]
        return "(outside the data directory)"

    out = {"fsyncs": 0, "renames": 0, "writes": 0, "written": 0, "failed": 0, "files": {}}

    def share(f, key, n=1):
        c = out["files"].setdefault(f, {"writes": 0, "written": 0, "fsyncs": 0, "renames": 0})
        c[key] += n

    taken = 0
    for name, args, ret in strace_calls(lines):
        if name in IO_ACCEPTS:
            taken += ret is not None and ret >= 0
            continue
        if not start <= taken < end:
            continue
        ok = ret is not None and ret >= 0
        fds = STRACE_FD.findall(args)
        if name in IO_RENAMES:
            # The new name: the last path, joined to the directory whose
            # descriptor stands before it where renameat's is relative.
            quoted = list(re.finditer(r'"([^"]*)"', args))
            target = quoted[-1].group(1) if quoted else ""
            if quoted and not target.startswith("/"):
                before = STRACE_FD.findall(args[: quoted[-1].start()])
                target = before[-1][1] + "/" + target if before else ""
            if not ok:
                out["failed"] += 1
                continue
            out["renames"] += 1
            share(file_of(target) or "(not a file)", "renames")
        elif name in IO_SYNCS:
            f = "(mapped)" if name == "msync" else file_of(fds[0][1]) if fds else None
            if f is None:
                continue
            if not ok:
                out["failed"] += 1
                continue
            out["fsyncs"] += 1
            share(f, "fsyncs")
        elif name in IO_WRITES:
            i = IO_WRITES[name]
            f = file_of(fds[i][1]) if len(fds) > i else None
            if f is None:
                continue
            if not ok:
                out["failed"] += 1
                continue
            out["writes"] += 1
            out["written"] += ret
            share(f, "writes")
            share(f, "written", ret)
    return out, taken


def tracing():
    """Whether strace can count here: Linux with strace on the PATH."""
    if platform.system() != "Linux" or shutil.which("strace") is None:
        print(
            "perf_gate: the I/O workloads are counted on Linux with strace installed; on macOS run "
            "them in Docker (docs/design/benchmarks.md#the-gate). Nothing measured.",
            file=sys.stderr,
        )
        return False
    return True


def listed_io(client):
    """The I/O workloads the client program runs: [(name, requests)]; none
    where it has no `--list-io` (a program from before them)."""
    p = subprocess.run([client, "--list-io"], capture_output=True, text=True)
    if p.returncode != 0:
        return []
    return [(name, int(n)) for name, n in (line.split() for line in p.stdout.splitlines() if line.strip())]


def phase(client, which, name, addr):
    """A phase of workload `name` run by the client program against the
    server at `addr`: what it said it sent, {answered, rows, sent}."""
    p = subprocess.run([client, which, name, addr], capture_output=True, text=True, timeout=IO_TIMEOUT)
    m = re.search(rf"^{which}: {re.escape(name)} answered=(\d+) rows=(\d+) sent=(\d+)$", p.stdout, re.M)
    if p.returncode != 0 or not m:
        raise RuntimeError(f"{name}: `{which}` exited {p.returncode}\n{p.stdout[-1500:]}\n{p.stderr[-3000:]}")
    return dict(zip(("answered", "rows", "sent"), map(int, m.groups())))


def io_one_run(server, client, name, requests, scratch, keep=None):
    """Runs I/O workload `name` once against `server`, the server alone
    under `strace -f`: the setup over one connection, the size it left the
    file at, the run over the next, then the driver's own request over a
    third, which ends what is counted, and the server's stdin closed for
    its one shutdown. What the server did to files in between
    (`io_counts`) beside what the client sent. With `keep`, the trace is
    copied there."""
    data_dir = tempfile.mkdtemp(prefix=f"{name}.", dir=scratch)
    trace = data_dir + ".strace"
    cmd = [
        "strace", "-f", "-qq", "-y", "-s", "0", "--seccomp-bpf", "-o", trace,
        "-e", "trace=" + ",".join("?" + c for c in IO_CALLS),
        "--", server, "--file", os.path.join(data_dir, "io.fennec"), *IO_SERVER,
    ]
    # The server's stderr -- its log and its `listening on` line -- is this
    # pipe, so no line of its log is a write to a file.
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    said, listening = [], threading.Event()
    addr = []

    def read_stderr():
        for line in proc.stderr:
            said.append(line)
            m = re.search(r"listening on: http://(\S+)", line)
            if m and not addr:
                addr.append(m.group(1))
                listening.set()
        listening.set()

    reader = threading.Thread(target=read_stderr, daemon=True)
    reader.start()
    try:
        if not listening.wait(IO_TIMEOUT) or not addr:
            raise RuntimeError(f"{name}: the server never said where it listens:\n{''.join(said)[-3000:]}")
        setup = phase(client, "io-setup", name, addr[0])
        file_before = os.path.getsize(os.path.join(data_dir, "io.fennec"))
        run = phase(client, "io", name, addr[0])
        if run["answered"] != requests:
            raise RuntimeError(f"{name}: {run['answered']} requests answered of {requests}")
        # The third connection: taken once the run is answered, it ends
        # what is counted, before the shutdown's own sync.
        host, port = addr[0].rsplit(":", 1)
        conn = http.client.HTTPConnection(host, int(port), timeout=IO_TIMEOUT)
        conn.request("GET", "/_health")
        status = conn.getresponse()
        status.read()
        conn.close()
        if status.status != 200:
            raise RuntimeError(f"{name}: GET /_health answered {status.status}")
    finally:
        # The one shutdown, whatever happened: the server exits on its own,
        # strace with it.
        proc.stdin.close()
        try:
            proc.wait(IO_TIMEOUT)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        reader.join(10)
    try:
        if proc.returncode != 0:
            raise RuntimeError(f"{name}: the server exited {proc.returncode}:\n{''.join(said)[-3000:]}")
        with open(trace, encoding="utf-8", errors="replace") as f:
            counts, taken = io_counts(f, data_dir)
        if taken != 3:
            raise RuntimeError(f"{name}: the trace shows {taken} connections taken, where a run makes 3")
        if keep:
            os.makedirs(keep, exist_ok=True)
            shutil.copyfile(trace, os.path.join(keep, f"{name}.strace"))
    finally:
        shutil.rmtree(data_dir, ignore_errors=True)
        if os.path.exists(trace):
            os.unlink(trace)
    return {**run, "setup_sent": setup["sent"], "file_before": file_before, **counts}


def measure_io(base_server, head_server, client, runs, jobs, scratch, log=print, only=None, traces=None):
    """Every I/O workload of the client program run `runs` times against
    each side's server, `jobs` at a time, each round of every workload and
    side before the next: results of `"metric": "io"`. The client is one
    program -- this tree's -- for both sides, so each is sent the same
    requests, byte for byte, and only the servers differ. A run the head
    fails is the gate's failure; one the base fails keeps no base run and
    says why (`base_error`), the judgement holding it to nothing only where
    the base's own program had no such workload. With `traces`, each
    side's last run of each workload keeps its trace there."""
    work = {"head": head_server, "base": base_server}
    names = [(n, k) for n, k in listed_io(client) if not only or n in only]
    if not names:
        raise RuntimeError(f"{client} lists no I/O workload (--list-io)")
    results = {"metric": "io", "tool": "strace", "runs": runs, "workloads": {}}
    failed = {}
    for name, n in names:
        results["workloads"][name] = {"requests": n, "base": [], "head": []}
        if not base_server:
            failed[("base", name)] = f"{name}: the base's fennec-server did not build"
    tasks = [
        (side, name, n)
        for _ in range(runs)
        for name, n in names
        for side in ("base", "head")
        if work[side] and (side, name) not in failed
    ]
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {
            pool.submit(
                io_one_run, work[side], client, name, n, scratch,
                os.path.join(traces, side) if traces else None,
            ): (side, name)
            for side, name, n in tasks
        }
        for fut in concurrent.futures.as_completed(futures):
            side, name = futures[fut]
            try:
                results["workloads"][name][side].append(fut.result())
            except (RuntimeError, OSError, subprocess.SubprocessError) as e:
                failed.setdefault((side, name), str(e))
            done += 1
            if done % 8 == 0 or done == len(tasks):
                log(f"  {done}/{len(tasks)} I/O runs")
    heads = [msg for (side, _), msg in sorted(failed.items()) if side == "head"]
    if heads:
        raise RuntimeError("an I/O workload failed against this tree's server:\n" + "\n".join(heads))
    for (_, name), msg in failed.items():
        results["workloads"][name]["base"] = []
        results["workloads"][name]["base_error"] = msg[-600:]
    return results


def io_measure(args):
    if not tracing():
        return 0
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    scratch = tempfile.mkdtemp(prefix="perf-gate-io-")
    try:
        results = measure_io(
            args.base_server, args.head_server, args.client, args.runs, args.jobs, scratch,
            only=args.only.split(",") if args.only else None, traces=args.traces,
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
    return check(check_args(
        out, accepted_from=os.path.abspath(__file__), accepted_base=accepted_base,
        base_program=base_program, summary=args.summary,
    ))


def build_server(tree, target, target_dir, cargo, log):
    """Builds fennec-server in `tree` into `target_dir` -- the gate
    program's, in the same profile and environment, so the crates both
    reach are built once: its path, or None where it does not build."""
    cmd = [cargo, "build", "--locked", "--release", "-p", "fennec-server", "--bin", "fennec-server"]
    if target:
        cmd += ["--target", target]
    env = dict(os.environ, CARGO_TARGET_DIR=target_dir, CARGO_PROFILE_RELEASE_STRIP="false")
    p = subprocess.run(cmd, cwd=tree, env=env, capture_output=True, text=True)
    if p.returncode != 0:
        log(f"fennec-server did not build in {tree}:\n{p.stderr[-4000:]}")
        return None
    return os.path.join(target_dir, *([target] if target else []), "release", "fennec-server")


def tree_at(ref, where):
    """A worktree of `ref` at `where`: the one `run --keep` left there where
    it is at that commit -- its builds' sources untouched, so cargo builds
    nothing of them again -- else made anew. Its commit, and whether it was
    made here."""
    sha = git("rev-parse", "--verify", f"{ref}^{{commit}}")
    if os.path.isdir(where):
        at = subprocess.run(["git", "rev-parse", "HEAD"], cwd=where, capture_output=True, text=True)
        if at.returncode == 0 and at.stdout.strip() == sha:
            return sha, False
    worktree(ref, where)
    shutil.copyfile(os.path.join(ROOT, PROGRAM), os.path.join(where, PROGRAM))
    return sha, True


def io(args):
    """`make perf-gate-io`: this tree's fennec-server -- or HEAD's, a replay
    -- and BASE's, built beside the gate program in `run`'s trees and target
    directories, sent the I/O workloads by this tree's gate program and
    counted under strace, then judged."""
    if not tracing():
        return 0
    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)
    log = lambda m: print(m, flush=True)
    base_tree = os.path.join(work, "base")
    sha, _ = tree_at(args.base, base_tree)
    head_tree = ROOT
    if args.head:
        head_tree = os.path.join(work, "head")
        tree_at(args.head, head_tree)
    try:
        log(f"building fennec-server in {head_tree} and at {args.base} ({sha[:10]}), side by side")
        targets = {"head": os.path.join(work, "head-target"), "base": os.path.join(work, "base-target")}
        def head_side():
            # The client after it: this tree's gate program, which `run`
            # built, and cargo finds built.
            server = build_server(head_tree, args.target, targets["head"], args.cargo, log)
            return server, build(head_tree, args.target, targets["head"], args.cargo, log)

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            head_f = pool.submit(head_side)
            base_f = pool.submit(build_server, base_tree, args.target, targets["base"], args.cargo, log)
            (head_server, client), base_server = head_f.result(), base_f.result()
        if head_server is None or client is None:
            log(f"I/O replay {args.base} -> {args.head or 'this tree'}: fennec-server or the gate program "
                "does not build at the head")
            return 1
        log(f"counting {args.runs} runs a side of each I/O workload, {args.jobs} at a time")
        scratch = tempfile.mkdtemp(prefix="scratch-io-", dir=work)
        try:
            results = measure_io(base_server, head_server, client, args.runs, args.jobs, scratch, log)
        finally:
            shutil.rmtree(scratch, ignore_errors=True)
        results["base"] = sha
        out = args.out or os.path.join(work, "io.json")
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
    return check(check_args(
        out, accepted_from=os.path.abspath(__file__), accepted_base=accepted_base,
        base_program=base_program, summary=args.summary,
    ))


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

    i = sub.add_parser("io", help="build this tree's fennec-server and BASE's, run the I/O workloads under strace, judge")
    i.add_argument("--base", default="origin/main", help="the git ref held to (origin/main)")
    i.add_argument("--head", help="a git ref held to it in place of this tree: a replay of two commits")
    i.add_argument("--target", help="the build's target triple (CI: x86_64-unknown-linux-musl)")
    i.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    i.add_argument("--runs", type=int, default=IO_RUNS)
    i.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1))
    i.add_argument("--work", default=os.path.join(ROOT, "target", "perf-gate"), help="scratch: `run`'s trees and builds")
    i.add_argument("--out", help="the results file (WORK/io.json)")
    i.add_argument("--keep", action="store_true", help="keep the base's worktree")
    i.add_argument("--no-check", action="store_true", help="count only: CI judges with the base's checker")
    i.add_argument("--summary", help="append the judgement's table to this file (a step summary)")

    im = sub.add_parser("io-measure", help="run the I/O workloads against two built servers under strace")
    im.add_argument("--base-server", help="the base's fennec-server (none: this tree's alone)")
    im.add_argument("--head-server", required=True)
    im.add_argument("--client", required=True, help="the gate program that sends the workloads")
    im.add_argument("--runs", type=int, default=IO_RUNS)
    im.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1))
    im.add_argument("--out", required=True)
    im.add_argument("--only", help="these workloads alone, a comma between")
    im.add_argument("--traces", help="keep each side's last trace of each workload here")

    check_options(sub.add_parser("check", help="judge a results file"))

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
        commands = {
            "run": run, "measure": measure, "io": io, "io-measure": io_measure, "check": check,
            "reaches": reach, "edits": edits,
        }
        return commands[args.command](args)
    except (ValueError, RuntimeError, subprocess.CalledProcessError) as e:
        print(f"perf_gate: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
