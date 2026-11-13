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
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n",
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
    let mut req =
        b"POST /query HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
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
        .write_all(b"GET /notes/changes HTTP/1.1\r\nHost: localhost\r\n\r\n")
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

/// A head trickled over TLS, a record a byte, each well inside the idle
/// timeout, is cut at `--header-timeout` as a plain one is: its 408 read
/// over TLS. A record coming in parts is the test below's.
#[test]
fn a_trickled_head_over_tls_is_cut_at_its_deadline() {
    let within = Duration::from_millis(600);
    let (port, _server) = start(|c| c.header_timeout = Some(within));
    let mut s = dial(port).unwrap();
    let started = Instant::now();
    s.write_all(b"GET /collections HTTP/1.1\r\nHost: localhost\r\nX-Trickled: ")
        .unwrap();
    s.flush().unwrap();
    // A byte a record every 50 ms, until the answer comes, or 30 s.
    while started.elapsed() < Duration::from_secs(30) {
        if s.write_all(b"a").and_then(|()| s.flush()).is_err() {
            break;
        }
        match s.readable(Duration::from_millis(50)) {
            Ok(false) => {}
            Ok(true) | Err(_) => break,
        }
    }
    let took = started.elapsed();
    let mut got = Vec::new();
    let _ = s.read_to_end(&mut got);
    let got = String::from_utf8_lossy(&got);
    assert!(got.starts_with("HTTP/1.1 408"), "{got}");
    assert!(took >= within, "{took:?}");
    assert!(took < Duration::from_secs(20), "{took:?}");
}

/// A [`proxy`]'s gate: what the client sends passed on at once.
const AT_ONCE: usize = usize::MAX;
/// A [`proxy`]'s gate: a byte every 50 ms.
const TRICKLED: usize = usize::MAX - 1;

/// Between a client and the server at `port`, a proxy passing on what the
/// client sends as `gate` says -- [`AT_ONCE`], [`TRICKLED`], or a count:
/// that many bytes at once, then nothing until the gate changes -- and
/// all the server sends at once, its end too. Its port.
fn proxy(port: u16, gate: Arc<std::sync::atomic::AtomicUsize>) -> u16 {
    use std::net::TcpListener;
    use std::sync::atomic::Ordering::SeqCst;
    let front = TcpListener::bind("127.0.0.1:0").unwrap();
    let front_port = front.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut from, _) = front.accept().unwrap();
        let mut to = TcpStream::connect(("127.0.0.1", port)).unwrap();
        to.set_nodelay(true).unwrap();
        let (mut back_from, mut back_to) = (to.try_clone().unwrap(), from.try_clone().unwrap());
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut back_from, &mut back_to);
            let _ = back_to.shutdown(std::net::Shutdown::Write);
        });
        let mut buf = [0u8; 4096];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut i = 0;
            while i < n {
                let passed = match gate.load(SeqCst) {
                    AT_ONCE => to.write_all(&buf[i..n]).map(|()| n - i),
                    TRICKLED => {
                        std::thread::sleep(Duration::from_millis(50));
                        to.write_all(&buf[i..=i]).map(|()| 1)
                    }
                    0 => {
                        std::thread::sleep(Duration::from_millis(5));
                        Ok(0)
                    }
                    // Taken from the count only where the test did not
                    // change the gate meanwhile.
                    g => match gate.compare_exchange(g, g - g.min(n - i), SeqCst, SeqCst) {
                        Ok(_) => {
                            let k = g.min(n - i);
                            to.write_all(&buf[i..i + k]).map(|()| k)
                        }
                        Err(_) => Ok(0),
                    },
                };
                match passed {
                    Ok(k) => i += k,
                    Err(_) => return,
                }
            }
        }
    });
    front_port
}

/// A connection to the server at `port` through a [`proxy`] whose gate is
/// `gate`, a first request answered over it: the handshake's last flight,
/// the client's, is through the proxy before the test closes the gate.
/// `dial` returns once the client is done with the handshake, and a gate
/// closed then held the client's `Finished`, the server's handshake ending
/// as the gate opened -- a first record that came whole after it.
fn handshaken(
    port: u16,
    gate: &Arc<std::sync::atomic::AtomicUsize>,
) -> BufReader<fennec_tls::TlsStream> {
    let mut r = BufReader::new(dial(proxy(port, Arc::clone(gate))).unwrap());
    let a = ask(&mut r, "/query", &query("collections"));
    assert_eq!(a.status, 200, "{}", a.text());
    r
}

/// A request's record trickled in parts, a byte every 50 ms through a
/// proxy, each part well inside the idle timeout, is cut at
/// `--header-timeout` from its first part: rustls reads the socket again
/// inside one read until a record is whole, so a timeout set a read
/// bounded each part, never the record, and one read lasted as long as the
/// bytes came (`fennec_tls::TlsStream::read_by`). No byte of the request
/// was read: the connection ends with nothing written, as a silence before
/// a request does.
#[test]
fn a_record_trickled_in_parts_is_cut_at_the_requests_time() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let within = Duration::from_millis(600);
    let (port, _server) = start(|c| c.header_timeout = Some(within));
    let gate = Arc::new(AtomicUsize::new(AT_ONCE));
    let mut r = handshaken(port, &gate);
    let s = r.get_mut();
    gate.store(TRICKLED, SeqCst);
    let started = Instant::now();
    // One record of some 120 bytes: six seconds at a byte every 50 ms.
    s.write_all(&request("/query", &query("collections"), &[]))
        .unwrap();
    s.flush().unwrap();
    let mut got = Vec::new();
    let ended = s.read_to_end(&mut got);
    let took = started.elapsed();
    gate.store(AT_ONCE, SeqCst);
    // Ended by the server -- its `close_notify`, its end, or a reset for
    // the bytes it left unread -- never by this read's own 30 s.
    if let Err(e) = &ended {
        assert!(
            !matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "{e}"
        );
    }
    // Cut before the record was whole: unanswered.
    assert!(got.is_empty(), "{}", String::from_utf8_lossy(&got));
    assert!(took >= within, "{took:?}");
}

/// Over TLS a request's time runs from its first record's first part, as a
/// plain one's from its first byte: the time a first record took to come
/// whole is spent, not given again. The record's first byte, 3 s of
/// nothing, then its rest: whole within the 5 s asked. The rest of the
/// head goes 7.5 s after the first byte, unless an answer came first: the
/// request is cut at 5 s and the 40 ms its bytes earned, 408, where a time
/// started at the record's end ran to 8 s and answered it 200. The time is
/// the property here, the cut held to seconds before the rest is sent.
#[test]
fn a_requests_time_over_tls_runs_from_its_first_records_first_part() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let within = Duration::from_secs(5);
    let (port, _server) = start(|c| c.header_timeout = Some(within));
    let gate = Arc::new(AtomicUsize::new(AT_ONCE));
    let mut r = handshaken(port, &gate);
    let s = r.get_mut();
    // The next request's first record, its line and its host: its first
    // byte passed on alone.
    gate.store(1, SeqCst);
    let started = Instant::now();
    s.write_all(b"POST /query HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    s.flush().unwrap();
    std::thread::sleep(Duration::from_secs(3));
    gate.store(AT_ONCE, SeqCst);
    let rest = started + Duration::from_millis(7500);
    let body = query("collections");
    let answered = s
        .readable(rest.saturating_duration_since(Instant::now()))
        .unwrap();
    if !answered {
        let mut tail = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        tail.extend_from_slice(&body);
        s.write_all(&tail).unwrap();
        s.flush().unwrap();
    }
    let a = answer(&mut r);
    let took = started.elapsed();
    assert_eq!(a.status, 408, "{} after {took:?}", a.text());
    assert!(took >= within, "{took:?}");
}

/// Plain HTTP to the HTTPS port is answered 400 in the clear, naming the
/// scheme, and the connection ended after it -- read whole, no reset.
#[test]
fn plain_http_is_told_to_ask_over_https() {
    let (port, _server) = start(|_| {});
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(b"GET /_health HTTP/1.1\r\nHost: localhost\r\n\r\n")
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
