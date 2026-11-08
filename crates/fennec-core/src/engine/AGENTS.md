# AGENTS.md

fennec-core's prepared blocks and pinned reads (`Database::prepare` in `prepared.rs`, `Database::pin` in `pinned.rs`) -- for code in `crates/fennec-core/src/engine/`; the rest of fennec-core's rules are in [its AGENTS.md](../../AGENTS.md), the rules every change meets in the root's [AGENTS.md](../../../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

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
stray read reruns under the lock. A stream's seed is one (`sse::seed`), its
scope bound under the pin's lock and its inner `get`s answered on the pin
(`Pinned::answer_subqueries`), the ring held back to it till the first look (`hold_changes`);
it reads again under the lock where the ring passed it or an image was
adopted (`Database::adoptions`), as every look reseeds. A look answering
them again pins what they read and the rows written since alone
(`Database::pin_lists`, `sse::look`), the shape where they moved.
