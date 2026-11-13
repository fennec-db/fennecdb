# Conventions

The long form of the test and docs conventions [CLAUDE.md](../../CLAUDE.md) lists under *Conventions*: where tests live and which run alone, a test that fails on one run, Chrome's launch, the clock, ports, and what a change a user meets owes the docs. They hold no invariant of the code, so [README.md](README.md#the-original-order) does not number them. Each section below is a bullet moved word for word from CLAUDE.md, which keeps its rule in a sentence or two.

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
