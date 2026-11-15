# CLAUDE.md

fennec-core's prepared blocks and pinned reads (`Database::prepare` in `prepared.rs`, `Database::pin` in `pinned.rs`), the handover, maintenance beside the database and compaction (`handover.rs`, `maintenance.rs`, `garbage.rs`) -- for code in `crates/fennec-core/src/engine/`; the rest of fennec-core's rules are in [its CLAUDE.md](../../CLAUDE.md), the rules every change meets in the root's [CLAUDE.md](../../../../CLAUDE.md).

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
stray read reruns under the lock. A stream's seed is one (`sse::seed`), read
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
changed (`Tails::all_changed`).

**[A file compacts itself once half of it is dead](../../../../docs/design/storage.md#compaction)**
and 64 MB: `CompactPolicy` is the one rule, run beside the database by
`Compactor` or `fennec_http::link` (`--auto-compact`), on replicas too.

**[A file is compacted before it outgrows the page cache](../../../../docs/design/storage.md#compaction-and-the-page-cache)**
(`CompactPolicy::memory`) so its live records fit the cache's room
(`cache::room`): once a third of what it writes is dead and the file
reached `ROOM_SHARE` of that room or the process faults
(`cache::faulting`), its own faults forgotten (`cache::forget_faults`).
