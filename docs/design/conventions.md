# Conventions

The long form of the test and docs conventions [CLAUDE.md](../../CLAUDE.md) lists under *Conventions*: where tests live and which run alone, a test that fails on one run, a browser's launch and a page's reload, the clock, ports, what a change a user meets owes the docs, the fuzzing of every parser of untrusted bytes, why each `unsafe` block is sound and the sanitized tests that hold it, and how a secret is compared and a primitive held to its vectors. They hold no invariant of the code, so [README.md](README.md#the-original-order) does not number them. Each section below is a bullet moved word for word from CLAUDE.md, which keeps its rule in a sentence or two; those on test binaries, fuzzing, unsafe code and secrets are now kept by [crates/CLAUDE.md](../../crates/CLAUDE.md).

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
not to CI -- but for the perf gate's workloads
(`crates/fennec-http/examples/gate.rs`), which CI counts in instructions on
every pull request ([benchmarks.md](benchmarks.md#the-gate)).
An example that is a check rather than a measurement runs in
CI: fennec-ql's `corpus`, which `make fennecql-corpus` builds in this tree
and in main's from an archive of it, run by the test job's two steps on
the ubuntu runner -- against main, and against every corpus frozen at a
tag (`integrations/fennecql-corpus/`) -- its reader held by
`tools/test_fennecql_corpus.py` in the fmt job and in the test job's
ubuntu leg, before the steps ([fennecql.md](fennecql.md#the-corpus)). It is an example, not a test,
because it is built in two trees, BASE's taking this tree's file as it
stands; its own tests (`test = true`), which only this tree builds, run
with fennec-ql's.

## Skips

A suite that skips itself when `web/fennec.wasm`, `web/fennec-lite.wasm`,
`web/fennec-min.wasm` or a binary cargo builds is missing is right on a
developer's machine and wrong on CI: about 120 JS tests in ten suites once
skipped there, and a job that built nothing reported green. Under `CI` a
missing prerequisite throws (`web/ci-prereq.mjs`'s `needs`, called after the
suite finds what it needs, as `studio/test/transport.test.mjs` already did),
and a Rust test that skips for a missing program (`fixture!` in
`fennec-import/tests/sqlite.rs`, the inflater in `fennec-http/src/gzip.rs`)
panics. What `make test` cannot run -- the tests that need a live PostgreSQL, which
CI's `postgres (import and --follow)` job runs and fails unless every one
passed (`replication.md`), and the exhaustive float sweeps, run by hand -- is counted: `make test` copies node's spec
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
with changed files, a later push or scheduled run touching a source file the
failing job's area owns or a pull request's branch updated; every conclusion
but success, skipped and cancelled counts, a timeout too). A failure that
passed again with the same files is a flake and counts, whatever its cause.
The 1.0 plan's budget is none without a fixing commit.

**`ci.yml` runs on a pull request, in the merge queue, twice a day on main
and on a push to main that moved a cache's key.** main moves only through the
merge queue, and a merge group's head is the commit main then holds, so a run
on every push tested it a second time (14 landings in 14 hours on 2026-11-14,
three runs of about 28 jobs each, merge groups dropped at the queue's
60-minute timeout behind the backlog; the account runs 20 jobs, 5 on macOS).
A push to main starts the workflow only when an input of a cache changed, or
`ci.yml`: a run on main is the only one whose caches other branches' runs
restore (GitHub's caches are read from the run's own ref or the default
branch's; a pull request's is its merge ref's, a merge group's its queue
branch's). The caches and their inputs, each in `paths`: the 21 cargo `actions/cache`
steps by `hashFiles('**/Cargo.lock', 'rust-toolchain.toml')` (the Unicode
table's by its pinned version in the file), `sanitize`'s by the nightly the
Makefile names (`FUZZ_NIGHTLY`) as well, so no stable build's entry is
restored under it -- the Makefile being no path, a new nightly's entry is
saved by main's next scheduled run; `setup-node`'s npm cache by the
Grafana plugin's `package-lock.json`; `setup-go`'s by its `go.sum`, and
`go.mod` for the Go version; the Flutter caches by the version pinned in
`ci.yml`; `setup-gradle`'s key is the job and the commit, not a file's hash,
and only main writes it, so the Gradle build files (`**/*.gradle`,
`**/*.gradle.kts`, `**/gradle.properties`) are listed for what it holds, not
for its key (no `gradle-wrapper.properties` or `libs.versions.toml` exists
today: add either to `paths` when one does). The merge group's run is a
commit's whole run; its jobs finish after it merges as long as nothing cancels
them (observed on #651 and #652: they finished minutes after the merge). The `schedule` (04:17 and 16:17 UTC)
is main's periodic whole run: the badge, runner and base images that drift,
the ledger's view of main, and the backstop for an evicted cache (unused for 7
days, the 10 GB limit). Jobs gated on the event: `test (macos x86_64,
Rosetta)` runs on everything but a pull request, so on the schedule, and `size
(browser module)` only on a pull request.

**Every cache is restored by every run and saved by a run on main alone, and
what main saves is small.** A cache saved on a pull request's ref is read by
no other run, so each pull request that missed a key saved a private copy and
pushed main's entries out of the 10 GB: at 01:15Z on 2026-11-15 the repository
held 49 entries, 11.31 GiB (pull-request refs 35 entries, 7.18 GiB; main 12,
3.36 GiB; merge groups 0.24 GiB), and main's Gradle entry was gone within
about 1.5 hours, so 6 of the Android job's 50 runs restored it. Now the 21
cargo steps and the Unicode table are `actions/cache/restore` with a
`restore-keys` fallback (a pull request that moves `Cargo.lock` restores the
last entry) and an `actions/cache/save` step at the job's end under
`github.ref == 'refs/heads/main'`, keyed by the restore step's
`cache-primary-key` and skipped on an exact hit. `setup-node`'s and
`setup-go`'s `cache` and `subosito/flutter-action`'s save from any ref and
have no read-only switch, so the Grafana job's npm (`~/.npm`) and Go
(`~/go/pkg/mod`, `~/.cache/go-build`) caches are the same restore and save
pair over the paths the actions used, and the Flutter caches are gone.
`setup-gradle` writes only on main by its own default (`cache-read-only`).
`desktop.yml`'s Cargo and Hutch caches and `site.yml`'s are the same pair
(`site-lighthouse.yml`, which runs on pull requests alone, restores only).
`desktop.yml` and `site.yml` run on main by their own paths filters, the
caches' inputs among them, and `desktop.yml` gained a schedule, Monday and
Thursday at 05:41 UTC, so that an entry evicted or unused for 7 days is saved
again. The cargo keys moved to a `v2-` prefix so that no run restores the
unpruned entries through `restore-keys`.

*What main keeps of `target/`.* A checkout's files carry new times, so cargo
builds the workspace's crates again whatever `target/` holds; what a restored
`target/` saves is the crates from outside the workspace, and the workspace's
test binaries and debug info were most of every entry.
`tools/ci_prune_target.sh` runs on main just before a save and keeps only the
units whose dep-info (`deps/*.d`) names a file under the registry or a git
checkout, with their build scripts and fingerprints. Measured on a local build
of the fennec-server and fennec-import tests: 822 MiB to 59 MiB, and the next
build compiled only the workspace's own crates, none of rustls, ring or libc
(mtimes are kept, as the cache's tar keeps them; a copy that does not keep
them makes cargo rebuild everything). An entry is now 0 to 47 MiB, but for the
clippy job's 373 MiB (it checks the Windows target too), against 15 to 801 MiB
before (`ubuntu-latest-cargo` 801, `ubuntu-24.04-arm` 643, `macos-latest` 522,
`clippy` 440 MiB). Four of the 21 are 0 MiB because fennec-core and the C
library have no crate from outside the workspace: they were 15 to 31 MiB of
workspace objects. Nor did the cache save time in those jobs: a hit and a
miss of `windows (http)` took 242 and 241 s, of `test (ubuntu-latest)` 962 and
940 s (runs 43648331326, 43644153745, 43638860592), because the workspace is
what takes the time.

*Flutter, Go and the rest.* The SDK's entry was 2.0 GiB on macOS and 1.6 GiB
on Linux, and restoring it saved 28 s (35 s against 63 s) on macOS and 16 s
(13 s against 29 s) on Linux of a download (run 43663494522), under 3% of
either job; it is not cached, nor is the pub cache (25 to 30 MiB). The Go
cache is 436 MiB, the npm one 93 MiB, `desktop.yml`'s Hutch caches 68 to 98
MiB a system (435 MiB in all) and Gradle's 2.1 GiB (setup-gradle's entries:
dependencies 1090, transforms 656, wrapper zips 179, the distribution 130
MiB, measured with `cache-read-only: false` on the job as it was before the
wrapper above: the Gradle User Home it keeps now is of that order, not
measured).

*The set main will hold*, measured on a test branch that saved as main does
(run 43665618926): about 1.0 GiB of cargo entries, 0.44 GiB Go, 0.09 GiB npm,
0.43 GiB Hutch and 2.1 GiB Gradle, about 4.1 GiB in some 40 entries, against
about 11 GiB projected for the same keys unpruned with the Flutter SDKs. A key
change leaves the old entries until they age out, so for a while the
repository holds both, up to 7.4 GiB with main's 3.36 GiB: under the 10 GB.

Which `ci.yml` jobs are required checks is a repository setting (classic
branch protection on main, not the merge queue's ruleset). All of them are
but `size (browser module)`, which only runs on a pull request and so would
never report in the merge queue, and `sanitize` ([below](#sanitizers)),
whose verdict rests on a dated nightly and a sanitizer's runtime, which a
runner image can break with no change of ours -- a kernel laying out more of
the address space at random than the sanitizer's shadow memory leaves room
for has done so to ASan before -- and a merge queue held by that holds every
change (a red `sanitize` is read and fixed in a change of its own, and a
release waits for a green one: RELEASING.md); and a job is left out until it
has passed on main: `perf (instructions)` until the gate's holdout has
([benchmarks.md](benchmarks.md#the-gate)), a new job until the merge group
that brought it has. A job skipped by an `if:` reports success
and can be required (the Rosetta job is one, skipped on a pull request); a
whole workflow skipped by a path or branch filter reports nothing, its checks
stay pending, and a required check of one blocks a merge it should not, so
`ci.yml`'s `pull_request` and `merge_group` triggers take no filter (its
`push` has a path filter, which no required check is read from). GitHub's
"Troubleshooting required status checks" says both. `size (browser module)`
is left out because a required check that never reports in the merge queue
holds every group. The list is kept by hand, the setting being no file: a
new job is added once its first group is green, and a required job that is
renamed is taken out before the pull request renaming it is queued and
added under its new name once that has landed, since a required check no
job reports holds every group too, the old name in the renaming group and
the new one in every other. With every job
required, a merge group lands when its last job ends and a red job drops
it, a flaky one's too, so the queue builds two groups at a time and waits
180 minutes for a group's checks (the ruleset's `max_entries_to_build` and
`check_response_timeout_minutes`): the account runs 20 jobs at once, a run
is about 35, and a group timed out at 60 behind other pull requests' runs
(#605 and #645 on 2026-11-14).

The mobile jobs upload build products nothing downloads
(`fennecdb-android`, `fennecdb-xcframework`) with `continue-on-error`: an
upload that fails on GitHub's side does not fail a job whose tests passed
(#590 and #591 lost merge-queue runs to one). The test-report uploads run
only when a step failed, and no step retries a test.

The Android job's Gradle takes what it can from setup-gradle's cache of the
Gradle User Home, which only main's runs write: the plugins and
dependencies, and the distributions its wrappers unpacked -- 8.10.2 for the
Kotlin builds, through a wrapper the job writes with the runner's own Gradle
(offline, the download held to Gradle's published sum), and 9.5.0 for the
app `flutter create` makes. setup-gradle's `gradle-version: 8.10.2` had kept
that distribution under a key of its own beside the entry, and asked
services.gradle.org for its list of versions every run: of the job's 50
runs on 2026-11-14, the 6 that restored main's Gradle User Home each
downloaded 8.10.2 again. Its six failures on a fetch from 2026-11-08 to
11-11 -- the Kotlin plugin from the plugin portal, and five times Gradle
9.3.1 for the Flutter app from GitHub's releases, a 504 -- were each in a
run that restored nothing, which is most runs: an entry written for a pull
request or a merge group is read by no other run, and main's was gone within
about an hour and a half, so each of main's four runs that day began
empty. The repository's caches then stood at 10.3 GB against GitHub's 10,
7 GB of it in two merge groups' entries, 3.6 GB of that the Flutter SDK for
Linux and for macOS, which both mobile jobs cache on every ref that misses.
A step after the builds, `What Gradle fetched`, names each distribution
unpacked and each plugin fetched, and counts the artifacts written after the
restore, so a run's log says whether the cache served it.

## Browsers

A step that launches a browser reads its own files first, or a cold
runner's disk spends 5 to 20 s of the launcher's wait (`waiting for
dynamic debugging port`): the studio's harness (`warm`),
`examples/{shop,analytics}/scripts/warm-chrome.mjs` and
`web/browser/run.mjs` (Chrome's and Firefox's, `warm`) do; a longer wait
only moves the bound.

A test of what a page keeps across a reload (`web/browser/`) has the page
run its suite itself, its state kept between loads by the server that
opened it: a reload is the page's own `location.reload()`, and the one
suite runs under puppeteer and under safaridriver alike. It holds what it
reloads in the middle of until the page is gone -- a persist's transaction
by a read after a read, a worker's flush by keeping the worker busy, which
every engine ends with its page (one waiting on `Atomics.wait` Safari did
not) -- never catching it with a timer. A test a
browser cannot run is skipped by name in `run.mjs`'s `SKIPS`, with why --
Safari's quota tests, no driver setting its quota -- and any other skip
fails the run, as does a listed one that ran.

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
database's CORS names it -- or a program that takes its port on its
command line and names none it chose -- safaridriver, whose `-p 0` is a
default port of its own -- takes a port the system picks, held by a
stand-in listener until the moment it starts (`examples/shop`,
`examples/dispatch`'s `ci.sh`, `web/browser/run.mjs`).

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
for a hang) is written the same way by the handler of a timer the run
sets (`setitimer`, `SIGALRM`) -- a watchdog thread beside the runs ran
code the coverage counts, below -- an allocation past
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
timer's -- says its recipe (`guard::say_recipe`, with system calls alone):
the target, the run, the input's length and FNV hash, and the command that
makes it again, `fennec-fuzz reproduce <target> --seed N --start HASH
[--corpus DIR] --run N --fnv HASH --kept RUNS`, the start's fingerprint
telling another checkout's seeds apart and the runs kept written as steps
in base 36. `reproduce` checks both hashes, writes the input and runs it as
`replay` does. `tests/all.rs` holds each part: a panic's recipe read back
and made into its input, an abort's from its handler in a child process,
a run's input following the runs kept before it, and every target's start
the same bytes in two processes -- the `image` target's seeds are files
the engine writes, which must hold no clock, path or hash map's order --
and a run past its timeout, from the timer's handler.

*What a run reaches is the same in every process of a platform*, so a
count of runs from a seed keeps the same inputs wherever it runs, and
`make fuzz-smoke` -- each target for a count of its own (`Target::smoke`,
`--smoke`), from the repository's seeds alone (`--corpus none`) -- ends
each target on the same line on every runner: the runs, the seed, the
corpus's size and fingerprint, the edges (the time on the line before).
Four things made one process keep an input another did not, found by
running a target twice for the same count and comparing those lines --
`codec` kept 597 inputs in one process and 601 in the next over 20 000
runs, `image` 1 052 and 1 040 over 15 000, its seeds alone reaching 13 298
edges in one and 13 301 in the other. std's hash maps were keyed at random
in each process, and a map's probes are code the counters count: the
binary takes std's call for keys (`CCRandomGenerateBytes` on Apple's
systems; `getrandom` on Linux, which std calls through its `weak!` macro,
a symbol of `extern_weak` linkage that the static link binds to the
binary's own) and hands back the same bytes each time, and a run
says where it could not (`guard::KEYS_FIXED`); with keys drawn at random
`codec` stayed the same and `image` did not. The engine's tasks cut in
shares over threads -- an image's index of its frames, a graph's lists,
its copies table, a batch's links -- bumped the same counters unlocked,
losing counts as the scheduler had it: the fuzzer holds them to the
thread that asked (`fennec_core::one_thread`, each share the same work
either way, [hnsw.md](hnsw.md#parallel-linking)), which `image` needed
with the keys fixed. A watchdog thread woke beside the runs, its code
counted in whichever run was in flight: a timer now. And progress was said
every five seconds, a clock's comparison counted in the next run: where a
count bounds the run it is said by the count. The platform's own code
differs -- a commit's smoke is the same on every Linux runner and on
every Mac, not on both -- and the counts were set from CI's runner, about
30 s each under ASan. Nor does a target read the wall clock: the `image`
target's databases read a fixed time as now (`set_clock`), where a
`@ttl` collection's read asked the system's for its cutoff, a row alive
in one run and expired in a later one. A count still needs a bound in
time, or a commit slowing every run ends at the job's timeout naming no
target: `--smoke` caps each target at six minutes of CPU time
(`SMOKE_CAP`, twelve times its 30 s on CI and three times the most a Mac
under ASan spent, `http`'s 110 s; six targets just under it end inside
the job's 45 minutes with the build), by a timer of the process's CPU
time (`ITIMER_PROF`) set once, whose handler says the target and the run
it reached and exits 4. A bound, never a pass; CPU time, which a machine
others load lengthens less than the clock (`http` 375 s by the clock and
110 s of CPU at a load of 70 to 200, 150 s and 104 s near 15). Two tests
hold it: a child past a cap of 300 ms ends so, and `--smoke` -- the flag
`make fuzz-smoke` passes, read by `run_options` in the library rather
than the binary -- gives each target its count and arms the timer, read
off it by a run (`guard::cap_left`). The timers are stopped as `fuzz`
returns or unwinds (a guard's drop): a fuzzer whose own thread panicked
skipped the line after the join that stopped the cap, and a test binary
went on under it.

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
(95 089 runs) and three of `codec` (181 725) found nothing more. One was
left for a decision: JSON's reader took a number past a double's range,
`1e400`, as infinity, which its writer writes as `null` -- no crash, and
the `json` target held its round trip to finite values until it was
decided. It is refused now, naming it
([vector-search.md](vector-search.md#numbers-in)), and the round trip
holds every value read.

CI's smoke was red from the day the fuzzer landed, red or green by its
runner's speed as 30 s reached a finding or not: three runs on two
commits failed, a fourth passed. It had found a point 64 lists down read
as a value 65 levels deep (`codec`,
[crates.md](crates.md#how-deep-a-value-nests)), and a record naming
collection `u32::MAX`, whose `cid + 1` overflowed in the open and in a
replica's apply (`image`, `tests/damage.rs`): a record's collection is now
one a create gives, below the last `u32`, or the record is damage --
one past `u32` among them, which `as u32` had read as another
collection's -- and a create in a file that gave the last id is refused.
Its review found the first version of that refusal where a record's head
is read (`record_at`), past the walk's judgment of a head: stale bytes
past the durable point reading as one (`0a ff ff ff ff 0f 05 hello`, and
checked as kind 3) refused the open a version before cut. The walk judges
the id where it judges a head (`whole_record`, `head_at`), so such a head
is damage, cut past the durable point and refused before it
(`persist::stale_bytes_past_the_durable_point_are_judged_as_damage`); and
the other refusals the fix added -- a point's level, a stored vector's
count -- an open meets only in a record its walk kept, a torn one cut
before a byte of its documents is read
(`persist::a_torn_tail_is_cut_before_its_documents_are_read`), a sparse
vector's numbers in no open at all.
Making the smoke reproducible found one more, a stored vector's count
multiplied unchecked where an open restoring a graph reads the
documents' vectors (`store::tests`), and a sparse vector's dimension went
the way of the collection's id (`codec::tests`). Each finding's input is a
seed of its target since (`crates/fennec-fuzz/seeds/<target>/`), every run
handing it over again. Ten minutes each of `codec` and `image` under ASan
and three of every other target, a round a seed, found nothing more: two
rounds with the first two fixed (`codec` 1 807 846 and 1 143 649 runs,
`image` 641 173 and 347 255), then with every fix, going on from their
corpus (`codec` 1 408 861, `image` 342 251) and from the seeds alone
(542 458 and 134 768, on a machine others held at a load of 20 to 100).
Two runs of `make fuzz-smoke` in a row, one at a load near 100 and one
near 20, ended each target on the same line.

*CI* runs each target for its count of runs under ASan on every pull
request and push (ci.yml's `fuzz-smoke`, `make fuzz-smoke`), about 30 s
each on its runner, and for 30 minutes each on Sundays (`fuzz.yml`), by
the clock and from a seed of each week's own (the run's number), to look
where the smoke does not. The repository is public, and so are a job's
log and its artifacts for 90 days, while SECURITY.md asks a finding be
reported privately. The smoke's inputs are its commit's, which anyone
makes again with `make fuzz-smoke`: it says a panic's message and where
it was raised, and ASan's report, and keeps no input and no corpus. The
weekly run's are its week's: it keeps no input and no corpus, and says no
message and no backtrace (`--private`; ASan's report goes into a file of
the runner's, `ASAN_OPTIONS=log_path`), only the recipe and the commit,
from which a maintainer makes the input again on a machine of their own.
The first version kept a finding's input and backtrace as the job's
artifact, and carried the weekly corpus from week to week in the Actions
cache: inputs no maintainer holds, which no recipe could make again. And
it gave the smoke 30 s, so its inputs were its runner's. Each week starts
from the repository's seeds instead, its seed the run's number, so each
week's mutations are its own. Not yet fuzzed, after 1.0: a sealed archive's
and a backup's frames (`seal.rs`), an archive's segments, PostgreSQL's
messages and `pgoutput` (`fennec-wire`), a JWT and a JWKS (`access`), the
sync's messages through the native library and the browser module, and
IndexedDB's chunks; nor is each target held to a count of runs since its
code last changed.

## Unsafe code

Every `unsafe` block and `unsafe impl` says why it is sound, in a `//
SAFETY:` comment on the lines above it or above the statement holding it
(attributes may stand between): what the caller or the code around it
guarantees, by name, that makes each access in bounds, aligned and alive
for as long as it is used. For the C ABIs that is the contract written for
the caller -- fennec.h's *Pointers and lengths*, each function's `# Safety`
section, `fennec-wasm`'s memory contract -- for a mapping the file's
length and the lock that keeps another process from cutting it, for a
system call the layout of the struct it writes. A comment that restates
the operation, or says "the caller guarantees" without saying where that
is written, is not one.

Two checks hold it, each where the other cannot reach. Clippy's
`undocumented_unsafe_blocks` is denied in the root Cargo.toml's
`[workspace.lints.clippy]`, which every crate takes by `[lints] workspace
= true` -- `tools/lints_check.py` fails a member of the workspace that does
not, a new crate too -- so every clippy run CI makes refuses a site without
one in the code it compiles: the workspace's, wasm32's and each feature
set's, part of Windows', the JNI functions' and `test-clock`'s. That is
x86_64 Linux, wasm32 and Windows' check. The scanner below counts 362
`unsafe` blocks and impls in the repository's 478 `.rs` files (2026-11-15,
after the 79 blocks `unsafe_op_in_unsafe_fn` added and the merges after
them), and a review found about 48 of them, when they were 255, compiled
in none of those runs -- aarch64's NEON kernels, prefetch and CRC
instructions, Apple's system calls (the page cache's `F_NOCACHE`,
`vm_statistics64`, `rusage_info_v4`, the lease's clock), the macOS-only
examples, Windows' TLS and tests. So `tools/lints_check.py`, in CI's fmt
job, also reads every `.rs` file the repository holds, whatever its `cfg`,
comments, strings and characters masked, and refuses an `unsafe` block or
`unsafe impl` with no comment saying `SAFETY:` where clippy reads one: on
the lines just above it, blank lines skipped, or above the statement it is
-- itself, or a `let`, `static` or `const` whose value it is -- attributes
included, and never above the `fn` of a body the block opens the line of.
It is a reader of text, never a compiler, and so reads only where the
comment is, never whether it is true. Where it and clippy differ, its
header says, each case run through both: it is stricter only for a block
deeper in a statement on a later line -- an argument of a call, a term of
a sum -- whose comment above the statement clippy takes and it does not.
`tools/test_lints_check.py` holds it to a case of each kind it refuses and
each place it reads a comment from, and, its last case, to the
repository's own files, which is how CI's fmt job runs it.

What neither check sees are the forms that are unsafe with no `unsafe`
written, which edition 2021 allows. A plain `extern` block -- `extern "C"
{ ... }`, Windows' `extern "system"`, 73 of them in 48 files (2026-11-21):
`mmap`, `signal`, `pthread_kill`, `proc_pid_rusage`, `CreateFileMappingW`
-- declares what the linker will bind by name alone, and a declaration
whose signature is not the function's is undefined behaviour at its first
call. `#[no_mangle]` -- 50 of them, the C ABIs' exports -- names a symbol
another object of the program may define too. Neither asks a reason: clippy's lint is about blocks, and
the scanner reads the keyword, which these do not carry. rustc's
`missing_unsafe_on_extern` and `unsafe_attr_outside_unsafe` (edition 2024's
rules, allowed by default in 2021) would have each written `unsafe extern
"C"` and `#[unsafe(no_mangle)]`, which the scanner would then hold to a
`SAFETY:` comment as it holds a block. Denying both in
`[workspace.lints.rust]`, and the scanner reading those forms, is the next
step; no lint's level changed with this paragraph. The scanner reads no
`unsafe extern` block, `unsafe trait` or `#[unsafe(..)]` either, of which
the tree has none; a macro's body it reads as text, so a block in one is
held like any other (`jni.rs`'s), and the repository uses no outside
macro. An unsafe operation in an `unsafe fn` is rustc's to refuse outside
a block, on every target CI compiles -- x86_64 Linux, wasm32, arm64 macOS,
x86_64 and arm64 Windows -- which leaves the Unix that is neither Linux nor
Apple's, whose fallback (`lock.rs`) holds no `unsafe fn`.

An unsafe operation in an `unsafe fn` goes in a block of its own, which
says why it is sound like any other: rustc's `unsafe_op_in_unsafe_fn`,
denied in `[workspace.lints.rust]`, is a lint of the compiler, not of
clippy, so every build for every target refuses one outside a block --
the release's for macOS, Linux on ARM and Windows, the mobile jobs', a
developer's -- and the scanner holds that block's comment whatever builds
it. Turned on on 2026-11-15, it moved the C ABIs' raw reads and writes --
fennec-wasm's exports, fennec-ffi's and every JNI function -- into blocks,
79 of them, each with its comment. `fennec-server`'s `__TEXT` and the
browser module's code section stayed byte for byte what they were; the
native library's code kept its size and its instructions, but for the
registers two functions take -- `fennec_schema` and `fennec_sync_feed`,
whose answer closures are bound before their `call` now, which a block
around a closure passed inline would cover whole -- one value kept on the
stack there rather than in a register, a load a call.

When the lint was turned on (2026-11-15) the crates held 236 `unsafe`
blocks and impls, 127 of them commented -- most of the rest in tests,
examples and fennec-ffi's callers of its own ABI -- and writing the reasons
down found three that had none. `fennec_alloc` made a `Layout` of any
length the page asked for, unchecked: past `isize::MAX`, 2 GiB on wasm32,
that is undefined behaviour, and `fennec_load_owned` then took the block as
a `Vec`; it answers a null now. `boxed` copied an answer through the null a
failed allocation answers, address 0 in a module, the bottom of its stack:
it aborts now, and the page's `#write` throws on a null rather than write
there. And `fennec_schema` read its text as UTF-8 before it looked at the
mode, where fennec.h says `FENNEC_SCHEMA_DESCRIBE` leaves it unread: it
answers the description first now. Each has its test, the first and last
aborting on the standard library's precondition checks before the fix.

A mapping's bytes are a `&[u8]` (`fs::Mapping::as_ref`), so two things
must hold of them while a slice lives: no byte of it is on a page past the
file's end, which a read ends the process at (`SIGBUS`), and no byte of it
changes. A database's mapping covers the file's length when it is made and
grows only over bytes written (`FileSink::written_through`). The one cut, a
torn tail at the open, takes the bytes past it out of the mapping before
the file is cut (`FileSink::cut`, `a_cut_takes_the_bytes_past_it_out_of_the_mapping`):
until 2026-11-15 it left them covered, a slice of the mapping spanning
pages past the file's end until the first handover, and the first appends
after the cut landed in its bytes in place. No read reached them -- no
record points past the cut, the cut runs before the open returns the
database, and the appends were read from memory until a handover covered
them anew -- but no slice may span them either. Appends land past what the
mapping covers, and a rewrite renames a new file over the old (root
CLAUDE.md, *A mapped file is replaced only by rename*). The one write in
place is a compact's beside the database: the counter's head patched into
its side file under the write lock (`finish_beside`), after `Beside::write`
mapped that file and pointed the compact's stores at it. Those stores are
the compact's alone until the swap, which comes after the patch, all on
the one thread that runs it, and no slice of the mapping outlives the read
that took it (the touch of every page ends before), so no read meets those
bytes as they change. Another process writing the file is what the
`<file>.lock` every opener of a file to write takes keeps out (root
CLAUDE.md, *Single writer*); a process ignoring it can end a reader with
`SIGBUS`. A reader that takes no lock (`open_read_only`: `fennec types`,
`--read-only`) maps the file as it stood at its open, and a server opening
it after a crash cuts a torn tail under it. So the reader's open covers its
mapping only to the records it took (`Mapping::cover` once the load
returns, `a_reader_is_covered_to_the_records_its_open_took`), and no slice
it takes after its open spans a byte past the cut. Until 2026-11-15 it kept
the length the file had at its open, every slice spanning the pages past
the file's new end once a server had cut it -- no record pointed there, so
no read reached them, but no slice may span them. What is left is the
open's own walk of the file, which reads to the length the file had: a
server's open cutting it in the middle of that walk ends the reader. On
Windows the reader's view refuses the cut (`ERROR_USER_MAPPED_FILE`, the
server's open failing by name: [windows.md](windows.md)); its mapping is
views laid end to end, which only grow (`fs::windows::map`), and its own
cut drops the database and its views first (`cut_and_open`).

## Sanitizers

The crates whose `unsafe` code an app's or a page's pointers reach run
their tests under AddressSanitizer: `make sanitize`, CI's `sanitize` job
on x86_64-unknown-linux-gnu, is `cargo test -p fennec-ffi -p fennec-abi -p
fennec-core --lib --tests` built with `-Zsanitizer=address` on the
Makefile's `FUZZ_NIGHTLY`, the fuzzer's dated nightly, for the host's
triple, so build scripts and proc macros run uninstrumented (the nightly is
only here, in `fuzz-smoke` and in `fuzz.yml`), in the `sanitize` profile:
the test profile's checks -- debug assertions, overflow checks, the
standard library's preconditions -- at opt-level 1, as ASan's own
documentation advises. A report -- a read or write past a block, after its
free, a double free -- ends that test binary, failed, the others run on
(`--no-fail-fast`) and the run fails at its end. Three tests are skipped
(`SANITIZE_SKIP`): those held to the standard library's own Unicode, which
a nightly moves ahead of the pinned release's -- they fail on a new
Unicode by design, for the case tables to be written again (`case.rs`,
`tests/nocase.rs`), and say nothing of memory.
`fennec-ffi`'s `tests/bounds.rs` calls each function of the C ABI at its
edges -- a length of 0, a NULL with the largest length a `size_t` holds,
a buffer exactly as long as it says, a heap block of that length alone or
one whose bytes past the length change the answer if read, a vector
claiming more `f32`s than its block holds, a place past the parameters, a
handle no database holds, every mode, kind and status past the last -- so
a read past a block is a report rather than a wrong answer.

Every unsafe site of the three crates that the job's target compiles is
reached by a test it runs, checked by a probe never committed: each site
noted its number in a file the first time a process reached it, over the
same tests on macOS (`make sanitize`'s run, ASan or not, reaches the
same paths), and each Linux-only site by its macOS twin's path -- the
`posix_fadvise` a compact past the page cache asks where macOS asks
`F_NOCACHE`, the `fcntl` lock's struct, the CRC's SSE 4.2 path where macOS
takes the ARM one. What the job does not reach: Windows' sites (the
mapping in views, the lock's kernel32 calls), the browser module's
(`vector::simd` is safe Rust; `Rows::row` and fennec-wasm's exports are
wasm32's, which has no ASan, and run natively only in fennec-wasm's own
tests), the `flock` lock of a Unix that is neither Linux nor Apple's, and
the JNI functions, which need a JVM; aarch64's NEON kernels, prefetch and
CRC instructions run under it on Apple silicon (`make sanitize`), not in
CI's x86_64 leg.

Leaks are not looked for (`detect_leaks=0`): a leak is no undefined
behaviour, the engine's tests forget a database on purpose as a killed
process leaves one (`fs::tests`), and LeakSanitizer does not run on Apple
silicon, where the rest of the job is run again by hand. Nor does the job
hold the binaries' own unsafe code -- fennec-http's sockets, `poll`,
`getrandom` and file ACLs, fennec-server's signals and user switch,
fennec-shard's and the shell's -- which the clippy lint holds to its
comments alone; uninitialized reads (MemorySanitizer) and data races
(ThreadSanitizer), each a build of the standard library of its own; doc
tests; or what Miri checks of aliasing and provenance, which follows.
It runs for a pull request, a push to main, the schedule and a dispatch,
never for a merge group, where nothing waits for a check that is not
required. Its first step looks at the files a pull request or a push
changed against the commit it starts from and skips the tests -- the job
still reporting success -- where none is under `crates/`, the Makefile, a
manifest or the lock file, `rust-toolchain.toml`, `.cargo/` or `ci.yml`;
the schedule and a dispatch run them whatever changed. A pull request
merges past a red one (it is no required check, [above](#reruns)), and a
release waits for a green one at its commit (RELEASING.md).

Its first run, on an M-series Mac on 2026-11-15, found nothing: 1 000
tests, no report. Built at opt-level 1 they ran no longer than the
unsanitized debug build's: fennec-core's unit tests 54 s against 79,
`tests/all.rs` 198 against 258, the machine shared with other builds (a
load of 35 to 46 through the sanitized run); the build took 104 s. In CI's
test job the same binaries, unsanitized, take 3.5 minutes (60 s and 146
s), so the job, the nightly's install and the build with it, is bounded at
30 minutes and expected within 15. It restores the cargo cache as every job
of `ci.yml` does, saved by main's runs alone ([above](#reruns)), keyed by
the job and the nightly's date beside `Cargo.lock` and
`rust-toolchain.toml`: what it restores was built by that nightly with
AddressSanitizer's flags, the registry's crates among it, and never a
stable build's `target/`. The nightly itself is installed each run.

## Coverage

`.github/workflows/coverage.yml` runs weekly (Wednesday 03:41 UTC) and by
hand (`gh workflow run coverage.yml`), never on a pull request or a push. It
builds the workspace with `-C instrument-coverage` (the `llvm-tools-preview`
rustup component, not a crate: the workspace takes no dependency), runs
`make test`'s three Rust invocations, merges the counters with
`llvm-profdata` and exports them with `llvm-cov export`;
`tools/coverage_report.py` writes the tables, published as the
`coverage-report` artifact for 90 days (`coverage.md`,
`coverage-summary.json`) and in the run's summary: lines, regions and
functions per crate, and a second table, *Trust boundaries*, for the modules
that parse bytes from outside the process. It leaves out the doctests
(rustdoc's coverage is a nightly flag), the JS and browser tests, and the
PostgreSQL arm's `#[ignore]` tests, which need services the job lacks, so a
module those tests alone reach shows less than it has.

*The figure is information, not a gate.* No threshold is applied anywhere,
by the tool or the workflow: a percentage measures how much code ran, not
whether its behaviour is right, and a bar such as 85% would be arbitrary and
steer toward tests that execute lines without asserting anything. What
guards the boundary modules is their fuzz targets ([Fuzzing](#fuzzing)); the
report shows where a target or a test reaches little. Miri is a separate
item; the sanitized tests are [above](#sanitizers).

*The boundary list is data at the top of `tools/coverage_report.py`*
(`BOUNDARIES`): each module with one line saying what untrusted bytes it
reads. The engine's load path is `store.rs` (`replay`, `replay_mapped`,
`adopt_index`) and `fs.rs` (`open`, `open_with`), each a file row with a row
beneath it for those functions' regions and the number that ran (the export
gives a function's regions, not its lines). A listed path that is gone fails
the tool, so a rename cannot drop a row; `python3 -I tools/coverage_report.py
--check-paths` checks it, and `tools/test_coverage_report.py` (CI's fmt job)
holds the tables' numbers. A new parser of untrusted bytes is listed there
beside its fuzz target.

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
stood in the old test as "case 3".

Every primitive Project Wycheproof has a file for is held to every case of
it too. Six files of C2SP/wycheproof's `testvectors_v1/`, at one commit,
are committed unchanged in `fennec-http/tests/wycheproof/` with their
licence -- its README names the commit, each file's SHA-256 and how one is
updated: on purpose, in a change of its own, never in passing -- and run by
`tests/wycheproof.rs`; ES256's is `tests/es256/`'s. ChaCha20-Poly1305's 325
(`aead_wycheproof`): a valid case sealed gives its ciphertext and tag and
opened gives its message back, Poly1305's edge cases of a one-time key's
limbs, carrying sums and the final addition among them; the 60 tags with
bits changed are refused, the bytes left as they came, and the 9 nonces
not of 12 bytes by the type the primitives take, `&[u8; 12]`.
RSASSA-PKCS1-v1_5 with SHA-256's 776, at 2048, 3072 and 4096 bits
(`rsa_pkcs1_sha256_wycheproof_2048` and on), each key read from its JWK as
`--jwt-keys` reads one: the 24 valid verify, those of three e = 3 keys
among them, and the 749 invalid are refused -- forged paddings, BER
encodings, lengths up to 2^64 - 1, other hashes, PSS, and signatures of 0,
1, 2, n - 1, n, n + 1 and no bytes. `RsaKey` makes the message's encoding
and compares it whole, as RFC 8017 verifies (8.2.2), so none of a padding
a lenient parser would read -- where an e = 3 key lets anyone forge
(Bleichenbacher, 2006) -- is read at all. HMAC-SHA-256's 174
(`hmac_sha256_wycheproof`): keys of 16, 32 and 65 bytes, tags of 256 bits
and of 128 as the router's and the entry's marks are cut, each compared by
`ct_eq`. PBKDF2-HMAC-SHA-256's 60 (`pbkdf2_sha256_wycheproof`), passwords
of 0 to 257 bytes, UTF-8 or not: a case asking 16 or 32 bytes is held to
them all, one asking 42, 64 or 65 to its first 32 -- block 1, all
`pbkdf2_sha256` makes, the bytes past it made from an INT(2) or INT(3)
nothing in fennecdb asks for. An `acceptable` case is held to an answer
chosen for its kind, never let go either way, and a kind the files did not
hold fails its test until one is chosen. They hold one, `MissingNull`, an
RSA DigestInfo without the NULL parameters of its algorithm: refused, as
RFC 8017's EMSA-PKCS1-v1_5 writes the NULL for SHA-256 (9.2's note 1, and
appendix B.1's exception to SHA-2's parameters being left out) and its
verification compares encodings; Wycheproof calls such signatures invalid,
taken by some libraries for old signers' sake, and an RS256 token is an
identity provider's of today. Each test names every case answered
otherwise at once, never only the first, and holds its count of cases to
the file's `numberOfTests` and each result's to what the file held.

Every case agreed as first run: no primitive was wrong. Mutations show what
the files hold: an RSA encoding judged by its first two bytes and its hash
alone, as a parser finding the hash would judge it, is taken in 182 or 183
cases of each file, each named; a tag compared on 15 of its 16 bytes in
12; Poly1305's carry into h1 at a block's end dropped fails 255 of the 256
valid. One step neither RFC 8439's appendix nor Wycheproof's file reaches
is Poly1305's last carry, h0's into h1 once h4's overflow is folded back in
as 5: dropped, every vector of both still passed. It is reached where the
sum before s is added is j * 2^26 + i, i from 0 to 4, held as that plus p,
so a key and a last block solved for each other give it at once; four
such, their tags a big-integer Poly1305's and OpenSSL's alike, hold it
(`poly1305_last_carry`). The six files are 1.16 MB, included in the test
binary; their tests take 2.5 s of a core in `cargo test`'s build on an
Apple M1, 1.6 s of the clock beside each other: PBKDF2's 58 cases of 4 096
rounds and one of 80 000 are 1.6 s, the RSA files 0.8, the AEAD's and
HMAC's 0.01.

## API baselines

`fennec.h`, `web/fennec.d.ts`, `web/client.d.ts`, the flags and exit codes of
`fennec`, `fennec-server` and `fennec-shard`, and the metrics the server and
the router serve are held to the dumps in `integrations/api-golden/` by
`tools/api_compat.py` (`make api-check`; CI's `test` job runs it as a step,
this checkout's copy and main's, BREAKS read from the pull request's as
data, as `tools/openapi_compat.py`'s are; the fmt job runs the checker's own
tests). The dumps always are what the sources dump to: a dump that is not
fails, saying `make api-golden`, so an addition is committed with its dump
and a later pull request cannot remove it unseen. The pull request's dumps are compared with main's, dump against
dump: every entry of main's is in the pull request's, unless a `BREAKS`
entry names that difference, which also catches a dump edited smaller by
hand. An entry names one change: the surface, the symbol, the kind of
change and the digests of the declaration before and after (the failure
prints the entry to paste), so it approves no other change of that symbol.
What 1.0 changes is the rule for `BREAKS`: `integrations/api-golden/frozen.json`
says `null` until `python3 tools/api_compat.py --freeze 1.0` (once, at the
tag; it needs the dumps current and no entry for a release not made), and
until then an entry's release is `"1.0"`; after it, a break is a major
version above the frozen one, and `make version` refuses 1.0.0 or later while
the baselines are not frozen. The header is read
as macros (one under an `#if` carries its condition) and prototypes (return
type, each parameter's type and name, in order; whitespace and comments do
not count). The `.d.ts` files are read by
the checker's own scanner, not by `tsc`, so the check needs no `npm ci` and
runs as plain Python: every export is a record, a class's, interface's,
enum's or namespace's members are grouped by name, and a declaration is
compared as written, so any change to one -- narrowed parameter, widened
return, optional member made required, overload dropped or reordered --
fails, and a harmless widening is named in `BREAKS` too. Overloads are held
in the order written, since the first that fits is the one resolved; one
added anywhere passes. What the scanner does not know it refuses with exit 2,
never skips: `export { a as b }`, `export * from`, `export default`, a
namespace sharing a name with a function or class, and in `fennec.h` a
`typedef`, `struct`, `union` or `enum`, a function-like macro and a
prototype under an `#if`. A change that needs one of them teaches the
scanner in a pull request of its own first, because main's copy of the
checker judges the next pull request and would refuse the form. It cannot see a type's meaning through the names it uses,
a non-exported helper's body, or a new required member of an interface a
caller implements, an overload added in front of another; `make types-check` holds that the declarations still
compile and match `web/fennec.js`. The SDKs' dumps need outside tools
(Kotlin's binary-compatibility-validator, swift-api-digester, .NET's
PublicApiAnalyzers, `apidiff`) and come in 1.0.x.

The CLI's and the metrics' dumps (`cli-<binary>.json`,
`metrics-<binary>.json`) are the binaries' own, made by each binary's
crate's `tests/api_golden.rs` (fennec-server's `tests/api_dump.rs` holds
how, for the three) and held there byte for byte under `cargo test`, which
`make test` runs in every leg of the required `test` job; the checker reads
them and never makes them, and `make api-golden` has the tests write them
into `target/api-golden` (`FENNEC_API_GOLDEN`) for `--write --built` to
judge and copy. A command's flags are the dash-led words of its `--help`
that its parser takes, each asked of the binary: `<flag> --help <no such
flag>` is refused naming the flag where the parser does not know it,
prints the help and exits 0 for a switch, and is refused at the unknown
flag where the flag took `--help` for its value -- so a probe never runs
the binary, and a flag's arity is the parser's, not the help's words.
That holds because every binary and subcommand reads its whole command
line, refusing a flag it does not read or a value missing, before it
makes, opens, reads or locks anything a flag names: the router opened
`--audit`'s file as the flag was read, so its probe left a file named
`--help`, and the router, the backup family and `fennec mcp` read
`--tls-ca`'s. The shell's subcommands go further: every refusal of the
line -- the arguments, a flag the command needs, a URL, a token not given,
a sealed backup's key -- comes before a file a flag names (`--tls-ca`'s,
`--key-file`'s) is read or a lock is taken, held by fennec-cli's
`every_refusal_comes_before_a_file_a_flag_names` beside an unreadable
`--tls-ca` or `--key-file`; only whether the file a backup or a restore
writes is there already (2 without `--force`) is asked under its lock, so
nothing makes it between the asking and the rename. The router reads its
CAs and opens its audit log before it starts tracing, as fennec-server
does. Each probe runs in an empty directory of its own, which it
must leave empty, and its answer must carry no system error (`(os
error`, a file named `--help` read). A command reads, and its help names,
its own flags alone: `fennec key`, `verify` and `prune` shared `backup`'s
parser and help, so each took and ignored the family's eleven flags and
the dump froze them. The subcommands are those the helps name (`fennec
import ...`), found from help to help. A literal in the crate's sources
that is a flag's whole spelling and no help shows fails the test, so no
flag is taken unseen by the dump; `fennec-server`'s `--follow-<option>`
flags, made at run time from fennec-import's `args::OPTIONS` where no scan
sees them, are held to that list instead, each a flag of the server's help
and dump taking a value, as `fennec import`'s are. Removing a flag, or
turning a switch into a flag taking a value or the reverse, is a break;
the words of its help line are not held, so a value read in other units is
not seen. The exit codes are each binary's `exit::Code` (`src/exit.rs`)
with its meaning: every exit goes through `exit::now` or a subcommand
returning a `Code`, and the test refuses any other way out of the process
in the crate's sources, read as tokens with comments and literals aside
-- `std::process::exit` or `abort` by any path (spaced out, `r#exit`), a
`use` taking either in under any name, or `process`'s every name (`*`), or
the module under another (`process as p`, `{self as p}`), an `ExitCode`,
the binary's `main()` returning a value (an `Err` exits 1 with no `Code`),
each seeded in fennec-cli's `every_way_out_but_exit_now_is_refused` -- and
a variant `Code::ALL` leaves out. Its lexer is the literal scan's
(`api_dump::not_code`): every comment, string and char form Rust has, a
nested `/* */` and a raw byte or C string (`br#"..."#`, `cr"..."`) among
them, which it once read as a plain string, an exit after one unseen. The scheduler hands the server a failure
(`fn()`), never a number, so no code is decided outside it, and
fennec-server's `a_scheduler_s_failure_ends_the_server_with_1` holds that
stop to 1, a file's server and a node's, through a test build's fault
(`FENNEC_TEST_SCHEDULER_PANIC`) behind the `test-clock` feature, which
`tools/release_features_check.py` and release.yml's `+test-clock` check
hold out of a release: no failure a release can meet ends that thread on
demand. Behind `debug_assertions` it was live in every debug server
(`cargo run`, the web tests'), and `cargo test --release` waited for it in
vain; built without the feature a server reads no such variable
(`a_build_without_test_clock_has_no_scheduler_fault`), and CI's `test` job
runs the test built with it, by name. A code means what the code
does: 2 is a line refused before anything ran, 1 the work failing -- `fennec
types` exits 1 for a file it cannot read or parse and an `-o` it cannot
write, as the shell does for a file it cannot open, `fennec key` 1 when
the system gives it no randomness, as for a key it cannot write, `verify`, `prune` and
`restore --tenants` 1 for a root they cannot read, as for one archive, and
the router 2 for a lock file it cannot make, as for a directory it cannot open, keeping 1 for
a lock another holds. A code removed or its meaning reworded is a break;
one added grows the dump. The metrics are the
families the binaries' scrapes show, each with its type and every label a
sample carries (a histogram's `le` aside), over configurations that between
them show every family: a file's primary with `--hooks`, `--mcp`, a
schedule and `--replication-token`, its replica once in contact, and a
`--dir` node replicating; a router with `--replicas` and `--auto-failover`
over a node in a region taking leases and a node it cannot reach, and its
standby. A family with no sample fails the test, its labels unknown, and
the server's test holds every `fennec_` literal of fennec-http's,
fennec-server's and fennec-shard's sources to a dump (`fennec_router_` the
router's, every other the server's), refusing a name built at run time:
a family a configuration does not show, or a new one, fails there. A
metric removed or renamed, its type changed or a label of it removed is a
break; a metric or a label added passes, a selector naming the labels it
had still matching.

## Security policy

`SECURITY.md` is what a reporter and an operator act on, so it is held to
the tree by `tools/security_md_check.py`, in CI's fmt job beside
`tools/secret_eq_check.py` (its fixtures: `tools/test_security_md_check.py`).
It fails on a `--flag` the file names that the argument parser of the binary
it says it of does not read (`"--flag" =>` in fennec-server's, fennec-shard's
or fennec-cli's sources, outside comments and the `#[cfg(test)] mod`; any of
the three where no binary is named near the flag), a path or a
link's anchor it names that is not in the repository, a primitive of its
list of in-tree cryptography whose function is gone (or one the file stopped
naming), a missing "Supported versions" section or acknowledgement in working
days, and a missing "A security release" section in `RELEASING.md`. It
also searches `SECURITY.md`, `crates/*/src` and `site/content/docs` for
phrases once true and since false -- `no TLS` first, which fennec-tls made
untrue and which a comment, a help text and a page went on saying -- outside `ALLOWED`: the messages a build made without
the `tls` feature gives, each an entry with its reason, an entry that finds
nothing refused. It prints how many things it checked of each kind (332 when
written: 26 flags, 47 paths and anchors, 7 primitives, 249 files searched,
3 promises) and fails when any one kind is zero, not only the total: the
files searched alone would let a file stripped of every flag, path and link
pass. Each primitive's needle is its own declaration (`pub fn sha256(`), so a
test function of a like name does not hold it. What a line scan cannot tell
is in the tool's header.

The policy itself: 0.x has no advisories, CVEs or patch releases, a fix
lands on `main` and the pull request says which releases had the bug; from
1.0.0 the latest 1.x minor and the previous one for 6 months after the next
ships, a `release/1.N` branch cut when a backport needs one, a report
acknowledged within 3 working days. The steps are `RELEASING.md`'s "A
security release", written and not yet rehearsed.
