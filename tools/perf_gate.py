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
each entry, and each edit of the gate's own files -- gate.rs, this checker,
the workflow's two jobs, the jobs they need and the local actions they use,
the workflow outside its jobs, and any workflow's job named as one of
them -- which `edits` names (`workflow_edits`).

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
syncs that make a file durable and the writebacks that make nothing
durable, each by its call and its file; renames; the calls writing to
files and the bytes they wrote, file by file; the bytes written and not
yet durable when the server next began to write to a socket, its answer,
file by file; the files created, truncated, linked and unlinked; the calls of
those that failed, by call and file -- and beside them the database
file's size once the setup was answered. Each to the call, so every run
of a side must count the same, and any count of this tree's that is not
the base's fails, up or down (fewer fsyncs a durable write is durability
lost), unless the pull request adds a PERF_ACCEPTED entry naming the
workload and the count, the file where the count is a file's and the call
where it is a call's, and the move, exactly (`"metric": "fsyncs", "file":
"io.fennec.sync", "call": "fdatasync"`, a `most` of `+100%` or `+1`). A
run under its workload's floor (IO_FLOORS) -- no fsync, no write, fewer
fsyncs than durable writes answered -- fails: the trace missed what the
server did. A server whose writes the trace cannot see -- io_uring, Linux
AIO, a file of the data directory opened O_SYNC, O_DSYNC or O_DIRECT, a
`pwritev2` with a sync flag, an `ioctl` setting its attributes -- is
refused, and so is a run that counts a file outside its data directory or
a call on a descriptor strace did not name. A store into a shared mapping
is no call, and the mappings are not traced (IO_CALLS says why).

    python3 tools/perf_gate.py io --base origin/main      # make perf-gate-io
    python3 tools/perf_gate.py io-measure --base-server B --head-server H \\
        --client G --out IO.json                          # two built servers
    python3 tools/perf_gate.py check IO.json              # judged as above

CI counts and judges the I/O with the base's checker too, but where the
base's own gate.rs mentions no IO_WORKLOADS: the pull request that brought
them was counted and judged by its own. Both jobs choose it in the shell
(`io_checker`, which the tests hold their `grep` to). Each writes the
base's copy of this checker under tools/ -- where the pull request tracks
nothing, the path cleared first -- from the base's file as git reads it,
this tree's only where git says the base has none.

`run` and `measure` count with valgrind, `io` and `io-measure` with strace,
both on Linux: elsewhere they say so and exit 0 (on macOS, in Docker:
docs/design/benchmarks.md#the-gate) -- `io` and `io-measure` fail under CI
(GITHUB_ACTIONS). `check`, `reaches` and `edits` run anywhere.
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
# owner (`edits`), compared by their lines as YAML reads them, comments and
# the blank lines that hold nothing aside (`yaml_lines`) -- and so is an
# edit of what reaches them: the workflow outside its `jobs:`, a job they
# need, a local action they use, and any workflow's job named as one of
# them (`workflow_edits`).
WORKFLOW = ".github/workflows/ci.yml"
WORKFLOWS = ".github/workflows/"
JOBS = ("perf-count", "perf")
# What an entry lets through: the figure (instructions an op, the
# allocator priced), or the heap bytes an op; or one of an I/O workload's
# counts (IO_METRICS, below).
INSTRUCTION_METRICS = ("instructions", "bytes")
# An I/O workload's counts, each judged to the call: the syncs that make
# a file durable (IO_DURABLE), the calls that start a writeback and make
# nothing durable (IO_WRITEBACKS), renames, the calls writing to files,
# the bytes they wrote, the bytes written and not yet durable when the
# server answered (`unsynced`), the calls of those that failed, and the
# files made, truncated, linked and unlinked.
IO_COUNTS = (
    "fsyncs", "writebacks", "renames", "writes", "written", "unsynced", "failed", "creates", "truncates", "links",
    "unlinks",
)
# Of those, the ones judged file by file, each file named within the data
# directory: an fsync of the database where its sync log's was -- the same
# count, some 356 against 65 microseconds an fsync (durability.md) -- or
# its bytes written to the other is a change, which a total does not show;
# a log record answered before its sync, the database's left so by design;
# a failed write where a failed fsync was.
IO_BY_FILE = ("fsyncs", "writebacks", "written", "unsynced", "failed")
# Of those, the ones judged call by call too: an `fsync` of the log where
# its `fdatasync` was is the same count of the same file, and a journal
# commit on ext4 every durable write -- what the log exists to save -- and
# a `syncfs` in its place syncs the whole file system each time.
IO_BY_CALL = ("fsyncs", "writebacks", "failed")
# What an entry may name of an I/O workload: its counts, and the database
# file's size once the setup was answered (`file_before`), which the
# server's encoding of the setup decides -- a record's, the schema's, the
# header's -- and which an entry lets move as it does a count.
IO_METRICS = IO_COUNTS + ("file_before",)
METRICS = INSTRUCTION_METRICS + IO_METRICS
# What a run of each I/O workload counts at the least: under it, the trace
# missed what the server did -- a call strace was not asked for, a file
# `-y` did not name -- and both sides, missing it alike, would agree on a
# count of nothing. Each sends durable writes one at a time under `--sync
# always`, where a write is answered only once it has synced and one
# request in flight shares its sync with none: an fsync an answer at the
# least ("answered", the requests the run answered) -- a sync that makes a
# file durable (IO_DURABLE), never a writeback, which a durable write
# answered on would have lost its durability -- and a write call. A
# compact renames its new file over the old. A workload named nowhere here
# is held to IO_FLOOR; one that writes nothing on purpose names its own.
IO_FLOOR = {"fsyncs": 1, "writes": 1}
IO_FLOORS = {
    "io-put": {"fsyncs": "answered", "writes": 1},  # 500 puts: 500 fsyncs
    "io-batch": {"fsyncs": "answered", "writes": 1},  # one block: one fsync
    "io-spill": {"fsyncs": "answered", "writes": 1},  # one block, spilled: one fsync
    "io-compact": {"fsyncs": "answered", "writes": 1, "renames": 1},
}

# Every workload a pull request made slower on purpose, in the order they
# went out, never removed: the history of what the gate let through. An
# entry lets its workload through only in the pull request that adds it, up
# to `most`, the measured delta with room for the merge queue's rerun; a
# later regression of the same workload needs an entry of its own. `metric`
# says what it lets through, "bytes" for the heap's bytes and the figure
# where it is left out; one entry a metric. An I/O workload's entry names
# its count (IO_METRICS) -- the file, for a count judged file by file
# (IO_BY_FILE), and the call, for one judged call by call (IO_BY_CALL) --
# and lets through the move its `most` says, exactly, the counts being
# exact: that share of the base's count, to the decimals it gives, or that
# many, signed -- "+100%", "+1", "-50%" -- an entry larger than its move
# failing it, so that no room is left a later commit could take. A share of
# a count of none is none, so a count from 0 is let through by a number
# alone; of several entries naming one count, any lets through its own
# move. `reason` says what the cost buys and how it was
# measured (`make perf-gate`, `make perf-gate-io`, `make instr-compare`'s
# figure on macOS).
# The owner approves an entry before its pull request merges. A literal:
# the base's checker reads it from a pull request's copy as data, never
# running it (`--accepted-from`).
#
#   {"workload": "http-put-keyed", "most": "+1.8%",
#    "reason": "a key's row now holds its request's digest, which a replay
#               checks: +260 instructions a keyed put (make perf-gate),
#               +1.5% by make instr-compare MODES=keyed on an M1"}
#   {"workload": "io-compact", "metric": "fsyncs", "file": ".", "call": "fsync", "most": "+1",
#    "reason": "the directory is synced after the rename too, or a power
#               cut could bring the old file back: one fsync a compact"}
PERF_ACCEPTED = [
    {"workload": "io-compact", "metric": "truncates", "most": "-1",
     "reason": "the compact's side file is made new with create_private (O_EXCL, following no link at its "
               "name) once a leftover is unlinked, where an O_TRUNC open of the old name stood: one truncate "
               "a compact fewer, its fsyncs, renames, writes and bytes unchanged (CI's perf (instructions) on "
               "pull request #678, io-compact truncated 1 -> 0)"},
]

# What an entry must say, what it may, and how long its reason is at the
# least: a written justification, not a word.
ENTRY_KEYS = {"workload", "most", "reason"}
ENTRY_OPTIONAL = {"metric", "file", "call"}
REASON_AT_LEAST = 40


# ------------------------------------------------------------------ entries


def accepted_text(text, where):
    """PERF_ACCEPTED as the checker `text` writes it, read as data -- a
    literal, never run: what the base's copy holds a pull request's entries
    by. A file that does not parse, or a value that is no literal or nests
    past what the parser takes, is a ValueError naming it, which the check
    says as a refusal: never a traceback."""
    try:
        tree = ast.parse(text, where)
        for node in tree.body:
            targets = node.targets if isinstance(node, ast.Assign) else [getattr(node, "target", None)]
            if any(isinstance(t, ast.Name) and t.id == "PERF_ACCEPTED" for t in targets):
                return ast.literal_eval(node.value)
    except (SyntaxError, ValueError, TypeError, RecursionError, MemoryError) as e:
        raise ValueError(f"{where}: PERF_ACCEPTED cannot be read as a literal: {type(e).__name__}: {e}") from None
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


def io_share_said(most, share):
    """`share`, a fraction, said as a percentage to as many decimals as
    entry `most` gives its own: `+0.19%` for a `most` of `+0.25%`."""
    digits = re.fullmatch(r"[+-]\d+(?:\.(\d+))?%", str(most).strip()).group(1) or ""
    return f"{share * 100:+.{len(digits)}f}%"


def io_lets(most, base, head):
    """Whether an I/O entry's `most` lets a count move from `base` to
    `head`: the move it says, exactly -- the counts are exact, so an entry
    larger than its move would leave room a later change could take. A
    count (`+1`) is the move to the call; a share (`+100%`) the move's share
    of the base's count, to the decimals it gives. A share of a base of
    none is none: from 0, only a count lets anything through."""
    kind, v = io_most(most)
    d = head - base
    if d == 0 or (d > 0) != (v > 0):
        return False
    if kind == "count":
        return d == v
    return base != 0 and io_share_said(most, d / base) == io_share_said(most, v)


def io_larger(most, base, head):
    """Whether entry `most` names a larger move than the one from `base` to
    `head`, in its direction: an entry that would have let this one through
    when entries were ceilings."""
    kind, v = io_most(most)
    d = head - base
    if d == 0 or (d > 0) != (v > 0) or (kind == "share" and base == 0):
        return False
    return abs(v) > abs(d) if kind == "count" else abs(v) > abs(d / base)


def metric_of(e):
    """What entry `e` lets through: its `metric`, the figure's where none."""
    return e.get("metric", "instructions")


def checked(entries):
    """`entries` held to the form an entry takes: each a dict of ENTRY_KEYS
    and at most ENTRY_OPTIONAL, a workload's name, a metric, a `most` above
    the threshold -- an I/O count's signed (`io_most`) -- a file where the
    count is judged file by file (IO_BY_FILE) and none elsewhere, a call
    of its kind where it is judged call by call (IO_BY_CALL) and none
    elsewhere, and a reason in words. A malformed entry is the gate's
    failure, never one passed over."""
    if not isinstance(entries, list):
        raise ValueError("PERF_ACCEPTED is a list of entries")
    for e in entries:
        if not isinstance(e, dict) or not ENTRY_KEYS <= set(e) <= ENTRY_KEYS | ENTRY_OPTIONAL:
            raise ValueError(
                f"an entry names {sorted(ENTRY_KEYS)}, and `metric` where it lets bytes or an I/O count through, "
                f"`file` where that count is a file's, `call` where it is a call's: {e!r}"
            )
        if not isinstance(e["workload"], str) or not e["workload"]:
            raise ValueError(f"an entry's workload is a name: {e!r}")
        if not isinstance(metric_of(e), str) or metric_of(e) not in METRICS:
            raise ValueError(f"{e['workload']}: `metric` is one of {', '.join(METRICS)}: {e['metric']!r}")
        if metric_of(e) in IO_BY_FILE:
            # Judged file by file: the entry says whose count it lets move.
            if not isinstance(e.get("file"), str) or not e["file"]:
                raise ValueError(
                    f"{e['workload']}: {metric_of(e)} is judged file by file, and its entry names the file "
                    f"as the table does (`\"file\": \"io.fennec\"`): {e!r}"
                )
        elif "file" in e:
            raise ValueError(f"{e['workload']}: {metric_of(e)} is judged in total, and its entry names no file: {e!r}")
        if metric_of(e) in IO_BY_CALL:
            # And call by call: an `fsync` where an `fdatasync` was is a
            # change of its own, which its entry names as it names the file.
            calls = IO_CALLS_OF[metric_of(e)]
            if not isinstance(e.get("call"), str) or e["call"] not in calls:
                raise ValueError(
                    f"{e['workload']}: {metric_of(e)} is judged call by call, and its entry names the call, "
                    f"one of {', '.join(sorted(calls))} (`\"call\": \"fdatasync\"`): {e!r}"
                )
        elif "call" in e:
            raise ValueError(f"{e['workload']}: {metric_of(e)} is judged in total of every call, and its entry "
                             f"names no call: {e!r}")
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


def io_label(metric, f=None, call=None):
    """A count as a verdict names it: `fdatasync of `io.fennec.sync``, one
    count, and `fsync of `io.fennec.sync`` another; a writeback's call
    said to make nothing durable, and a failure's to have failed; a file's
    share of a count by the file; a total by its name."""
    if metric == "file_before":
        return "the file after the setup (`file_before`)"
    if call is not None:
        said = f"{call} of `{f}`"
        return said if metric == "fsyncs" else f"{said} (no durability)" if metric == "writebacks" else \
            f"{said} that failed"
    return metric if f is None else f"{metric} of `{f}`"


# A file's own counts beside its calls, each run of a side agreeing on
# every one: fsyncs, writebacks and failures the sums of their calls'.
IO_FILE_COUNTS = (
    "fsyncs", "writebacks", "written", "writes", "unsynced", "failed", "renames", "creates", "truncates", "links",
    "unlinks",
)
# A call's kind, as a run's files hold its counts.
IO_CALL_KEYS = {"fsyncs": "fsync_calls", "writebacks": "writeback_calls", "failed": "failed_calls"}


def io_flat(run):
    """A run's counts as {label: value}: every one each run of a side must
    agree on -- the requests sent and answered (IO_SENT), each count in
    total, each file's apiece and each call's of each file, and the file
    after the setup."""
    out = {k: run.get(k) for k in IO_SENT + IO_METRICS}
    for f, c in sorted((run.get("files") or {}).items()):
        for k in IO_FILE_COUNTS:
            out[io_label(k, f)] = c.get(k, 0)
        for k, key in IO_CALL_KEYS.items():
            for call, n in sorted((c.get(key) or {}).items()):
                out[io_label(k, f, call)] = n
    return out


def io_side(runs):
    """A side's runs of an I/O workload: the one run they agree on, and each
    count they differ in (`io_flat`'s label) with every run's value, in the
    runs' order -- a file or a call one run counted and another did not is
    0 in the other. Counted to the call, a count that moves from one run to
    the next is the clock's or a draw's, and no count to judge by."""
    flats = [io_flat(r) for r in runs]
    labels = dict.fromkeys(k for f in flats for k in f)
    differ = [(k, [f.get(k, 0) for f in flats]) for k in labels if len({f.get(k, 0) for f in flats}) > 1]
    return runs[0], differ


def io_values(differ):
    """`io_side`'s differences as a verdict says them: each count, and
    every run's value by the run's number."""
    said = lambda v: f"{v:,}" if isinstance(v, int) else str(v)
    return "; ".join(f"{k} " + ", ".join(f"{said(v)} (run {i})" for i, v in enumerate(vals, 1)) for k, vals in differ)


def io_moved(b, h):
    """A count's move from `b` to `h`, as the verdict says it: its share
    to a tenth of a percent, or to two figures where it is smaller -- eight
    bytes more of a 4 MB file is no +0.0%."""
    pct = (h - b) / b * 100 if b else None
    share = "from none" if pct is None else f"{pct:+,.1f}%" if abs(pct) >= 0.1 else f"{pct:+.2g}%"
    return f"{b:,} → {h:,} ({share})"


def io_judged(run):
    """What of a run is held to the other side's, {(metric, file, call):
    value}: each sync call of each file, durable or a writeback, by its
    name (IO_BY_CALL); bytes written file by file (IO_BY_FILE); the rest of
    the counts and the file after the setup in total, their file and call
    None. A file's fsyncs, the sum of its calls', move only where a call's
    does, and are the table's, not judged twice."""
    out = {(k, None, None): run.get(k, 0) for k in IO_METRICS if k not in IO_BY_FILE}
    for f, c in (run.get("files") or {}).items():
        for k in IO_BY_FILE:
            if k in IO_BY_CALL:
                for call, n in (c.get(IO_CALL_KEYS[k]) or {}).items():
                    out[(k, f, call)] = n
            else:
                out[(k, f, None)] = c.get(k, 0)
    return out


def io_floor(name, run):
    """What `run` of workload `name` counts under its floor (IO_FLOORS):
    [(count, counted, the least)], none where it holds. Its fsyncs are the
    syncs that made a file durable, never a writeback's call."""
    out = []
    for k, least in IO_FLOORS.get(name, IO_FLOOR).items():
        at_least = run["answered"] if least == "answered" else least
        if run[k] < at_least:
            out.append((k, run[k], at_least))
    return out


def judge_io(results, entries=(), base_names=None):
    """Each I/O workload of `results` judged: (rows, failed), a row (name,
    requests, base, head, verdict, moved) -- base and head the run each
    side's runs agree on, or None, and moved {(metric, file, call): (base,
    head)} for each judged count (`io_judged`) that is not the base's. Any
    such move fails unless one of `entries`, the pull request's own, names
    the workload, the count, its file and its call where it has them, and
    lets it so far in its direction (`io_lets`). A side whose runs differ
    fails, naming every run's value; so does one under its workload's floor
    (`io_floor`), and sides sent different requests. As the instructions'
    (`judge`): a workload of `base_names` -- the base's own program's
    IO_WORKLOADS -- this tree dropped, or the base's build did not run,
    fails, where one new to this tree is held to nothing."""
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
        allowed.setdefault((e["workload"], metric_of(e), e.get("file"), e.get("call")), []).append(e["most"])
    order = {k: i for i, k in enumerate(IO_METRICS)}

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
            failed |= fail(f"this tree's runs differ, a count no run repeats: {io_values(differ)}", h=h)
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
            failed |= fail(f"the base's runs differ, a count no run repeats: {io_values(differ)}", b, h)
            continue
        under = [
            f"{side} runs counted {k} {got:,}, under {name}'s floor of {least:,}"
            for side, r in (("the base's", b), ("this tree's", h))
            for k, got, least in io_floor(name, r)
        ]
        if under:
            # Both sides missing a call alike would agree on nothing.
            failed |= fail("; ".join(under) + ": under it a write was answered before its sync, or the trace "
                           "missed what the server did", b, h)
            continue
        sent = [k for k in IO_SENT if b.get(k) != h.get(k)]
        if sent:
            moves = ", ".join(f"{k} {io_moved(b[k], h[k])}" for k in sent)
            failed |= fail(f"the two sides were not sent the same requests: {moves}", b, h)
            continue
        bj, hj = io_judged(b), io_judged(h)
        moved = {
            key: (bj.get(key, 0), hj.get(key, 0))
            for key in sorted(set(bj) | set(hj), key=lambda k: (order[k[0]], k[1] or "", k[2] or ""))
            if bj.get(key, 0) != hj.get(key, 0)
        }
        failures, accepted = [], []
        for (k, f, call), (was, now) in moved.items():
            said = f"{io_label(k, f, call)} {io_moved(was, now)}"
            mosts = allowed.get((name, k, f, call), [])
            if any(io_lets(most, was, now) for most in mosts):
                accepted.append(said)
            elif mosts:
                # Entries name it and none lets this move through: said, so
                # the author reads why, not only that -- one larger than the
                # move too, which says the move it lets through.
                past = f"{said}, which its entries ({', '.join(f'`{m}`' for m in mosts)}) do not let through"
                larger = [m for m in mosts if io_larger(m, was, now)]
                if larger:
                    shares = [m for m in larger if io_most(m)[0] == "share"]
                    move = f"{now - was:+,}" + (f", {io_share_said(shares[0], (now - was) / was)}" if shares else "")
                    past += (f" -- an entry names the move it lets through, and {', '.join(f'`{m}`' for m in larger)} "
                             f"{'is' if len(larger) == 1 else 'are'} larger than this one ({move})")
                if was == 0 and any(io_most(m)[0] == "share" for m in mosts):
                    past += " -- a share of none is none: from none an entry names a count (`+1`)"
                failures.append(past)
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


def io_named(counts, skip=()):
    """A cell naming each count of `counts` that is not none, `syncfs 2,
    msync 1`, or `-`."""
    said = [f"{k} {v:,}" for k, v in counts.items() if v and k not in skip]
    return ", ".join(said) or "-"


def io_table(results, rows):
    """The I/O judgement as Markdown, a step summary's: each workload's
    counts in total, `base → this tree` where they differ, the ratios a
    reader weighs them by, and each file's counts -- each sync call by its
    name, by which with bytes written the files are judged."""
    out = [
        f"### I/O a workload, base against this tree ({results.get('runs', '?')} runs a side of each "
        "side's fennec-server under `strace -f`, every run of a side alike)",
        "",
        "| workload | answered | rows | bytes sent | file after setup | fsyncs | fsyncs an answer | writebacks "
        "| renames | created | truncated | linked | unlinked | write calls | bytes written | unsynced at an answer "
        "| written a byte sent | written a byte of the file | verdict |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]

    def cell(sides, key, of=lambda r, k: r.get(k, 0)):
        vals = [of(r, key) for r in sides]
        return " → ".join(f"{v:,}" if isinstance(v, int) else str(v) for v in dict.fromkeys(vals)) if vals else "-"

    for name, n, b, h, verdict, _ in rows:
        sides = [r for r in (b, h) if r]

        def ratio(num, den, digits):
            vals = [f"{r[num] / r[den]:,.{digits}f}" if r[den] else "-" for r in sides]
            return " → ".join(dict.fromkeys(vals)) or "-"

        counts = " | ".join(
            cell(sides, k) for k in ("writebacks", "renames", "creates", "truncates", "links", "unlinks", "writes",
                                     "written", "unsynced")
        )
        out.append(
            f"| {name} | {cell(sides, 'answered')} | {cell(sides, 'rows')} | {cell(sides, 'sent')} "
            f"| {cell(sides, 'file_before')} | {cell(sides, 'fsyncs')} | {ratio('fsyncs', 'answered', 2)} "
            f"| {counts} | {ratio('written', 'sent', 3)} | {ratio('written', 'file_before', 3)} | {verdict} |"
        )
    def file_cells(c):
        """A file's counts as its row gives them, a cell each."""
        calls = c.get("fsync_calls") or {}
        failed = {f"failed {call}": n for call, n in sorted((c.get("failed_calls") or {}).items())}
        return (
            c.get("fsyncs", 0), calls.get("fsync", 0), calls.get("fdatasync", 0),
            io_named(calls, skip=("fsync", "fdatasync")), io_named(c.get("writeback_calls") or {}),
            c.get("written", 0), c.get("writes", 0), c.get("unsynced", 0), c.get("renames", 0),
            io_named({**{said: c.get(k, 0) for k, said in (("creates", "created"), ("truncates", "truncated"),
                                                           ("links", "linked"), ("unlinks", "unlinked"))}, **failed}),
        )

    files = []
    for name, _, b, h, _, _ in rows:
        sides = [r for r in (b, h) if r]
        for f in sorted({f for r in sides for f in (r.get("files") or {})}):
            each = [file_cells((r.get("files") or {}).get(f) or {}) for r in sides]
            cells = [cell(each, i, lambda r, i: r[i]) for i in range(len(each[0]))]
            files.append(f"| {name} | `{f}` | " + " | ".join(cells) + " |")
    if files:
        out += [
            "",
            "Each file's counts, named within the data directory -- the database, its `.sync` log, the file a "
            "compact writes, `.` the directory; `(file system)` a `syncfs`'s, `(every file)` a `sync`'s, "
            "`(mapped)` an `msync` of no file's mapping -- each sync and each failure judged by its call and its "
            "file, bytes written and unsynced at an answer file by file:",
            "",
            "| workload | file | fsyncs | fsync | fdatasync | other syncs | writebacks | bytes written | write calls "
            "| unsynced at an answer | renamed to it | created, truncated, linked, unlinked, failed |",
            "|---|---|---:|---:|---:|---|---|---:|---:|---:|---:|---|",
            *files,
        ]
    out += [
        "",
        "Any count of this tree's that is not the base's fails, up or down -- fewer fsyncs a durable "
        "write is durability lost -- unless the pull request adds a PERF_ACCEPTED entry (tools/perf_gate.py) "
        "naming the workload, the count -- the file for a sync, bytes written or unsynced or a failure, the "
        "call for a sync or a failure -- and the move it lets through, exactly, which the owner approves: "
        "docs/design/benchmarks.md#the-gate. Counted from the server taking the "
        "workload's connection to its taking the driver's next: the syncs that make a file durable (fsync, "
        "fdatasync, syncfs, sync, msync with MS_SYNC) and the writebacks that make nothing durable "
        "(sync_file_range, msync without MS_SYNC), each by its call; renames; the calls writing to files "
        "(write, pwrite64, writev, pwritev, pwritev2, copy_file_range, sendfile, splice, fallocate) and the "
        "bytes they returned -- never a socket's or a pipe's, the log being a pipe; *unsynced at an answer*, "
        "the bytes written to a file and not yet made durable when the server next wrote to a socket, each "
        "byte once; files created (an open with O_CREAT or O_TMPFILE, creat, mkdir, mknod, symlink), "
        "truncated (ftruncate, truncate, an open with O_TRUNC), linked and unlinked; the calls that failed, "
        "each by its call and file; *answered* and *bytes sent* are the client's, *file after setup* the "
        "database's size once the setup was answered, judged as a count is.",
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
        try:
            with open(args.results, encoding="utf-8") as f:
                results = json.load(f)
        except OSError as e:
            # A count that failed left none: said, never a traceback.
            raise ValueError(
                f"no results to judge at {args.results} ({e.strerror or e}): the count that writes them "
                "failed or never ran, and its log says why"
            ) from None
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
    except (ValueError, OSError) as e:
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
    # The path as git gave it (`diff_paths`), its bytes that are not UTF-8
    # handed back as they came whatever the locale; git's error, which
    # quotes them, is never decoded. The file reads as it did through
    # `text=True` under the runner's UTF-8 locale, its line ends `\n`.
    p = subprocess.run(["git", "show", f"{ref}:{path}".encode("utf-8", "surrogateescape")],
                       cwd=ROOT, capture_output=True)
    return p.stdout.decode("utf-8").replace("\r\n", "\n").replace("\r", "\n") if p.returncode == 0 else ""


def diff_paths(base, head):
    """Every path a change from `base` to `head` touches, a rename as both
    of its paths, each as git stores it: without `-z` git quotes a path
    holding a byte past ASCII, a `"`, a `\\` or a control character
    (`".github/workflows/\\303\\274.yml"`), which no test of a prefix here
    reads as the place it is. Decoded as UTF-8, each byte that is no UTF-8
    kept as a lone surrogate (`surrogateescape`): such a path is never
    dropped nor refused, its ASCII prefix reads as written, `show` hands
    git its bytes back, and a note writes them escaped (`path_said`). Only
    the empty piece after the last NUL is left out."""
    out = subprocess.run(["git", "diff", "--name-only", "-z", "--no-renames", base, head],
                         cwd=ROOT, capture_output=True, check=True).stdout
    return [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]


def path_said(path):
    """`path` as a note writes it, on one line and as it is: a character no
    line shows as itself -- a control, a separator, a format character such
    as a direction's override -- escaped, a byte that is not UTF-8 as
    `\\xNN`, and a `\\` doubled so no escape reads as one."""
    out = []
    for c in path:
        if c == "\\":
            out.append("\\\\")
        elif 0xDC80 <= ord(c) <= 0xDCFF:
            out.append(f"\\x{ord(c) - 0xDC00:02x}")
        elif c.isprintable():
            out.append(c)
        else:
            out.append(c.encode("unicode_escape").decode("ascii"))
    return "".join(out)


def reaches(paths):
    """Whether a change of `paths` may reach what the gate builds: anything
    but the places UNREAD names and Markdown does, and so does a change
    whose paths could not be read (none)."""
    if not paths:
        return True
    return any(not (p.startswith(UNREAD) or p.endswith(".md")) for p in paths)


# YAML's line breaks, CR LF, CR and LF, and no other: `str.splitlines`
# breaks on \v, \f, \x1c-\x1e, \x85, U+2028 and U+2029 too, which YAML 1.2
# reads as a line's own characters, so a `# ; curl ... | sh` after one was
# read here as a comment on a line of its own, an edit of a script unnamed.
YAML_BREAK = re.compile(r"\r\n|\r|\n")
# What Python's strings take for a break or a space and YAML 1.2 does not:
# those eight, \x1f, U+00A0, U+1680, U+2000-U+200A, U+202F, U+205F and
# U+3000, which `strip()` and `\s` here read otherwise than YAML, and on
# which readers of YAML differ (YAML 1.1 broke a line at \x85, U+2028 and
# U+2029): a line one reads as a comment another may run, so where a
# workflow holds one its every edit is named, as where it reads an alias.
UNYAML = re.compile(r"[^\S \t\r\n]")
# A block scalar's header, `|`, `>-`, `|2+`: its lines are the scalar's,
# a `#` among them a script's, never a comment.
YAML_BLOCK = re.compile(r"[|>](?:[1-9][+-]?|[+-][1-9]?)?(?=[ \t]|$)")
# A mapping's key at a line's start, plain or quoted, and the text after
# its `:`.
YAML_KEY = re.compile(r"""^ *(?:-[ \t]+)*(?:"((?:[^"\\]|\\.)*)"|'((?:[^']|'')*)'|([^\s"'#&*!|>\[\]{},-][^#]*?|-[^\s#][^#]*?))[ \t]*:(?:[ \t]+(.*))?$""")


def yaml_closed(line, i, quote):
    """Where the scalar `quote` opened, running on from `i`, closes in
    `line`: the index after its closing quote, or None where it runs on to
    the next line -- `\\` escaping a character between double quotes, `''`
    a quote between single ones."""
    while i < len(line):
        c = line[i]
        if quote == '"' and c == "\\":
            i += 2
            continue
        if c == quote:
            if quote == "'" and line[i + 1:i + 2] == "'":
                i += 2
                continue
            return i + 1
        i += 1
    return None


def yaml_lines(text):
    """A workflow's lines as YAML reads them, ([(line, kind)], aliases):
    each line up to its comment, its trailing spaces aside -- a block
    scalar's kept as written, its trailing spaces its own -- and `kind`
    "node" where a node of the document's structure begins it, "scalar"
    where it is a scalar's text run on from a line before. A line holding nothing
    -- a comment, or blank -- is left out, but where it is a scalar's own:
    inside a block scalar (`run: |`, whose `#` lines are the script's),
    inside a quoted one, and a blank line in a plain or a flow one running
    on, which YAML folds to a break. A `#` begins a comment at a line's
    start or after a space, outside a quoted scalar and a block one, so an
    edit of a comment changes no line here and an edit of a key's value
    does. `aliases`, every alias read (`*name`, `<<: *name`): through one,
    a part of the workflow takes another's lines. Lines end where YAML's
    do (`YAML_BREAK`), never where `str.splitlines` alone ends one."""
    out, aliases, held = [], set(), []
    block = quote = plain = opened = None
    flow, blanks, expect = 0, 0, True
    rows = YAML_BREAK.split(text)
    if rows and rows[-1] == "":
        rows.pop()
    for raw in rows:
        line = raw.rstrip()
        indent = len(line) - len(line.lstrip(" "))
        if block is not None:
            # A block scalar's lines are those deeper than the key or the
            # entry that holds it, as written -- a trailing space is its
            # own (a script's `\ ` is no line's continuation) -- and its
            # blank lines but the last ones.
            if not line.strip():
                held.append((raw, "scalar"))
                continue
            if indent > block:
                out += held + [(raw, "scalar")]
                held = []
                continue
            block, held = None, []
        if quote is not None:
            j = yaml_closed(line, 0, quote)
            if j is None:
                out.append((line, "scalar"))
                continue
            quote, i, expect, last, kind, parent = None, j, False, "quoted", "scalar", -1
        else:
            if not line.strip():
                if flow or plain is not None:
                    blanks += 1
                continue
            if line.lstrip().startswith("#"):
                # A comment ends a plain scalar: a blank before it is none.
                plain, blanks = None, 0
                continue
            if not flow and indent == 0 and (line in ("---", "...") or line.startswith(("--- ", "%"))):
                out.append((line, "node"))
                plain = opened = None
                expect, blanks = True, 0
                continue
            if plain is not None and indent > plain and not flow:
                # A plain scalar's next line, held by the key that holds it.
                out += [("", "scalar")] * blanks
                expect, last, kind, parent = False, "plain", "scalar", plain
            else:
                if flow:
                    out += [("", "scalar")] * blanks
                plain, last, kind = None, None, "node"
                expect = expect if flow else True
                parent = opened if opened is not None else -1
            blanks, i = 0, indent
        start, end = indent, len(line)
        while i < len(line):
            ch = line[i]
            if ch in " \t":
                i += 1
                continue
            if ch == "#" and (i == 0 or line[i - 1] in " \t"):
                end = i
                if last == "plain":
                    last = "ended"
                break
            if expect:
                nxt = line[i + 1:i + 2]
                if ch in "-?" and not flow and nxt in ("", " ", "\t"):
                    # A sequence's entry, or a key of its own: a node follows.
                    parent, i = i, i + 1
                    continue
                if ch in "&!":
                    # An anchor or a tag, then the node.
                    while i < len(line) and line[i] not in " \t" + (",[]{}" if flow else ""):
                        i += 1
                    continue
                if ch == "*":
                    j = i + 1
                    while j < len(line) and line[j] not in " \t,[]{}":
                        j += 1
                    aliases.add(line[i + 1:j])
                    i, expect, last, start = j, False, "alias", i
                    continue
                if ch in "\"'":
                    start = i
                    j = yaml_closed(line, i + 1, ch)
                    if j is None:
                        quote = ch
                        break
                    i, expect, last = j, False, "quoted"
                    continue
                if ch in "|>" and not flow:
                    m = YAML_BLOCK.match(line, i)
                    if m:
                        block = parent
                        rest = line[m.end():].strip()
                        end = m.end() if not rest or rest.startswith("#") else len(line)
                        break
                if ch in "[{":
                    flow, i = flow + 1, i + 1
                    continue
                start, expect, last = i, False, "plain"
                i += 1
                continue
            if flow and ch == ",":
                expect, last, i = True, None, i + 1
                continue
            if flow and ch in "]}":
                flow, last, i = max(flow - 1, 0), None, i + 1
                continue
            if ch == ":" and (line[i + 1:i + 2] in ("", " ", "\t") or (flow and line[i + 1:i + 2] in ",[]{}")):
                # A key ended: its value follows, held by the key's column.
                parent, expect, last, i = start, True, None, i + 1
                continue
            i += 1
        out.append((line[:end].rstrip(), kind))
        if block is None and quote is None and not flow:
            # A node left for the next line (`key:`, `-`) is held by this
            # line's key or entry; a plain scalar may run on beneath it.
            opened = parent if expect else None
            plain = parent if last == "plain" else None
    return out, aliases


def yaml_key(line):
    """The key a node's line begins with, unquoted, and the text after its
    `:` -- (None, None) where it begins with none."""
    m = YAML_KEY.match(line)
    if not m:
        return None, None
    key = m.group(1) if m.group(1) is not None else m.group(2) if m.group(2) is not None else m.group(3)
    if m.group(2) is not None:
        key = key.replace("''", "'")
    return key.strip(), (m.group(4) or "").strip()


def workflow_parts(text):
    """A workflow's parts as YAML reads them (`yaml_lines`): {"keys": {key:
    its lines}, the top level's keys but `jobs`, "jobs": {id: its lines},
    "inline": the jobs whose header holds more than the id, "aliases":
    every alias read}. A job's lines are those beneath its header, and the
    header where it holds more; a key read twice keeps both its lines, so
    neither hides the other."""
    lines, aliases = yaml_lines(text)
    keys, jobs, inline = {}, {}, set()
    cur, in_jobs, level = None, False, None
    for line, kind in lines:
        indent = len(line) - len(line.lstrip(" "))
        if kind == "node" and indent == 0:
            key, rest = yaml_key(line)
            in_jobs = key == "jobs"
            level = None
            cur = keys.setdefault(key if key is not None else "(the document)", [])
            if not in_jobs or rest:
                cur.append(line)
            cur = None if in_jobs and not rest else cur
            continue
        if in_jobs and kind == "node" and (level is None or indent <= level):
            level = indent
            key, rest = yaml_key(line)
            job = key if key is not None else line.strip()
            cur = jobs.setdefault(job, [])
            if key is None or rest:
                cur.append(line)
                inline.add(job)
            continue
        if cur is None:
            cur = keys.setdefault("jobs", [])
        cur.append(line)
    return {"keys": {k: "\n".join(v) for k, v in keys.items()},
            "jobs": {k: "\n".join(v) for k, v in jobs.items()}, "inline": inline, "aliases": aliases}


def jobs_of(text, ids=JOBS):
    """The jobs `ids` of a workflow's `text`, each its lines as YAML reads
    them, comments and blank lines aside (`workflow_parts`): {id: lines}, a
    job the text lacks absent."""
    jobs = workflow_parts(text)["jobs"]
    return {k: jobs[k] for k in ids if k in jobs}


def job_property(lines, name):
    """The text after `name:` at a job's own level (its first line's), and
    the lines beneath it: (text, [lines]), or None where the job has none."""
    # Split where `workflow_parts` joined them, never where a character
    # of a line's own (`YAML_BREAK`) would end one.
    rows = lines.split("\n") if lines else []
    if not rows:
        return None
    level = len(rows[0]) - len(rows[0].lstrip(" "))
    for i, line in enumerate(rows):
        if len(line) - len(line.lstrip(" ")) != level:
            continue
        key, rest = yaml_key(line)
        if key == name:
            below = []
            for more in rows[i + 1:]:
                if len(more) - len(more.lstrip(" ")) <= level:
                    break
                below.append(more)
            return rest, below
    return None


def job_needs(lines):
    """The jobs a job's `needs:` names, or None where they cannot be read
    here -- every job then, as any could hand it an output or a file."""
    found = job_property(lines, "needs")
    if found is None:
        return set()
    rest, below = found
    if rest.startswith("[") and rest.endswith("]"):
        items = rest[1:-1].split(",")
    elif rest:
        items = [rest]
    else:
        items = [re.sub(r"^\s*-\s+", "", b) for b in below if re.match(r"^\s*-\s+\S", b)]
        if len(items) != len(below):
            return None
    out = {i.strip().strip("'\"") for i in items if i.strip()}
    return out if all(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", i) for i in out) else None


def job_name(job, lines):
    """The name a job's check is shown by: its `name:` unquoted, the id
    where it has none, None where it cannot be read here (a scalar running
    on, or a block one) -- any name then."""
    found = job_property(lines, "name")
    if found is None:
        return job
    rest, below = found
    if below or not rest or rest[:1] in "|>&!*[{":
        return None
    if rest[:1] in "'\"":
        end = yaml_closed(rest, 1, rest[0])
        if end != len(rest):
            return None
        inner = rest[1:-1]
        return inner.replace("''", "'") if rest[0] == "'" else re.sub(r"\\(.)", r"\1", inner)
    return rest


def matrix_values(lines):
    """Every value a job's `strategy:` writes out -- each key's and each
    entry's, a flow list's items one by one, and "" for a key it lacks --
    which `${{ matrix.<key> }}` can only be one of; None where the matrix
    is not all written out here (an expression, a block or flow mapping, a
    scalar running on to the next line), when it may be anything."""
    found = job_property(lines, "strategy")
    if found is None:
        return {""}
    rest, below = found
    if rest:
        return None
    out = {""}
    for line in below:
        text = line.strip()
        if "${{" in text:
            return None
        while text.startswith("- ") or text == "-":
            text = text[2:].strip()
        key, value = yaml_key(text)
        if key is None:
            if not line.lstrip().startswith("- "):
                return None
            value = text
        if not value:
            continue
        items = [value]
        if value[:1] in "[{":
            # A flow list or mapping on its one line, of plain scalars.
            close = "]" if value[0] == "[" else "}"
            if not value.endswith(close) or any(c in value[1:-1] for c in "[]{}\"'#&*!|>"):
                return None
            items = value[1:-1].split(",")
            if close == "}":
                items = [i.split(":", 1)[1] if ":" in i else "" for i in items]
        elif value[:1] in "|>&*!":
            return None
        for item in items:
            item = item.strip()
            if item[:1] in "'\"":
                if len(item) < 2 or item[-1] != item[0]:
                    return None
                item = item[1:-1].replace("''", "'") if item[0] == "'" else re.sub(r"\\(.)", r"\1", item[1:-1])
            out.add(item)
    return out


def name_in(parts, job):
    """A job's check name as `job_name` reads it and its matrix's values
    (`matrix_values`): (None, None) where its workflow writes the job on its
    header line or reads an alias, either of which could give it any name."""
    if job in parts["inline"] or parts["aliases"]:
        return None, None
    lines = parts["jobs"][job]
    return job_name(job, lines), matrix_values(lines)


def named_like(name, names, values=None):
    """Whether a job's check `name` may be one of `names`: a `${{
    matrix.<key> }}` in it any of `values` (a matrix written out), every
    other `${{ }}` any text, and a name not read (None) any name."""
    if name is None:
        return True
    pieces = re.split(r"(\$\{\{.*?\}\})", name, flags=re.S)
    pattern = ""
    for i, p in enumerate(pieces):
        if i % 2 == 0:
            pattern += re.escape(p)
        elif values is not None and re.fullmatch(r"\$\{\{\s*matrix\.[A-Za-z_][A-Za-z0-9_-]*\s*\}\}", p):
            pattern += "(?:" + "|".join(re.escape(v) for v in sorted(values)) + ")"
        else:
            pattern += ".*"
    return any(re.fullmatch(pattern, n, re.I | re.S) for n in names)


def gate_jobs(parts, starts=JOBS):
    """The jobs `starts` of a workflow's parts and every job they need, on
    and on: {id: why it is the gate's}. A `needs:` not read here takes in
    every job."""
    jobs = parts["jobs"]
    out = {j: "the gate's own job" for j in starts if j in jobs}
    todo = list(out)
    while todo:
        job = todo.pop()
        needs = None if job in parts["inline"] else job_needs(jobs[job])
        for n in sorted(set(jobs) if needs is None else needs):
            if n not in out:
                out[n] = f"a job the gate's `{job}` needs, which can hand it an output or a file"
                todo.append(n)
    return out


def named_jobs_edited(was, now, names, skip=(), keys=True):
    """The jobs of a workflow's two sides whose check may be shown under one
    of `names` (`named_like`, or a gate job's id) and whose lines are
    edited, or those of a job they need, or -- with `keys` -- the
    workflow's own keys outside its jobs, which reach all of them; every
    such job where the workflow reads an alias and is edited at all."""
    aliased = bool(was["aliases"] or now["aliases"]) and (was["keys"], was["jobs"]) != (now["keys"], now["jobs"])
    out = set()
    for parts in (was, now):
        for j in parts["jobs"]:
            if j in skip or j in out:
                continue
            name, values = name_in(parts, j)
            if not (j in JOBS or named_like(name, names, values)):
                continue
            chain = set(gate_jobs(was, (j,))) | set(gate_jobs(now, (j,)))
            if aliased or (keys and was["keys"] != now["keys"]) or \
                    any(was["jobs"].get(c) != now["jobs"].get(c) for c in chain):
                out.add(j)
    return sorted(out)


def local_uses(texts):
    """The paths the jobs' `uses: ./...` name -- a local action or a
    workflow of the checkout's own -- each without its `./`."""
    out = set()
    for text in texts:
        for m in re.finditer(r"""^\s*(?:-\s+)?uses:\s*["']?\./([^\s"'@]*)""", text, re.M):
            out.add(m.group(1).rstrip("/"))
    return out


def workflow_edits(base, head, changed):
    """What a change does to the workflow the gate runs in, a line each for
    the owner: a job of the gate's, or one it needs, added, edited or
    removed; the workflow outside its `jobs:` -- its `env`, `defaults`,
    `permissions`, `concurrency`, `on` and the rest, which reach every
    step of the gate's jobs -- edited; a local action or workflow one of
    them uses; and a job of any workflow whose check may be shown under a
    gate job's name, edited as `named_jobs_edited` reads it (an expression
    in a name may stand for any text, `release.yml`'s `${{ matrix.name }}`
    among them, so its job is named where its own lines change, not at
    every edit of its file). Compared as YAML reads them, so a comment's
    edit is none; and every edit where the workflow reads an alias,
    through which any of its lines may reach the gate's, or where a
    workflow holds a character of `UNYAML`'s, a comment's edit too."""
    was_text, now_text = show(base, WORKFLOW), show(head, WORKFLOW)
    was, now = workflow_parts(was_text), workflow_parts(now_text)
    notes = []
    odd = unyaml_said(was_text, now_text)
    if odd and was_text != now_text:
        notes.append(
            f"`{WORKFLOW}` holds {odd} and is edited: a break or a space to Python and none to YAML, on which "
            "readers of YAML differ, so a line one reads as a comment another may run; every edit of it is the "
            "gate's, which the owner approves"
        )
    if (was["aliases"] or now["aliases"]) and (was["keys"], was["jobs"]) != (now["keys"], now["jobs"]):
        notes.append(
            f"`{WORKFLOW}` reads a YAML alias (`*{sorted(was['aliases'] | now['aliases'])[0]}`) and is edited: through "
            "an alias any of its lines may reach the gate's jobs, so every edit of it is the gate's, which the owner "
            "approves"
        )
    ours = {**gate_jobs(now), **gate_jobs(was)}
    for job in sorted(ours, key=lambda j: (j not in JOBS, JOBS.index(j) if j in JOBS else 0, j)):
        a, b = was["jobs"].get(job), now["jobs"].get(job)
        if a != b:
            how = "added" if a is None else "removed or renamed" if b is None else "edited"
            notes.append(f"the `{job}` job of `{WORKFLOW}` is {how}: {ours[job]}, which the owner approves")
    keys = sorted(k for k in set(was["keys"]) | set(now["keys"]) if was["keys"].get(k) != now["keys"].get(k))
    if keys:
        notes.append(
            f"`{WORKFLOW}` outside its `jobs:` is edited ({', '.join(f'`{k}`' for k in keys)}): the workflow's own "
            "keys, which reach every step of the gate's jobs -- their environment, shell, permissions and triggers "
            "-- the gate's own, which the owner approves"
        )
    for used in sorted(local_uses([*(was["jobs"].get(j, "") for j in ours), *(now["jobs"].get(j, "") for j in ours)])):
        hit = sorted(p for p in changed if not used or p == used or p.startswith(used + "/"))
        if hit:
            notes.append(f"`./{path_said(used)}`, which a job of the gate's uses, is edited (`{path_said(hit[0])}`): "
                         "the gate's own, which the owner approves")
    names = {n for parts in (was, now) for j in JOBS if j in parts["jobs"]
             for n in (j, name_in(parts, j)[0]) if n is not None}
    for path in sorted(p for p in changed if p.startswith(WORKFLOWS) and "/" not in p[len(WORKFLOWS):]
                       and p.endswith((".yml", ".yaml"))):
        if path == WORKFLOW:
            # Its keys outside the jobs are named above, and its gate's jobs.
            like = named_jobs_edited(was, now, names, skip=ours, keys=False)
        else:
            a, b = show(base, path), show(head, path)
            odd = unyaml_said(a, b)
            if odd and a != b:
                notes.append(
                    f"`{path_said(path)}` holds {odd} and is edited: a break or a space to Python and none to YAML, "
                    "on which readers of YAML differ, so a job one reads as a comment another may show under the "
                    "gate's name, which the owner approves"
                )
                continue
            like = named_jobs_edited(workflow_parts(a), workflow_parts(b), names)
        if like:
            notes.append(
                f"`{path_said(path)}` is edited, and its job `{like[0]}` is named as one of the gate's "
                f"({', '.join(sorted(names))}): "
                "a check shown under the gate's name, which the owner approves"
            )
    return notes


def unyaml_said(*texts):
    """The characters of `UNYAML` that `texts` hold, as a note names them
    (`U+0085, U+2028`), or "" where they hold none."""
    return ", ".join(f"U+{ord(c):04X}" for c in sorted({c for t in texts for c in UNYAML.findall(t)}))


def gate_edits(base, head):
    """What a change from `base` to `head` does to the gate's own files, a
    line each for the owner: the program or the checker edited, what the
    workflow runs the gate's jobs with edited (`workflow_edits`), and every
    PERF_ACCEPTED entry added, edited or removed."""
    changed = set(diff_paths(base, head))
    notes = [
        f"`{path}` is edited: one of the gate's own files, which the owner approves"
        for path in (PROGRAM, CHECKER)
        if path in changed
    ]
    notes += workflow_edits(base, head, changed)
    old, new = show(base, CHECKER), show(head, CHECKER)
    old = accepted_text(old, f"{base}:{CHECKER}") if old else []
    new = accepted_text(new, f"{head}:{CHECKER}") if new else []
    for e in added(new, old):
        notes.append(f"PERF_ACCEPTED gains an entry: {entry_said(e)} -- {e.get('reason')}")
    for e in added(old, new):
        notes.append(f"PERF_ACCEPTED loses an entry, the list's history cut or an entry edited: {entry_said(e)}")
    return notes


def entry_said(e):
    """Entry `e` as the owner reads it: its workload, what it lets through
    -- the file and the call too where it names them, the I/O's judged by
    both -- and how far."""
    what = e.get("metric", "instructions")
    if isinstance(e, dict) and e.get("file") is not None:
        what += f" of `{e['file']}`"
    if isinstance(e, dict) and e.get("call") is not None:
        what += f" by {e['call']}"
    return f"`{e.get('workload')}`, {what} up to {e.get('most')}"


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
# the counted stretch; every call that writes, syncs or starts a writeback;
# every call that renames, makes, truncates, links or unlinks a file; the
# writes to a socket -- `writev` and the rest above, and the sends -- which
# are the server's answers (IO_SENDS); the opens and `fcntl`, read for a
# flag that makes every write durable or passes the cache by
# (IO_FLAGS_REFUSED), and `ioctl`, for a request that does
# (IO_IOCTLS_REFUSED); and the calls whose writes no call shows
# (IO_REFUSED). Each goes as `?name`, which strace passes over where the
# architecture has no such call (aarch64 has no `rename`, no `open`,
# x86_64's musl calls both). A call missing here is a write the gate cannot
# see: an added one would pass on counts that never moved. The mappings
# are not asked for: `mmap`, `mremap` and `mprotect` were 4 778 of the
# 6 966 lines of the four workloads' traces, the allocator's (3.2 times
# the lines; 1.4 times in the counted stretch), so a store into a shared
# mapping of a file is a write the gate cannot see.
IO_CALLS = (
    "accept", "accept4",
    "write", "pwrite64", "writev", "pwritev", "pwritev2", "copy_file_range", "sendfile", "splice", "fallocate",
    "sendto", "sendmsg", "sendmmsg",
    "fsync", "fdatasync", "syncfs", "sync", "msync", "sync_file_range", "sync_file_range2",
    "rename", "renameat", "renameat2", "open", "openat", "openat2", "creat", "mkdir", "mkdirat",
    "mknod", "mknodat", "symlink", "symlinkat",
    "truncate", "ftruncate", "link", "linkat", "unlink", "unlinkat", "rmdir",
    "fcntl", "ioctl", "io_uring_setup", "io_setup", "io_submit",
)
IO_ACCEPTS = frozenset({"accept", "accept4"})
# The syncs that make a file durable (`fsyncs`), each counted by its name
# and the file it names: `syncfs` syncs a whole file system, `(file
# system)`; `msync` names no descriptor and is a mapped file's, `(mapped)`,
# a durability only with MS_SYNC -- MS_ASYNC asks for nothing the kernel
# does not do anyway; `sync` syncs everything, `(every file)`. These alone
# meet a floor.
IO_DURABLE = frozenset({"fsync", "fdatasync", "syncfs", "sync", "msync"})
# The calls that start a writeback and make nothing durable
# (`writebacks`): `sync_file_range`, whatever its flags -- it flushes
# neither the drive's cache nor the file's metadata -- arm's
# `sync_file_range2`, and an `msync` without MS_SYNC. Counted, each by its
# name and file, so one in an fsync's place is two moves and under a
# floor, never a sync.
IO_WRITEBACKS = frozenset({"sync_file_range", "sync_file_range2", "msync"})
IO_SYNCS = IO_DURABLE | IO_WRITEBACKS
IO_UNNAMED = {"msync": "(mapped)", "sync": "(every file)", "syncfs": "(file system)"}
IO_RENAMES = frozenset({"rename", "renameat", "renameat2"})
# A call writing to a file, and which of its arguments is the descriptor
# written to: copy_file_range's and splice's third, the rest's first.
# `fallocate` gives a file its room and returns no bytes: a write call, its
# bytes none. To a socket, any of them is an answer.
IO_WRITES = {
    "write": 0, "pwrite64": 0, "writev": 0, "pwritev": 0, "pwritev2": 0, "copy_file_range": 2, "sendfile": 0,
    "splice": 2, "fallocate": 0,
}
# The calls that write to a socket alone: an answer, as a write call to a
# socket is (`unsynced`). std's `TcpStream::write` is a `send`, which
# Linux takes as `sendto`.
IO_SENDS = frozenset({"sendto", "sendmsg", "sendmmsg"})
# The opens. One with O_CREAT or O_TMPFILE, or `creat`, counts as a file
# created -- whether or not it was there, which the trace cannot tell and
# a run repeats -- and one with O_TRUNC, or `creat`, as one truncated.
IO_OPENS = frozenset({"open", "openat", "openat2", "creat"})
IO_MADE = frozenset({"O_CREAT", "O_TMPFILE", "__O_TMPFILE"})
# The directory's other changes, each counted in total as its kind: a
# node or a symbolic link made is a file made.
IO_CHANGES = {
    "mkdir": "creates", "mkdirat": "creates", "mknod": "creates", "mknodat": "creates", "symlink": "creates",
    "symlinkat": "creates", "truncate": "truncates", "ftruncate": "truncates",
    "link": "links", "linkat": "links", "unlink": "unlinks", "unlinkat": "unlinks", "rmdir": "unlinks",
}
# Every call counted in the stretch, and so every call a failure is
# counted by (`failed`, by its call and file as the syncs are).
IO_COUNTED = IO_SYNCS | set(IO_WRITES) | IO_RENAMES | IO_OPENS | set(IO_CHANGES)
IO_CALLS_OF = {"fsyncs": IO_DURABLE, "writebacks": IO_WRITEBACKS, "failed": frozenset(IO_COUNTED)}
# What makes a write durable, or costs its bytes otherwise, with the same
# calls -- an open of a file in the data directory with O_SYNC or O_DSYNC
# (or the bit O_SYNC adds to it, `__O_SYNC`, alone), whose every write
# flushes the device, or O_DIRECT (by `fcntl` too), which passes the page
# cache by; a `pwritev2` with RWF_SYNC or RWF_DSYNC -- is refused anywhere
# in the trace, where it took: the counts would read the same.
IO_FLAGS_REFUSED = frozenset({"O_SYNC", "__O_SYNC", "O_DSYNC", "O_DIRECT", "RWF_SYNC", "RWF_DSYNC"})
# An `ioctl` on a file of the data directory that makes its writes durable
# or writes it with no call the trace counts: refused as a flag is.
IO_IOCTLS_REFUSED = {
    "FS_IOC_SETFLAGS": "sets the file's attributes (`chattr`: FS_SYNC_FL makes every write durable)",
    "FS_IOC32_SETFLAGS": "sets the file's attributes (`chattr`: FS_SYNC_FL makes every write durable)",
    "FS_IOC_FSSETXATTR": "sets the file's attributes (FS_XFLAG_SYNC makes every write durable)",
    "FICLONE": "writes the file's blocks by no write call",
    "FICLONERANGE": "writes the file's blocks by no write call",
}
# A call whose writes and syncs no call of the trace shows -- io_uring's
# ring is read by the kernel, its submissions no system call each; Linux
# AIO's are submitted in batches, their completions no call either -- so a
# server that sets one up anywhere in the trace is refused, never counted:
# the gate learns the call first.
IO_REFUSED = frozenset({"io_uring_setup", "io_setup", "io_submit"})
# What else each run of a side must agree on, and both sides: the client's
# requests answered, and rows and bytes sent in the run and in the setup.
IO_SENT = ("answered", "rows", "sent", "setup_sent")
# The server: the workload's file, a port the system picks (its `listening
# on` line names it), every write durable before it is answered, no
# compaction of its own -- looked for every 5 s, by the clock -- and the one
# shutdown once its stdin closes, which also ends a server whose driver
# died, so none is left behind.
IO_SERVER = ("--http", "127.0.0.1:0", "--sync", "always", "--auto-compact", "off", "--exit-on-stdin-close")
# A phase that takes longer has hung.
IO_TIMEOUT = 300
# Where a counted call's file is not in the data directory: a run that
# counts one is refused, since a data directory the trace names otherwise
# -- through a link -- would put every file there on both sides.
IO_OUTSIDE = "(outside the data directory)"

STRACE_LINE = re.compile(r"^(?:\[pid\s+)?(\d+)\]?\s+(.*)$")
STRACE_RESUMED = re.compile(r"^<\.\.\. (\w+) resumed>(.*)$")
STRACE_CALL = re.compile(r"^(\w+)\((.*)$")
# A call's arguments and what it returned, after the last `) =`: an int
# (in hex too, `0x4002`), `-1 EBADF (...)`, a descriptor with its path, or
# `?` where the process ended inside the call.
STRACE_RETURN = re.compile(r"^(.*)\)\s+=\s+(\S.*)$")
# A descriptor as `-y` shows it, `3</data/io.fennec>`, `5<socket:[812]>`,
# and the working directory a relative path of renameat's is in,
# `AT_FDCWD</data>`.
STRACE_FD = re.compile(r"(-?\d+|AT_FDCWD)<([^<>]*)>")
UNFINISHED = "<unfinished ...>"


def strace_number(text):
    """A number as strace prints it, `4096` or `0xffff8a2c1000`."""
    text = text.strip()
    return int(text, 16) if text.lower().startswith(("0x", "-0x")) else int(text)


def strace_calls(lines):
    """The calls a `strace -f -y` trace holds, (name, arguments, return,
    the returned descriptor's path) -- the return an int, None where the
    process ended inside the call, the path None
    where it returned none -- in the order they returned: a call another
    thread's interleaved, written `<unfinished ...>` where it began and
    `<... name resumed>` where it returned, is joined by its thread's id
    and yielded where it returned. Exits and signals are passed over, and
    so is a call resumed whose beginning the trace does not hold."""
    for _, _, *call in strace_placed(lines):
        yield tuple(call)


def strace_placed(lines):
    """The calls of `strace_calls`, each with the line it began on and the
    one it returned on, in the order they returned: (began, returned, name,
    arguments, return, path) -- one line both where strace printed the
    call whole."""
    begun = {}
    for at, raw in enumerate(lines):
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
            name, text, began = start[0], start[1] + r.group(2), start[2]
        else:
            c = STRACE_CALL.match(body)
            if not c:
                continue
            name, text = c.groups()
            began = at
            if text.endswith(UNFINISHED):
                begun[tid] = (name, text[: -len(UNFINISHED)].rstrip(), at)
                continue
        # strace pads before the `=` where a call was resumed or its
        # arguments were short: `<... write resumed>)        = 100`.
        done = STRACE_RETURN.match(text)
        if not done:
            continue
        v = re.match(r"-?(?:0x[0-9a-fA-F]+|\d+)", done.group(2))
        fd = STRACE_FD.match(done.group(2))
        yield began, at, name, done.group(1), strace_number(v.group()) if v else None, fd.group(2) if fd else None


def strace_args(args):
    """A call's arguments as strace prints them, one string each: split
    where a comma stands outside a string, a bracket, a brace, a
    parenthesis, a comment and a descriptor's path (`5<socket:[2]>`)."""
    parts, cur, depth, i = [], "", 0, 0
    while i < len(args):
        ch = args[i]
        if ch == '"':
            j = i + 1
            while j < len(args) and args[j] != '"':
                j += 2 if args[j] == "\\" else 1
            cur, i = cur + args[i:j + 1], j + 1
            continue
        if args.startswith("/*", i):
            j = args.find("*/", i + 2)
            j = len(args) if j < 0 else j + 2
            cur, i = cur + args[i:j], j
            continue
        if ch == "<" and re.search(r"(?:\d|AT_FDCWD)$", cur):
            j = args.find(">", i)
            j = len(args) if j < 0 else j + 1
            cur, i = cur + args[i:j], j
            continue
        if ch in "[{(":
            depth += 1
        elif ch in "]})":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(cur.strip())
            cur, i = "", i + 1
            continue
        cur, i = cur + ch, i + 1
    if cur.strip():
        parts.append(cur.strip())
    return parts


def strace_fd(arg):
    """An argument as a descriptor, (its number, the path `-y` named it
    by, or None where it named none) -- None where the argument is no
    descriptor."""
    m = re.fullmatch(r"(-?\d+|AT_FDCWD)(?:<(.*)>)?", arg.strip(), re.S)
    return (m.group(1), m.group(2)) if m else None


def strace_paths(args):
    """Each path a call's arguments name, in their order, joined to the
    directory whose descriptor stands just before it where it is relative:
    None for a relative one whose directory strace did not name -- a bare
    descriptor, or none, the working directory."""
    parts = strace_args(args)
    out = []
    for i, a in enumerate(parts):
        m = re.fullmatch(r'"([^"]*)"(?:\.\.\.)?', a)
        if not m:
            continue
        path = m.group(1)
        if not path.startswith("/"):
            d = strace_fd(parts[i - 1]) if i else None
            path = d[1].rstrip("/") + "/" + path if d and d[1] else None
        out.append(path)
    return out


def strace_path(args):
    """The last path a call's arguments name -- the new name of a rename
    or a link, the one an open, an unlink or a truncate names -- joined to
    the directory whose descriptor stands before it where it is relative:
    "" where there is none, or no directory to join it to."""
    paths = strace_paths(args)
    return (paths[-1] or "") if paths else ""


def strace_flags(args, after_path=True):
    """The flags a call's arguments spell, `O_RDWR|O_CREAT` and the like
    -- `__O_SYNC` too -- as a set of names: those after its last path where
    it names one, so no name in a path is read as a flag."""
    quoted = list(re.finditer(r'"([^"]*)"', args))
    tail = args[quoted[-1].end():] if after_path and quoted else args
    return set(re.findall(r"\b_*[A-Z][A-Z0-9]*_[A-Z0-9_]+\b", tail))


def io_counts(lines, data_dir, start=2, end=3):
    """What a server's trace (`lines`) did to files between the `start`th
    connection it took and the `end`th: {fsyncs, writebacks, renames,
    writes, written, unsynced, failed, creates, truncates, links, unlinks,
    files}, and the connections it took in all. A call counts where it
    returned, an answer where it began (below). A descriptor counts as a file where `-y` names it by a path
    outside /dev, /proc and /sys -- never a socket, a pipe (the server's
    log, its stderr, is the driver's pipe) or /dev/null; one unlinked
    since it was opened by its name and ` (deleted)`, its own file. A call
    that failed counts in `failed` alone, by its call and file, its bytes
    none. `unsynced` is the bytes the server had written to a file and not
    made durable when it next wrote to a socket -- an answer -- each byte
    once, at the first answer after it. `files` gives each file's share,
    named within `data_dir` -- the directory as the trace names it, its
    links resolved -- and each sync's call and each failure's. A
    RuntimeError, the run not to be counted: a call of IO_REFUSED anywhere
    in the trace, a flag of IO_FLAGS_REFUSED or a request of
    IO_IOCTLS_REFUSED that took on a file in the data directory, a counted
    call whose descriptor or directory strace did not name, or a count of
    a file outside it."""
    data_dir = data_dir.rstrip("/")

    def file_of(path):
        path, deleted = (path[: -len(" (deleted)")], " (deleted)") if path.endswith(" (deleted)") else (path, "")
        if not path.startswith("/") or path.startswith(("/dev/", "/proc/", "/sys/")):
            return None
        if path == data_dir:
            return "." + deleted
        if path.startswith(data_dir + "/"):
            return path[len(data_dir) + 1:] + deleted
        return IO_OUTSIDE

    out = {k: 0 for k in IO_COUNTS}
    out["files"] = {}

    def share(name, path, f, key, n=1, call=None):
        if f == IO_OUTSIDE:
            raise RuntimeError(
                f"the server's {name} of {path} counts a file outside its data directory {data_dir}: every "
                "file is named within it, or files would be judged as one on both sides"
            )
        c = out["files"].setdefault(f, {
            **{k: 0 for k in IO_FILE_COUNTS}, **{key: {} for key in IO_CALL_KEYS.values()},
        })
        c[key] += n
        out[key] += n
        if call:
            calls = c[IO_CALL_KEYS[key]]
            calls[call] = calls.get(call, 0) + n

    def refuse(name, what, path):
        raise RuntimeError(
            f"the server's {name} of {path} {what}: the trace's calls would read the same, so the gate cannot "
            "judge this server (tools/perf_gate.py, IO_FLAGS_REFUSED)"
        )

    def unnamed(name, what):
        raise RuntimeError(
            f"the server's {name} names {what}, which strace did not name: its file unknown, the run cannot be "
            "judged -- a count dropped would agree with itself on both sides (tools/perf_gate.py, io_counts)"
        )

    # Each file's bytes written in the stretch since its last durable sync
    # or the server's last answer, whichever came later.
    fresh = {}

    def answered():
        for f, n in sorted(fresh.items()):
            if n:
                share("answer", f, f, "unsynced", n)
        fresh.clear()

    # Where each call counts, against the stretch's ends -- the returns of
    # the accepts that took the workload's connection and the driver's
    # next -- and against each other. strace prints a call's beginning as
    # it stops the thread on its way in, before the call runs, and its
    # return as it stops it on its way out, before the thread goes on;
    # another thread's line can fall between the two (`<unfinished ...>`,
    # `<... resumed>`). An answer's bytes reach the client inside the call,
    # and the client connects again once it has read them: on a loaded
    # runner the next accept's return can be printed before the answer's,
    # and placed there the last answer would fall past the stretch
    # (io-batch's unsynced 23,883 -> 0), runs of one build differing on a
    # required check that nothing changed. So an answer counts
    # where it began, which comes before its client can connect. The rest
    # count where they returned, and no other thread's line moves them: an
    # accept takes its connection as it returns (it began waiting long
    # before), and the server hands the connection on only after, so a
    # workload's calls all begin past it; a sync makes a file durable as it
    # returns -- placed where it began, one still running at an answer would
    # pass for done -- and the bytes of a write and the error of a failure
    # are known there; and what the server does for a request returns
    # before that request's answer begins, on the answering thread in turn
    # or on one it waited for, which went on only once strace had printed
    # its return. A call the server did not wait for before answering can
    # fall either side of an end in any reading: then its runs differ, and
    # the judge says so.
    def placed(call):
        began, returned, name, args = call[:4]
        if name in IO_SENDS or name in IO_WRITES:
            parts = strace_args(args)
            i = 0 if name in IO_SENDS else IO_WRITES[name]
            d = strace_fd(parts[i]) if i < len(parts) else None
            if d and d[1] and d[1].startswith("socket:"):
                return began
        return returned

    taken = 0
    for _, _, name, args, ret, shown in sorted(strace_placed(lines), key=placed):
        ok = ret is not None and ret >= 0
        if name in IO_REFUSED:
            # Set up before the stretch, a ring or a context could write in it.
            if ok:
                raise RuntimeError(
                    f"the server called {name}, whose writes and syncs no call of the trace shows: the gate "
                    "must learn it (tools/perf_gate.py, IO_CALLS) before it can count this server"
                )
            continue
        if name in IO_ACCEPTS:
            taken += ok
            continue
        parts = strace_args(args)

        def fd_at(i):
            """The descriptor argument `i` is, (number, path or None), or
            None where it is no descriptor."""
            return strace_fd(parts[i]) if i < len(parts) else None

        def named(i, what=None):
            """The path of the descriptor argument `i` is: "" where it is
            none, the run refused where strace did not name it."""
            d = fd_at(i)
            if d is None:
                return ""
            if d[1] is None:
                unnamed(name, what or f"descriptor {d[0]}")
            return d[1]

        # Refused wherever it took, the stretch or before it: a file opened
        # O_DSYNC at the start makes each later write a device flush.
        if name in IO_OPENS or name == "fcntl" or name == "pwritev2":
            if name in IO_OPENS:
                paths = strace_paths(args)
                path, flags = shown or (paths[-1] if paths else ""), strace_flags(args)
            elif name == "fcntl":
                flags = strace_flags(args, after_path=False) if "F_SETFL" in args else set()
                path = named(0) if ok and flags & IO_FLAGS_REFUSED else ""
            else:
                flags = strace_flags(args, after_path=False)
                path = named(0) if ok and flags & IO_FLAGS_REFUSED else ""
            bad = sorted(flags & IO_FLAGS_REFUSED)
            if ok and bad and path is None:
                unnamed(name, f"a path with {'|'.join(bad)}")
            f = file_of(path) if path else None
            if ok and bad and f not in (None, IO_OUTSIDE):
                refuse(name, f"with {'|'.join(bad)}", path)
        if name == "ioctl":
            request = parts[1] if len(parts) > 1 else ""
            if ok and request in IO_IOCTLS_REFUSED:
                path = named(0)
                if file_of(path) not in (None, IO_OUTSIDE):
                    refuse(name, f"by {request}, which {IO_IOCTLS_REFUSED[request]}", path)
            continue
        if not start <= taken < end:
            continue
        if name in IO_RENAMES:
            paths = strace_paths(args)
            if None in paths:
                unnamed(name, "a path relative to a directory")
            target = paths[-1] if paths else ""
            f = file_of(target) or "(not a file)"
            if not ok:
                share(name, target, f, "failed", call=name)
                continue
            share(name, target, f, "renames")
            # What was written to the old name and not made durable is the
            # new name's now; the file it replaced is gone.
            was = fresh.pop(file_of(paths[0]), 0) if len(paths) > 1 and file_of(paths[0]) else 0
            fresh.pop(f, None)
            if was:
                fresh[f] = was
        elif name in IO_SYNCS:
            if name in IO_UNNAMED:
                f = path = IO_UNNAMED[name]
            else:
                path = named(0)
                f = file_of(path) if path else None
            if f is None:
                continue
            if not ok:
                share(name, path, f, "failed", call=name)
                continue
            durable = name in IO_DURABLE and (name != "msync" or "MS_SYNC" in strace_flags(args, after_path=False))
            share(name, path, f, "fsyncs" if durable else "writebacks", call=name)
            if durable:
                if name in ("sync", "syncfs"):
                    fresh.clear()
                else:
                    fresh.pop(f, None)
        elif name in IO_WRITES:
            path = named(IO_WRITES[name])
            if path.startswith("socket:"):
                if ok:
                    answered()
                continue
            f = file_of(path) if path else None
            if f is None:
                continue
            if not ok:
                share(name, path, f, "failed", call=name)
                continue
            share(name, path, f, "writes")
            share(name, path, f, "written", ret)
            fresh[f] = fresh.get(f, 0) + ret
        elif name in IO_SENDS:
            if ok and named(0).startswith("socket:"):
                answered()
        elif name in IO_OPENS:
            # The file it opened as `-y` names it, else the path it named.
            paths = strace_paths(args)
            path = shown or (paths[-1] if paths else "")
            flags = strace_flags(args) | ({"O_CREAT", "O_TRUNC"} if name == "creat" else set())
            kinds = [k for k, made in (("creates", IO_MADE), ("truncates", {"O_TRUNC"})) if flags & made]
            if not kinds:
                continue
            if path is None:
                unnamed(name, "a path relative to a directory")
            f = file_of(path) if path else None
            if f is None:
                continue
            if not ok:
                share(name, path, f, "failed", call=name)
                continue
            for k in kinds:
                share(name, path, f, k)
        elif name in IO_CHANGES:
            if name == "ftruncate":
                path = named(0)
            else:
                paths = strace_paths(args)
                if paths and paths[-1] is None:
                    unnamed(name, "a path relative to a directory")
                path = paths[-1] if paths else ""
            f = file_of(path) if path else None
            if f is None:
                continue
            if not ok:
                share(name, path, f, "failed", call=name)
                continue
            share(name, path, f, IO_CHANGES[name])
            if IO_CHANGES[name] == "unlinks":
                fresh.pop(f, None)
    return out, taken


def tracing():
    """Whether strace can count here: Linux with strace on the PATH. Where
    it cannot, `io` and `io-measure` say so and count nothing, and exit 0
    -- on a Mac, whose developer runs them in Docker; under CI
    (GITHUB_ACTIONS) that is a RuntimeError: a count that exited 0 with no
    results would leave the judge nothing to judge."""
    if platform.system() == "Linux" and shutil.which("strace") is not None:
        return True
    why = "strace is not installed" if platform.system() == "Linux" else f"this is {platform.system()}"
    if os.environ.get("GITHUB_ACTIONS") == "true":
        raise RuntimeError(f"the I/O workloads are counted on Linux with strace, and {why}: nothing measured")
    print(
        f"perf_gate: the I/O workloads are counted on Linux with strace installed, and {why}; on macOS run "
        "them in Docker (docs/design/benchmarks.md#the-gate). Nothing measured.",
        file=sys.stderr,
    )
    return False


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


def io_data_dir(scratch, name):
    """A new data directory for a run of workload `name` in `scratch`, as
    the trace names it, its links resolved: a scratch directory reached
    through a link (macOS's /tmp, a runner's) would name every file outside
    it, on both sides alike, and fail the run (`io_counts`)."""
    return os.path.realpath(tempfile.mkdtemp(prefix=f"{name}.", dir=scratch))


def io_one_run(server, client, name, requests, scratch, keep=None):
    """Runs I/O workload `name` once against `server`, the server alone
    under `strace -f`: the setup over one connection, the size it left the
    file at, the run over the next, then the driver's own request over a
    third, which ends what is counted, and the server's stdin closed for
    its one shutdown. What the server did to files in between
    (`io_counts`) beside what the client sent. With `keep`, the trace is
    copied there."""
    data_dir = io_data_dir(scratch, name)
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


# The word both jobs choose the checker by, in the shell, no Python of the
# change's run to choose it: the base's own gate.rs naming IO_WORKLOADS
# anywhere, however its table is spelt. tools/test_perf_gate.py holds each
# job's `grep` to `io_checker`.
IO_CHECKER_WORD = "IO_WORKLOADS"


def io_checker(base_program):
    """Which checker counts and judges the I/O, by the base's own gate.rs
    (`base_program`, its text, empty where it had none): "base" wherever
    that program mentions IO_WORKLOADS, "this tree" only where it mentions
    it nowhere -- the pull request that brought the I/O counts, whose base
    had none, counted and judged by its own as the gate's first was. Any
    mention, not a table spelt one way: one anchored on `const` read a
    `pub const`, a `pub(crate) const` or a `static` table as none, and a
    pull request making the table public would have handed every one after
    it to its own checker. A base checker that then cannot count fails the
    step, never the other way. Never by whether the base's checker runs
    `io`: a later change that dropped or broke it would hand every pull
    request after it to its own checker for good, where this fails it.
    Both jobs decide it in the shell, by a `grep` of the same word
    (IO_CHECKER_WORD) that chooses the base's too where it cannot read the
    file, which the tests hold to this: a pull request's own copy of this
    function decides nothing."""
    return "base" if IO_CHECKER_WORD in base_program else "this tree"


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
    except (ValueError, RuntimeError, OSError, subprocess.CalledProcessError) as e:
        print(f"perf_gate: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
