//! MCP over Streamable HTTP and stdio: the 2026-07-28 revision and the
//! handshake revisions beside it, every tool, a scoped token's agent seeing
//! what its `/query` sees, the bounds that refuse rather than cut, the
//! write tools off unless asked for, and a page from another origin -- or a
//! name rebound to this machine -- refused before its token is read.

use fennec_core::prelude::*;
use fennec_http::access::{Access, Who};
use fennec_http::mcp::{self, Options};
use fennec_http::tenants::Tenants;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const MODERN: &str = "2026-07-28";

/// Statements the tests of `call_<name>` tools define, as the server's
/// token defines them over `/query`: one that reads, one that reads the
/// caller's claims, one that writes and answers what it wrote, one that
/// writes and answers every row, and one that writes a text back.
const STATEMENTS: &[&str] = &[
    "create statement years($from int, $n int = 2) comment 'Titles from a year on, oldest first' \
     as get docs select title, year where year >= $from order year limit $n",
    "create statement mine() claims (sub text) as get docs select title where owner = $jwt.sub",
    "create statement stamp($x int) public as insert secrets {x: $x} returning id, x",
    "create statement grow($x int) public scans as begin insert secrets {x: $x}; \
     get secrets select x end",
    "create collection pads (t text)",
    "create statement pad($t text) public as insert pads {t: $t} returning t",
];

/// `sql` run over `/query` as the server's token, each answered 200.
fn define(n: &Node, sql: &[&str]) {
    let auth = format!("Bearer {ROOT}");
    for q in sql {
        let body = format!(r#"{{"query":"{q}"}}"#);
        let a = n.post_raw("POST", "/query", &[("Authorization", &auth)], &body);
        assert_eq!(a.status, 200, "{q}: {}", a.body);
    }
}

/// A 2026-07-28 `tools/list` as `token`: each tool by name.
fn listed(n: &Node, token: &str) -> Vec<(String, Value)> {
    let v = n.modern(Some(token), 2, "tools/list", "{}").json();
    match at(&v, &["result", "tools"]) {
        Value::List(t) => t
            .iter()
            .map(|t| (text(at(t, &["name"])).to_string(), t.clone()))
            .collect(),
        other => panic!("{other:?}"),
    }
}

fn named<'a>(tools: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    tools.iter().find(|(n, _)| n == name).map(|(_, t)| t)
}

/// Each statement the token may call is a tool of its own, `call_<name>`:
/// its arguments its typed `inputSchema`, its answer its `outputSchema`,
/// one that writes only under `--mcp-write`, one reading claims none of the
/// server's token's -- `as` is not a model's -- and every name one Claude's
/// and OpenAI's tool names take. A statement's tool is called as `/query`
/// calls it, its arguments values; `query` and `write` send a call to it.
#[test]
fn a_statement_is_a_tool_of_its_own_typed_by_its_signature() {
    let n = start(opts());
    define(&n, STATEMENTS);
    let tools = listed(&n, ROOT);
    let names: Vec<&str> = tools.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["schema", "search", "query", "get", "call_years"]);
    let years = named(&tools, "call_years").unwrap();
    assert_eq!(
        at(years, &["description"]),
        &Value::Text(
            "Titles from a year on, oldest first. An answer of more than 100 rows or 65536 \
             bytes is refused, not cut."
                .into()
        )
    );
    let json = |t: &str| fennec_core::json::parse_json(t).unwrap();
    assert_eq!(
        at(years, &["inputSchema"]),
        &json(
            r#"{"type":"object","properties":{"from":{"type":"integer","description":"int"},"n":{"type":"integer","default":2,"description":"int, 2 when absent"}},"required":["from"],"additionalProperties":false}"#
        )
    );
    assert_eq!(
        at(years, &["outputSchema"]),
        &json(
            r#"{"type":"object","properties":{"rows":{"type":"array","items":{"type":"object","properties":{"title":{"type":["string","null"]},"year":{"type":["integer","null"]}}}}},"required":["rows"]}"#
        )
    );
    assert_eq!(
        at(years, &["annotations", "readOnlyHint"]),
        &Value::Bool(true)
    );

    let (err, s) = n.call(Some(ROOT), "call_years", r#"{"from":2024}"#);
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["rows", "0", "title"]), &Value::Text("dog".into()));
    assert_eq!(at(&s, &["rows", "1", "year"]), &Value::Int(2025));
    assert_eq!(at(&s, &["rows", "2"]), &Value::Null);
    let (_, s) = n.call(Some(ROOT), "call_years", r#"{"from":2023,"n":1}"#);
    assert_eq!(at(&s, &["rows", "0", "title"]), &Value::Text("fox".into()));
    // Refused as `/query` refuses the call, for the model to correct.
    for (args, status) in [
        (r#"{"from":"2024"}"#, 400),
        (r#"{}"#, 400),
        (r#"{"from":2024,"to":2025}"#, 400),
    ] {
        let (err, s) = n.call(Some(ROOT), "call_years", args);
        assert!(err, "{args}: {s:?}");
        assert_eq!(at(&s, &["status"]), &Value::Int(status), "{args}: {s:?}");
    }
    // One the list leaves out is the tool no statement is: a protocol error.
    for tool in ["call_nope", "call_mine", "call_stamp", "call_"] {
        let a = n.modern(
            Some(ROOT),
            3,
            "tools/call",
            &format!(r#"{{"name":"{tool}","arguments":{{}}}}"#),
        );
        assert_eq!(error_code(&a), -32602, "{tool}: {}", a.body);
    }
    // With --mcp-write, the statements that write; an insert's alone adds.
    let w = start(Options {
        write: true,
        ..opts()
    });
    define(&w, STATEMENTS);
    for (tool, args) in [
        ("query", r#"{"query":"call years {from: 2024}"}"#),
        ("write", r#"{"statement":"call stamp {x: 1}"}"#),
    ] {
        let (err, s) = w.call(Some(ROOT), tool, args);
        assert!(err, "{tool}: {s:?}");
        assert!(text(at(&s, &["error"])).contains("its own tool"), "{s:?}");
    }
    let tools = listed(&w, ROOT);
    for (name, _) in &tools {
        let ok = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        assert!(ok, "{name}");
    }
    let stamp = named(&tools, "call_stamp").expect("call_stamp");
    assert!(named(&tools, "call_grow").is_some() && named(&tools, "call_mine").is_none());
    assert_eq!(
        at(stamp, &["annotations"]),
        &fennec_core::json::parse_json(
            r#"{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":false,"openWorldHint":false}"#
        )
        .unwrap()
    );
    let (err, s) = w.call(Some(ROOT), "call_stamp", r#"{"x":7}"#);
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["rows", "0", "x"]), &Value::Int(7));
    // A handshake client lists and calls them alike.
    let l = w.legacy(Some("2025-11-25"), 4, "tools/list", "{}");
    assert!(l.body.contains(r#""name":"call_stamp""#), "{}", l.body);
    let c = w.legacy(
        Some("2025-11-25"),
        5,
        "tools/call",
        r#"{"name":"call_years","arguments":{"from":2025}}"#,
    );
    assert!(c.body.contains(r#""title":"cat""#), "{}", c.body);
}

/// A statement's tool is held to `--mcp-max-rows` and `--mcp-max-bytes`
/// where `/query`'s path answers it: one that writes before its block
/// lands -- past them, refused (413) and put back, so a model is never told
/// half an answer nor leaves a write it was not told of -- and one that
/// reads as it reads.
#[test]
fn a_call_tool_past_its_bound_is_refused_and_its_writes_put_back() {
    let n = start(Options {
        write: true,
        max_rows: 2,
        max_bytes: 120,
        ..opts()
    });
    define(&n, STATEMENTS);
    let count = |c: &str| {
        let auth = format!("Bearer {ROOT}");
        let body = format!(r#"{{"query":"get {c} count"}}"#);
        let a = n.post_raw("POST", "/query", &[("Authorization", &auth)], &body);
        int(at(&a.json(), &["0", "count"]))
    };
    // 42 and 1: two rows, within the bound.
    let (err, s) = n.call(Some(ROOT), "call_grow", r#"{"x":1}"#);
    assert!(!err, "{s:?}");
    assert_eq!(count("secrets"), 2);
    // A third row is past it: refused, and the insert put back.
    let (err, s) = n.call(Some(ROOT), "call_grow", r#"{"x":2}"#);
    assert!(err, "{s:?}");
    assert_eq!(at(&s, &["status"]), &Value::Int(413));
    assert!(text(at(&s, &["error"])).contains("--mcp-max-rows"), "{s:?}");
    assert_eq!(count("secrets"), 2);
    // An answer past the bytes, the same.
    let (err, s) = n.call(Some(ROOT), "call_pad", r#"{"t":"short"}"#);
    assert!(!err, "{s:?}");
    let long = "x".repeat(200);
    let (err, s) = n.call(Some(ROOT), "call_pad", &format!(r#"{{"t":"{long}"}}"#));
    assert!(err && int(at(&s, &["status"])) == 413, "{s:?}");
    assert!(
        text(at(&s, &["error"])).contains("--mcp-max-bytes"),
        "{s:?}"
    );
    assert_eq!(count("pads"), 1);
    // A read past the rows is refused as it reads; a client's call through
    // `/query` is answered whole, held to no tool's bound.
    define(&n, &["put docs {title: 'owl', year: 2026, owner: 'carol'}"]);
    let (err, s) = n.call(Some(ROOT), "call_years", r#"{"from":2023,"n":3}"#);
    assert!(err && int(at(&s, &["status"])) == 413, "{s:?}");
    let auth = format!("Bearer {ROOT}");
    let a = n.post_raw(
        "POST",
        "/query",
        &[("Authorization", &auth)],
        r#"{"query":"call years {from: 2023, n: 3}"}"#,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    assert_eq!(at(&a.json(), &["2", "title"]), &Value::Text("cat".into()));
    let a = n.post_raw(
        "POST",
        "/query",
        &[("Authorization", &auth)],
        r#"{"query":"call grow {x: 3}"}"#,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    assert_eq!(count("secrets"), 3);
}

/// A scoped token's agent is handed the statements a `call` line grants
/// it, calls them as itself -- its claims the statement's -- and is told of
/// no other: an ungranted one is the unknown tool a missing one is.
#[test]
fn a_scoped_agent_calls_what_its_policy_grants() {
    let policy = "\
docs         read  where owner = $jwt.sub
years, mine  call
";
    let access = Arc::new(Access::new(SECRET, policy).unwrap());
    let n = serve(
        Config {
            token: Some(ROOT.into()),
            access: Some(access),
            mcp: Some(Arc::new(Options {
                write: true,
                ..opts()
            })),
            ..Config::default()
        },
        DOCS,
    );
    define(&n, STATEMENTS);
    let alice = n.token(r#"{"sub":"alice"}"#);
    let tools = listed(&n, &alice);
    let calls: Vec<&str> = tools
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| n.starts_with("call_"))
        .collect();
    assert_eq!(calls, ["call_mine", "call_years"]);
    let (err, s) = n.call(Some(&alice), "call_mine", "{}");
    assert!(!err, "{s:?}");
    assert_eq!(
        at(&s, &["rows"]),
        &fennec_core::json::parse_json(r#"[{"title":"fox"},{"title":"cat"}]"#).unwrap()
    );
    // The statement's authority: rows of every owner, which its `get docs`
    // would not show it.
    let (err, s) = n.call(Some(&alice), "call_years", r#"{"from":2024}"#);
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["rows", "0", "title"]), &Value::Text("dog".into()));
    let a = n.modern(
        Some(&alice),
        3,
        "tools/call",
        r#"{"name":"call_stamp","arguments":{"x":1}}"#,
    );
    assert_eq!(error_code(&a), -32602, "{}", a.body);
    // A token granted `mine` that lacks the claim it declares -- held as
    // `null`, `""`, of another type or not at all -- may never call it, so
    // it is told nothing of its arguments: no tool, the unknown one.
    for claims in [
        r#"{"sub":null}"#,
        r#"{"sub":""}"#,
        r#"{"sub":{"id":1}}"#,
        "{}",
    ] {
        let t = n.token(claims);
        let tools = listed(&n, &t);
        assert!(named(&tools, "call_years").is_some(), "{claims}");
        assert!(named(&tools, "call_mine").is_none(), "{claims}");
        let a = n.modern(
            Some(&t),
            4,
            "tools/call",
            r#"{"name":"call_mine","arguments":{}}"#,
        );
        assert_eq!(error_code(&a), -32602, "{claims}: {}", a.body);
    }
}

/// Whether a statement's tool writes is asked again under the lock its call
/// runs under, never apart from it: a statement listed as a read, made
/// again as one that writes between the tool list's question and the
/// call's lock, is refused where the session takes no writes
/// (`--mcp-write`) -- never run, at whichever lock the call met it.
#[test]
fn a_tool_listed_as_a_read_never_writes_once_its_statement_does() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    let n = Arc::new(start(opts()));
    let reads = "create statement flip() public as get marks select id, v where v = 0";
    let writes = "create statement flip() public as insert marks {v: 1} returning id, v";
    define(&n, &["create collection marks (v int)", reads]);
    assert!(named(&listed(&n, ROOT), "call_flip").is_some());
    let line = |q: &str| {
        let mut body = String::from("{\"query\":");
        fennec_core::json::escape_into(&mut body, q);
        body + "}"
    };
    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let (stop, n) = (Arc::clone(&stop), Arc::clone(&n));
        let (reads, writes) = (line(reads), line(writes));
        let drop = line("drop statement flip");
        std::thread::spawn(move || {
            let auth = format!("Bearer {ROOT}");
            let mut i = 0;
            while !stop.load(Relaxed) {
                i += 1;
                let next = if i % 2 == 1 { &writes } else { &reads };
                let batch = format!("{drop}\n{next}");
                let a = n.post_raw("POST", "/batch", &[("Authorization", &auth)], &batch);
                assert_eq!(a.status, 200, "{}", a.body);
            }
        })
    };
    // Called until the swap has landed between the question and the lock
    // twice -- a refusal here, a write had it been asked apart from the
    // lock -- which a call in a hundred or so meets; the count of calls
    // bounds only that wait.
    let refused = Arc::new(AtomicUsize::new(0));
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let (n, refused) = (Arc::clone(&n), Arc::clone(&refused));
            std::thread::spawn(move || {
                for i in 0..2000 {
                    if refused.load(Relaxed) >= 2 {
                        break;
                    }
                    let a = n.modern(
                        Some(ROOT),
                        i,
                        "tools/call",
                        r#"{"name":"call_flip","arguments":{}}"#,
                    );
                    assert_eq!(a.status, 200, "{}", a.body);
                    let v = a.json();
                    // One that writes when the call asked: no tool.
                    if at(&v, &["error"]) != &Value::Null {
                        assert_eq!(error_code(&a), -32602, "{}", a.body);
                        continue;
                    }
                    let r = at(&v, &["result"]);
                    if at(r, &["isError"]) == &Value::Bool(true) {
                        let why = text(at(r, &["content", "0", "text"]));
                        assert!(why.contains("--mcp-write"), "{why}");
                        refused.fetch_add(1, Relaxed);
                        continue;
                    }
                    assert_eq!(
                        at(r, &["structuredContent", "rows"]),
                        &Value::List(Vec::new()),
                        "a tool with no --mcp-write wrote: {}",
                        a.body
                    );
                }
            })
        })
        .collect();
    let joined: Vec<_> = callers.into_iter().map(|c| c.join()).collect();
    stop.store(true, Relaxed);
    flipper.join().unwrap();
    for j in joined {
        j.unwrap();
    }
    let auth = format!("Bearer {ROOT}");
    let count = line("get marks count");
    let a = n.post_raw("POST", "/query", &[("Authorization", &auth)], &count);
    assert!(a.body.contains(r#""count":0"#), "{}", a.body);
    assert!(refused.load(Relaxed) >= 2, "no swap met a call's lock");
}

fn setup(db: &mut Database, sql: &[&str]) {
    for s in sql {
        db.execute(&fennec_ql::parse_one(s).unwrap()).unwrap();
    }
}

const DOCS: &[&str] = &[
    "create collection docs (title text, body text @text, embed vector<3> @hnsw(cosine), year int, owner text @hash)",
    "put docs [{title: \"fox\", body: \"the quick brown fox jumps\", embed: [1, 0, 0], year: 2023, owner: \"alice\"}, \
     {title: \"dog\", body: \"a lazy dog sleeps all day\", embed: [0, 1, 0], year: 2024, owner: \"bob\"}, \
     {title: \"cat\", body: \"the cat watches the fox\", embed: [0.9, 0.1, 0], year: 2025, owner: \"alice\"}]",
    "create collection secrets (x int)",
    "put secrets {x: 42}",
];

/// Calls a test makes are not held to a rate unless the test asks: the
/// buckets are the process's, and every unscoped test shares one.
fn opts() -> Options {
    Options {
        calls_per_sec: 0,
        ..Options::default()
    }
}

struct Node {
    port: u16,
    access: Option<Arc<Access>>,
}

fn serve(cfg: Config, sql: &[&str]) -> Node {
    fennec_http::audit::set_delay(0);
    let mut db = Database::new();
    setup(&mut db, sql);
    let access = cfg.access.clone();
    let server = Server::new(
        Arc::new(RwLock::new(db)),
        Config {
            addr: "127.0.0.1:0".into(),
            ..cfg
        },
    );
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { port, access }
}

/// A server with a token, MCP on with `o`.
fn start(o: Options) -> Node {
    serve(
        Config {
            token: Some(ROOT.into()),
            mcp: Some(Arc::new(o)),
            ..Config::default()
        },
        DOCS,
    )
}

const POLICY: &str = "\
docs     read,write  where owner = $jwt.sub
memories read,write  where owner = $jwt.sub
";

fn start_scoped(o: Options) -> Node {
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    serve(
        Config {
            token: Some(ROOT.into()),
            access: Some(access),
            mcp: Some(Arc::new(o)),
            ..Config::default()
        },
        DOCS,
    )
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        fennec_core::json::parse_json(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// `v`'s member at `path`, `Null` where there is none.
fn at<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    static NULL: Value = Value::Null;
    let mut v = v;
    for p in path {
        v = match v {
            Value::Object(m) => m.iter().find(|(k, _)| k == p).map_or(&NULL, |(_, x)| x),
            Value::List(items) => p
                .parse::<usize>()
                .ok()
                .and_then(|i| items.get(i))
                .unwrap_or(&NULL),
            _ => &NULL,
        };
    }
    v
}

fn text(v: &Value) -> &str {
    match v {
        Value::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        other => panic!("not an int: {other:?}"),
    }
}

impl Node {
    fn token(&self, claims: &str) -> String {
        self.access
            .as_ref()
            .expect("a server with tokens")
            .mint(claims)
            .unwrap()
    }

    /// A raw request to the endpoint.
    fn post_raw(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Answer {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            head.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        s.write_all(head.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        Answer {
            status: head[9..12].parse().unwrap(),
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    /// A 2026-07-28 request, with the headers its binding mirrors.
    fn modern(&self, token: Option<&str>, id: i64, method: &str, params: &str) -> Answer {
        self.modern_at("/_mcp", token, id, method, params)
    }

    fn modern_at(
        &self,
        path: &str,
        token: Option<&str>,
        id: i64,
        method: &str,
        params: &str,
    ) -> Answer {
        let meta = format!(
            r#""_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}","io.modelcontextprotocol/clientCapabilities":{{}},"io.modelcontextprotocol/clientInfo":{{"name":"test","version":"1"}}}}"#
        );
        let params = match params.trim() {
            "" | "{}" => format!("{{{meta}}}"),
            p => format!("{},{meta}}}", &p[..p.len() - 1]),
        };
        let body =
            format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#);
        let name =
            fennec_core::json::parse_json(&params)
                .ok()
                .and_then(|p| match at(&p, &["name"]) {
                    Value::Text(t) => Some(t.clone()),
                    _ => None,
                });
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut headers = vec![("MCP-Protocol-Version", MODERN), ("Mcp-Method", method)];
        if let (Some(n), "tools/call") = (&name, method) {
            headers.push(("Mcp-Name", n.as_str()));
        }
        if let Some(a) = &auth {
            headers.push(("Authorization", a.as_str()));
        }
        self.post_raw("POST", path, &headers, &body)
    }

    /// A tool's call, its result's members back: `isError` and what it
    /// answered.
    fn call(&self, token: Option<&str>, tool: &str, args: &str) -> (bool, Value) {
        let a = self.modern(
            token,
            7,
            "tools/call",
            &format!(r#"{{"name":"{tool}","arguments":{args}}}"#),
        );
        assert_eq!(a.status, 200, "{}", a.body);
        let v = a.json();
        let r = at(&v, &["result"]).clone();
        assert_eq!(
            at(&r, &["resultType"]),
            &Value::Text("complete".into()),
            "{}",
            a.body
        );
        let error = at(&r, &["isError"]) == &Value::Bool(true);
        // The text a model reads is the structured content, written out.
        let shown = text(at(&r, &["content", "0", "text"]));
        let structured = at(&r, &["structuredContent"]).clone();
        if !error {
            assert_eq!(fennec_core::json::parse_json(shown).unwrap(), structured);
        }
        (error, structured)
    }

    /// A handshake revision's request, `version` in its header.
    fn legacy(&self, version: Option<&str>, id: i64, method: &str, params: &str) -> Answer {
        let body =
            format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#);
        let auth = format!("Bearer {ROOT}");
        let mut headers = vec![("Authorization", auth.as_str())];
        if let Some(v) = version {
            headers.push(("MCP-Protocol-Version", v));
        }
        self.post_raw("POST", "/_mcp", &headers, &body)
    }
}

fn error_code(a: &Answer) -> i64 {
    int(at(&a.json(), &["error", "code"]))
}

#[test]
fn a_2026_client_discovers_lists_and_calls_every_reading_tool() {
    let n = start(opts());
    let d = n.modern(Some(ROOT), 1, "server/discover", "{}");
    assert_eq!(d.status, 200, "{}", d.body);
    let v = d.json();
    assert_eq!(at(&v, &["id"]), &Value::Int(1));
    assert_eq!(text(at(&v, &["result", "supportedVersions", "0"])), MODERN);
    assert_eq!(text(at(&v, &["result", "resultType"])), "complete");
    assert_eq!(
        text(at(
            &v,
            &[
                "result",
                "_meta",
                "io.modelcontextprotocol/serverInfo",
                "name"
            ]
        )),
        "fennecdb"
    );
    assert!(text(at(&v, &["result", "instructions"])).contains("schema"));
    // No session is made, and none is echoed.
    assert!(
        !d.head.to_ascii_lowercase().contains("mcp-session-id"),
        "{}",
        d.head
    );

    let l = n.modern(Some(ROOT), 2, "tools/list", "{}");
    let v = l.json();
    let names: Vec<&str> = match at(&v, &["result", "tools"]) {
        Value::List(t) => t.iter().map(|t| text(at(t, &["name"]))).collect(),
        other => panic!("{other:?}"),
    };
    // The writing tools are off unless the server is told.
    assert_eq!(names, ["schema", "search", "query", "get"]);
    assert_eq!(at(&v, &["result", "ttlMs"]), &Value::Int(300_000));
    assert_eq!(
        at(&v, &["result", "tools", "1", "inputSchema", "type"]),
        &Value::Text("object".into())
    );

    let (err, s) = n.call(Some(ROOT), "schema", "{}");
    assert!(!err);
    let fennecql = text(at(&s, &["fennecql"]));
    assert!(
        fennecql.contains("create collection docs (title text, body text @text"),
        "{fennecql}"
    );
    assert_eq!(at(&s, &["collections", "0", "rows"]), &Value::Int(3));

    // By words: the snippet marks them; by an embedding; both, fused.
    let (err, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"fox"}"#,
    );
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["count"]), &Value::Int(2));
    assert!(text(at(&s, &["rows", "0", "snippet"])).contains("**fox**"));
    assert!(matches!(at(&s, &["rows", "0", "_score"]), Value::Float(_)));
    // Vectors stay out of the rows.
    assert_eq!(at(&s, &["rows", "0", "embed"]), &Value::Null);
    let (_, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","vector":[0,1,0],"limit":1}"#,
    );
    assert_eq!(text(at(&s, &["rows", "0", "title"])), "dog");
    // A word misspelt, and one begun, found when asked for, the snippet
    // marking the word the index holds.
    let (_, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"wtches"}"#,
    );
    assert_eq!(at(&s, &["count"]), &Value::Int(0), "{s:?}");
    let (err, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"wtches","fuzzy":true}"#,
    );
    assert!(!err, "{s:?}");
    assert_eq!(text(at(&s, &["rows", "0", "title"])), "cat");
    assert!(text(at(&s, &["rows", "0", "snippet"])).contains("**watches**"));
    let (_, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"lazy slee","prefix":true}"#,
    );
    assert_eq!(
        text(at(&s, &["rows", "0", "snippet"])),
        "a **lazy** dog **sleeps** all day"
    );
    let (err, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"fox","fuzzy":"yes"}"#,
    );
    assert!(
        err && format!("{s:?}").contains("is true or false"),
        "{s:?}"
    );
    let (_, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"fox","vector":[1,0,0],"filter":"year >= $1","params":[2024],"fields":["title"]}"#,
    );
    // The words find the cat alone past 2024, the embedding the dog too.
    assert_eq!(at(&s, &["count"]), &Value::Int(2), "{s:?}");
    assert_eq!(text(at(&s, &["rows", "0", "title"])), "cat");
    assert_eq!(text(at(&s, &["rows", "1", "title"])), "dog");
    assert_eq!(at(&s, &["rows", "0", "year"]), &Value::Null);

    let (_, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get docs where year < $1 order year","params":[2025]}"#,
    );
    assert_eq!(at(&s, &["count"]), &Value::Int(2));
    assert_eq!(at(&s, &["rows", "0", "embed"]), &Value::Null);
    let (_, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get docs order year limit 1","include_vectors":true}"#,
    );
    assert!(matches!(at(&s, &["rows", "0", "embed"]), Value::List(_)));
    let (_, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get docs select owner, count(*) group owner"}"#,
    );
    assert_eq!(at(&s, &["count"]), &Value::Int(2));
    let (_, s) = n.call(Some(ROOT), "query", r#"{"query":"get docs count"}"#);
    assert_eq!(at(&s, &["rows", "0", "count"]), &Value::Int(3), "{s:?}");

    let (_, s) = n.call(Some(ROOT), "get", r#"{"collection":"docs","ids":[2,99]}"#);
    assert_eq!(text(at(&s, &["rows", "0", "title"])), "dog");
    assert_eq!(at(&s, &["missing", "0"]), &Value::Int(99));
}

#[test]
fn what_a_tool_refuses_comes_back_for_the_model_to_correct() {
    let n = start(opts());
    let refused = |tool: &str, args: &str, has: &str| {
        let (err, s) = n.call(Some(ROOT), tool, args);
        assert!(err, "{tool} {args}: {s:?}");
        let why = text(at(&s, &["error"]));
        assert!(why.contains(has), "{tool} {args}: {why}");
        int(at(&s, &["status"]))
    };
    refused(
        "search",
        r#"{"collection":"docs"}"#,
        "takes `text`, `vector` or both",
    );
    refused(
        "search",
        r#"{"collection":"docs","text":"fox","limt":3}"#,
        "no argument `limt`",
    );
    refused(
        "search",
        r#"{"collection":"docs","vector":[1,0]}"#,
        "holds vector<3>",
    );
    refused(
        "search",
        r#"{"collection":"nope","text":"x"}"#,
        "no collection `nope`",
    );
    refused(
        "search",
        r#"{"collection":"docs; del docs","text":"x"}"#,
        "cannot be written",
    );
    refused(
        "search",
        r#"{"collection":"secrets","text":"x"}"#,
        "no @text field",
    );
    // A filter is a condition, and nothing it holds closes on the
    // statement around it.
    refused(
        "search",
        r#"{"collection":"docs","text":"fox","filter":"year > 1 limit 99"}"#,
        "condition alone",
    );
    refused(
        "search",
        r#"{"collection":"docs","text":"fox","filter":"year > 1) or (1 = 1"}"#,
        "condition alone",
    );
    refused(
        "search",
        r#"{"collection":"docs","text":"fox","filter":"year = $2","params":[1]}"#,
        "names $2",
    );
    refused("query", r#"{"query":"del docs"}"#, "query runs reads");
    refused("query", r#"{"query":"get docs where"}"#, "");
    refused("get", r#"{"collection":"docs","ids":["a"]}"#, "row ids");
    assert_eq!(refused("query", r#"{"query":"get nope"}"#, "nope"), 404);

    // An unknown tool is the protocol's error, not the tool's.
    let a = n.modern(
        Some(ROOT),
        3,
        "tools/call",
        r#"{"name":"write","arguments":{}}"#,
    );
    assert_eq!(error_code(&a), -32602, "{}", a.body);
    assert!(a.body.contains("Unknown tool: write"));
}

#[test]
fn an_answer_past_its_bound_is_refused_never_cut() {
    let o = Options {
        max_rows: 10,
        max_bytes: 2_000,
        ..opts()
    };
    let mut sql: Vec<String> = DOCS.iter().map(|s| s.to_string()).collect();
    sql.push("create collection many (n int, pad text)".into());
    let rows: Vec<String> = (0..25)
        .map(|i| format!("{{n: {i}, pad: \"{}\"}}", "x".repeat(200)))
        .collect();
    sql.push(format!("put many [{}]", rows.join(", ")));
    // Five children of each of the first three: a join's fifteen rows.
    sql.push("create collection kids (parent int @hash, k int)".into());
    let kids: Vec<String> = (0..15)
        .map(|i| format!("{{parent: {}, k: {i}}}", 1 + i / 5))
        .collect();
    sql.push(format!("put kids [{}]", kids.join(", ")));
    let sql: Vec<&str> = sql.iter().map(String::as_str).collect();
    let n = serve(
        Config {
            token: Some(ROOT.into()),
            mcp: Some(Arc::new(o)),
            ..Config::default()
        },
        &sql,
    );
    let (err, s) = n.call(Some(ROOT), "query", r#"{"query":"get many select n"}"#);
    assert!(err, "{s:?}");
    assert_eq!(at(&s, &["status"]), &Value::Int(413));
    assert!(text(at(&s, &["error"])).contains("more than 10 rows"));
    // A limit past the bound is refused the same way; one inside it is
    // answered whole; a count is one row.
    let (err, _) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get many select n limit 50"}"#,
    );
    assert!(err);
    let (err, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get many select n limit $1","params":[10]}"#,
    );
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["count"]), &Value::Int(10));
    let (err, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get many where n > 20 select n"}"#,
    );
    assert!(!err);
    assert_eq!(at(&s, &["count"]), &Value::Int(4));
    let (_, s) = n.call(Some(ROOT), "query", r#"{"query":"get many count"}"#);
    assert_eq!(at(&s, &["rows", "0", "count"]), &Value::Int(25));
    // A join's rows are its flat ones: three parents' fifteen rows are
    // past the bound, which counted the parents and answered them all.
    let join = "SELECT m.n, k.k FROM many m JOIN kids k ON k.parent = m.id";
    let (err, s) = n.call(Some(ROOT), "query", &format!(r#"{{"query":"{join}"}}"#));
    assert!(err, "{s:?}");
    assert_eq!(at(&s, &["status"]), &Value::Int(413));
    let (err, s) = n.call(
        Some(ROOT),
        "query",
        &format!(r#"{{"query":"{join} ORDER BY m.n LIMIT 8"}}"#),
    );
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["count"]), &Value::Int(8));
    // Ten rows of 200 bytes and more: past 2 000 bytes.
    let (err, s) = n.call(Some(ROOT), "query", r#"{"query":"get many limit 10"}"#);
    assert!(err);
    assert!(
        text(at(&s, &["error"])).contains("past this server's bound of 2000"),
        "{s:?}"
    );
    let (err, s) = n.call(
        Some(ROOT),
        "search",
        r#"{"collection":"docs","text":"fox","limit":11}"#,
    );
    assert!(err && text(at(&s, &["error"])).contains("between 1 and 10"));
    let ids: Vec<String> = (1..=11).map(|i| i.to_string()).collect();
    let (err, s) = n.call(
        Some(ROOT),
        "get",
        &format!(r#"{{"collection":"many","ids":[{}]}}"#, ids.join(",")),
    );
    assert!(err && int(at(&s, &["status"])) == 413);
}

#[test]
fn a_scoped_token_s_agent_sees_what_its_query_sees() {
    let n = start_scoped(Options {
        write: true,
        memory: Some("memories".into()),
        ..opts()
    });
    let alice = n.token(r#"{"sub":"alice"}"#);
    let bob = n.token(r#"{"sub":"bob"}"#);
    // Its own collections alone, and no count of the rows it may not read.
    let (_, s) = n.call(Some(&alice), "schema", "{}");
    let fennecql = text(at(&s, &["fennecql"]));
    assert!(
        fennecql.contains("collection docs") && !fennecql.contains("secrets"),
        "{fennecql}"
    );
    assert_eq!(at(&s, &["collections", "0", "rows"]), &Value::Null);
    let (err, s) = n.call(Some(&alice), "schema", r#"{"collection":"secrets"}"#);
    assert!(err && text(at(&s, &["error"])).contains("no collection `secrets`"));

    let (_, s) = n.call(
        Some(&alice),
        "search",
        r#"{"collection":"docs","text":"fox dog cat"}"#,
    );
    assert_eq!(at(&s, &["count"]), &Value::Int(2), "{s:?}");
    let (_, s) = n.call(
        Some(&bob),
        "search",
        r#"{"collection":"docs","vector":[1,0,0],"limit":5}"#,
    );
    assert_eq!(at(&s, &["count"]), &Value::Int(1));
    assert_eq!(text(at(&s, &["rows", "0", "owner"])), "bob");
    let (_, s) = n.call(Some(&bob), "query", r#"{"query":"get docs"}"#);
    assert_eq!(at(&s, &["count"]), &Value::Int(1));
    let (_, s) = n.call(Some(&bob), "get", r#"{"collection":"docs","ids":[1,2,3]}"#);
    assert_eq!(at(&s, &["count"]), &Value::Int(1));
    assert_eq!(
        at(&s, &["missing"]),
        &Value::List(vec![Value::Int(1), Value::Int(3)])
    );
    let (err, _) = n.call(Some(&bob), "query", r#"{"query":"get secrets"}"#);
    assert!(err);

    // A write is held to the rules as the row it writes: bob's put is
    // his, and alice's row is not his to change.
    let (err, s) = n.call(
        Some(&bob),
        "write",
        r#"{"statement":"insert docs {title: $1}","params":["mine"]}"#,
    );
    assert!(!err, "{s:?}");
    let (_, s) = n.call(
        Some(&bob),
        "query",
        r#"{"query":"get docs where title = 'mine' select owner"}"#,
    );
    assert_eq!(text(at(&s, &["rows", "0", "owner"])), "bob");
    let (err, s) = n.call(
        Some(&bob),
        "write",
        r#"{"statement":"set docs {title: 'taken'} where id = 1 require 1"}"#,
    );
    assert!(err, "{s:?}");
    let (_, s) = n.call(Some(ROOT), "get", r#"{"collection":"docs","ids":[1]}"#);
    assert_eq!(text(at(&s, &["rows", "0", "title"])), "fox");
    // Schema changes are nobody's over MCP, the server's token's neither.
    let (err, s) = n.call(
        Some(ROOT),
        "write",
        r#"{"statement":"drop collection docs"}"#,
    );
    assert!(err && int(at(&s, &["status"])) == 403, "{s:?}");

    // Memories: the collection is the server token's to make, then each
    // user keeps and recalls their own.
    let (err, s) = n.call(Some(&alice), "remember", r#"{"text":"alice likes tabs"}"#);
    assert!(err, "{s:?}");
    assert!(text(at(&s, &["error"])).contains("create collection if not exists memories"));
    let (err, s) = n.call(
        Some(ROOT),
        "remember",
        r#"{"text":"the server keeps its notes"}"#,
    );
    assert!(!err, "{s:?}");
    let (err, _) = n.call(
        Some(&alice),
        "remember",
        r#"{"text":"alice likes tabs","tags":["style"]}"#,
    );
    assert!(!err);
    let (err, _) = n.call(
        Some(&bob),
        "remember",
        r#"{"text":"bob likes spaces","tags":["style"]}"#,
    );
    assert!(!err);
    let (_, s) = n.call(Some(&alice), "recall", r#"{"text":"likes tabs spaces"}"#);
    assert_eq!(at(&s, &["count"]), &Value::Int(1), "{s:?}");
    assert_eq!(text(at(&s, &["memories", "0", "text"])), "alice likes tabs");
    let (_, s) = n.call(Some(&bob), "recall", r#"{"tags":["style"]}"#);
    assert_eq!(text(at(&s, &["memories", "0", "text"])), "bob likes spaces");
    let alices = int(at(
        &n.call(Some(&alice), "recall", "{}").1,
        &["memories", "0", "id"],
    ));
    let (err, s) = n.call(Some(&bob), "forget", &format!(r#"{{"id":{alices}}}"#));
    assert!(
        err && text(at(&s, &["error"])).contains("no memory"),
        "{s:?}"
    );
    let (err, _) = n.call(Some(&alice), "forget", &format!(r#"{{"id":{alices}}}"#));
    assert!(!err);
    let (_, s) = n.call(Some(&alice), "recall", "{}");
    assert_eq!(at(&s, &["count"]), &Value::Int(0));
}

/// A token whose rules name the fields a read shows: the `schema` tool
/// lists those alone, `query` and `get` answer each row with the fields of
/// the rules admitting it, and `search` ranks by no field some row does
/// not show -- its agent sees what its `/query` sees.
#[test]
fn a_scoped_agent_sees_the_fields_its_rules_show() {
    let policy = "\
docs  read(title, year)  where year >= 2024       for reader
docs  read               where owner = $jwt.sub   for reader
docs  read(title)                                 for viewer
";
    let n = serve(
        Config {
            token: Some(ROOT.into()),
            access: Some(Arc::new(Access::new(SECRET, policy).unwrap())),
            mcp: Some(Arc::new(opts())),
            ..Config::default()
        },
        DOCS,
    );
    let viewer = n.token(r#"{"sub":"v","role":"viewer"}"#);
    let (_, s) = n.call(Some(&viewer), "schema", "{}");
    let fennecql = text(at(&s, &["fennecql"]));
    assert!(fennecql.contains("title"), "{fennecql}");
    for hidden in ["body", "embed", "year", "owner"] {
        assert!(!fennecql.contains(hidden), "{hidden}: {fennecql}");
    }
    let (_, s) = n.call(Some(&viewer), "get", r#"{"collection":"docs","ids":[1]}"#);
    assert_eq!(text(at(&s, &["rows", "0", "title"])), "fox");
    assert_eq!(at(&s, &["rows", "0", "body"]), &Value::Null, "{s:?}");
    let (err, s) = n.call(
        Some(&viewer),
        "query",
        r#"{"query":"get docs select body"}"#,
    );
    assert!(err && int(at(&s, &["status"])) == 403, "{s:?}");

    // Bob reads his dog whole, alice's cat by its title and year alone.
    let bob = n.token(r#"{"sub":"bob","role":"reader"}"#);
    let (_, s) = n.call(Some(&bob), "query", r#"{"query":"get docs order id"}"#);
    assert_eq!(at(&s, &["count"]), &Value::Int(2), "{s:?}");
    assert_eq!(
        text(at(&s, &["rows", "0", "body"])),
        "a lazy dog sleeps all day"
    );
    assert_eq!(text(at(&s, &["rows", "1", "title"])), "cat");
    assert_eq!(at(&s, &["rows", "1", "body"]), &Value::Null, "{s:?}");
    assert_eq!(at(&s, &["rows", "1", "owner"]), &Value::Null, "{s:?}");
    // The cat's words would rank it: `match` over a body it does not show.
    for args in [
        r#"{"collection":"docs","text":"fox"}"#,
        r#"{"collection":"docs","vector":[1,0,0]}"#,
    ] {
        let (err, s) = n.call(Some(&bob), "search", args);
        assert!(err && int(at(&s, &["status"])) == 403, "{args}: {s:?}");
    }
    // Over his own, which a rule shows whole, it searches.
    let (err, s) = n.call(
        Some(&bob),
        "search",
        r#"{"collection":"docs","text":"dog","filter":"owner = $1","params":["bob"]}"#,
    );
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["count"]), &Value::Int(1), "{s:?}");
}

/// An agent whose token holds `"sub": null` is held to what the same
/// token's `/query` is: `owner = $jwt.sub` does not apply to it, and the
/// draft no one owns is not its to read, search or change.
#[test]
fn a_null_subject_s_agent_reads_and_writes_no_ones_rows() {
    let mut sql = DOCS.to_vec();
    sql.push("put docs {title: \"draft\", body: \"an unowned fox\", year: 2026}");
    let n = serve(
        Config {
            token: Some(ROOT.into()),
            access: Some(Arc::new(Access::new(SECRET, POLICY).unwrap())),
            mcp: Some(Arc::new(Options {
                write: true,
                ..opts()
            })),
            ..Config::default()
        },
        &sql,
    );
    let anonymous = n.token(r#"{"sub":null}"#);
    for (tool, args) in [
        ("query", r#"{"query":"get docs"}"#),
        ("get", r#"{"collection":"docs","ids":[4]}"#),
        ("search", r#"{"collection":"docs","text":"fox"}"#),
        (
            "write",
            r#"{"statement":"set docs {title: 'taken'} where id = 4"}"#,
        ),
        ("write", r#"{"statement":"insert docs {title: 'mine'}"}"#),
    ] {
        let (err, s) = n.call(Some(&anonymous), tool, args);
        assert!(err, "{tool} {args}: {s:?}");
        assert!(!format!("{s:?}").contains("unowned"), "{tool}: {s:?}");
    }
    let (_, s) = n.call(Some(&anonymous), "schema", "{}");
    assert!(!text(at(&s, &["fennecql"])).contains("docs"), "{s:?}");
    let (_, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get docs select title order id"}"#,
    );
    assert_eq!(at(&s, &["count"]), &Value::Int(4));
    assert_eq!(text(at(&s, &["rows", "3", "title"])), "draft");
}

#[test]
fn memories_are_found_by_words_then_by_meaning_and_go_when_they_expire() {
    let n = start(Options {
        memory: Some("notes".into()),
        ..opts()
    });
    // Nothing kept yet: nothing recalled, and nothing made.
    let (err, s) = n.call(Some(ROOT), "recall", r#"{"text":"anything"}"#);
    assert!(!err && at(&s, &["count"]) == &Value::Int(0));
    // Text alone first: the collection is made without a vector field...
    let (err, s) = n.call(
        Some(ROOT),
        "remember",
        r#"{"text":"the deploy runs on fridays","tags":["ops"]}"#,
    );
    assert!(!err, "{s:?}");
    // ...and given one at the first memory that comes with a vector.
    let (err, s) = n.call(
        Some(ROOT),
        "remember",
        r#"{"text":"staging is eu-west-1","vector":[0,0,1]}"#,
    );
    assert!(!err, "{s:?}");
    let (err, s) = n.call(
        Some(ROOT),
        "remember",
        r#"{"text":"wrong size","vector":[0,1]}"#,
    );
    assert!(
        err && text(at(&s, &["error"])).contains("vector<3>"),
        "{s:?}"
    );
    let (err, _) = n.call(
        Some(ROOT),
        "remember",
        r#"{"text":"a passing thought","expires_in":"1ms"}"#,
    );
    assert!(!err);
    std::thread::sleep(Duration::from_millis(5));
    let (_, s) = n.call(Some(ROOT), "schema", r#"{"collection":"notes"}"#);
    assert!(
        text(at(&s, &["fennecql"])).contains("vector vector<3> @hnsw(cosine"),
        "{s:?}"
    );
    // By words; by meaning, fused with the words; the latest first; the
    // one that expired out of every read.
    let (_, s) = n.call(
        Some(ROOT),
        "recall",
        r#"{"text":"when does the deploy run"}"#,
    );
    assert_eq!(
        text(at(&s, &["memories", "0", "text"])),
        "the deploy runs on fridays"
    );
    let (_, s) = n.call(
        Some(ROOT),
        "recall",
        r#"{"text":"which region","vector":[0,0.1,1]}"#,
    );
    assert_eq!(
        text(at(&s, &["memories", "0", "text"])),
        "staging is eu-west-1",
        "{s:?}"
    );
    let (_, s) = n.call(Some(ROOT), "recall", "{}");
    assert_eq!(at(&s, &["count"]), &Value::Int(2), "{s:?}");
    assert_eq!(
        text(at(&s, &["memories", "0", "text"])),
        "staging is eu-west-1"
    );
    assert_eq!(at(&s, &["memories", "0", "vector"]), &Value::Null);
    let (_, s) = n.call(Some(ROOT), "recall", r#"{"tags":["ops"]}"#);
    assert_eq!(at(&s, &["count"]), &Value::Int(1));
    let (err, s) = n.call(
        Some(ROOT),
        "remember",
        r#"{"text":"x","expires_in":"soon"}"#,
    );
    assert!(err && text(at(&s, &["error"])).contains("30m, 12h or 7d"));
}

#[test]
fn the_docs_give_the_memory_collection_remember_makes() {
    // site/content/docs/mcp.html's recipe is run by fennec-core's
    // tests/mcp_docs.rs; this holds its collection to the tool's.
    let page = include_str!("../../../site/content/docs/mcp.html");
    let made = mcp::memory_schema("memories", Some(3))
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    assert!(page.contains(&made), "the page lacks `{made}`");
}

#[test]
fn the_binding_s_headers_must_say_what_the_body_says() {
    let n = start(opts());
    let auth = format!("Bearer {ROOT}");
    let body = |method: &str, params: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":5,"method":"{method}","params":{{{params}"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#
        )
    };
    let post = |headers: &[(&str, &str)], b: &str| {
        let mut h = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(headers);
        n.post_raw("POST", "/_mcp", &h, b)
    };
    let list = body("tools/list", "");
    let a = post(&[("Mcp-Method", "tools/list")], &list);
    assert_eq!((a.status, error_code(&a)), (400, -32020), "{}", a.body);
    let a = post(
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
        ],
        &list,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32020));
    assert!(a.body.contains("does not match"), "{}", a.body);
    let call = body("tools/call", r#""name":"schema","arguments":{},"#);
    let a = post(
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
        ],
        &call,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32020), "{}", a.body);
    let a = post(
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "search"),
        ],
        &call,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32020));
    // A name may come in base64, as the binding encodes one.
    let encoded = format!("=?base64?{}?=", fennec_http::crypto::b64_encode(b"schema"));
    let a = post(
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", &encoded),
        ],
        &call,
    );
    assert_eq!(a.status, 200, "{}", a.body);

    // A revision not served names the ones that are.
    let old = body("tools/list", "").replace(MODERN, "1900-01-01");
    let a = post(
        &[
            ("MCP-Protocol-Version", "1900-01-01"),
            ("Mcp-Method", "tools/list"),
        ],
        &old,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32022));
    let v = a.json();
    assert_eq!(text(at(&v, &["error", "data", "requested"])), "1900-01-01");
    assert_eq!(
        text(at(&v, &["error", "data", "supported", "1"])),
        "2025-11-25"
    );
    // The client's capabilities are not optional.
    let bare = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"tools/list","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}"}}}}}}"#
    );
    let a = post(
        &[
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/list"),
        ],
        &bare,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32602));
    // An unknown method is a 404 with a JSON-RPC body, which a client
    // tells from a server with no endpoint by.
    let a = n.modern(Some(ROOT), 6, "resources/list", "{}");
    assert_eq!((a.status, error_code(&a)), (404, -32601));
    // `ping` is the handshake revisions'; 2026-07-28 has none.
    let a = n.modern(Some(ROOT), 6, "ping", "{}");
    assert_eq!(a.status, 404);
}

#[test]
fn a_handshake_client_initializes_and_calls_without_a_session() {
    let n = start(opts());
    let init = r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"old","version":"1"}}"#;
    let a = n.legacy(None, 1, "initialize", init);
    assert_eq!(a.status, 200, "{}", a.body);
    let v = a.json();
    assert_eq!(text(at(&v, &["result", "protocolVersion"])), "2025-06-18");
    assert_eq!(
        at(&v, &["result", "capabilities", "tools", "listChanged"]),
        &Value::Bool(false)
    );
    assert_eq!(text(at(&v, &["result", "serverInfo", "name"])), "fennecdb");
    // The 2026-07-28 members stay out of an older revision's results.
    assert_eq!(at(&v, &["result", "resultType"]), &Value::Null);
    assert!(!a.head.to_ascii_lowercase().contains("mcp-session-id"));
    // A version not served is answered with the newest that is.
    let a = n.legacy(
        None,
        1,
        "initialize",
        r#"{"protocolVersion":"2024-11-05","capabilities":{}}"#,
    );
    assert_eq!(
        text(at(&a.json(), &["result", "protocolVersion"])),
        "2025-11-25"
    );

    let a = n.post_raw(
        "POST",
        "/_mcp",
        &[
            ("Authorization", &format!("Bearer {ROOT}")),
            ("MCP-Protocol-Version", "2025-06-18"),
        ],
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    assert_eq!((a.status, a.body.as_str()), (202, ""));
    let a = n.legacy(Some("2025-06-18"), 2, "ping", "{}");
    assert_eq!(
        a.json(),
        fennec_core::json::parse_json(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#).unwrap()
    );
    let a = n.legacy(Some("2025-06-18"), 3, "tools/list", "{}");
    assert_eq!(
        text(at(&a.json(), &["result", "tools", "0", "name"])),
        "schema"
    );
    // With no header a request is 2025-03-26's, as the binding allows.
    let a = n.legacy(
        None,
        4,
        "tools/call",
        r#"{"name":"query","arguments":{"query":"get docs count"}}"#,
    );
    let v = a.json();
    assert_eq!(
        at(&v, &["result", "isError"]),
        &Value::Bool(false),
        "{}",
        a.body
    );
    assert_eq!(
        at(&v, &["result", "structuredContent", "rows", "0", "count"]),
        &Value::Int(3)
    );
    let a = n.legacy(Some("1999-01-01"), 5, "tools/list", "{}");
    assert_eq!((a.status, error_code(&a)), (400, -32022));
    let a = n.legacy(Some(MODERN), 5, "tools/list", "{}");
    assert_eq!((a.status, error_code(&a)), (400, -32602));
    let a = n.legacy(Some("2025-11-25"), 6, "prompts/list", "{}");
    assert_eq!((a.status, error_code(&a)), (200, -32601));
}

#[test]
fn what_is_not_one_request_is_refused_by_the_transport() {
    let n = start(opts());
    let auth = format!("Bearer {ROOT}");
    let h = [("Authorization", auth.as_str())];
    for method in ["GET", "DELETE", "PUT"] {
        let a = n.post_raw(method, "/_mcp", &h, "");
        assert_eq!(a.status, 405, "{method}");
        assert!(a.head.contains("Allow: POST"), "{}", a.head);
    }
    let a = n.post_raw("POST", "/_mcp", &h, "{nope");
    assert_eq!((a.status, error_code(&a)), (400, -32700));
    assert_eq!(at(&a.json(), &["id"]), &Value::Null);
    let a = n.post_raw(
        "POST",
        "/_mcp",
        &h,
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32600));
    let a = n.post_raw(
        "POST",
        "/_mcp",
        &h,
        r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
    );
    assert_eq!((a.status, error_code(&a)), (400, -32600));
    let a = n.post_raw(
        "POST",
        "/_mcp",
        &h,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
    );
    assert_eq!(a.status, 400);
    // A client's response to a request this server never sends.
    let a = n.post_raw(
        "POST",
        "/_mcp",
        &h,
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
    );
    assert_eq!(a.status, 400);
    // Without the token, 401 as anywhere; a stranger's token likewise.
    let a = n.post_raw(
        "POST",
        "/_mcp",
        &[],
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    );
    assert_eq!(a.status, 401);
    assert!(a.head.contains("WWW-Authenticate: Bearer"));
}

#[test]
fn another_origin_and_a_rebound_name_are_refused_before_the_token() {
    // An open server on loopback: what a DNS rebinding attack reaches.
    let open = serve(
        Config {
            mcp: Some(Arc::new(opts())),
            ..Config::default()
        },
        DOCS,
    );
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let a = open.post_raw(
        "POST",
        "/_mcp",
        &[
            ("Host", "evil.example:8080"),
            ("Origin", "http://evil.example:8080"),
        ],
        ping,
    );
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(a.body.contains("not this machine's"), "{}", a.body);
    let a = open.post_raw("POST", "/_mcp", &[("Origin", "https://evil.example")], ping);
    assert_eq!(a.status, 403);
    let own = format!("http://127.0.0.1:{}", open.port);
    let a = open.post_raw("POST", "/_mcp", &[("Origin", own.as_str())], ping);
    assert_eq!(a.status, 200, "{}", a.body);
    let a = open.post_raw("POST", "/_mcp", &[("Host", "localhost:9")], ping);
    assert_eq!(a.status, 200);
    // A server with a token reaches past a name of its own, and a page of
    // the origin --http-cors names reaches it.
    let guarded = serve(
        Config {
            token: Some(ROOT.into()),
            cors: Some("https://app.example".into()),
            mcp: Some(Arc::new(opts())),
            ..Config::default()
        },
        DOCS,
    );
    let auth = format!("Bearer {ROOT}");
    let a = guarded.post_raw(
        "POST",
        "/_mcp",
        &[("Host", "db.example"), ("Authorization", &auth)],
        ping,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    let a = guarded.post_raw(
        "POST",
        "/_mcp",
        &[("Origin", "https://app.example"), ("Authorization", &auth)],
        ping,
    );
    assert_eq!(a.status, 200);
    let a = guarded.post_raw(
        "POST",
        "/_mcp",
        &[
            ("Origin", "https://other.example"),
            ("Authorization", &auth),
        ],
        ping,
    );
    assert_eq!(a.status, 403);
}

#[test]
fn the_write_tool_is_off_until_asked_for_and_then_writes_rows_alone() {
    let n = start(Options {
        write: true,
        ..opts()
    });
    let (err, s) = n.call(
        Some(ROOT),
        "write",
        r#"{"statement":"insert docs {title: $1, year: $2}","params":["new",2026]}"#,
    );
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["affected"]), &Value::Int(1));
    let (err, s) = n.call(Some(ROOT), "write", r#"{"statement":"set docs {year: year + 1} where title = $1 returning title, year","params":["new"]}"#);
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["rows", "0", "year"]), &Value::Int(2027));
    let (err, s) = n.call(
        Some(ROOT),
        "write",
        r#"{"statement":"del docs where id = 999 require 1"}"#,
    );
    assert!(err && int(at(&s, &["status"])) == 412, "{s:?}");
    assert_eq!(at(&s, &["reason"]), &Value::Null, "{s:?}");
    // A `require` that says why: the reason beside the words, as /query's
    // 412 gives it, for the model to tell one refusal from another by.
    let (err, s) = n.call(
        Some(ROOT),
        "write",
        r#"{"statement":"del docs where id = 999 require 1 else 'no such doc'"}"#,
    );
    assert!(err && int(at(&s, &["status"])) == 412, "{s:?}");
    assert_eq!(at(&s, &["reason"]), &Value::Text("no such doc".into()));
    for (stmt, has) in [
        ("get docs", "a read goes through query"),
        ("create collection x (a int)", "not made over MCP"),
        ("compact", "not made over MCP"),
        ("create index on docs (year) @sorted", "not made over MCP"),
    ] {
        let (err, s) = n.call(Some(ROOT), "write", &format!(r#"{{"statement":"{stmt}"}}"#));
        assert!(
            err && text(at(&s, &["error"])).contains(has),
            "{stmt}: {s:?}"
        );
    }
    // A server that takes no writes takes none over MCP either.
    let ro = serve(
        Config {
            token: Some(ROOT.into()),
            read_only: true,
            mcp: Some(Arc::new(Options {
                write: true,
                ..opts()
            })),
            ..Config::default()
        },
        DOCS,
    );
    let (err, s) = ro.call(
        Some(ROOT),
        "write",
        r#"{"statement":"insert docs {title: 'x'}"}"#,
    );
    assert!(err && int(at(&s, &["status"])) == 403, "{s:?}");
}

/// A call its client marked as a read (`Fennec-Read: replica`) runs
/// read-only, as every route does: the endpoint answered before the mark
/// was read, and a router sends a marked request to a replica -- or again
/// to home, its answer lost, since a read cannot run twice.
#[test]
fn a_write_tool_call_marked_as_a_read_is_refused() {
    let n = start(Options {
        write: true,
        ..opts()
    });
    let body = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write","arguments":{"statement":"insert docs {title: 'marked'}"}}}"#;
    let auth = format!("Bearer {ROOT}");
    let marked = [
        ("Authorization", auth.as_str()),
        ("MCP-Protocol-Version", "2025-06-18"),
        ("Fennec-Read", "replica"),
    ];
    let a = n.post_raw("POST", "/_mcp", &marked, body);
    assert_eq!(a.status, 200, "{}", a.body);
    let v = a.json();
    assert_eq!(
        at(&v, &["result", "isError"]),
        &Value::Bool(true),
        "{}",
        a.body
    );
    let r = at(&v, &["result", "structuredContent"]);
    assert_eq!(int(at(r, &["status"])), 403, "{}", a.body);
    assert!(
        text(at(r, &["error"])).contains("Fennec-Read: replica only reads"),
        "{}",
        a.body
    );
    let (err, s) = n.call(
        Some(ROOT),
        "query",
        r#"{"query":"get docs where title = 'marked' count"}"#,
    );
    assert!(!err, "{s:?}");
    assert_eq!(at(&s, &["rows", "0", "count"]), &Value::Int(0), "{s:?}");
    // A read marked so is answered as any.
    let read = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"query","arguments":{"query":"get docs count"}}}"#;
    let a = n.post_raw("POST", "/_mcp", &marked, read);
    let v = a.json();
    assert_eq!(
        at(&v, &["result", "isError"]),
        &Value::Bool(false),
        "{}",
        a.body
    );
    // And a value the mark does not take is refused before the call.
    let wrong = [
        ("Authorization", auth.as_str()),
        ("MCP-Protocol-Version", "2025-06-18"),
        ("Fennec-Read", "nearest"),
    ];
    assert_eq!(n.post_raw("POST", "/_mcp", &wrong, read).status, 400);
}

#[test]
fn a_caller_past_its_rate_is_told_when_to_call_again() {
    let n = start_scoped(Options {
        calls_per_sec: 2,
        ..opts()
    });
    // A subject of this test's own: the buckets are the process's.
    let t = n.token(r#"{"sub":"rate-test"}"#);
    let mut refused = 0;
    for _ in 0..8 {
        let (err, s) = n.call(Some(&t), "schema", "{}");
        if err {
            assert_eq!(at(&s, &["status"]), &Value::Int(429));
            assert!(text(at(&s, &["error"])).contains("call again in"));
            // When, in seconds, as every 429 of the server says it.
            assert_eq!(at(&s, &["retry_after"]), &Value::Int(1), "{s:?}");
            refused += 1;
        }
    }
    assert!(refused >= 1, "eight calls at once, two a second");
}

#[test]
fn a_tenant_s_endpoint_is_under_its_prefix_and_bound_to_its_token() {
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-mcp-tenants-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    for name in ["acme", "globex"] {
        let t = tenants.create(name).unwrap();
        setup(&mut t.db.write().unwrap(), DOCS);
    }
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let server = Server::with_tenants(
        Arc::clone(&tenants),
        Config {
            addr: "127.0.0.1:0".into(),
            access: Some(Arc::clone(&access)),
            admin_token: Some("adm".into()),
            mcp: Some(Arc::new(opts())),
            ..Config::default()
        },
    );
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let n = Node {
        port,
        access: Some(access),
    };
    let acme = n.token(r#"{"sub":"alice","tenant":"acme"}"#);
    let a = n.modern_at(
        "/t/acme/_mcp",
        Some(&acme),
        1,
        "tools/call",
        r#"{"name":"query","arguments":{"query":"get docs"}}"#,
    );
    let v = a.json();
    assert_eq!(
        at(&v, &["result", "structuredContent", "count"]),
        &Value::Int(2),
        "{}",
        a.body
    );
    let a = n.modern_at("/t/globex/_mcp", Some(&acme), 1, "tools/list", "{}");
    assert_eq!(a.status, 403, "{}", a.body);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stdio_answers_a_line_with_a_line_and_a_notification_with_nothing() {
    let mut db = Database::new();
    setup(&mut db, DOCS);
    let db = Arc::new(RwLock::new(db));
    let cfg = Config::default();
    let o = Options {
        memory: Some("memories".into()),
        ..opts()
    };
    let ctx = mcp::Ctx {
        db: &db,
        cfg: &cfg,
        who: &Who::Full,
        opts: &o,
        run: mcp::Run::local(),
    };
    let meta = format!(
        r#""_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}","io.modelcontextprotocol/clientCapabilities":{{}}}}"#
    );
    let input = format!(
        "{}\n{}\n\n{}\n{}\nnot json\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        format_args!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"remember","arguments":{{"text":"stdio works"}},{meta}}}}}"#
        ),
        format_args!(
            r#"{{"jsonrpc":"2.0","id":"three","method":"tools/call","params":{{"name":"recall","arguments":{{"text":"stdio"}},{meta}}}}}"#
        ),
    );
    let mut out = Vec::new();
    mcp::serve_lines(&ctx, input.as_bytes(), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert!(lines[0].contains("\"protocolVersion\":\"2025-11-25\""));
    assert!(lines[1].contains("\"remembered\":1"), "{}", lines[1]);
    let v = fennec_core::json::parse_json(lines[2]).unwrap();
    assert_eq!(text(at(&v, &["id"])), "three");
    assert_eq!(
        text(at(
            &v,
            &["result", "structuredContent", "memories", "0", "text"]
        )),
        "stdio works"
    );
    assert!(lines[3].contains("-32700"));
}

#[test]
fn the_bridge_carries_a_stdio_client_to_a_server_s_endpoint() {
    let n = start_scoped(Options {
        memory: Some("memories".into()),
        ..opts()
    });
    let bob = n.token(r#"{"sub":"bob"}"#);
    let up = mcp::bridge::Upstream::parse(&format!("http://127.0.0.1:{}/_mcp", n.port), Some(bob))
        .unwrap();
    let meta = format!(
        r#""_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}","io.modelcontextprotocol/clientCapabilities":{{}}}}"#
    );
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        // The handshake's version goes out as the header from here on.
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"query","arguments":{"query":"get docs"}}}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"get","arguments":{{"collection":"docs","ids":[2]}},{meta}}}}}"#),
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}"#.to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    mcp::bridge::bridge(&up, input.as_bytes(), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    let v = fennec_core::json::parse_json(lines[1]).unwrap();
    // Bob's rows alone: the server held the bridge's token to its rules.
    assert_eq!(
        at(&v, &["result", "structuredContent", "count"]),
        &Value::Int(1),
        "{}",
        lines[1]
    );
    let v = fennec_core::json::parse_json(lines[2]).unwrap();
    assert_eq!(
        text(at(
            &v,
            &["result", "structuredContent", "rows", "0", "owner"]
        )),
        "bob",
        "{}",
        lines[2]
    );

    // A token the server refuses, and a server that is not there, are the
    // bridge's own errors, answering the request they stopped.
    let wrong = mcp::bridge::Upstream::parse(
        &format!("http://127.0.0.1:{}/_mcp", n.port),
        Some("nope".into()),
    )
    .unwrap();
    let mut out = Vec::new();
    mcp::bridge::bridge(
        &wrong,
        &b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n"[..],
        &mut out,
    )
    .unwrap();
    let v = fennec_core::json::parse_json(String::from_utf8(out).unwrap().trim()).unwrap();
    assert_eq!(at(&v, &["id"]), &Value::Int(9));
    assert!(
        text(at(&v, &["error", "message"])).contains("fennec mcp: the server answered 401"),
        "{v:?}"
    );
    let gone = mcp::bridge::Upstream::parse("http://127.0.0.1:9/_mcp", None).unwrap();
    let mut out = Vec::new();
    mcp::bridge::bridge(
        &gone,
        &b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"[..],
        &mut out,
    )
    .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("could not reach"));
}

#[test]
fn tool_calls_are_counted_by_tool_in_the_metrics() {
    let n = start(opts());
    n.call(Some(ROOT), "schema", "{}");
    n.call(Some(ROOT), "search", r#"{"collection":"docs"}"#);
    let a = n.post_raw(
        "GET",
        "/_metrics",
        &[("Authorization", &format!("Bearer {ROOT}"))],
        "",
    );
    let count = |line: &str| -> u64 {
        a.body
            .lines()
            .find(|l| l.starts_with(line))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no `{line}` in\n{}", a.body))
    };
    // Other tests of the binary count beside these: at least.
    assert!(count(r#"fennec_mcp_tool_calls_total{tool="schema",outcome="ok"}"#) >= 1);
    assert!(count(r#"fennec_mcp_tool_calls_total{tool="search",outcome="error"}"#) >= 1);
}
