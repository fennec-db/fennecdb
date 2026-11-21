//! `Idempotency-Key`: a write sent again is answered as the first time and
//! not made again, on every write path, per user, and only for its request.

use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";

struct Node {
    port: u16,
    access: Arc<Access>,
    db: Arc<RwLock<Database>>,
}

fn start(ttl: Duration) -> Node {
    start_over(ttl, &[])
}

fn start_over(ttl: Duration, setup: &[&str]) -> Node {
    let mut db = Database::new();
    for sql in [
        "create collection notes (owner text, t text)",
        "create collection logs (t text)",
    ]
    .iter()
    .chain(setup)
    {
        db.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
    }
    serve_over(Arc::new(RwLock::new(db)), ttl)
}

/// A server over `db`, keeping keys for `ttl`.
fn serve_over(db: Arc<RwLock<Database>>, ttl: Duration) -> Node {
    let access =
        Arc::new(Access::new(SECRET, "notes  read,write  where owner = $jwt.sub\n").unwrap());
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        idempotency_ttl: ttl,
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

struct Answer {
    status: u16,
    replayed: bool,
    body: String,
}

impl Node {
    fn call(
        &self,
        token: &str,
        key: Option<&str>,
        method: &str,
        target: &str,
        body: &str,
    ) -> Answer {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let key = key.map_or(String::new(), |k| format!("Idempotency-Key: {k}\r\n"));
        write!(
            s,
            "{method} {target} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n{key}\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        Answer {
            status: head[9..12].parse().unwrap(),
            replayed: head.contains("Idempotent-Replayed: true"),
            body: body.to_string(),
        }
    }

    fn query(&self, token: &str, key: Option<&str>, sql: &str) -> Answer {
        let mut body = String::from("{\"query\":");
        fennec_core::json::escape_into(&mut body, sql);
        body.push('}');
        self.call(token, key, "POST", "/query", &body)
    }

    fn count(&self, collection: &str) -> String {
        self.query(ROOT, None, &format!("get {collection} count"))
            .body
    }
}

#[test]
fn a_write_sent_again_is_answered_as_before_and_not_made_again() {
    let n = start(Duration::from_secs(3600));
    // /query: a put with no id makes a row each time it runs.
    let first = n.query(ROOT, Some("k1"), "put logs {t: \"once\"}");
    assert_eq!(
        (first.status, first.replayed),
        (200, false),
        "{}",
        first.body
    );
    let again = n.query(ROOT, Some("k1"), "put logs {t: \"once\"}");
    assert_eq!(
        (again.status, again.replayed, again.body.as_str()),
        (200, true, first.body.as_str())
    );
    assert_eq!(n.count("logs"), "[{\"count\":1}]");
    // Another key is another write.
    n.query(ROOT, Some("k2"), "put logs {t: \"once\"}");
    assert_eq!(n.count("logs"), "[{\"count\":2}]");
    // REST and /batch as well.
    for _ in 0..2 {
        assert_eq!(
            n.call(ROOT, Some("k3"), "POST", "/logs", "{\"t\": \"rest\"}")
                .status,
            201
        );
    }
    assert_eq!(n.count("logs"), "[{\"count\":3}]");
    let batch =
        "{\"query\":\"put logs {t: \\\"b1\\\"}\"}\n{\"query\":\"put logs {t: \\\"b2\\\"}\"}";
    let b1 = n.call(ROOT, Some("k4"), "POST", "/batch", batch);
    let b2 = n.call(ROOT, Some("k4"), "POST", "/batch", batch);
    assert_eq!(
        (b1.status, b2.body == b1.body, b2.replayed),
        (200, true, true),
        "{}",
        b1.body
    );
    assert_eq!(n.count("logs"), "[{\"count\":5}]");
}

#[test]
fn a_key_is_for_one_request_one_user_and_a_write_that_landed() {
    let n = start(Duration::from_secs(3600));
    n.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    // The key with another request: refused, nothing made.
    assert_eq!(n.query(ROOT, Some("k"), "put logs {t: \"b\"}").status, 422);
    assert_eq!(n.count("logs"), "[{\"count\":1}]");
    // A failed write keeps no key: sent again, it runs again.
    assert_eq!(
        n.query(ROOT, Some("bad"), "put nowhere {t: \"x\"}").status,
        404
    );
    n.query(ROOT, None, "create collection nowhere (t text)");
    assert_eq!(
        n.query(ROOT, Some("bad"), "put nowhere {t: \"x\"}").status,
        200
    );
    // Two users' keys never meet: the same key is each one's own.
    let alice = n.access.mint("{\"sub\":\"alice\"}").unwrap();
    let bob = n.access.mint("{\"sub\":\"bob\"}").unwrap();
    let a = n.query(
        &alice,
        Some("same"),
        "put notes {owner: \"alice\", t: \"hers\"}",
    );
    let b = n.query(&bob, Some("same"), "put notes {owner: \"bob\", t: \"his\"}");
    assert_eq!(
        (a.status, b.status, b.replayed),
        (200, 200, false),
        "{} {}",
        a.body,
        b.body
    );
    assert_eq!(n.count("notes"), "[{\"count\":2}]");
    // A scoped token with no subject has no one to keep a key for.
    let nobody = n.access.mint("{\"role\":\"x\"}").unwrap();
    let r = n.query(&nobody, Some("k"), "put notes {owner: \"x\", t: \"y\"}");
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(!r.body.contains("must be a string"), "{}", r.body);
    // Nor one whose `sub` is a number, which RFC 7519 makes a string: the
    // refusal names the claim, and says so.
    for sub in ["42", "[\"alice\"]"] {
        let numbered = n.access.mint(&format!("{{\"sub\":{sub}}}")).unwrap();
        let r = n.query(&numbered, Some("k"), "put notes {owner: \"x\", t: \"y\"}");
        assert_eq!(r.status, 400, "{}", r.body);
        assert!(
            r.body.contains("`sub` claim must be a string"),
            "{sub}: {}",
            r.body
        );
    }
    assert_eq!(n.count("notes"), "[{\"count\":2}]");
    // Keys are the server's own: a scoped user reads none of them, and is
    // not told they are there.
    let peek = n.query(&alice, None, "get _idempotency").status;
    assert!(matches!(peek, 403 | 404), "{peek}");
}

/// A key is its caller's, and a caller is everything of its token that
/// decides what it may do: its issuer, its subject, the claims its rules
/// read. Found by `sub` alone, a key's answer was replayed to another
/// issuer's user of the same `sub`, and to the same subject's token of
/// another team, its `returning` rows those of a team that token cannot
/// read. Each is another caller, and the key its own: the write runs, and
/// answers what it may.
#[test]
fn a_key_is_replayed_only_to_a_caller_of_the_same_authority() {
    let access =
        Arc::new(Access::new(SECRET, "teams  read,write  where team = $jwt.team\n").unwrap());
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        ..Config::default()
    };
    let db = Arc::new(RwLock::new(Database::new()));
    let server = Server::new(Arc::clone(&db), cfg);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let n = Node { port, access, db };
    n.query(ROOT, None, "create collection teams (team text, t text)");
    n.query(ROOT, None, "put teams {team: \"red\", t: \"red's plan\"}");
    n.query(ROOT, None, "put teams {team: \"blue\", t: \"blue's plan\"}");
    let set = "set teams {t: t} where t != \"\" returning t";
    let red = n.access.mint(r#"{"sub":"vera","team":"red"}"#).unwrap();
    let first = n.query(&red, Some("k"), set);
    assert_eq!(first.status, 200, "{}", first.body);
    assert!(first.body.contains("red's plan"), "{}", first.body);
    let again = n.query(&red, Some("k"), set);
    assert!(
        again.replayed,
        "the same caller is replayed: {}",
        again.body
    );
    for (who, claims) in [
        ("another team", r#"{"sub":"vera","team":"blue"}"#),
        (
            "another issuer",
            r#"{"sub":"vera","team":"red","iss":"https://other.example/"}"#,
        ),
        ("a role", r#"{"sub":"vera","team":"red","role":"intern"}"#),
    ] {
        let token = n.access.mint(claims).unwrap();
        let a = n.query(&token, Some("k"), set);
        assert_eq!(a.status, 200, "{who}: {}", a.body);
        assert!(
            !a.replayed,
            "{who} was replayed the first caller's answer: {}",
            a.body
        );
        if who == "another team" {
            assert!(
                !a.body.contains("red's plan") && a.body.contains("blue's plan"),
                "{who}: {}",
                a.body
            );
        }
    }
}

/// A key was kept as its owner, `U+001F` and the key, so a key holding that
/// character, beside a subject holding it, named another subject's: `sub`
/// "a\u{1f}b" with key "c" was `sub` "a" with key "b\u{1f}c", which was
/// replayed the other's answer. A key holds no control character; its
/// caller is of one width now, and the two run apart either way.
#[test]
fn a_key_holding_a_control_character_is_refused() {
    let n = start(Duration::from_secs(3600));
    let joined = n.access.mint("{\"sub\":\"a\\u001fb\"}").unwrap();
    let a = n.access.mint("{\"sub\":\"a\"}").unwrap();
    let put = "put notes {t: \"mine\"}";
    let first = n.query(&joined, Some("c"), put);
    assert_eq!(first.status, 200, "{}", first.body);
    let other = n.query(&a, Some("b\u{1f}c"), put);
    assert_eq!(
        (other.status, other.replayed),
        (400, false),
        "{}",
        other.body
    );
    assert!(other.body.contains("control"), "{}", other.body);
    for key in ["tab\there", "nul\u{0}", "del\u{7f}"] {
        assert_eq!(
            n.query(ROOT, Some(key), "put logs {t: \"x\"}").status,
            400,
            "{key:?}"
        );
    }
    assert_eq!(n.count("logs"), "[{\"count\":0}]");
}

#[test]
fn a_key_past_its_time_is_a_new_request() {
    let n = start(Duration::from_millis(200));
    n.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    std::thread::sleep(Duration::from_millis(400));
    let again = n.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    assert_eq!((again.status, again.replayed), (200, false));
    assert_eq!(n.count("logs"), "[{\"count\":2}]");
}

/// The keys are `@ttl`: one past its time is out of every read at once and
/// the sweeper deletes it, where the write path once purged them itself.
///
/// "None past its time yet" holds only while the pass comes within the
/// keys' time of their writes, which at 200 ms bounded two requests and a
/// read by little more than the 161 ms one answer took in #264's container
/// of three cores. Two seconds leaves a stall its room.
#[test]
fn the_sweeper_deletes_the_keys_past_their_time() {
    const TTL: Duration = Duration::from_secs(2);
    let n = start(TTL);
    n.query(ROOT, Some("k1"), "put logs {t: \"a\"}");
    n.query(ROOT, Some("k2"), "put logs {t: \"b\"}");
    let keys = |n: &Node| {
        let mut db = n.db.write().unwrap();
        // Every row the file holds, past its time or not.
        db.set_clock(Some(0));
        let r = db.query(
            &fennec_ql::parse_one("get _idempotency count").unwrap(),
            &[],
        );
        db.set_clock(None);
        r.unwrap().rows().unwrap().rows[0].values[0].clone()
    };
    assert_eq!(keys(&n), Value::Int(2));
    assert_eq!(
        fennec_http::sweep::pass("test", &n.db),
        0,
        "none past its time yet"
    );
    std::thread::sleep(TTL + Duration::from_millis(100));
    assert_eq!(fennec_http::sweep::pass("test", &n.db), 2);
    assert_eq!(keys(&n), Value::Int(0));
    let schema =
        n.db.read()
            .unwrap()
            .collection("_idempotency")
            .unwrap()
            .schema
            .clone();
    assert_eq!(schema.field("at").unwrap().index.ttl(), Some(2000));
}

/// Keys kept before the expiry -- `at int @sorted`, purged by the write
/// path -- are made again as the expiry keeps them, and a server started
/// with another `--idempotency-ttl` moves it.
#[test]
fn the_keys_collection_follows_the_ttl_it_is_started_with() {
    let n = start_over(
        Duration::from_secs(60),
        &[
            "create collection _idempotency (key text @hash, request int, status int, \
             body text, at int @sorted)",
            "put _idempotency {key: \"x\", request: 1, status: 200, body: \"{}\", at: 1}",
        ],
    );
    let ttl = |n: &Node| {
        let db = n.db.read().unwrap();
        let f = db.collection("_idempotency").unwrap().schema.clone();
        let at = f.field("at").unwrap();
        (at.ty.clone(), at.index.ttl())
    };
    n.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    assert_eq!(ttl(&n), (DataType::Timestamp, Some(60_000)));
    let again = n.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    assert!(again.replayed);
    let db = Arc::clone(&n.db);
    let moved = Node {
        port: {
            let cfg = Config {
                addr: "127.0.0.1:0".into(),
                token: Some(ROOT.into()),
                idempotency_ttl: Duration::from_secs(3600),
                ..Config::default()
            };
            let server = Server::new(Arc::clone(&db), cfg);
            let listener = server.bind().unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let _ = server.serve_on(listener);
            });
            port
        },
        access: Arc::clone(&n.access),
        db,
    };
    let kept = moved.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    assert!(kept.replayed, "a key kept is kept across the move");
    moved.query(ROOT, Some("k2"), "put logs {t: \"b\"}");
    assert_eq!(ttl(&moved), (DataType::Timestamp, Some(3_600_000)));
}

/// A server started again with a longer `--idempotency-ttl` brings back,
/// at its first keyed write, the keys past the old expiry the sweeper has
/// not reached -- and the look that write made, before the expiry moved,
/// found none of them. Its key brought back so, the write writes over the
/// row: one row for the key, its retry handed the new answer. Where the
/// look's finding no row was the end of it, the key was two live rows,
/// the older answering its retries: its own answer, or a 422 for the write
/// that landed.
#[test]
fn a_key_a_longer_ttl_brings_back_is_written_over() {
    let dir = std::env::temp_dir().join(format!("fennec-http-idempotency-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("longer.fennec");
    let _ = std::fs::remove_file(&path);
    let mut db = fennec_core::fs::open(&path).unwrap();
    db.execute(&fennec_ql::parse_one("create collection logs (t text)").unwrap())
        .unwrap();
    let db = Arc::new(RwLock::new(db));
    let minute = serve_over(Arc::clone(&db), Duration::from_secs(60));
    let first = minute.query(ROOT, Some("k"), "put logs {t: \"a\"}");
    assert_eq!(
        (first.status, first.replayed),
        (200, false),
        "{}",
        first.body
    );

    // The file opened again two minutes on, and served keeping keys for an
    // hour: the key is past the minute it was kept for, and not swept.
    {
        let mut g = db.write().unwrap();
        drop(std::mem::replace(&mut *g, Database::new()));
        *g = fennec_core::fs::open(&path).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        g.set_clock(Some(now + 120_000));
    }
    let hour = serve_over(Arc::clone(&db), Duration::from_secs(3600));
    let again = hour.query(ROOT, Some("k"), "put logs {t: \"b\"}");
    assert_eq!(
        (again.status, again.replayed),
        (200, false),
        "a key past its time is a new request: {}",
        again.body
    );
    let retried = hour.query(ROOT, Some("k"), "put logs {t: \"b\"}");
    assert_eq!(
        (retried.status, retried.replayed, retried.body.as_str()),
        (200, true, again.body.as_str()),
        "the retry was not handed the write's answer"
    );
    let keys = db
        .read()
        .unwrap()
        .query(
            &fennec_ql::parse_one("get _idempotency count").unwrap(),
            &[],
        )
        .unwrap()
        .rows()
        .unwrap()
        .rows[0]
        .values[0]
        .clone();
    assert_eq!(keys, Value::Int(1), "the key is more than one live row");
    assert_eq!(hour.count("logs"), "[{\"count\":2}]");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Keys kept as text -- the owner, `U+001F` and the key, from before a
/// caller was the bytes of its digest -- are made again as bytes, the
/// expiry the server keeps or not: no request names one of them now. The
/// server token's key and a caller's of the same text never meet.
#[test]
fn keys_kept_as_text_are_made_again_as_bytes() {
    let n = start_over(
        Duration::from_secs(60),
        &[
            "create collection _idempotency (key text @hash, request int, status int, \
             body text, at timestamp @ttl(1m))",
            "put _idempotency {key: \"x\", request: 1, status: 200, body: \"{}\", at: 1}",
        ],
    );
    let ty = |n: &Node| {
        let db = n.db.read().unwrap();
        let c = db.collection("_idempotency").unwrap();
        c.schema.field("key").unwrap().ty.clone()
    };
    let put = "put notes {owner: \"alice\", t: \"x\"}";
    let first = n.query(ROOT, Some("k"), put);
    assert_eq!(
        (first.status, first.replayed),
        (200, false),
        "{}",
        first.body
    );
    assert_eq!(ty(&n), DataType::Bytes);
    assert!(n.query(ROOT, Some("k"), put).replayed);
    let alice = n.access.mint("{\"sub\":\"alice\"}").unwrap();
    let hers = n.query(&alice, Some("k"), put);
    assert_eq!(
        (hers.status, hers.replayed),
        (200, false),
        "the server's key is no caller's: {}",
        hers.body
    );
    assert!(n.query(&alice, Some("k"), put).replayed);
    assert_eq!(n.count("notes"), "[{\"count\":2}]");
}
