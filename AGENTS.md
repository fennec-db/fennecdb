# AGENTS.md

This file provides guidance to Codex and other coding agents when working with code in this repository. It is written from CLAUDE.md by `make agents-md`: edit that one.

fennecdb -- a minimal embedded database in Rust, compiled to WASM for the
browser, with its own query language (FennecQL) and a server
(`fennec-server`) speaking HTTP, as every language reaches it: Python,
JavaScript, Go, .NET, Swift, Kotlin and Dart through official clients. User
docs are `site/content/docs/`, `README.md` their front door; this file is
the working summary and the rules a change anywhere can break, each area's
invariants in a CLAUDE.md beside its code ([indexed](#area-invariants)),
each invariant's and convention's long form in `docs/design/` (ordered by
its `README.md`); `tools/agents_md.py --check` fails once this file passes
32 000 bytes or an area's 24 000. Each `AGENTS.md` is written from the
CLAUDE.md beside it (`make agents-md`), CI failing when the two differ:
edit `CLAUDE.md`.

## Commands

```bash
make test  # Rust (fennec-core also without its indexes and without any feature), the JS tests, sync-scenarios-check
make wasm  # fennec-wasm to web/fennec.wasm; FEATURES="text sorted": those alone (indexes, language: a group; none: none)
make wasm-lite  # test build without the indexes (web/fennec-lite.wasm), never shipped; wasm-min: with no feature
make wasm-sizes  # what each feature adds to the module with none, and what leaving it out saves
make size-report  # where the module's bytes go (BASE=main, BIN=fennec-server, FEATURES=none)
make ffi  # the native library (crates/fennec-ffi); TARGET=... another, JNI=1 for Kotlin
make sync-scenarios-check  # both sync runners passed every scenario (after make test)
make agents-md  # each AGENTS.md rewritten from the CLAUDE.md beside it
make builder-golden  # integrations/builder-golden.json rewritten from the JS builder (web/golden.mjs)
make docs-builder  # the SDKs' tests of the site's builder tabs (site/builder_tabs.py)
make schema-golden  # integrations/schema-golden.json rewritten through the module (web/schema-golden.mjs)
make types-check  # web/fennec.d.ts held to web/fennec.js and a caller's code under tsc --strict, then docs-types
make studio-highlight  # studio/highlight.js and the Grafana data source's copy rewritten from site/highlight.js
make pgvector-up  # PostgreSQL 17 + pgvector in Docker on 127.0.0.1:55432 for comparisons and --follow (pgvector-down)
make server TOKEN=secret HTTP=127.0.0.1:8080  # the server against ./data.fennec
make node ADMIN=secret  # a tenant node: fennec-server --dir tenants
make shard  # the router in front of the nodes (./shard.fennec)
# Every other target has a `##` comment above its rule in the Makefile:
# serve, small, a release's (packages, packaging-test, version V=X.Y.Z:
# RELEASING.md), the suites (*-test, grafana-check, deploy-check, drill;
# `make wasm` first for react, langchain, ai-sdk and cloudflare) and the
# measurements (every *-bench, ycsb, beir, ...), none in CI.
```

Single tests:

```bash
cargo test -p fennec-core --test all persist::  # one file of a crate's integration tests (one binary)
cargo test -p fennec-ql near  # by name substring (integration: fn name only)
cargo test -p fennec-core codec::tests  # inline unit tests in a module
cargo test -p fennec-core --no-default-features --features std-fs,language --lib --test features  # without the indexes (`--features std-fs` alone: without any feature)
cargo test -p fennec-import --test all pg:: -- --ignored  # needs a live PostgreSQL
node --test web/fennec.test.js  # JS: builder
node --test --test-name-pattern 'shape' web/fennec.sync.test.js
```

`make test` runs Rust first on purpose: `cargo test` builds the
`fennec-server` binary that `web/fennec.sync.test.js` runs against (it
self-skips without it or `web/fennec.wasm`). **The wasm32 target comes from
rustup**, never Homebrew's `cargo` (`$(CARGO)` prefers `~/.cargo/bin/cargo`;
by hand, `rustup target add wasm32-unknown-unknown`).

## Architecture

Dependency direction (nothing points back up):

```
fennec-core (std only, zero deps)
     |
fennec-ql   (lexer + parser)          fennec-abi  (both C ABIs' answers, a replica's sync)
     |                                fennec-wasm (browser C ABI)   fennec-ffi (native C ABI, apps)
     |
fennec-http (REST/JSON + SSE, tenant registry, replication, /_metrics)
     |                     \
fennec-wire (a PostgreSQL     fennec-shard (tenant router: directory,
     |      client: framing,             placement, move)
     |      SCRAM, TLS, COPY, pgoutput)
fennec-import (SQLite file reader + PG COPY source + --follow)
     |                                  \
fennec-server (HTTP, the syncer and       fennec-cli (`fennec` shell,
     shutdown, the importer's --follow)   `fennec import`, `fennec types`)
```

In `crates/fennec-core/src/`, `engine` holds `Database` and `Collection`,
`store` the segments and offset index; `fs`, `cache` and `synclog` are
behind `std-fs`; the indexes are `vector`, `text`, `highlight`, `fuzzy`,
`sparse`, `sorted`, `composite`, `geo`, `hll` and `maps`, and `off` stands
in for one a build is made without.

The browser client is `web/fennec.js` -- WASM glue (~636 lines),
persistence and the sync layer -- re-exporting the query builder
(`web/builder.js`) and the HTTP client (`web/http.js`), dependency-free ES
modules; `web/fennec.d.ts`
and `web/client.d.ts` hold the types, `fennec types <file>` a schema's.
[In the page](docs/design/js-client.md#persistence-in-the-page)
`persist`/`restore` keep an image and the journal's chunks in IndexedDB
(`fennec_journal`, `fennec_drain`; off until asked for), sealed by a
`cryptoKey`; `openFile` keeps `fennec-server`'s own file in OPFS, never
sealed, flushing a new image into `<name>~` before writing over the file.

## Invariants

These are the rules a change anywhere can break; every other invariant is
in the CLAUDE.md of the area whose code enforces it, each listed in the
[index below](#area-invariants).

### The crates

**[Zero dependencies is a hard rule](docs/design/crates.md#zero-dependencies)**
but for `fennec-bench` and the user's exception, `fennec-tls` (rustls,
ring, webpki-roots), behind a `tls` feature: never by fennec-core, -ql,
-abi, -wasm or -ffi, its tree pinned (`tools/deps_check.py`).

**[Limits error, they do not truncate.](docs/design/crates.md#limits)**
`near`, `match` and `fuse` rankings cap at 10 000 rows, expressions at 512 levels,
a `lookup` chain at 8, `in (get ...)` or `in $n` at 100 000 values (a
`get` 4 levels deep), each a query error, never a cut result; a new limit
takes that shape.

**[Profiles differ on purpose.](docs/design/crates.md#profiles)**
`fennec-cli` takes `cli` (`panic = abort`), `fennec-server` `release`: a
connection's panic unwinds alone.

### The engine's lock and blocks

**[Single writer, and readers beside it.](docs/design/engine.md#single-writer)**
Never two processes over one file: whatever writes a served file is a
`fennec-server` thread, never a binary of its own, and a block is open only
under its opener's write lock. Whatever opens a file to write it, or
renames one into its place, holds `<file>.lock` (`fennec_abi::lock`; a
`--dir` node's tenant in its sink, `lock::Holding`, let go with the
database), refused while another does, a symbolic link resolved to its
file for the lock and every rename (`fs::resolved`); `--read-only` and
`fennec types` take none, and `backup` or `restore` replaces a file only
with `--force`. The design file names every holder and what escapes the
lock.

**[Every write is a block, and a block is one record.](docs/design/engine.md#blocks)**
`commit` appends it whole and only then notes it, so a crash, a replica
and a subscriber see all of it or none. A failed one is undone in memory,
a schema change too (`Undo`, a variant each); whatever counts records
counts their writes (`writes_in`), and what cannot be undone, a compact,
is refused in a block (`Statement::fits_block`). A server takes a lock a
panic poisoned as it stands: a panic before the sink call puts the block
back whoever opened it (`Database::whole`, `Unlanded`, a server's or the
native library's between statements by `held::Writing`); one from the
sink call through the notes to the block's end fails the database
(`landing`), or the next block reuses the file's change numbers; nothing
lent out around a write -- the registry, its hooks and `Check` (`Lent`), a
flag -- is left out.

### Durability

**[A storage error stops writes.](docs/design/durability.md#storage-errors)**
Writes after one are `Error::Io` until reopened (`Database::failure`). A
mapped file is replaced only by rename. The fsync runs after the lock
(`Database::flush`'s `Durability`), a durable write answered only once it
has, and a write's refusal, read from what others landed, once theirs has
(`refusal_durability`; none for a storage error's, a replica's or the
request's own, `api::refusal_read`); a failed
one is never retried (500 under `--sync always`) and goes to
`Database::fail`. A write the lock still makes is put off while a
durability holds the disk (`Sink::syncing`). A durability asked under the
read lock clears nothing (`Database::durability`), its caller handing a
sink's error to `Database::fail` once the lock is let go. A durability
covers what an earlier process left in the file, a plain `sync` does not
(`Disk::inherited`): the file's own fsync, no sync log begun, where
nothing was appended.

## Area invariants

The rules above are the ones a change anywhere can break. Every other
invariant is in the CLAUDE.md beside the code that enforces it, which
Claude Code loads once it reads a file in that directory (Codex reads the
`AGENTS.md` beside it). A rule binds wherever its code reaches -- access
binds fennec-core's hooks, replication spans the engine and the server --
so before changing code, find here every rule it may touch and read the
file that holds it. Each entry names a file, the directories it covers and
its rules' titles by section; `tools/agents_md.py --check` holds the index
to the files.

### `crates/fennec-core/CLAUDE.md`

Covers `crates/fennec-core/`; the browser module's rules bind `crates/fennec-wasm/` and `web/` too.

*Storage*
- No buffer pool of its own, and the file is mapped.
- Writes are handed over to the mapped file.
- A block past 16 MB spills into the file before it lands.
- A lone `create index` or `compact` runs beside the database
- A file compacts itself once half of it is dead
- A file is compacted before it outgrows the page cache
- File format

*The engine's lock and blocks*
- A block's documents are built beside the readers
- A long read is pinned, and read with no lock held

*Durability*
- Buffered writes are written behind the lock
- A durable write syncs a log beside the file, not the file

*The HNSW graph*
- The HNSW graph is derived data, not a cache.
- A restore gives back the arena it had, to the bit.
- A graph record is laid out flat.
- A batch links in parallel, into the graph it would link in turn.
- The upper layers are walked with a beam.
- A block's `put`s link their vectors together.
- A server answers before its graph is linked.
- A server keeps its graphs in its file.
- A vector written again is one node.
- Natively the copies table grows from its own slots.
- A graph record carries the copies table's halves.
- The browser module hashes as a native build does.
- An open copies a graph's vectors into the arena.

*Writes*
- `insert` never writes over; `put` does.
- A `set` reads the row it writes, and every write says what it did.
- `require <n>` makes a write's count a condition.
- `if absent else set` is an upsert; `put <c> $n` takes a parameter's documents.
- A `set` or a `del` picks its rows with `order` and `limit`, and `returning` answers them.
- A key of `order` may be an expression
- A `put` answers the rows it wrote with `returning`.

*Indexes*
- `@unique` is a `@hash` that asks its bucket before a write.
- A field keeps one index, and a text field `@text` beside a `@hash`, `@unique` or `@sorted`.
- A `@hash` bucket holds its ids ascending, in runs
- A hash index's table grows a 256th at a time, and a text index's terms too
- `@ttl` is an ordered index whose rows expire, and every read leaves them out at once.
- Only an `and` chain reaches an index.
- A filter is bound to its collection once a query.
- `@sorted` must give the scan's answer, row for row.
- An index over several fields is its parts' bytes laid end to end
- An open builds no hash, text, ordered or sparse index.
- A server warms what an open left for the first read
- A point is two `f64`s on Redis's sphere; `@geo` is Morton keys

*The browser module*
- Threads are `cfg`'d out of WASM.
- Indexes are features; a build without one opens a file declaring it.
- The rest of the language is features too.
- What is core, and why one module ships.
- The module holds one copy of a generic.
- The core lost what it held twice or for one use.
- The module grows nothing by doubling past a mebibyte.

*Vector search*
- wasm32's SIMD kernels match the scalar ones bit for bit.
- aarch64's strips are NEON, to the same bits.
- A vector goes out as its `f32`s.
- A number is read exactly.
- A vector parameter goes into the module as `f32`s.
- A quantized index holds codes, and `near` orders by the vectors.
- A bit code is a residual's signs from a learned centre.
- Filtered `near` needs its fallback.
- `sparse<N>` is pgvector's `sparsevec`, and `@inverted` answers exactly.

*Text*
- `collate und` and `collate tr` are ICU's orders.
- `nocase` text compares by its case fold wherever it compares.
- `match` prunes with MaxScore.
- Beside `order`, `count` or an aggregate, `match` is a filter: the rows its ranking scores above zero.
- `@text` matches whole words unless told otherwise.
- The text index is derived and never persisted.
- A text index's words are a table of their own (`text/terms.rs`).
- `fuzzy` and `prefix` walk a sorted copy of the words.
- `rerank` deliberately uses no index.
- `fuse` adds ranks, not scores.
- `highlight()` re-reads the text; `facet` counts beside the rows.
- A disjunctive facet's filter is split as written.

*lookup and aggregates*
- `lookup` chains, and the chain is still positional.
- `lookup` is one bucket probe per parent, not a join.
- `required` is the other half, and it is a pass.
- A `lookup` over a key of several fields is a run of an index over several fields.
- A `lookup` level may fold its children, and a `lookup` hangs off groups.
- An item of a select list is an expression, and an aggregate folds one
- `approx_count_distinct` is HyperLogLog, and `having` keeps groups
- `bucket(at, 1d, 'Europe/Berlin')` reads a zone's clock

### `crates/fennec-ql/CLAUDE.md`

Covers `crates/fennec-ql/` and the shell in `crates/fennec-cli/`; its rules bind fennec-core's statements, schemas and named statements (`declared.rs`, `named.rs`, `engine/stored.rs`) and fennec-http's `/_call` too.

*FennecQL and schemas*
- SQL's spellings are FennecQL, read by the one parser
- A JOIN on a key is a `lookup` answered flat, or one table's `get` with the other's rows a filter
- `limit`, `offset` and `top` take a parameter, read where the plan reads them
- `in (get ...)` is answered before the query, as the list it is.
- `alter collection` rewrites no document; positions are not places.
- A `json` field holds any value, and a path is a name.
- The shell runs a statement once nothing after it can go on with it
- A schema declared in code is checked at every open, in the engine.
- Named statements are records of their database, called with their own text's authority.
- A write-bound statement runs in the block of the write that woke it, once a row.
- A scheduled statement runs once a slot, by the primary that may write its file.
- A webhook consumer's record is kept by every build, and no server takes its events yet.

### `crates/fennec-http/CLAUDE.md`

Covers `crates/fennec-http/`; access binds fennec-core's hooks (`plugin::Hook`, `Check`) and its own collections' names (`declared::OWN`), replication fennec-core's `wal` and `changes_in`, held writes its wakes (`changes.rs`).

*Replication and archives*
- Replication ships only what is on disk, by the change counter.
- A node's tenants are archived over one stream
- `/_changes` reads the writes on disk, documents and all
- A read can wait for a write on a replica
- An archive and a backup can be sealed

*Access*
- A scoped token is held to its rules at every level, twice for writes.
- A field a read rule withholds is held out of what a statement does.
- A rule may read another collection, answered before each statement.
- A collection whose name starts with `_` is the server's, out of every rule's reach.
- A token is bound to the tenant it names.
- A token is taken only from the issuers, for the audiences and clients, a server was told.
- An empty token is no token.

*Quotas and retries*
- A tenant is held to its quota by its node, a block as it lands.
- A quota is a refusal where waiting makes no room, a wait the server names otherwise.
- Every 503 says when to come back, in its body too.

*The HTTP server*
- An agent's MCP tool is a statement, held to the token as `/query` is.
- An `Idempotency-Key` makes a write once.
- Refusals and schema changes are logged; a refusal waits.
- Every request has an id, in its answer and every line it wrote.
- A request is a trace, sent over OTLP.
- `/_metrics` counts at the edge, a shard per thread.
- `/_stats/statements` counts by shape, a tenant's apart.
- A statement is parsed once, and an answer goes out in one write.
- A chunked body grows as it comes, held to the ceiling
- A plain `get` is written as JSON from the stored documents.
- A connection no thread can be made for is answered 503.
- `--tls-cert` and `--tls-key` make a listener HTTPS alone
- Every HTTP dial takes HTTPS by its URL.

*Held writes*
- `Fennec-Wait` holds a `set` or a `del` that writes nothing until it can
- A write wakes the held writes reading what it wrote, a row going to the group ranking it first
- One held request looks for every group, and a group finding nothing looks among the rows written since

### `crates/fennec-server/CLAUDE.md`

Covers `crates/fennec-server/`.

*Durability*
- Every way a server stops is the one shutdown.

### `crates/fennec-import/CLAUDE.md`

Covers `crates/fennec-import/` and `crates/fennec-wire/`; the mirror is `crates/fennec-server/src/mirror.rs`.

*Replication and archives*
- `--follow` confirms nothing that is not on disk.

### `crates/fennec-shard/CLAUDE.md`

Covers `crates/fennec-shard/`; its rules bind fennec-http's tenant registry, leases and replication (`tenants.rs`, `lease.rs`, `replication.rs`) and the entry Worker (`integrations/cloudflare/entry/`) too.

*Tenants and leases*
- Scaling out is by tenant, one file each.
- A failed attach is tried again, a file left taken back by identity.
- A tenant node runs its tenants' schedules from one thread, by name, a run no use of a tenant.

*Regions and the global entry*
- A node is in a region and a zone, and a tenant in its node's region.
- A region may keep its own directory, and leases and fails over inside itself.
- One URL reaches every region: the Worker learns homes from the routers.

*Read replicas*
- Read replicas in other regions answer only marked reads.

*The router*
- The router tells a page one CORS rule, its own.
- The router looks at a pooled connection first, and resends only what cannot have run or may run twice.
- A failover ends what the router has out to the node for the tenants it moved.
- A query across tenants is the router's admin's, merged as the request says.

### `crates/fennec-abi/CLAUDE.md`

Covers `crates/fennec-abi/`; the scenario file is `integrations/sync-scenarios.json`, run by `web/` too.

*The native library and sync*
- A replica's sync is a state machine; the network is the binding's.
- Both sync runners are held to one scenario file.

### `crates/fennec-ffi/CLAUDE.md`

Covers `crates/fennec-ffi/`.

*The native library and sync*
- An app keeps its database through `fennec-ffi`.

### `web/CLAUDE.md`

Covers `web/`.

*The JS client and Cloudflare*
- One module and a client entry, by where the queries run.
- A Durable Object keeps a database as a file would.
- An object's alarm keeps its database.
- A live query learns what changed from the change ring.

### `studio/CLAUDE.md`

Covers `studio/`; fennec-http serves it.

*The HTTP server*
- fennec studio is a page with no authority of its own.

### `integrations/CLAUDE.md`

Covers `integrations/`.

*Integrations and deploy*
- `integrations/` may use outside packages; the crates may not.
- The desktop app is a window on a studio a `fennec-server` serves, one server per file.
- Grafana's data source binds a dashboard's values; it never splices them.

### `deploy/CLAUDE.md`

Covers `deploy/`.

*Integrations and deploy*
- `deploy/` is the regions' kit, held to the binaries by two checks.

### `tools/CLAUDE.md`

Covers `tools/homebrew-formula.py`, `tools/linux-packages-check.sh`, `site/install.sh` and `.github/workflows/`'s `release.yml` and `packages.yml`.

*Integrations and deploy*
- A release is installed from its own files, each checked before it is used.

### `crates/fennec-bench/CLAUDE.md`

Covers `crates/fennec-bench/`.

*Benchmarks*
- `make scale-bench` holds fennec-server to pgvector, each over its own protocol.
- `make ycsb` is fennecdb as a general database.

## Conventions

- `Error` (`fennec-core/src/error.rs`) is the single error type,
  allocation-free, no `Box`; `fennec-http` maps it onto HTTP statuses
  (`api::status_of`).
- [Unit tests live inline](docs/design/conventions.md#test-binaries)
  (`#[cfg(test)] mod tests`); cross-crate and protocol tests in
  `crates/*/tests/`, a file each, one binary a crate through `tests/all.rs`
  (`autotests = false`: a new file needs its `mod` line). A test reading the
  process's own counts (`/_metrics`, the statements'), one whose servers
  hold many threads and one opening the audit log (`tests/audit.rs`,
  `tests/refusals.rs`) are each a `[[test]]` of its own, the other tests'
  servers asking with no refusal wait (`audit::set_delay(0)`). Measurement
  programs (`examples/`, `fennec-bench`) are wired to `make`, not CI.
- [A test that fails and passes on a rerun](docs/design/conventions.md#reruns)
  is a bug, the test's or the code's: reproduce it in a loop under load
  (`docker run --cpus=3` beside busy loops is GitHub's runner) and fix it,
  never retry it away or lengthen a sleep. A test waits for an event, not a
  time: it holds what it races (`Replica.setOnline(false)` before a write it
  reads back) and bounds only the wait for what must happen. A step
  launching Chrome reads Chrome's files first (`warm`, `warm-chrome.mjs`),
  never waiting longer for its port. A CI log names each failing test and
  its assertion (spec reporters, Gradle's `testLogging`); a failing mobile
  job uploads `test-reports-android` or `test-results-apple`.
- [A test reads the clock from below](docs/design/conventions.md#the-clock)
  (`took >= wait`), bounding it from above only where the time is the
  property, by what it stands against with seconds to spare; elsewhere it
  asserts the event, the count or the order (`audit::would_wait`,
  `Router::grants_waiting`, `Router::round_grants`). A timeout given a
  test's servers or clients bounds every request they make.
- [A server a test or a harness starts](docs/design/conventions.md#ports)
  listens on port 0, never a fixed or found-free port, found by its
  `listening on: http://<address>` stderr line, a router told its nodes by
  the addresses they said; only a site needing its address before it binds
  takes a system-picked port, held by a stand-in listener till then.
- [A change a user meets](docs/design/conventions.md#docs-a-change-owes)
  says so in the same pull request: its page under `site/content/docs/`,
  README's feature table and the links beside it, the home page and docs
  index where a visitor would pick fennecdb for it, and the SDK lists
  (`languages.html`, `mobile.html`, `sharding-read-replicas.html`,
  `integrations.html`) when an SDK changes; after sections change, commit
  the next `site` deploy's `search-embeddings` artifact as
  `site/search-embeddings.json` where it differs.
- An invariant's rule goes in the CLAUDE.md of the area whose code
  enforces it, its title in the root's [index](#area-invariants) -- in the
  root itself if a change anywhere can break it -- in a few sentences: what
  must hold, must never be done, enforces it and a change owes; its
  measurements, rejected alternatives and history in its `docs/design/`
  file, in the same pull request; a new invariant gets both. A new area
  file gets an index entry and its `AGENTS.md` (`make agents-md`).
- Comments explain *why* -- a measured cost, a trap that was hit, an
  alternative rejected; match that when adding code.
- All prose in the repo (comments, docs, README) is English.