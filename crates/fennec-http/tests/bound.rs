//! Write-bound statements over HTTP (`docs/design/named-statements.md`,
//! F2's slices 3 and 4): the server's token defines one and every way it
//! writes wakes it -- `/query`, `/batch`, a call's body -- in the block of
//! the write. A scoped token wakes one only where a policy's `wake` line
//! grants it, at every way in -- `/query`, `/batch`, `/_call`, MCP's
//! `write`, an `Idempotency-Key` write, a held write -- the binding's
//! writes its text's authority, never the token's rules; one not granted,
//! or one updating or deleting in an `append-only` collection, is refused
//! (403) before it runs, nothing landed, and its runs are charged to
//! `--call-rate`. A binding's refusal is told the token with its status
//! and `reason` alone, the server's token with its name. `--max-memory`
//! holds a `del` by the rows its binding inserts; and no statement is
//! bound to the database's own collections or writes one.

use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::mcp::Options;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
/// No `wake` line: a customer's write waking a binding is refused.
const POLICY: &str = "orders  read,insert,update(status)  where by = $jwt.sub  for customer
ledger  read  for customer
notes  read,insert,delete  for customer
tally  append-only
pay, open, unnote, paid  call  for customer
";
/// The same, with each binding's waking granted to a customer -- and
/// nothing on `ledger` or `tally` but what it had.
const GRANTED: &str = "orders  read,insert,update(status)  where by = $jwt.sub  for customer
ledger  read  for customer
notes  read,insert,delete  for customer
tally  append-only
pay, open, unnote, paid  call  for customer
paid, voided, unnoted, untally  wake  for customer
";
const SCHEMA: [&str; 11] = [
    "create collection orders (status text, total int, by text)",
    "create collection ledger (o int, amount int)",
    "create collection notes (n int)",
    "create collection tally (n int)",
    "create statement paid after set orders where new.status = 'paid' and old.status = 'open'
     as insert ledger {o: new.id, amount: new.total}",
    "create statement voided after set orders where new.status = 'void' scans as begin
       get ledger select o where o = new.id limit 1 require 1 else 'no ledger line';
       insert ledger {o: new.id, amount: 0}
     end",
    "create statement unnoted after del notes as insert ledger {o: old.n, amount: 0}",
    "create statement untally after del notes scans as del tally where n = old.n",
    "create statement pay($id int) public as set orders {status: 'paid'} where id = $id",
    "create statement open($n int) public as insert orders {status: 'open', total: $n, by: 'x'}",
    "create statement unnote($n int) public scans as del notes where n = $n",
];

struct Node {
    port: u16,
    access: Arc<Access>,
    server: Arc<Server>,
}

fn serve(
    db: &Arc<RwLock<Database>>,
    access: &Arc<Access>,
    tune: impl FnOnce(&mut Config),
) -> (u16, Arc<Server>) {
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(access)),
        call_rate: 0,
        mcp: Some(Arc::new(Options {
            write: true,
            ..Options::default()
        })),
        ..Config::default()
    };
    tune(&mut cfg);
    let server = Arc::new(Server::new(Arc::clone(db), cfg));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = serving.serve_on(listener);
    });
    (port, server)
}

fn line(q: &str) -> String {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    body
}

/// An answer: its status, head and body.
struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Node {
    fn request(
        &self,
        method: &str,
        token: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let extra: String = headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect();
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n{extra}\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        Answer {
            status: out[9..12].parse().unwrap(),
            head: head.to_ascii_lowercase(),
            body: body.to_string(),
        }
    }

    fn post(&self, token: &str, target: &str, body: &str) -> (u16, String) {
        let a = self.request("POST", token, target, &[], body);
        (a.status, a.body)
    }

    fn query(&self, token: &str, q: &str) -> (u16, String) {
        self.post(token, "/query", &line(q))
    }

    fn ok(&self, token: &str, q: &str) -> String {
        let (status, body) = self.query(token, q);
        assert_eq!(status, 200, "{q}: {body}");
        body
    }

    /// MCP's `write` tool, as a model calls it: whether it answered an
    /// error, and what.
    fn mcp_write(&self, token: &str, statement: &str) -> (bool, String) {
        let mut args = String::from("{\"statement\":");
        fennec_core::json::escape_into(&mut args, statement);
        args.push('}');
        let a = self.mcp(
            token,
            "tools/call",
            &format!(r#"{{"name":"write","arguments":{args}}}"#),
        );
        assert_eq!(a.status, 200, "{}", a.body);
        (a.body.contains(r#""isError":true"#), a.body)
    }

    fn mcp(&self, token: &str, method: &str, params: &str) -> Answer {
        const MODERN: &str = "2026-07-28";
        let meta = format!(
            r#""_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}","io.modelcontextprotocol/clientCapabilities":{{}},"io.modelcontextprotocol/clientInfo":{{"name":"test","version":"1"}}}}"#
        );
        let params = match params.trim() {
            "{}" => format!("{{{meta}}}"),
            p => format!("{},{meta}}}", &p[..p.len() - 1]),
        };
        let body = format!(r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{params}}}"#);
        let mut headers = vec![("MCP-Protocol-Version", MODERN), ("Mcp-Method", method)];
        if method == "tools/call" {
            headers.push(("Mcp-Name", "write"));
        }
        self.request("POST", token, "/_mcp", &headers, &body)
    }

    fn customer(&self, sub: &str) -> String {
        let claims = format!(r#"{{"sub":"{sub}","role":"customer"}}"#);
        self.access.mint(&claims).unwrap()
    }
}

fn start(tune: impl FnOnce(&mut Config)) -> (Node, Arc<RwLock<Database>>) {
    start_with(POLICY, tune)
}

/// A server under `policy`: the schema above, and orders 1, 2 and 4 to 10
/// a customer `u0`'s, 3 `u1`'s, each open and of ten times its id.
fn start_with(policy: &str, tune: impl FnOnce(&mut Config)) -> (Node, Arc<RwLock<Database>>) {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, policy).unwrap());
    let db = Arc::new(RwLock::new(Database::new()));
    let (port, server) = serve(&db, &access, |_| {});
    let node = Node {
        port,
        access: Arc::clone(&access),
        server,
    };
    for s in SCHEMA {
        node.ok(ROOT, s);
    }
    for id in 1..=10 {
        let by = if id == 3 { "u1" } else { "u0" };
        let total = id * 10;
        node.ok(
            ROOT,
            &format!("put orders {{status: 'open', total: {total}, by: '{by}'}}"),
        );
    }
    let (port, server) = serve(&db, &access, tune);
    let tuned = Node {
        port,
        access,
        server,
    };
    (tuned, db)
}

fn rows(db: &Arc<RwLock<Database>>, q: &str) -> Vec<Vec<Value>> {
    let q = fennec_ql::parse_one(q).unwrap();
    match db.read().unwrap().query(&q, &[]).unwrap() {
        Response::Rows(rs) => rs.rows.into_iter().map(|r| r.values).collect(),
        other => panic!("{other:?}"),
    }
}

fn ledger(db: &Arc<RwLock<Database>>) -> Vec<Vec<Value>> {
    rows(db, "get ledger select o, amount order id")
}

fn line_of(o: i64, amount: i64) -> Vec<Value> {
    vec![Value::Int(o), Value::Int(amount)]
}

/// The orders still open, by id.
fn open(db: &Arc<RwLock<Database>>) -> Vec<i64> {
    let ids = rows(db, "get orders select id where status = 'open' order id");
    ids.iter()
        .map(|r| match r[0] {
            Value::Int(i) => i,
            _ => unreachable!(),
        })
        .collect()
}

/// Each way a scoped token writes paying order `id`, granted or not: a
/// write passing `paid`'s condition, its answer's status and body.
fn pay_by(node: &Node, token: &str, way: &str, id: i64) -> (u16, String) {
    let q = format!("set orders {{status: 'paid'}} where id = {id}");
    match way {
        "/query" => node.query(token, &q),
        "/batch" => node.post(
            token,
            "/batch",
            &[line(&q), line("get ledger count")].join("\n"),
        ),
        "/_call" => node.post(
            token,
            "/_call/pay",
            &format!(r#"{{"params":{{"id":{id}}}}}"#),
        ),
        "mcp" => match node.mcp_write(token, &q) {
            (true, body) if body.contains("403") || body.contains("would wake") => (403, body),
            (true, body) => (500, body),
            (false, body) => (200, body),
        },
        "key" => {
            let key = format!("pay-{id}");
            let a = node.request(
                "POST",
                token,
                "/query",
                &[("Idempotency-Key", &key)],
                &line(&q),
            );
            (a.status, a.body)
        }
        "wait" => {
            let a = node.request(
                "POST",
                token,
                "/query",
                &[("Fennec-Wait", "2000")],
                &line(&q),
            );
            (a.status, a.body)
        }
        _ => unreachable!(),
    }
}

const WAYS: [(&str, i64); 6] = [
    ("/query", 1),
    ("/batch", 2),
    ("/_call", 4),
    ("mcp", 5),
    ("key", 6),
    ("wait", 7),
];

#[test]
fn the_server_s_token_wakes_a_binding_at_every_way_in() {
    let (node, db) = start(|_| {});
    node.ok(ROOT, "set orders {status: 'paid'} where id = 1");
    let batch = [
        line("set orders {status: 'paid'} where id = 2"),
        line("get ledger count"),
    ]
    .join("\n");
    let (status, body) = node.post(ROOT, "/batch", &batch);
    assert_eq!(status, 200, "{body}");
    node.ok(ROOT, "call pay(3)");
    assert_eq!(
        ledger(&db),
        [line_of(1, 10), line_of(2, 20), line_of(3, 30)]
    );
    // A `/batch` whose later statement fails puts back what the binding
    // wrote with the rest.
    let batch = [
        line("set orders {status: 'paid'} where id = 4"),
        line("insert orders {id: 1, status: 'x'}"),
    ]
    .join("\n");
    let (status, body) = node.post(ROOT, "/batch", &batch);
    assert_eq!(status, 409, "{body}");
    assert_eq!(ledger(&db).len(), 3);
}

/// A customer granted `paid wake`, and nothing on `ledger`, pays an order
/// at every way in: the line its binding writes lands with the write, no
/// rule of the customer's judging it.
#[test]
fn a_granted_scoped_write_wakes_its_binding_at_every_way_in() {
    let (node, db) = start_with(GRANTED, |_| {});
    let u0 = node.customer("u0");
    for (way, id) in WAYS {
        let (status, body) = pay_by(&node, &u0, way, id);
        assert_eq!(status, 200, "{way}: {body}");
    }
    let expected: Vec<_> = WAYS.iter().map(|&(_, id)| line_of(id, id * 10)).collect();
    assert_eq!(ledger(&db), expected);
    // A keyed write sent again is answered as the first time, and wakes
    // nothing again.
    let (status, body) = pay_by(&node, &u0, "key", 6);
    assert_eq!(status, 200, "{body}");
    assert_eq!(ledger(&db).len(), WAYS.len());
    // The customer's own write to `ledger` is still refused: the binding's
    // authority is its text's, never lent to its writer.
    let (status, body) = node.query(&u0, "insert ledger {o: 99, amount: 1}");
    assert_eq!(status, 403, "{body}");
    // Its rules still judge its own write: an order of another's is none
    // it may pay, and wakes nothing.
    let (status, body) = node.query(&u0, "set orders {status: 'paid'} where id = 3 require 1");
    assert_eq!(status, 412, "{body}");
    assert_eq!(ledger(&db).len(), WAYS.len());
}

/// No `wake` line: each way in refused (403) before the binding runs,
/// nothing landed, no binding named; writes waking none land as ever.
#[test]
fn an_ungranted_scoped_write_is_refused_at_every_way_in() {
    let (node, db) = start(|_| {});
    let u0 = node.customer("u0");
    for (way, id) in WAYS {
        let (status, body) = pay_by(&node, &u0, way, id);
        assert_eq!(status, 403, "{way}: {body}");
        assert!(!body.contains("paid`"), "{way}: no name told: {body}");
    }
    assert!(ledger(&db).is_empty());
    assert_eq!(open(&db), (1..=10).collect::<Vec<_>>());
    // A refused keyed write keeps no key: granted later, it would run.
    let (status, body) = node.query(ROOT, "get _idempotency count");
    assert_eq!(status, 404, "no key was ever kept: {body}");
    // Its writes waking none land as ever: a row the condition does not
    // pass, and a row made, which nothing is bound to.
    node.ok(&u0, "set orders {status: 'held'} where id = 8");
    node.ok(&u0, "insert orders {status: 'open', total: 5, by: 'u0'}");
    node.ok(&u0, "call open(7)");
    assert!(ledger(&db).is_empty());
    // The server's token's same write lands, the line with it.
    node.ok(ROOT, "set orders {status: 'paid'} where id = 1");
    assert_eq!(ledger(&db), [line_of(1, 10)]);
}

/// A binding updating or deleting in an `append-only` collection is
/// refused a scoped token's write, granted or not, and a call whose body
/// may wake it before the body runs; the server's token's write lands.
#[test]
fn a_binding_reaching_an_append_only_collection_is_refused_a_scoped_write() {
    let (node, db) = start_with(GRANTED, |_| {});
    let u0 = node.customer("u0");
    node.ok(ROOT, "insert tally {n: 1}");
    node.ok(&u0, "insert notes {n: 1}");
    let (status, body) = node.query(&u0, "del notes where n = 1");
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("append-only"), "{body}");
    assert!(!body.contains("untally"), "{body}");
    let (status, body) = node.query(&u0, "call unnote(1)");
    assert_eq!(status, 403, "{body}");
    assert_eq!(rows(&db, "get notes select n").len(), 1);
    assert!(ledger(&db).is_empty());
    node.ok(ROOT, "del notes where n = 1");
    assert!(rows(&db, "get tally select n").is_empty());
    assert_eq!(ledger(&db), [line_of(1, 0)]);
}

/// A granted call wakes what its body writes; an ungranted one is refused
/// before its body runs (403), and a bound statement is called by none --
/// to a scoped token the 404 a missing one is, though a `call` line names
/// it, to the server's a 400 saying why -- and is no MCP tool.
#[test]
fn a_call_is_judged_by_what_its_body_may_wake() {
    let (granted, db) = start_with(GRANTED, |_| {});
    let u0 = granted.customer("u0");
    granted.ok(&u0, "call pay(4)");
    assert_eq!(ledger(&db), [line_of(4, 40)]);
    let (status, body) = granted.query(&u0, "call paid");
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("statement `paid`"), "{body}");
    assert!(!body.contains("bound"), "{body}");
    let (status, body) = granted.query(ROOT, "call paid");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("bound after"), "{body}");
    let tools = granted.mcp(&u0, "tools/list", "{}");
    assert_eq!(tools.status, 200, "{}", tools.body);
    assert!(tools.body.contains("call_pay"), "{}", tools.body);
    assert!(!tools.body.contains("call_paid"), "{}", tools.body);

    let (refused, db) = start(|_| {});
    let u0 = refused.customer("u0");
    let (status, body) = refused.query(&u0, "call pay(4)");
    assert_eq!(status, 403, "{body}");
    assert!(!body.contains("paid`"), "{body}");
    assert!(ledger(&db).is_empty());
    assert!(open(&db).contains(&4));
}

/// A binding's refusal is its writer's: the block put back whole, a
/// scoped writer told its status and the guard's `reason` alone, never the
/// binding's name or the engine's words; the server's token both.
#[test]
fn a_binding_s_refusal_is_told_the_writer_as_its_token_may_know_it() {
    let (node, db) = start_with(GRANTED, |_| {});
    let u0 = node.customer("u0");
    let q = "set orders {status: 'void'} where id = 1";
    let (status, body) = node.query(&u0, q);
    assert_eq!(status, 412, "{body}");
    assert!(body.contains(r#""reason":"no ledger line""#), "{body}");
    assert!(body.contains("a write bound to it refused it"), "{body}");
    assert!(
        !body.contains("voided") && !body.contains("get ledger"),
        "{body}"
    );
    let (status, body) = node.post(
        &u0,
        "/batch",
        &[line("get orders count"), line(q)].join("\n"),
    );
    assert_eq!(status, 412, "{body}");
    assert!(body.contains(r#""at":1"#), "{body}");
    assert!(body.contains(r#""reason":"no ledger line""#), "{body}");
    assert!(!body.contains("voided"), "{body}");
    assert_eq!(open(&db), (1..=10).collect::<Vec<_>>());
    let (status, body) = node.query(ROOT, q);
    assert_eq!(status, 412, "{body}");
    assert!(body.contains(r#""binding":"voided""#), "{body}");
    assert!(
        body.contains("bound statement `voided`, after `set` on `orders`, refused this write:"),
        "{body}"
    );
    assert!(body.contains(r#""reason":"no ledger line""#), "{body}");
    assert_eq!(open(&db), (1..=10).collect::<Vec<_>>());
    // A keyed `/query` and a `/batch` name it to the server's token alike,
    // the batch at the statement it stopped at.
    let a = node.request(
        "POST",
        ROOT,
        "/query",
        &[("Idempotency-Key", "v-1")],
        &line(q),
    );
    assert_eq!(a.status, 412, "{}", a.body);
    assert!(a.body.contains(r#""binding":"voided""#), "{}", a.body);
    assert!(a.body.contains("bound statement `voided`"), "{}", a.body);
    assert!(
        a.body.contains(r#""reason":"no ledger line""#),
        "{}",
        a.body
    );
    let (status, body) = node.post(
        ROOT,
        "/batch",
        &[line("get orders count"), line(q)].join("\n"),
    );
    assert_eq!(status, 412, "{body}");
    assert!(body.contains(r#""at":1"#), "{body}");
    assert!(body.contains(r#""binding":"voided""#), "{body}");
    assert!(
        body.contains(
            r#""error":"bound statement `voided`, after `set` on `orders`, refused this write: "#
        ),
        "{body}"
    );
    assert!(body.contains(r#""reason":"no ledger line""#), "{body}");
    assert_eq!(open(&db), (1..=10).collect::<Vec<_>>());
}

/// A binding whose condition reads a field the token is not shown is
/// judged on every write of the token's reaching it, a row passing the
/// condition or not: judged where one passed alone, the 403 against the
/// 200 told the token the field's value. One reading shown fields alone is
/// asked as a row passes it, as ever.
#[test]
fn a_condition_reading_an_unshown_field_is_judged_whatever_it_answers() {
    const CARDS: &str = "cards  read(holder, note)  where holder = $jwt.sub  for customer
cards  update(note)  where holder = $jwt.sub  for customer
";
    let granted = format!("{POLICY}{CARDS}overdrawn  wake  for customer\n");
    for (grant, policy) in [(false, format!("{POLICY}{CARDS}")), (true, granted)] {
        let (node, db) = start_with(&policy, |_| {});
        node.ok(
            ROOT,
            "create collection cards (holder text, cap int, note text)",
        );
        node.ok(
            ROOT,
            "create statement overdrawn after set cards where old.cap > 100
             as insert ledger {o: new.id, amount: 1}",
        );
        node.ok(
            ROOT,
            "put cards [{holder: 'u0', cap: 500, note: ''}, {holder: 'u0', cap: 50, note: ''}]",
        );
        let u0 = node.customer("u0");
        // Card 2, under the cap, passes no condition; card 1 does. The
        // customer reads neither's `cap`, and is told the same of both.
        let low = node.query(&u0, "set cards {note: 'a'} where id = 2");
        let high = node.query(&u0, "set cards {note: 'a'} where id = 1");
        let (status, lines) = match grant {
            false => (403, vec![]),
            true => (200, vec![line_of(1, 1)]),
        };
        assert_eq!(low.0, status, "granted {grant}: {}", low.1);
        assert_eq!(high.0, status, "granted {grant}: {}", high.1);
        assert_eq!(ledger(&db), lines);
    }
    // `paid` reads `status` alone, which a customer is shown: asked as a
    // row passes it, so an ungranted write passing none lands.
    let (node, db) = start(|_| {});
    let u0 = node.customer("u0");
    node.ok(&u0, "set orders {status: 'held'} where id = 1");
    let (status, body) = node.query(&u0, "set orders {status: 'paid'} where id = 2");
    assert_eq!(status, 403, "{body}");
    assert!(ledger(&db).is_empty());
}

/// A scoped writer's runs are charged to its `--call-rate`: past the
/// bucket 429 with when to come back, its block put back; more runs than
/// a full bucket holds 400, never told to come back; a write waking none
/// charged nothing, and the server's token unheld.
#[test]
fn a_scoped_writer_s_runs_are_charged_to_its_call_rate() {
    let (node, db) = start_with(GRANTED, |c| c.call_rate = 1);
    let u0 = node.customer("u0");
    node.ok(&u0, "set orders {status: 'paid'} where id = 1");
    // The bucket is empty for the second that follows.
    let q = line("set orders {status: 'paid'} where id = 2");
    let a = node.request("POST", &u0, "/query", &[], &q);
    assert_eq!(a.status, 429, "{}", a.body);
    assert!(a.head.contains("retry-after: 1"), "{}", a.head);
    assert!(a.body.contains(r#""retry_after":1"#), "{}", a.body);
    assert!(a.body.contains("--call-rate"), "{}", a.body);
    assert_eq!(ledger(&db), [line_of(1, 10)]);
    // More runs than a full bucket holds: never let through.
    let (status, body) = node.query(&u0, "set orders {status: 'paid'} where id = 4 or id = 5");
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("--call-rate") && body.contains("never"),
        "{body}"
    );
    // A write waking none is charged nothing.
    node.ok(&u0, "set orders {status: 'held'} where id = 6");
    // The server's token is unheld.
    node.ok(ROOT, "set orders {status: 'paid'} where status = 'open'");
    assert_eq!(ledger(&db).len(), 9);
}

/// `/_whoami` lists the bindings a token's writes may wake.
#[test]
fn whoami_lists_a_token_s_wakes() {
    let (node, _db) = start_with(GRANTED, |_| {});
    let u0 = node.customer("u0");
    let a = node.request("GET", &u0, "/_whoami", &[], "");
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(
        a.body
            .contains(r#""wakes":["paid","voided","unnoted","untally"]"#),
        "{}",
        a.body
    );
}

/// The database's own collections are written beside a client's write --
/// a scoped token's `Idempotency-Key` row outside `within()`, where no
/// hook judges its wake -- so no statement is bound to one or writes one.
#[test]
fn no_statement_is_bound_to_the_database_s_own_collections() {
    let (node, db) = start(|_| {});
    for q in [
        "create statement keyed after insert _idempotency as insert ledger {o: 0, amount: 0}",
        "create statement consumed after set or del _consumers as insert ledger {o: 0}",
        "create statement keys after set orders as insert _idempotency {key: 'x'}",
    ] {
        let (status, body) = node.query(ROOT, q);
        assert_eq!(status, 400, "{q}: {body}");
        assert!(body.contains("is the database's own"), "{q}: {body}");
    }
    let (status, body) = node.query(ROOT, "statements");
    assert_eq!(status, 200, "{body}");
    assert!(
        !body.contains("keyed") && !body.contains("consumed"),
        "{body}"
    );
    assert!(ledger(&db).is_empty());
}

#[test]
fn a_del_whose_binding_inserts_is_held_to_the_data_ceiling() {
    let (full, db) = start(|c| c.max_memory = 1);
    db.write()
        .unwrap()
        .execute(&fennec_ql::parse_one("put notes {n: 1}").unwrap())
        .unwrap();
    let (status, body) = full.query(ROOT, "del notes where n >= 0");
    assert_eq!(status, 507, "{body}");
    assert!(body.contains("data ceiling exceeded"), "{body}");
    // A `del` waking nothing is the way out, as ever.
    full.ok(ROOT, "del orders where total >= 0");
}

/// `statements` names what each binding is bound to, `describe statement`
/// its condition as written; `GET /_schema` lists a collection's bindings
/// beside it, and `?as=fennecql` after the collections, to the server's
/// token alone -- a scoped token is told no binding's name, nor the
/// collections a binding writes that it may not read -- and either reads
/// back as a plan of nothing.
#[test]
fn the_listings_and_the_schema_name_a_binding_to_the_server_s_token_alone() {
    let (node, _db) = start_with(GRANTED, |_| {});
    let listed = node.ok(ROOT, "statements");
    assert!(listed.contains(r#"["orders","set"]"#), "{listed}");
    assert!(listed.contains(r#"["notes","del"]"#), "{listed}");
    let paid = node.ok(ROOT, "describe statement paid");
    assert!(
        paid.contains(r#""new.status = 'paid' and old.status = 'open'""#),
        "{paid}"
    );
    let voided = node.ok(ROOT, "describe statement voided");
    assert!(voided.contains(r#""new.status = 'void'""#), "{voided}");
    let pay = node.ok(ROOT, "describe statement pay");
    assert!(
        pay.contains(r#""after":null"#) && pay.ends_with(r#""condition":null}]"#),
        "{pay}"
    );

    let a = node.request("GET", ROOT, "/_schema", &[], "");
    assert_eq!(a.status, 200, "{}", a.body);
    let described = a.body;
    assert!(
        described.contains(r#""bindings":[{"name":"paid","events":"set","text":"create statement paid after set orders"#),
        "{described}"
    );
    assert!(
        described.contains(r#"{"name":"unnoted","events":"del""#),
        "{described}"
    );
    let a = node.request("GET", ROOT, "/_schema?as=fennecql", &[], "");
    assert_eq!(a.status, 200, "{}", a.body);
    let text = a.body;
    let tally = text.find("create collection tally").unwrap();
    let bound = text.find("create statement paid after set orders").unwrap();
    assert!(tally < bound, "{text}");
    assert!(!text.contains("create statement pay("), "{text}");
    for (target, body) in [
        ("/_schema/plan?mode=follow", described.as_str()),
        ("/_schema/plan?mode=follow", text.as_str()),
    ] {
        let a = node.request("POST", ROOT, target, &[], body);
        assert_eq!(a.status, 200, "{}", a.body);
        assert!(
            a.body.contains(r#""statements":[]"#) && a.body.contains(r#""refusals":[]"#),
            "{}",
            a.body
        );
    }

    // A scoped token: no binding, no text, no name.
    let u0 = node.customer("u0");
    for target in ["/_schema", "/_schema?as=fennecql"] {
        let a = node.request("GET", &u0, target, &[], "");
        assert_eq!(a.status, 200, "{}", a.body);
        for word in ["bindings", "paid", "voided", "create statement", "tally"] {
            assert!(!a.body.contains(word), "{word} told in {}", a.body);
        }
    }
    let (status, _) = node.query(&u0, "statements");
    assert_eq!(status, 403);
    let (status, _) = node.query(&u0, "describe statement paid");
    assert_eq!(status, 404);
}

/// A `set` held on `ledger` (`Fennec-Wait`) is answered by the write that
/// made the line it waits for -- a `set orders` whose binding inserts it
/// -- through the collections the landed block noted, the binding's
/// among them.
#[test]
fn a_claim_held_on_a_derived_collection_is_answered_by_the_write_making_its_row() {
    let (node, db) = start(|_| {});
    let port = node.port;
    let held = std::thread::spawn(move || {
        let body = line("set ledger {amount: 0} where o = 3");
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "POST /query HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {ROOT}\r\n\
             Fennec-Wait: 20000\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    });
    let t = std::time::Instant::now();
    while node.server.waits().held < 1 {
        assert!(t.elapsed() < Duration::from_secs(20), "never held");
        std::thread::sleep(Duration::from_millis(1));
    }
    node.ok(ROOT, "set orders {status: 'paid'} where id = 3");
    let out = held.join().unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.ends_with(r#"{"affected":1}"#), "{out}");
    assert_eq!(ledger(&db), [line_of(3, 0)]);
}
