# CLAUDE.md

The benchmarks' invariants, for code in `crates/fennec-bench/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### Benchmarks

**[`make scale-bench` holds fennec-server to pgvector, each over its own protocol.](../../docs/design/benchmarks.md#scale-bench)**
Sides run in turns after idle minutes; a hot run's figure is not one.

**[`make ycsb` is fennecdb as a general database.](../../docs/design/benchmarks.md#ycsb)**
`--verify` checks every answer; `ycsb/results.tsv` is appended to, never
rewritten (a replaced run named in `ycsb/superseded.tsv`), and
`site/build.py` holds every YCSB figure on the site to its median there.
