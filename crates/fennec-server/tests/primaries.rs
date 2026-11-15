//! A tenant node's copy following a stand-in that answers at its primary's
//! address with bytes no primary of this code sends (G14, area D): a count
//! of writes past its message, a length of a terabyte, a status line with
//! no end, a primary gone silent in the middle of a message. After each
//! the node is whole: the tenant's reads are answered, the admin's
//! promotion of it completes, and the node stops on SIGTERM within its
//! usual bound. The real binary: a refused allocation aborts the process,
//! and the one shutdown is the process's.

use crate::support::{self, start, tmp, Http};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const ADMIN: &str = "adm";
const REPL: &str = "replication-secret";

/// How long a step may take: a follower's back-off is 5 s at most, its
/// silence 10 s.
const WITHIN: Duration = Duration::from_secs(30);

const ANSWER: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\
Cache-Control: no-cache, no-transform\r\nConnection: close\r\n\r\n";

/// A stand-in primary: each connection's request read to the end of its
/// head, its number said on the channel, then `serve` handed it.
fn stand_in(
    serve: impl Fn(usize, TcpStream) + Send + Sync + 'static,
) -> (String, mpsc::Receiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let serve = Arc::new(serve);
    std::thread::spawn(move || {
        for (n, s) in listener.incoming().enumerate() {
            let Ok(mut s) = s else { return };
            let (serve, tx) = (Arc::clone(&serve), tx.clone());
            std::thread::spawn(move || {
                s.set_read_timeout(Some(WITHIN)).unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match s.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                let _ = tx.send(n);
                serve(n, s);
            });
        }
    });
    (url, rx)
}

fn message(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&(body.len() as u64).to_le_bytes());
    m.extend_from_slice(body);
    m
}

fn announced(tag: u8, len: u64) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&len.to_le_bytes());
    m
}

/// A hello at change 0, no image to follow, of a lineage of one history.
fn hello() -> Vec<u8> {
    let mut b = vec![1, 0];
    b.extend_from_slice(&[0u8; 24]);
    // The history: not following, one entry -- an id, the change it began.
    b.extend_from_slice(&[0, 1]);
    b.extend_from_slice(&((7u64 << 32) | 1).to_le_bytes());
    b.push(0);
    message(b'H', &b)
}

/// The tenant `acme` of a node, holding `c {x: 1}`, made to follow the
/// stand-in `serve` is; once the stand-in has served `connections` -- and
/// said it wrote, where it says so on `written` -- the tenant is read,
/// promoted and the node stopped, each within its bound.
fn held_against(
    tag: &str,
    connections: usize,
    written: Option<mpsc::Receiver<()>>,
    serve: impl Fn(usize, TcpStream) + Send + Sync + 'static,
) {
    let (url, served) = stand_in(serve);
    let dir = tmp("primaries", tag);
    std::fs::create_dir_all(&dir).unwrap();
    let mut node = start(&[
        "--admin-token",
        ADMIN,
        "--replication-token",
        REPL,
        "--dir",
        dir.to_str().unwrap(),
    ]);
    let mut admin = node.http().with_token(ADMIN);
    let made = admin.ask("PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    let mut c = node.http();
    for q in ["create collection c (x int)", "put c {x: 1}"] {
        c.query_at("/t/acme", q)
            .unwrap_or_else(|e| panic!("{q}: {e:?}"));
    }
    let from = format!(r#"{{"from":"{url}"}}"#);
    let followed = admin.ask("POST", "/_admin/tenants/acme/follow", &from);
    assert_eq!(followed.status, 200, "{}", followed.body);
    let deadline = Instant::now() + WITHIN;
    let mut seen = 0;
    while seen < connections {
        let left = deadline.saturating_duration_since(Instant::now());
        match served.recv_timeout(left) {
            Ok(_) => seen += 1,
            Err(_) => panic!(
                "{tag}: the follower came {seen} times of {connections} within {WITHIN:?}:\n{}",
                node.log.lock().unwrap()
            ),
        }
    }
    if let Some(written) = written {
        written
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("{tag}: the stand-in never wrote"));
    }

    let read = Http::open(node.port).query_at("/t/acme", "get c count");
    let read = read.unwrap_or_else(|e| panic!("{tag}: the tenant's read: {e:?}"));
    assert!(read.contains(r#""count":1"#), "{tag}: {read}");
    let promoted = Http::open(node.port).with_token(ADMIN).try_ask(
        "POST",
        "/_admin/tenants/acme/promote",
        "",
        &[],
    );
    let promoted = promoted.unwrap_or_else(|| {
        panic!(
            "{tag}: the promotion did not answer within 30 s:\n{}",
            node.log.lock().unwrap()
        )
    });
    assert_eq!(promoted.status, 200, "{tag}: {}", promoted.body);

    support::ask_to_stop(&mut node.child);
    let Some((code, log)) = node.exited(Duration::from_secs(10)) else {
        panic!("{tag}: still running 10 s after SIGTERM");
    };
    assert_eq!(code, 0, "{tag}:\n{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2^61 writes counted in a `W` message: the follower's thread died of the
/// panic, never coming back, and the promotion's halt waited for it.
#[test]
fn a_count_of_writes_past_its_message() {
    held_against("wraps", 2, None, |_, mut s| {
        let mut w = 1u64.to_le_bytes().to_vec();
        w.extend_from_slice(&(1u64 << 61).to_le_bytes());
        w.extend_from_slice(&[0u8; 16]);
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello());
        out.extend_from_slice(&message(b'W', &w));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
}

/// A write announced at a terabyte, a mebibyte of it sent: allocated whole
/// before a byte of it came, which Linux refuses and a refused allocation
/// aborts the node, every tenant with it, its supervisor's restart
/// following the same stream.
#[test]
fn a_terabyte_announced() {
    held_against("terabyte", 2, None, |_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello());
        out.extend_from_slice(&announced(b'W', 1 << 40));
        out.extend_from_slice(&vec![0u8; 1 << 20]);
        let _ = s.write_all(&out);
    });
}

/// A status line that never ends: read on, the follower's memory grew with
/// every byte. The stand-in writes until its writes fail -- the follower
/// gone -- long before the 64 MiB it would write to a reader with no
/// bound.
#[test]
fn a_status_line_without_an_end() {
    let most = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&most);
    held_against("endless", 2, None, move |_, mut s| {
        let chunk = vec![b'a'; 64 << 10];
        let mut total = 0;
        let _ = s.write_all(b"HTTP/1.1 200 ");
        while total < 64 << 20 {
            if s.write_all(&chunk).is_err() {
                break;
            }
            total += chunk.len();
        }
        seen.fetch_max(total, Ordering::SeqCst);
        std::thread::sleep(WITHIN);
    });
    let total = most.load(Ordering::SeqCst);
    assert!(
        total < 16 << 20,
        "the follower read {total} bytes of a head"
    );
}

/// A primary that goes silent in the middle of a message, the follower
/// blocked reading it: the promotion's halt cuts the read, and nothing
/// waits out the silence.
#[test]
fn a_primary_silent_in_the_middle_of_a_message() {
    let (tx, written) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    held_against("silent", 1, Some(written), move |_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello());
        out.extend_from_slice(&announced(b'W', 1000));
        out.extend_from_slice(&[0u8; 10]);
        let _ = s.write_all(&out);
        let _ = tx.lock().unwrap().send(());
        std::thread::sleep(WITHIN);
    });
}
