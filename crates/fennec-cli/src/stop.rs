//! Stopping a command that runs until interrupted -- `fennec archive`,
//! `fennec import --follow` -- between two of its steps rather than in the
//! middle of one.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by SIGINT or SIGTERM.
pub static STOP: AtomicBool = AtomicBool::new(false);

/// Routes SIGINT and SIGTERM to [`STOP`]; libc's `signal` is declared here,
/// as `fennec-server` does, to add no dependency.
#[cfg(not(windows))]
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
    }
}

/// Routes Ctrl+C and Ctrl+Break to [`STOP`] on Windows, which raises no
/// SIGTERM: Ctrl+Break is the one event a supervisor can send a process's
/// group, its SIGTERM there (docs/design/windows.md, decision 4). kernel32's
/// `SetConsoleCtrlHandler`, as `fennec-server` takes them, in the CRT's
/// `signal`'s place: the MSVC CRT puts a signal's default action back
/// before it calls the handler, so a second Ctrl+Break ended the command
/// in the middle of a step, and SIGBREAK uncaught ended it at the first.
#[cfg(windows)]
pub fn on_signals() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    /// Ctrl+C (0) and Ctrl+Break (1) taken; the close, a logoff and the
    /// system's shutdown passed on, to end the command as they did.
    extern "system" fn stop(event: u32) -> i32 {
        if event > 1 {
            return 0;
        }
        STOP.store(true, Ordering::SeqCst);
        1
    }
    // SAFETY: Ctrl+C turned on -- a command started in a group of its own
    // begins with it off -- then a handler that lives as long as the
    // process; the call keeps no pointer of the caller's.
    unsafe {
        SetConsoleCtrlHandler(None, 0);
        SetConsoleCtrlHandler(Some(stop), 1);
    }
}
