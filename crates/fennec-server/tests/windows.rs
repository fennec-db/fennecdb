//! What only Windows settles for the server (docs/design/windows.md,
//! slice 3): whether the harness shares a console with its servers, which
//! Ctrl+Break to a server's process group asks; whether Ctrl+C sent to
//! that group reaches a server, which starts with it off there; whether
//! the server loads a library that would keep the system's shutdown from
//! its handler; and whether listening on a socket again changes its queue
//! (`fennec_http::accept::widen`). Each prints what it found, so a CI log
//! says it, and asserts it, so a Windows update that changes it fails here
//! first.

#![cfg(windows)]

use crate::support::{self, documents, start, tmp};
use std::ffi::c_void;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

#[link(name = "kernel32")]
extern "system" {
    fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
    fn K32EnumProcessModules(
        process: *mut c_void,
        modules: *mut *mut c_void,
        bytes: u32,
        needed: *mut u32,
    ) -> i32;
    fn K32GetModuleBaseNameW(
        process: *mut c_void,
        module: *mut c_void,
        name: *mut u16,
        len: u32,
    ) -> u32;
}

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

/// Ctrl+C sent to a server's process group stops it through the one
/// shutdown, its writes synced. `CREATE_NEW_PROCESS_GROUP`, which the
/// harness starts every server with, turns Ctrl+C off in the group it
/// makes, and adding a handler does not turn it back on: the server does,
/// as it takes the console's events (`SetConsoleCtrlHandler(NULL, FALSE)`).
/// `GenerateConsoleCtrlEvent`'s page says a Ctrl+C sent to a group is
/// received by none of it; this asserts what the system does.
#[test]
fn ctrl_c_to_a_servers_group_stops_it_through_the_one_shutdown() {
    let path = tmp("windows", "ctrl-c.fennec");
    let mut server = start(&["--file", path.to_str().unwrap(), "--sync", "off"]);
    let mut c = server.http();
    c.run("create collection t (name text)");
    c.run(r#"put t {name: "one"}"#);
    // SAFETY: no pointers; CTRL_C_EVENT to the server's own group.
    let sent = unsafe { GenerateConsoleCtrlEvent(0, server.child.id()) };
    assert_ne!(sent, 0, "{}", std::io::Error::last_os_error());
    // What must happen, bounded: a server Ctrl+C missed is stopped by
    // Ctrl+Break, and its log said.
    let Some((code, log)) = server.exited(Duration::from_secs(60)) else {
        let (_, log) = server.terminate();
        panic!("Ctrl+C to the server's group did not stop it:\n{log}");
    };
    println!("Ctrl+C to a server's group stopped it: exit {code}");
    assert_eq!(code, 0, "{log}");
    assert!(
        log.contains("shutting down: writes were pushed to disk"),
        "{log}"
    );
    assert_eq!(documents(&path, "t"), 1, "{log}");
}

/// The server loads neither user32.dll nor gdi32.dll. The system takes a
/// console process that loads one for a window's, and calls its handler no
/// more for a logoff or its shutdown (`SetConsoleCtrlHandler`'s remarks):
/// the shutdown's arm would never run. So the logoff's answer asks the
/// session, kernel32's, not the window station, user32's.
#[test]
fn the_server_loads_no_window_library() {
    use std::os::windows::io::AsRawHandle;
    let path = tmp("windows", "modules.fennec");
    let server = start(&["--file", path.to_str().unwrap()]);
    let process = server.child.as_raw_handle();
    let size = std::mem::size_of::<*mut c_void>();
    let mut modules = vec![std::ptr::null_mut::<c_void>(); 256];
    loop {
        let mut needed = 0u32;
        // SAFETY: the child's own handle, open while `server` holds it, and
        // a buffer of `modules.len()` handles.
        let listed = unsafe {
            K32EnumProcessModules(
                process,
                modules.as_mut_ptr(),
                (modules.len() * size) as u32,
                &mut needed,
            )
        };
        assert_ne!(listed, 0, "{}", std::io::Error::last_os_error());
        let n = needed as usize / size;
        if n <= modules.len() {
            modules.truncate(n);
            break;
        }
        modules.resize(n, std::ptr::null_mut());
    }
    let names: Vec<String> = modules
        .iter()
        .map(|&m| {
            let mut name = [0u16; 260];
            // SAFETY: a module of the child's, and a buffer of 260 units.
            let n = unsafe { K32GetModuleBaseNameW(process, m, name.as_mut_ptr(), 260) };
            String::from_utf16_lossy(&name[..n as usize]).to_ascii_lowercase()
        })
        .collect();
    println!("the server's modules: {}", names.join(", "));
    for gui in ["user32.dll", "gdi32.dll"] {
        assert!(!names.iter().any(|n| n == gui), "the server loads {gui}");
    }
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
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
