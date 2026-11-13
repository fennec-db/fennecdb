//! Slow clients of the router, held as a node holds its own
//! (fennec-http's `tests/server.rs`): a head or a body trickled is cut at
//! the router's `--header-timeout` with a 408, and an answer never read
//! ends its connection at `--write-timeout`; each gives its slot back. A
//! client sending a byte a while, or reading nothing, held a router thread
//! and a `--max-connections` slot as long as it liked.

use fennec_http::tenants::Tenants;
use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::{Config, Router};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A router of one slot in front of a node holding tenant `acme`; the
/// node's directory removed on drop.
struct Front {
    port: u16,
    dir: PathBuf,
}

impl Drop for Front {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn front(tag: &str, tune: impl FnOnce(&mut Config)) -> Front {
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-shard-slow-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    tenants.create("acme").unwrap();
    let node = fennec_http::Server::with_tenants(
        tenants,
        fennec_http::Config {
            addr: "127.0.0.1:0".into(),
            admin_token: Some("adm".into()),
            ..fennec_http::Config::default()
        },
    );
    let listener = node.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = node.serve_on(listener);
    });
    let mut d = Directory::in_memory();
    let placed = Node {
        addr,
        token: "adm".into(),
        ..Node::default()
    };
    d.set_node("n1", placed).unwrap();
    d.place("acme", "n1", State::Active).unwrap();
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        max_connections: 1,
        upstream_timeout: Duration::from_secs(30),
        ..Config::default()
    };
    tune(&mut cfg);
    let router = Router::new(d, cfg);
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    Front { port, dir }
}

/// One request on a connection of its own: its status and its answer.
fn ask(port: u16, method: &str, target: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let req = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, out)
}

/// The router's one slot given back: a tenant's request answered 200, each
/// refused one (503) asked again, 30 s at most.
fn served_again(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match ask(port, "POST", "/t/acme/query", r#"{"query":"collections"}"#) {
            (200, _) => return,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "the slot was never given back: {other:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// A request whose first `sent` bytes go at once, then `trickled` a byte
/// every `every`, until the router answers and ends the connection: its
/// answer, and how long after the first byte the end came. The trickle
/// stops at the answer or after 30 s, a bound on a router that never cuts.
fn trickled(port: u16, sent: &[u8], trickled: Vec<u8>, every: Duration) -> (String, Duration) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let started = Instant::now();
    s.write_all(sent).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (mut w, stopped) = (s.try_clone().unwrap(), Arc::clone(&stop));
    let trickle = std::thread::spawn(move || {
        for b in trickled {
            std::thread::sleep(every);
            if stopped.load(Ordering::SeqCst)
                || started.elapsed() > Duration::from_secs(30)
                || w.write_all(&[b]).is_err()
            {
                return;
            }
        }
    });
    let mut answer = Vec::new();
    let _ = s.read_to_end(&mut answer);
    let took = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    trickle.join().unwrap();
    (String::from_utf8_lossy(&answer).into_owned(), took)
}

#[test]
fn a_trickled_head_is_cut_at_its_deadline() {
    let within = Duration::from_millis(600);
    let f = front("head", |c| c.header_timeout = Some(within));
    let header = b"X-Trickled: ".iter().copied().chain([b'a'; 600]).collect();
    let (answer, took) = trickled(
        f.port,
        b"POST /t/acme/query HTTP/1.1\r\nHost: localhost\r\n",
        header,
        Duration::from_millis(50),
    );
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(took >= within, "{took:?}");
    served_again(f.port);
}

#[test]
fn a_trickled_body_is_cut() {
    use fennec_http::conn::{BODY_RATE, FREE};
    let within = Duration::from_millis(600);
    let f = front("body", |c| c.header_timeout = Some(within));
    let every = Duration::from_millis(50);
    let declared =
        b"POST /t/acme/query HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048576\r\n\r\n";
    let (answer, took) = trickled(f.port, declared, vec![b' '; 600], every);
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(took >= within, "{took:?}");
    served_again(f.port);

    // Past the first 64 KiB by one KiB, at once: a second more earned.
    let mut sent = declared.to_vec();
    sent.resize(sent.len() + FREE as usize + BODY_RATE as usize, b' ');
    let (answer, took) = trickled(f.port, &sent, vec![b' '; 600], every);
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(took >= within + Duration::from_secs(1), "{took:?}");
    served_again(f.port);
}

#[test]
fn an_unread_answer_frees_its_slot() {
    let write_within = Duration::from_millis(300);
    let f = front("unread", |c| c.write_timeout = Some(write_within));
    let made = ask(
        f.port,
        "POST",
        "/t/acme/query",
        r#"{"query":"create collection t (s text)"}"#,
    );
    assert_eq!(made.0, 200, "{}", made.1);
    let row = format!(
        r#"{{"query":"put t {{id: 1, s: '{}'}}"}}"#,
        "x".repeat(1 << 20)
    );
    let put = ask(f.port, "POST", "/t/acme/query", &row);
    assert_eq!(put.0, 200, "{}", put.1);
    // Forty answers of a mebibyte asked at once, and none read.
    let mut s = TcpStream::connect(("127.0.0.1", f.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let started = Instant::now();
    s.write_all(&b"GET /t/acme/t/1 HTTP/1.1\r\nHost: localhost\r\n\r\n".repeat(40))
        .unwrap();
    served_again(f.port);
    assert!(started.elapsed() >= write_within, "{:?}", started.elapsed());
    // The unread client's connection was ended: what was sent is read off,
    // then its end.
    let mut rest = Vec::new();
    let read = s.read_to_end(&mut rest);
    assert!(
        !matches!(&read, Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)),
        "{read:?}"
    );
}
