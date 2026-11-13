# CLAUDE.md

`--follow`'s invariant, for code in `crates/fennec-import/` and the wire client it reads through, `crates/fennec-wire/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### Replication and archives

**[`--follow` confirms nothing that is not on disk.](../../docs/design/replication.md#follow)**
Every write is a put or delete by id; the mirror is a `fennec-server` thread,
its collections' only writer. A wire message's length is held to
`proto::MAX_MESSAGE` before anything is allocated (`proto::zeroed`). The
tests need a live PostgreSQL and are `#[ignore]`d; CI's `postgres` job runs
them as `make import-test` does and fails unless every test `tests/pg.rs`
and both `tests/follow.rs` mark so passed: a new one goes in one of those
files.
