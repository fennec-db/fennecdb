# Benchmarks

The long form of the invariants [crates/fennec-bench/CLAUDE.md](../../crates/fennec-bench/CLAUDE.md) lists under *Benchmarks*: what `make scale-bench` and `make ycsb` measure and how, the two regimes `ycsb` runs in a container under a memory limit -- [data larger than memory](#data-larger-than-memory) and [the soak](#the-soak) --, [the gate](#the-gate) every pull request's instruction counts are held to, and [the budgets](#binary-sizes) the binaries a release ships are held to. The first two sections are paragraphs moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists. [Instruction counts](#instruction-counts) holds no invariant of the code: it is how two builds' counts are read on macOS, which `tools/instr/` runs. [Binary sizes](#binary-sizes) is how the binaries a release ships are measured and held to their budgets, which CI's build job does.

## scale-bench

**`make scale-bench` holds fennec-server to pgvector, each over its own
protocol.** fennec-server is asked over HTTP by a keep-alive client written
in the bench crate (`fennec-bench/src/http.rs`), PostgreSQL by the
`postgres` crate with pgvector-rust's types, with the same vectors and m and
ef_construction, and each side takes its best way: fennec-server keeps its
graph as JSON arrays of rows land (`POST /<collection>`), pgvector builds
after a binary COPY in memory on every core (`parallel_workers`: 768-dim
vectors live in TOAST, and PostgreSQL planned one process for them), reads
its index into its buffers and searches a filter with `iterative_scan`; the
container needs 2 GB of shared memory for that build (`pgvector-up`).
At a million 128-dim vectors fennec-server loads and indexes in 49.6 s against
108.5, on 612 MB of disk against 1 432, its engine counting 738 MB against
PostgreSQL's 1 131 (its container's anonymous memory and the shared memory
of its buffers, from its cgroup; macOS puts fennec-server's physical
footprint at 992), answers a beam of 100 with 99.1% recall in 0.141 ms
against 98.6% in 2.08, a filter keeping 1% in 0.67 ms against 16.1, and
eight clients at 18 852 queries/s against 2 289; at 250 000 x 768, 39.9 s
against 79.9 and 99.8% in 0.431 ms against 98.1% in 4.22 (`site/content/docs/benchmarks-vector.html#scale`). The laptop this was
measured on is a fanless M1 Air, which slows to a third under minutes of
load on every core: a comparison runs its sides in turns (`--only fennec`,
then `--only pg`), each after idle minutes, and a figure from a hot run is
not one.

## YCSB

**`make ycsb` is fennecdb as a general database.** YCSB's core workloads
A-F written in the bench crate (`fennec-bench/src/bin/ycsb.rs`) rather than
run through the Java YCSB, to its definitions: ten fields of 100 random
characters, a read the whole record, an update one field, its scrambled
zipfian (0.99, FNV-hashed over ten billion items), its skewed-latest for
D, scans of 1 to 100 records for E. The key is an integer from 1
(`insertorder=ordered`, no `user` prefix), so each system keys by what it
keys best by: `id`, `INTEGER PRIMARY KEY`, a `bigint` primary key,
`_id`; E's scan is `where id >= $1 limit n`, the id index walked from
the key, no `@sorted`. A key not yet acknowledged is never drawn: an
insert publishes a bound below its key before it takes one (`Keys`).
fennecdb in process against SQLite (a connection a thread, WAL, mapped),
fennec-server over HTTP against PostgreSQL 17 and MongoDB 8, each started
in a container for its turn and removed with its volume, so the VM holds
one at a time; `server-docker` is the same server in a container, which
shows Docker's share -- its network, and an fsync in the VM that is not
macOS's `F_FULLFSYNC`. Durable is an fsync a write in each engine's own
terms (a flush and its durability, `synchronous=FULL` with `fullfsync`,
`synchronous_commit=on`, `j: true`), buffered its default
(`--sync 250`, `NORMAL`, `off`, `j: false`); C writes nothing and runs
once. A server switching modes is stopped with SIGTERM, which syncs: a
kill lost the last 250 ms of the load, and the next update found no row.
Redis 7 (`redis`, redis-rs 1.7.1, a connection a thread) keeps a record
as YCSB's own binding does -- a hash a key, `HGETALL` and `HSET`, every
key in the `_indices` sorted set a scan reads by `ZRANGEBYSCORE` -- but
scored by the key, not a hash of its text, so a scan reads keys in order
as elsewhere, an insert's `HSET` and `ZADD` one `MULTI`/`EXEC` and a
scan's `HGETALL`s one pipeline. Durable is `appendfsync always`,
buffered `everysec`, and `snapshot` -- its image as it comes, RDB
snapshots alone, minutes of writes a crash loses -- a mode of its own
the others pass over (`System::modes`); the default snapshots stay on in
all three, as Redis's persistence page advises. Its container starts
afresh for each load, which ends with a `BGSAVE` and a `BGREWRITEAOF`
run out, so no fork the load made due falls in a cell. After each phase
a container's memory -- `anon` and `shmem` of its cgroup, read through a
container sharing its processes, fennec-server's image having no `cat` --
is a `memory-<phase>` line beside the size. A size read through a
container or a connection of its own -- fennec-server's in Docker, Redis's,
PostgreSQL's -- is read once a cell's clients stop, not every 100 ms
beside them (`System::size_beside`): two `docker run`s a second in the VM
took fennec-server's C at one client from 3.14-3.17 k to 2.33-2.38 k.
benchmarks-ycsb.html's Redis part
sits between `ycsb redis` markers that `site/build.py` takes out until
the results hold a Redis cell, its figures written in from the medians
(`ycsb_redis`); `ycsb/run-redis.sh` is its run, a `--verify` pass first.
Each cell is 30 s after a 5 s warm-up, and starts once a one-core probe
runs within 4% of its idle time (`cool`), the probe's ratio written with
the cell; the systems take turns with three idle minutes between them.
Latencies go into a log-linear histogram, 64 steps an octave.
`--verify` holds every answer to what was written (`check`): each
thread logs its operations and answers into its own `Vec`, and after the
cell a read must hold, field by field, a value a write that could have
been the last one left (one ended before it and not followed by another
that did, or one overlapping it), a scan consecutive keys as far as it
could see, a write one record -- every write logged, since a read under
concurrent writes is judged by every write's span, and nothing compared
in an operation's time. The rule is `src/register.rs`'s, which the soak
holds its hours to as well (below). A pass over all six systems found no mismatch
(`ycsb/verify.tsv`). `results/ycsb.tsv` is the three runs the site
quotes, appended to and never rewritten -- a run's cell measured again
counts as its last line, and a run a later one replaced is named in
`results/superseded.tsv` and counts in no median (`ycsb report`, `site/build.py`)
-- and `site/build.py` holds every YCSB figure on the site to its median
there, the full grid row by row.

Every family's results are one schema, `results/<family>.tsv`: run, commit,
machine, date, family, cell, metric, value, a line a value (`ycsb` writes
`<system>:<mode>:<workload>:<threads>` as the cell and each column as a
metric). `site/build.py` takes the median of each `(family, cell, metric)`
and holds every figure with a unit in README.md, the home page,
benchmarks.html and the `vs-*` pages to one (a `CLAIMS` entry, or a
`FIGURES` one for the families other than YCSB); the detail pages carry a
dated "measured at" note. The YCSB runs migrated from the old wide file took
their commit from the first commit that held the line, which is where the
results were committed, not necessarily the build measured. A figure no
recorded run backs is a `hand` run on the machine `hand-measured`, its
commit and date those of when the figure was first published, not of a
build that measured it; the release
candidates' nights replace those rows with measured runs.
`tools/bench_record.py` appends a run's values in the schema. Docker's VM syncs a
write in 0.08-0.10 ms where the Mac's `F_FULLFSYNC` takes 3.9
(`ycsb/fsync.txt`), so the durable server comparison is `server-docker`
against PostgreSQL and MongoDB, all in the VM, the native server beside
them. fennecdb's buffered reads lost to SQLite's after the updates for the
engine's reason, not the harness's: an update writes the record again at
the file's end, the file outgrew the page cache, and a profile put 86% of
a one-thread read on its page coming in (`ycsb/profile-c1.txt`). Runs
`c1`-`c3` measured fennecdb and fennec-server again with the file compacting
itself (below; the harness starts the engine's `Compactor` as an app's
library does, `--no-auto-compact` for neither): C at one thread 30.1k ->
438k against SQLite's 279k, B 30.8k -> 410k, D 62.6k -> 432k, and fennecdb
ahead of SQLite in every in-process cell; the buffered file stood at 3.7-4.0
GB after D, between compacts of a file A grows by 100 MB a second. Official
YCSB 0.17.0 against the same containers came within -14% to +2% of the
harness (`ycsb/calibration/`). Docker's network moves from day to day --
PostgreSQL's C at one client was 3.34 k, 3.00 k and 3.53 k on three -- so
the one-client server cells were measured again on 2026-10-04 with
PostgreSQL beside them in turns, under the same run ids (`c1`-`c3`,
`r1`-`r3`), each cell counting as its run's last line. `make
roundtrip-bench` took the round trip apart: of a 0.29 ms read in Docker
about 0.25 is Docker's port forwarding, the server's part 0.023 ms and
PostgreSQL's bind and execute 0.016, so the published guess that a binary
protocol answers sooner was not the cause; the durable gap was the fsync
of a growing file (the sync log, above): durable A 2.24 k -> 3.27 k at one
client against PostgreSQL's 2.60 k, even at 16. E's scans were
fennec-server's own -- 213-220 us inside for 50 rows, 37 page faults --
and a plain `get` written as JSON from the stored rows (below) took E at
one client 1.03 k -> 1.62 k buffered against 1.34 k, 1.06 k -> 1.82 k
durable against 1.39 k. B, C, D and E were measured again on 2026-10-05,
servers and PostgreSQL in turns under the same ids, run 1 again on a quiet
machine after its idle probe was taken at a load of five; C stays 3.19 k
against 3.66 k, with each server's part of a read under 0.02 of 0.29 ms
and PostgreSQL's own B at 2.65 k beside its C: the rest moves with the VM.
Then every server cell, 1, 4 and 16 clients, was measured in one night,
2026-10-13 (`rx1`-`rx3`: fennec-server natively and in Docker, PostgreSQL,
MongoDB and Redis in turns), superseding the servers' `c`, PostgreSQL's
and MongoDB's `r` runs and the `rd` attempt sized beside the cells: the
server in Docker leads the other three in every cell but buffered C and
F at one client, 3.07 k against PostgreSQL's 3.14 k and 2.07 k against
2.09 k, and Redis in every one.

A cell can run open loop (`--rate <ops/s>`): each thread keeps a schedule
of send times, an operation every `threads / rate` seconds, the threads'
schedules staggered, and an operation's time runs from when it was due,
not from when it went out, so a stall counts the wait of every operation
queued behind it -- coordinated omission, which a closed loop hides by
slowing its clients down with the server. `seed.rs` measures its writes
so. A test holds it: one 200 ms stall in a second at 1 000 a second puts
the p99 at the stall open loop and leaves it at nothing closed
(`open_loop_counts_the_wait_behind_a_stall`). The closed loop stays the
default and the published grid's.

`ycsb` builds without the rivals' crates (`--no-default-features --bin
ycsb`, the `rivals` feature off): fennecdb in process and fennec-server
alone, from the workspace's own crates, which is what lets
`ycsb/linux-tools.sh` build it for Linux in `rust:1.99.1-alpine` offline,
from the host's registry index, for the two regimes below that run it in a
container.

## Data larger than memory

`ycsb/run-larger-than-memory.sh` runs YCSB's B, C and E over live data at
least twice the memory the system may use -- 4 700 000 records, 4.8 GB
live, under `--memory 2g` by default -- durable and buffered, three runs
a cell, every answer held to what was written (`--verify`). Two systems,
in turns: `server-docker`, fennec-server in a container started with
`--memory` and `--memory-swap` the same (a page the limit pushes out is
read again from the file, never swapped; the server's own memory past it
is an OOM kill, not a slow cell), and fennecdb in process, `ycsb` itself
run in a container under the same limit. The file is mapped and there is
no buffer pool: the kernel keeps what fits of it, the rest is read again
from the disk at a major fault, and the derived indexes are on the heap
beside it. What was measured before was a file past the page cache only
through its dead bytes (`compact-bench`'s `docker run --memory 2g`, and
YCSB's files growing between compacts); here the live records alone are
twice the memory. The default was 4 000 000 at first, sized at 1 100 bytes
a record; the dry run measured 1 026 to 1 059, which made 4 000 000 1.92 to
1.96 times 2 GiB and every cell a failure of the criterion three hours in.
4 700 000 x 1 026 bytes is 2.24 times it at the smallest record measured.
The script refuses a size under twice the limit at that record before
anything starts, and after each load `ycsb` reads the live bytes and the
cgroup's own limit and refuses, naming both, when the live bytes are under
twice it: no cell runs over data that would not count.

Each cell's line carries, beside YCSB's figures, its cgroup looked at each
second through the measured window -- by a shell in the server's process
namespace for `server-docker` (`Watch`: one process the run long, where a
`docker run` a look took a quarter of a one-client server's reads), by
`ycsb` reading its own for the process in a container (`Cgroup::own`):
`majflt_s`, the major faults a second over the window, and `majflt_s_max`
in its worst second; `cg_peak`, the most the cgroup was charged, page
cache and all; `anon_peak`, the most of it anonymous -- the heap, the
indexes and, in process, the verify logs; `file_mean`, its files' pages on
average, the mapped database's among them; `limit` and `live`, the live
records' bytes (`fennec_data_bytes`, or the engine's stats in process) --
`limit` being what the cgroup's own `memory.max` says, never the
`--memory` asked for. `failed` and `oom` are on every line under
`--memory`: a cell whose
clients panicked -- a refused request, the server gone -- is a line with
`failed 1`, and `oom 1` where Docker says the kernel killed it for its
memory; a system that failed is a failed line for each cell it would have
run; and a process the kernel killed whole, which can write nothing, has
its cells written failed by `ycsb fill` from the script. No cell asked for
is ever a missing line. A failed cell writes no measured value at all --
no `ops_s 0` a median could take -- and `site/build.py`'s reading of the
results drops every metric of a run's cell marked failed, so a figure
naming the cell never averages it in (`site/test_results.py`).
`mismatches` and `undecided` are the cell's verdict. `ycsb report` takes
them as it takes the rest: a failed run, and one whose live bytes were
under twice its limit (`under_2x`), counts in no median and is counted
apart.

The machine is shared, so the environment is looked at all along, not only
at the start (`ycsb/env-check.sh`, both scripts): before each system's turn
and, through `ycsb --gate`, before each load and each cell, the load must
be down to `QUIET_LOAD` (waited for, `ENV_LOAD_WAIT` s at most) and both
disks must have room -- this host's and Docker's VM's, a container's `df`
on the data volume, since the VM's disk fills on its own -- and while a
step runs both are looked at every `ENV_EVERY` s and the step stopped under
a floor, before a write fails for want of room, which would read as the
server's error. A step the environment cannot take ends the run, its cells
written `env_failed 1`: the environment's failure, counted in the report's
`env` column, never a pass and never `failed`.

In process, the verify logs are the measured process's own memory, under
the limit: `--verify-max` (half the limit, from the script) ends a cell's
measured window once the logs reach it, so they never push the engine's
pages out past what the limit already does, nor the process past it. The
line's `seconds` says how long the window ran; its rate is over those
seconds. The server's logs are `ycsb`'s on the host, outside the
container, and its cells run their full length.

The lines go to `results/ycsb_larger.tsv`, family `ycsb_larger`, a file of
its own rather than a column of `ycsb.tsv`: its cells are named as the
published grid's (`server-docker:durable:B:16`), and both `ycsb report`
and `site/build.py` take the median of every run of a `(family, cell,
metric)` -- a memory-bound run there would join the medians of the runs
that had the page cache to themselves.

At the same size, `ycsb heap` measures the heap a row of each index kind
holds, from the server's own `memory_bytes` (`fennec_memory_bytes`): a
collection of as many rows -- a 16-letter token for `@hash`, an integer for
`@sorted`, eight words of a 10 000-word vocabulary drawn zipfian for
`@text`, a `vector<32>` for `@hnsw(cosine)` -- each index made by `create
index`, measured once no vector waits to be linked
(`fennec_vectors_unlinked`), and dropped with its field before the
next, the server started again over its file so that no freed memory of
one kind's build is reused by the next: `server-docker:heap:<kind>`, with
`bytes`, `bytes_row`, `build_s`, `rows` and `live`, failed where the
server was killed building it. It runs with no memory limit: `@hnsw` over
millions of vectors is heap that has to fit, and under the cells' 2 GB the
dry run's was killed, which says nothing of how much it takes. Beside the
engine's own accounting (`bytes_row`, what each index's tables hold:
`@sorted` came out at exactly 20.0 bytes a row) the line has what the
kernel charged, the container's anonymous memory changed by the build
(`cg_anon_bytes`, `cg_anon_bytes_row`): the allocator's own and every
buffer the build kept.

Not measured: a machine with a disk slower than this Mac's SSD behind
Docker's VM (a major fault here is the VM's block device on APFS), a
Linux host's page cache without a VM between, workloads A, D and F, and
data larger than memory under the vector index's own arena, which is heap
and has to fit (`An open copies a graph's vectors into the arena`): the
heap figures say how much it takes.

## The soak

`ycsb/run-soak.sh <sync>` runs `ycsb soak` (`src/soak.rs`, a module of
`ycsb` rather than a program of its own): fennec-server in a container
whose `--memory` (512 MB by default) is under its file's size (1 000 000
records, 1.1 GB live), a second container following it as a replica, for
8 hours a run. First the peak the mix reaches as fast as it is answered,
four minutes; then the soak open loop at 50% and at 80% of it, a run each.

**One checker.** `soak.rs` is the driver -- the mix, the schedule, the
kills, the minute's line and the report -- and decides no answer: whether
a read could have been answered so is `src/register.rs`'s alone, the rule
`--verify`'s `check` holds YCSB's cells to (above), moved into a module
both use. A register is a field of a record, the record's being there, a
session's, or a counter; a read over `[s, e]` may answer a write beside it,
one that could have been the last before it, or, where no certain write
ended before it, what the register held. The soak's mix needed three
things YCSB's did not, each in the module with its tests: a delete (a
write of the record's absence), a row a `@ttl` expired (a write of its
absence over the span its time can fall in), and a write that may not have
landed -- one not answered, and one a kill may have taken (below). Every
such write is a value a read may answer, never one that makes an earlier
value impossible. Counters are the module's too: an increment is not
idempotent, so a read must answer at least the certain increments that
ended before it and at most every one begun by its end. The table of
histories in `register.rs`'s tests is run both ways the module is used: a
cell at once, and settled a minute at a time behind the reads as the soak
settles it.

**What a kill may take.** Under `--sync always` a write is answered after
its fsync: a `kill -9` takes nothing answered. Under `--sync 250` a write
is answered from the process's buffer, which reaches the kernel -- after
which a kill of the process takes nothing, the kernel surviving it -- at the
first of a write behind after a request once the buffer holds 64 KB (passed
over while a durability holds the disk), an append past the buffer's
bounds, or the syncer's next flush, which writes the buffer before its
fsync. The syncer sleeps 250 ms, then takes the lock, writes and fsyncs: a
write answered just after one flush's write waits out that sync's own time,
the interval, and the next one's lock and write. So it is in the kernel
within 250 ms and two syncs' own time: the window the power-cut rig holds a
cut to (`.github/power-cut/client.py`, 200 ms a sync, 650 ms), which also
covers the second sync's fsync a kill does not need. The soak holds a kill
to that: a write answered within 650 ms of the kill (or after it was sent,
until the server answered again) may be gone for a read ending after the
kill; one answered earlier must be there. A write not answered may or may
not have landed under either mode.

**The mix**, over 32 connections: 36% read and 37% update of YCSB's records
(A, its scrambled zipfian) and 4% read of them through the replica, sent
after the latest write answered (`Fennec-After`: the replica answers once
it holds it, so its answer is held to the registers as the primary's is --
never across a restart, where it would wait out the primary's absence); 2%
increment of one of 1 000 counters (`set counters {n: n + 1}`) and 1% read
of one; 3% insert of new keys and 3% delete of the oldest inserted; 2% read
of an inserted key; 4% insert into `sessions`, whose `seen timestamp
@ttl(60s)` expires them, and 2% read of them -- by id, through a filter of
16 ids, and counted (`count`), each held to the registers: a row is there
until its time and gone from it in every read ("every read leaves them out
at once"), within 5 s either way. The 5 s is the clocks', not the server's:
this host's, which times each operation, against Docker's VM's, which
`now()` and the expiry read and which Docker Desktop keeps to the host's --
a drift of seconds is what it allows for, nothing on the server's side;
2% `match` on a `@text` body, 2% `near` on a `vector<16> @hnsw(cosine)`
(ten rows), 2% update of a doc. Two rows for one id, or a row not asked
for, is a mismatch.

Beside it 64 subscriptions to `GET /usertable/changes`, a range of 64 keys
each, held to the writes in their range by the change each write was
answered as (`Fennec-Seq`): within a connection the changes rise strictly
from the seed's; each write in the range is in the first event at or past
its change -- a write that may not have landed aside -- and no key is in an
event without a write to it in the changes the event covers; a stream open
past a write in its range for 10 s without reporting it is silent. Each
event's rows are held to the registers over the span from the write that
took its change to when it was read. A reconnect -- the server killed --
may replay one thing, the seed: the whole range again, every record of it
there and as written. Each minute a sample of three `match` and three
`near` queries is held to a scan of the same rows: every doc read, `match`
as a filter (`order id`: every row holding a word of the query) against
the docs read holding one, its ranked page among them; `near ... exact`,
the server's own scan, against the ranking over the vectors read, to the
last digits of the f32 sums; and the `@hnsw` walk against `exact` by
recall@10, the run's mean held to 0.925 -- the lowest the site states
(`benchmarks-vector.html`, ef 40 over 1 000 000 x 128), a far harder set
than 20 000 x 16. A doc written while a sample ran is left out of it.

**The schedule.** `compact usertable` every 20 minutes; every 120 from
minute 50 a backup over the replication stream (`fennec backup` into an
archive), restored (`fennec restore`) and the restored file verified, each
step counted; every 30 the server killed (`docker kill --signal KILL`),
its file held to its checksums (`fennec verify`) before it starts again.
They queue one behind another, so a backup due with a compact runs after
it (the first version let the compact take the minute, and with a compact
every 20 minutes no backup ever ran); a kill waits for the queue but every
90 minutes, when it lands a second into a compact of its own: a crash in
the middle of a compact is what a soak should meet, and the file must
verify after it (a mapped file is replaced only by rename). After each
restart the keys written in the 10 s before the kill, or by a write not
answered, are read back; at the end every key is, every counter and every
session.

**A minute's line**, `results/soak.tsv`, family `soak`, cell
`<sync>:<fraction>:m<minute>`, appended as the minute ends so that a run
cut short keeps what it measured, each line stamped with the harness's
start (`started`, so a run id used again is told apart: the report judges
the latest attempt) and the primary's `life` (its restarts before it):
`ops`, `ops_s` against `rate`, `idle_workers` (connections that ended no
operation in the minute); the steady operations' `p50`, `p99`, `p999` and
`max` in microseconds, each timed from when it was due, and `p99_hour`, the
steady p99 of the 60 minutes to then; apart from them `restart_ops` and
`p99_restart`, the operations due from a second before a kill until the
server answered again, and `compact_ops` and `p99_compact`, those due while
a compact ran; `errors`, `refused` (writes refused by their status) and
`down`; `mismatches`; the streams' `stream_missing`, `stream_twice`,
`stream_order`, `stream_late`, `stream_silent`, `events` and `streams`
open; `file`, the database file's length alone, read through `/proc/1/root`
(never the image a compact writes beside it), and `live`,
`fennec_data_bytes`; `compactions` so far, the server's own and the run's;
`rss_anon`, `memory_bytes`, `cg_anon`, `cg_file`, `cg_current`, `majflt`,
`threads`, `fds` and `connections` (`fennec_connections`) of the primary,
`r_rss_anon`, `r_threads`, `r_fds` and `r_connections` of the replica;
`lag` (`/_replication/status`'s `behind`), `lag_s` (those writes at the
minute's write rate) and `r_connected`; the sample's `match_n`,
`match_differs`, `near_n`, `exact_differs`, `recall_sum` and
`sample_skipped`; `host_free` and `vm_free`, the disk on each side; the
run's doings in the minute (`kill`, `verify`, `verify_failed`, `backup`,
`restore`, `backup_failed`, `compact`, `compact_failed`, `compact_killed`,
`restart_ms`, `swept`), `died`, `oom` and `r_died` (a server gone that the
run did not kill, the kernel's OOM kills of it); and `load1`, `load5`, the
host's load: the machine is shared, and a minute it was busy shows.
`:start` says the run's settings, `:end` closes a finished run: totals,
`swept` against `keys`, the replica's `catchup_s`, `p99_restart` and
`restart_ms_max` over the run, both files verified after both stopped as a
supervisor stops them, and `replica_differs`: the same ordered read of
`usertable`, `counters` and `docs` on both once the replica is at the
primary's change, its pages' bytes digested (`sessions` aside: time, not
the position, decides its rows).

**The report**, `ycsb soak report soak.tsv [<run>]`, judges a run by the
item's exit criterion and what holds it, a line each with the minutes that
broke it: 8 hours and an end line; the planned minutes run; every minute
with operations from every connection (the dead-man: a minute nothing was
answered in, or a worker stuck); the last steady hour's p99 at most 1.25
times the first's -- the restarts' windows and the compacts' apart, so that
hours holding two kills and three compacts compare with one holding one
and two (with no dry run yet, the compacts were put apart without data:
`p99_compact` says whether it was needed); the file at most 2.2 times the
live bytes every minute; the primary's `rss_anon` grown less than 5% from
the fifth minute of each life to its last, every life (it is killed every
30 minutes, so a run-long baseline would see processes at most 30 minutes
old), and the replica's, never killed, from minute 60 to every minute
after; threads and descriptors less the connections open, within 2 of
their median, for both (each connection is a thread and a socket; a compact
or a backup running at the look, the replica's reconnect and the scrape's
own connection still move them); no error -- a status other than a 2xx is
one wherever it falls, and so is a connection error or a 503 saying when to
come back outside a restart's window, which within one is `down`; no
mismatch; every stream held and all 64 open each minute; the replica
connected each minute and within 5 s of the primary, its first follow
within 1 800 s, caught up at the end within 300 s, and holding the
primary's rows; every restart within 120 s and the restarts' p99 within
122 s (the announced second, the restart and a request after it) -- a
bound set without a measured restart, which the first run's `restart_ms`
must show room under; the kills and verifies the schedule owes for the
run's length (`(minutes - 1) / 30`), its compacts, kills in a compact,
backups and restores; every verify clean; match and near as a scan; every
key read back at the end; no server died and none was OOM-killed. A disk
short of `--disk-floor` (3 GB) on either side ends the run as the
environment's: the report says so and judges nothing of it (exit 2), never
a pass and never the server's failure. It lists the minutes the host's
load was 4 or more, and exits 1 unless every line passed. Each criterion
has a synthetic run of lines that passes it and one that breaks it alone
(`the_report_passes_a_good_run_and_fails_each_criterion_alone`).

Not measured: the servers outside Docker's VM (the soak's kill, cgroup and
`/proc` looks are Linux containers'), a primary under a failover (P5's),
and a load the machine shares: the minutes it did are flagged, not taken
out. A sweep of every key after each restart, rather than of those the kill
could have touched: it would read the whole file through the cgroup's
512 MB sixteen times under load and push the working set out each time,
into the tail the p99 is judged on.

## Instruction counts

**A difference under ~500 instructions a request between two builds is
read over several identities of each, or in a trace's own code.** Two
builds are held to each other by the instructions their loop retires an
op (`proc_pid_rusage`'s `ri_instructions` read around it: `replays.rs`,
`hook_events.rs`), not by the clock, which a shared machine moves by more
than most changes cost. On macOS 26 that count carries the allocator's
draw. Its allocator, xzone malloc, sends a binary's allocations and frees
down paths its identity picks -- some through `mfm_alloc` and `mfm_free`,
some through `_xzm_xzone_malloc_tiny` and `_xzm_free` -- a mix fixed for a
binary and changed by a rebuild, or by its Mach-O UUID alone. #513 found it
([named-statements.md](named-statements.md), *What slice 3 left on a
request that is no event*): `edb629d2`'s `replays` with only its UUID
changed and signed again ran one traced `put` through `/query` in 539
instructions fewer, every one of them the allocator's; `bc7636b2` built
without slice 3's three touches on a request's path (`v1`, its own code
`edb629d2`'s to the instruction) 1 062 fewer, again all the allocator's.
In plain pairs `v1` read -110 and -150 a `put` against `edb629d2` while
`bc7636b2`, 26 instructions of its own code more, read +67 and +58, and a
keyed write +485 and +311. Main's `replays` at `61fe4261` and one copy of
it under another UUID trace one `put`'s `handle` in 18 833 and 17 698
instructions: 8 630 of its own code both, `libsystem_malloc`'s 8 843
against 7 741. `MallocNanoZone=0`, which held such a count still when
[text.md](text.md#match-as-a-filter)'s ordered `get` was measured,
holds it no longer, nor do `MallocSecureAllocator=0` or
`MallocXzonePtrBucketCount=1`: the nano zone is not what serves these.

Two ways read past the draw, both in `tools/instr/`. *The means over
identities* (`make instr-compare`, `compare.py`): each build is copied
under 8 random UUIDs (`setuuid.py` sets `LC_UUID` in a copy, never in the
build's own output, and signs it again with `codesign -s - -f`), each copy
another draw of the allocator's paths; every copy runs 7 times, all of them
once a round in an order turning each round, each run started under a
1-minute load below 4, and a build's figure is the mean of its copies'
medians beside its standard error. A difference inside two standard errors
is none. The report gives what one copy a side reads too: the range a
plain pair could have landed anywhere in. In #513 `bc7636b2` read +45 a
`put` and -14 a keyed write against `edb629d2` this way, where plain pairs
read +126 and +121, and `v1` +2 and -35; two sets of eight read main
(`d1be9713`) +40 and +61 a `put`. `make hook-bench`'s paired group reads
this way by default (`UUIDS=8`). *A trace* (`TRACE=fennec_http::handle`,
`trace.py`): lldb stops the program at a function's Nth call (the 30th,
past a first request's one-time work), single-steps it into every call to
its return and names each instruction's module and function, a clock read
run whole (the commpage's retry loop never settles under single steps).
The same binary traced again counts the same to the instruction, and its
own code counts the same under any UUID, so two builds' traces say what a
change costs in its own code by function, the allocator's apart (`trace.py
diff`). A trace is one call: work done once every so many requests -- a
table growing, a flush -- falls in some calls and not others, and the
means count it.

Which to use. Plain pairs (`UUIDS=0`, one binary a build, the order
alternating) are enough where a difference is well past the draw, a few
thousand instructions a request or a few percent of an op of
microseconds, and for time on a large effect. Under ~500 instructions a
request a difference is confirmed by the means or a trace before it is
called a cost, a trace where what moved must be named. A control, `BASE`
the commit this tree is at, shows the spread the program at hand gives.
Main at `61fe4261` built twice, in a scratch worktree and in a tree whose
crates are main's, is one code under two UUIDs: one `put`'s traced
`handle` runs 8 630 instructions of its own code in each, the allocator's
8 393 in one and 8 843 in the other. `replays.rs`'s `put`, every run
started under a 1-minute load below 4 (2.5 to 3.9): in plain pairs, 30
rounds, the second build read +98 instructions a request (+0.45%), the
rounds' differences a median of +135 and 26 of 30 above -- a cost the code
does not have; against the first, the second re-signed under a third UUID
read +55, 18 of 30 above. Over 8 UUIDs a build, 7 runs each, two sets read
-22 (standard error 42) and -63 (38), each inside two standard errors,
where one copy a side read anywhere from -285 to +148. Eight copies a
build resolve a `put` to about ±80 instructions (two standard errors);
more narrow it as their square root, and a trace settles a program's own
code to the instruction. The tools are macOS's -- the counts, the signing
and lldb -- and elsewhere say so and exit 0; `tools/instr/test_instr.py`,
run in CI, holds a copy's UUID, the refusals keeping a build's output
untouched, the figures and a trace's deltas.

## The gate

**A pull request is held to its base in instructions and heap bytes an
op.** CI runs the gate as two jobs, on every pull request and in the merge
queue. `perf (count)` builds `crates/fennec-http/examples/gate.rs` twice --
at the merge's first parent, which is a pull request's base branch or the
commit a merge group queues onto, and in the change -- and counts each of
its workloads under valgrind's callgrind, five runs a side, each run
tallying what its loop asked of the allocator. `perf (instructions)`
judges the counts, and is the check a merge may require. A side's figure
is the median of its runs' instructions, each run's calls into the
allocator counted at what the base's runs paid a call (*The allocator,
priced*, below), and its bytes the median of what its runs asked the
allocator for (*The bytes*). A workload whose figure or bytes are 1% or
more above the base's fails the job (`tools/perf_gate.py`), unless the
pull request adds an entry to `PERF_ACCEPTED`, in the same file: the
workload, what it lets through (`"metric": "bytes"`, the figure where it
names none), the most (`"+1.8%"`) and why, in 40 characters at the least
-- what the cost buys, and what `make perf-gate` and `make instr-compare`
measured. An entry lets its workload through only in the pull request that
adds it -- one the base holds already lets nothing through, and an entry
edited counts as added -- so the list is the history of what was let
through, never cut. A workload the base's own program lists fails when
this tree's program dropped it, and when the base's build did not run it
-- a crash, or a loop the change rewrote into one the base cannot run:
neither is held to nothing. Only a workload new to the program that the
base cannot run is held to nothing, and the summary says so. Making `perf
(instructions)` a required check is a setting of the branch's protection,
beside `ci.yml`'s other jobs
([conventions.md](conventions.md#reruns)), made once the
holdout below has passed.

*Judged by the rule it would replace.* Both jobs run the base's copy of
the checker. The count job writes it under `tools/`, so it reads the
checkout as its tree, and builds, counts and reads the profiles with it;
the judge runs it again on the results, reading the pull request's
`PERF_ACCEPTED` as data, never running it, as `tools/openapi_compat.py`
reads `BREAKS` ([http.md](http.md#compatibility)), and the workloads the
base had from the base's own `gate.rs` (`git show`, `--base-program`). A
pull request that lowers the bar, cuts the runs, adds a function to
`ALLOCATOR` -- which would price that function at the base's cost a call,
whatever grew inside it -- or rewrites the rule is counted and judged by
the rule it would replace; its edit counts once it has landed. Only the
pull request that brought the checker, whose base had none, was counted
and judged by its own. The judge is a job of its own (`needs: perf-count`,
`contents: read`): the count runs the change's build scripts and its
programs, which can put anything on a later step's `PATH`
(`$GITHUB_PATH`) -- a `git` or a `python3` of their own -- where the judge
runs nothing of the change, its `git` and `python3` the runner's. A count
that failed or never ran fails the judge. A newer push to a pull request
cancels its older run (the workflow's `concurrency`); a merge group's run
is never cancelled.

*The owner approves what the gate lets through.* The judge names, as a
warning on the run and in its summary (`edits`), every edit of the gate's
own files -- `gate.rs`, `tools/perf_gate.py`, the workflow's `perf-count`
and `perf` jobs, comments and blank lines aside -- and every
`PERF_ACCEPTED` entry the pull request adds, edits or removes. main asks
for no review, and pull requests come from the owner's account, so a
reviewer approves nothing: such a pull request is the owner's to approve,
and the agent merging it waits for the owner's OK rather than merging on a
green check. The warnings are no lock -- the workflow that prints them is
the pull request's own, and an edit to it can leave them out -- so the
merging agent reads the diff for these files and entries too. What no
check holds: the count runs the change's code, and a build script or a
workload that forges the count forges the gate.

*What is counted.* One program, this tree's source copied into the base's
worktree, so both sides run the same work against their own crates; a
program the base's crates do not build -- an interface the change altered
-- falls back to the base's own copy, its workloads held to these by name.
It is built for `x86_64-unknown-linux-musl` in the release profile, the
code the release's binaries and the image are, its symbols kept
(`CARGO_PROFILE_RELEASE_STRIP=false`): stripping moves no instruction, and
a profile of a stripped binary names no function. Each side builds with its
own toolchain, the base's channel installed beside the change's, so a pull
request moving `rust-toolchain.toml` builds its base as main did; and asks
for `fennec-abi/sync` on the command line, for `sync-apply`, which a base
whose fennec-http manifest predates its dev-dependency would not build
otherwise. Each workload sets its data up from a fixed seed, then runs its
ops in a loop, and only the loop is counted: valgrind starts with
`--instr-atstart=no --collect-atstart=no`, and the program asks callgrind
to instrument and count as the loop starts and to stop as it ends -- client
requests, the magic sequences of valgrind's `valgrind.h` written as inline
assembly for x86_64 and aarch64, which a processor runs as no work. Setup
so runs under valgrind's core alone, about five times slower than native,
where instrumented it ran some eighty times slower. A run that counts
nothing -- valgrind passed a sequence by -- is refused, and so is one that
tallied no heap. Every workload is in process and on one thread, with no
socket: the figures over the wire are `make requests-bench`'s, `make
roundtrip-bench`'s, `make load-bench`'s, `make scale-bench`'s, `make
ycsb`'s and `make beir`'s, whose clients, servers in Docker and systems
beside them an instruction count cannot hold. `make perf-gate` runs the
job's count and judgement on Linux with valgrind; `HEAD=<ref>` replays two
commits, as the calibration below did. Each side's median run's profile is
kept (`target/perf-gate/profiles/<workload>.<side>.callgrind`), for
`callgrind_annotate` to say which function a difference is in; the count
job uploads them (`perf-profiles`). A change is counted unless every path
it touches is under `site/`, `docs/`, `integrations/`, `web/`, `studio/` or
`deploy/`, or is Markdown (`reaches`, a rename read as both its paths):
such a change is the base's program to the byte, and the job says so. What
may move the build is listed by what may not, so a place a build reads is
counted from the day it changes: the list it replaced named what was
counted -- `crates/`, the lock, the toolchain's file -- and missed the root
manifest, whose `[profile.release]` the gate builds, and `.cargo/`; a pull
request setting `lto = "thin"` there was counted as nothing.

*Why on musl.* The release's Linux binaries and the image are static musl
builds (`release.yml`, `Dockerfile`), and musl's allocator is much of a
request -- [http.md](http.md#json-from-stored-rows): it gave a freed group
of blocks back to the system and faulted it in again at the next request --
so glibc's, which ships nowhere, would count another program's costs.
Instructions rather than time: a shared runner's clock moves by more than
most changes cost, and one binary's count, run after run, is the same to
the instruction where nothing it does depends on a draw or on the clock.

*Where a count moves.* Held to itself -- one build on both sides, ten runs
a side, eight at a time on a loaded M1 in Docker (aarch64 musl) -- 17 of 24
workloads counted the same to the instruction, or within 0.03%, run after
run; `group`, `near` and the filtered `near` within 0.5%; `facet` 15-18%
and the three scoped requests 2-6% apart. Two things moved those, each
found by diffing two runs' profiles by function. The clock:
`--request-rate`'s window is swept once a second of the wall's time
(`rate::Held::take`), forgetting the callers that asked nothing in it, and
a caller forgotten is taken in anew at its next request. A thousand
subjects in turn -- `rows.rs`'s -- took two seconds a turn under callgrind,
and a hundred on a loaded machine more than one at times: how many each
sweep forgot was the clock's, and a scoped request's count moved by 2 to 6%
from one run to the next. Four subjects each ask again within milliseconds,
and the sweep forgets none. The draw: std's hash tables take their keys at
random in each process (`RandomState`), and a facet through a `@hash` index
walks its buckets in the order the keys lay its table out. Picking the ten
most, `order_rows`' `select_nth_unstable_by` and its sort compare as many
times as that order makes them, 87 to 92 a facet, every other function of
it counting the same to the instruction (ten runs' profiles diffed by
function, on x86_64): one database's facet cost 9.7% apart over ten
processes. Each op of `core-facet` asks the next of 256 databases, each
with keys of its own (a thread's tables take its keys with the first moved
by one, a new key each), and the mean of 256 draws still spread 1.2% over
ten runs, 0.70% between two sides' medians in the control: too near the
threshold. So `core-facet` picks the commonest brand, a pass of 39
comparisons whatever the order, its walk, counts and values counted as they
were: ten runs within 0.003% (one database alone, 0.019%). The pick of ten
is `core-lookup`'s -- the first ten of some 500 rows in id order, an order
the seed fixes -- and the sort `core-group`'s, its 100 groups. In the local
control ten runs a side spread 0.03% for the scoped read, 0.8% and 1.0% for
the scoped insert and set -- the set within 0.04% over eight runs made one
at a time -- and 0.4% for the filtered `near`: a step a workload's lookups
pay or not, which the median of five takes, the threshold standing well
above it.

*The allocator, priced.* musl's allocator hands out a slot of a group of
its size class and frees the group once its last slot is freed, so an op
whose allocations fill and empty a group of their own makes and frees that
group each time -- `alloc_slot`, `nontrivial_free` and
`__malloc_alloc_meta` running a few hundred instructions more, the calls
the same. Which ops do is the heap's layout, every size and lifetime of
what the program holds, and a change anywhere moves it. The calibration's
plain counts failed two merged pull requests on it: at #474, `get t where
id = $1` made the same 16 allocations and frees an op, its own code
counting the same to the instruction, and paid 401 instructions more
(+3.6%), every one of them the allocator's; #524's `PATCH` by a filter, its
own code 18 instructions more, paid the allocator 1 146 more (+5.7%). The
same pull requests' other paths moved the other way (#474's ordered read
-2.0%), and #474's own first head, against its fix, -4.1% on a put: a
regression of a few percent beside such a swing would pass the plain count.
So each run's instructions in the allocator's own functions -- `malloc`,
`free`, `realloc` and the rest, musl's mallocng behind them, its lock, and
the system calls it makes and frees its groups with, `mmap`, `munmap`,
`madvise`, `brk` and the wrappers they share (`ALLOCATOR`), each
function's self cost -- are counted as the run's calls at what the base's
five runs paid a call, the calls the program's own tally (*The bytes*,
below): an allocation more or fewer moves the figure by the base's price
for one, a group's making and freeing by nothing. The program's own system
calls pass through the same wrappers, a few instructions each, and are
priced with them: the gate counts no system call's work in any case. The
plain count's delta stands beside the judged one (*as counted*), the
difference the allocator's. Self costs, because callgrind charges an
instruction to the function it is in, where a call line's inclusive cost
rests on its tracking of calls and returns, which loses returns from
musl's allocator: on aarch64 it charged `facets`' `malloc` calls with 29
000 instructions each, the allocator's inclusive cost above the loop's
whole count, and the calls it saw moved with the heap -- 16 078 `malloc`s
in one run, 13 222 in the next, where the program counted the same. The
first version priced each entry point's calls by that inclusive cost, and
the heap-shift control (below) moved its figures by -17.9% to +14.4% with
the code the same. What the price hides is what grows inside a call with the
calls the same -- an allocation past its size class, or past the 128 KB
musl maps on its own, `realloc`'s copies as a buffer grows, `calloc`'s
zeroing -- and the bytes see that (below); what fragmentation alone costs
a call, the sizes the same, neither sees. `make instr-compare` is no
backstop for it: it runs by hand, counts macOS's allocator rather than
musl's, and reads no difference under some 500 instructions a request
([Instruction counts](#instruction-counts)).

*The bytes.* The program's global allocator is `System`'s -- musl's in the
build counted, not one in its place -- with every call counted and the
bytes each asks for (`Counting` in `gate.rs`): an allocation's size, a
zeroed one's (`alloc_zeroed`, which is `calloc`), and a `realloc`'s
growth, what its copy grows by. Read around the loop, they go out as a
`heap:` line, and the table gives calls and bytes an op beside the
figure. Bytes asked for are the code's own: the same from one process to
the next whatever its hash tables drew, and whatever the heap's layout --
every workload's to the byte, natively, over three processes, one of them
after a leak of 77 777 bytes, and under callgrind in the heap-shift
control below. So they are held to the same 1%, through the same
`PERF_ACCEPTED`, an entry naming `"metric": "bytes"`. A counter is an
atomic, the same few instructions a call on both sides.

*The threshold.* Set by a calibration in this change's CI: the gate
replayed on six merged pull requests `make instr-compare` had judged
neutral, which must pass; on three known regressions, each as it stood
before it was fixed or undone, which must fail; and on the base held to
itself -- two builds of one commit at two paths, as the job builds a change
that moves no code, the binaries the same to the byte -- twenty times over.
Each pair is its two commits built with this change's program and judged
as the job judges, first by the plain count (run 43418039339) and then
with the allocator priced and `core-facet` picking one (run 43449980626)
-- priced then by the call lines' inclusive costs, which the self costs
replaced after the heap-shift control below; the holdout judges these
pairs again by self costs.
A replay, two builds and 240 runs, took 99 to 167 s on a hosted runner.

| pair (base → head) | expected | plain count | allocator priced | what decided it |
|---|---|---|---|---|
| the base against itself, 20 rounds | pass | pass | pass | plain: `core-facet` +0.70%, +0.37%, -0.27%, -0.45% (its pick of ten), every other workload within 0.08%; priced: `core-group` +0.04%, every other within 0.01%, `core-facet` 0.00% |
| #531 (`088bc30f` → `de59fdd1`) | pass | pass | pass | nothing past 0.01% |
| #506, #513's `v1`/`bc7636b2` (`edb629d2` → `bc7636b2`) | pass | pass | pass | `http-put` +0.23% |
| #513 (`fb93539e` → `61fe4261`) | pass | pass | pass | nothing past +0.01%; `http-put` -0.77% |
| #542 as merged (`4a157cdd` → `1dee77aa`) | pass | pass | pass | `http-patch` +0.20% |
| #474 as merged (`d45afa21` → `e3d58955`) | pass | **fail** | pass | plain: `core-put` +4.30%, `core-get-id` +3.59%, `core-filter-limit` +3.44%, `http-get-row` +2.96%, `http-patch` +2.92%, `http-put-keyed` +1.02%, all musl's groups; priced: `http-put` +0.15% (25 an op), `http-patch` +0.12%, `core-put` +0.04% |
| #524 (`a96bd554` → `c736b5c0`) | pass | **fail** | pass | plain: `http-patch` +5.71%, `http-scoped-set` +2.35%, `http-get-row` +1.94%, all musl's groups; priced: `ql-parse` +0.34% (its parser's own growth), `http-patch` +0.09%, `http-scoped-set` -0.97% (two allocations fewer) |
| #511 undone (`82319b7f` → `9da6411e`) | fail | fail | fail | `core-json-page` +11.06% either way |
| #542's first head (`de23ce27` → `0e05fe22`) | fail | fail | fail | `http-patch` +1.70%, `http-get-row` +1.12% either way |
| #474's first head, against its fix (`e1d6a47c` → `41f293a9`) | fail | fail | fail | `http-scoped-insert` +2.12% plain, +3.25% priced; `http-scoped-set` +1.43%, +2.41%; `http-scoped-get` +1.45%, +1.11% |

The bar stays +1%, on the figure with the allocator priced. The widest
delta of a merged change was +0.34% -- #524's parser, its own code, which
no request-level figure reads apart -- and the narrowest that decided a
regression +1.11% (`http-scoped-get`) and +1.12% (`http-get-row`); the
base against itself moved 0.04% at most. Each regression fails on the
workloads that stand for it, and no merged change fails. The plain count
failed two of the six merged changes on musl's groups alone, and
understated #474's first head by 515 and 438 instructions an op on its
scoped set and insert, a flip the other way: priced, they read +1 258 and
+1 263 an op, beside the +1 128 measured when it was found. Neither failure
was a cost `make instr-compare` had missed: priced, both changes moved
every `http-*` workload 0.15% at most. The gate reads finer than a
request on macOS all the same -- an op of 9 000 to 35 000 instructions
where `make instr-compare` resolves a request to about ±80 -- and a change
that costs a workload 1% on purpose says so in `PERF_ACCEPTED`, with what
the instructions buy and how they were measured: the rule is no loss, and
a cost is justified where it is taken.

The control held one build to itself -- two builds of one commit, the
binaries the same to the byte -- so it could see no shift of the heap's
layout, nor of inlining: the merged pairs moved both, and the allocator's
price is what took the layout's shift out of them. *The heap-shift control*
moves the layout with the code the same: one build against itself, its head
runs leaking 4 KiB, 64 KiB or 1 MiB before the setup, in blocks of six size
classes (`FENNEC_GATE_LEAK`, which `gate.rs` alone reads and the gate never
sets), so every later block lands elsewhere in musl's groups (`perf_gate.py
measure --head-leak`). Locally, on an M1 in Docker -- aarch64 musl,
valgrind 3.22, every workload five runs a side -- the calls and bytes an op
held to the byte under each leak, but for the scoped requests', which a
slow run's extra `--request-rate` sweeps on the wall clock move by some 50
bytes a loop (0.002%), in base and leak runs alike, the median taking the
commoner. The plain count moved by -5.2% to +6.8%. The first version's
figure, priced by call lines, moved by -17.9% to +14.4% under the 64 KiB
leak and failed three workloads. Priced by self costs it moved by -0.21% to
+0.65%, the rest musl making and freeing groups through `mmap` and `munmap`
in the leak's runs, 76 instructions an op of `core-get-id`'s; with those
system calls priced, as now, it moved by -0.04% to +0.11% under 4 KiB,
-0.10% to +0.08% under 64 KiB and -0.18% to +0.12% under 1 MiB, 24, 24 and
22 of the 25 workloads within 0.1%. What is left is the program's own code,
each function repeating to the instruction within a layout and moving with
it -- `Filter::made` 42 instructions an op fewer on `http-get-row` under 1
MiB, `Store::read` 16 more on `core-put` -- 0.18% at most, under a fifth of
the threshold. The base at origin/main built with this program and this
tree, the binaries the same to the byte, held within 0.02% of each other.

*The holdout* (`.github/workflows/perf-holdout.yml`, run by hand at a
quiet hour, by dispatch or the `perf-holdout` label): the fifteen most
recent first-parent pairs of main touching `crates/` before the gate, none
of them the calibration's -- #592, #593, #568, #559, #556, #554, #540,
#541, #537, #538, #464, #529, #526, #522 and #520 -- each replayed as the
job judges a pull request, every one to pass or to fail on a cost its
change is shown to have; the calibration's nine pairs again, judged by
self costs where the calibration priced by call lines, its six merged
changes to pass and its three regressions to fail; and the heap-shift
control on x86_64 musl. Each pair says what it must do (`expect`): `pass`,
no workload failing, or the workloads that fail and by what --
instructions, bytes or both, two joined by `+` -- and is as expected only
when exactly those fail, a workload failing for no metric's reason
(dropped, unrun) never; a metric marked `?` may fail or not
(`http-scoped-get:bytes+instructions?`), for a judged figure its own runs
put within their spread of the bar. A pair whose base predates what a
workload's setup asks for names it (`cannot_run`) with the words of the
base's error, 16 characters at the least -- a word or two is in every
error -- and is judged on the workloads both sides run, one so named that
the base runs, or does not for another reason, or that this tree runs
fewer than five times, failing it (`perf_gate.py check --expect
--cannot-run`). Its first run (43682293646, main at 8743f0d9), every
merged pair expected to pass, held the control and 19 of the 24 pairs as
expected; five were not, each cost found in the two sides' median
profiles, function by function, and in the lines its change wrote. #593: a
read by an id or a `@hash` bucket goes through `candidates_held`, and its
rows are tested by `Filter::without`. Read off the profiles: +326 to +426
instructions an op outside the allocator's functions, and an allocation of
32 bytes more and its free; on `core-get-id`, `candidates_held` 268
instructions of its own where `filter_candidates`, whose place it takes,
had 241, and a call to `bcmp` (90 with `memcmp`); `Filter::made` 90 of its
own, `Filter::taking`, which calls it, 53 fewer; `matching_ids_to` 90 more
of its own, a call to `Expr::conjuncts` (226 with the vector it grows:
`grow_one`, `finish_grow`, the `malloc`) and one to `bcmp` (20).
`Filter::without` is no frame, inlined into `matching_ids_to`, so what
follows is the source's, inferred: it collects the `and`'s terms into a
vector even where the equality is the only one -- the call to
`Expr::conjuncts` and its 32 bytes -- and compares each term's field with
the bucket's, the second `bcmp`; the first is `candidates_held`'s
comparison of the index's name, which tells it a bucket gave its rows.
`core-get-id` +6.51% (31 → 33 calls, 807 → 839 bytes an op, +3.97%),
`core-filter-limit` +2.15% (+1.01%), `http-get-row` +4.09% (+2.55%),
`http-patch` +3.56% (+1.99%), `http-scoped-get` +3.02% (+1.45%), a cost to
win back in a change of its own. #554: every scoped request charged to its
caller's `--request-rate` as its token is checked. Read off the profiles:
+652 to +696 instructions an op outside the allocator's functions, no call
into it more; on `http-scoped-get`, `authenticate` 110 more of its own and
two calls it did not make, `rate::Table::lock` (94 of its own, 170 with
the clock read it makes) and `rate::Held::take` (64 of its own, 298 with
the `HashMap::get_mut` it makes, 93 of its own, and that one's `bcmp`).
`rate::admitted`, which makes those calls in the source, is no frame: it
is inlined into `authenticate`, so that `authenticate`'s 110 are
`admitted`'s is inferred, as is, from the source, that the lookup is keyed
by the caller's subject. `http-scoped-get` +2.85%, `http-scoped-insert`
+1.63%, `http-scoped-set` +1.22%. #568: every record carries a CRC32C --
an open checks each it reads, `file-open` +20.23%, 257 instructions a
record of 100 000 (`Rec::summed` 80, the sum 77, the walk over each
record's framing 100); a compaction sums each it writes, `file-compact`
+21.76% (141 a record); a write sums its record, `Database::commit` 81,
the sum 35, a copy 37 to 41 (a frame with no symbol, which `realloc` calls
too) and `record_at` 17, `core-insert` +1.82%, `core-put` +1.20%,
`http-put` +1.13%, the longer records growing the buffers they are written
into, `core-insert` +1.54%, `core-put` +1.24% and `http-patch` +1.32% in
bytes, all of them `realloc`'s growth -- the write's cost the owner
accepted as it merged. The sum ran as SSE4.2's `crc32` (`crc::x86::raw`,
no table in any profile): valgrind's CPUID offered SSE4.2, as every x86_64
processor made since 2011 does, and an aarch64 with `crc` takes its own
instruction a word, so the figures are a user's; only the browser module
and a processor without the instruction take the slicing-by-8 table, eight
lookups and their XORs for the eight bytes one instruction sums. Recheck
#524: `http-batch` -3.18% in instructions, 4 calls a put fewer, and +2.85%
in bytes: a batch's lines are parsed into `Query`s, each its text and what
its first read found, then moved by `exactly_batch` into a vector of
exactly their number, where the base pushed each statement into its one
vector -- `realloc`'s growth 840 480 bytes a batch less (824 an element,
over the 1 020 it grows by), 911 736 allocated outright more, 71 a put.
Recheck #474: its scoped writes' policy grants `wake`, which #474 brought,
and its base refuses the policy before the loop: the two workloads "the
base's build did not run" are named as its cannot-run now, and the other
23 passed. #474's first head against its fix -- a base that reads `wake`,
and ran every workload -- failed on its regression alone:
`http-scoped-insert` +3.25% (+3.26% in bytes), `http-scoped-set` +2.42%
(+2.79%), `http-scoped-get` +1.11% (+1.09%). The pairs with a cost and the
three regressions now name the workloads and metrics above -- #511 undone
`core-json-page`'s instructions, #542's first head `http-get-row`'s and
`http-patch`'s -- and every pair, judged again by the new step from the
first run's results, came out as expected.

The second run (43686447758, this workflow on its branch at 19a8322b) held
every pair and the control as expected, and the two runs' figures agree.
Of the 25 workloads, 13 ran their five runs the same to the instruction as
judged -- the allocator's instructions priced at the base's cost a call --
on every side of every pair in both runs, `http-scoped-insert` and
`http-scoped-set` on all sides but one; the others spread, a side's runs
against their median, by 0.12% at the most (`core-group`), then 0.03%
(`core-insert`, `http-scoped-insert`), 0.02% (`core-put`,
`http-put-keyed`) and 0.01% or less. As counted, the runs' spread the
table shows, they reach 1.11% (`http-scoped-set`, on #520 in the first run
and four other pairs in the second), 0.78% (`core-near-filtered`), 0.42%
(`core-near`), 0.29% (`http-scoped-get`) and 0.21% (`http-scoped-insert`):
the allocator's cost a call moving with the heap's layout, which the
judged figure prices at the base's. The bytes an op were the same in every
run of every side. Every judged figure within 0.2 points of the bar,
failing or passing, came out the same in both runs, its runs the same to
the instruction or the byte on both sides -- #593's `core-filter-limit`
+1.012% in bytes, #568's `http-put` +1.131% and `http-patch` +0.891% in
instructions, #542's first head's `http-get-row` +1.119% and
`http-scoped-get` +0.898%, #474's first head's `http-scoped-get` +1.115%
and +1.087% in bytes -- but #568's `core-put`, +1.196% and +1.197%, its
runs 0.014% apart at the most. None is within its spread of the bar, and
no pair names a metric that may fail. The 0.29% `http-scoped-get` spreads
by as counted on #474's first head is the allocator's cost a call, 178.7
or 180.0 instructions by the heap's layout, which the judged figure prices
at the base's, 178.1 or 178.5 a run, pooled: with 53 calls an op on both
sides, only 231 a call would bring it to +1%. `perf (instructions)` stays
a check no merge requires until the corrected workflow, on main, has a
green run.

*Why fennec-http's examples.* The program reaches every crate a request
does and no more. In `fennec-bench`, its bundled SQLite, the MongoDB
driver, tokio, PostgreSQL's and Redis's clients would be built twice a
pull request, which the job's fifteen minutes cannot hold. fennec-http's
examples build its dev-dependency, fennec-tls -- rustls and ring, whose C
the runner's `musl-gcc` compiles (`musl-tools`, as `release.yml` installs
it) -- though no workload links it. valgrind is the runner's package too:
the crates take no dependency.

*The workloads and the claims they stand for.* Each is the path a page's
figure measures; a hot path a page quotes gets one.

| workload | its loop | the claim it stands for |
|---|---|---|
| `core-put` | a row written over by id, its `@hash` bucket moved, into a file | YCSB's updates ([benchmarks-ycsb.html](../../site/content/docs/benchmarks-ycsb.html#ycsb)) |
| `core-insert` | a new row into a file | YCSB's load, and D's and E's inserts |
| `core-get-id` | a row by id of 10 000 | YCSB C's reads by id, 438 k a second at one thread |
| `core-filter-limit` | a `@hash` bucket of some 100 rows, the first ten | a filter with a limit of 10, 0.032 ms over HTTP ([benchmarks-requests.html](../../site/content/docs/benchmarks-requests.html#requests)) |
| `core-order` | a range of a `@sorted` field, the first ten in order | YCSB E's short scans; [`@sorted`](indexes.md#sorted) |
| `core-group` | 10 000 rows grouped by a field of 100 values, counted and summed | an aggregate of 50 000 groups ([benchmarks-requests.html](../../site/content/docs/benchmarks-requests.html#concurrency)) |
| `core-lookup` | ten parents in id order, each with its children through a `@hash` | [`lookup`](lookup-and-aggregates.md#lookup), one bucket probe a parent (`make lookup-bench`) |
| `core-match` | BM25 over two words, the ten best, of 10 000 documents of 30 words | `match … limit 10`, two words: 0.06 ms ([benchmarks-text.html](../../site/content/docs/benchmarks-text.html#text-search)) |
| `core-fuzzy` | a word with a typo, its words within an edit | a word's neighbours, one edit: 14 µs ([benchmarks-text.html](../../site/content/docs/benchmarks-text.html#typos)) |
| `core-facet` | the brands of every row through `@hash`, the commonest picked, over 256 databases in turn | a facet over every row through `@hash`: 0.01 ms ([benchmarks-text.html](../../site/content/docs/benchmarks-text.html#text-search)) |
| `core-near` | the ten nearest of 4 000 64-dim vectors by the graph, a beam of 100 | `near` at a beam of 100 ([benchmarks-vector.html](../../site/content/docs/benchmarks-vector.html#scale), `make sweep`) |
| `core-near-filtered` | the same under a `@hash` filter keeping 1% | a filter keeping 1%: 0.67 ms at a million ([benchmarks-vector.html](../../site/content/docs/benchmarks-vector.html#scale)) |
| `core-json-page` | 1 000 rows of four floats written as JSON off the stored documents | [a plain `get` written from the stored rows](http.md#json-from-stored-rows) (`json_numbers.rs`) |
| `ql-parse` | 34 texts of every kind, SQL's spellings among them, each parsed | every statement's first request, before [the parse cache](http.md#parse-cache) holds it |
| `http-get-row` | `GET /t?id=eq.<id>` through `fennec_http::handle` | a row by id: 0.022 ms over HTTP ([benchmarks-requests.html](../../site/content/docs/benchmarks-requests.html#requests)) |
| `http-patch` | `PATCH /t?id=eq.<id>`, a row written by a filter | REST's writes; [row tags](http.md#row-tags) cost a write by a filter nothing |
| `http-put` | `put` by id through `/query` (`replays.rs`'s `put`) | put a row: 0.022 ms over HTTP ([benchmarks-requests.html](../../site/content/docs/benchmarks-requests.html#requests)) |
| `http-put-keyed` | the same, each with an `Idempotency-Key` of its own | [idempotency keys](http.md#idempotency-keys) (`make replay-bench`) |
| `http-batch` | a `/batch` of 1 000 puts, one block | a `POST /batch` of single puts ([benchmarks-requests.html](../../site/content/docs/benchmarks-requests.html#loading)) |
| `http-scoped-get` | `http-get-row` under a scoped token, `--request-rate` charged | what a scoped caller pays ([access.md](access.md); `rows.rs`'s `scoped`) |
| `http-scoped-set` | a customer's `set` of its own row beside a bound collection | a scoped write waking nothing does main's work ([named-statements.md](named-statements.md); `make wake-bench`) |
| `http-scoped-insert` | a customer's `insert` of a row of its own, the same | the same |
| `file-open` | a file of 100 000 rows opened, no index built | an open ([benchmarks-vector.html](../../site/content/docs/benchmarks-vector.html#against-sqlite-and-pgvector)); [lazy indexes](indexes.md#lazy-indexes) |
| `file-compact` | a file of 10 000 rows each written twice, compacted | [compaction](storage.md#compaction) (`make compact-bench`) |
| `sync-apply` | a replica's sync applying a change of 100 rows over the 1 000 it holds, into a file | rows in changes of 100 cost the replica what a put does ([mobile.html](../../site/content/docs/mobile.html#sync), `make sync-bench`) |

*What it does not catch.* Instructions, not cycles: a cache miss, a
branch mispredicted, memory's bandwidth and a system call's time cost
nothing here, and a change that trades instructions for any of them
passes. No system call is counted: a change that doubles a durable write's
fsyncs, or a compaction's bytes written, passes it -- those counts, fsyncs
and bytes written a durable write, a `/batch`, a spill and a compact,
under `strace -f -c`, are to be another `metric` of the same results file,
over the same two builds and the same checker, the job's step after this
one's. Every workload runs on one thread, so what threads cost -- a lock's
contention, connections side by side, a batch's vectors linked in parallel
-- is not counted. One target: x86_64 musl's code, not aarch64's NEON
kernels, macOS's or Windows' builds, nor the browser module's (wasm32,
whose size `make wasm-sizes` and the size job hold). No workload writes
vectors in its loop -- the graph each `near` reads is linked in its setup,
uncounted -- and none applies a primary's records as a server's replica
does ([replication.md](replication.md#replication)); `sync-apply` is a
sync replica's apply alone. A change on those paths is measured by hand:
`make instr-compare`, `make sweep`, `make replica-bench`.

*Rejected.* A glibc build: it ships nowhere, and its allocator is not
musl's. `--toggle-collect` on the loop's function, valgrind's other way to
count a part: the setup runs instrumented. Every map's hasher fixed under a
`cfg`: the counts would be exact, but the change touches some hundred maps
of shipped code, and needs a measurement of its own that the `cfg` off
costs nothing. The seeds fixed from outside -- `getrandom` refused under a
seccomp filter, so std reads `/dev/urandom`, and a file of fixed bytes
bound over it: no code changes, but a thread's tables take their keys in
the order they are made, so a map added anywhere before a workload's moves
every later one's keys, and a change's count would be one draw from the
same spread, once a build, where five runs and their median read past it.
More databases for `core-facet`'s pick of ten: 4 096 of them still spread
0.29% over five runs, sixteen times the setup. An absolute floor beside the
1% -- a delta failing only past so many instructions an op too -- for the
allocator's swings: they ran 360 to 1 170 instructions an op, past
#542's regression of 219 a read. The allocator left out of the count: an
allocation more would cost only its call. A global allocator of the
program's own, its cost a call fixed, in place of musl's: the build counted
would no longer be the one that ships -- the counting one hands every call
to musl's. The count by the pull request's own checker, as the first
version had it: an entry in `ALLOCATOR` priced a function away in the pull
request that added it. The count and the judgement in one job: the
change's code had run before the judgement, on the same `PATH`.

## Binary sizes

**What a release ships is measured as it is built, and held to a budget.**
CI's build job (`build (release + cli profile)`, a required check) builds
what `release.yml` puts in the x86_64 Linux tarball -- `fennec` on the
`cli` profile and `fennec-server` on `release`, for
`x86_64-unknown-linux-musl`, static, each in a cargo run of its own, ring's
C built by musl-tools' compiler -- and `make small`, the shell without its
default features, the same way, and holds each file's bytes to
`tools/size-budgets.json` (`tools/size_budgets.py`). An entry carries what
the job measured when it was last written (`measured`) and a budget half a
percent above it, rounded up to a thousand bytes. A binary past its budget
fails the job: the pull request that grew it writes the two figures the
failure prints and says in its body what the bytes buy. One more than 1%
under its budget, or more than 1% from its measurement, fails too, so that
the figures the docs quote stay what ships. The docs quote `measured`, a KB
1024 bytes, and `site/build.py` holds every figure they write within 1% of
it (`binary_facts`): the size table, README's range, the image wherever a
page names it. `tools/test_size_budgets.py`, in the fmt job, holds the
check's refusals and the file's own entries.

The image's figure is `fennec-server`'s, in MB of 10^6 bytes: the root
`Dockerfile` builds the same binary with the same toolchain, on Alpine,
and copies it alone into `scratch`. 0.2.0's amd64 image held 8 178 208
bytes where its tarball's `fennec-server` was 8 174 128, 0.05% apart, and
the monitoring (Grafana) job, the one job that builds the image, holds its
binary within 1% of the figure (`--near`). What `docker images` reports is
not it: 23.2 MB for that image under Docker Desktop.

Why these figures. The docs quoted macOS arm64's, to be re-measured by
hand at each release, and they drifted with nothing to stop them: 3357 KB
for `fennec` and 5922 for `fennec-server` (measured 2026-11-02) when 0.2.0
shipped 3616 and 6730 on macOS, and 7.18 MB for an image whose binary was
8.18. A check in CI, which is Linux, cannot hold a Mac's figure, and the
Mac's was never the Linux release's: 0.2.0's `fennec-server` is 8 174 128
bytes for x86_64 musl against 6 891 744 for aarch64-apple-darwin. The musl
build is what a Linux tarball, the `.deb`, the `.rpm` and the image ship,
and a runner builds it to the same bytes each time: the compiler is
pinned, and the paths it writes into panics' locations are the runner's.
The job runs on `ubuntu-24.04`, the runner `release.yml` builds the
tarball on, not `ubuntu-latest`, which became 26.04 in 2026-11: another
release's musl, gcc (ring's C) and linker would measure another binary
than the one that ships, and move every figure the day GitHub moves the
label. Built in an amd64 Ubuntu 24.04 container laid out as a runner is
(`/home/runner/work/fennecdb/fennecdb`, `CARGO_HOME` under
`/home/runner`), main at `38f6a0bc` gave 4 856 592, 8 186 416 and
2 951 680 bytes, and the job's first run on `ubuntu-24.04` (#648) the
same three to the byte; 0.2.0's tarball to them moved `fennec` 4 096
bytes and `fennec-server` 12 288, an ELF file's segments being laid out by
4 KB pages, so a figure moves a page at a time or not at all.

The check's first run on the tree that landed it (#672, 2026-11-15) failed
all three, against figures a day old: `fennec` 4 893 456 bytes (+36 864,
+0.76%), `fennec-server` 8 292 944 (+106 528, +1.30%) and `make small`
2 968 064 (+16 384, +0.56%). In that day 25 pull requests had landed on
main, three of them trains of ten, eight and six branches, none held to a
budget main did not have yet: each had been measured by its own author on
macOS, where a Mach-O file grows a 16 KB page at a time and most of them
read +0. The file was written again from that run. A percent in a day,
with no pull request in which anybody saw it, is what the check is for:
from that landing the job holds each one to what main measures.

Rejected: a budget of the measurement itself, which would stop every pull
request that adds a byte and make any two in the merge queue that each
write one conflict; a ceiling 5% above it, the 1.0 plan's for after 1.0,
which would leave the docs' figures free to drift 5% before anything
failed; measuring the image in `release.yml` (`docker image inspect`),
once a release and after the docs have shipped its figure.

What the job costs. It built `make small` and `fennec-server` for the
host (glibc), one after the other: a median of 2.3 minutes over 40 green
runs (2026-11-13 and 14, the slowest 2.7), the two builds about 40 and
80 s of it, the crates from outside the workspace restored from the cache.
Each build is a chain of compiles of one unit (`codegen-units = 1`) ending
in one fat LTO -- 68 s of `fennec-server`'s 85 -- a core's work most of
its time, and the runner has four: the three builds run at once, `make
small` in a target directory of its own, since a second cargo run on the
`cli` profile's waits for the first's lock, and the cache holds the musl
builds' crates from outside the workspace under a key of its own. Its
first run, with nothing in that cache, took 2.2 minutes: the builds 98 s
at once -- `fennec-server` 1 min 38 s, `fennec` 1 min 19 s, `make small`
1 min 8 s, ring's C and rustls compiled for each profile, the three
adding to 4 min 5 s -- and the musl target and musl-tools about 13 s of
the install's 23.

Not built here: `fennec-shard`, which no release ships yet -- its row left
the size table, and it joins this job and the file when `release.yml`
builds it -- nor the release profile's `fennec` with and without its
default features, which ship nowhere and whose rows the table had. The
browser module's figures are `site/build.py`'s own, against the module the
test job builds.
