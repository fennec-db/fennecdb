//! A test build's clock for a tenant node (`--features test-clock`, never a
//! release's): `make chaos` jumps a node's wall clock by writing a shift in
//! milliseconds into the file `FENNEC_TEST_CLOCK` names, and every tenant
//! open on the node then answers `now()` and its `@ttl` reads at the
//! system's time plus that shift -- through `Database::set_clock`, the seam
//! the browser module hands its time through.
//!
//! Why not the container's clock: Docker cannot move one container's wall
//! clock -- a time namespace offsets `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME`
//! alone -- and the kit's binaries are static musl, so no faked
//! `clock_gettime` can be preloaded into them. Why a file and not a route:
//! a route would be one the OpenAPI document must list
//! (`every_route_the_server_matches_is_documented`) or one a test build
//! answers that no release does, and the file is reachable only from inside
//! the node's machine, as its clock is.
//!
//! What it moves: what reads a database's clock -- `now()` in a statement,
//! the rows a `@ttl` leaves out. Not the lease, which runs on the boot clock
//! (`lease::Moment`) and is no wall clock's to move, nor what reads the
//! system's time itself (the sweep, a token's `exp`, the archive's stamps).
//! A clock pinned by `set_clock` stands still, so it is set again every
//! [`STEP`] while a shift holds: the time runs on in steps of it.

use crate::tenants::Tenants;
use std::sync::Arc;
use std::time::Duration;

/// How often every open tenant's clock is set again while a shift holds,
/// and how soon a new shift, or its end, reaches them.
const STEP: Duration = Duration::from_millis(50);

/// Follows the file `FENNEC_TEST_CLOCK` names, where it names one, on a
/// thread of its own: its text a shift in milliseconds (`86400000`,
/// `-3600000`); a file missing, empty, unreadable or `0` is none, and every
/// tenant goes back to the system's clock. Nothing where the variable is
/// unset.
pub fn follow(tenants: &Arc<Tenants>) -> Result<(), String> {
    let Some(file) = std::env::var_os("FENNEC_TEST_CLOCK").filter(|f| !f.is_empty()) else {
        return Ok(());
    };
    crate::log!(
        "test-clock: every tenant's clock is the system's shifted by the milliseconds {} holds \
         -- a test build's, never a release's",
        std::path::Path::new(&file).display()
    );
    let tenants = Arc::clone(tenants);
    std::thread::Builder::new()
        .name("fennec-test-clock".into())
        .spawn(move || {
            let mut shifted = false;
            loop {
                let shift = std::fs::read_to_string(&file)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .filter(|&s| s != 0);
                match shift {
                    Some(s) => {
                        let now = fennec_core::time::now_ms().unwrap_or(0);
                        tenants.set_clocks(Some(now.saturating_add(s)));
                        shifted = true;
                    }
                    None if shifted => {
                        tenants.set_clocks(None);
                        shifted = false;
                    }
                    None => {}
                }
                std::thread::sleep(STEP);
            }
        })
        .map(|_| ())
        .map_err(|e| format!("could not start the test clock's thread: {e}"))
}
