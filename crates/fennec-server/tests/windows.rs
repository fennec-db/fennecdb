//! What only Windows settles for the server (docs/design/windows.md,
//! slice 3): whether the harness shares a console with its servers, which
//! Ctrl+Break to a server's process group asks, and whether listening on a
//! socket again changes its queue (`fennec_http::accept::widen`). Each
//! prints what it found, so a CI log says it, and asserts it, so a Windows
//! update that changes it fails here first.

#![cfg(windows)]

use crate::support::{self, start, tmp};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// `GenerateConsoleCtrlEvent` sends to the processes of the caller's
/// console alone: a server the harness starts is on the harness's console,
/// in a process group of its own, and its Ctrl+Break stops it alone.
#[test]
fn the_harness_shares_its_console_with_its_servers() {
    let path = tmp("windows", "console.fennec");
    let server = start(&["--file", path.to_str().unwrap()]);
    let had = support::windows::had_one().expect("a server was started");
    println!(
        "the harness had a console of its own: {had}{}",
        if had { "" } else { " (AllocConsole made one)" }
    );
    let attached = support::windows::console_processes();
    assert!(
        attached.contains(&server.child.id()),
        "server {} is not on the harness's console: {attached:?}",
        server.child.id()
    );
    assert!(attached.contains(&std::process::id()), "{attached:?}");
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    assert!(log.contains("shutting down"), "{log}");
}

/// How many of `n` connections, made at once, `listener`'s queue takes
/// while nothing accepts them: each beyond it is refused, or not answered
/// within the bound. Each taken one is held open until all are counted.
fn queued(listener: &TcpListener, n: usize) -> usize {
    let at = listener.local_addr().unwrap();
    let tries: Vec<_> = (0..n)
        .map(|_| {
            std::thread::spawn(move || TcpStream::connect_timeout(&at, Duration::from_secs(5)))
        })
        .collect();
    let made: Vec<_> = tries.into_iter().map(|t| t.join().unwrap()).collect();
    made.iter().filter(|c| c.is_ok()).count()
}

/// Winsock's `listen` on a socket already listening answers success
/// "without changing the value for the backlog parameter", its page says:
/// so `accept::widen`'s second `listen`, which gives a Unix listener a
/// queue of 1 024, is not made on Windows, where a listener keeps the 128
/// std's `bind` asks. Measured: as many connections queue after the second
/// `listen` as before it.
#[test]
fn listening_again_leaves_the_queue_as_it_was() {
    #[link(name = "ws2_32")]
    extern "system" {
        fn listen(socket: usize, backlog: i32) -> i32;
    }
    use std::os::windows::io::AsRawSocket;
    const TRIES: usize = 300;
    let plain = TcpListener::bind("127.0.0.1:0").unwrap();
    let before = queued(&plain, TRIES);
    let again = TcpListener::bind("127.0.0.1:0").unwrap();
    // SAFETY: the listener's own socket, open while it is borrowed.
    let answered = unsafe { listen(again.as_raw_socket() as usize, 1024) };
    let after = queued(&again, TRIES);
    println!(
        "listen again answered {answered}; of {TRIES} connections at once, {before} queued \
         behind std's listen, {after} after a second listen of 1 024"
    );
    assert_eq!(answered, 0, "{}", std::io::Error::last_os_error());
    assert!(before < TRIES, "std's queue took all {TRIES}");
    assert!(
        after < TRIES,
        "a second listen widened the queue: {before} -> {after}"
    );
}
