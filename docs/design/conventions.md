# Conventions

The long form of the test and docs conventions [CLAUDE.md](../../CLAUDE.md) lists under *Conventions*: where tests live and which run alone, a test that fails on one run, Chrome's launch, the clock, ports, what a change a user meets owes the docs, the fuzzing of every parser of untrusted bytes, and how a secret is compared and a primitive held to its vectors. They hold no invariant of the code, so [README.md](README.md#the-original-order) does not number them. Each section below is a bullet moved word for word from CLAUDE.md, which keeps its rule in a sentence or two.

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

## Skips

A suite that skips itself when `web/fennec.wasm`, `web/fennec-lite.wasm`,
`web/fennec-min.wasm` or a binary cargo builds is missing is right on a
developer's machine and wrong on CI: about 120 JS tests in ten suites once
skipped there, and a job that built nothing reported green. Under `CI` a
missing prerequisite throws (`web/ci-prereq.mjs`'s `needs`, called after the
suite finds what it needs, as `studio/test/transport.test.mjs` already did),
and a Rust test that skips for a missing program (`fixture!` in
`fennec-import/tests/sqlite.rs`, the inflater in `fennec-http/src/gzip.rs`)
panics. What no runner can run -- the tests that need a live PostgreSQL, the
exhaustive float sweeps -- is counted: `make test` copies node's spec
reporter and each cargo run to `target/skips/`, and `tools/skips_check.py`
fails a job whose `skipped` or `ignored` sum passes
`tools/skips-allowlist.json`'s count for its runner (`Linux`, `macOS`;
`RUNNER_OS`), each entry carrying its reason. A platform where a suite cannot
run gets an entry, never a silent skip. A log that parsed nothing (no node
`skipped N` line, no cargo `test result:` line) fails under CI as well, so a
changed reporter cannot read as zero skips. `CI` means set and non-empty, in
Rust, JS and Python alike. The program-dependent checks in
`fennec-cli/src/types.rs` assert `python3` under CI (both images ship it);
`swiftc`, `dart` and `gofmt` stay deliberately uncounted: the test job
installs none of them (a Mac image has swiftc, an Ubuntu one not), and a developer's machine
without one skips only that language's syntax check of a generated file. The Windows jobs run `cargo test`
by hand, outside `make test`, and are not counted yet.

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

`make stress-test` is that reproduction as a target (`tools/stress-test.sh`):
the Rust suites (`cargo test --workspace --exclude fennec-bench`, or one crate
with `CRATE=`) and the JS tests (`node --test web/*.test.js`, on the node 22 CI
runs, from nodejs.org's tarball checked against its pinned SHASUMS256, not
Debian's 18) `ITER=` times (20), inside
`docker run --cpus=3` beside three busy loops, in the rust image of
`rust-toolchain.toml`'s release by digest (`IMAGE=`). Everything runs in the
container, load included: many loopback clients on the host once exhausted
its ports and cut every agent off the API. The script starts every busy loop
and kills it by PID on every way out (exit, Ctrl-C, a failure), and kills
its container by name if interrupted. The cargo home, the target directory
and a copy of the sources (made by content, as `tools/image-cargo.sh` does,
so an older checkout is not taken for built) live in a Docker volume
named for the architecture and the image's digest
(`fennec-stress-<arch>-<digest>`) and locked while a run uses it, so the
iterations rebuild nothing and two runs do not race; `docker volume rm` of it
clears it. An iteration runs every test binary (`--no-fail-fast`); the run
stops after the first iteration that fails, keeps that
iteration's log (the directory is printed) and names the iteration and the
failing tests; a passing iteration's log is dropped. `TIMEOUT=` (seconds,
14400) bounds the build and the whole run. It is a measurement run by hand or on a quiet
night, not a CI job; a test it fails is fixed in the test or the code,
never retried.

`tools/ci_flakes.py -n 50` is the ledger: the failed jobs of the last 50
completed `ci.yml` runs on main and in the merge queue, each with its failing
step and whether a later commit fixed it (a pull request that passed again
with changed files, a later push touching a source file the failing job's area
owns or a pull request's branch updated; every conclusion but success, skipped
and cancelled counts, a timeout too). A failure that
passed again with the same files is a flake and counts, whatever its cause.
The 1.0 plan's budget is none without a fixing commit.

Which `ci.yml` jobs are required checks is a repository setting (classic
branch protection on main, not the merge queue's ruleset). All of them are,
but `size (browser module)`, which only runs on a pull request and so would
never report in the merge queue. A job given a path filter or an `if:` that
skips it on some events cannot be required for the same reason: a skipped
required check blocks a merge it should not. A new job that is neither
needs adding to the list.

The mobile jobs upload build products nothing downloads
(`fennecdb-android`, `fennecdb-xcframework`) with `continue-on-error`: an
upload that fails on GitHub's side does not fail a job whose tests passed
(#590 and #591 lost merge-queue runs to one). The test-report uploads run
only when a step failed, and no step retries a test.

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
a value read written back as its bytes), `image` (a `.fennec` file opened
read into memory and mapped as a server opens it -- an image's data record
taken from the index it wrote of its frames, `Store::adopt_index`, in shares
on threads past 16 384 documents, any other replayed in place,
`replay_mapped` -- each read whole, its indexes built and its vectors
linked, and its records applied as a replica applies a primary's; the
first byte of an input says whether its records are summed anew, so an
open reads what a record holds rather than stopping at its sum) and
`sqlite` (`fennec import`'s reader over a file). `make fuzz`
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

*A run is its seed's, so a finding is made again from its recipe.* A run
hands its target each input of its start -- the target's seeds and what a
corpus directory holds, in order, once -- then a mutation of one the corpus
holds, the corpus taking each input that reached new coverage (`Inputs`).
Nothing else is read: no clock, and coverage only as which runs the corpus
took, so a seed, the start and the runs kept make the same inputs again, on
another machine and in a build that counts no coverage; the fuzzer's own
edges are taken as seen by a mutator of their own, which draws nothing from
the run's. Whatever ends a run -- a panic, an abort's handler, the
watchdog -- says its recipe (`guard::say_recipe`, with system calls alone):
the target, the run, the input's length and FNV hash, and the command that
makes it again, `fennec-fuzz reproduce <target> --seed N --start HASH
[--corpus DIR] --run N --fnv HASH --kept RUNS`, the start's fingerprint
telling another checkout's seeds apart and the runs kept written as steps
in base 36. `reproduce` checks both hashes, writes the input and runs it as
`replay` does. `tests/all.rs` holds each part: a panic's recipe read back
and made into its input, an abort's from its handler in a child process,
a run's input following the runs kept before it, and every target's start
the same bytes in two processes -- the `image` target's seeds are files
the engine writes, which must hold no clock, path or hash map's order.
Coverage itself is not reproducible -- a hash map's random keys, threads,
the platform's own code -- which is why the corpus's growth is written
down rather than taken again.

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
one, now refused by the decoder, an entry being below its dimension
wherever a write puts one (`codec::tests`).

Its review found more, each fixed in that pull request with its test. A
vector field's `m` and dimension were read from a file as written, so 77
bytes declaring `@hnsw` with an `m` of 2^40 had the open killed asking for
2^41 links a node: a schema read off the bytes is held to the language's
ranges ([crates.md](crates.md#limits), *A file's schema is held to the
same ranges*), a graph record's own `m` too. A frame running past its data
record stopped the replay as at a torn tail, the record's frames after it
dropped with no word: refused, as the apply refuses it
([storage.md](storage.md#file-format)). And the `image` target did not
run the open a server runs, mapped: it does now, where the index an image
writes of its frames took `u64::MAX` as an id, its next id overflowing
(`store::tests`). Ten minutes of `image` with the mapped open under ASan
(95 089 runs) and three of `codec` (181 725) found nothing more. One is
left for a decision: JSON's reader takes a number past a double's range,
`1e400`, as infinity, which its writer writes as `null` -- no crash, and
the `json` target holds its round trip to finite values until that is
decided.

*CI* runs each target for 30 s under ASan on every pull request and push
(ci.yml's `fuzz-smoke`, `make fuzz-smoke`) and for 30 minutes each on
Sundays (`fuzz.yml`). The repository is public, and so are a job's log and
its artifacts for 90 days, while SECURITY.md asks a finding be reported
privately: so a job keeps no input and no corpus, and says no message and
no backtrace (`--private`; ASan's report goes into a file of the runner's,
`ASAN_OPTIONS=log_path`), only the recipe and the commit, from which a
maintainer makes the input again on a machine of their own. The first
version kept a finding's input and backtrace as the job's artifact, and
carried the weekly corpus from week to week in the Actions cache: inputs
no maintainer holds, which no recipe could make again. Each week starts
from the repository's seeds instead, its seed the run's number, so each
week's mutations are its own. Not yet fuzzed, after 1.0: a sealed archive's
and a backup's frames (`seal.rs`), an archive's segments, PostgreSQL's
messages and `pgoutput` (`fennec-wire`), a JWT and a JWKS (`access`), the
sync's messages through the native library and the browser module, and
IndexedDB's chunks; nor is each target held to a count of runs since its
code last changed.

## Secrets compared

A token, a mark, a MAC, a signature or a password is compared by
`fennec_http::crypto::ct_eq` alone, never by `==` or `!=`: those leave at
the first byte that differs, so the time an answer takes says how much of
a guess was right, and a guess can be grown a byte at a time. `ct_eq` ORs
every byte's difference into one accumulator, its lengths compared first
(a length is no secret here), and reads the accumulator through
`core::hint::black_box`: what is asked of it hidden, the optimizer cannot
end the loop at the first difference, as one it took for a comparison it
may (`bcmp`). `black_box` is a best effort, as its documentation says.
The loop compiles to what it was, vectorised, its one branch on the count
(aarch64, opt-level 3), and the accumulator's store and load are what is
added: five instructions a call out of line, two where `authenticate`
inlines it -- one `handle` call under the server's token traced in main's
build and this one (`make instr-compare EXAMPLE=rows MODES=token
TRACE=fennec_http::handle`'s binaries, `trace.py diff`), the program's own
code 15 399 instructions against 15 401, the allocator's and the system's
share apart, which a binary's identity moves. There were
three such loops, the same bytes for bytes -- `crypto::ct_eq`, fennec-http's
`constant_eq` and a copy of it in fennec-shard -- and one is left, which
every bearer token (the server's, the admin's, the replication token, the
router's `--token` and `/_metrics`'), the router's mark on a forwarded
request, the global entry's mark, a hook's signatures, an HS256 JWT's MAC,
the AEAD's tag, RS256's encoded message and SCRAM's server signature go
through. The router compared tokens with `==` in three places, none an
attacker could time -- a node's admin token against `--entry-token` as the
node is added, behind `--token`, and at the start one flag's token against
another's -- and goes through `ct_eq` there too: a rule without exceptions
is one a reviewer need not weigh. SCRAM's server signature is fresh each
exchange, so no guess grows by it; it goes through `ct_eq` for the same
reason.

`tools/secret_eq_check.py`, in CI's fmt job, holds it. It reads every
`.rs` file under `crates/*/src`, comments blanked, strings and characters
made literals of their length, and what each `#[cfg(test)]` is on left
out -- an item, a statement, a field, a variant, a match arm or an
argument, to its `;`, its `,` (an item's generics and `where` clause hold
`,`s of their own), its block's `}` or the bracket closing around it, and
all a `#![cfg(test)]` is inside -- the cfg read as Rust reads it,
`all(feature = "x", test)` a test's and `any(test, unix)` not. Each `==`
and `!=` is read with both operands whole, as Rust binds them: paths,
fields, calls, indexes, `?`, `::<..>`, macros, prefixes and the operators
binding tighter (`+`, `^`, `as`), a closure's parameters ending one. It is
refused where an operand names a value whose name holds `token`,
`secret`, `mark`, `signature` or `password`, or has `sig`, `mac`, `hmac` or
`proof` (or their plural) as a whole part between its `_`s -- inside a
word they are `signal`, `design` and `machine` -- a static's name among
them (`ADMIN_TOKEN.get()`). A type or a variant names no value, and a part
of an operand ending in `.len()`, `.is_empty()`, `.is_some()` or
`.is_none()` no secret's bytes; a comparison one side of which is literals
and constants alone is held to a value the binary carries, never to a
guess -- but a static read through a method is no constant, and an
operand the reader cannot read whole is never public. What is left and is
no secret -- a select list's marks, a schedule's time mark, the router
pool's tenant marks (a keyed hash of a name), a maintenance's tail number,
a snippet's window of tokens -- is in its `ALLOWED`, by file and the
comparison as written, with why and how many times the file has it: one
more of the same text is a comparison nobody weighed, refused, and one
fewer a stale pass, refused too. `--self-test` holds the reader to its own
cases, every shape below among them. It goes by names: a tripwire, never a
proof, and a secret under another name (`got`, `want`) is its reviewer's
to see.

The check's first form had blind spots its review found. It ended a test's
item at a `;` or a block alone, so a `#[cfg(test)]` field ran on past its
struct's `}` and blanked the code after it -- fennec-core's
`fs::windows::map::calls()`, `fs.rs`'s `thread_local!` and the end of
`FileSink::over`, fennec-http's scheduler `Queue` -- and read an operand
it could not parse (`x.parse::<u32>()? == token`, `S { .. } == token`) as
public; it took a static's SCREAMING name for a constant's, matched no
`sig`, `mac`, `hmac` or `proof`, and let a second comparison through by the
text of a first. Read now, none of that code compares a secret, and of the
crates' comparisons only the snippet's, read across its `+` as
`at + tokens == n`, reads otherwise. Reading operands whole costs the scan
its tokens: 2.6 s of a core over the crates' 8.4 MB where it was 2.2, a
comment or a literal with no newline blanked by a multiplication rather
than a substitution.

A primitive we write is held to its RFC's vectors, every one, each test's
count of cases held so none is skipped unseen: where the test writes its
cases, by the length its array's type states (a counter over such an
array could not fail, and none is kept); where a file holds them, by the
lines it read, each kind counted. SHA-256 is held to FIPS 180-4's
examples; HMAC-SHA-256 to RFC 4231's seven cases (`hmac_rfc4231_all`;
`HmacKey` with its message in parts too); PBKDF2-HMAC-SHA-256 to RFC 7914's
c = 1 and c = 80 000 (`pbkdf2_sha256_vectors`, the first 32 bytes of the 64
it asks: its first block, which no length asked after it changes); both,
where a key meets SHA-256's 64-byte block, which no RFC vector does, to
Python's standard library's -- HMAC keys and PBKDF2 passwords of 0, 63, 64
and 65 bytes, and salts of 60 to 63 whose INT(1) ends the inner hash's
second block or crosses into its third
(`hmac_and_pbkdf2_where_a_key_meets_the_block`, `hmac_pbkdf2_vectors.txt`,
the command printing every line in its header, the 64-byte key's HMAC
checked against OpenSSL's too): a key of 64 bytes is the padded block as it
is, and hashing it -- `>=` for the `>` in `HmacKey::new` -- passed every
RFC vector and fails this test alone; ChaCha20, Poly1305 and the
AEAD to RFC 8439's appendix A whole -- A.1's five block functions, A.2's
three encryptions, A.3's eleven Poly1305 tags with #5 to #11 the carries
and reductions 26-bit limbs get wrong, A.4's three one-time keys and A.5's
decryption (`chacha20_rfc8439_appendix_a`, `poly1305_rfc8439_appendix_a`,
`aead_rfc8439_appendix_a`, read from `rfc8439_vectors.txt`, each hex dump
of the RFC's text read as bytes and held to its own ASCII column as it was
made); ES256 to Wycheproof's (`tests/es256.rs`); and SCRAM's client half,
fennec-wire's, to RFC 7677's exchange byte for byte, its nonce a parameter
of `Scram` so the RFC's can be given (`scram_rfc7677_exchange`). Each
vector is read from its RFC's text or computed by another implementation,
never written from memory, and every one passed as first run: no primitive
was wrong. RFC 4231's case 6 had
stood in the old test as "case 3". Wycheproof's ChaCha20-Poly1305, RSA
PKCS#1 v1.5 SHA-256 (2048, 3072 and 4096 bits, its e = 3 forgeries among
them), HMAC-SHA-256 and PBKDF2-HMAC-SHA-256 files are to follow, vendored
with a NOTICE as `tests/es256/` has its own.
