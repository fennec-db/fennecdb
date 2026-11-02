//! Write-bound statements over HTTP (`docs/design/named-statements.md`,
//! F2's slice 3): the server's token defines one and every way it writes
//! wakes it -- `/query`, `/batch`, a call's body -- in the block of the
//! write; a scoped token's write or call that would wake one is refused
//! (403) before the binding runs, nothing landed, until a policy's lines
//! grant it (slice 4), while its writes waking none land as ever;
//! `--max-memory` holds a `del` by the rows its binding inserts; and no
//! statement is bound to the database's own collections or writes one.

use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const POLICY: &str = "orders  read,insert,update(status)  where by = $jwt.sub  for customer
ledger  read  for customer
pay, open  call  for customer
";
const SCHEMA: [&str; 7] = [
    "create collection orders (status text, total int, by text)",
    "create collection ledger (o int, amount int)",
    "create collection notes (n int)",
    "create statement paid after set orders where new.status = 'paid' and old.status = 'open'
     as insert ledger {o: new.id, amount: new.total}",
    "create statement unnoted after del notes as insert ledger {o: old.n, amount: 0}",
    "create statement pay($id int) public as set orders {status: 'paid'} where id = $id",
    "create statement open($n int) public as insert orders {status: 'open', total: $n, by: 'x'}",
];

struct Node {
    port: u16,
    access: Arc<Access>,
}

fn serve(db: &Arc<RwLock<Database>>, access: &Arc<Access>, tune: impl FnOnce(&mut Config)) -> u16 {
    let mut cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(access)),
        call_rate: 0,
        ..Config::default()
    };
    tune(&mut cfg);
    let server = Server::new(Arc::clone(db), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

fn line(q: &str) -> String {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    body
}

impl Node {
    fn post(&self, token: &str, target: &str, body: &str) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "POST {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        (out[9..12].parse().unwrap(), body.to_string())
    }

    fn query(&self, token: &str, q: &str) -> (u16, String) {
        self.post(token, "/query", &line(q))
    }

    fn ok(&self, token: &str, q: &str) -> String {
        let (status, body) = self.query(token, q);
        assert_eq!(status, 200, "{q}: {body}");
        body
    }

    fn customer(&self, sub: &str) -> String {
        let claims = format!(r#"{{"sub":"{sub}","role":"customer"}}"#);
        self.access.mint(&claims).unwrap()
    }
}

fn start(tune: impl FnOnce(&mut Config)) -> (Node, Arc<RwLock<Database>>) {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let db = Arc::new(RwLock::new(Database::new()));
    let node = Node {
        port: serve(&db, &access, |_| {}),
        access: Arc::clone(&access),
    };
    for s in SCHEMA {
        node.ok(ROOT, s);
    }
    for (by, total) in [("u0", 10), ("u0", 20), ("u1", 30), ("u0", 40)] {
        node.ok(
            ROOT,
            &format!("put orders {{status: 'open', total: {total}, by: '{by}'}}"),
        );
    }
    let tuned = Node {
        port: serve(&db, &access, tune),
        access,
    };
    (tuned, db)
}

fn ledger(db: &Arc<RwLock<Database>>) -> Vec<Vec<Value>> {
    let q = fennec_ql::parse_one("get ledger select o, amount order id").unwrap();
    match db.read().unwrap().query(&q, &[]).unwrap() {
        Response::Rows(rs) => rs.rows.into_iter().map(|r| r.values).collect(),
        other => panic!("{other:?}"),
    }
}

fn line_of(o: i64, amount: i64) -> Vec<Value> {
    vec![Value::Int(o), Value::Int(amount)]
}

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

#[test]
fn a_scoped_write_waking_a_binding_is_refused_until_a_grant() {
    let (node, db) = start(|_| {});
    let u0 = node.customer("u0");
    // Its write passing the condition: refused before the binding runs,
    // nothing landed, no binding named.
    let (status, body) = node.query(&u0, "set orders {status: 'paid'} where id = 1");
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("would wake a statement bound to it"),
        "{body}"
    );
    assert!(!body.contains("paid`"), "no name told: {body}");
    assert!(ledger(&db).is_empty());
    let status_of = |id: i64| node.ok(ROOT, &format!("get orders select status where id = {id}"));
    assert!(status_of(1).contains("open"), "{}", status_of(1));
    // Its writes waking none land as ever: a row the condition does not
    // pass, and a row made, which nothing is bound to.
    node.ok(&u0, "set orders {status: 'held'} where id = 2");
    node.ok(&u0, "insert orders {status: 'open', total: 5, by: 'u0'}");
    assert!(ledger(&db).is_empty());
    // A call whose body may wake one: refused before the body runs, by
    // what its text writes; one whose body wakes none runs.
    let (status, body) = node.query(&u0, "call pay(4)");
    assert_eq!(status, 403, "{body}");
    assert!(status_of(4).contains("open"), "{}", status_of(4));
    node.ok(&u0, "call open(7)");
    // The server's token's same write lands, the line with it.
    node.ok(ROOT, "set orders {status: 'paid'} where id = 1");
    assert_eq!(ledger(&db), [line_of(1, 10)]);
}

/// The database's own collections are written beside a client's write --
/// a scoped token's `Idempotency-Key` row outside `within()`, where no
/// hook refuses its wake -- so no statement is bound to one or writes one.
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
    assert!(!body.contains("keyed") && !body.contains("consumed"), "{body}");
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
