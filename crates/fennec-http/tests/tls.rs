//! HTTPS served by the listener itself (`Config::tls`, `--tls-cert`):
//! `/query` and `/batch` over one kept-alive connection, a long body each
//! way (chunked too), a stream of changes, a held write; and what is not
//! served -- a handshake stalled or left unfinished, a plain request, a
//! connection past the ceiling or with no thread -- holding up no other.
//! The private CA and the server's certificate are
//! `tests/certs/` (written by fennec-tls's `http_certs`).
#![cfg(feature = "tls")]

use fennec_core::engine::Database;
use fennec_http::conn::Tls;
use fennec_http::{Config, Server};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs")
}

fn tls() -> Tls {
    Tls::from_pem_files(&certs().join("server.pem"), &certs().join("server.key")).unwrap()
}

/// A client trusting the test CA alone, offering `http/1.1`.
fn client() -> fennec_tls::Client {
    fennec_tls::Client::trusting_pem_file(&certs().join("ca.pem"))
        .unwrap()
        .alpn(&[b"http/1.1"])
}

fn start(tune: impl FnOnce(&mut Config)) -> (u16, Arc<Server>) {
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        tls: Some(Arc::new(tls())),
        ..Config::default()
    };
    tune(&mut cfg);
    let server = Arc::new(Server::new(Arc::new(RwLock::new(Database::new())), cfg));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = serving.serve_on(listener);
    });
    (port, server)
}

fn dial(port: u16) -> Result<fennec_tls::TlsStream, fennec_tls::Error> {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    client().dial(s, "localhost")
}

struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One answer read off `r`: its head, and its body by `Content-Length`.
fn answer(r: &mut impl BufRead) -> Answer {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert!(
            r.read_line(&mut line).unwrap() > 0,
            "closed in a head: {head}"
        );
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .map_or(0, |n| n.trim().parse().unwrap());
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    Answer { status, head, body }
}

fn request(path: &str, body: &[u8], headers: &[(&str, &str)]) -> Vec<u8> {
    let mut out = format!(
        "POST {path} HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    out
}

fn query(q: &str) -> Vec<u8> {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    body.into_bytes()
}

/// A request and its answer on a kept-alive connection.
fn ask(r: &mut BufReader<fennec_tls::TlsStream>, path: &str, body: &[u8]) -> Answer {
    let w = r.get_mut();
    w.write_all(&request(path, body, &[])).unwrap();
    w.flush().unwrap();
    answer(r)
}

#[test]
fn query_and_batch_go_over_one_kept_alive_connection() {
    let (port, _server) = start(|_| {});
    let s = dial(port).unwrap();
    assert_eq!(s.alpn_protocol(), Some(&b"http/1.1"[..]));
    let mut r = BufReader::new(s);
    for q in [
        "create collection notes (t text)",
        "put notes {id: 1, t: 'one'}",
        "get notes select t where id = 1",
    ] {
        let a = ask(&mut r, "/query", &query(q));
        assert_eq!(a.status, 200, "{q}: {}", a.text());
        assert!(a.head.contains("Connection: keep-alive"), "{}", a.head);
        assert!(a.head.contains("X-Request-Id: "), "{}", a.head);
    }
    let batch = b"{\"query\":\"put notes {id: 2, t: 'two'}\"}\n{\"query\":\"get notes select t order id\"}\n";
    let a = ask(&mut r, "/batch", batch);
    assert_eq!(a.status, 200, "{}", a.text());
    assert!(a.text().contains("\"two\""), "{}", a.text());
    let a = ask(&mut r, "/query", &query("get notes select t where id = 1"));
    assert!(a.text().contains("\"one\""), "{}", a.text());
}

/// A body past a record's 16 KiB each way, by its length and in chunks:
/// read whole, and answered whole, however the records split it.
#[test]
fn a_long_body_goes_both_ways() {
    let (port, _server) = start(|_| {});
    let mut r = BufReader::new(dial(port).unwrap());
    let a = ask(&mut r, "/query", &query("create collection notes (t text)"));
    assert_eq!(a.status, 200, "{}", a.text());
    let long: String = (0..300_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let put = query(&format!("put notes {{id: 1, t: '{long}'}}"));
    let a = ask(&mut r, "/query", &put);
    assert_eq!(a.status, 200, "{}", a.text());

    // Chunked, in pieces a record splits.
    let put = query(&format!("put notes {{id: 2, t: '{long}'}}"));
    let mut req = b"POST /query HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for c in put.chunks(10_000) {
        req.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
        req.extend_from_slice(c);
        req.extend_from_slice(b"\r\n");
    }
    req.extend_from_slice(b"0\r\n\r\n");
    r.get_mut().write_all(&req).unwrap();
    r.get_mut().flush().unwrap();
    let a = answer(&mut r);
    assert_eq!(a.status, 200, "{}", a.text());

    let a = ask(&mut r, "/query", &query("get notes select t order id"));
    assert_eq!(a.status, 200);
    assert!(a.body.len() > 600_000, "{}", a.body.len());
    assert_eq!(a.text().matches(&long).count(), 2);
}

/// A collection's changes stream over TLS: the seed, then a change as it
/// lands, each event flushed as it is written.
#[test]
fn changes_stream_over_tls() {
    let (port, _server) = start(|_| {});
    let mut r = BufReader::new(dial(port).unwrap());
    assert_eq!(
        ask(&mut r, "/query", &query("create collection notes (t text)")).status,
        200
    );

    let mut s = BufReader::new(dial(port).unwrap());
    s.get_mut()
        .write_all(b"GET /notes/changes HTTP/1.1\r\nHost: t\r\n\r\n")
        .unwrap();
    s.get_mut().flush().unwrap();
    let mut events = String::new();
    let mut line = String::new();
    while !events.contains("event: seed") {
        line.clear();
        assert!(s.read_line(&mut line).unwrap() > 0, "{events}");
        events.push_str(&line);
    }
    assert!(events.starts_with("HTTP/1.1 200"), "{events}");
    assert!(events.contains("text/event-stream"), "{events}");
    assert_eq!(
        ask(&mut r, "/query", &query("put notes {id: 7, t: 'seven'}")).status,
        200
    );
    while !events.contains("event: change") || !events.contains("seven") {
        line.clear();
        assert!(s.read_line(&mut line).unwrap() > 0, "{events}");
        events.push_str(&line);
    }
}

/// A write held by `Fennec-Wait` answers over TLS once another connection's
/// write lets it.
#[test]
fn a_held_write_answers_over_tls() {
    let (port, _server) = start(|_| {});
    let mut r = BufReader::new(dial(port).unwrap());
    assert_eq!(
        ask(&mut r, "/query", &query("create collection jobs (n int)")).status,
        200
    );
    let held = std::thread::spawn(move || {
        let mut h = BufReader::new(dial(port).unwrap());
        let req = request(
            "/query",
            &query("del jobs where n = 1 returning id"),
            &[("Fennec-Wait", "20000")],
        );
        h.get_mut().write_all(&req).unwrap();
        h.get_mut().flush().unwrap();
        answer(&mut h)
    });
    // A request held, before the row it waits for lands. The count is the
    // process's, another test's among it: the answer holds either way, a
    // `del` finding its row at once if it came after.
    let started = Instant::now();
    while fennec_http::waits::totals().held == 0 {
        assert!(started.elapsed() < Duration::from_secs(20), "never held");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        ask(&mut r, "/query", &query("put jobs {id: 5, n: 1}")).status,
        200
    );
    let a = held.join().unwrap();
    assert_eq!(a.status, 200, "{}", a.text());
    assert!(a.text().contains("\"id\":5"), "{}", a.text());
}

/// A client that connected and sends nothing, and one that began a
/// handshake and stalled, hold up no other: each handshake is its
/// connection's own thread's, and a good client is answered meanwhile.
#[test]
fn a_stalled_handshake_holds_up_no_other() {
    let (port, _server) = start(|_| {});
    let _silent = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut stalled = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stalled
        .write_all(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01])
        .unwrap();
    let mut r = BufReader::new(dial(port).unwrap());
    let a = ask(&mut r, "/query", &query("create collection x (n int)"));
    assert_eq!(a.status, 200, "{}", a.text());
}

/// A handshake not done within the bound has its connection closed then,
/// and not before.
#[test]
fn a_handshake_past_its_bound_is_closed() {
    let within = Duration::from_millis(400);
    let (port, _server) = start(|c| {
        c.tls = Some(Arc::new(tls().handshake_within(within)));
    });
    let mut stalled = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stalled
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let started = Instant::now();
    stalled
        .write_all(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01])
        .unwrap();
    let mut rest = Vec::new();
    let ended = stalled.read_to_end(&mut rest);
    let took = started.elapsed();
    // Ended by the server (its end, or a reset for the bytes it left
    // unread), never by this read's own 30 s.
    if let Err(e) = &ended {
        assert_ne!(e.kind(), std::io::ErrorKind::WouldBlock, "{e}");
    }
    assert!(took >= within, "{took:?}");
    assert!(took < Duration::from_secs(20), "{took:?}");
}

/// Plain HTTP to the HTTPS port is answered 400 in the clear, naming the
/// scheme, and the connection ended after it -- read whole, no reset.
#[test]
fn plain_http_is_told_to_ask_over_https() {
    let (port, _server) = start(|_| {});
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(b"GET /_health HTTP/1.1\r\nHost: t\r\n\r\n")
        .unwrap();
    let mut got = String::new();
    s.read_to_string(&mut got).unwrap();
    assert!(got.starts_with("HTTP/1.1 400 "), "{got}");
    assert!(got.contains(fennec_http::conn::PLAIN_TO_TLS), "{got}");
    assert!(got.contains("Connection: close"), "{got}");
}

/// A connection no thread can be made for is told the alert, its handshake
/// never run on the accepting thread; the next is served.
#[test]
fn a_connection_with_no_thread_is_told_an_alert() {
    let (port, server) = start(|_| {});
    server.fail_spawns().next(1);
    let e = dial(port).unwrap_err();
    assert!(e.to_string().contains("InternalError"), "{e}");
    let mut r = BufReader::new(dial(port).unwrap());
    assert_eq!(
        ask(&mut r, "/query", &query("create collection x (n int)")).status,
        200
    );
}

/// Past `--max-connections` too: the alert, and a connection let go of is
/// room for the next.
#[test]
fn past_the_ceiling_is_told_an_alert() {
    let (port, _server) = start(|c| c.max_connections = 1);
    let held = dial(port).unwrap();
    let e = dial(port).unwrap_err();
    assert!(e.to_string().contains("InternalError"), "{e}");
    drop(held);
    let started = Instant::now();
    let mut r = loop {
        match dial(port) {
            Ok(s) => break BufReader::new(s),
            Err(e) => {
                assert!(started.elapsed() < Duration::from_secs(20), "{e}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    assert_eq!(
        ask(&mut r, "/query", &query("create collection x (n int)")).status,
        200
    );
}
