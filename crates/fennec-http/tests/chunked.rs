//! A body sent `Transfer-Encoding: chunked` -- as a proxy in front sends
//! one it re-chunks -- is read whole, on every route that reads a body, as
//! its `Content-Length` would have had it read; a framing another server
//! could read otherwise is refused, the connection closed after it; and a
//! kept-alive connection reads its next request right after the last
//! chunk's trailers. The decoder's own cases, a byte a read among them,
//! are `fennec_http::http`'s unit tests.

use fennec_core::engine::Database;
use fennec_http::replication::{self, Replication};
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const ROOT: &str = "root-token";

fn start(tune: impl FnOnce(&mut Config)) -> u16 {
    let port = serve(Arc::new(RwLock::new(Database::new())), None, tune);
    for s in [
        "create collection notes (t text)",
        "create statement titled($t text) public as get notes select t where t = $t",
    ] {
        let a = plain(port, "POST", "/query", "", &query(s));
        assert_eq!(a.status, 200, "{s}: {}", a.body);
    }
    port
}

fn serve(
    db: Arc<RwLock<Database>>,
    replication: Option<Arc<Replication>>,
    tune: impl FnOnce(&mut Config),
) -> u16 {
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        ..Config::default()
    };
    tune(&mut cfg);
    let mut server = Server::new(db, cfg);
    if let Some(r) = replication {
        server = server.with_replication(r);
    }
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

/// `body` framed in chunks of `size`, each a write of its own on a socket
/// that sends at once, so the server meets sizes and data split across its
/// reads; then the last chunk and `trailers`.
fn chunks_of(body: &[u8], size: usize, trailers: &str) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = body
        .chunks(size)
        .map(|c| {
            let mut one = format!("{:x}\r\n", c.len()).into_bytes();
            one.extend_from_slice(c);
            one.extend_from_slice(b"\r\n");
            one
        })
        .collect();
    out.push(format!("0\r\n{trailers}\r\n").into_bytes());
    out
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

/// Every answer on a connection after its requests were written, in order:
/// read until the server closes it.
fn answers(out: &str) -> Vec<Answer> {
    let mut all = Vec::new();
    let mut rest = out;
    while let Some((head, after)) = rest.split_once("\r\n\r\n") {
        let length: usize = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse().unwrap())
            })
            .unwrap_or(0);
        all.push(Answer {
            status: head[9..12].parse().unwrap(),
            head: head.to_string(),
            body: after[..length].to_string(),
        });
        rest = &after[length..];
    }
    all
}

fn connect(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.set_nodelay(true).unwrap();
    s
}

/// A chunked request, its `body` in chunks of `size`, `headers` beside the
/// coding, the connection closed after the answer.
fn chunked(
    port: u16,
    method: &str,
    target: &str,
    headers: &str,
    body: &[u8],
    size: usize,
) -> Answer {
    let mut s = connect(port);
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {ROOT}\r\n\
         {headers}Connection: close\r\nTransfer-Encoding: chunked\r\n\r\n"
    )
    .unwrap();
    for piece in chunks_of(body, size, "") {
        s.write_all(&piece).unwrap();
    }
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    answers(&out).remove(0)
}

fn plain(port: u16, method: &str, target: &str, headers: &str, body: &str) -> Answer {
    let mut s = connect(port);
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer {ROOT}\r\n{headers}\
         Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    answers(&out).remove(0)
}

fn query(q: &str) -> String {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    body
}

#[test]
fn a_chunked_body_of_any_size_is_read_whole() {
    let port = start(|_| {});
    // A statement past the reader's 8 KiB buffer and one past a mebibyte,
    // in chunks of a byte, of a few, and of more than a read takes.
    let mut wrote = 0;
    for (len, size) in [
        (1, 1),
        (300, 7),
        (9_000, 4096),
        (1_200_000, 65_536),
        (40_000, 1),
    ] {
        let t = "x".repeat(len);
        let a = chunked(
            port,
            "POST",
            "/query",
            "",
            query(&format!("put notes {{t: '{t}'}}")).as_bytes(),
            size,
        );
        assert_eq!(a.status, 200, "{len} in chunks of {size}: {}", a.body);
        wrote += 1;
        let a = chunked(
            port,
            "POST",
            "/query",
            "",
            query(&format!("get notes count where t = '{t}'")).as_bytes(),
            size,
        );
        assert_eq!(
            a.body.trim(),
            r#"[{"count":1}]"#,
            "{len} in chunks of {size}"
        );
    }
    let a = plain(port, "POST", "/query", "", &query("get notes count"));
    assert_eq!(a.body.trim(), format!(r#"[{{"count":{wrote}}}]"#));
    // An empty body is the last chunk alone, read as no body at all.
    let a = chunked(port, "POST", "/query", "", b"", 1);
    assert_eq!(a.status, 400, "{}", a.body);
    assert_eq!(a.status, plain(port, "POST", "/query", "", "").status);
}

#[test]
fn a_kept_alive_connection_reads_on_right_after_the_trailers() {
    let port = start(|_| {});
    let mut s = connect(port);
    let auth = format!("Authorization: Bearer {ROOT}\r\n");
    let put = query("put notes {t: 'piped'}");
    let count = query("get notes count where t = 'piped'");
    let batch = format!("{put}\n{count}");
    let mut all = Vec::new();
    // Chunked with trailers, plain, chunked again, plain closing it.
    all.extend_from_slice(
        format!("POST /query HTTP/1.1\r\nHost: t\r\n{auth}Transfer-Encoding: chunked\r\nTrailer: Checksum\r\n\r\n")
            .as_bytes(),
    );
    for piece in chunks_of(put.as_bytes(), 5, "Checksum: 1\r\n") {
        all.extend_from_slice(&piece);
    }
    all.extend_from_slice(
        format!(
            "POST /query HTTP/1.1\r\nHost: t\r\n{auth}Content-Length: {}\r\n\r\n{count}",
            count.len()
        )
        .as_bytes(),
    );
    all.extend_from_slice(
        format!("POST /batch HTTP/1.1\r\nHost: t\r\n{auth}Transfer-Encoding: chunked\r\n\r\n")
            .as_bytes(),
    );
    for piece in chunks_of(batch.as_bytes(), 11, "") {
        all.extend_from_slice(&piece);
    }
    all.extend_from_slice(
        format!("GET /notes?count HTTP/1.1\r\nHost: t\r\n{auth}Connection: close\r\n\r\n")
            .as_bytes(),
    );
    // One write: every request in the server's reader at once.
    s.write_all(&all).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let got = answers(&out);
    assert_eq!(got.len(), 4, "{out}");
    assert!(got.iter().all(|a| a.status == 200), "{out}");
    assert_eq!(got[1].body.trim(), r#"[{"count":1}]"#);
    assert!(got[2].body.contains(r#""count":2"#), "{}", got[2].body);
    assert_eq!(got[3].body.trim(), r#"{"count":2}"#);
}

#[test]
fn a_framing_read_two_ways_is_refused_and_the_connection_closed() {
    let port = start(|c| c.max_body = 1 << 10);
    let put = query("put notes {t: 'smuggled'}");
    let next = format!(
        "POST /query HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer {ROOT}\r\nContent-Length: {}\r\n\r\n{put}",
        put.len()
    );
    let cases: &[(&str, &str, u16)] = &[
        (
            "Content-Length: 5\r\nTransfer-Encoding: chunked\r\n",
            "5\r\nhello\r\n0\r\n\r\n",
            400,
        ),
        (
            "Transfer-Encoding: gzip, chunked\r\n",
            "5\r\nhello\r\n0\r\n\r\n",
            501,
        ),
        (
            "Transfer-Encoding: chunked, gzip\r\n",
            "5\r\nhello\r\n0\r\n\r\n",
            400,
        ),
        (
            "Transfer-Encoding: chunked\r\n",
            "5\nhello\r\n0\r\n\r\n",
            400,
        ),
        (
            "Transfer-Encoding: chunked\r\n",
            "12345678901234567\r\n",
            400,
        ),
        (
            "Transfer-Encoding: chunked\r\n",
            &format!("5;{}\r\nhello\r\n0\r\n\r\n", "e".repeat(2000)),
            400,
        ),
        // Past the ceiling: its size said, no byte of it sent.
        ("Transfer-Encoding: chunked\r\n", "401\r\n", 413),
        (
            "Transfer-Encoding: chunked\r\n",
            "ffffffffffffffff\r\n",
            413,
        ),
        (
            "X: a\r\n Transfer-Encoding: chunked\r\n",
            "5\r\nhello\r\n0\r\n\r\n",
            400,
        ),
    ];
    for (headers, body, status) in cases {
        let mut s = connect(port);
        // A request the refused one's body would be read as, after it.
        write!(
            s,
            "POST /query HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer {ROOT}\r\n{headers}\r\n{body}{next}"
        )
        .unwrap();
        // Closed with the rest unread: what came before the reset is read.
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        let got = answers(&out);
        assert_eq!(got.len(), 1, "{headers:?}: the connection read on: {out}");
        assert_eq!(
            got[0].status, *status,
            "{headers:?} {body:?}: {}",
            got[0].body
        );
        assert!(got[0].head.contains("Connection: close"), "{}", got[0].head);
    }
    let a = plain(port, "POST", "/query", "", &query("get notes count"));
    assert_eq!(a.body.trim(), r#"[{"count":0}]"#, "nothing smuggled ran");
    // HTTP/1.0 has no transfer coding.
    let mut s = connect(port);
    s.write_all(b"POST /query HTTP/1.0\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert_eq!(answers(&out)[0].status, 400, "{out}");
}

#[test]
fn every_route_that_reads_a_body_reads_a_chunked_one() {
    let port = start(|c| c.mcp = Some(Arc::new(fennec_http::mcp::Options::default())));
    let a = chunked(
        port,
        "POST",
        "/query",
        "",
        query("put notes {t: 'a'}").as_bytes(),
        3,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    let batch = format!(
        "{}\n{}",
        query("put notes {t: 'b'}"),
        query("get notes count")
    );
    let a = chunked(port, "POST", "/batch", "", batch.as_bytes(), 4);
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(a.body.contains(r#""count":2"#), "{}", a.body);
    // A named statement's arguments.
    let a = chunked(
        port,
        "POST",
        "/_call/titled",
        "",
        br#"{"params":{"t":"b"}}"#,
        2,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(a.body.contains(r#""t":"b""#), "{}", a.body);
    // The REST surface's own bodies.
    let a = chunked(port, "POST", "/notes", "", br#"{"t":"c"}"#, 2);
    assert!(a.status < 300, "{}: {}", a.status, a.body);
    let a = chunked(port, "PATCH", "/notes?t=eq.c", "", br#"{"t":"d"}"#, 2);
    assert_eq!(a.status, 200, "{}", a.body);
    let a = plain(port, "GET", "/notes?t=eq.d&count", "", "");
    assert_eq!(a.body.trim(), r#"{"count":1}"#);
    // MCP, its JSON-RPC.
    let mcp = "Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n";
    let a = chunked(
        port,
        "POST",
        "/_mcp",
        mcp,
        br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#,
        6,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(a.body.contains(r#""id":7"#), "{}", a.body);
}

#[test]
fn a_consumer_is_made_and_moved_by_chunked_bodies() {
    let dir = std::env::temp_dir().join(format!("fennec-chunked-cdc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("data.fennec");
    let (db, feed) =
        replication::open(path.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    let port = serve(
        Arc::new(RwLock::new(db)),
        Some(Replication::new(None, Some(feed), None)),
        |c| c.sync_on_write = true,
    );
    let a = plain(
        port,
        "POST",
        "/query",
        "",
        &query("create collection a (t text)"),
    );
    assert_eq!(a.status, 200, "{}", a.body);
    // Read, the body is refused for what it holds, not for how it came.
    let a = chunked(
        port,
        "POST",
        "/_changes/consumers/idx",
        "",
        br#"{"since": "x"}"#,
        3,
    );
    assert_eq!(a.status, 400, "{}", a.body);
    let a = chunked(port, "POST", "/_changes/consumers/idx", "", b"", 1);
    assert_eq!(a.status, 201, "{}", a.body);
    let a = chunked(
        port,
        "PATCH",
        "/_changes/consumers/idx",
        "",
        br#"{"since": 99999}"#,
        4,
    );
    assert_eq!(a.status, 409, "{}", a.body);
    let a = chunked(
        port,
        "PATCH",
        "/_changes/consumers/idx",
        "",
        br#"{"since": 1}"#,
        4,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_idempotency_key_digests_the_body_however_it_was_framed() {
    let port = start(|c| c.idempotency_ttl = Duration::from_secs(60));
    let put = query("put notes {t: 'once'}");
    let key = "Idempotency-Key: k1\r\n";
    let first = chunked(port, "POST", "/query", key, put.as_bytes(), 4);
    assert_eq!(first.status, 200, "{}", first.body);
    // The same body by its length is the same request: replayed.
    let again = plain(port, "POST", "/query", key, &put);
    assert_eq!(again.status, 200, "{}", again.body);
    assert!(
        again.head.contains("Idempotent-Replayed: true"),
        "{}",
        again.head
    );
    assert_eq!(again.body, first.body);
    // Another body under the key, chunked, is another request.
    let other = chunked(
        port,
        "POST",
        "/query",
        key,
        query("put notes {t: 'twice'}").as_bytes(),
        4,
    );
    assert_eq!(other.status, 422, "{}", other.body);
    let a = plain(port, "POST", "/query", "", &query("get notes count"));
    assert_eq!(a.body.trim(), r#"[{"count":1}]"#);
}
