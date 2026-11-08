//! The router's OpenAPI document (fennec-http's `openapi.rs`, `Kind::Router`)
//! held to a router: every operation it lists -- a tenant's, forwarded to
//! its node, and the router's own -- is sent to a router in front of a
//! node, answered neither 404 nor 405; each one whose `security` asks a
//! token is 401 without one, and one asking none is not; a GET it lists no
//! HEAD beside answers HEAD 404 or 405; one it marks `deprecated` answers
//! `Deprecation`, and no other does. fennec-server's `tests/openapi.rs`
//! holds a file's server and a node to theirs.

use fennec_core::value::Value;
use fennec_http::openapi::{document, Kind};
use fennec_http::replication::{fresh_id, Replication};
use fennec_http::tenants::{Replicated, Tenants};
use fennec_shard::directory::Directory;
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// The node's data token.
const ROOT: &str = "root";
const ADMIN: &str = "adm";
/// The router's `--token`.
const ROUTER: &str = "rtr";
/// The node's tenants' and the router's directory's replication token.
const REPLICATION: &str = "rep";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn members(v: Option<&Value>) -> &[(String, Value)] {
    match v {
        Some(Value::Object(m)) => m,
        _ => &[],
    }
}

/// A node over `dir` replicating its tenants, with the MCP endpoint: its
/// address.
fn node(dir: &std::path::Path) -> String {
    fennec_http::audit::set_delay(0);
    let tenants = Arc::new(Tenants::new(dir).unwrap().with_replication(Replicated {
        token: REPLICATION.into(),
        buffer: 8 << 20,
        upstream: None,
        sync_on_write: false,
    }));
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some(ADMIN.into()),
        token: Some(ROOT.into()),
        mcp: Some(Arc::new(fennec_http::mcp::Options::default())),
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(tenants, cfg);
    let listener = server.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    addr
}

/// A router whose directory at `path` is replicated, with the studio: its
/// port.
fn router(path: &std::path::Path) -> u16 {
    let (mut db, feed) = fennec_http::replication::open(path.to_str().unwrap(), 8 << 20).unwrap();
    db.fork(fresh_id()).unwrap();
    db.sync().unwrap();
    let dir = Directory::load(Arc::new(RwLock::new(db))).unwrap();
    let repl = Replication::new(Some(REPLICATION.into()), Some(feed), None);
    let studio = fennec_http::studio::Studio::new(fennec_http::studio::ASSETS, None).unwrap();
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROUTER.into()),
        upstream_timeout: Duration::from_secs(10),
        studio: Some(Arc::new(studio)),
        ..Config::default()
    };
    let router = Router::replicated(dir, cfg, repl);
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    port
}

/// The status of `method target`, on a connection of its own: its head
/// alone, so a stream's is enough.
fn status(port: u16, method: &str, target: &str, token: Option<&str>, body: &str) -> u16 {
    answer(port, method, target, token, body).0
}

/// The status of `method target`, and whether its head carries
/// `Deprecation`: the head alone, read to its blank line, never into a
/// stream's body.
fn answer(port: u16, method: &str, target: &str, token: Option<&str>, body: &str) -> (u16, bool) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut head = BufReader::new(s);
    let mut line = String::new();
    head.read_line(&mut line).unwrap();
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("{method} {target}: no status line: {line:?}"));
    let mut marked = false;
    loop {
        let mut h = String::new();
        if head.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
            return (status, marked);
        }
        marked |= h
            .split_once(':')
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case("deprecation"));
    }
}

/// The status and body of `method target`, read to its end.
fn call(port: u16, method: &str, target: &str, token: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, out)
}

struct Operation {
    method: String,
    target: String,
    /// Its id as a file's server names it: `tenantQuery` is `query`.
    id: String,
    token: Option<&'static str>,
    /// 401 with no token where its `security` asks one (or, asking none,
    /// it lists a 401: a hook's signature); `None` where `{}` is among
    /// what it takes.
    unauthorized: Option<bool>,
    body: bool,
    /// The document marks it `deprecated`: its answer, the node's relayed
    /// or the router's own, carries `Deprecation`.
    deprecated: bool,
}

fn operations(doc: &Value) -> Vec<Operation> {
    let mut out = Vec::new();
    for (path, item) in members(get(doc, "paths")) {
        for (method, op) in members(Some(item)) {
            if method == "parameters" {
                continue;
            }
            let Some(Value::Text(id)) = get(op, "operationId") else {
                panic!("{method} {path} has no operationId");
            };
            let id = match id.strip_prefix("tenant") {
                Some(rest) if rest.starts_with(char::is_uppercase) => {
                    let mut c = rest.chars();
                    c.next().unwrap().to_lowercase().chain(c).collect()
                }
                _ => id.clone(),
            };
            let security = match get(op, "security") {
                Some(Value::List(reqs)) => reqs.clone(),
                _ => vec![Value::Object(vec![("token".into(), Value::List(vec![]))])],
            };
            let scheme = security
                .first()
                .and_then(|r| members(Some(r)).first().map(|(k, _)| k.clone()));
            let token = match scheme.as_deref() {
                None => None,
                Some("token" | "jwt") => Some(ROOT),
                Some("router") => Some(ROUTER),
                Some("replication") => Some(REPLICATION),
                Some(other) => panic!("{method} {path}: a scheme a router does not take: {other}"),
            };
            let lists_401 = members(get(op, "responses"))
                .iter()
                .any(|(s, _)| s == "401");
            let unauthorized = match security.is_empty() {
                true => Some(lists_401),
                false if security.iter().any(|r| members(Some(r)).is_empty()) => None,
                false => Some(true),
            };
            let target = path
                .replace("{collection}", "c")
                .replace("{tenant}", "acme")
                .replace("{statement}", "s")
                .replace("{hook}", "stripe")
                .replace("{consumer}", "k")
                .replace("{file}", "app.js");
            out.push(Operation {
                method: method.to_ascii_uppercase(),
                target,
                id,
                token,
                unauthorized,
                body: get(op, "requestBody").is_some(),
                deprecated: get(op, "deprecated") == Some(&Value::Bool(true)),
            });
        }
    }
    // What makes a thing before what changes it, every delete last.
    let rank = |m: &str| {
        ["PUT", "POST", "GET", "HEAD", "PATCH", "DELETE"]
            .iter()
            .position(|r| *r == m)
    };
    out.sort_by_key(|o| (o.method == "DELETE", rank(&o.method)));
    out
}

fn body(id: &str) -> &'static str {
    match id {
        "insertRows" => r#"{"t":"x"}"#,
        "updateRows" | "updateRowsPut" | "updateAll" | "updateAllPut" => r#"{"t":"y"}"#,
        "nearRows" => r#"{"vector":[1,0],"limit":1}"#,
        "query" => r#"{"query":"get c"}"#,
        "batch" => "{\"query\":\"get c\"}\n",
        "moveConsumer" => r#"{"since":0}"#,
        "mcp" => r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        _ => "{}",
    }
}

#[test]
fn every_operation_of_the_routers_document_is_answered() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "fennec-shard-openapi-{}-{:?}",
        std::process::id(),
        Instant::now()
    )));
    std::fs::create_dir_all(scratch.0.join("tenants")).unwrap();
    let addr = node(&scratch.0.join("tenants"));
    let port = router(&scratch.0.join("shard.fennec"));
    let placed = call(
        port,
        "PUT",
        "/_shard/nodes/n1",
        ROUTER,
        &format!(r#"{{"addr":"{addr}","token":"{ADMIN}"}}"#),
    );
    assert_eq!(placed.0, 201, "{}", placed.1);
    let made = call(port, "PUT", "/_shard/tenants/acme", ROUTER, "");
    assert_eq!(made.0, 201, "{}", made.1);
    for q in [
        "create collection c (t text, v vector<2>)",
        "put c {t: \"a\", v: [1, 0]}",
        "create statement s() as get c select t",
    ] {
        let mut body = String::from("{\"query\":");
        fennec_core::json::escape_into(&mut body, q);
        body.push('}');
        let r = call(port, "POST", "/t/acme/query", ROOT, &body);
        assert_eq!(r.0, 200, "{q}: {}", r.1);
    }

    let src = std::str::from_utf8(document(Kind::Router)).unwrap();
    let doc = fennec_core::json::parse_json(src).unwrap();
    let ops = operations(&doc);
    assert!(ops.len() >= 40, "{} operations", ops.len());
    for own in ["/_metrics", "/_studio/", "/_replication", "/_openapi.json"] {
        assert!(
            ops.iter().any(|o| o.target == own),
            "{own} is not in the router's document"
        );
    }
    let mut wrong = Vec::new();
    for op in &ops {
        let Some(asks) = op.unauthorized else {
            continue;
        };
        let body = if op.body { body(&op.id) } else { "" };
        let s = status(port, &op.method, &op.target, None, body);
        if (s == 401) != asks {
            let want = if asks { "401" } else { "not 401" };
            wrong.push(format!(
                "{s} {} {} with no token, {want}",
                op.method, op.target
            ));
        }
    }
    for op in ops.iter().filter(|o| o.method == "GET") {
        let listed = ops
            .iter()
            .any(|o| o.method == "HEAD" && o.target == op.target);
        if !listed {
            let s = status(port, "HEAD", &op.target, op.token, "");
            if s != 404 && s != 405 {
                wrong.push(format!(
                    "{s} HEAD {}: the document lists no HEAD",
                    op.target
                ));
            }
        }
    }
    let mut answered = Vec::new();
    let mut unrouted = false;
    for op in &ops {
        let body = if op.body { body(&op.id) } else { "" };
        let (s, marked) = answer(port, &op.method, &op.target, op.token, body);
        // A 502 or a 503 is a forward that never reached the node: the
        // run would hold nothing.
        unrouted |= matches!(s, 404 | 405 | 502 | 503);
        answered.push(format!("{s} {} {} ({})", op.method, op.target, op.id));
        // A refusal made before a deprecated route's handler ran may carry
        // no mark; no other route's answer ever carries one.
        if marked && !op.deprecated {
            wrong.push(format!(
                "{s} {} {}: answered Deprecation, which the document does not say",
                op.method, op.target
            ));
        } else if !marked && op.deprecated && s < 400 {
            wrong.push(format!(
                "{s} {} {}: deprecated in the document, answered without Deprecation",
                op.method, op.target
            ));
        }
    }
    assert!(
        !unrouted && wrong.is_empty(),
        "answers the router's document does not say:\n{}\n\nevery operation, one \
         answered 404, 405, 502 or 503 among them or not:\n{}",
        wrong.join("\n"),
        answered.join("\n")
    );
}
