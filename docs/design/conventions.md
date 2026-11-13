# Conventions

The long form of the test and docs conventions [CLAUDE.md](../../CLAUDE.md) lists under *Conventions*: where tests live and which run alone, a test that fails on one run, Chrome's launch, the clock, ports, what a change a user meets owes the docs, and the fuzzing of every parser of untrusted bytes. They hold no invariant of the code, so [README.md](README.md#the-original-order) does not number them. Each section below is a bullet moved word for word from CLAUDE.md, which keeps its rule in a sentence or two.

## Test binaries

Unit tests live inline in `#[cfg(test)] mod tests`; cross-crate and
protocol tests in `crates/*/tests/`, a file each, gathered into one binary
a crate by `tests/all.rs` (`autotests = false`: a new file needs its `mod`
line there, or it is never built). A test reading the process's own
counts, `/_metrics` or the statements', is a `[[test]]` of its own; so is
one whose servers hold many threads (past macOS's 2 048 a process, a
server answers 503 to whichever test asked), and so are `fennec-http`'s
`tests/audit.rs` and `fennec-shard`'s `tests/refusals.rs`: the wait after a
refusal is the process's, so the other tests' servers ask with none
(`audit::set_delay(0)` in their helpers). Measurement programs are each
crate's `examples/` and `fennec-bench`'s binaries, wired to `make` targets,
not to CI.

## Reruns

A test that fails and passes on a rerun is a bug, the test's or the
code's: reproduce it -- in a loop, under load (`docker run --cpus=3`
beside busy loops is GitHub's three-core runner) -- and fix it, never
retry it away or lengthen a sleep. A test waits for an event, not for a
time: it holds what it races (`Replica.setOnline(false)` before a write it
reads back, a call held under way across a burst, a disk's fsync held at a
gate) and bounds only the wait for something that must happen. A CI log
names every failing test with its assertion (cargo's and `swift test`'s
own output, Gradle's `testLogging`, `dart test`'s GitHub reporter, `node
--test --test-reporter=spec`); a failing mobile job uploads
`test-reports-android` or `test-results-apple` (the simulator's
`.xcresult`).

## Chrome

A step that launches Chrome reads Chrome's own files first, or a cold
runner's disk spends 5 to 20 s of the launcher's wait (`waiting for
dynamic debugging port`): the studio's harness (`warm`) and
`examples/{shop,analytics}/scripts/warm-chrome.mjs` do; a longer wait only
moves the bound.

A `waitForFunction` predicate returns `null` for what is not on the page
yet and never throws. Puppeteer starts the page's poller with `void
poller.start()`, so a throw -- `document.querySelector('.ed')` is `null`
until the query editor, a module fetched by its first use, is mounted --
is a `pageerror` and not a failed wait: the polling ends and the wait runs
out its timeout with nothing said of why. That was the studio's `cell`
after a call form's submit: the test's own call answered before the editor
had mounted, 5 runs in 100 on one core (`docker run --cpus=1`) and none in
84 with the root checked.

## The clock

A test reads the clock from below. A sleep never ends early, so `took >=
wait` holds on any machine, while a bound from above measures it: macOS
gives a QoS-clamped job's timers hundreds of milliseconds of leeway
(`taskpolicy -c background` tries it). Where the time is the property -- a
held request answered before its wait, a node that stops once its lease is
out -- the bound is what it stands against, with seconds to spare: half
the wait, the lease the node reports it has left. Elsewhere the test
asserts the event, the count or the order: the wait a refusal would take
(`audit::would_wait`), a grant queued behind another
(`Router::grants_waiting`), the grants a node took while another hangs
(`Router::round_grants`). A timeout a test gives its servers or clients
bounds every request they make, not just the one it is about (a router's
upstream timeout of 600 ms failed a tenant's create at background QoS).

## Ports

A server a test or a harness starts listens on port 0 (`--http
127.0.0.1:0`, `--listen 127.0.0.1:0`, Node's `listen(0)`) and is found by
the `listening on: http://<address>` line on stderr of fennec-server and
fennec-shard (their usage names it, `fennec-shard`'s `tests/listen.rs` holds
it): a fixed port, or one found free and handed over, met another suite's
server. A router is told its nodes by the addresses they said. A test
stops a server as a supervisor does, with `SIGTERM`, on Windows with
Ctrl+Break to its process group -- the harness starts each in a group of
its own (`CREATE_NEW_PROCESS_GROUP`), on the console it shares with them
-- and kills one with `SIGKILL`, on Windows `TerminateProcess`
(fennec-server's `support::Server::signal`). Only a site
whose address is needed before it binds -- a Next.js build renders it, the
database's CORS names it -- takes a port the system picks, held by a
stand-in listener until the moment it starts (`examples/shop`,
`examples/dispatch`'s `ci.sh`).

An address a test means nothing to listen on is `127.0.0.1:1`, which no
system hands out, never a port picked and let go of: Linux hands a free
port out at random, and a listener of a test running beside it took it. A
router meant to find its node refused forwarded to another test's router
(`fennec-shard`'s `cors.rs`, once in 50 runs), and a relay meant for a
region whose routers refuse it was answered by one naming its own node
(`unsent.rs`, once in 300 runs of `unsent::`; none in 600 on port 1), each
in Docker's Linux, three cores beside six busy loops. In Docker's Linux a
port let go of came back from a `bind` to port 0 17 times in 100 000, and
none while a connection its listener accepted was open or closing, so a
stand-in node closed after it answered keeps its port while the test holds
that connection or has just closed it.

## Docs a change owes

A change that adds or alters something a user meets says so where a user
looks, in the same pull request: its reference page under
`site/content/docs/`; README's feature table and the links beside it; the
home page (`site/content/index.html`) and the docs index when a visitor
would pick fennecdb for the feature; and the SDK lists (`languages.html`,
`mobile.html`, `sharding-read-replicas.html`'s SDK paragraph, `integrations.html`'s)
when an SDK changes. A section added or changed is found by its words
alone until the site's search vector cache has its vector: after sections
change, commit the next `site` deploy's `search-embeddings` artifact as
`site/search-embeddings.json` (`gh run download <run> -n
search-embeddings`) where it differs.

## Fuzzing

Every parser of bytes a process takes from outside is fuzzed:
`crates/fennec-fuzz`, a target each -- `http` (`http::read_request`, a
request's head and its body by length or chunked, pipelined, read whole or
a few bytes at a time as a slow client sends them), `json`
(`json::parse_json`, `parse`, a page's documents and a query's parameters,
and what parses written back and read as itself), `fennecql` (`parse` and
`parse_exact` over the repository's FennecQL corpus,
`tools/fennecql_corpus.py --list`), `codec` (a value's and a type's
encoding decoded, skipped, read by a path and as a document's fields, and
a value read written back as its bytes), `image` (a `.fennec` file opened,
read whole and its indexes built, and its records applied as a replica
applies a primary's; the first byte of an input says whether its records
are summed anew, so an open reads what a record holds rather than stopping
at its sum) and `sqlite` (`fennec import`'s reader over a file). `make fuzz`
lists them, `make fuzz TARGET=<name>` runs one; a new parser of untrusted
bytes gets its target in the pull request that adds it, and
`.github/workflows/fuzz.yml`'s matrix its name, which the crate's tests
hold to the registry.

*The fuzzer is the repository's own.* A mutator of its own
(`src/mutate.rs`: a bit flipped, a byte or a word set to a value on an
edge, bytes inserted, dropped, copied or repeated ten thousand times, a
length written as fennecdb's varint or SQLite's at an edge, a run of digits
made another number, a token of the target's or of its seeds put in, two
inputs spliced), guided by the coverage LLVM's sanitizer coverage counts
on every edge of the code it compiles (`-Cpasses=sancov-module
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters`): each instrumented
module hands its counters to `__sanitizer_cov_8bit_counters_init`, the one
function of libFuzzer's runtime the instrumentation calls, written in
`src/cover.rs`, and an input reaching a counter's bucket of counts (AFL's:
1, 2, 3, 4-7, ... 128-255) for the first time is kept. libFuzzer through
`cargo fuzz` would have been a crate from outside the workspace in a
crate of it, which the zero-dependency rule
([crates.md](crates.md#zero-dependencies)) keeps out; what the fuzzer
needs of it is these few hundred lines. It runs under AddressSanitizer,
a nightly flag (`-Zsanitizer=address`), so the build that fuzzes takes the
Makefile's `FUZZ_NIGHTLY`, a date, and only it and the jobs running it do;
a plain build of the same targets runs in `cargo test` (`tests/all.rs`:
each over its seeds and 300 of their mutations), unguided and unsanitized.
The fuzz profile keeps every check a debug build makes, so an overflow that
wraps in a release build is a panic, a finding.

*Whatever ends a run keeps its input* (`src/guard.rs`). A panic is caught
around the run, and one the code under it caught and went on from is noted
by the panic hook: either ends the fuzzer, the input written to
`target/fuzz/crashes/`. A stack overflow, a failed allocation and
AddressSanitizer's report end in `abort` -- ASan told so by
`__asan_default_options` -- whose handler writes the input from where the
run left it with nothing but system calls. A run past `--timeout-ms`
(30 s of wall time: at 10 s a machine at a load of 40 took a 2.5 s input
for a hang) is written by a watchdog thread, an allocation past
`--max-alloc-mb` (2 GiB) aborts before it is made: a length read off the
input and allocated before it met the input. A run is on a thread of a
server's connection's stack, 8 MiB (`RUN_STACK`), so what overflows here
overflows there. `fennec-fuzz replay <target> <file>` runs one again.

*A finding is a regression test in its own crate*, the input's bytes or
the file made by hand, failing before the fix. The pull request that added
the fuzzer fixed what was known of these parsers then -- a value nested
100 000 levels deep ([crates.md](crates.md#how-deep-a-value-nests)), a
SQLite file whose b-tree loops, whose cell declares 2^62 bytes, or whose
column's type follows a space of two bytes
([crates.md](crates.md#a-sqlite-files-lengths)) -- and what the `image`
target found in its first half hour: a file's schema holding an index
over several fields with no part, whose first part the plan of a `get`
read (`tests/composite.rs`); a frame putting an id no write gives
(`store::written_id`, `tests/damage.rs`) -- document 0, which the id
index's dense array grew over and indexed at -1, and `u64::MAX`, past
which the next id wrapped to 0; a frame's length near 2^64, added
unchecked past the replay's test (`tests/damage.rs`); a graph record
counting 2^32 nodes over a few bytes, whose arrays asked for 32 GiB
before a node was read (`vector::tests`); and a sparse vector read back
with the last `u32` as an index, whose pgvector form overflowed adding
one (`sparse::tests`). Two are left for a decision. JSON's reader takes a
number past a double's range, `1e400`, as infinity, which its writer
writes as `null`: no crash, and the `json` target holds its round trip to
finite values until that is decided. And a vector field's `m` and
dimension are read from a file as written -- a field a build before their
bounds made opens as it did (`engine::altered`) -- so a crafted file
declaring `@hnsw` with m = 2^40 asks the index's build for 2m slots a
node, which the `image` target will meet.

*CI* runs each target for 30 s under ASan on every pull request (ci.yml's
`fuzz-smoke`, `make fuzz-smoke`) and for 30 minutes each on Sundays
(`fuzz.yml`), each week's corpus going on from the last's; a finding is
kept as the job's artifact. Not yet fuzzed, after 1.0: a sealed archive's
and a backup's frames (`seal.rs`), an archive's segments, PostgreSQL's
messages and `pgoutput` (`fennec-wire`), a JWT and a JWKS (`access`), the
sync's messages through the native library and the browser module, and
IndexedDB's chunks; nor is each target held to a count of runs since its
code last changed.
