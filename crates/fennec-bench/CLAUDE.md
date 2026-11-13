# CLAUDE.md

The benchmarks' invariants, for code in `crates/fennec-bench/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

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
