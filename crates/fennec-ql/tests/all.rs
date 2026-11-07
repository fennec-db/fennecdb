//! The crate's integration tests, one binary: each file was a binary of
//! its own, each linked with everything under it, and on macOS each
//! waited out the system's check of a new program on its first run.
//! The files apart (Cargo.toml) read the process's own counts, or run
//! without the indexes.

// Write-bound statements' syntax, and their refusal where statements run.
#[cfg(feature = "statements")]
mod bound;
mod e2e;
// Webhook consumers' syntax, and their refusal where statements run.
#[cfg(feature = "statements")]
mod hooked;
mod join;
// Named statements' syntax, read where the parser reads it.
#[cfg(feature = "statements")]
mod named;
// Numbers a statement writes, read whole or refused naming their range.
mod numbers;
// Scheduled statements' syntax, and their refusal where statements run.
#[cfg(feature = "statements")]
mod scheduled;
mod sql;
mod text;
