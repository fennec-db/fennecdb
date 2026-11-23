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
that did, or one overlapping it), and never go back on a read that ended
before it began -- the register is atomic, not merely regular, so the reads
are held in the order they ended -- a scan consecutive keys as far as it
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

What a run needs is looked at before its first command, and nothing is
pulled, fetched or built while it runs (`env_preflight`, both scripts): the
rust image the Dockerfile names, by its digest -- a tag can move -- which
the Linux tools build in; `alpine`, by its digest too (`ENV_ALPINE` in the
scripts, `ALPINE` in `ycsb`, a test holding them to one), which the looks,
verifies, backups and `df` run in, and which the check runs, first, rather
than inspects -- Docker Desktop 29.2.1 answered `docker image inspect` "No
such image" for every image while its VM had idled some minutes, `image
ls` listing them all, and a run answered truly and woke it, the inspects
after it answering truly too; rustup's toolchain and the targets rust-toolchain.toml names,
which a cargo in the tree would fetch; cargo's registry index, which the
tools' build reads offline; and `fennecdb-ycsb` built from this tree. Each
is said `ok` or `MISSING` with how to get it, and a run with one missing
starts nothing. Every `docker run` of the scripts and of `ycsb` takes
`--pull never`; the tools build with `--network none`, the host's `ycsb`
with `--offline`.

The image is built by `--preflight` alone, in a window where the network
may be used, as a plain `docker build` builds it, and labelled with the
tree's identity (`com.fennecdb.tree`, `env_files`): a hash of every file git
tracks or sees untracked and unignored, path and bytes as on disk, but the
harness's own crate (its results and logs are what a run writes, and the
server is built from none of it), `.claude/` (out of the build context) and
the root's untracked files (a run's log). Every other file counts, the docs
too -- the server embeds `studio/` and `web/` files, and a list of what it
reads would miss the next -- so a change anywhere else costs a pre-flight,
never a stale server; a commit and a dirty flag would not do, since a run's
own lines dirty the tree. A run builds nothing: it computes the same hash,
refuses an image labelled with another ("built from another tree ...: run
--preflight"), none, or one built without the label (run-redis.sh's, `make
ycsb`'s), and starts every server from the matching image by its id
(`--image sha256:...`), so a build from another checkout moving the tag
while it runs changes nothing it starts. Rejected: building the image at
each run with `docker build --network=none`, which held Docker's build cache
to the online build's layers. BuildKit keys a `RUN` step's cache by its
network mode, so an offline build never reuses an online build's apk layer
and goes to the network for it: the check failed on the machine whose
online build had just succeeded (Docker Desktop 29.2.1, 2026-11-22).

Every container, volume and network a run makes carries its process id
(`p51-<pid>-server`), so that a run never removes another's; the trap stops
the step's own children before the step -- `caffeinate -i` runs the
measurement as its child and passes no signal on, so stopping it alone left
`ycsb` running, writing lines and removing containers by a shared name --
and whatever a bounded docker call was in when the run was stopped: each
call (`env_within`) writes the pids of its command and the watch beside
it into the shell's own directory until it has waited for both, and the
cleanup stops them by those pids wherever the call was made -- the
guard's run in its own shell, held still (`SIGSTOP`) while its children
are stopped, its `df` read into a file rather than through a pipeline
whose subshell would go on to its next call; a watch looks once a second,
so a stopped one leaves no long `sleep`. `--self-test` holds it to that
with toys and no Docker (`env_self_test`), in CI's clippy job: interrupted
under `caffeinate`, and in a docker call that never ends -- its own
shell's before a step, its guard's during one -- every process under the
script then is gone within 30 s; and the pre-flight's build labels the
image with the tree's hash, a run starts its servers by the image's id and
builds nothing, and an image labelled for another tree is refused, nothing
built or started.

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
thirty minutes, its operations over its time but the first minute's; then
the soak open loop at 50% and at 80% of it, a run each -- the second only
where the first's report passed, unless `KEEP_GOING=1`: eight hours on a
known failure tell nothing more. The peak is long enough for the file to
double with the updates and the server to compact it on its own, which
the script says it did (`peak_compacts`): a rate a soak keeps for eight
hours is held through both. Four minutes of a freshly loaded file, their
median minute, measured its best -- 3 214 a second on the second dry run
of 2026-11-23, whose 80% held twelve minutes, until the file passed 1.8 GB
under 512 MB and both servers compacted on their own (the primary at
1 196-1 258 a second meanwhile), and was never held again.

**The servers ask a token.** Both, and the backup's read-only server, run
with `--http-token` and `--replication-token`, two secrets of the run's
own (32 bytes of `/dev/urandom` each, held in memory), handed to each
container in its environment -- `-e FENNEC_HTTP_TOKEN` with no value, the
docker CLI's own environment carrying it -- never as an argument, which
`ps` shows on the host and in the VM, nor in the log; every request the
harness sends carries the first (`http::bearer`), `fennec backup` reads
the second from its container's environment. A server that asks for no
token answers only a `Host` of this machine's (fennec-http's
`open_refusal`): the replica's follow and `fennec backup` reach the primary
by its container's name on the run's network, and the two-hour dry run of
2026-11-22, its servers open under `--insecure`, had both refused 403 from
the first second -- the replica at change 0 for two hours, every read sent
through it waiting out its wait and answering 504, the backup failed --
while the harness only waited. The servers' `--replication-buffer` is
passed too (64 MiB, the server's default), for the report's memory line.

**The harness's own faults stop the run.** What every minute after would
meet again is not carried for hours: the replica not following within the
first follow's bound (1 800 s), its status naming a refusal by its primary
(`the primary answered 401` or `403`) during that follow or at any
minute's look, an operation or a subscription answered 401 or 403, a
backup refused so (`refused`, `Shared::fault`), and a server not running
during the first follow (both containers looked at every 5 s, an OOM kill
named), before any load has begun. The run stops at once
(`stop_run`): its STOPPED line says why, its volumes are kept and named,
its containers removed, a `:stopped` line written (the report's "ran its
planned minutes" says it), and `ycsb soak` exits 4, on which the script
stops after the peak or the fraction it ran -- `KEEP_GOING` or not --
with exit 4 itself. The script's cleanup goes on past a failing call
(`|| true` on each): under `set -e` the first to fail ended the trap, and
`docker network rm` of the network the soak had removed itself fails, so
the dry run's cleanup named the first soak's kept volumes, then stopped,
leaving the second's unsaid and the tools' volume behind; the self-test's
toy docker fails that call as Docker does, and holds the tools' volume
removed and a stopped soak ending the run with nothing after it.

**An image under the run's memory.** A replica's first follow, and a
backup, are sent an image, which the primary built whole in its heap
under its read lock (fennec-http's replication `stream`:
`Database::snapshot`) and the replica received whole: under a `--memory`
below the file's size -- the soak's premise -- the primary was OOM-killed
the moment its replica first asked. The first short run after the token,
250 000 records (259 MB of file) under `--memory 256m`, lost its primary
0.7 s after the replica started (exit 137, OOM-killed), and the script
stopped 30 s into the peak. Since the image goes out as a pin takes it and
is taken as it arrives (#680), the second dry run's replica followed in
4 s, 1 GB of file under 512 MB, and its backup was taken, restored and
verified.

**What a run keeps of its servers.** Both servers' logs, every life of
the primary's, their lines stamped by Docker, are written into `--logs`
(`LOGS`, beside the run's other files in `$TMPDIR`) as their containers
go, a stopped run's too (`Kit::keep_logs`); and a read the replica
answers 504 -- held past its wait (`Fennec-After`, 5 s) -- is said on the
log with what each server's `/_replication/status` said as it answered
(`Shared::replication_said`). The second dry run of 2026-11-23 counted 40
such reads, every error it had: 7 in the minute after the restart of
minute 60, 32 -- every connection -- as the replica came back after the
kill of minute 90, and 1 at minute 86 as the run's compact ended. In each
every 504 named one change of the replica's (5 562 489, 7 115 618,
6 837 278): it held still for the whole wait, while the reads named
changes the primary had answered up to 2 927 past it. Nothing of the run
said whether the primary's durable change stood still -- under `--sync
250` a replica is sent only what is on disk -- or the replica took
nothing, and the containers' logs went with them.

**One checker.** `soak.rs` is the driver -- the mix, the schedule, the
kills, the minute's line and the report -- and decides no answer: whether
a read could have been answered so is `src/register.rs`'s alone, the rule
`--verify`'s `check` holds YCSB's cells to (above), moved into a module
both use. A register is a field of a record, the record's being there, a
session's, or a counter; a read over `[s, e]` may answer a write beside it,
one that could have been the last before it, or, where no certain write
ended before it, what the register held. And the register is atomic, not
merely regular: reads do not go back. Once a read answered `v`, a read
that began after it ended may answer another value only from a write that
can be ordered after the first read -- one that ended after it began, or
one never answered -- until a kill, which may take back what the first saw.
A kill is between two reads where the server may have died after the
first began and before the second ended: it dies somewhere in the kill's
window, from when `docker kill` was sent (it lands a moment later; a death
is held as a kill from the last look before it, a minute back) to when the
server answered again. So a kill sent by the second read's end lets go of
a first read begun before the server was back -- one begun after the kill
was sent saw the process the kill then took, and with it a write the kill
may take; holding it to the kill's send alone failed a right server
whose hot key a read touched in that moment. So two reads during one write cannot answer the new value and then the
old, and after two overlapping writes -- the hot keys' everyday -- the reads
cannot flip between their values: the first that settles on one holds the
rest to it. The checker holds the reads in the order they ended, and keeps
a register's last reads (eight, back to the settling's cutoff: a check
that forgets only ever weakens). A read through the replica is held to the
registers alone, never to the primary's reads before it: it is sent after
the writes answered before it began and may lag the ones answered since.
The soak's mix needed three things YCSB's did not, each in the module with
its tests: a delete (a write of the record's absence), a row a `@ttl`
expired (a write of its absence over the span its time can fall in), and
a write that may not have landed -- one not answered, and one a kill may
have taken (below). Every such write is a value a read may answer, never
one that makes an earlier value impossible. Counters are the module's too:
an increment is not idempotent, so a read must answer at least the certain
increments that ended before it and at most every one begun by its end.
The table of histories in `register.rs`'s tests is run both ways the
module is used -- a cell at once, and settled a minute at a time behind
the reads -- each read alone and, for the atomic rule, each history whole.

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
covers the second sync's fsync a kill does not need. But the rig measured a
sync's own time on a GitHub runner with no compact running; the soak kills
on Docker Desktop's disk, every third kill in a compact, and a slow fsync
keeps answered writes in the process longer. So the bound is a parameter,
`--loss-ms` (`LOSS_MS`), its present value the default, and the run
measures what it bounds: a read after a kill that answered `d` shows a
write gone where the write was answered before the read began, began after
every write of `d` it could have answered ended -- had it landed, the
register could not hold `d` -- and the server had not answered again since
the kill when it was answered (`Registers::gone`). Each such write is
counted once (`lost_writes`) with how long before its kill it was answered
(`lost_ms_max`, the minute's largest, in whole ms rounded up: a gap a
fraction of a ms past the bound, held in ns, reads past it, never as the
bound the report would say the run supports), as the rig's `WINDOW` line
names its first lost write; one past the bound is a mismatch, whose words say by how
much. And each kill's line says whether a compact was writing at it (its
side file, `<file>.beside`, left behind: `kill_compacting`) and whether the
primary's main thread -- its syncer -- was in an fsync then, and for how
long (`kill_sync_ms`): a container in the primary's process namespace
watches `/proc/1/task/1/syscall` every 5 ms from the announced second, and
the kill ends it with the namespace, its last look the thread's state at
the kill. The report prints the run's largest gap beside the bound, and
whether the run supports it or what it suggests (`notes`).

**The mix**, over 32 connections: 36% read and 37% update of YCSB's records
(A, its scrambled zipfian) and 4% read of them through the replica, sent
after the latest write answered (`Fennec-After`, the read's start taken
before the change it names is read: every write answered before the read
began is in it) -- the replica answers once it holds it, so its answer is
held to the registers as the primary's is; never across a restart, nor
after one until the replica says it is connected and holds the newest
change answered before the kill (`/_replication/status`; its reconnect
pauses up to 5 s, and the restarted primary first syncs what the killed
process left), those reads going to the primary meanwhile, and the time it
took on the line (`replica_back_ms`, bounded by `--replica-back-max`, 60
s); 2% increment of one of 1 000 counters (`set counters {n: n + 1}`) and
1% read of one; 3% insert of new keys and 3% delete of the oldest
inserted; 2% read of an inserted key; 4% insert into `sessions`, whose
`seen timestamp @ttl(60s)` expires them, and 2% read of them -- by id,
through a filter of 16 ids, and counted (`count`), each held to the
registers: a row is there until its time and gone from it in every read
("every read leaves them out at once"), within a second either way. The
server stamps `seen` and judges the expiry by its own clock, so the VM's
offset from this host cancels; a drift or a step of it within the ttl is
what the second allows for, and each minute a probe (`put clock {id: 1, t:
now()} returning *`) reads the server's clock against this host's
(`clock_off_ms`, `clock_rtt_ms`), marking a move past 200 ms more than the
probes' own round trips (`clock_step`); 2% `match` on a `@text` body, 2%
`near` on a `vector<16> @hnsw(cosine)` (ten rows), 2% update of a doc. Two
rows for one id, or a row not asked for, is a mismatch.

Beside it 64 subscriptions to `GET /usertable/changes`, a range of 64 keys
each, held to the writes in their range by the change each write was
answered as (`Fennec-Seq`): within a connection the changes rise strictly
from the seed's; each write in the range is in the first event at or past
its change -- a write that may not have landed aside -- and no key is in an
event without a write to it in the changes the event covers; a stream open
past a write in its range for 10 s without reporting it is silent. Each
event's rows are held to the registers over the span from the write that
took its change to when it was read. A deletion of a key outside the
range is passed over: a plain stream judges each id written since its
cursor by its filter alone, as the row stands, and tells one that does
not match as deleted whether or not it ever did (fennec-core's
`Database::changes_since`, "a deliberate looseness": a client deleting a
row it lacks does nothing), so every write outside a range reaches its
stream as one -- the dry run counted each as a key sent twice and a
mismatch, 23.5 million of both in two hours. A row put outside the range
is still a mismatch. A change is known by its number in the primary's
life (`in_life`), turned as a killed server, gone and its file verified,
is started again, each the life of the connection that brought it, read
as that connection was first answered (not at the connect: Docker's
forwarded port takes a connection before the server behind it is there
to answer) -- the connection a worker makes waiting for the server too,
whose life left as the one before counted 4 148 and 4 464 events a key
sent twice in two runs: a kill takes the writes answered from the
server's buffer, and the server started again gives their numbers to the
writes after it. Known by its number alone, an event from
before a kill was held from the start of the write that took its number
after it, and owed that write -- a read ending before it began, "holds no
value written to it", and 7 writes missing in the run that verified the
token. A reconnect -- the server killed --
may replay one thing, the seed: the whole range again, every record of it
there and as written. Each minute a sample of three `match` and three
`near` queries is held to a scan of the same rows: every doc read, `match`
as a filter (`order id`: every row holding a word of the query) against
the docs read holding one, its ranked page among them; `near ... exact`,
the server's own scan, against the ranking over the vectors read, to the
last digits of the f32 sums; and the `@hnsw` walk against `exact` by
recall@10, the run's mean held to 0.925 -- the lowest the site states
(`benchmarks-vector.html`, ef 40 over 1 000 000 x 128), a far harder set
than 20 000 x 16. A doc written while a sample ran is left out of it, from
before its request is sent: under `--sync always` it is visible from when
it lands, before its fsync and its answer.

**The schedule.** `compact usertable` every 20 minutes; every 120 from
minute 50 a backup over the replication stream (`fennec backup` into an
archive, its directory made first: into a path that is none it writes a
database file of its own, which `fennec restore` takes for a sealed backup
and refuses without a key), restored (`fennec restore`), the restored file verified and then
served read-only beside the run, its change number held to the primary's
at the backup's start and 1 000 of the records written so far read from it
and held to the registers as reads over the backup's span (an empty or
stale archive verifies as well as a whole one): a backup counts only when
all of it holds (`backup_stale`, `backup_differs` where it does not); every
30 the server killed (`docker kill --signal KILL`), its file held to its
checksums (`fennec verify`) before it starts again. They queue one behind
another, so a backup due with a compact runs after it; a kill waits for the
queue but every 90 minutes, when it lands in a compact of its own: asked,
and under way for a second when the kill is sent. fennec-server compacts
on its own besides (`--auto-compact`), under the run's memory at a third
dead, and refuses a second compact while one writes its file beside the
database (`engine/maintenance.rs`): such a refusal is counted apart
(`compact_busy`), never failed -- the file was being compacted -- and the
compact a kill is to land in is asked again until it is taken; a kill must
cut one (`compact_killed` at least the kills in a compact), and find it
writing: its side file left at the kill (`kill_compacting` on the line
after each kill the schedule puts in a compact) -- a compact that ended
just before the kill leaves its request cut all the same. The server's
own compactions are read from its `fennec_auto_compactions_total`, the
minute's count on its line (`auto_compacts`, `r_auto_compacts` for the
replica), and whether one was writing at the minute's look from its side
file written in the last minute (`compacting`, `r_compacting`) -- never by
its being there: a kill leaves the side file of a compact it cut, which
the server started again leaves until its next compact makes it anew, and
the second dry run read the twelve minutes after its kill of minute 90 as
a compaction writing, none running, and judged no memory in them (the
same test says a kill found a compact writing, `kill_compacting`). After each restart the keys written in
the 10 s before the kill, or by a write not answered, are read back -- a
sweep that cannot connect or read is counted (`sweep_failed`), never
passed over -- and at the end every key, every counter and every session.

Every wait the run can block on is bounded and named: a docker call (two
minutes unless its own is longer), a kill's wait for the side job before
it and for its compact to be under way, a server's answer after its start
(`WAIT_MAX`, 540 s), and the end's wait for the side jobs still queued,
the same fence a kill's is -- with a line before each wait that could
outlast the guard with another, the end's two verifies among them -- under
the ten minutes the script's guard gives a
soak whose progress file (`--progress`) gained no line (`ENV_SILENT`):
the guard stops it as hung and the script marks the run
(`<sync>:<fraction>:hung`), which fails the report's dead-man. A soak that
failed a verify -- the file copied aside before the server opens it again,
`failed-m<minute>.fennec` -- or counted a mismatch keeps its volumes past
its end, their names printed and listed for the script's cleanup to leave
(`--kept`). Every name a run makes carries its process id, so that no run
removes another's.

**A minute's line**, `results/soak.tsv`, family `soak`, cell
`<sync>:<fraction>:m<minute>`, appended as the minute ends so that a run
cut short keeps what it measured, each line stamped with the harness's
start (`started`, so a run id used again is told apart: the report judges
the latest attempt) and the primary's `life` (its restarts before it):
`ops`, `ops_s` against `rate`, `idle_workers` (connections that ended no
operation in the minute); `behind_s`, how far the open loop is behind its
schedule at the minute's end -- the median connection's lag behind the
operation it waits for or works on, none where it is ahead; `steady_ops`
and their `p50`, `p99`, `p999` and `max` in microseconds, each timed from
when it was due, and `p99_hour`, the steady p99 of the 60 minutes to then,
each written only where there were steady operations: a minute every
connection spent behind its schedule has none, and its quantile written
as 0 read as the run's best minute -- the second dry run's every minute
from 28 to 120; apart from them `restart_ops` and
`p99_restart`, the operations due from a second before a kill until the
server answered again and, after it, until their connection was back on
its schedule -- at 80% the open loop's backlog drains for minutes, its waits
the restart's -- and `compact_ops`, `p99_compact` and `p99_compact_hour`,
those due while a compact ran and until back on schedule after it;
`errors`, `refused` (writes refused by their status) and `down`;
`mismatches`, `lost_writes`, `lost_ms_max`, `backup_differs`; the streams'
`stream_missing`, `stream_twice`, `stream_order`, `stream_late`,
`stream_silent`, `events` and `streams` open; `file`, the database file's
length alone, read through `/proc/1/root` (never the image a compact
writes beside it), and `live`, `fennec_data_bytes`; `compactions` so far,
`auto_compacts`, `r_auto_compacts`, `compacting`, `r_compacting`;
`rss_anon`, `memory_bytes`, `seq` (`fennec_change_sequence`), `cg_anon`, `cg_file`, `cg_current`, `majflt`,
`threads`, `fds` and `connections` (`fennec_connections`) of the primary,
`r_rss_anon`, `r_memory_bytes`, `r_threads`, `r_fds` and `r_connections` of the replica;
`lag` (`/_replication/status`'s `behind`), `lag_s` (those writes at the
minute's write rate) and `r_connected`; the sample's `match_n`,
`match_differs`, `near_n`, `exact_differs`, `recall_sum` and
`sample_skipped`; `host_free` and `vm_free`, the disk on each side; the
run's doings in the minute (`kill`, `verify`, `verify_failed`, `backup`,
`restore`, `backup_failed`, `backup_stale`, `compact`, `compact_failed`,
`compact_busy`, `compact_killed`, `restart_ms`, `swept`, `sweep_owed`,
`sweep_failed`) and a kill's parts, each timed apart -- docker's kill and
wait (`kill_ms`), the verify (`verify_ms`) and the server's own part,
`docker start` to answering (`start_ms`) -- with `kill_compacting`,
`kill_sync_ms` and `replica_back_ms`; `died`, `oom` and `r_died` (a server
gone that the run did not kill, the kernel's OOM kills of it); the clock's
`clock_off_ms`, `clock_rtt_ms` and `clock_step`; and what else used the
machine: `load1`, `load5`, `others_host` (this host's other processes, in
cores, by `ps` -- on macOS a decaying average of about the minute, every
process but the run's and Docker's, the three that took the most named on
the log's line for a minute they took a core or more) and `others_vm` (what the VM's CPUs did
beside the two servers' cgroups, from `/proc/stat` and their `cpu.stat`;
the run's own look, verify and backup containers among it). `:start` says
the run's settings (`replication_buffer` and the VM's `cpus` among them) and the load before it (`load_before`), the peak's `:end` its `peak_ops_s` and `peak_auto_compacts`, `:end` closes a
finished run: totals (the mismatches of the last minutes' reads and events, held only at the end, said apart from the end's read's where the volumes are kept), `swept` against `keys`, the replica's `catchup_s`,
`p99_restart` and `restart_ms_max` over the run, both files verified after
both stopped as a supervisor stops them, and `replica_differs`: the same
read of `usertable`, `counters` and `docs` in id order (`order id`: a page
in storage order would differ after either side compacted) on both once
the replica is at the primary's change, its pages' bytes digested
(`sessions` aside: time, not the position, decides its rows).

**The report**, `ycsb soak report soak.tsv [<run>]`, judges a run by the
item's exit criterion and what holds it, a line each with the minutes that
broke it: 8 hours and an end line; the planned minutes run; every minute
with operations from every connection (the dead-man: a minute nothing was
answered in, a worker stuck, or a soak the guard stopped as hung); the last
hour's steady p99, and its p99 in compacts, at most 1.25 times those of
the earliest hour before it that holds the same schedule -- the same kills,
one in a compact, the same compacts and backups at the same minutes of the
hour: lines 61-120 of an 8-hour run, against 421-480 (`same_hour`), so
that neither a restart's backlog nor a compact's tail weighs more in one
than in the other; a two-hour run has no such hour and is not judged on
it, and neither is an hour with no operation on its schedule, which has
no steady p99 (written as 0, a last hour behind its schedule passed
against any); the file at most 2.2 times the live bytes every minute; the primary's
`rss_anon` beyond what the server holds by its own account grown less
than 5% of its `rss_anon` within each life (it is killed every 30
minutes) and between two compactions, from the median of a stretch's
first five quiet looks from the life's fifth line to the median of its
last five -- what it holds being its engine's
tables and indexes (`memory_bytes`) and its replication feed
(`feed_held`): the records appended since the open, up to
`--replication-buffer`, and the feed's index of them (three `u64`s a
record in vectors grown by doubling, 48 bytes at most, the records counted
by `seq`), a minute's appends its file's growth and a compacted minute the
life's median, the life the records were loaded in full from its first
line -- and the replica's, never killed and its feed full by the first
hour's end, beyond its engine's own (`r_memory_bytes`), from the median of
its first five quiet looks from minute 60 to the median of every five
after, against 5% of its `r_rss_anon` at the first five -- a quiet look
being none in a minute a compact ran or was writing at the look
(`begin_beside` clones each store), so that one look in a compaction never
decides. Judged on `rss_anon` alone, the dry run of 2026-11-22 failed
every life of the primary, the most +75.9%: the feed starts empty at each
open and fills to its 64 MiB at the file's append rate, 3 MB a minute at
80% of the peak (over 20 minutes of a 30-minute life, longer at 50%), its
index some 6% more; with the replica never connected it filled all the
same. Beyond the feed's bytes and the engine's own, the memory was flat
within a megabyte in each life once the feed was full (lives 1-3 at 16.5,
18 and 24 MB, the last a step of 6.7 MB at the compact of minute 100),
growing before that some 0.25 MB a minute with the feed, its index not
counted then (no `seq` on the dry run's lines). A compaction drops what
the engine held of the old file and builds it again, and the heap it let
go of is the allocator's to keep or hand back: the second dry run of
2026-11-23, judged across them, failed one life at +9.4%, its memory
beyond what was held flat at 12-16 MB through minutes 65-80, 26 MB after
the compaction of minutes 81-86 -- the engine's own down from some 60 MB
to 40-48 at it -- and flat again; the other lives' compactions moved it
5 MB at most, and the replica's, never killed, settled at 70-78 MB beyond
its engine's through four compactions of its own. A leak grows inside a
stretch; the largest step across a compaction is said beside the verdict,
where one a compaction of every life made would show. A life with no
stretch of ten quiet looks between two compactions
is not judged, and a run none of whose lives has ten -- the server's own
compactions taking most minutes, which depends on the data and the memory
-- fails the line as "NOT JUDGED, too few quiet looks", each life's quiet
and settled looks and those the server's own compactions took beside it,
never as a leak; threads and descriptors less the
connections open within 2 of their run's median, for both, and the
primary's within 2 from the median of a life's first five looks to its last
five -- a leak of a few a life never leaves a run-wide band -- but a look
over the band by no more than the VM's CPUs less one whose next look is
back within it, said apart: a task the engine cuts in shares runs them on
a thread a CPU but the one that asked, joined before it returns
(`vector::spread` under `std::thread::scope`, `fennec_core::threads`), and
the second dry run failed the primary on one look, minute 17, 7 more on
the VM's 8 CPUs, back the next minute and 0 within each life; no error -- a
status other than a 2xx is one wherever it falls, and so is a connection
error or a 503 saying when to come back outside a restart's window, which
within one is `down`; no mismatch (and the minutes the VM's clock stepped
said beside it); every stream held and all 64 open each minute; the
replica connected each minute and within 5 s of the primary, its first
follow within 1 800 s, back after each restart within `--replica-back-max`,
caught up at the end within 300 s, and holding the primary's rows; every
restart within `--restart-max` (120 s) and the restarts' operations' p99
within the run's longest restart, two seconds and the most the loop was
behind its schedule at a kill's look, a backlog its operations wait out
too; the kills and verifies
the schedule owes for the run's length (`(minutes - 1) / 30`), its
compacts (one the server's own turned away counting), the kills that cut
a compact, backups whose restored records held and restores; every verify,
backup and restore clean; match and near as a scan; every key read back,
after each restart and at the end; no server died and none was
OOM-killed; and the open loop back on its schedule (`behind_s` a second
at most) at some look of every stretch as long as the kill interval -- a
compact's or a restart's backlog drains, at 80% of the peak a kill's in
four times as long as the server was gone, and a loop behind for longer was asked for more than
the server answers, from which on every latency the run writes is its
queue's: the second dry run fell behind at minute 13 and never caught up,
1 656 s behind at its end, from minute 28 every operation a restart's or
a compact's backlog and its p99 of the operations due in a restart
1 640.7 s. A disk short of `--disk-floor` (3 GB) on either side ends the
run as the environment's: the report says so and judges nothing of it
(exit 2), never a pass and never the server's failure. Below the verdicts
it says what the run measured of what was set without one: each kill's
state (a compact writing, an fsync in flight); `--loss-ms` beside the
largest gap of a write a read showed gone, and whether the run supports it
or suggests another; `--restart-max` beside the longest restart and its
parts -- the server's own at the median and at most -- and whether twice
the longest fits it or what twice the longest is; the clock's offset and
steps; and the load before the run and the minutes another process took a
core or more, on this host or in the VM. It exits 1 unless every line
passed. Each criterion has a synthetic run of lines that passes it and one
that breaks it alone, and each finding of the reviews a run that breaks it
or a right server's run it passes
(`the_report_passes_a_good_run_and_fails_each_criterion_alone`).

Not measured: the servers outside Docker's VM (the soak's kill, cgroup and
`/proc` looks are Linux containers'), a primary under a failover (P5's),
and a load the machine shares: the minutes it did are said, not taken
out. A sweep of every key after each restart, rather than of those the kill
could have touched: it would read the whole file through the cgroup's
512 MB sixteen times under load and push the working set out each time,
into the tail the p99 is judged on. An fsync in flight on a thread other
than the primary's main one, where `--sync always` makes them: under it a
kill takes nothing answered, so nothing turns on it.

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
op, and in I/O to the call.** CI runs the gate as two jobs, on every pull
request and in the merge queue; the I/O a durable write, a `/batch`, a
spill and a compact make is counted and judged in the same two (*The I/O*,
below). `perf (count)` builds `crates/fennec-http/examples/gate.rs` twice --
at the merge's first parent, which is a pull request's base branch or the
commit a merge group queues onto, and in the change -- and counts each of
its workloads under valgrind's callgrind, five runs a side, each run
tallying what its loop asked of the allocator. `perf (instructions)`
judges the counts, and is the check a merge requires (since 2026-11-21). A side's figure
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
([conventions.md](conventions.md#reruns)), made on 2026-11-21, once the
holdout below had passed.

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
own files and every `PERF_ACCEPTED` entry the pull request adds, edits or
removes. The gate's own files, each in the words the judge prints:
`gate.rs` and `tools/perf_gate.py` ("one of the gate's own files"); in
`ci.yml`, the `perf-count` and `perf` jobs ("the gate's own job"), every
job they need, on and on ("a job the gate's `perf` needs, which can hand
it an output or a file" -- none today), a local action or workflow they
use ("`./x`, which a job of the gate's uses" -- none today), and the
workflow outside its `jobs:` ("`ci.yml` outside its `jobs:` is edited
(`env`)"): its `env` -- a `BASH_ENV` there runs a script of the tree's
before every step's first command, the judge's `python3 -I` among them --
its `defaults` (a step's shell), `permissions`, `concurrency`, `on`
(whether the jobs run at all) and the rest; and in any workflow, a job
whose check may be shown under a gate job's name or id ("its job `j` is
named as one of the gate's"), whose result could stand for the gate's: a
name written so, or one an expression may make so -- a `${{ matrix.<key>
}}` any value its matrix writes out, any other expression any text --
named where its lines, a job it needs or its workflow's own keys change,
so `release.yml`'s `${{ matrix.name }}` jobs, whose matrices name
platforms, are not. The workflows are compared as YAML reads them, by a
reader of the checker's own (`yaml_lines`; Python has no YAML, and the
checker imports nothing of the tree's), which the tests hold to
`tools/workflow_check.py`'s reader over every workflow of the repository
and sixteen snippets a reader of lines gets wrong (Ruby's YAML reads each
the same): a comment, on a line of its own or after a value, and a blank
line holding nothing are no edit, while a `#` line or a blank one inside
a scalar -- a `run: |` script's, a quoted one's, a plain one running on to
the next line -- is, as is a trailing space in a block scalar (a script's
`\ ` is no continuation). Where the workflow reads a YAML alias, every
edit of it is named: through one, any line may reach the gate's jobs. So
is every edit, a comment's too, of a workflow holding a character Python's
strings take for a line's break or a space and YAML 1.2 for neither
(`\v`, `\f`, `\x1c`-`\x1f`, `\x85`, U+00A0, U+1680, U+2000-U+200A,
U+2028, U+2029, U+202F, U+205F, U+3000; no workflow holds one): readers of
YAML differ there -- YAML 1.1 broke a line at `\x85`, U+2028 and U+2029 --
so a line one reads as a comment another may run, and the reader, which
ends a line where YAML 1.2 does, cannot say which GitHub's is; with
`str.splitlines`, which breaks at eight of them, a `# ; curl ... | sh`
after one on a line of `perf-count`'s script was a comment of its own line
and the edit named by nothing. The paths a change touches are read as git
stores them (`git diff -z`): without it git quotes a path holding a byte
past ASCII, a `"`, a `\` or a control character, and a workflow named
`ü.yml` holding a job named `perf (count)` was no workflow's; a byte that
is not UTF-8 is kept, handed back to git as it came and written escaped
(`\xff`), never dropped.
Over main's last 120 landings the judge names three edits the two jobs'
lines did not, each of `ci.yml`'s own keys: #617's `concurrency`, #656's
`on` and #672's `permissions`. main asks for no review, and pull requests
come from the owner's account, so a reviewer approves nothing: such a
pull request is the owner's to approve, and the agent merging it waits
for the owner's OK rather than merging on a green check. The warnings are
no lock, and this is the gate's bootstrap limit: on `pull_request` and in
a merge group GitHub runs the workflow of the commit it tests, the pull
request's own, so a pull request that edits the workflow is judged by its
own workflow, and an edit to it can leave the warnings out -- the judge
can only name the edit, and the owner's OK is the lock. So the merging
agent reads the diff for these files and entries too. What no check
holds: the count runs the change's code, and a build script or a workload
that forges the count forges the gate -- as does any job of the run, an
edit of whose lines is not the gate's, that uploads over the count's
artifacts (`overwrite`) before the judge downloads them, and any job given
`checks: write`, which could post a check run named `perf (count)` through
the API: an edit of another job, named by nothing, of the same class.

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
sides, only 231 a call would bring it to +1%. `perf (instructions)` stayed
a check no merge required until the corrected workflow had a green run on
main (the holdout's, 43709604383); since 2026-11-21 it is required, one of
the 38 checks every merge group passes, `size (browser module)` alone left
out ([conventions.md](conventions.md#reruns)).

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

*The I/O.* Callgrind counts no system call: a change that doubled a
durable write's fsyncs, broke a `/batch` into a block a statement, or made
a compaction write more passed the count above. So the count job goes on,
over the same two trees and target directories -- its `run` keeps them
(`--keep`), and `perf_gate.py io` builds fennec-server in each beside the
gate program, the crates both reach built once -- to run each of
`gate.rs`'s `IO_WORKLOADS` against each side's server, three runs a side,
and the judge holds them to the base's by the same checker, the same
`PERF_ACCEPTED` and the same rule for a workload the base had (`check` on a
results file of `"metric": "io"`):

| workload | its requests | what it stands for |
|---|---|---|
| `io-put` | 500 puts of a row of a 530-byte text, each answered before the next | a durable write: its fsync, of the [sync log](durability.md#the-sync-log), the log filling some 450 puts in and the file synced in its place |
| `io-batch` | a `/batch` of 1 000 puts of a 16-byte text, one block of some 24 KB | a block is one fsync however many writes it holds: the group commit |
| `io-spill` | a `/batch` of 2 000 rows of a 9 000-byte text, some 18 MB | a block past 16 MB [spilling into the file](storage.md#spills) before it lands |
| `io-compact` | `compact` of a file of 10 000 rows each written twice | [compaction](storage.md#compaction): the live half written anew and renamed over |

The server runs under `--sync always` -- every answer waits for its
fsync, so nothing is left for a clock to sync -- with `--auto-compact
off`, whose look comes every 5 s by the clock, and `--exit-on-stdin-close`,
so the run's end, or its driver's death, is the one shutdown and no server
outlives it. `gate.rs` is only the client, never traced: `strace -f -y -s 0
--seccomp-bpf` runs the server alone, stopping it at the calls asked for
and no others. The setup goes over one connection, the run's requests over
a second, and the driver's own `GET /_health` over a third once the run is
answered; what is counted is what any thread of the server did between its
taking the second connection and its taking the third (`accept4`, traced
for that), so the start-up and the setup before, and the shutdown's sync
after, are not. Counted, each to the call and each by its name: the syncs
that make a file durable (`fsyncs`) -- `fsync` and `fdatasync` of the file
they name; `syncfs`, which syncs a whole file system, as `(file system)`'s;
`msync`, which names no descriptor, as `(mapped)`'s, and a durability only
with `MS_SYNC` (`MS_ASYNC` asks for nothing the kernel does not do
anyway); `sync`, at the least an fsync of every file, as `(every file)`'s
-- and apart from them the writebacks that make nothing durable
(`writebacks`): `sync_file_range`, whatever its flags, since it flushes
neither the drive's cache nor the file's metadata, arm's
`sync_file_range2`, and an `msync` without `MS_SYNC`. Then renames
(`rename`, `renameat`, `renameat2`); the calls writing to a file (`write`,
`pwrite64`, `writev`, `pwritev`, `pwritev2`, `copy_file_range`,
`sendfile`, `splice`, and `fallocate`, which gives a file room and returns
no bytes) and the bytes they returned; the bytes written to a file and not
yet made durable when the server next began to write to a socket
(`unsynced`, below); the files created (an open with `O_CREAT` or `O_TMPFILE`, whether
or not the file was there, which the trace cannot tell and a run repeats;
`creat`; `mkdir`; `mknod`; `symlink`), truncated (`ftruncate`, `truncate`,
an open with `O_TRUNC`), linked (`link`, `linkat`) and unlinked (`unlink`,
`unlinkat`, `rmdir`); and the calls of all these that failed (`failed`,
their bytes none), each by its call and file. A descriptor is a file
where `-y` names it by a path outside `/dev`, `/proc` and `/sys`, one
unlinked since it was opened by its name and ` (deleted)`, a file of its
own: an answer written to a socket is no file's, nor the server's log, its
stderr, which is the driver's pipe, nor its stdout, `/dev/null`. Beside the
counts, the database file's size once the setup was answered
(`file_before`). The table gives each count in total, fsyncs an answer,
bytes written a byte the client sent and a byte of the file the setup
left, and under it each file's counts by its name in the data directory --
the database, its `.sync` log, the file a compact writes, `.` the
directory -- each sync and each failure by its call.

*What the trace cannot see is refused.* A call strace is not asked for is
a write the gate cannot see, and one a change adds would pass on counts
that never moved; worse, some ways of making a write durable, or of
writing it at another cost, use the very calls the gate counts. So a
server is refused -- the run cannot be judged, in words naming the call,
the file and the flag -- where, anywhere in the trace, the start-up as
much as the stretch, it set up io_uring (its submissions read off a ring by the
kernel, no call each) or Linux AIO (`io_setup`, `io_submit`: submitted in
batches, completed with no call); opened a file of the data directory with
`O_SYNC` or `O_DSYNC` -- or `__O_SYNC` alone, the bit `O_SYNC` adds, which
strace names apart -- whose every write then flushes the device with the
same calls, or `O_DIRECT` (by `fcntl` too), which passes the page cache by
and changes what a byte written costs; wrote with `pwritev2` and
`RWF_SYNC` or `RWF_DSYNC`; set a file's attributes by `ioctl`
(`FS_IOC_SETFLAGS`, `chattr`, whose `FS_SYNC_FL` makes every write
durable, and `FS_IOC_FSSETXATTR`'s `FS_XFLAG_SYNC`) or cloned blocks into
it (`FICLONE`, `FICLONERANGE`, a write no write call shows). One the
kernel refused took nothing and is passed over. A store into a shared,
writable mapping of a file is a write no call shows too, and is not
refused: the mappings are not traced, as *Its time* says why. The
engine's own opens use none of these flags: in `crates/`, `custom_flags`
appears in shipped Unix code only in `fennec-server`'s hand-over of its
files to `--user`, read-only and `O_NOFOLLOW |
O_NONBLOCK`, which only a server started as root runs, never one under
the gate, and otherwise on Windows (`FILE_FLAG_BACKUP_SEMANTICS`) and in
tests (`O_NOCTTY`); no `O_SYNC`, `O_DSYNC`, `O_DIRECT`, `RWF_*`,
`io_setup` or `pwritev2` is named, and no `ioctl`; so the base passes
every refusal, as the rounds that brought them showed: every run of all
four workloads. And every file a counted call names is named within
the data directory: the scratch directory is resolved first (`realpath`)
and the server handed that, since `-y` names a file by its path with every link
resolved -- through a linked work directory every file would have read as
outside it on both sides, and been judged as one -- and a run that counts a
file outside it is refused, naming the call and the path. So is one with a
counted call on a descriptor, or a relative path in a directory, that
strace did not name (`fsync(9) = 0`): dropped, as it was before, it would
have agreed with itself on both sides.

*Exact, so held to the call.* Nothing a workload sends, or the server
writes between those connections, is sized by a clock or a draw. The ids,
values and texts are fixed, and so is the `Host`: `localhost`, not the port
the system picked, whose digits would have moved the bytes sent. Where the
engine writes a time or a draw into a file it moves no count. The sync
log's generation, begun from the clock in nanoseconds where the log is new
(`SyncLog::open`), goes into its header and every entry as eight bytes
whatever it reads, and so does each entry's hash of it; `<file>.lock`
holds the process's id in digits, which a run's number moves, but is
written at the open, before the counted stretch. A record's claim --
where a completed fsync left the file -- is the same place every run when
every write is answered before the next; the idempotency keys' rows, a
`@ttl` read and `now()` are no workload's. So the counts repeat to the
call, and are judged so: every run of a side must count the same -- each
count in total and each file's, and the file after the setup -- or the job
fails naming every run's value of each count that moved; and any count of
this tree's that is not the base's fails, up or down -- fewer fsyncs a
durable write is durability lost -- unless the pull request adds a
`PERF_ACCEPTED` entry naming the workload and the count (`"metric":
"fsyncs"`, `"writebacks"`, `"renames"`, `"writes"`, `"written"`,
`"unsynced"`, `"failed"`, `"creates"`, `"truncates"`, `"links"`,
`"unlinks"` or `"file_before"`) and the move it lets through, which way
and how far: a signed share of the base's count or a signed number
(`"+100%"`, `"+1"`, `"-50%"`). No threshold: a count that holds to the
call has no draw to stand above.

*File by file, call by call.* Fsyncs, writebacks and bytes written are
judged by the file they went to, named within the data directory, which is
the same from run to run, and their entry names it (`"file":
"io.fennec"`); each sync and writeback by its call too -- `` fdatasync of
`io.fennec.sync` `` is one count, `` fsync of `io.fennec.sync` `` another --
and its entry names the call as it names the file (`"call":
"fdatasync"`); the bytes unsynced at an answer by file too, and a failed
call by its call and file -- a failed write in a failed fsync's place is
two moves, where its total was one count that held; write calls, renames
and the files created, truncated, linked and unlinked are judged in
total, an entry letting each through as any count. A change that syncs
the database where it synced its log -- as many fsyncs, each some 356 µs where the log's took 65
([durability.md](durability.md#the-sync-log)) -- or writes to one what it
wrote to the other moves no total, and is what the log exists against. One
that syncs the log with `fsync` where it did with `fdatasync` -- one word,
`sync_data` to `sync_all` -- moves no file's count, and on ext4 adds a
journal commit to every durable put, the cost the log exists to save; a
`syncfs` in its place syncs the whole file system each time; a
`sync_file_range` makes nothing durable. Judged file by file alone, as the
second version was, all three passed; judged by call, each is two counts
that moved, and the last is under `io-put`'s floor too. The table shows
each file's fsyncs summed beside its calls: the sum moves only where a
call's count does, and is not judged twice.

*Answered before it is durable.* Which comes first, a write or its sync,
moves none of the counts above: a server that synced its log before
writing the record into it, and then answered, makes the same writes and
the same syncs as one that wrote, synced and answered -- and a put
answered so is lost to a power cut after the client heard it. So at each
write the server makes to a socket -- an answer: `writev` or another
write call to a socket, or `sendto`, `sendmsg` or `sendmmsg`, the calls
std's `TcpStream::write` makes -- the bytes written to each file of the
data directory since its last durable sync (an `fsync` or `fdatasync` of
the file, a `syncfs` or `sync` of every file) are counted, each byte once, at the first answer after it
was written (`unsynced`, file by file). A rename carries a file's bytes
to its new name, and an unlinked file's are no answer's. Under `--sync
always`, one request in flight, every workload's log record is durable
when its answer goes -- 0 of `io.fennec.sync` in all four -- while the
database's own bytes are not, by design: the sync log holds them
([durability.md](durability.md#the-sync-log)), and the file is synced
only where the log fills. So `io-put` answers 274 260 of the 274 810
bytes it writes into `io.fennec` before they are durable -- all but the
put whose answer followed the file's own fsync -- and `io-batch` its whole
block, 23 883; the spill and the compact sync what they wrote before they
answer, 0. That is the base's own count, judged exactly as any other.
Nothing in the server's threads moves it: the answer is written by the
thread that committed, after its sync returned, with no other request in
flight, and twenty runs of each workload counted it the same (below). The
log synced before its record is written -- one line moved in
`SyncLog::append` -- fails with no entry, every other count of every file
the same (*What fails*).

An answer counts where its call began, every other call where it
returned. strace prints a call's beginning as it stops the thread on the
way in, before the call runs, and its return as it stops it on the way
out; another thread's line can fall between (`<unfinished ...>`, `<...
resumed>`). An answer's bytes reach the client inside the call, and the
driver connects again -- the stretch's end -- once it has read them, so
on a loaded runner (four traced servers, their clients and strace on
four vCPUs) the accept's return can be printed before the answer's:
placed at its return, as round four placed it, the last answer would
fall past the stretch -- `io-batch`'s 23 883 bytes read as none,
`io-put`'s 274 260 as 273 710 -- and runs of a build that changed nothing
would differ on a required check (the twenty runs that agreed ran at a
load near 1). Its beginning comes before its client can connect. The
rest stay at their returns, which no other thread's line moves: an
accept takes its connection as it returns (it began waiting long
before), and the server hands the connection on only after, so a
workload's calls all begin past it, and the setup's last answer begins
before it; a sync makes a file durable as it returns -- placed where it
began, one still running when an answer began would pass for done, and
the two returns' order would decide the count; a write's bytes and a
failure's error are known there; and what the server does for a request
returns before that request's answer begins, on the answering thread in
turn or on one it waited for, which strace let go on only once it had
printed the return. A call the server did not wait for before answering
falls either side of an end in any reading: its runs differ, and the
judge says so. The tests hold each on hand-written lines as strace prints
them -- the last answer resumed after the driver's accept, printed whole
or resumed from its wait, and `io-batch.strace`'s answer so split, count
as resumed before it; the setup's last answer straddling the workload's
accept counts as the setup's; a sync interleaved with other threads'
lines counts as one printed whole, and one still in flight when its
answer began counts its record unsynced whichever return is printed
first -- and the checker before this fails the first and the last. The
captures in `tools/perf_gate_traces/` count the same in both readings,
none holding an answer another thread's line split.

*The file after the setup.* The client sends both sides the same setup,
so the size the database file has once it is answered is the server's: a
change to what the setup writes -- a record's encoding, the schema's, the
file's header -- grows it for the same documents, and that is worth a pull
request saying so. Every run of a side must agree on it, as on a count;
between the sides it is judged as the count it is, its failure naming the
file after the setup, never requests that differed, and an entry lets it
through (`"metric": "file_before"`). Each workload's entry is its own, as
every other count's is: the setups differ -- a collection's schema alone
in three, 4 MB of rows in the compact's -- so one change moves each by its
own share, and an entry for all four would let through a workload whose
setup grew for another reason.

*What an entry lets through.* The move it names, exactly: the counts are
exact, so an entry of `+500` for a move of one would leave room for 499
more, which a later commit of the same pull request, or a merge's, could
take unseen. A number is the move to the call; a share is the move's share
of the base's count to the decimals it gives (`+0.00019%` for 8 bytes of
4 199 800, `+100%` for 1 → 2), and a share of a count of none is none: a
file a side did not sync, synced now, is let through by a number
(`"+1"`), and the verdict says so where a share was given. An entry larger
than its move fails it and is named so (``an entry names the move it
lets through, and `+500` is larger than this one (+1)``). Of several
entries naming one count, each lets through its own move: a `+1` beside a
`+5` lets a move of one or of five through, never three, and a `+1`
beside a `-1` either way by one. The
entries are the instructions' -- added by the pull request, never let
through by the base's, read as data -- and so is the rule for a workload:
one the base's own `gate.rs` lists that this tree dropped, or the base's
server did not run, fails; one new to the program that the base cannot run
is held to nothing. A count that moved past the entries naming it is said
with them (``which its entries (`+1`) do not let through``), and a share
given for a count from none says so besides.

*What a workload sends is this tree's.* Its requests -- `IO_WORKLOADS`'
count, and what each holds -- are not held to the base's program's for a
workload both list: both sides are sent this tree's client's requests,
byte for byte, so a count compares like with like whatever they are, and
the run's floor follows the requests answered. A workload resized or
rewritten is an edit of `gate.rs`, which the judge names for the owner, as
it is for an instruction workload's ops; held to the base's number, a
resize would fail with no entry to say why, and a rewrite that kept the
number -- a row of 200 bytes for one of 530 -- would pass all the same.

*Floors.* Counts that missed would agree with each other: a strace asked
for none of a call, or a `-y` that named no file, counts nothing on both
sides, and every workload would pass. So each workload has a floor
(`IO_FLOORS`), and a run of either side under it fails, whatever an entry
says. Each sends durable writes, one at a time, under `--sync always`,
where a write is answered only once it has synced and one request in
flight shares its sync with none: an fsync an answer at the least -- a
sync that makes a file durable (`fsync`, `fdatasync`, `syncfs`, `sync`,
an `msync` with `MS_SYNC`), never a writeback, so a `sync_file_range` in
a durable write's place falls under it -- and a write call. `io-put`'s
floor is 500 fsyncs and a write call; `io-batch`'s and `io-spill`'s, one
answer each, an fsync and a write call;
`io-compact`'s an fsync, a write call and a rename, the new file renamed
over the old. Under it a durable write was answered before its sync, or
the trace missed a call, and neither is a count to let through. A workload
named nowhere there is held to an fsync and a write call, and one new to
the program, held to nothing, to no floor: one that writes nothing on
purpose names its own.

*By the base's checker but once.* The pull request that brought the I/O
counts was counted and judged by its own checker: its base's `gate.rs`
listed no `IO_WORKLOADS`, and its checker had no `io`. Which checker runs
is the base's program's to say: the base's wherever the base's `gate.rs`
mentions `IO_WORKLOADS`, however it is spelt, this tree's only where it
mentions it nowhere, and a base checker that then cannot count them fails
the step. Both jobs decide it in two lines of shell, no Python of the
change's -- `checker=tools/perf_gate_base.py`, then `grep -q IO_WORKLOADS`
of the base's `gate.rs` setting `checker=tools/perf_gate.py` only where
grep read the file and found none (its exit 1; a file it cannot read
leaves the base's) -- and `tools/test_perf_gate.py` runs each job's own
two lines under bash against programs that list the table, list none, or
name it otherwise, and the committed `gate.rs` itself, and holds their
choice to `io_checker`'s, so the two cannot drift. The first version asked
the base's checker for `io --help` and fell back where it failed: a later
change that dropped or broke `io` would have had every pull request after
it counted and judged by its own. The second ran this tree's `perf_gate.py
io-checker` to decide: a pull request editing it, or the checker's
dispatch, chose the checker that counted and judged it, and the judge job,
whose comment says nothing of the change runs there, ran the pull
request's Python. The command is gone, not kept as a convenience: a local
`make perf-gate-io` counts and judges with this tree's checker anyway, and
a command no job runs would be a second statement of the rule to drift.
The third grepped for `const IO_WORKLOADS` at a line's start: a base whose
table was `pub const`, `pub(crate) const` or `static` read as one with
none, and the pull request after it, and every one after that, was judged
by its own checker. The base's server answers every request the workloads
send, so all four are compared from that first run on, as the gate's own
first run compared every workload its base ran.

*Where the base's checker is written.* Under `tools/`, as
`tools/perf_gate_base.py`: a checker finds the tree it reads as the
directory above its own (`ROOT`), main's too, so outside the checkout it
would read no tree. That path is the pull request's tree's, so the step
that writes it decides by the base alone: it refuses, in words, a change
whose `tools/` is a link or that tracks anything at the path, clears the
path, and writes the base's file as `git show` gives it, this tree's copy
only where `git ls-tree HEAD^1 -- tools/perf_gate.py` lists the base's
tree and no file in it (exit 0, nothing listed); an error of git's --
the tree unreadable, or the file it lists -- fails the step in words.
Round four decided by `git cat-file -e`'s exit 128, which is also git's
answer where the base's `tools` tree cannot be read ("exists on disk, but
not in 'HEAD^1'"): this tree's checker would then have counted and
judged. The base's `gate.rs` is read in the same step and the same way,
into `$RUNNER_TEMP`, before anything of the change runs. From the gate's first version (#619)
the copy was written by a redirection whose failure fell back to this
tree's copy: a
pull request committing a directory at the path made the write fail, its
own checker was copied into the directory, and every later call ran the
directory's `__main__.py` -- the judge too -- and one committing a link to
`/dev/null` took the base's bytes there and ran nothing, no `built=`
written and every judging step skipped, the job green.
`tools/test_perf_gate.py` runs each job's first steps under bash in
scratch repositories: a directory and a link at the path, `tools/` a link,
a base with no checker (this tree's, and only then), a base with no
parent, one whose checker git cannot read, and one whose `tools` tree,
program's directory or program git cannot read -- each but the bootstrap
failing with nothing of the change's run. With the step's `git cat-file
-e`, the unreadable trees took this tree's checker or an empty program:
four of those subtests fail.

*The results kept apart.* The count job keeps the instructions' results
(`perf-results`) before the I/O runs, and the I/O's in an artifact of
their own after (`perf-io-results`), each step running whatever became of
the other (`!cancelled()`): a strace refused, a server's crash, a run
answering fewer requests or a phase past its 300 s leaves the
instructions to be judged, and an instruction count that failed leaves
the I/O to be. The first artifact is never uploaded again -- the second
version re-uploaded both files under its name, `overwrite: true`, which
deletes the first before it uploads, so a second upload that failed lost
both. The judge downloads each apart, into a directory of its own that
holds nothing else it reads -- the base's `gate.rs`, unpacked beside them
in the third version, could have been replaced by an artifact carrying a
file of its name, and with it the choice of checker -- judges each set
whatever became of the other, says which it lacks (`results.json`,
`io.json`), and then fails a count that did not end well. Without strace, `io` and `io-measure` say
so and count nothing -- on a Mac, whose developer runs them in Docker --
but fail under CI (`GITHUB_ACTIONS`); a results file the judge cannot read
is a failure that names it.

*The counts, held to themselves.* One build's server against itself, 20
runs a side -- 40 runs of each workload, in Docker on an M1 (aarch64 musl,
strace 6.19), four at a time while the machine around it ran a load of 20
to 70 -- counted each workload the same to the call and the byte, every
run one line of counts; the build merged with what landed since counted
the same, three runs a side, and so did the tree merged again (with
`train-d`) once each sync was counted by its call and the directory's
changes were counted too, and the tree merged with `train-d` once more,
ten runs a side -- twenty of each workload -- once the bytes unsynced at
an answer and each failure by its call were counted too, and the sends
and `ioctl` traced. x86_64's first runs are CI's: the pull request
that brought the I/O counts changes no server code, so its own run is the
base against itself on x86_64 musl, every workload's line the same on
both sides. Those counts, in total and file by file, each file's syncs by
their call -- no `syncfs`, `sync`, `msync` or writeback in any workload --
as the judgement's table gives them:

| workload | answered | file after setup | fsyncs | an answer | writebacks | renames | created | truncated | linked | unlinked | write calls | bytes written | unsynced at an answer | a byte sent | a byte of the file after setup |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `io-put` | 500 | 32 | 500 | 1.00 | 0 | 0 | 0 | 0 | 0 | 0 | 1 001 | 565 166 | 274 260 | 1.64 | - |
| `io-batch` | 1 | 32 | 1 | 1.00 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 47 862 | 23 883 | 0.60 | - |
| `io-spill` | 1 | 32 | 1 | 1.00 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 18 019 912 | 0 | 0.99 | - |
| `io-compact` | 1 | 4 199 800 | 6 | 6.00 | 0 | 1 | 1 | 1 | 0 | 1 | 5 | 2 139 997 | 0 | - | 0.51 |

| workload | file | fsyncs | fsync | fdatasync | bytes written | write calls | unsynced at an answer | renamed to it | created, truncated, linked, unlinked, failed |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| `io-put` | `io.fennec` | 1 | 0 | 1 | 274 810 | 500 | 274 260 | 0 | - |
| `io-put` | `io.fennec.sync` | 499 | 0 | 499 | 290 356 | 501 | 0 | 0 | - |
| `io-batch` | `io.fennec` | 0 | 0 | 0 | 23 883 | 1 | 23 883 | 0 | - |
| `io-batch` | `io.fennec.sync` | 1 | 0 | 1 | 23 979 | 2 | 0 | 0 | - |
| `io-spill` | `io.fennec` | 1 | 0 | 1 | 18 019 912 | 2 | 0 | 0 | - |
| `io-compact` | `.` | 2 | 2 | 0 | 0 | 0 | 0 | 0 | - |
| `io-compact` | `io.fennec` | 1 | 0 | 1 | 0 | 0 | 0 | 1 | - |
| `io-compact` | `io.fennec.beside` | 3 | 0 | 3 | 2 139 997 | 5 | 0 | 0 | created 1, truncated 1 |
| `io-compact` | `io.fennec.sync` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | unlinked 1 |

A durable put is one fsync, and its record's bytes written twice -- into
the file and into the log beside it, 550 and 581 bytes a put, the log's
price for the journal commit it saves ([durability.md](durability.md#the-sync-log))
-- until the log fills, some 450 puts in: that put syncs the file in its
place, and the next writes the log's header for a new generation. A
`/batch` of 1 000 is one block and one fsync of the log, its 23 883 bytes
written to the file and again to the log. The spill writes its 16.8 MB
spill record and 1.2 MB land straight into the file and syncs it once,
never the log, which takes no entry past 64 KB. The compact makes its
side file (an open with `O_CREAT` and `O_TRUNC`, a file created and one
truncated), writes the live half into it, 0.51 of the file, and syncs six
times, each an `fdatasync` but the directory's `fsync`s: the new file
three, the old one once -- the log may hold what it lacks, and goes,
durably, before the rename, the one file unlinked -- and the directory
twice, after the log's removal and after the rename. A durable put's log
filling makes no file: the next generation is written over the same one.
One of the six syncs nothing: the image synced, the
first catch-up round (`maintenance.rs`) finds no write made meanwhile --
there is one writer, waiting on the compact -- and syncs the side file
again, an `fdatasync` with nothing written since. Dropping it is a change
of its own; the gate would read it as `io-compact`'s `fdatasync`s of
`io.fennec.beside` 3 → 2, let through by an entry of `"-1"` naming that
file and that call.

*What fails, and what lets it through.* Four builds, each a patch never
committed, against the build they patch, three runs a side, through the
checker as the judge runs it. One `fdatasync` of the database more a
durable write (`FileSink`'s sync): `io-put`'s `fdatasync`s of `io.fennec`
1 → 501, `io-batch`'s 0 → 1 and `io-spill`'s 1 → 2 failed, every run of a
side alike, `io-compact` as it was; an entry each that the pull request
added, naming the file and the call, let them through (`+500`, `+1` -- a
`+100%` of none let nothing through, and said so -- and `+100%`), and the
same entries held by the base failed them again. Eight bytes more in the
schema record `create` writes, which its reading passes over: the file
after the setup 32 → 40 in three workloads and 4 199 800 → 4 199 808 in
the compact's, every count of the runs the same; failed as the file after
the setup, let through by a `file_before` entry of `+8` each, failed again
with the base holding them -- where the first version's checker failed all
four as sides sent other requests, past any entry. The database synced
where its log was (`SyncLog::append` syncing nothing, its caller the
file): `io-put`'s `fdatasync`s of `io.fennec` 1 → 500 and of
`io.fennec.sync` 499 → 0, `io-batch`'s 0 → 1 and 1 → 0, every total the
same -- failed, where the first version's checker, judging totals, passed
all four. The log synced with `fsync` where it was with `fdatasync` (one
word in `SyncLog::append`, `sync_data` to `sync_all`): `io-put`'s
`fdatasync`s of `io.fennec.sync` 499 → 0 and its `fsync`s 0 → 499,
`io-batch`'s 1 → 0 and 0 → 1, every file's count the same -- failed, let
through by the four entries naming each call (`-100%`, `+499`; `-100%`,
`+1`), failed again with the base holding them; the second version's
checker, judging each file's syncs as one count, passed all four on the
same results. A fifth, the log synced before its record is written (one
line moved in `SyncLog::append`: `sync_data` above `write_all_at`), each
put then answered before its record was durable, every other count of
every file the same: `io-put`'s bytes unsynced at an answer of
`io.fennec.sync` 0 → 290 228 -- its 290 356 bytes but the generation's
header, written and synced before the first record -- and `io-batch`'s
0 → 23 915, failed with no entry, let through by an entry of each move
the pull request added and failed again with the base holding them, an
entry one byte larger named as larger than its move; the third version's
checker, which counted no order, passed all four. The four builds'
results judged again by this round's checker came out as they did, every
verdict the same. The `syncfs` and `sync_file_range` swaps and an `msync`
without `MS_SYNC`, which no patch here builds, are held by the tests on
traces as strace prints them, each passed by the second version's
checker.

*Its time.* The runs are short: in Docker on the M1, started at a
1-minute load of 3.98 (4.21 after), a run of `io-put` took 1.1 s, of the
others 0.3 to 0.5 s, and the step's 24 -- four workloads, three runs a
side, four at a time -- 2.5 s (10 s while the machine ran a load of 20).
Those runs traced the syncs, renames and writes alone; the opens, `fcntl`
and the directory's changes traced since are a few calls a run more, not
timed again: the machine's load stayed past 30 for the hour they waited.
What strace prints stands for what it costs -- each line a call the
server stopped at -- so the calls asked for since are weighed by their
lines on the four workloads' traces: the sends, `ioctl`, `mknod` and
`symlink` add 4 lines to 2 178 (0.18%), the start-up's `ioctl` of stdin,
none in the counted stretch. The mappings would have added 4 778 --
`mmap` 4 581, `mremap` 78, `mprotect` 119, nearly all the allocator's,
`io-compact`'s 3 700 among them -- 3.2 times the lines, 1.4 times in the
stretch: refusing a shared, writable mapping of a file of the data
directory would have cost far past a tenth of the step's runs, so the
mappings are not traced (eighty runs, twenty of each workload, took 24 s
with them and 13 s without, a minute apart in the same Docker VM on a
loaded Mac: no timing, but the same way).
What the step pays is the two servers' build. On a hosted runner
`build (release + cli profile)` built fennec-server in 73 s, 53 of them its
own crate and the link, full LTO into one codegen unit; neither tree's
gate program built that, nor fennec-http with its `studio` and `tls`
features, fennec-wire or fennec-import, some 7 s before it, while
fennec-core, fennec-ql, fennec-abi and fennec-tls are the gate program's,
built once. The two sides' links run side by side, each on a thread of
its own. So the step adds an estimated 70 to 85 s a pull request -- an
estimate from those timings and the runs here, not a run of the job.
The workloads are sized for it: 1 000 puts of a 200-byte text, the first
version's, paid twice the fsyncs and never filled the log, and 20 000 rows
of 1 000 bytes, a block of about the spill's size, ten times the
statements to parse and answer.

*What it does not catch.* Instructions, not cycles: a cache miss, a
branch mispredicted, memory's bandwidth and a system call's time cost
nothing here, and a change that trades instructions for any of them
passes. The I/O is counted in calls and bytes, not in time: an fsync
slower on one file system than another, a write that waits for the disk,
the kernel writing back a mapped file's pages, and reads -- the file is
mapped, and a read is a page fault no call shows -- are not counted, nor
is a store into a mapped file, which is no call: the engine maps its file
`PROT_READ, MAP_SHARED`, so it stores nothing there, but a change that
mapped it writable is not seen, the mappings not being traced (*Its
time*). A call the trace does not ask for is not seen, and io_uring,
Linux AIO, a flag or an `ioctl` that makes each write durable or direct,
and a call strace named no file of are refused rather than counted. Not traced at all: the
extended attributes' calls (`setxattr` and its kin), metadata no workload
writes, and `fadvise64`, advice -- the engine's one, `SideFile::sync`'s
`POSIX_FADV_DONTNEED`, comes after its fsync -- neither a byte written
nor a sync. `unsynced` is bytes written to files: a file made, renamed or
unlinked and answered before its directory was synced moves no byte, and
is held only by the directory's fsyncs, which are counted by file and
call; and every write to a socket in the stretch is taken for an answer,
as under the workloads, one connection answering, it is. Nothing outside
the counted span is counted -- a server's open, the setup, and the
shutdown's sync -- nor anything the clock runs, `--sync <ms>`'s syncer and
the write behind the lock outside `--sync always` among them
([durability.md](durability.md#writes-behind-the-lock)). Every workload runs on one thread -- an I/O workload
one connection, a request in flight -- so what threads cost -- a lock's
contention, connections side by side and the fsync their writes share, a
batch's vectors linked in parallel -- is not counted. One target: x86_64
musl's code on Linux -- the I/O counts were shown exact on aarch64 musl in
Docker, and x86_64's first runs are the pull request's that brought them,
in CI -- not aarch64's NEON kernels, macOS's or Windows' builds -- nor
their I/O: macOS keeps no sync log and flushes the drive by
`F_FULLFSYNC`, Windows flushes the renamed file and its directory
([windows.md](windows.md#3-durability-on-ntfs)) -- nor the browser
module's (wasm32, whose size `make wasm-sizes` and the size job hold). No
workload writes vectors in its loop -- the graph each `near` reads is
linked in its setup, uncounted, and no I/O workload has a vector, so a
graph appended to the file (`link::keep`) or a shutdown's checkpoint is no
count -- and none applies a primary's records as a server's replica does
([replication.md](replication.md#replication)), nor counts what a
replica's apply or a primary's feed writes and syncs; `sync-apply` is a
sync replica's apply alone, in instructions. A change on those paths is
measured by hand: `make instr-compare`, `make sweep`, `make replica-bench`,
`make roundtrip-bench` with `--strace` in `RT_ARGS`.

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
change's code had run before the judgement, on the same `PATH`. For the
I/O, `strace -c`, the plan's first thought: its table is each call's
total over the process by name -- the start-up, the setup and the shutdown
with the run, a socket's writes and the log's pipe with a file's, no byte
and no file -- so a durable write's fsync would stand among the answers'
writes, and a sync moved from the log to the database would read the same;
counting by descriptor, file and stretch is what holds a run to the call.
The I/O judged in total alone, as the first version had it: an fsync of
the database where the log's was passed. Every call of the fsync family
one count, judged file by file, as the second had it: an `fsync` where an
`fdatasync` was, a `syncfs` and a `sync_file_range` all passed, the last
meeting `io-put`'s floor of 500 though it made nothing durable, and an
`msync` with `MS_ASYNC`, which does nothing, met it too. The bytes not
durable at an answer counted again at every answer -- each file's bytes
since its last sync, at each write to a socket -- rather than once: an
answer written in two calls would count them twice, and the database's
file, left to the sync log by design, would be counted at each of
`io-put`'s 500 answers, a sum that grows as the square of the puts between
the file's syncs and moves with any change to how answers are written.
An entry as a ceiling, as the third version had it -- a `+500` letting a
move of one through -- for exact counts: the room it leaves is a later
change's to take unseen. Tracing the mappings (`mmap`, `mremap`,
`mprotect`) to refuse a shared, writable mapping of a file of the data
directory: 4 778 lines more on the four workloads' 2 178, the allocator's
(*Its time*), for a write the engine does not make -- it maps its file
`PROT_READ, MAP_SHARED` -- and a change would make only by design. Refusing every
open of the data directory rather than reading its flags: the engine
opens and makes files in the counted stretch -- the compact's side file,
the log's next generation -- and those are counts. The file after the setup held
as the requests sent are: a change to what the setup writes failed every
workload, and no entry could let it through. One entry for it across the
four workloads: each setup grows by its own share, and a workload whose
setup grew for another reason would pass with the rest.

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

The fifth train (#677, 2026-11-21) was the first to meet the budgets in
its own run, and went past two: `fennec` 5 028 624 bytes (+135 168,
+2.76%) and `fennec-server` 8 378 960 (+86 016, +1.04%), with `make small`
2 972 160 (+4 096) inside its own. Built a step of the train at a time on
macOS, the symbols kept, the code it added splits by branch. G14's
PostgreSQL client (verify-full off this machine, SCRAM alone and bound to
TLS, a follower's copy that holds no lock) is 43% of the shell's growth
and 58% of the server's, some 21 KB of `fennec_wire::client` in each. The
sealed format's version 3, which only the shell's `backup`, `archive` and
`restore` hold, is 30% of the shell's, and the SQLite reader 26%; a
schema's names held in a set natively, where a SQLite table may declare
32 767 of them, is all of `make small`'s page. The HTTP edge (a JWKS file
read again with a back-off, an `Idempotency-Key` bound to its caller,
refusals and MCP rates counted by caller) is 29% of the server's, and
db-64's registry that waits on no lock after its shutdown 18%. The file
was written from that run, each budget the one the job printed.

The sixth train (#678, 2026-11-22) took `make small` 544 bytes past its
budget: 2 988 544 bytes, four pages (+16 384, +0.55%) more than its
measurement, where the budget's half a percent left 15 840. `fennec`
(5 049 104, +0.41%) and `fennec-server` (8 411 856, +0.39%) stayed inside
theirs. Built a step at a time on macOS, the symbols kept, the shell
without its default features grew 14 205 bytes of `__TEXT` (code, constants
and strings): G50's read-only reader, which surveys a file by reading it
rather than through a mapping of its whole length, 56%; G14 F5's file
helpers (`create_private`, `copy_private`, `mode_like`, `still_named`) 26%;
the compact that keeps places 16%; the fuzz fix's walk over a damaged tail
3%. G14 F1 and the release fixtures compile nothing into it. Only
`small`'s entry was written from that run.

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
