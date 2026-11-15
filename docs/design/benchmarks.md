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
after, are not. Counted, each to the call: the fsync family's calls on
files -- `fsync`, `fdatasync` and `sync_file_range`; `syncfs`, which syncs
the file system, as the file's it names; `msync`, which names no
descriptor, as `(mapped)`'s; `sync`, at the least an fsync of every file,
as `(every file)`'s -- renames (`rename`, `renameat`, `renameat2`), the
calls writing to a file (`write`, `pwrite64`, `writev`, `pwritev`,
`pwritev2`, `copy_file_range`, `sendfile`, `splice`, and `fallocate`, which
gives a file room and returns no bytes) and the bytes they returned, and
the calls of those that failed (`failed`, their bytes none). A descriptor
is a file where `-y` names it by a path outside `/dev`, `/proc` and `/sys`,
one unlinked since it was opened by the name it had (`(deleted)`): an
answer written to a socket is no file's, nor the server's log, its stderr,
which is the driver's pipe, nor its stdout, `/dev/null`. A call strace is
not asked for is a write the gate cannot see, and one a change adds would
pass on counts that never moved; so a server that sets up io_uring
anywhere in the trace -- its submissions read off a ring by the kernel, no
call each -- is refused, the gate to learn the call before it counts
such a server. Beside the counts, the database file's size once the setup
was answered (`file_before`). The table gives each count in total, fsyncs
an answer, bytes written a byte the client sent and a byte of the file the
setup left, and under it each file's counts by its name in the data
directory: the database, its `.sync` log, the file a compact writes, `.`
the directory.

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
"fsyncs"`, `"renames"`, `"writes"`, `"written"`, `"failed"` or
`"file_before"`) and how far it may move, and which way: a signed share of
the base's count or a signed number (`"+100%"`, `"+1"`, `"-50%"`). No
threshold: a count that holds to the call has no draw to stand above.

*File by file.* Fsyncs and bytes written are judged by the file they went
to, named within the data directory, which is the same from run to run,
and their entry names it (`"file": "io.fennec"`); write calls, renames and
failed calls are judged in total. A change that syncs the database where
it synced its log -- as many fsyncs, each some 356 µs where the log's took
65 ([durability.md](durability.md#the-sync-log)) -- or writes to one what
it wrote to the other moves no total, and is what the log exists against.

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

*What an entry lets through.* A share is of the base's count, so a share
of a count of none is none: a file a side did not sync, synced now, is let
through by a number (`"+1"`), and the verdict says so where a share was
given. Of several entries naming one count, each lets through what it
would alone, so the widest stands and none narrows another: a `+1` beside
a `+5` lets a move of five through, and a `+1` beside a `-1` either. The
entries are the instructions' -- added by the pull request, never let
through by the base's, read as data -- and so is the rule for a workload:
one the base's own `gate.rs` lists that this tree dropped, or the base's
server did not run, fails; one new to the program that the base cannot run
is held to nothing.

*Floors.* Counts that missed would agree with each other: a strace asked
for none of a call, or a `-y` that named no file, counts nothing on both
sides, and every workload would pass. So each workload has a floor
(`IO_FLOORS`), and a run of either side under it fails, whatever an entry
says. Each sends durable writes, one at a time, under `--sync always`,
where a write is answered only once it has synced and one request in
flight shares its sync with none: an fsync an answer at the least, and a
write call. `io-put`'s floor is 500 fsyncs and a write call; `io-batch`'s
and `io-spill`'s, one answer each, an fsync and a write call;
`io-compact`'s an fsync, a write call and a rename, the new file renamed
over the old. Under it a durable write was answered before its sync, or
the trace missed a call, and neither is a count to let through. A workload
named nowhere there is held to an fsync and a write call, and one new to
the program, held to nothing, to no floor: one that writes nothing on
purpose names its own.

*By the base's checker but once.* The pull request that brought the I/O
counts was counted and judged by its own checker: its base's `gate.rs`
listed no `IO_WORKLOADS`, and its checker had no `io`. Which checker runs
is the base's program's to say (`perf_gate.py io-checker`, held by the
tests): the base's wherever the base's `gate.rs` lists `IO_WORKLOADS`,
this tree's only where it lists none, and a base checker that then cannot
count them fails the step. The first version asked the base's checker for
`io --help` and fell back where it failed: a later change that dropped or
broke `io` would have had every pull request after it counted and judged
by its own. The decision is this tree's copy's, in both jobs -- a gate
file, its every edit named for the owner, as the jobs' steps are. The
base's server answers every request the workloads send, so all four are
compared from that first run on, as the gate's own first run compared
every workload its base ran.

*The results kept apart.* The count job keeps the instructions' results
before the I/O runs, and the I/O's beside them after, in the same artifact
(`perf-results`): a strace refused, a server's crash, a run answering
fewer requests or a phase past its 300 s leaves the instructions to be
judged. The judge judges each set whatever became of the other, says which
it lacks (`results.json`, `io.json`), and then fails a count that did not
end well. Without strace, `io` and `io-measure` say so and count nothing --
on a Mac, whose developer runs them in Docker -- but fail under CI
(`GITHUB_ACTIONS`); a results file the judge cannot read is a failure that
names it.

*The counts, held to themselves.* One build's server against itself, 20
runs a side -- 40 runs of each workload, in Docker on an M1 (aarch64 musl,
strace 6.19), four at a time while the machine around it ran a load of 20
to 70 -- counted each workload the same to the call and the byte, every
run one line of counts; the build merged with what landed since counted
the same, three runs a side. x86_64's first runs are CI's: the pull
request that brought the I/O counts changes no server code, so its own
run is the base against itself on x86_64 musl, every workload's line the
same on both sides. Those counts, in total and file by file, as the
judgement's table gives them:

| workload | answered | file after setup | fsyncs | an answer | renames | write calls | bytes written | a byte sent | a byte of the file after setup |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `io-put` | 500 | 32 | 500 | 1.00 | 0 | 1 001 | 565 166 | 1.64 | - |
| `io-batch` | 1 | 32 | 1 | 1.00 | 0 | 3 | 47 862 | 0.60 | - |
| `io-spill` | 1 | 32 | 1 | 1.00 | 0 | 2 | 18 019 912 | 0.99 | - |
| `io-compact` | 1 | 4 199 800 | 6 | 6.00 | 1 | 5 | 2 139 997 | - | 0.51 |

| workload | file | fsyncs | bytes written | write calls | renamed to it |
|---|---|---:|---:|---:|---:|
| `io-put` | `io.fennec` | 1 | 274 810 | 500 | 0 |
| `io-put` | `io.fennec.sync` | 499 | 290 356 | 501 | 0 |
| `io-batch` | `io.fennec` | 0 | 23 883 | 1 | 0 |
| `io-batch` | `io.fennec.sync` | 1 | 23 979 | 2 | 0 |
| `io-spill` | `io.fennec` | 1 | 18 019 912 | 2 | 0 |
| `io-compact` | `io.fennec.beside` | 3 | 2 139 997 | 5 | 0 |
| `io-compact` | `io.fennec` | 1 | 0 | 0 | 1 |
| `io-compact` | `.` | 2 | 0 | 0 | 0 |

A durable put is one fsync, and its record's bytes written twice -- into
the file and into the log beside it, 550 and 581 bytes a put, the log's
price for the journal commit it saves ([durability.md](durability.md#the-sync-log))
-- until the log fills, some 450 puts in: that put syncs the file in its
place, and the next writes the log's header for a new generation. A
`/batch` of 1 000 is one block and one fsync of the log, its 23 883 bytes
written to the file and again to the log. The spill writes its 16.8 MB
spill record and 1.2 MB land straight into the file and syncs it once,
never the log, which takes no entry past 64 KB. The compact writes the
live half anew, 0.51 of the file, and syncs six times: the new file three,
the old one once -- the log may hold what it lacks, and goes, durably,
before the rename -- and the directory twice, after the log's removal and
after the rename. One of the six syncs nothing: the image synced, the
first catch-up round (`maintenance.rs`) finds no write made meanwhile --
there is one writer, waiting on the compact -- and syncs the side file
again, an `fdatasync` with nothing written since. Dropping it is a change
of its own; the gate would read it as `io-compact`'s fsyncs of
`io.fennec.beside` 3 → 2, let through by an entry of `"-1"` naming that
file.

*What fails, and what lets it through.* Three builds, each a patch never
committed, against the build they patch, three runs a side, through the
checker as the judge runs it. One `fdatasync` of the database more a
durable write (`FileSink`'s sync): `io-put`'s fsyncs of `io.fennec` 1 →
501, `io-batch`'s 0 → 1 and `io-spill`'s 1 → 2 failed, every run of a side
alike, `io-compact` as it was; an entry each that the pull request added
let them through (`+500`, `+1` -- a `+100%` of none let nothing through,
and said so -- and `+100%`), and the same entries held by the base failed
them again. Eight bytes more in the schema record `create` writes, which
its reading passes over: the file after the setup 32 → 40 in three
workloads and 4 199 800 → 4 199 808 in the compact's, every count of the
runs the same; failed as the file after the setup, let through by a
`file_before` entry of `+8` each, failed again with the base holding them
-- where the first version's checker failed all four as sides sent other
requests, past any entry. The database synced where its log was
(`SyncLog::append` syncing nothing, its caller the file): `io-put`'s fsyncs
of `io.fennec` 1 → 500 and of `io.fennec.sync` 499 → 0, `io-batch`'s 0 →
1 and 1 → 0, every total the same -- failed, where the first version's
checker, judging totals, passed all four.

*Its time.* The runs are short: in Docker on the M1, started at a
1-minute load of 3.98 (4.21 after), a run of `io-put` took 1.1 s, of the
others 0.3 to 0.5 s, and the step's 24 -- four workloads, three runs a
side, four at a time -- 2.5 s (10 s while the machine ran a load of 20).
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
is a store into a mapped file, which is no call; a call the trace does not
ask for is not seen, and io_uring's is refused rather than counted. Nor
is anything the clock runs, `--sync <ms>`'s syncer and the write behind the
lock outside `--sync always` among them
([durability.md](durability.md#writes-behind-the-lock)), nor a server's
open and shutdown. Every workload runs on one thread -- an I/O workload
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
the database where the log's was passed. The file after the setup held
as the requests sent are: a change to what the setup writes failed every
workload, and no entry could let it through. One entry for it across the
four workloads: each setup grows by its own share, and a workload whose
setup grew for another reason would pass with the rest.
