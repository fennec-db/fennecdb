# AGENTS.md

The benchmarks' invariants, for code in `crates/fennec-bench/`, and the perf gate's, for `tools/perf_gate.py`, `crates/fennec-http/examples/gate.rs`, CI's `perf (count)` and `perf (instructions)` jobs and `.github/workflows/perf-holdout.yml`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Benchmarks

**[`make scale-bench` holds fennec-server to pgvector, each over its own protocol.](../../docs/design/benchmarks.md#scale-bench)**
Sides run in turns after idle minutes; a hot run's figure is not one.

**[`make ycsb` is fennecdb as a general database.](../../docs/design/benchmarks.md#ycsb)**
`--verify` checks every answer. Every figure the site quotes as a
measurement is the median of a `(family, cell, metric)` in
`results/<family>.tsv` (run, commit, machine, date, family, cell, metric,
value), appended to, never rewritten (a replaced run named in
`results/superseded.tsv`), and `site/build.py` fails on one it does not hold;
a figure no run has measured is a `hand` run on `hand-measured`, its commit
the one that first wrote it.

**[A pull request is held to its base in instructions and heap bytes an op, and in I/O to the call.](../../docs/design/benchmarks.md#the-gate)**
CI's `perf (count)` counts each workload of
`crates/fennec-http/examples/gate.rs` under callgrind in the base's static
musl build and the pull request's -- one program, five runs a side, each
tallying what it asked of the allocator -- and `perf (instructions)`, a job
apart, fails a median 1% or more above the base's in instructions -- the
allocator's own functions' self cost counted as the program's tally of
calls at the base's cost a call, never a call line's inclusive cost -- or
in the bytes asked of it: a call's cost moves with the heap's layout, the
calls and bytes with the code. Both jobs run the base's copy of
`tools/perf_gate.py`, and a workload the base's `gate.rs` lists that this
tree dropped, or that the base did not run, fails. A cost taken on purpose
is a `PERF_ACCEPTED` entry the pull request adds -- the workload, its
metric, the most and why -- never the threshold, the runs, `ALLOCATOR` or a
workload changed to pass. The owner approves each entry and each edit of
the gate's own files (`gate.rs`, the checker, the two jobs), which the
judge names: the agent merging such a pull request waits for the owner's
OK. A change is counted unless every path is the site's, the docs', an
integration's, `web/`'s, `studio/`'s, `deploy/`'s or Markdown. A workload
runs in process on one thread from a fixed seed, only its loop counted, the
same count run after run but for a draw well inside the threshold (the base
against itself, 0.1% at most), and stands for a claim in the design file's
table: a hot path a page quotes gets one. An I/O workload (`IO_WORKLOADS`)
is sent by `gate.rs` to each side's `fennec-server` under `--sync always`,
the server alone under `strace -f`, counted from its taking the workload's
connection to the driver's next: the fsync family's calls, renames, the
calls writing to files and their bytes, never a socket's or a pipe's.
Nothing it sends or the server writes then is sized by a clock or a draw,
so every run of a side counts the same to the call, and a count that is
not the base's, up or down, fails unless an entry names it.

**[Two builds' instruction counts](../../docs/design/benchmarks.md#instruction-counts)**
differing by under ~500 a request are read over re-signed copies'
UUIDs or a trace (`make instr-compare`), never from plain pairs: on
macOS the allocator draws its path from a binary's identity.
