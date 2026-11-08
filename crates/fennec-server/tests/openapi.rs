//! The OpenAPI document (fennec-http's `openapi.rs`) held to the routes
//! from the other side: every operation a file's server and a tenant node
//! list in the document they serve is sent to them, under every flag a
//! route is served with, and answered neither 404 nor 405; each one whose
//! `security` asks a token is 401 without one, and one asking none is not;
//! a GET the document lists no HEAD beside answers HEAD 404 or 405. Sent
//! with its token, its body and, for a hook, its sender's signature, every
//! operation reaches its handler -- none is refused before it, 401 -- and
//! one the document marks `deprecated` answers `Deprecation` whatever its
//! status, no other ever. A row of the document with no route behind it
//! fails here; a route with no row, fennec-http's
//! `every_route_the_server_matches_is_documented`.
//! fennec-shard's `tests/openapi.rs` sends the router's.
//!
//! The documents it read are left in `CARGO_TARGET_TMPDIR/openapi/`, the
//! router's beside them, for `make openapi-lint` to hand an OpenAPI
//! linter.

use crate::support::{start, tmp, Http, Server};
use fennec_core::value::Value;
use fennec_http::crypto::hmac_sha256;
use fennec_http::openapi::{document, Kind};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

const ROOT: &str = "root";
const ADMIN: &str = "adm";
const REPLICATION: &str = "rep";
/// The page origin `--http-cors` allows.
const APP: &str = "https://app.example";
/// The `stripe` hook's secret, as Stripe shows it.
const STRIPE_SECRET: &str = "whsec_a_secret_stripe_shows";
/// The event the `stripe` hook is sent, signed: what `taken` inserts.
const EVENT: &str = r#"{"id":"evt_openapi","type":"openapi.sent"}"#;

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

struct Operation {
    method: String,
    path: String,
    /// Its id as a file's server names it: a node's `tenantQuery` is
    /// `query`.
    id: String,
    token: Option<&'static str>,
    /// What it answers with no token: 401 where its `security` asks one
    /// (or, asking none, it lists a 401 of its own: a hook's signature),
    /// never 401 where it asks none; `None` where `{}` is among what it
    /// takes, the server's flags deciding.
    unauthorized: Option<bool>,
    body: bool,
    /// The document marks it `deprecated`: its answer carries
    /// `Deprecation` (RFC 9745).
    deprecated: bool,
}

/// The document `server` serves, with no token, as it is: written where
/// `make openapi-lint` reads it.
fn served(server: &Server, name: &str) -> Value {
    let a = server.http().ask("GET", "/_openapi.json", "");
    assert_eq!(a.status, 200, "{}", a.body);
    assert_eq!(
        a.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("openapi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.json")), &a.body).unwrap();
    std::fs::write(dir.join("router.json"), document(Kind::Router)).unwrap();
    fennec_core::json::parse_json(&a.body).expect("the document reads as JSON")
}

/// A page on the allowed origin reads the document: its preflight is 204
/// with the CORS headers, as every path's is, and the GET carries them.
fn a_page_reads_the_document(server: &Server) {
    let mut http = server.http();
    let asked = http.ask_with(
        "OPTIONS",
        "/_openapi.json",
        "",
        &[("Origin", APP), ("Access-Control-Request-Method", "GET")],
    );
    assert_eq!(asked.status, 204, "{}", asked.body);
    assert_eq!(asked.header("access-control-allow-origin"), Some(APP));
    assert!(asked
        .header("access-control-allow-methods")
        .is_some_and(|m| m.contains("GET")));
    let read = http.ask_with("GET", "/_openapi.json", "", &[("Origin", APP)]);
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.header("access-control-allow-origin"), Some(APP));
    // And reads a deprecated route's marks, which no list safelists.
    let exposed = read.header("access-control-expose-headers").unwrap_or("");
    for h in ["deprecation", "sunset", "link"] {
        assert!(exposed.split(", ").any(|e| e == h), "{h}: {exposed}");
    }
}

/// Every operation of `doc`, in the order it is sent: what makes a thing
/// before what changes it, a node's administration after its tenants'
/// routes, every delete last and the tenant's own after its hooks'.
fn operations(doc: &Value) -> Vec<Operation> {
    let mut out = Vec::new();
    for (path, item) in members(get(doc, "paths")) {
        for (method, op) in members(Some(item)) {
            if method == "parameters" {
                continue;
            }
            let id = match get(op, "operationId") {
                Some(Value::Text(id)) => id.clone(),
                _ => panic!("{method} {path} has no operationId"),
            };
            let id = match id.strip_prefix("tenant") {
                Some(rest) if rest.starts_with(char::is_uppercase) => {
                    let mut c = rest.chars();
                    c.next().unwrap().to_lowercase().chain(c).collect()
                }
                _ => id,
            };
            // The first scheme an operation takes, the document's where it
            // names none of its own; `[]` none at all.
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
                Some("admin") => Some(ADMIN),
                Some("replication") => Some(REPLICATION),
                Some(other) => panic!("{method} {path}: a scheme the test knows not: {other}"),
            };
            let lists_401 = members(get(op, "responses"))
                .iter()
                .any(|(s, _)| s == "401");
            let unauthorized = match security.is_empty() {
                true => Some(lists_401),
                false if security.iter().any(|r| members(Some(r)).is_empty()) => None,
                false => Some(true),
            };
            out.push(Operation {
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                id,
                token,
                unauthorized,
                body: get(op, "requestBody").is_some(),
                deprecated: get(op, "deprecated") == Some(&Value::Bool(true)),
            });
        }
    }
    // A JSON object's members are read in key order, not the document's:
    // what makes a thing goes by its method. A `PUT` before a `POST`, so a
    // hook is set before its tenant is frozen, and a freeze thawed after.
    let rank = |m: &str| {
        ["PUT", "POST", "GET", "HEAD", "PATCH", "DELETE"]
            .iter()
            .position(|r| *r == m)
    };
    out.sort_by_key(|o| {
        (
            o.method == "DELETE",
            o.path == "/_admin/tenants/{tenant}",
            o.path.starts_with("/_admin"),
            rank(&o.method),
        )
    });
    out
}

/// A body each operation can be answered by; `{}` for the rest, which
/// either reads or is refused as a body, never as a path.
fn body(id: &str) -> &'static str {
    match id {
        "insertRows" => r#"{"t":"x"}"#,
        "updateRows" | "updateRowsPut" | "updateAll" | "updateAllPut" | "updateRow" | "putRow" => {
            r#"{"t":"y"}"#
        }
        "nearRows" => r#"{"vector":[1,0],"limit":1,"where":"t != \"z\"","select":["t"]}"#,
        "query" | "adminQuery" => r#"{"query":"get c"}"#,
        "batch" => "{\"query\":\"get c\"}\n",
        "moveConsumer" => r#"{"since":0}"#,
        "mcp" => r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        "adminSetHook" => r#"{"scheme":"stripe","secrets":["whsec_a_secret_stripe_shows"]}"#,
        "hook" => EVENT,
        _ => "{}",
    }
}

/// The headers an operation is sent beside its token: a hook's event its
/// sender's signature, so its handler -- never the refusal before it --
/// answers it.
fn signed(id: &str) -> Vec<(&'static str, String)> {
    if id != "hook" {
        return Vec::new();
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac = hmac_sha256(STRIPE_SECRET.as_bytes(), format!("{t}.{EVENT}").as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    vec![("Stripe-Signature", format!("t={t},v1={hex}"))]
}

/// The path with its placeholders named as the fixtures are.
fn filled(path: &str) -> String {
    path.replace("{collection}", "c")
        .replace("{id}", "1")
        .replace("{tenant}", "acme")
        .replace("{statement}", "s")
        .replace("{hook}", "stripe")
        .replace("{consumer}", "k")
        .replace("{file}", "app.js")
}

/// The status `method path` is answered with, on a connection of its own:
/// its head alone, so a stream's is enough.
fn status(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> u16 {
    answer(port, method, path, token, body, &[]).0
}

/// The status `method path` is answered with, sent `headers` too, and
/// whether its head carries `Deprecation`: the head alone, read to its
/// blank line.
fn answer(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
    headers: &[(&str, String)],
) -> (u16, bool) {
    let target = filled(path);
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut more = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    for (name, value) in headers {
        more.push_str(&format!("{name}: {value}\r\n"));
    }
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\n{more}Content-Type: application/json\r\n\
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
    (status, deprecation(&mut head))
}

/// Whether the head left on `head`, past its status line, carries
/// `Deprecation`: read to its blank line, never into a stream's body.
fn deprecation(head: &mut impl BufRead) -> bool {
    let mut marked = false;
    loop {
        let mut h = String::new();
        if head.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
            return marked;
        }
        marked |= h
            .split_once(':')
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case("deprecation"));
    }
}

/// Holds the server on `port` to `doc`, the fixtures made: what a token
/// is asked for, what HEAD is answered on, then every operation sent.
fn every_operation_is_answered(port: u16, doc: &Value, least: usize) {
    let ops = operations(doc);
    assert!(ops.len() >= least, "{} operations", ops.len());
    let mut wrong = Vec::new();
    // With no token: refused before anything runs, so before the rest.
    for op in &ops {
        let body = if op.body { body(&op.id) } else { "" };
        let Some(asks) = op.unauthorized else {
            continue;
        };
        let s = status(port, &op.method, &op.path, None, body);
        if (s == 401) != asks {
            let want = if asks { "401" } else { "not 401" };
            wrong.push(format!(
                "{s} {} {} with no token, {want}",
                op.method, op.path
            ));
        }
    }
    // HEAD where the document lists none beside a GET: refused, as the
    // document says, while the fixtures it would read are there.
    for op in ops.iter().filter(|o| o.method == "GET") {
        let listed = ops.iter().any(|o| o.method == "HEAD" && o.path == op.path);
        if !listed {
            let s = status(port, "HEAD", &op.path, op.token, "");
            if s != 404 && s != 405 {
                wrong.push(format!("{s} HEAD {}: the document lists no HEAD", op.path));
            }
        }
    }
    // Every operation, HEAD among them where listed. Every answer, in the
    // order sent: a 404 after a refused make reads as the make's failure.
    let mut answered = Vec::new();
    let mut unrouted = false;
    for op in &ops {
        let body = if op.body { body(&op.id) } else { "" };
        let (s, marked) = answer(port, &op.method, &op.path, op.token, body, &signed(&op.id));
        unrouted |= s == 404 || s == 405;
        answered.push(format!("{s} {} {} ({})", op.method, op.path, op.id));
        // A token and a hook's signature are what is asked before a
        // handler runs: each given, every answer is its handler's, which a
        // deprecated route's marks whatever its status.
        if s == 401 {
            wrong.push(format!(
                "{s} {} {}: refused before its handler, its token or signature given",
                op.method, op.path
            ));
        }
        if marked != op.deprecated {
            let what = match marked {
                true => "answered Deprecation, which the document does not say",
                false => "deprecated in the document, answered without Deprecation",
            };
            wrong.push(format!("{s} {} {}: {what}", op.method, op.path));
        }
    }
    assert!(
        !unrouted && wrong.is_empty(),
        "answers the document does not say:\n{}\n\nevery operation, an operation the \
         document lists answered 404 or 405 among them or not:\n{}",
        wrong.join("\n"),
        answered.join("\n")
    );
}

fn file_at(tag: &str, name: &str, text: &str) -> String {
    let path = tmp(tag, name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_string()
}

const FIXTURES: &[&str] = &[
    "create collection c (t text, v vector<2>)",
    "put c {t: \"a\", v: [1, 0]}",
    "create statement s() as get c select t",
    "create statement taken($id text from event.id) on hook stripe as insert c {t: $id}",
];

#[test]
fn every_operation_of_a_files_server_is_answered() {
    let tag = "openapi-file";
    let secret = file_at(tag, "stripe.whsec", STRIPE_SECRET);
    let hooks = file_at(tag, "hooks", &format!("stripe stripe {secret}\n"));
    let data = tmp(tag, "data.fennec");
    let server = start(&[
        "--file",
        data.to_str().unwrap(),
        "--http-token",
        ROOT,
        "--replication-token",
        REPLICATION,
        "--cdc",
        "--hooks",
        &hooks,
        "--mcp",
        "--studio",
        "--http-cors",
        APP,
        "--auth-delay",
        "0",
    ]);
    let doc = served(&server, "file");
    a_page_reads_the_document(&server);
    let mut http = server.http().with_token(ROOT);
    for q in FIXTURES {
        http.query(q).unwrap_or_else(|e| panic!("{q}: {e:?}"));
    }
    every_operation_is_answered(server.port, &doc, 35);
}

#[test]
fn every_operation_of_a_tenant_node_is_answered() {
    let tag = "openapi-node";
    let dir = tmp(tag, "tenants");
    std::fs::create_dir_all(&dir).unwrap();
    let server = start(&[
        "--dir",
        dir.to_str().unwrap(),
        "--admin-token",
        ADMIN,
        "--http-token",
        ROOT,
        "--replication-token",
        REPLICATION,
        "--mcp",
        "--studio",
        "--http-cors",
        APP,
        "--auth-delay",
        "0",
    ]);
    let doc = served(&server, "node");
    a_page_reads_the_document(&server);
    let mut admin = Http::open(server.port).with_token(ADMIN);
    let made = admin.ask("PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    // The tenant's hook, whose event is sent before the admin's routes are.
    let hooked = admin.ask(
        "PUT",
        "/_admin/tenants/acme/hooks/stripe",
        body("adminSetHook"),
    );
    assert_eq!(hooked.status, 200, "{}", hooked.body);
    let mut http = server.http().with_token(ROOT);
    for q in FIXTURES {
        http.query_at("/t/acme", q)
            .unwrap_or_else(|e| panic!("{q}: {e:?}"));
    }
    every_operation_is_answered(server.port, &doc, 60);
}
