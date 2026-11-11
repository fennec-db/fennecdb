//! `fennec-shard --http-cors`: what a page on another origin is told by the
//! router. Its own answers -- an unknown tenant's 404, a node's 502, a
//! standby's 503, its connection ceiling's -- carried no CORS header while
//! the nodes' answers through it carried the nodes', so a page read every
//! one of them as the network failing; and a preflight was forwarded, so
//! the request after it was never sent where the router could not reach a
//! node.

use fennec_core::prelude::Database;
use fennec_http::tenants::Tenants;
use fennec_shard::directory::{Directory, Node as Placed, State};
use fennec_shard::{Config, Router};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const APP: &str = "https://app.example";
const EVIL: &str = "https://evil.example";

struct Node {
    addr: String,
    dir: PathBuf,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A `--dir` node answering browsers from `cors`.
fn node(tag: &str, cors: Option<&str>) -> Node {
    // A refusal's wait is the process's; see router.rs.
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-shard-cors-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        stream_keepalive: Duration::from_millis(80),
        cors: cors.map(String::from),
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

/// A router over `dir` answering browsers from `cors`, its port. Its
/// upstream timeout is the default minute: it bounds every answer a node
/// sends through it, and at 2 s a node under load could miss it, while the
/// node it cannot reach refuses at once.
fn router(dir: Directory, cors: Option<&str>, max_connections: usize) -> u16 {
    fennec_http::audit::set_delay(0);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some("root".into()),
        max_connections,
        cors: cors.map(String::from),
        ..Config::default()
    };
    let router = Router::new(dir, cfg);
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    port
}

/// A directory placing `acme` on `addr`, as the router's admin would.
fn placed_at(addr: &str) -> Directory {
    let mut dir = Directory::in_memory();
    dir.set_node(
        "n1",
        Placed {
            addr: addr.into(),
            token: "adm".into(),
            ..Placed::default()
        },
    )
    .unwrap();
    dir.place("acme", "n1", State::Active).unwrap();
    dir
}

/// A router in front of `n`, which holds `acme`: placed and made as an
/// operator would, through `/_shard/`.
fn in_front_of(n: &Node, cors: Option<&str>) -> u16 {
    let port = router(Directory::in_memory(), cors, 0);
    let root = "Authorization: Bearer root\r\n";
    let body = format!(r#"{{"addr":"{}","token":"adm"}}"#, n.addr);
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "PUT /_shard/nodes/n1 HTTP/1.1\r\nHost: localhost\r\n{root}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    assert_eq!(read_answer(&mut s).status, 201);
    let made = ask(port, "PUT", "/_shard/tenants/acme", None, root);
    assert_eq!(made.status, 201, "{}", made.body);
    port
}

/// An address nothing listens on: a crashed node, seen from the router.
/// Port 1, which no test's server takes: a port the system picked and that
/// was let go of was taken again by another test's router running beside
/// this one, and the request forwarded there was answered its 404, no
/// tenant `acme` in its directory (once in 50 runs of the file, three cores
/// beside six busy loops).
fn dead_addr() -> String {
    "127.0.0.1:1".into()
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    /// Every value of the header `name`, in order.
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
        let all = self.all(name);
        assert!(all.len() <= 1, "{name} twice: {}", self.head);
        all.first().copied()
    }

    /// The CORS headers a node's are, held to the page's needs: one
    /// origin allowed, the headers a replica and `FennecHttp` read exposed,
    /// the ones they send allowed in.
    fn tells(&self, origin: &str) {
        assert_eq!(
            self.one("access-control-allow-origin"),
            Some(origin),
            "{}",
            self.head
        );
        let exposed = self.one("access-control-expose-headers").unwrap_or("");
        for h in ["retry-after", "fennec-seq", "x-request-id"] {
            assert!(exposed.contains(h), "{h} not exposed: {}", self.head);
        }
        let allowed = self.one("access-control-allow-headers").unwrap_or("");
        for h in ["idempotency-key", "authorization", "content-type"] {
            assert!(allowed.contains(h), "{h} not allowed in: {}", self.head);
        }
        assert_eq!(self.one("vary"), Some("Origin"), "{}", self.head);
    }

    fn tells_nothing(&self) {
        assert!(
            !self.head.to_ascii_lowercase().contains("access-control-"),
            "{}",
            self.head
        );
    }
}

/// One request on a connection of its own, `Origin` sent when given.
fn ask(port: u16, method: &str, target: &str, origin: Option<&str>, extra: &str) -> Answer {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let origin = origin.map_or(String::new(), |o| format!("Origin: {o}\r\n"));
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\n{origin}{extra}Content-Length: 0\r\n\
         Connection: close\r\n\r\n"
    )
    .unwrap();
    read_answer(&mut s)
}

fn read_answer(s: &mut TcpStream) -> Answer {
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Answer {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

const PREFLIGHT: &str = "Access-Control-Request-Method: POST\r\n\
                         Access-Control-Request-Headers: authorization, content-type, idempotency-key\r\n";

#[test]
fn the_routers_own_answers_tell_an_allowed_page() {
    let port = router(Directory::in_memory(), Some(APP), 0);

    // A tenant the directory does not hold.
    let r = ask(port, "POST", "/t/nosuch/query", Some(APP), "");
    assert_eq!(r.status, 404, "{}", r.body);
    r.tells(APP);
    assert!(r.one("x-request-id").is_some(), "{}", r.head);
    // A path the router does not serve, and /_shard/ without its token.
    let r = ask(port, "GET", "/nosuch", Some(APP), "");
    assert_eq!(r.status, 404);
    r.tells(APP);
    let r = ask(port, "GET", "/_shard/tenants", Some(APP), "");
    assert_eq!(r.status, 401);
    r.tells(APP);
    // With it: an answer a page reads, as the studio's admin view does.
    let r = ask(
        port,
        "GET",
        "/_shard/tenants",
        Some(APP),
        "Authorization: Bearer root\r\n",
    );
    assert_eq!(r.status, 200, "{}", r.body);
    r.tells(APP);

    // A page on another origin is never named: the browser compares the
    // origin allowed with its own and keeps the answer from it.
    let r = ask(port, "POST", "/t/nosuch/query", Some(EVIL), "");
    assert_eq!(r.status, 404);
    assert_eq!(r.one("access-control-allow-origin"), Some(APP));
    assert!(!r.head.contains(EVIL), "{}", r.head);
}

/// `/_openapi.json` (fennec-http's `openapi.rs`): what a tenant's client
/// calls through the router, each tenant's routes below `/t/{tenant}`,
/// served with no token and told to an allowed page, as Swagger UI on
/// another origin reads it. The node's administration is not in it.
#[test]
fn the_router_serves_the_openapi_document_of_a_tenants_routes() {
    use fennec_http::openapi::{document, Kind};
    let port = router(Directory::in_memory(), Some(APP), 0);
    let r = ask(port, "GET", "/_openapi.json", Some(APP), "");
    assert_eq!(r.status, 200, "{}", r.body);
    r.tells(APP);
    assert_eq!(r.body.as_bytes(), document(Kind::Router));
    assert!(r.body.starts_with(r#"{"openapi":"3.1.0""#), "{}", r.body);
    for path in [r#""/t/{tenant}/query""#, r#""/t/{tenant}/{collection}""#] {
        assert!(r.body.contains(path), "{path}");
    }
    assert!(!r.body.contains(r#""/_admin/"#));
    assert_eq!(ask(port, "HEAD", "/_openapi.json", None, "").status, 200);
    assert_eq!(ask(port, "POST", "/_openapi.json", None, "").status, 405);
}

#[test]
fn a_node_the_router_cannot_reach_is_a_503_a_page_reads() {
    let port = router(placed_at(&dead_addr()), Some(APP), 0);
    let r = ask(port, "POST", "/t/acme/query", Some(APP), "");
    assert_eq!(r.status, 503, "{}", r.body);
    r.tells(APP);
    // Its wait and its mark, each a header the page may read.
    assert_eq!(r.one("retry-after"), Some("1"));
    assert_eq!(r.one("fennec-unsent"), Some("1"));
    let exposed = r.one("access-control-expose-headers").unwrap_or_default();
    assert!(exposed.contains("fennec-unsent"), "{exposed}");
    // The preflight before it never reached the node: the router says
    // what its answers say, and the page sends the request.
    let r = ask(port, "OPTIONS", "/t/acme/query", Some(APP), PREFLIGHT);
    assert_eq!(r.status, 204, "{}", r.body);
    r.tells(APP);
}

/// A standby whose directory has not arrived asks to be asked again: a page
/// reads the 503 and its wait, in the header and in the body.
#[test]
fn a_standby_without_its_directory_tells_a_page_when_to_come_back() {
    let mut db = Database::new();
    db.follow(Vec::new()).unwrap();
    let dir = Directory::load(Arc::new(RwLock::new(db))).unwrap();
    assert!(!dir.arrived());
    let port = router(dir, Some(APP), 0);
    let r = ask(port, "GET", "/t/acme/collections", Some(APP), "");
    assert_eq!(r.status, 503, "{}", r.body);
    r.tells(APP);
    assert_eq!(r.one("retry-after"), Some("1"));
    assert!(r.body.contains(r#""retry_after":1"#), "{}", r.body);
    let r = ask(port, "OPTIONS", "/t/acme/collections", Some(APP), PREFLIGHT);
    assert_eq!(r.status, 204);
    r.tells(APP);
}

#[test]
fn the_connection_ceiling_tells_a_page_when_to_come_back() {
    let port = router(Directory::in_memory(), Some(APP), 1);
    // One connection held open, answered once so it is surely counted.
    let mut held = TcpStream::connect(("127.0.0.1", port)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(held, "GET /nosuch HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut first = [0u8; 12];
    held.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"HTTP/1.1 404");

    // The refusal is written as the connection is taken, before a request
    // is read; closed with it, a request written after it met the closed
    // socket (`EPIPE`) one run in about sixty, and is read off now
    // (`a_request_written_after_the_routers_refusal_...`).
    let r = ask(port, "GET", "/nosuch", Some(APP), "");
    assert_eq!(r.status, 503, "{}", r.body);
    r.tells(APP);
    assert_eq!(r.one("retry-after"), Some("1"));
    assert!(r.body.contains(r#""retry_after":1"#), "{}", r.body);
    drop(held);
}

/// A client past the router's ceiling writing its request as the answer
/// comes -- a request sent after its answer and end included, head and
/// body apart as Python's `http.client` writes them -- is read off and its
/// connection ended with no reset. Closed with its answer, the router's
/// socket reset the head and the body's write failed `EPIPE`, the 503
/// unread: 300 such clients writing through a router at a ceiling of 100
/// read 2 939 503s and 22 957 broken pipes of 25 921 refusals (Linux). The
/// body is larger than a send buffer -- macOS's 128 KiB, Linux's first
/// 16 KiB -- so written to a socket that is gone, its write waits for room
/// that never comes and the reset ends it every time.
#[test]
fn a_request_written_after_the_routers_refusal_is_read_off_not_reset() {
    const BODY: usize = 512 << 10;
    let port = router(Directory::in_memory(), None, 1);
    let mut held = TcpStream::connect(("127.0.0.1", port)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(held, "GET /nosuch HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut first = [0u8; 12];
    held.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"HTTP/1.1 404");
    for i in 0..3 {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        // The answer and its end: where the refusal closed the socket, it
        // had closed it by then.
        let r = read_answer(&mut c);
        assert_eq!(r.status, 503, "{}", r.body);
        assert_eq!(r.one("retry-after"), Some("1"));
        let head = format!(
            "POST /t/acme/query HTTP/1.1\r\nHost: localhost\r\nContent-Length: {BODY}\r\n\r\n"
        );
        c.write_all(head.as_bytes())
            .unwrap_or_else(|e| panic!("{i}: the head: {e}"));
        c.write_all(&vec![b' '; BODY])
            .unwrap_or_else(|e| panic!("{i}: the body: {e}"));
        c.shutdown(std::net::Shutdown::Write)
            .unwrap_or_else(|e| panic!("{i}: the end: {e}"));
        // Over either way -- the end acknowledged, or reset -- a
        // connection has no peer.
        let by = Instant::now() + Duration::from_secs(30);
        while c.peer_addr().is_ok() {
            assert!(Instant::now() < by, "the connection never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(c.take_error().unwrap().is_none(), "{i}: reset");
    }
    drop(held);
}

/// The router answers a preflight itself, before the directory is read: a
/// tenant it does not hold is the request's 404, not the preflight's.
#[test]
fn a_preflight_is_the_routers_to_answer() {
    let port = router(Directory::in_memory(), Some(APP), 0);
    let r = ask(port, "OPTIONS", "/t/nosuch/query", Some(APP), PREFLIGHT);
    assert_eq!(r.status, 204, "{}", r.body);
    r.tells(APP);
    assert!(r.one("x-request-id").is_some(), "{}", r.head);
    let r = ask(port, "OPTIONS", "/_shard/tenants", Some(APP), PREFLIGHT);
    assert_eq!(r.status, 204);
    r.tells(APP);

    // Another origin's goes where it went before: the router lets it in
    // no further than its answers do.
    let r = ask(port, "OPTIONS", "/t/nosuch/query", Some(EVIL), PREFLIGHT);
    assert_eq!(r.status, 404);
    assert_eq!(r.one("access-control-allow-origin"), Some(APP));
    let r = ask(port, "OPTIONS", "/_shard/tenants", Some(EVIL), PREFLIGHT);
    assert_eq!(r.status, 401);
    assert_eq!(r.one("access-control-allow-origin"), Some(APP));

    // `*` lets every origin's in.
    let port = router(Directory::in_memory(), Some("*"), 0);
    let r = ask(port, "OPTIONS", "/t/nosuch/query", Some(EVIL), PREFLIGHT);
    assert_eq!(r.status, 204);
    r.tells("*");
}

/// A node allowing `*` behind a router allowing one origin: both kept,
/// the answer named two origins, and a browser refuses an answer with two
/// `Access-Control-Allow-Origin`s whole. The router's own goes out in
/// place of the node's, on an answer and on a stream's head.
#[test]
fn a_forwarded_answer_tells_the_routers_rule_once() {
    let n = node("fwd", Some("*"));
    let port = in_front_of(&n, Some(APP));
    let r = ask(port, "GET", "/t/acme/collections", Some(APP), "");
    assert_eq!(r.status, 200, "{}", r.body);
    r.tells(APP);

    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let create = "create collection notes (title text)";
    let body = format!("{{\"query\":\"{create}\"}}");
    write!(
        s,
        "POST /t/acme/query HTTP/1.1\r\nHost: localhost\r\nOrigin: {APP}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let r = read_answer(&mut s);
    assert_eq!(r.status, 200, "{}", r.body);
    r.tells(APP);

    // A subscription's head, copied from the node's.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET /t/acme/notes/changes HTTP/1.1\r\nHost: localhost\r\nOrigin: {APP}\r\n\r\n"
    )
    .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let r = Answer {
        status: 200,
        head: String::from_utf8(head).unwrap(),
        body: String::new(),
    };
    assert!(r.head.starts_with("HTTP/1.1 200"), "{}", r.head);
    assert!(r.head.contains("text/event-stream"), "{}", r.head);
    r.tells(APP);
}

/// Without `--http-cors` the router is what it was: none of its own, and a
/// node's passed through as the node wrote them.
#[test]
fn without_http_cors_the_nodes_headers_pass_through() {
    let n = node("off", Some("*"));
    let port = in_front_of(&n, None);
    let r = ask(port, "GET", "/t/acme/collections", Some(APP), "");
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.one("access-control-allow-origin"), Some("*"));
    let r = ask(port, "GET", "/t/nosuch/collections", Some(APP), "");
    assert_eq!(r.status, 404);
    r.tells_nothing();
    // The preflight goes to the node, which answers it.
    let r = ask(port, "OPTIONS", "/t/acme/query", Some(APP), PREFLIGHT);
    assert_eq!(r.status, 204, "{}", r.body);
    assert_eq!(r.one("access-control-allow-origin"), Some("*"));
    let r = ask(port, "OPTIONS", "/t/nosuch/query", Some(APP), PREFLIGHT);
    assert_eq!(r.status, 404);
    r.tells_nothing();
}

/// A router with no `--token`, answering browsers from `cors`, its port.
fn open_router(cors: Option<&str>) -> u16 {
    fennec_http::audit::set_delay(0);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        cors: cors.map(String::from),
        ..Config::default()
    };
    let router = Router::new(Directory::in_memory(), cfg);
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    port
}

/// One request on a connection of its own, under `host`, with the header
/// lines `extra` and `body`.
fn send(port: u16, method: &str, target: &str, host: &str, extra: &str, body: &str) -> Answer {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    read_answer(&mut s)
}

/// A router with no `--token` answers `/_shard/` -- the directory, every
/// node's admin token in it, a query across every tenant, a tenant's
/// delete -- to whoever reaches it: held, as a server asking for no token
/// is, to this machine's names and its own pages or `--http-cors`'s, on
/// every route, a forward's too.
#[test]
fn an_open_router_answers_only_this_machines_name_and_its_pages() {
    let n = node("open-router", None);
    let port = open_router(Some(APP));
    let local = "localhost";
    let body = format!(r#"{{"addr":"{}","token":"adm"}}"#, n.addr);
    let r = send(port, "PUT", "/_shard/nodes/n1", local, "", &body);
    assert_eq!(r.status, 201, "{}", r.body);

    // A rebound name reads nothing of the directory, writes nothing to it.
    let rebound = "evil.example:8090";
    let r = send(port, "GET", "/_shard/nodes", rebound, "", "");
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(!r.body.contains("adm"), "{}", r.body);
    assert!(r.body.contains("or give it --token"), "{}", r.body);
    let r = send(port, "PUT", "/_shard/tenants/acme", rebound, "", "");
    assert_eq!(r.status, 403, "{}", r.body);
    let q = r#"{"query":"collections"}"#;
    let r = send(port, "POST", "/_shard/query", rebound, "", q);
    assert_eq!(r.status, 403, "{}", r.body);
    // Another site's page, by a simple request.
    let evil = format!("Origin: {EVIL}\r\nContent-Type: text/plain\r\n");
    let r = send(port, "PUT", "/_shard/tenants/acme", local, &evil, "");
    assert_eq!(r.status, 403, "{}", r.body);
    let r = send(port, "GET", "/_shard/tenants", local, "", "");
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains("acme"), "{}", r.body);

    // This machine's name, a client with no origin and the page
    // `--http-cors` names are answered.
    let r = send(port, "PUT", "/_shard/tenants/acme", local, "", "");
    assert_eq!(r.status, 201, "{}", r.body);
    let app = format!("Origin: {APP}\r\n");
    let r = send(port, "GET", "/_shard/tenants", "127.0.0.1", &app, "");
    assert_eq!(r.status, 200, "{}", r.body);
    r.tells(APP);

    // A forward is held to it too, at the router.
    let make = r#"{"query":"create collection notes (title text)"}"#;
    let r = send(port, "POST", "/t/acme/query", local, &app, make);
    assert_eq!(r.status, 200, "{}", r.body);
    let r = send(port, "GET", "/t/acme/notes", rebound, "", "");
    assert_eq!(r.status, 403, "{}", r.body);
    let drop = r#"{"query":"drop collection notes"}"#;
    let r = send(port, "POST", "/t/acme/query", local, &evil, drop);
    assert_eq!(r.status, 403, "{}", r.body);
    let r = send(port, "GET", "/t/acme/collections", local, "", "");
    assert!(r.body.contains("\"notes\""), "{}", r.body);
}

/// A node with no token asks a page's origin, and a router in front of it
/// with `--http-cors` answers for the page: the origin it lets in goes to
/// the node unnamed, which has no `--http-cors` of its own, and any other
/// goes on for the node to refuse.
#[test]
fn a_router_answers_for_the_origins_it_lets_in_to_an_open_node() {
    let n = node("vouched", None);
    let port = in_front_of(&n, Some(APP));
    let app = format!("Origin: {APP}\r\n");
    let make = r#"{"query":"create collection notes (title text)"}"#;
    let r = send(port, "POST", "/t/acme/query", "localhost", &app, make);
    assert_eq!(r.status, 200, "{}", r.body);
    r.tells(APP);
    // The router's own page under a name of this machine: its studio's.
    let own = format!("Origin: http://127.0.0.1:{port}\r\n");
    let host = format!("127.0.0.1:{port}");
    let r = send(port, "GET", "/t/acme/notes", &host, &own, "");
    assert_eq!(r.status, 200, "{}", r.body);

    let evil = format!("Origin: {EVIL}\r\nContent-Type: text/plain\r\n");
    let drop = r#"{"query":"drop collection notes"}"#;
    let r = send(port, "POST", "/t/acme/query", "localhost", &evil, drop);
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(r.body.contains("--http-cors"), "{}", r.body);
    let r = send(port, "GET", "/t/acme/collections", "localhost", "", "");
    assert!(r.body.contains("\"notes\""), "{}", r.body);
}

/// A router with `--token` asks a page nothing of its own routes: a page
/// holds no token. A forward's node judges the page, told the name the
/// router was reached by (below).
#[test]
fn a_router_with_a_token_asks_a_page_nothing_of_its_own_routes() {
    let port = router(Directory::in_memory(), None, 0);
    let root = "Authorization: Bearer root\r\n";
    let r = send(
        port,
        "GET",
        "/_shard/nodes",
        "router.example:8090",
        root,
        "",
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let evil = format!("{root}Origin: {EVIL}\r\n");
    let r = send(port, "GET", "/_shard/tenants", "router.example", &evil, "");
    assert_eq!(r.status, 200, "{}", r.body);
}

/// An open router without `--http-cors` tells a page the node's rule,
/// passed through: a forward's origin is the node's to judge, its `Host`
/// the router's, and the router's own routes answer no page of another
/// origin.
#[test]
fn an_open_router_without_http_cors_leaves_a_forwards_origin_to_its_node() {
    let n = node("open-plain", Some(APP));
    let port = open_router(None);
    let body = format!(r#"{{"addr":"{}","token":"adm"}}"#, n.addr);
    let r = send(port, "PUT", "/_shard/nodes/n1", "localhost", "", &body);
    assert_eq!(r.status, 201, "{}", r.body);
    let r = send(port, "PUT", "/_shard/tenants/acme", "localhost", "", "");
    assert_eq!(r.status, 201, "{}", r.body);

    let app = format!("Origin: {APP}\r\n");
    let make = r#"{"query":"create collection notes (title text)"}"#;
    let r = send(port, "POST", "/t/acme/query", "localhost", &app, make);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.one("access-control-allow-origin"), Some(APP));
    // Another origin, refused by the node, its answer passed through.
    let evil = format!("Origin: {EVIL}\r\nContent-Type: text/plain\r\n");
    let drop = r#"{"query":"drop collection notes"}"#;
    let r = send(port, "POST", "/t/acme/query", "localhost", &evil, drop);
    assert_eq!(r.status, 403, "{}", r.body);
    // A rebound name, refused by the router.
    let r = send(port, "GET", "/t/acme/notes", "evil.example", "", "");
    assert_eq!(r.status, 403, "{}", r.body);
    assert!(r.body.contains("give it --token"), "{}", r.body);
    // The router's own routes are its own: no page of another origin.
    let r = send(port, "GET", "/_shard/tenants", "localhost", &app, "");
    assert_eq!(r.status, 403, "{}", r.body);
    let r = send(port, "GET", "/t/acme/collections", "localhost", "", "");
    assert!(r.body.contains("\"notes\""), "{}", r.body);
}

/// A router with `--token` asks a page nothing on a forward, and a node
/// with no data token saw only the router's address for itself: a page
/// whose name was rebound to the router read a tenant's rows, its streams
/// and `/_changes` through it. The router names the `Host` its client
/// reached it by in `X-Forwarded-Host`, in place of any the client sent,
/// and the node asks that as it asks its own.
#[test]
fn a_token_router_tells_an_open_node_the_name_its_client_reached_it_by() {
    let n = node("forwarded-host", None);
    let port = in_front_of(&n, None);
    let make = r#"{"query":"create collection notes (title text)"}"#;
    let r = send(port, "POST", "/t/acme/query", "localhost", "", make);
    assert_eq!(r.status, 200, "{}", r.body);

    let rebound = "evil.example:8090";
    for target in [
        "/t/acme/notes",
        "/t/acme/notes/changes",
        "/t/acme/_changes",
        "/t/acme/collections",
    ] {
        let r = send(port, "GET", target, rebound, "", "");
        assert_eq!(r.status, 403, "{target}: {}", r.body);
        assert!(r.body.contains("Host `evil.example:8090`"), "{}", r.body);
        assert!(r.body.contains("give it --http-token"), "{}", r.body);
        // One naming this machine, sent by the page itself, is no use.
        let forged = "X-Forwarded-Host: localhost\r\n";
        let r = send(port, "GET", target, rebound, forged, "");
        assert_eq!(r.status, 403, "{target}: {}", r.body);
    }
    // A write, a same-origin page's sending no `Origin`.
    let drop = r#"{"query":"drop collection notes"}"#;
    let r = send(port, "POST", "/t/acme/query", rebound, "", drop);
    assert_eq!(r.status, 403, "{}", r.body);

    // This machine's names pass, through the router as straight to the
    // node; a client's own header naming another is replaced, as any is.
    let own = format!("127.0.0.1:{port}");
    for host in ["localhost", "app.localhost:8090", own.as_str()] {
        let r = send(port, "GET", "/t/acme/notes", host, "", "");
        assert_eq!(r.status, 200, "{host}: {}", r.body);
    }
    let other = "X-Forwarded-Host: evil.example\r\n";
    let r = send(port, "GET", "/t/acme/notes", "localhost", other, "");
    assert_eq!(r.status, 200, "{}", r.body);
    let r = send(port, "GET", "/t/acme/collections", "localhost", "", "");
    assert!(r.body.contains("\"notes\""), "{}", r.body);
}

/// A relay to the home region's router carries the name the first
/// router's client reached it by: the router it relays to keeps it, the
/// request marked by the `--token` both hold, and its node is told the
/// page's name rather than the first router's for the second. Sent to
/// that router straight, a client's own is replaced as at any.
#[test]
fn a_relay_keeps_the_name_its_client_reached_the_first_router_by() {
    use crate::unsent::{read_request, ANSWERED};
    use std::io::BufReader;
    use std::net::TcpListener;
    use std::sync::mpsc;

    // A stand-in node, answering every request and handing on its head.
    let stand_in = TcpListener::bind("127.0.0.1:0").unwrap();
    let (heads, asked) = mpsc::channel::<String>();
    let at = stand_in.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for conn in stand_in.incoming() {
            let Ok(conn) = conn else { return };
            let heads = heads.clone();
            std::thread::spawn(move || {
                let mut conn = BufReader::new(conn);
                loop {
                    let head = read_request(&mut conn);
                    if heads.send(head).is_err() || conn.get_mut().write_all(ANSWERED).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let forwarded = |head: &str| -> Vec<String> {
        head.lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("x-forwarded-host"))
            .map(|(_, v)| v.trim().to_string())
            .collect()
    };
    let next = || asked.recv_timeout(Duration::from_secs(30)).unwrap();

    // The home region's router, holding `acme` on the stand-in.
    let home = router(placed_at(&at), None, 0);
    // The router in front, relaying `acme` to home's.
    let mut d = Directory::in_memory();
    d.set_region("us", Some(&[format!("http://127.0.0.1:{home}")]))
        .unwrap();
    d.place_home("acme", "us").unwrap();
    let front = router(d, None, 0);

    let forged = "X-Forwarded-Host: localhost\r\n";
    let r = send(
        front,
        "GET",
        "/t/acme/notes",
        "evil.example:8090",
        forged,
        "",
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let head = next();
    assert_eq!(forwarded(&head), ["evil.example:8090"], "{head}");
    // Its own `Host` is its address, as a node's is.
    assert!(head.contains(&format!("Host: {at}")), "{head}");

    let r = send(
        home,
        "GET",
        "/t/acme/notes",
        "localhost",
        "X-Forwarded-Host: x.example\r\n",
        "",
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let head = next();
    assert_eq!(forwarded(&head), ["localhost"], "{head}");
}
