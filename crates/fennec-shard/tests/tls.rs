//! The router serving HTTPS (`--tls-cert`): a client's TLS ends at the
//! router, which forwards each request to its node in plain HTTP as ever,
//! and writes the node's answer back over TLS -- an answer by its length
//! past a record's 16 KiB, on a kept-alive connection, and a stream with no
//! length relayed as it arrives. A plain request to it is told the scheme.
//! The node stands in, written by hand, to read what the router sends.
#![cfg(feature = "tls")]

use fennec_http::conn::Tls;
use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fennec-http/tests/certs")
}

/// A router serving HTTPS, placing tenant `acme` on the node at `addr`.
fn router_to(addr: &str) -> u16 {
    fennec_http::audit::set_delay(0);
    let mut d = Directory::in_memory();
    let placed = Node {
        addr: addr.to_string(),
        token: "adm".into(),
        ..Node::default()
    };
    d.set_node("n1", placed).unwrap();
    d.place("acme", "n1", State::Active).unwrap();
    let tls = Tls::from_pem_files(&certs().join("server.pem"), &certs().join("server.key"));
    let router = Router::new(
        d,
        Config {
            addr: "127.0.0.1:0".into(),
            upstream_timeout: Duration::from_secs(10),
            tls: Some(Arc::new(tls.unwrap())),
            ..Config::default()
        },
    );
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    port
}

fn dial(port: u16) -> BufReader<fennec_tls::TlsStream> {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let client = fennec_tls::Client::trusting_pem_file(&certs().join("ca.pem")).unwrap();
    BufReader::new(client.dial(s, "localhost").unwrap())
}

/// The router's connection to the stand-in node, the wait for it bounded.
fn accepted(node: &TcpListener) -> BufReader<TcpStream> {
    node.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match node.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false).unwrap();
                s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                return BufReader::new(s);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("the router sent the node nothing: {e}"),
        }
    }
}

/// A head off `r`, and the length it gives.
fn head(r: &mut impl BufRead) -> (String, Option<usize>) {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(
            r.read_line(&mut line).unwrap(),
            0,
            "closed in a head: {head}"
        );
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length = head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse().unwrap())
    });
    (head, length)
}

fn body(r: &mut impl BufRead, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).unwrap();
    b
}

#[test]
fn https_from_the_client_plain_http_to_the_node() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = router_to(&node.local_addr().unwrap().to_string());
    let mut client = dial(port);
    let long: String = (0..100_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let mut conn = None;
    for n in 0..2 {
        let q = format!("{{\"query\":\"get notes where n = {n}\"}}");
        let req = format!(
            "POST /t/acme/query HTTP/1.1\r\nHost: r\r\nContent-Length: {}\r\n\r\n{q}",
            q.len()
        );
        client.get_mut().write_all(req.as_bytes()).unwrap();
        client.get_mut().flush().unwrap();
        // One pooled connection to the node carries both.
        let node_conn = conn.get_or_insert_with(|| accepted(&node));
        let (h, length) = head(node_conn);
        assert!(h.starts_with("POST /t/acme/query HTTP/1.1"), "{h}");
        assert_eq!(body(node_conn, length.unwrap()), q.as_bytes());
        let answer = format!("{{\"rows\":[{{\"t\":\"{long}\"}}]}}");
        let out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: keep-alive\r\n\r\n{answer}",
            answer.len()
        );
        node_conn.get_mut().write_all(out.as_bytes()).unwrap();

        let (h, length) = head(&mut client);
        assert!(h.starts_with("HTTP/1.1 200"), "{h}");
        assert!(h.contains("Fennec-Shard: 1"), "{h}");
        assert_eq!(body(&mut client, length.unwrap()), answer.as_bytes());
    }
}

/// An answer with no length -- a node's stream of changes -- is relayed
/// over TLS as each part arrives, not held for the stream's end.
#[test]
fn a_stream_is_relayed_over_tls_as_it_arrives() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = router_to(&node.local_addr().unwrap().to_string());
    let mut client = dial(port);
    client
        .get_mut()
        .write_all(b"GET /t/acme/notes/changes HTTP/1.1\r\nHost: r\r\n\r\n")
        .unwrap();
    client.get_mut().flush().unwrap();
    let mut node_conn = accepted(&node);
    let (h, _) = head(&mut node_conn);
    assert!(h.starts_with("GET /t/acme/notes/changes"), "{h}");
    node_conn
        .get_mut()
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
              event: seed\ndata: {}\n\n",
        )
        .unwrap();
    let (h, length) = head(&mut client);
    assert!(h.starts_with("HTTP/1.1 200"), "{h}");
    assert_eq!(length, None);
    let mut line = String::new();
    client.read_line(&mut line).unwrap();
    assert_eq!(line, "event: seed\n");
    // The next event, written while the stream stays open, reaches the
    // client before any end.
    node_conn
        .get_mut()
        .write_all(b"event: change\ndata: {\"seq\":1}\n\n")
        .unwrap();
    let mut events = String::new();
    while !events.contains("event: change") {
        line.clear();
        assert!(client.read_line(&mut line).unwrap() > 0, "{events}");
        events.push_str(&line);
    }
}

/// A stream relayed to its end: the node's own end goes on as the
/// router's `close_notify`, and a node that broke off -- its connection
/// reset -- goes on as none, so the client reads the stream cut short.
#[test]
fn a_relayed_stream_s_end_is_a_close_notify_and_a_break_none() {
    for reset in [false, true] {
        let node = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = router_to(&node.local_addr().unwrap().to_string());
        let mut client = dial(port);
        client
            .get_mut()
            .write_all(b"GET /t/acme/notes/changes HTTP/1.1\r\nHost: r\r\n\r\n")
            .unwrap();
        client.get_mut().flush().unwrap();
        let mut node_conn = accepted(&node);
        head(&mut node_conn);
        node_conn
            .get_mut()
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
                  event: seed\ndata: {}\n\n",
            )
            .unwrap();
        let (h, _) = head(&mut client);
        assert!(h.starts_with("HTTP/1.1 200"), "{h}");
        let mut line = String::new();
        client.read_line(&mut line).unwrap();
        assert_eq!(line, "event: seed\n");
        if reset {
            crate::upstream::reset_on_close(node_conn.get_ref());
        }
        drop(node_conn);
        let mut rest = Vec::new();
        let ended = client.read_to_end(&mut rest);
        match reset {
            false => assert!(ended.is_ok(), "the node's end: {ended:?}"),
            true => assert_eq!(
                ended.map_err(|e| e.kind()),
                Err(ErrorKind::UnexpectedEof),
                "the node's reset"
            ),
        }
    }
}

/// A plain request to the router's HTTPS port is answered 400 in the
/// clear, naming the scheme.
#[test]
fn plain_http_to_the_router_is_told_to_ask_over_https() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = router_to(&node.local_addr().unwrap().to_string());
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(b"GET /t/acme/notes HTTP/1.1\r\nHost: r\r\n\r\n")
        .unwrap();
    let mut got = String::new();
    s.read_to_string(&mut got).unwrap();
    assert!(got.starts_with("HTTP/1.1 400 "), "{got}");
    assert!(got.contains(fennec_http::conn::PLAIN_TO_TLS), "{got}");
}

/// A held request's client ending its TLS with `close_notify`, and reading
/// on, has its end passed on to the node, as a plain client's end is
/// (`upstream::answered_or_gone`), and is answered what the node answers.
/// The router's watch peeked the socket, where the alert's record read as
/// bytes of a next request: it watched no more, and the node's held claim
/// took a job for a client that had ended.
#[test]
fn a_held_request_s_close_notify_goes_on_to_the_node() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = router_to(&node.local_addr().unwrap().to_string());
    let mut client = dial(port);
    let q = r#"{"query":"get jobs"}"#;
    let req = format!(
        "POST /t/acme/query HTTP/1.1\r\nHost: r\r\nFennec-Wait: 30000\r\n\
         Content-Length: {}\r\n\r\n{q}",
        q.len()
    );
    client.get_mut().write_all(req.as_bytes()).unwrap();
    client.get_mut().flush().unwrap();
    let mut node_conn = accepted(&node);
    let (h, length) = head(&mut node_conn);
    assert!(h.starts_with("POST /t/acme/query HTTP/1.1"), "{h}");
    assert_eq!(body(&mut node_conn, length.unwrap()), q.as_bytes());
    client.get_mut().close().unwrap();
    // The router's connection shut for writing: the end the node reads
    // from a client of its own.
    let mut more = [0u8; 1];
    match node_conn.read(&mut more) {
        Ok(0) => {}
        got => panic!("the client's close_notify never reached the node: {got:?}"),
    }
    let answer = r#"{"rows":[]}"#;
    let out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{answer}",
        answer.len()
    );
    node_conn.get_mut().write_all(out.as_bytes()).unwrap();
    let (h, length) = head(&mut client);
    assert!(h.starts_with("HTTP/1.1 200"), "{h}");
    assert_eq!(body(&mut client, length.unwrap()), answer.as_bytes());
}
