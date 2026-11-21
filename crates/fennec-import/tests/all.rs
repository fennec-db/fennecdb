//! The crate's integration tests, one binary: each file was a binary of
//! its own, each linked with everything under it, and on macOS each
//! waited out the system's check of a new program on its first run.
//! The files apart (Cargo.toml) read the process's own counts, or run
//! without the indexes.

mod follow;
mod held;
mod pg;
mod sqlite;

/// Each thread's bytes counted (`held`): the SQLite reader's tests hold its
/// memory to a bound by what it allocates.
#[global_allocator]
static COUNTING: held::Counting = held::Counting;
