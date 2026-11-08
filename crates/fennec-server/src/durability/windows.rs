//! Stopping on Windows: every console event onto the one shutdown
//! (docs/design/windows.md, decision 4).
//!
//! Windows raises no `SIGTERM`, and the CRT's `signal` catches Ctrl+C
//! alone: Ctrl+Break, its `SIGBREAK`, ended the process by the default
//! action, as closing the console did, with no final sync. kernel32's
//! `SetConsoleCtrlHandler` takes every event in the CRT's place, declared
//! here as `fennec_abi::lock` declares its calls: the crates take no
//! `windows-sys`. The system calls the handler on a thread it makes for
//! the event, so the handler stores the flag and the syncer does the rest,
//! as a signal's handler leaves it on Unix.

use super::SHUTDOWN;
use std::sync::atomic::{AtomicBool, Ordering};

/// The events, wincon.h's.
const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_CLOSE_EVENT: u32 = 2;
const CTRL_LOGOFF_EVENT: u32 = 5;
const CTRL_SHUTDOWN_EVENT: u32 = 6;

/// A handler's answers: the event taken, or passed to the next handler.
const TAKEN: i32 = 1;
const PASSED: i32 = 0;

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

/// Takes the console's events (`SetConsoleCtrlHandler`): Ctrl+C,
/// Ctrl+Break, the console's close and the system's shutdown each start
/// the one shutdown. Installed where Unix installs its signals' handlers,
/// before the line saying the server listens: a supervisor that sends
/// Ctrl+Break as soon as it reads that line would otherwise end the server
/// by the default action, with no final sync.
pub fn install_signal_handlers() {
    // SAFETY: a handler of the signature the call asks, living as long as
    // the process; the call copies nothing it points to.
    if unsafe { SetConsoleCtrlHandler(Some(on_event), 1) } == 0 {
        fennec_http::log!(
            "the console's events could not be taken ({}): Ctrl+C and Ctrl+Break end the \
             server with no final sync",
            std::io::Error::last_os_error()
        );
    }
}

/// The handler the system calls, on a thread of its own, for each event.
extern "system" fn on_event(event: u32) -> i32 {
    answer(event, &SHUTDOWN, until_the_exit)
}

/// What a close or the system's shutdown holds its handler to: the
/// syncer's `process::exit`, which ends this thread with the process.
fn until_the_exit() {
    loop {
        std::thread::park();
    }
}

/// The answer to `event`: the flag stored where the event stops the
/// server, and `hold` run after it where the handler's return would let
/// the system end the process at once.
///
/// - Ctrl+C and Ctrl+Break have no timeout: the flag, and the event taken.
///   Ctrl+Break is the one event a process can send another, to a process
///   group (`GenerateConsoleCtrlEvent`), so it is a supervisor's `SIGTERM`
///   on Windows and the tests'.
/// - The console's close (its window shut, *End Task*) and the system's
///   shutdown end the process once the handler returns, and wait for it
///   5 s at most (`SPI_GETHUNGAPPTIMEOUT`): the flag, then held until the
///   exit, so the final sync and the checkpoint have those seconds. A
///   checkpoint cut short leaves the file whole, as a failed rewrite does.
/// - A logoff reaches services alone, at any user's logoff: passed on, so
///   a server run as a service outlives it -- as the JVM's handler passes
///   it on in a session with no desktop. An interactive console's process
///   is ended at logoff with its console, which is a close.
fn answer(event: u32, flag: &AtomicBool, hold: impl FnOnce()) -> i32 {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            flag.store(true, Ordering::SeqCst);
            TAKEN
        }
        CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT => {
            flag.store(true, Ordering::SeqCst);
            hold();
            TAKEN
        }
        CTRL_LOGOFF_EVENT => PASSED,
        _ => PASSED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    /// Long enough for a loaded runner; a thread's step is waited for, not
    /// slept through.
    const WITHIN: Duration = Duration::from_secs(30);

    /// Ctrl+C and Ctrl+Break store the flag and are taken at once: the
    /// system waits for neither, and nothing holds them.
    #[test]
    fn ctrl_c_and_ctrl_break_store_the_flag_and_return() {
        for event in [CTRL_C_EVENT, CTRL_BREAK_EVENT] {
            let flag = AtomicBool::new(false);
            let taken = answer(event, &flag, || panic!("event {event} was held"));
            assert_eq!(taken, TAKEN, "event {event}");
            assert!(flag.load(Ordering::SeqCst), "event {event}");
        }
    }

    /// The close and the system's shutdown store the flag first, then hold
    /// the handler until the exit is asked -- returned, the system would end
    /// the process at once, before the final sync -- and are taken after.
    #[test]
    fn the_close_and_the_shutdown_are_held_until_the_exit_is_asked() {
        for event in [CTRL_CLOSE_EVENT, CTRL_SHUTDOWN_EVENT] {
            let flag = Arc::new(AtomicBool::new(false));
            let (held, is_held) = mpsc::channel();
            let (exit, exit_asked) = mpsc::channel::<()>();
            let seen = Arc::clone(&flag);
            let handler = std::thread::spawn(move || {
                let f = Arc::clone(&seen);
                answer(event, &seen, move || {
                    // The flag is stored before the hold: the syncer sees it
                    // while the handler waits.
                    held.send(f.load(Ordering::SeqCst)).unwrap();
                    exit_asked.recv().unwrap();
                })
            });
            let stored = is_held
                .recv_timeout(WITHIN)
                .unwrap_or_else(|_| panic!("event {event} was never held"));
            assert!(stored, "event {event} held before its flag was stored");
            assert!(!handler.is_finished(), "event {event} returned unheld");
            exit.send(()).unwrap();
            assert_eq!(handler.join().unwrap(), TAKEN, "event {event}");
            assert!(flag.load(Ordering::SeqCst));
        }
    }

    /// A logoff stops nothing and is passed on: it reaches a service at any
    /// user's logoff. So is an event this build does not know.
    #[test]
    fn a_logoff_is_passed_on_and_stops_nothing() {
        for event in [CTRL_LOGOFF_EVENT, 3, 4, 7] {
            let flag = AtomicBool::new(false);
            let passed = answer(event, &flag, || panic!("event {event} was held"));
            assert_eq!(passed, PASSED, "event {event}");
            assert!(!flag.load(Ordering::SeqCst), "event {event}");
        }
    }
}
