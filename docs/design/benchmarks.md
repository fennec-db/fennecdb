# Benchmarks

The long form of the invariants [crates/fennec-bench/CLAUDE.md](../../crates/fennec-bench/CLAUDE.md) lists under *Benchmarks*: what `make scale-bench` and `make ycsb` measure and how, and [the gate](#the-gate) every pull request's instruction counts are held to. The first two sections are paragraphs moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists. [Instruction counts](#instruction-counts) holds no invariant of the code: it is how two builds' counts are read on macOS, which `tools/instr/` runs.

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
in an operation's time. A pass over all six systems found no mismatch
(`ycsb/verify.tsv`). `ycsb/results.tsv` is the three runs the site
quotes, appended to and never rewritten -- a run's cell measured again
counts as its last line, and a run a later one replaced is named in
`ycsb/superseded.tsv` and counts in no median (`ycsb report`, `site/build.py`)
-- and `site/build.py` holds every YCSB figure on the site to its median
there, the full grid row by row. Docker's VM syncs a
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

**A pull request is held to its base in instructions an op.** CI's `perf
(instructions)` job, on every pull request and in the merge queue, builds
`crates/fennec-http/examples/gate.rs` twice -- at the merge's first parent,
which is a pull request's base branch or the commit a merge group queues
onto, and in the change -- and counts each of its workloads under
valgrind's callgrind, five runs a side. A side's figure is the median of
its runs' instructions; a workload whose figure is 1% or more above the
base's fails the job (`tools/perf_gate.py`), unless the pull request adds
an entry to `PERF_ACCEPTED`, in the same file: the workload, the most it
lets through (`"+1.8%"`) and why, in 40 characters at the least -- what the
instructions buy, and what `make perf-gate` and `make instr-compare`
measured. The job judges with the base's copy of the checker, reading the
pull request's entries as data, never running them, as
`tools/openapi_compat.py` reads `BREAKS` ([http.md](http.md#compatibility)):
a pull request that lowers the bar, cuts the runs or rewrites the rule is
judged by the rule it would replace. An entry lets its workload through
only in the pull request that adds it -- one the base holds already lets
nothing through, and an entry edited counts as added -- so the list is the
history of what was let through, never cut. A workload the base's build
cannot run, one over a statement the change brings, is held to nothing and
says so; one this tree's build cannot run fails the job. Making the job a
required check is a setting of the branch's protection, beside `test
(ubuntu-latest)`, `test (macos-latest)` and `build (release + cli
profile)`.

*What is counted.* One program, this tree's source copied into the base's
worktree, so both sides run the same work against their own crates; a
program the base's crates do not build -- an interface the change altered
-- falls back to the base's own copy, its workloads held to these by name.
It is built for `x86_64-unknown-linux-musl` in the release profile, the
code the release's binaries and the image are, its symbols kept
(`CARGO_PROFILE_RELEASE_STRIP=false`): stripping moves no instruction, and
a profile of a stripped binary names no function. Each workload sets its
data up from a fixed seed, then runs its ops in a loop, and only the loop is
counted: valgrind starts with `--instr-atstart=no --collect-atstart=no`,
and the program asks callgrind to instrument and count as the loop starts
and to stop as it ends -- client requests, the magic sequences of
valgrind's `valgrind.h` written as inline assembly for x86_64 and aarch64,
which a processor runs as no work. Setup so runs under valgrind's core
alone, about five times slower than native, where instrumented it ran some
eighty times slower. A run that counts nothing -- valgrind passed a
sequence by -- is refused. Every workload is in process and on one thread,
with no socket: the figures over the wire are `make requests-bench`'s,
`make roundtrip-bench`'s, `make load-bench`'s, `make scale-bench`'s, `make
ycsb`'s and `make beir`'s, whose clients, servers in Docker and systems
beside them an instruction count cannot hold. `make perf-gate` runs the
job's count and judgement on Linux with valgrind; `HEAD=<ref>` replays two
commits, as the calibration below did. Each side's median run's profile is
kept (`target/perf-gate/profiles/<workload>.<side>.callgrind`), for
`callgrind_annotate` to say which function a difference is in.

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
found by diffing two runs' profiles by function. The clock: `--request-rate`'s window is
swept once a second of the wall's time (`rate::Held::take`), forgetting the
callers that asked nothing in it, and a caller forgotten is taken in anew
at its next request. A thousand subjects in turn -- `rows.rs`'s -- took two
seconds a turn under callgrind, and a hundred on a loaded machine more than
one at times: how many each sweep forgot was the clock's, and a scoped
request's count moved by 2 to 6% from one run to the next. Four subjects
each ask again within milliseconds, and the sweep forgets none. The draw:
std's hash tables take their keys at random in each process
(`RandomState`), and a facet through a `@hash` index walks its buckets in
the order the keys lay its table out, `order_rows`' pick of the ten most
comparing as many times as that order makes it: one database's facet cost
some 15% apart from one process to the next. Each op of `core-facet` asks
the next of 256 databases, each with keys of its own (a thread's tables
take its keys with the first moved by one, a new key each), and a run
counts the mean of 256 draws. Ten runs a side then spread 1.1% for `facet`,
0.03% for the scoped read, 0.8% and 1.0% for the scoped insert and set --
the set within 0.04% over eight runs made one at a time -- and 0.4% for the
filtered `near`: a step a workload's lookups pay or not, which the median
of five takes, the threshold standing well above it.

*The threshold.* CALIBRATION PENDING: the replays of merged pull requests
`make instr-compare` judged neutral, and of known regressions, and the base
held to itself twenty times, in this change's CI.

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
| `core-facet` | the brands of every row through `@hash`, over 256 databases in turn | a facet over every row through `@hash`: 0.01 ms ([benchmarks-text.html](../../site/content/docs/benchmarks-text.html#text-search)) |
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

*What it does not hold yet.* Callgrind counts no system call: a change that
doubles a durable write's fsyncs, or a compaction's bytes written, passes
it. Those counts -- fsyncs and bytes written a durable write, a `/batch`, a
spill and a compact, under `strace -f -c` -- are to be another `metric` of
the same results file, over the same two builds and the same checker, the
job's step after this one's.

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
