//! The crate's integration tests, one binary: each file was a binary of
//! its own, each linked with everything under it, and on macOS each
//! waited out the system's check of a new program on its first run.
//! The files apart (Cargo.toml) read the process's own counts, or run
//! without the indexes.

mod access;
mod api;
mod archive;
mod bound;
mod cdc;
mod changes;
mod chunked;
mod claim_values;
mod claims;
mod conditional;
mod dials;
mod empty_tokens;
mod es256;
mod fields;
mod idempotency;
mod lease_answers;
mod mcp;
mod named;
mod node_archive;
mod node_scheduled;
mod ordered_claims;
mod own;
mod prepared;
mod providers;
mod reach;
mod replication;
mod request_rate;
mod require;
mod scheduled;
mod schema;
mod server;
mod tenant_hooks;
mod tls;
mod token_first;
mod waits;
