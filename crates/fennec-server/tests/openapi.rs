//! The OpenAPI document (fennec-http's `openapi.rs`) held to the routes
//! from the other side: every operation a file's server and a tenant node
//! list in the document they serve is sent to them, under every flag a
//! route is served with, and answered neither 404 nor 405. A row of the
//! document with no route behind it fails here; a route with no row,
//! fennec-http's `every_route_the_server_matches_is_documented`.
//!
//! The documents it read are left in `CARGO_TARGET_TMPDIR/openapi/`, the
//! router's beside them, for `make openapi-lint` to hand an OpenAPI
//! linter.

use crate::support::{start, tmp, Http, Server};
use fennec_core::value::Value;
use fennec_http::openapi::{document, Kind};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

const ROOT: &str = "root";
const ADMIN: &str = "adm";
const REPLICATION: &str = "rep";

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
    body: bool,
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
            let scheme = match get(op, "security") {
                Some(Value::List(reqs)) => reqs
                    .first()
                    .and_then(|r| members(Some(r)).first().map(|(k, _)| k.clone())),
                _ => Some("token".to_string()),
            };
            let token = match scheme.as_deref() {
                None => None,
                Some("token" | "jwt") => Some(ROOT),
                Some("admin") => Some(ADMIN),
                Some("replication") => Some(REPLICATION),
                Some(other) => panic!("{method} {path}: a scheme the test knows not: {other}"),
            };
            out.push(Operation {
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                id,
                token,
                body: get(op, "requestBody").is_some(),
            });
        }
    }
    // A JSON object's members are read in key order, not the document's:
    // what makes a thing goes by its method. A `PUT` before a `POST`, so a
    // hook is set before its tenant is frozen, and a freeze thawed after.
    let rank = |m: &str| {
        ["PUT", "POST", "GET", "PATCH", "DELETE"]
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
        "updateRows" | "updateRowsPut" | "updateAll" | "updateAllPut" => r#"{"t":"y"}"#,
        "nearRows" => r#"{"vector":[1,0],"limit":1}"#,
        "query" | "adminQuery" => r#"{"query":"get c"}"#,
        "batch" => "{\"query\":\"get c\"}\n",
        "moveConsumer" => r#"{"since":0}"#,
        "mcp" => r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        "adminSetHook" => r#"{"scheme":"stripe","secrets":["whsec_a_secret_stripe_shows"]}"#,
        _ => "{}",
    }
}

/// The path with its placeholders named as the fixtures are.
fn filled(path: &str) -> String {
    path.replace("{collection}", "c")
        .replace("{tenant}", "acme")
        .replace("{statement}", "s")
        .replace("{hook}", "stripe")
        .replace("{consumer}", "k")
        .replace("{file}", "app.js")
}

/// The status `op` is answered with, on a connection of its own: its
/// status line alone, so a stream's head is enough.
fn status(port: u16, op: &Operation) -> u16 {
    let target = filled(&op.path);
    let body = if op.body { body(&op.id) } else { "" };
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let auth = op
        .token
        .map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        s,
        "{} {target} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        op.method,
        body.len()
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("{} {target}: no status line: {line:?}", op.method))
}

/// Sends every operation of `doc` to `port`: none is a 404 or a 405.
fn every_operation_is_answered(port: u16, doc: &Value, least: usize) {
    let ops = operations(doc);
    assert!(ops.len() >= least, "{} operations", ops.len());
    // Every answer, in the order sent: a 404 after a refused make reads
    // as the make's failure.
    let mut answered = Vec::new();
    let mut unrouted = false;
    for op in &ops {
        let s = status(port, op);
        unrouted |= s == 404 || s == 405;
        answered.push(format!("{s} {} {} ({})", op.method, op.path, op.id));
    }
    assert!(
        !unrouted,
        "an operation the document lists answered 404 or 405:\n{}",
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
];

#[test]
fn every_operation_of_a_files_server_is_answered() {
    let tag = "openapi-file";
    let secret = file_at(tag, "stripe.whsec", "whsec_a_secret_stripe_shows");
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
        "--auth-delay",
        "0",
    ]);
    let doc = served(&server, "file");
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
        "--auth-delay",
        "0",
    ]);
    let doc = served(&server, "node");
    let made = Http::open(server.port)
        .with_token(ADMIN)
        .ask("PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    let mut http = server.http().with_token(ROOT);
    for q in FIXTURES {
        http.query_at("/t/acme", q)
            .unwrap_or_else(|e| panic!("{q}: {e:?}"));
    }
    every_operation_is_answered(server.port, &doc, 60);
}
