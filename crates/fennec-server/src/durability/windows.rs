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
use fennec_core::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

/// The events, wincon.h's.
const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_CLOSE_EVENT: u32 = 2;
const CTRL_LOGOFF_EVENT: u32 = 5;
const CTRL_SHUTDOWN_EVENT: u32 = 6;

/// A handler's answers: the event taken, or passed to the next handler.
const TAKEN: i32 = 1;
const PASSED: i32 = 0;

/// Set, before [`SHUTDOWN`], by an event the system ends the process five
/// seconds after: the console's close, a logoff in a user's session, the
/// system's shutdown. The shutdown then syncs before its waits
/// ([`before_the_deadline`]): the held requests' grace (2 s), the TLS
/// connections' (0.5 s) and the hooks -- an OTLP flush of up to 2 s, the
/// mirror's join -- came to 4.7 of the five seconds with tracing on, and
/// under `--sync <ms>` or `off` the writes they were waiting to sync were
/// lost where the system ended the process first.
static DEADLINE: AtomicBool = AtomicBool::new(false);

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
}

/// Takes the console's events (`SetConsoleCtrlHandler`): Ctrl+C,
/// Ctrl+Break, the console's close, a logoff in a user's session and the
/// system's shutdown each start the one shutdown. Installed where Unix
/// installs its signals' handlers, before the line saying the server
/// listens: a supervisor that sends Ctrl+Break as soon as it reads that
/// line would otherwise end the server by the default action, with no
/// final sync.
pub fn install_signal_handlers() {
    // Ctrl+C turned on first. A process started in a group of its own
    // (`CREATE_NEW_PROCESS_GROUP`: a harness's, a wrapper's) or by
    // `start /b` begins with it off, an attribute its children inherit,
    // and adding a handler does not turn it back on: such a server took
    // Ctrl+Break and the close, and a Ctrl+C did nothing.
    // SAFETY: no handler and `FALSE`, Ctrl+C processed as normal; the call
    // keeps no pointer.
    if unsafe { SetConsoleCtrlHandler(None, 0) } == 0 {
        fennec_http::log!(
            "Ctrl+C could not be turned on ({}): Ctrl+Break stops the server",
            std::io::Error::last_os_error()
        );
    }
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
    answer(
        event,
        &SHUTDOWN,
        &DEADLINE,
        in_a_users_session,
        until_the_exit,
    )
}

/// What a close or the system's shutdown holds its handler to: the
/// syncer's `process::exit`, which ends this thread with the process.
fn until_the_exit() {
    loop {
        std::thread::park();
    }
}

/// Whether this process runs in a user's session, one with a desktop: any
/// but session 0, the services' alone since Windows Vista. The JVM asks its
/// window station instead (`WSF_VISIBLE`), but that is user32's to answer
/// (`GetProcessWindowStation`, `GetUserObjectInformationW`), and a process
/// that loads user32.dll or gdi32.dll is no longer called for a logoff or
/// the system's shutdown (`SetConsoleCtrlHandler`'s remarks): the
/// shutdown's arm would never run. A session that cannot be read is taken
/// as the services'.
fn in_a_users_session() -> bool {
    let mut session = 0u32;
    // SAFETY: this process's id, and a `u32` the call writes.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) != 0 && session != 0 }
}

/// The answer to `event`: the flag stored where the event stops the
/// server, and `hold` run after it where the handler's return would let
/// the system end the process at once.
///
/// - Ctrl+C and Ctrl+Break have no timeout: the flag, and the event taken.
///   Ctrl+Break is the one event a process can send another, to a process
///   group (`GenerateConsoleCtrlEvent`), so it is a supervisor's `SIGTERM`
///   on Windows and the tests'.
/// - The console's close (its window shut, *End Task*), a logoff and the
///   system's shutdown end the process once the handler returns, and wait
///   for it 5 s at most (`SPI_GETHUNGAPPTIMEOUT`): `deadline` stored, so
///   the shutdown syncs before its waits, then the flag, then held until
///   the exit, so the syncs and the checkpoint have those seconds. A
///   checkpoint cut short leaves the file whole, as a failed rewrite does.
/// - A logoff reaches a service's session at any user's logoff: there it is
///   passed on, so a server run as a service outlives it, as the JVM passes
///   it on where its window station is not visible. In a user's session
///   (`in_a_users_session`) it is the session's own end, and taken as the
///   close is, as the JVM takes it there.
fn answer(
    event: u32,
    flag: &AtomicBool,
    deadline: &AtomicBool,
    in_a_users_session: impl FnOnce() -> bool,
    hold: impl FnOnce(),
) -> i32 {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            flag.store(true, Ordering::SeqCst);
            TAKEN
        }
        CTRL_LOGOFF_EVENT if !in_a_users_session() => PASSED,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            // Before the flag: the syncer that sees the flag sees this too.
            deadline.store(true, Ordering::SeqCst);
            flag.store(true, Ordering::SeqCst);
            hold();
            TAKEN
        }
        _ => PASSED,
    }
}

/// Runs `sync` where an event the system ends the process five seconds
/// after started the shutdown ([`DEADLINE`]), and nothing otherwise: the
/// shutdown calls it before its waits -- the held requests' and the TLS
/// connections' graces, then the hooks -- so every write acknowledged
/// before them is on disk whatever they take. A `--dir` node passes its
/// tenants' sync; a file's server, [`sync_first`]. Ctrl+C and Ctrl+Break,
/// which no timeout follows, keep the order a signal's shutdown has.
pub fn before_the_deadline(sync: impl FnOnce()) {
    when_set(&DEADLINE, sync);
}

/// `sync` run where `deadline` is set.
fn when_set(deadline: &AtomicBool, sync: impl FnOnce()) {
    if deadline.load(Ordering::SeqCst) {
        sync();
    }
}

/// [`before_the_deadline`] for a server over one file: `db`'s writes
/// synced, as the final sync syncs them.
pub(super) fn sync_first(db: &RwLock<Database>) {
    before_the_deadline(|| sync_now(db));
}

/// `db`'s pending writes pushed to disk and synced, under its write lock,
/// unless a storage error stopped them.
fn sync_now(db: &RwLock<Database>) {
    let mut g = super::write_lock(db);
    if !g.is_dirty() || g.failure().is_some() {
        return;
    }
    match g.sync() {
        Ok(()) => fennec_http::log!("the process ends within 5 s: writes pushed to disk first"),
        Err(e) => fennec_http::log!("sync error: {e}"),
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
    /// system waits for neither, nothing holds them, and the shutdown keeps
    /// a signal's order, no deadline set.
    #[test]
    fn ctrl_c_and_ctrl_break_store_the_flag_and_return() {
        for event in [CTRL_C_EVENT, CTRL_BREAK_EVENT] {
            let (flag, deadline) = (AtomicBool::new(false), AtomicBool::new(false));
            let taken = answer(
                event,
                &flag,
                &deadline,
                || panic!("event {event} asked the session"),
                || panic!("event {event} was held"),
            );
            assert_eq!(taken, TAKEN, "event {event}");
            assert!(flag.load(Ordering::SeqCst), "event {event}");
            assert!(!deadline.load(Ordering::SeqCst), "event {event}");
        }
    }

    /// The close, a user's logoff and the system's shutdown store the
    /// deadline and then the flag, then hold the handler until the exit is
    /// asked -- returned, the system would end the process at once, before
    /// the final sync -- and are taken after.
    #[test]
    fn the_close_a_users_logoff_and_the_shutdown_are_held_until_the_exit_is_asked() {
        for event in [CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
            let flag = Arc::new(AtomicBool::new(false));
            let deadline = Arc::new(AtomicBool::new(false));
            let (held, is_held) = mpsc::channel();
            let (exit, exit_asked) = mpsc::channel::<()>();
            let (seen, seen_deadline) = (Arc::clone(&flag), Arc::clone(&deadline));
            let handler = std::thread::spawn(move || {
                let (f, d) = (Arc::clone(&seen), Arc::clone(&seen_deadline));
                answer(
                    event,
                    &seen,
                    &seen_deadline,
                    || true,
                    move || {
                        // Both are stored before the hold: the syncer sees
                        // them while the handler waits.
                        held.send((f.load(Ordering::SeqCst), d.load(Ordering::SeqCst)))
                            .unwrap();
                        exit_asked.recv().unwrap();
                    },
                )
            });
            let stored = is_held
                .recv_timeout(WITHIN)
                .unwrap_or_else(|_| panic!("event {event} was never held"));
            assert_eq!(stored, (true, true), "event {event} held before its flags");
            assert!(!handler.is_finished(), "event {event} returned unheld");
            exit.send(()).unwrap();
            assert_eq!(handler.join().unwrap(), TAKEN, "event {event}");
            assert!(flag.load(Ordering::SeqCst));
            assert!(deadline.load(Ordering::SeqCst));
        }
    }

    /// A logoff in the services' session stops nothing and is passed on: it
    /// reaches a service at any user's logoff. So is an event this build
    /// does not know.
    #[test]
    fn a_services_logoff_is_passed_on_and_stops_nothing() {
        for event in [CTRL_LOGOFF_EVENT, 3, 4, 7] {
            let (flag, deadline) = (AtomicBool::new(false), AtomicBool::new(false));
            let passed = answer(
                event,
                &flag,
                &deadline,
                || false,
                || panic!("event {event} was held"),
            );
            assert_eq!(passed, PASSED, "event {event}");
            assert!(!flag.load(Ordering::SeqCst), "event {event}");
            assert!(!deadline.load(Ordering::SeqCst), "event {event}");
        }
    }

    /// The session this process runs in, read: what a logoff's answer turns
    /// on, printed so a CI log says which a runner's step is.
    #[test]
    fn the_session_is_read() {
        let mut session = u32::MAX;
        // SAFETY: as `in_a_users_session`.
        let read = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) };
        assert_ne!(read, 0, "{}", std::io::Error::last_os_error());
        println!("session {session}: a user's: {}", in_a_users_session());
        assert_eq!(in_a_users_session(), session != 0);
    }

    /// A file's pending writes are synced first only past a deadline: a
    /// Ctrl+C's shutdown leaves them to its final sync, in a signal's order.
    #[test]
    fn writes_are_synced_first_only_past_a_deadline() {
        let dir = std::env::temp_dir().join(format!("fennec-sync-first-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("first.fennec");
        let _ = std::fs::remove_file(&path);
        let db = RwLock::new(fennec_core::fs::open_serving(&path, false, Box::new(Ok)).unwrap());
        for q in ["create collection t (n int)", "put t {n: 1}"] {
            let s = fennec_ql::parse_exact(q).unwrap().remove(0);
            db.write().unwrap().execute(&s).unwrap();
        }
        assert!(db.read().unwrap().is_dirty());
        when_set(&AtomicBool::new(false), || sync_now(&db));
        assert!(db.read().unwrap().is_dirty(), "synced with no deadline");
        when_set(&AtomicBool::new(true), || sync_now(&db));
        assert!(
            !db.read().unwrap().is_dirty(),
            "not synced past the deadline"
        );
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
