//! `--request-rate` (`docs/design/quotas.md`, *A caller's request rate*):
//! a scoped caller's requests to a database are held to the rate on every
//! route, refused 429 with the wait before anything of them runs; another
//! caller, the same caller in another tenant, the server's token and a
//! server told 0 are not held.

use fennec_http::access::Access;
use fennec_http::tenants::Tenants;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const POLICY: &str = "notes  read,write  where owner = $jwt.sub\nmine  call\n";

/// Tries a caller gets to spend its bucket in: at one a second, a request
/// every 20 ms of fifty spends it whatever else the machine is doing.
const TRIES: usize = 50;

struct Node {
    port: u16,
    access: Arc<Access>,
    tenants: Arc<Tenants>,
}

/// A node of two tenants, `acme` and `globex`, each with `notes` and the
/// statement `mine`, its scoped callers held to `rate` requests a second.
fn node(rate: u32) -> Node {
    fennec_http::audit::set_delay(0);
    // A node a directory: the tests run at once, and macOS's clock reads
    // to the microsecond, two of them starting together named one.
    static NODES: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-request-rate-{}-{}-{}",
        std::process::id(),
        NODES.fetch_add(1, Ordering::Relaxed),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let server = Server::with_tenants(
        Arc::clone(&tenants),
        Config {
            addr: "127.0.0.1:0".into(),
            token: Some(ROOT.into()),
            access: Some(Arc::clone(&access)),
            admin_token: Some("adm".into()),
            mcp: Some(Arc::new(fennec_http::mcp::Options::default())),
            call_rate: 0,
            request_rate: rate,
            ..Config::default()
        },
    );
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let node = Node {
        port,
        access,
        tenants,
    };
    for t in ["acme", "globex"] {
        node.tenants.create(t).unwrap();
        for q in [
            "create collection notes (owner text, title text)",
            "create statement mine() claims (sub text) as \
             get notes select id where owner = $jwt.sub",
        ] {
            let a = node.ask(ROOT, "POST", &format!("/t/{t}/query"), &query(q));
            assert_eq!(a.status, 200, "{q}: {}", a.body);
        }
    }
    node
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

fn query(q: &str) -> String {
    format!(r#"{{"query":{}}}"#, json(q))
}

fn json(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Node {
    /// One request on a connection of its own. A stream's head alone is
    /// read: a subscription answered 200 goes on, and is let go.
    fn ask(&self, token: &str, method: &str, target: &str, body: &str) -> Answer {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        let stream = target.ends_with("/changes");
        loop {
            let n = s.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&out);
            if stream && text.starts_with("HTTP/1.1 200") && text.contains("\r\n\r\n") {
                break;
            }
        }
        let out = String::from_utf8_lossy(&out).into_owned();
        let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        Answer {
            status: out[9..12].parse().unwrap(),
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    fn token(&self, sub: &str, tenants: &str) -> String {
        let claims = format!(r#"{{"sub":"{sub}","tenant":{tenants}}}"#);
        self.access.mint(&claims).unwrap()
    }

    /// The first 429 `token` is answered asking `route` again and again,
    /// each answer before it a 200: `None` where none came in [`TRIES`].
    fn held(&self, token: &str, (method, target, body): (&str, &str, &str)) -> Option<Answer> {
        for _ in 0..TRIES {
            let a = self.ask(token, method, target, body);
            match a.status {
                200 => continue,
                429 => return Some(a),
                s => panic!("{method} {target}: {s} {}", a.body),
            }
        }
        None
    }

    fn notes(&self, tenant: &str) -> usize {
        let a = self.ask(
            ROOT,
            "POST",
            &format!("/t/{tenant}/query"),
            &query("get notes"),
        );
        assert_eq!(a.status, 200, "{}", a.body);
        a.body.matches("\"owner\"").count()
    }
}

/// The number after `"<field>":` in a body.
fn number(body: &str, field: &str) -> u64 {
    let at = body
        .find(&format!("\"{field}\":"))
        .unwrap_or_else(|| panic!("{field}: {body}"));
    let digits: String = body[at + field.len() + 3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or_else(|_| panic!("{field}: {body}"))
}

/// Past the rate a write is answered 429 naming its wait in the header and
/// the body, and does not land: every note there is a 200's. The wait it
/// names is enough: asked again after it, the request is answered.
#[test]
fn a_caller_past_the_rate_is_told_its_wait_and_nothing_of_it_runs() {
    let node = node(1);
    let alice = node.token("alice", r#""acme""#);
    let put = query("put notes {owner: 'alice', title: 'n'}");
    let mut landed = 0;
    let mut held = None;
    for _ in 0..TRIES {
        let a = node.ask(&alice, "POST", "/t/acme/query", &put);
        match a.status {
            200 => landed += 1,
            429 => {
                held = Some(a);
                break;
            }
            s => panic!("{s}: {}", a.body),
        }
    }
    let held = held.expect("a caller past --request-rate is held");
    assert!(held.head.contains("Retry-After: 1\r\n"), "{}", held.head);
    assert!(held.body.contains("--request-rate"), "{}", held.body);
    assert_eq!(number(&held.body, "retry_after"), 1, "{}", held.body);
    assert!(!held.body.contains("\"completed\""), "{}", held.body);
    let wait = number(&held.body, "retry_after_ms");
    assert!((1..=1000).contains(&wait), "{}", held.body);
    assert_eq!(node.notes("acme"), landed, "the refused write landed");
    std::thread::sleep(Duration::from_millis(wait));
    let a = node.ask(&alice, "POST", "/t/acme/query", &put);
    assert_eq!(a.status, 200, "asked again after its wait: {}", a.body);
    // Counted where a scrape reads it, beside every other test's.
    let m = node.ask("adm", "GET", "/_metrics", "");
    let line = (m.body.lines())
        .find(|l| l.starts_with("fennec_request_rate_refused_total "))
        .unwrap_or_else(|| panic!("{}", m.body));
    let refused: u64 = line.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(refused >= 1, "{line}");
}

/// Every route a scoped token reaches is a request: a collection's read,
/// `/query`, `/batch`, a call by its path -- a call charged as a request
/// whatever `--call-rate` says --, `/_whoami`, a subscription's opening
/// and MCP. Each a caller of its own, so each route is what spends it.
#[test]
fn every_route_a_scoped_token_reaches_is_held() {
    let node = node(1);
    let mcp = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let batch = [query("get notes"), query("get notes")].join("\n");
    let notes = query("get notes");
    let routes = [
        ("GET", "/t/acme/notes", ""),
        ("POST", "/t/acme/query", notes.as_str()),
        ("POST", "/t/acme/batch", batch.as_str()),
        ("POST", "/t/acme/_call/mine", r#"{"params":{}}"#),
        ("GET", "/t/acme/_whoami", ""),
        ("GET", "/t/acme/notes/changes", ""),
        ("POST", "/t/acme/_mcp", mcp),
    ];
    for (i, route) in routes.into_iter().enumerate() {
        let caller = node.token(&format!("route{i}"), r#""acme""#);
        let held = node.held(&caller, route);
        let held = held.unwrap_or_else(|| panic!("{} {}: never held", route.0, route.1));
        assert!(held.head.contains("Retry-After: "), "{}", held.head);
        assert!(held.body.contains("--request-rate"), "{}", held.body);
    }
}

/// A caller is held in the database it asks, by its subject: alice held in
/// one tenant asks the other at once, and bob, beside her in the first, is
/// answered.
#[test]
fn callers_and_tenants_are_held_apart() {
    let node = node(1);
    let alice = node.token("alice", r#"["acme","globex"]"#);
    let bob = node.token("bob", r#""acme""#);
    let read = ("GET", "/t/acme/notes", "");
    assert!(node.held(&alice, read).is_some(), "alice is held in acme");
    let a = node.ask(&bob, "GET", "/t/acme/notes", "");
    assert_eq!(a.status, 200, "bob: {}", a.body);
    let a = node.ask(&alice, "GET", "/t/globex/notes", "");
    assert_eq!(a.status, 200, "alice in globex: {}", a.body);
}

/// The server's token is never held, and `--request-rate 0` holds no one.
#[test]
fn the_server_token_and_a_rate_of_zero_are_unheld() {
    let held = node(1);
    let open = node(0);
    let alice = open.token("alice", r#""acme""#);
    for _ in 0..20 {
        let a = held.ask(ROOT, "GET", "/t/acme/notes", "");
        assert_eq!(a.status, 200, "the server's token: {}", a.body);
        let a = open.ask(&alice, "GET", "/t/acme/notes", "");
        assert_eq!(a.status, 200, "--request-rate 0: {}", a.body);
    }
}
