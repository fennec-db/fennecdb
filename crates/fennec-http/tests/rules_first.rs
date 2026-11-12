//! A scoped token's rules are asked of a row before any term of its own
//! statement. `and` stops at the first false, so a term that fails -- a
//! division by zero, an int past 64 bits -- never runs over a row the
//! rules hide, and the answer is the one a row the token may not read
//! gives, whatever that row holds. With the caller's `where` first, `get
//! notes where id = 1 and 1 / (case when body >= "m" then 0 else 1 end) =
//! 1` was answered 400 or 200 `[]` by bob's note's body, and a binary
//! search read any field of any row out of the two. An upsert's `else set`
//! read the row holding its value before that row was judged, 400 against
//! 403 the same way. And a scoped `explain` is refused: its plan counted
//! the rows an index chose before the rules held them, and which index it
//! named told whether a row held a value.

use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
const POLICY: &str = "\
notes  read,write  where owner = $jwt.sub
board  read
";

struct Node {
    port: u16,
    access: Arc<Access>,
    db: Arc<RwLock<Database>>,
}

/// Bob's note is id 1, its body `"middle"`; alice's is id 2.
fn start() -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let mut db = Database::new();
    for sql in [
        "create collection board (msg text)",
        "put board {msg: \"hello\"}",
        "create collection notes (owner text @hash, body text, board int @hash, n int, \
         email text @unique)",
        "put notes {owner: \"bob\", body: \"middle\", board: 1, n: 5, email: \"bob@x\"}",
        "put notes {owner: \"alice\", body: \"mine\", board: 1, n: 1, email: \"alice@x\"}",
    ] {
        db.execute(&fennec_ql::parse_one(sql).unwrap())
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    let db = Arc::new(RwLock::new(db));
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        ..Config::default()
    };
    let server = Server::new(Arc::clone(&db), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { port, access, db }
}

impl Node {
    fn token(&self, claims: &str) -> String {
        self.access.mint(claims).unwrap()
    }

    fn call(&self, token: &str, method: &str, target: &str, body: &str) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let body = out
            .split_once("\r\n\r\n")
            .map_or("", |(_, b)| b)
            .to_string();
        (out[9..12].parse().unwrap(), body)
    }

    fn post(&self, token: &str, path: &str, sql: &str) -> (u16, String) {
        let mut body = String::from("{\"query\":");
        fennec_core::json::escape_into(&mut body, sql);
        body.push('}');
        self.call(token, "POST", path, &body)
    }

    fn query(&self, token: &str, sql: &str) -> (u16, String) {
        self.post(token, "/query", sql)
    }

    /// Bob's note as the server's token reads it.
    fn bobs(&self) -> String {
        self.query(ROOT, "get notes select body, n where id = 1").1
    }
}

/// `sql` with `PIVOT` read as `"m"`, which bob's `"middle"` is past, and
/// as `"z"`, which it is not, answered alike: one answer whatever the row
/// the token may not read holds. Returned for the caller to hold to what a
/// row that is not there gives.
fn alike(n: &Node, token: &str, sql: &str) -> (u16, String) {
    let past = n.query(token, &sql.replace("PIVOT", "\"m\""));
    let short = n.query(token, &sql.replace("PIVOT", "\"z\""));
    assert_eq!(past, short, "{sql}: told bob's body apart");
    past
}

#[test]
fn a_failing_term_over_a_row_the_rules_hide_answers_as_no_row() {
    let n = start();
    let alice = n.token(r#"{"sub":"alice"}"#);
    let divides = "1 / (case when body >= PIVOT then 0 else 1 end) = 1";
    let before = n.bobs();
    // A `get`, found by the id index or a scan, and an int past 64 bits.
    for sql in [
        format!("get notes where id = 1 and {divides}"),
        format!("get notes where owner = \"bob\" and {divides}"),
        "get notes where id = 1 and \
         9223372036854775807 + (case when body >= PIVOT then 1 else 0 end) > 0"
            .to_string(),
        format!("get notes where id = 1 and {divides} count"),
        // A `lookup` level, held to its own rules.
        format!("get board where id = 1 lookup notes on board required where id = 1 and {divides}"),
        // An inner `get`, answered before the query.
        format!("get board where id in (get notes select board where id = 1 and {divides})"),
    ] {
        let (status, body) = alike(&n, &alice, &sql);
        assert_eq!(status, 200, "{sql}: {body}");
        let want = if sql.ends_with("count") {
            r#"[{"count":0}]"#
        } else {
            "[]"
        };
        assert_eq!(body.trim(), want, "{sql}");
    }
    // A `set` and a `del`, picked, answering their rows, or neither.
    for sql in [
        format!("set notes {{n: 0}} where id = 1 and {divides}"),
        format!("set notes {{n: 0}} where id = 1 and {divides} order n limit 1 returning *"),
        format!("del notes where id = 1 and {divides}"),
        format!("del notes where owner = \"bob\" and {divides} returning id"),
    ] {
        let (status, body) = alike(&n, &alice, &sql);
        assert_eq!(status, 200, "{sql}: {body}");
        assert!(
            body.contains("\"affected\":0") || body.trim() == "[]",
            "{sql}: {body}"
        );
    }
    assert_eq!(n.bobs(), before, "nothing of bob's was written");
}

/// An upsert's `else set` reads the row holding its value, which may be
/// anyone's: that row is judged by the token's update rules first, so a
/// row it may not update is refused before an expression reads it.
#[test]
fn an_upserts_else_set_reads_no_row_the_token_may_not_update() {
    let n = start();
    let alice = n.token(r#"{"sub":"alice"}"#);
    let before = n.bobs();
    let sql = "put notes {email: \"bob@x\"} if absent \
               else set {n: 1 / (case when body >= PIVOT then 0 else 1 end)}";
    let (status, body) = alike(&n, &alice, sql);
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("outside what this token may update"),
        "{body}"
    );
    assert_eq!(n.bobs(), before);
    // Her own row, which she may update, is set as before.
    let (status, body) = n.query(
        &alice,
        "put notes {email: \"alice@x\"} if absent else set {n: n + 1}",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        n.query(ROOT, "get notes select n where id = 2").1.trim(),
        r#"[{"n":2}]"#
    );
}

/// REST's `?where=` and a subscription's shape are held to the same
/// order: the rules, then the caller's terms.
#[test]
fn rest_and_a_subscription_ask_the_rules_first() {
    let n = start();
    let alice = n.token(r#"{"sub":"alice"}"#);
    let shape = |pivot: &str| {
        format!("id+%3D+1+and+1+%2F+(case+when+body+%3E%3D+%22{pivot}%22+then+0+else+1+end)+%3D+1")
    };
    for pivot in ["m", "z"] {
        let (status, body) = n.call(&alice, "GET", &format!("/notes?where={}", shape(pivot)), "");
        assert_eq!((status, body.trim()), (200, "[]"), "{pivot}");
    }
    for pivot in ["m", "z"] {
        let mut s = TcpStream::connect(("127.0.0.1", n.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        write!(
            s,
            "GET /notes/changes?where={} HTTP/1.1\r\nHost: x\r\n\
             Authorization: Bearer {alice}\r\n\r\n",
            shape(pivot)
        )
        .unwrap();
        let mut heard = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline
            && !heard.contains("event: seed")
            && !heard.contains("\"error\"")
        {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(k) => heard.push_str(&String::from_utf8_lossy(&buf[..k])),
                Err(_) => {}
            }
        }
        assert!(heard.starts_with("HTTP/1.1 200"), "{pivot}: {heard}");
        assert!(
            heard.contains("event: seed") && !heard.contains("\"error\""),
            "{pivot}: {heard}"
        );
        assert!(!heard.contains("middle"), "{heard}");
    }
}

/// A plan counts the rows an index chose before the rules held them, and
/// which index it names is a comparison of their counts: refused to a
/// scoped token on every route, the server's token's as before.
#[test]
fn a_scoped_explain_is_refused() {
    let n = start();
    let alice = n.token(r#"{"sub":"alice"}"#);
    for sql in [
        "explain get notes",
        "explain get notes where email = \"bob@x\"",
        "explain get board",
    ] {
        let (status, body) = n.query(&alice, sql);
        assert_eq!(status, 403, "{sql}: {body}");
        assert!(body.contains("`explain` is the server token's"), "{body}");
        // A `/batch`, a line a statement.
        let line = |q: &str| {
            let mut l = String::from("{\"query\":");
            fennec_core::json::escape_into(&mut l, q);
            l.push('}');
            l
        };
        let lines = format!("{}\n{}", line("get board"), line(sql));
        let (status, body) = n.call(&alice, "POST", "/batch", &lines);
        assert_eq!(status, 403, "{sql}: {body}");
        assert!(body.contains("`explain` is the server token's"), "{body}");
    }
    // As for a `get`, a collection it is granted nothing on is not there.
    let (status, body) = n.query(&alice, "explain get nosuch");
    assert_eq!(status, 404, "{body}");
    let (status, plan) = n.query(ROOT, "explain get notes where email = \"bob@x\"");
    assert_eq!(status, 200, "{plan}");
    assert!(plan.contains("filter:"), "{plan}");
}

/// What the rewrite plans is still the engine's to say: the statement as
/// the scope rewrites it, explained in process, takes the rule's index
/// where the rule's set is the smaller, and the caller's where its own is.
#[test]
fn a_scoped_statement_plans_by_the_smaller_set_its_rules_leave() {
    let n = start();
    let plan = |claims: &str, sql: &str| {
        let token = n.token(claims);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let scope = n.access.scope(&token, now).unwrap();
        let db = n.db.read().unwrap();
        let Statement::Select(sel) = scope.rewrite(fennec_ql::parse_one(sql).unwrap()).unwrap()
        else {
            panic!("{sql}: not a read")
        };
        let rows = db.query(&Statement::Explain(sel), &[]).unwrap();
        format!("{:?}", rows.rows().unwrap().rows)
    };
    {
        let mut db = n.db.write().unwrap();
        for i in 0..20 {
            let put = format!("put notes {{owner: \"carol\", body: \"c{i}\", board: 2}}");
            db.execute(&fennec_ql::parse_one(&put).unwrap()).unwrap();
        }
    }
    // Carol's 20 rows against the one on board 1 she holds none of.
    let p = plan(r#"{"sub":"carol"}"#, "get notes where board = 1");
    assert!(p.contains("the hash index on board, 2 rows"), "{p}");
    // Alice's one row against board 2's 20.
    let p = plan(r#"{"sub":"alice"}"#, "get notes where board = 2");
    assert!(p.contains("the hash index on owner, 1 rows"), "{p}");
}
