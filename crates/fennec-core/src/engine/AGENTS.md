# AGENTS.md

fennec-core's prepared blocks and pinned reads (`Database::prepare` in `prepared.rs`, `Database::pin` in `pinned.rs`), the handover, maintenance beside the database and compaction (`handover.rs`, `maintenance.rs`, `garbage.rs`) -- for code in `crates/fennec-core/src/engine/`; the rest of fennec-core's rules are in [its AGENTS.md](../../AGENTS.md), the rules every change meets in the root's [AGENTS.md](../../../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### The engine's lock and blocks

**[A block's documents are built beside the
readers](../../../../docs/design/engine.md#prepared-blocks)** (`Database::prepare`,
`early_put`), taken only where the collection holds the schema `Arc` they
were built against; all else is decided under the write lock. A hook that
may change a document keeps `Hook::rewrites` true, its payload encoded after
it.

**[A long read is pinned, and read with no lock held](../../../../docs/design/engine.md#pinned-reads)**
(`Database::pin`), sharing nothing a write changes in place; it declines a
read an index answers, and a pinned index refuses (`PINNED_OUT`), so a
stray read reruns under the lock. An image sent is one
(`Database::pin_image`), each record's sum made as it is written out. A
stream's seed is one (`sse::seed`), read
a page at a time, its scope bound under the pin's lock and its inner `get`s
answered on the pin (`Pinned::answer_subqueries`), the ring held back to it
till the first look (`hold_changes`); it reads again under the lock where
the ring passed it or an image was adopted (`Database::adoptions`), as
every look reseeds -- a long answer there pinned by its ids
(`Database::pin_rows`). A look answering
them again pins what they read and the rows written since alone
(`Database::pin_lists`, `sse::look`), the shape where they moved.

### Storage

**[Writes are handed over to the mapped file.](../../../../docs/design/storage.md#handover)**
Past `HANDOVER_AT` or `HANDOVER_DOCS`, `Database::hand_over` points the
stores at it, never with a block open, the file the same byte for byte; on
Windows at a view laid past the last, a mapping of many views let go of
on a thread, not under the write lock (`map::Teardown`).

**[A lone `create index` or `compact` runs beside the database](../../../../docs/design/storage.md#maintenance-beside-the-database)**
(`Database::maintain`): every write must pass `Database::note`; a schema
change meanwhile fails it, and an image adopted meanwhile marks every tail
changed (`Tails::all_changed`). Under the read lock it takes only what it
reads from and its tail's token, never a document out of the file: a
build's store as a pin takes it (`Store::pinned`), charged to the pins'
budget only while it is read (`Taken`), a compact's graph by graph; a
rewrite's cloned, its records in the file shared, uncharged -- the store
the swap puts in place, held the whole rewrite. The copy is read with no
lock held (`IndexCopy::read`), and the writes made meanwhile during a
graph's build or a file's rewrite are caught up in rounds, looked up
under the read lock and read after it (`catch_up_graphs`, `catch_up`),
the write lock taking the last round's few: a graph's links costing
`GRAPH_HELD` at most (`link_cost`: by dimensions, `m` and
`ef_construction`, a saturating `u64`), past which -- or where a round's
chunk or the last round's writes may take a graph past its nodes, asked
before any is linked (`past_nodes`) -- a compact keeps the graph and names
it (`KeptGraph`) and `create index` is refused; a rewrite's
bounded by the rounds' pace alone. Every collection is asked before any
graph is placed. Each hold is timed and handed back
(`Database::maintain_held`), and a server's log names the longest and
each graph kept.

**[A worn graph is rebuilt.](../../../../docs/design/hnsw.md#worn-graphs)**
Once its tombstones outnumber its live nodes, `GRAPH_WORN_FLOOR` of them
at least (`VectorIndex::worn`: two counts, no node read),
`Database::rebuild_worn` builds it again as a compact's graph phase does
(`graphs_online`), without the rewrite and within its bounds: a graph
behind its writes, unread or past its nodes is kept, and it alone tried
again only once as many writes as its tombstones have landed
(`VectorIndex::worn_due`); a refusal places none. The record a compact
beside the database wrote of the graph it replaces is dead from then
(`Persisted::kept`); one under the lock notes none. It runs where
compacts run, never on the graph keeper's thread: a server's compacter
after each compact look, a replica's too, the native library's
`Compactor` where no compact is due; every `compact` rebuilds each worn
graph, whatever it names. Whatever lets the database go stops a rebuild
at its next step, placing no graph (`rebuild_worn_until`, its stop
latched): the `Compactor`'s drop, a tenant's release, a shutdown
(`held::let_go`, `held::stops`).

**[A file compacts itself once half of it is dead](../../../../docs/design/storage.md#compaction)**
and 64 MB: `CompactPolicy` is the one rule, run beside the database by
`Compactor` or `fennec_http::link` (`--auto-compact`), on replicas too.
A compact renumbers no place and is refused for nothing a document holds:
one it cannot read is copied as it is, a graph it cannot rebuild kept
([dropped places](../../../../docs/design/storage.md#dropped-places)); a
copy reads each primary's write as it was made (`Schema::read_written`),
refusing one made by other places. From the first store it changes until
each points at the new file, a panic, a store refusing or a file the sink
cannot map fails the database (`compact_unwound`, `repoint`); a refusal the
server's compaction meets again is logged once and counted.

**[A file is compacted before it outgrows the page cache](../../../../docs/design/storage.md#compaction-and-the-page-cache)**
(`CompactPolicy::memory`) so its live records fit the cache's room
(`cache::room`): once a third of what it writes is dead and the file
reached `ROOM_SHARE` of that room or the process faults
(`cache::faulting`), its own faults forgotten (`cache::forget_faults`).
Past that room or faulting, a compact copies a collection of 4 MiB live
or more reading the old file [in
turn](../../../../docs/design/storage.md#reading-the-file-in-turn)
(`store::in_turn`, asked by the image under the lock and the side file
beside it, `ImageOut::in_turn`) and writes the bytes the walk by id
writes, the records in id order whatever read them: through buckets of a
quarter of the room, 16 MiB at most, dealt through buffers of 16 MiB
together at most whatever the file (`BUCKETS_MOST`), and a scratch file
`<file>.sorting` made by its open alone, following no link
(`fs::Scratch`), only where the volume holds twice the live bytes --
unlinked at once on a Unix, deleted on close on Windows; else, past 256
buckets, or where the scratch file fails before a byte of the new file is
written (said once), by id. One failing after, or reading back other than
its sum, fails the compact as a write of its new file does.
`tests/in_turn.rs` and `store::in_turn::tests` hold it to the walk by id,
byte for byte.

**[Dead bytes are whole frames](../../../../docs/design/storage.md#dead-bytes-are-whole-frames)**
Live bytes are the live frames to the byte: a version written over or deleted adds its head and its payload to
`dead_bytes`, through `store::dead_frame` alone; counting a payload alone,
or a writer adding a dead version any other way, leaves its head counted
live, and `Garbage::live`, `fennec_data_bytes` and the compact's trigger
read it high. `store::tests::the_live_bytes_are_the_live_frames_to_the_byte`
holds it through appends, an undone block and every load; a file an older
build wrote counts its dead heads short until its next compact.
