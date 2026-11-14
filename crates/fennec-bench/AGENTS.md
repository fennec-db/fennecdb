# AGENTS.md

The benchmarks' invariants, for code in `crates/fennec-bench/`, and the perf gate's, for `tools/perf_gate.py`, `crates/fennec-http/examples/gate.rs`, CI's `perf (count)` and `perf (instructions)` jobs and `.github/workflows/perf-holdout.yml`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Benchmarks

**[`make scale-bench` holds fennec-server to pgvector, each over its own protocol.](../../docs/design/benchmarks.md#scale-bench)**
Sides run in turns after idle minutes; a hot run's figure is not one.

**[`make ycsb` is fennecdb as a general database.](../../docs/design/benchmarks.md#ycsb)**
`--verify` checks every answer; `ycsb/results.tsv` is appended to, never
rewritten (a replaced run named in `ycsb/superseded.tsv`), and
`site/build.py` holds every YCSB figure on the site to its median there.

**[A pull request is held to its base in instructions and heap bytes an op.](../../docs/design/benchmarks.md#the-gate)**
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
table: a hot path a page quotes gets one.
