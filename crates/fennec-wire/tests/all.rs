//! The crate's integration tests, one binary, as every crate gathers them.

mod client;

// TLS is the `tls` feature's, on unless the crate is built without its
// defaults.
#[cfg(feature = "tls")]
mod tls;
