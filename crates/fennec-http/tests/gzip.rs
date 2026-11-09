//! Answers in gzip (`--http-gzip`): an answer past the bytes goes as gzip
//! where `Accept-Encoding` takes it and its client is not on the server's
//! host or network (`--http-gzip-local` here, the tests' clients being on
//! loopback), says `Vary: Accept-Encoding`, and never carries a tag; a
//! subscription's stream is one member, each event a piece of it ended on
//! a byte. The compressor is deterministic, so each compressed body is
//! held to `gzip::compress` of the identity body byte for byte; its
//! inflation by an inflater not ours is `gzip`'s unit tests'.

use fennec_core::prelude::*;
use fennec_http::{gzip, Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// A server over 200 rows, each some 120 bytes: a page of them is past
/// the bytes, and row 1 alone too.
fn start(local: bool, gzip: Option<usize>) -> u16 {
    fennec_http::audit::set_delay(0);
    let mut db = Database::new();
    db.execute(
        &fennec_ql::parse_one("create collection notes (title text, body text, n int)").unwrap(),
    )
    .unwrap();
    let rows: Vec<String> = (1..=200)
        .map(|i| {
            format!(
                r#"{{id: {i}, title: "note {i}", body: "{}", n: {i}}}"#,
                "the same words again and again ".repeat(if i == 1 { 80 } else { 3 })
            )
        })
        .collect();
    db.execute(&fennec_ql::parse_one(&format!("put notes [{}]", rows.join(","))).unwrap())
        .unwrap();
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        gzip,
        gzip_local: local,
        stream_keepalive: Duration::from_secs(30),
        ..Config::default()
    };
    let server = Server::new(Arc::new(RwLock::new(db)), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Answer {
    fn all(&self, name: &str) -> Vec<&str> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
            .collect()
    }

    fn one(&self, name: &str) -> Option<&str> {
        self.all(name).first().copied()
    }
}

fn ask(port: u16, method: &str, target: &str, body: &str, extra: &str) -> Answer {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: t\r\n{extra}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(out[..at].to_vec()).unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    Answer {
        status,
        head,
        body: out[at + 4..].to_vec(),
    }
}

const PAGE: &str = r#"{"query": "get notes order n limit 100"}"#;
const GZIP: &str = "Accept-Encoding: gzip, deflate, br\r\n";

fn gzipped(identity: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    gzip::compress(identity, &mut out);
    out
}

#[test]
fn a_page_of_rows_goes_as_gzip_where_it_is_taken() {
    let port = start(true, Some(gzip::MIN_BYTES));
    let plain = ask(port, "POST", "/query", PAGE, "");
    assert_eq!(plain.status, 200);
    assert!(plain.body.len() > gzip::MIN_BYTES);
    assert_eq!(plain.one("content-encoding"), None, "{}", plain.head);
    assert_eq!(plain.all("vary"), ["Accept-Encoding"], "{}", plain.head);
    let zipped = ask(port, "POST", "/query", PAGE, GZIP);
    assert_eq!(zipped.status, 200);
    assert_eq!(
        zipped.one("content-encoding"),
        Some("gzip"),
        "{}",
        zipped.head
    );
    assert_eq!(zipped.all("vary"), ["Accept-Encoding"]);
    assert_eq!(
        zipped.one("content-length"),
        Some(&*zipped.body.len().to_string())
    );
    assert!(
        zipped.body == gzipped(&plain.body),
        "not the identity's gzip"
    );
    assert!(zipped.body.len() * 3 < plain.body.len());
    // Its weight zero, gzip is refused: as it is.
    let refused = ask(
        port,
        "POST",
        "/query",
        PAGE,
        "Accept-Encoding: gzip;q=0, br\r\n",
    );
    assert_eq!(refused.one("content-encoding"), None);
    assert_eq!(refused.body, plain.body);
}

#[test]
fn a_tagged_row_a_head_and_a_small_answer_go_as_they_are() {
    let port = start(true, Some(gzip::MIN_BYTES));
    // Row 1 is past the bytes; its tag names its JSON as sent.
    let row = ask(port, "GET", "/notes/1", "", GZIP);
    assert_eq!(row.status, 200);
    assert!(row.body.len() > gzip::MIN_BYTES, "{}", row.body.len());
    assert!(row.one("etag").is_some(), "{}", row.head);
    assert_eq!(row.one("content-encoding"), None, "{}", row.head);
    assert_eq!(row.one("cache-control"), Some("no-transform"));
    let head = ask(port, "HEAD", "/notes", "", GZIP);
    assert_eq!(head.one("content-encoding"), None, "{}", head.head);
    let small = ask(port, "GET", "/notes/2", "", GZIP);
    assert!(small.body.len() < gzip::MIN_BYTES);
    assert_eq!(
        (small.one("content-encoding"), small.one("vary")),
        (None, None)
    );
}

#[test]
fn a_client_on_this_host_is_answered_as_it_is_unless_told() {
    // The default: a client on loopback is this host's.
    let port = start(false, Some(gzip::MIN_BYTES));
    let near = ask(port, "POST", "/query", PAGE, GZIP);
    assert_eq!(near.one("content-encoding"), None, "{}", near.head);
    assert_eq!(near.all("vary"), ["Accept-Encoding"]);
    // Off: nothing said.
    let port = start(true, None);
    let off = ask(port, "POST", "/query", PAGE, GZIP);
    assert_eq!((off.one("content-encoding"), off.one("vary")), (None, None));
}

/// Reads `s` onto `have` until `done` says it holds enough.
fn read_until(s: &mut TcpStream, have: &mut Vec<u8>, done: impl Fn(&[u8]) -> bool) {
    let mut buf = [0u8; 65536];
    while !done(have) {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "the stream ended");
        have.extend_from_slice(&buf[..n]);
    }
}

fn subscribe(port: u16, extra: &str) -> (TcpStream, String, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(s, "GET /notes/changes HTTP/1.1\r\nHost: t\r\n{extra}\r\n").unwrap();
    let mut got = Vec::new();
    read_until(&mut s, &mut got, |b| b.windows(4).any(|w| w == b"\r\n\r\n"));
    let at = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(got[..at].to_vec()).unwrap();
    (s, head, got[at + 4..].to_vec())
}

/// A stream's events as one member, each a piece read whole on its
/// arrival: the seed deflated, a change stored, each byte as the server
/// wrote it.
#[test]
fn a_stream_is_one_member_an_event_a_piece() {
    let port = start(true, Some(gzip::MIN_BYTES));
    // The identity stream: its seed and its first change, as text.
    let (mut plain, head, mut text) = subscribe(port, "");
    // Nothing keeps a stream: one sent as it is says no `Vary`.
    let lower = head.to_ascii_lowercase();
    assert!(
        !lower.contains("content-encoding") && !lower.contains("vary"),
        "{head}"
    );
    read_until(&mut plain, &mut text, |b| b.ends_with(b"\n\n"));
    let seed = text.clone();
    assert!(seed.starts_with(b"event: seed\n") && seed.len() > gzip::MIN_BYTES);
    let (mut zipped, head, mut bytes) = subscribe(port, GZIP);
    assert!(head.contains("Content-Encoding: gzip"), "{head}");
    assert!(head.contains("Vary: Accept-Encoding"), "{head}");
    let mut want = Vec::new();
    let mut stream = gzip::Stream::start(&mut want);
    stream.piece(&seed, gzip::MIN_BYTES, &mut want);
    read_until(&mut zipped, &mut bytes, |b| b.len() >= want.len());
    assert!(bytes[..want.len()] == want[..], "the seed's piece");
    // A write: its change, the same text on both, is the next piece.
    let wrote = ask(
        port,
        "POST",
        "/query",
        r#"{"query": "set notes {n: 0} where id = 2"}"#,
        "",
    );
    assert_eq!(wrote.status, 200);
    let mut more = Vec::new();
    read_until(&mut plain, &mut more, |b| b.ends_with(b"\n\n"));
    let at = want.len();
    stream.piece(&more, gzip::MIN_BYTES, &mut want);
    read_until(&mut zipped, &mut bytes, |b| b.len() >= want.len());
    assert!(bytes[at..want.len()] == want[at..], "the change's piece");
}

#[test]
fn the_openapi_document_is_deflated_once_and_served_the_same() {
    let port = start(true, Some(gzip::MIN_BYTES));
    let plain = ask(port, "GET", "/_openapi.json", "", "");
    assert_eq!(plain.status, 200);
    let a = ask(port, "GET", "/_openapi.json", "", GZIP);
    let b = ask(port, "GET", "/_openapi.json", "", GZIP);
    assert_eq!(a.one("content-encoding"), Some("gzip"), "{}", a.head);
    assert_eq!(a.all("vary"), ["Accept-Encoding"]);
    assert_eq!(a.body, b.body, "two requests, two different bytes");
    assert_eq!(a.body, gzipped(&plain.body), "not the document's gzip");
}
