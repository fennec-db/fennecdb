//! A body a client sends `Transfer-Encoding: chunked` reaches the node by
//! its `Content-Length`: the router reads a request with fennec-http's own
//! reader, which decodes it whole, and writes every forward's length
//! itself, the coding and its trailers being the client's connection's
//! alone. A framing read two ways is the router's 400, sent nowhere. The
//! node here stands in, written by hand, to read what the router sends.

use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// A router placing tenant `acme` on the node at `addr`.
fn router_to(addr: &str) -> String {
    fennec_http::audit::set_delay(0);
    let mut d = Directory::in_memory();
    let placed = Node {
        addr: addr.to_string(),
        token: "adm".into(),
        ..Node::default()
    };
    d.set_node("n1", placed).unwrap();
    d.place("acme", "n1", State::Active).unwrap();
    let router = Router::new(
        d,
        Config {
            addr: "127.0.0.1:0".into(),
            upstream_timeout: Duration::from_secs(10),
            ..Config::default()
        },
    );
    let listener = router.bind().unwrap();
    let front = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    front
}

/// The router's connection to the stand-in node, the wait for it bounded:
/// a router that sends nothing on fails the test rather than hold it.
fn accepted(node: &TcpListener) -> BufReader<TcpStream> {
    node.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match node.accept() {
            Ok((s, _)) => {
                // macOS hands the listener's non-blocking on to it.
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

/// One forwarded request off the stand-in node's connection: its head and
/// its body, read by the length the head gives.
fn forwarded(conn: &mut BufReader<TcpStream>) -> (String, Vec<u8>) {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(conn.read_line(&mut line).unwrap(), 0, "the router closed");
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length = header(&head, "content-length").map_or(0, |n| n.parse().unwrap());
    let mut body = vec![0u8; length];
    conn.read_exact(&mut body).unwrap();
    (head, body)
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// The statuses a client read off its connection, in order, until the
/// router closed it.
fn statuses(out: &str) -> Vec<u16> {
    out.match_indices("HTTP/1.1 ")
        .map(|(at, _)| out[at + 9..at + 12].parse().unwrap())
        .collect()
}

const ANSWERED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n{}";

fn chunks_of(body: &[u8], size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for c in body.chunks(size) {
        out.extend_from_slice(format!("{:x};ext=1\r\n", c.len()).as_bytes());
        out.extend_from_slice(c);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\nChecksum: 1\r\n\r\n");
    out
}

#[test]
fn a_chunked_body_goes_on_to_the_node_by_its_length() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(&node.local_addr().unwrap().to_string());
    let body = br#"{"query":"put notes {title: \"sent in pieces\"}"}"#.to_vec();
    let sent = body.clone();
    // A chunked request, then a plain one, on one kept-alive connection.
    let client = std::thread::spawn(move || {
        let mut s = TcpStream::connect(&front).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut all = b"POST /t/acme/query HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\
                        Trailer: Checksum\r\nTE: trailers\r\n\r\n"
            .to_vec();
        all.extend_from_slice(&chunks_of(&sent, 5));
        all.extend_from_slice(
            b"GET /t/acme/notes HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        );
        s.write_all(&all).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    });
    let mut conn = accepted(&node);
    let (head, got) = forwarded(&mut conn);
    assert!(
        head.starts_with("POST /t/acme/query HTTP/1.1\r\n"),
        "{head}"
    );
    assert_eq!(got, body, "{head}");
    assert_eq!(
        header(&head, "content-length"),
        Some(body.len().to_string().as_str())
    );
    for gone in ["transfer-encoding", "trailer", "te", "checksum"] {
        assert_eq!(header(&head, gone), None, "{gone}: {head}");
    }
    conn.get_mut().write_all(ANSWERED).unwrap();
    // The plain request after it, read from right after the trailers.
    let (head, got) = forwarded(&mut conn);
    assert!(head.starts_with("GET /t/acme/notes HTTP/1.1\r\n"), "{head}");
    assert!(got.is_empty());
    conn.get_mut().write_all(ANSWERED).unwrap();
    let out = client.join().unwrap();
    assert_eq!(statuses(&out), [200, 200], "{out}");
}

#[test]
fn a_framing_read_two_ways_is_the_router_s_400_sent_nowhere() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(&node.local_addr().unwrap().to_string());
    for (framing, status) in [
        ("Content-Length: 5\r\nTransfer-Encoding: chunked\r\n", 400),
        ("Transfer-Encoding: gzip, chunked\r\n", 501),
    ] {
        let mut s = TcpStream::connect(&front).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "POST /t/acme/query HTTP/1.1\r\nHost: t\r\n{framing}\r\n0\r\n\r\n\
             DELETE /t/acme/notes/1 HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert_eq!(statuses(&out), [status], "{framing:?}: {out}");
    }
    // The first request the node is sent is the one after them.
    let front2 = front.clone();
    let client = std::thread::spawn(move || {
        let mut s = TcpStream::connect(&front2).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s.write_all(b"GET /t/acme/notes HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    });
    let mut conn = accepted(&node);
    let (head, _) = forwarded(&mut conn);
    assert!(head.starts_with("GET /t/acme/notes HTTP/1.1\r\n"), "{head}");
    conn.get_mut().write_all(ANSWERED).unwrap();
    assert_eq!(statuses(&client.join().unwrap()), [200]);
}
