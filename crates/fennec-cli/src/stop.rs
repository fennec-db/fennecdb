//! Stopping a command that runs until interrupted -- `fennec archive`,
//! `fennec import --follow` -- between two of its steps rather than in the
//! middle of one.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by SIGINT or SIGTERM.
pub static STOP: AtomicBool = AtomicBool::new(false);

/// Routes SIGINT and SIGTERM to [`STOP`]; libc's `signal` is declared here,
/// as `fennec-server` does, to add no dependency.
pub fn on_signals() {
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    extern "C" fn stop(_sig: i32) {
        STOP.store(true, Ordering::SeqCst);
    }
    unsafe {
        for sig in [2, 15] {
            signal(sig, stop as *const () as usize);
        }
        // Windows raises no SIGTERM: Ctrl+Break, the CRT's SIGBREAK, is the
        // one event a supervisor can send a process's group, its SIGTERM
        // there (docs/design/windows.md, decision 4). Uncaught, it ended
        // the command mid-step.
        #[cfg(windows)]
        signal(21, stop as *const () as usize);
    }
}
