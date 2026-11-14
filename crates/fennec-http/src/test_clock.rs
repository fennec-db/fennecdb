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
//!
//! A shift's end is set until every open tenant took it: a slot busy as the
//! registry looks -- an open, a close, an install -- is passed over
//! (`Tenants::set_clocks`), and set once, a tenant busy at that moment stood
//! at the shifted time for good, its clock still. So the system's clock is
//! set again every step until no slot was passed over, for [`RESET_FOR`] at
//! most; past it the node says which it could not reach, and `make chaos`,
//! which reads every tenant's `now()` back after a jump, fails the seed.

use crate::tenants::Tenants;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// How often every open tenant's clock is set again while a shift holds,
/// and how soon a new shift, or its end, reaches them.
const STEP: Duration = Duration::from_millis(50);

/// How long a shift's end is set again while a slot stays busy: an open,
/// a close or an install holds its slot for as long as its disk takes.
const RESET_FOR: Duration = Duration::from_secs(10);

/// Resets that passed over a busy slot, for a test to wait on.
#[cfg(test)]
pub(crate) static PASSED_OVER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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
    follow_file(tenants, PathBuf::from(file))
}

/// [`follow`] of `file`, on a thread that ends once the registry has gone.
pub(crate) fn follow_file(tenants: &Arc<Tenants>, file: PathBuf) -> Result<(), String> {
    let tenants: Weak<Tenants> = Arc::downgrade(tenants);
    std::thread::Builder::new()
        .name("fennec-test-clock".into())
        .spawn(move || {
            // Since when a shift's end has been set without reaching every
            // open tenant; `None` once it has, or while a shift holds.
            let mut resetting: Option<Instant> = None;
            loop {
                // Held for the step alone, so the registry goes when the
                // node lets it go.
                let Some(registry) = tenants.upgrade() else {
                    return;
                };
                let shift = std::fs::read_to_string(&file)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .filter(|&s| s != 0);
                match (shift, resetting) {
                    (Some(s), _) => {
                        let now = fennec_core::time::now_ms().unwrap_or(0);
                        registry.set_clocks(Some(now.saturating_add(s)));
                        resetting = Some(Instant::now());
                    }
                    (None, Some(since)) => {
                        if registry.set_clocks(None) {
                            resetting = None;
                        } else {
                            #[cfg(test)]
                            PASSED_OVER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if since.elapsed() >= RESET_FOR {
                                crate::log!(
                                    "test-clock: a tenant's slot stayed busy for {} s as its \
                                     shift ended: its clock may stand at the shifted time",
                                    RESET_FOR.as_secs()
                                );
                                resetting = None;
                            }
                        }
                    }
                    (None, None) => {}
                }
                drop(registry);
                std::thread::sleep(STEP);
            }
        })
        .map(|_| ())
        .map_err(|e| format!("could not start the test clock's thread: {e}"))
}
