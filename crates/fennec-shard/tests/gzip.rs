//! A node's gzip through the router (`--http-gzip`): the client's
//! `Accept-Encoding` goes on to the node, and the node's
//! `Content-Encoding`, `Vary` and the length of what it compressed come
//! back as it wrote them, a stream's member too; the node judges by the
//! client the router names, never by the router's own address.

use fennec_http::crypto::{b64url_encode, hmac_sha256};
use fennec_http::gzip;
use fennec_http::tenants::Tenants;
use fennec_shard::directory::Directory;
use fennec_shard::{Config, Router};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The global entry's token, which the router believes its names under.
const ENTRY: &str = "entry-token-for-gzip";

struct Node {
    addr: String,
    dir: PathBuf,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A `--dir` node, compressing for a client on its host where `local`.
fn node(tag: &str, local: bool) -> Node {
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-shard-gzip-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        gzip_local: local,
        stream_keepalive: Duration::from_secs(30),
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(tenants, cfg);
    let listener = server.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { addr, dir }
}

/// A router in front of `n`, which holds `acme` and its 200 notes.
fn in_front_of(n: &Node) -> u16 {
    fennec_http::audit::set_delay(0);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some("root".into()),
        entry_token: Some(ENTRY.into()),
        ..Config::default()
    };
    let router = Router::new(Directory::in_memory(), cfg);
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    let root = "Authorization: Bearer root\r\n";
    let body = format!(r#"{{"addr":"{}","token":"adm"}}"#, n.addr);
    assert_eq!(
        ask(port, "PUT", "/_shard/nodes/n1", &body, root).status,
        201
    );
    assert_eq!(
        ask(port, "PUT", "/_shard/tenants/acme", "", root).status,
        201
    );
    let made = ask(
        port,
        "POST",
        "/t/acme/query",
        &query("create collection notes (title text, n int)"),
        "",
    );
    assert_eq!(made.status, 200, "{}", made.text());
    let rows: Vec<String> = (1..=200)
        .map(|i| format!("{{id: {i}, title: \\\"the same note again, number {i}\\\", n: {i}}}"))
        .collect();
    let put = ask(
        port,
        "POST",
        "/t/acme/query",
        &query(&format!("put notes [{}]", rows.join(","))),
        "",
    );
    assert_eq!(put.status, 200, "{}", put.text());
    port
}

fn query(q: &str) -> String {
    format!("{{\"query\":\"{q}\"}}")
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

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
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

const GZIP: &str = "Accept-Encoding: gzip\r\n";

#[test]
fn a_nodes_gzip_passes_through_the_router_as_it_was_written() {
    let n = node("pass", true);
    let port = in_front_of(&n);
    let page = query("get notes order n");
    let plain = ask(port, "POST", "/t/acme/query", &page, "");
    assert_eq!(plain.status, 200, "{}", plain.text());
    assert!(plain.body.len() > gzip::MIN_BYTES);
    assert_eq!(plain.one("content-encoding"), None, "{}", plain.head);
    assert_eq!(plain.all("vary"), ["Accept-Encoding"], "{}", plain.head);
    let zipped = ask(port, "POST", "/t/acme/query", &page, GZIP);
    assert_eq!(zipped.status, 200);
    assert_eq!(
        zipped.one("content-encoding"),
        Some("gzip"),
        "{}",
        zipped.head
    );
    assert_eq!(zipped.all("vary"), ["Accept-Encoding"], "{}", zipped.head);
    assert_eq!(
        zipped.one("content-length"),
        Some(&*zipped.body.len().to_string())
    );
    assert_eq!(zipped.one("fennec-shard"), Some("1"));
    let mut want = Vec::new();
    gzip::compress(&plain.body, &mut want);
    assert!(zipped.body == want, "not the node's gzip of the identity");

    // A stream: its head says so, and its member begins with the seed's
    // piece, as the node deflated it.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET /t/acme/notes/changes HTTP/1.1\r\nHost: t\r\n{GZIP}\r\n"
    )
    .unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 65536];
    let at = loop {
        if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let k = s.read(&mut buf).unwrap();
        assert!(k > 0);
        got.extend_from_slice(&buf[..k]);
    };
    let head = String::from_utf8(got[..at].to_vec()).unwrap();
    assert!(head.contains("Content-Encoding: gzip"), "{head}");
    assert!(head.contains("Vary: Accept-Encoding"), "{head}");
    let body = &got[at + 4..];
    let mut start = Vec::new();
    gzip::Stream::start(&mut start);
    let mut have = body.to_vec();
    while have.len() < start.len() + 64 {
        let k = s.read(&mut buf).unwrap();
        assert!(k > 0);
        have.extend_from_slice(&buf[..k]);
    }
    assert_eq!(have[..start.len()], start[..], "a gzip member's header");
}

/// The global entry's name for a client at `address`, as entry.js makes
/// it: HMAC-SHA256 of `fennec-client <address> <seconds>` under its token,
/// 16 bytes, base64url.
fn named(address: &str) -> String {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac = hmac_sha256(
        ENTRY.as_bytes(),
        format!("fennec-client {address} {at}").as_bytes(),
    );
    format!(
        "Fennec-Client: {address} {at} {}\r\n",
        b64url_encode(&mac[..16])
    )
}

/// The router names its client to the node, which judges by it, never by
/// the router's address: on loopback here, the router's and the client's
/// both, a client the entry names across the internet is answered in
/// gzip, and one on the router's host as it is.
#[test]
fn the_node_judges_by_the_client_the_router_names() {
    let n = node("near", false);
    let port = in_front_of(&n);
    let page = query("get notes order n");
    let near = ask(port, "POST", "/t/acme/query", &page, GZIP);
    assert_eq!(near.status, 200, "{}", near.text());
    assert_eq!(near.one("content-encoding"), None, "{}", near.head);
    assert_eq!(near.all("vary"), ["Accept-Encoding"], "{}", near.head);
    let far = ask(
        port,
        "POST",
        "/t/acme/query",
        &page,
        &format!("{GZIP}{}", named("203.0.113.7")),
    );
    assert_eq!(far.status, 200, "{}", far.text());
    assert_eq!(far.one("content-encoding"), Some("gzip"), "{}", far.head);
    let mut want = Vec::new();
    gzip::compress(&near.body, &mut want);
    assert!(far.body == want, "not the node's gzip of the identity");
}
