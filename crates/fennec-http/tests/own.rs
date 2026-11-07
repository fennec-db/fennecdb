//! The server's own collections -- every one whose name starts with `_`,
//! those it makes today named in `fennec_core::declared::OWN` -- are out of
//! every rule's reach: `*` grants nothing there, a rule naming one stops
//! the server, and a scoped token is answered as for a collection no
//! database holds, by every route. The server's token reads them and
//! changes none of them; the server keeps writing its own.

use fennec_core::declared::OWN;
use fennec_core::prelude::*;
use fennec_http::access::Access;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
const ROOT: &str = "root-token";
/// Every verb on every collection, unfiltered, beside a rule of its own.
const POLICY: &str = "\
notes  read,write  where owner = $jwt.sub
*      read,write
";

struct Node {
    port: u16,
    access: Arc<Access>,
    db: Arc<RwLock<Database>>,
}

/// A server keeping keys for `ttl`, holding `notes` and each collection of
/// [`OWN`] but `_idempotency`, which its first keyed write makes, as the
/// code keeping each would have made it, a row in each; `setup` run after.
fn start(ttl: Duration, setup: &[&str]) -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, POLICY).unwrap());
    let mut db = Database::new();
    let mut sql = vec!["create collection notes (owner text, t text)".to_string()];
    for c in OWN
        .iter()
        .filter(|c| **c != fennec_core::declared::IDEMPOTENCY)
    {
        sql.push(format!("create collection {c} (key text @hash, n int)"));
        sql.push(format!("put {c} {{key: \"k\", n: 1}}"));
    }
    sql.extend(setup.iter().map(|s| s.to_string()));
    for s in &sql {
        db.execute(&fennec_ql::parse_one(s).unwrap()).unwrap();
    }
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        token: Some(ROOT.into()),
        access: Some(Arc::clone(&access)),
        idempotency_ttl: ttl,
        ..Config::default()
    };
    let db = Arc::new(RwLock::new(db));
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
    fn token(&self, sub: &str) -> String {
        self.access.mint(&format!("{{\"sub\":\"{sub}\"}}")).unwrap()
    }

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
            "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n{key}\
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
        self.call(token, key, "POST", "/query", &body(sql))
    }

    /// The rows of `collection`, read in process past every token: what
    /// the server holds, whatever a refusal said.
    fn rows(&self, collection: &str) -> usize {
        let db = self.db.read().unwrap();
        db.collection(collection).map_or(0, |c| c.store.len())
    }
}

/// `/query`'s body for `sql`.
fn body(sql: &str) -> String {
    let mut out = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut out, sql);
    out.push('}');
    out
}

/// What a scoped token sends at a collection of the server's own, by every
/// route and verb: each must be answered as a collection no database holds.
fn attempts(c: &str) -> Vec<(&'static str, String, String)> {
    let q = |s: String| ("POST", "/query".to_string(), body(&s));
    vec![
        q(format!("get {c}")),
        q(format!("get {c} count")),
        q(format!("select * from {c}")),
        q(format!("describe {c}")),
        q(format!("explain get {c}")),
        q(format!("insert {c} {{key: \"x\"}}")),
        q(format!("put {c} {{key: \"x\"}}")),
        q(format!(
            "put {c} {{key: \"x\"}} if absent else set {{n: 2}}"
        )),
        q(format!("set {c} {{key: \"y\"}} where n = 1")),
        q(format!("del {c} where n = 1")),
        q(format!("get notes where t in (get {c} select key)")),
        q(format!("get notes lookup {c} on key = t")),
        ("POST", "/batch".into(), body(&format!("get {c}"))),
        (
            "POST",
            "/batch".into(),
            body(&format!("del {c} where n = 1")),
        ),
        ("GET", format!("/{c}"), String::new()),
        ("GET", format!("/{c}?count"), String::new()),
        ("GET", format!("/{c}?key=eq.k"), String::new()),
        ("GET", format!("/{c}?nosuch=eq.1"), String::new()),
        ("GET", format!("/{c}?select=nosuch"), String::new()),
        ("POST", format!("/{c}"), "{\"key\": \"x\"}".into()),
        ("PATCH", format!("/{c}/all"), "{\"key\": \"y\"}".into()),
        ("DELETE", format!("/{c}/all"), String::new()),
        ("GET", format!("/{c}/changes"), String::new()),
        ("GET", format!("/{c}/changes?select=nosuch"), String::new()),
    ]
}

/// An unfiltered `*` rule granting every verb reaches none of them: each
/// read, insert, update, put and delete, through `/query`, `/batch`, REST
/// and a subscription, is answered as the same statement over a collection
/// no database holds, and nothing is read or written. On main `get
/// _idempotency` read bob's key and kept answer, a `del` took his key so
/// his replay ran again, and an `insert` planted one.
#[test]
fn a_star_rule_reaches_none_of_the_servers_collections() {
    let n = start(Duration::from_secs(3600), &[]);
    let alice = n.token("alice");
    let bob = n.token("bob");
    // Bob's keyed write: his key and its answer kept in `_idempotency`.
    let paid = n.query(
        &bob,
        Some("pay-1"),
        "insert notes {owner: \"bob\", t: \"paid\"}",
    );
    assert_eq!((paid.status, paid.replayed), (200, false), "{}", paid.body);
    let held: Vec<(String, usize)> = OWN.iter().map(|c| (c.to_string(), n.rows(c))).collect();
    assert_eq!(n.rows(fennec_core::declared::IDEMPOTENCY), 1);
    let missing = attempts("_none");
    for c in OWN {
        for ((method, target, sent), (_, none, gone)) in attempts(c).iter().zip(&missing) {
            let a = n.call(&alice, None, method, target, sent);
            let b = n.call(&alice, None, method, none, gone);
            assert_eq!(a.status, 404, "{method} {target} {sent}: {}", a.body);
            assert_eq!(
                (a.status, a.body.replace(c, "_none")),
                (b.status, b.body),
                "{method} {target} {sent}: told apart from a missing collection"
            );
        }
    }
    // Nothing was read into an answer, written or taken.
    let after: Vec<(String, usize)> = OWN.iter().map(|c| (c.to_string(), n.rows(c))).collect();
    assert_eq!(after, held);
    // Bob's key holds: sent again, his write is answered as before and
    // not made again.
    let again = n.query(
        &bob,
        Some("pay-1"),
        "insert notes {owner: \"bob\", t: \"paid\"}",
    );
    assert_eq!(
        (again.status, again.replayed, again.body.as_str()),
        (200, true, paid.body.as_str())
    );
    assert_eq!(n.rows("notes"), 1);
}

/// No listing, schema or plan names one to a scoped token, `*` or not.
#[test]
fn the_listings_and_schemas_name_none_of_them() {
    let n = start(Duration::from_secs(3600), &[]);
    let alice = n.token("alice");
    n.query(&alice, Some("k"), "insert notes {t: \"x\"}");
    let named = |body: &str| OWN.iter().find(|c| body.contains(*c)).copied();
    for (method, target, sent) in [
        ("POST", "/query", body("collections")),
        ("GET", "/", String::new()),
        ("GET", "/collections", String::new()),
        ("GET", "/_schema", String::new()),
        ("GET", "/_schema?as=fennecql", String::new()),
        ("GET", "/_whoami", String::new()),
    ] {
        let a = n.call(&alice, None, method, target, &sent);
        assert_eq!(a.status, 200, "{method} {target}: {}", a.body);
        assert!(a.body.contains("notes"), "{target}: {}", a.body);
        assert_eq!(named(&a.body), None, "{method} {target}: {}", a.body);
    }
    // Compared as a client following the server's schema, one is not there.
    let declared = r#"{"format":1,"collections":[{"name":"_consumers","fields":[{"name":"key","type":"text","index":{"kind":"hash"}},{"name":"n","type":"int"}]}]}"#;
    let a = n.call(&alice, None, "POST", "/_schema/plan?mode=follow", declared);
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(
        a.body.contains(r#""kind":"collection_missing""#),
        "{}",
        a.body
    );
    // The server's token lists them, as an operator reads them.
    let a = n.query(ROOT, None, "collections");
    assert!(a.body.contains("_idempotency"), "{}", a.body);
}

/// A rule naming one, reading one through an inner `get`, or an
/// `append-only` line naming one stops the server as it reads the policy.
#[test]
fn a_rule_naming_or_reading_one_is_refused_at_startup() {
    for policy in [
        "_idempotency  read",
        "_consumers    read,write  where key = $jwt.sub",
        "_hook_events  insert",
        "_anything     read",
        "_follow       append-only",
        "notes  read  where t in (get _idempotency select key)",
        "notes  read  where t in (get other select k where k in (get _migrations select text))",
        "*      read  where t in (get _sync_queue select key)",
    ] {
        let why = Access::new(SECRET, policy)
            .err()
            .unwrap_or_else(|| panic!("{policy}"));
        assert!(why.contains("the server's own"), "{policy}: {why}");
        assert!(why.starts_with("policy line 1: `_"), "{policy}: {why}");
    }
    assert!(Access::new(SECRET, POLICY).is_ok());
}

/// The server's token reads them, and no statement of its makes, changes or
/// writes one, by any route -- `/query`, `/batch`, REST, a description's
/// collections or migrations -- one there or not: 403, nothing written.
#[test]
fn the_servers_token_reads_them_and_changes_none() {
    let n = start(Duration::from_secs(3600), &[]);
    n.query(ROOT, Some("k"), "insert notes {t: \"x\"}");
    for c in OWN {
        let a = n.query(ROOT, None, &format!("get {c} count"));
        assert_eq!((a.status, a.body.as_str()), (200, "[{\"count\":1}]"), "{c}");
    }
    let held: Vec<usize> = OWN.iter().map(|c| n.rows(c)).collect();
    for c in OWN.iter().chain(&["_new"]) {
        let q = |s: String| ("POST", "/query".to_string(), body(&s));
        for (method, target, sent) in [
            q(format!("create collection {c} (x int)")),
            q(format!("create collection if not exists {c} (x int)")),
            q(format!("alter collection {c} add field y int")),
            q(format!("alter collection {c} drop field n")),
            q(format!("create index on {c} (n) @sorted")),
            q(format!("create index on {c} (key, n) @sorted")),
            q(format!("drop collection {c}")),
            q(format!("drop collection if exists {c}")),
            q(format!("insert {c} {{key: \"x\"}}")),
            q(format!("put {c} {{key: \"x\"}}")),
            q(format!("set {c} {{n: 2}} where n = 1")),
            q(format!("del {c} where n = 1")),
            (
                "POST",
                "/batch".into(),
                body(&format!("del {c} where n = 1")),
            ),
            ("POST", format!("/{c}"), "{\"key\": \"x\"}".into()),
            ("PATCH", format!("/{c}/all"), "{\"n\": 2}".into()),
            ("DELETE", format!("/{c}/all"), String::new()),
        ] {
            let a = n.call(ROOT, None, method, &target, &sent);
            assert_eq!(a.status, 403, "{method} {target} {sent}: {}", a.body);
            assert!(a.body.contains("the server's own"), "{}", a.body);
        }
    }
    // A /batch holding one is refused before any line runs.
    let both = format!(
        "{}\n{}",
        body("insert notes {t: \"y\"}"),
        body("del _consumers")
    );
    assert_eq!(n.call(ROOT, None, "POST", "/batch", &both).status, 403);
    // A description declaring one, or a migration reaching one, is refused
    // as a plan and as an apply.
    let declares = r#"{"format":1,"collections":[{"name":"_idempotency","fields":[{"name":"key","type":"int"}]}]}"#;
    let migrates = r#"{"format":1,"collections":[],"migrations":["create collection b (t text)","drop collection _idempotency"]}"#;
    for sent in [declares, migrates] {
        for path in ["/_schema/plan", "/_schema/apply"] {
            let a = n.call(ROOT, None, "POST", path, sent);
            assert_eq!(a.status, 403, "{path} {sent}: {}", a.body);
        }
    }
    let after: Vec<usize> = OWN.iter().map(|c| n.rows(c)).collect();
    assert_eq!(after, held);
    assert_eq!(n.rows("notes"), 1);
    assert!(n.db.read().unwrap().collection("_new").is_err());
    assert!(n.db.read().unwrap().collection("b").is_err());
}

/// The keys go on being kept, replayed and swept: the server writes its own
/// through the engine. A collection the server's token made in its place
/// -- `key` an int, the expiry the server's -- failed every keyed write on
/// main, and a `del` of a key made its write run twice.
#[test]
fn keyed_writes_and_their_expiry_hold() {
    const TTL: Duration = Duration::from_secs(2);
    let n = start(TTL, &[]);
    let made = n.query(
        ROOT,
        None,
        "create collection _idempotency (key int @hash, at timestamp @ttl(2s))",
    );
    assert_eq!(made.status, 403, "{}", made.body);
    let bob = n.token("bob");
    let send = |who: &str, key| n.query(who, Some(key), "insert notes {owner: \"bob\", t: \"a\"}");
    let first = send(&bob, "b1");
    assert_eq!(
        (first.status, first.replayed),
        (200, false),
        "{}",
        first.body
    );
    let root = send(ROOT, "r1");
    assert_eq!((root.status, root.replayed), (200, false), "{}", root.body);
    let taken = n.query(ROOT, None, "del _idempotency");
    assert_eq!(taken.status, 403, "{}", taken.body);
    assert!(send(&bob, "b1").replayed);
    assert!(send(ROOT, "r1").replayed);
    assert_eq!(n.rows("notes"), 2);
    // Past their time, swept, and each key a new request.
    assert_eq!(
        fennec_http::sweep::pass("test", &n.db),
        0,
        "none past its time yet"
    );
    std::thread::sleep(TTL + Duration::from_millis(100));
    assert_eq!(fennec_http::sweep::pass("test", &n.db), 2);
    assert_eq!(n.rows(fennec_core::declared::IDEMPOTENCY), 0);
    let later = send(&bob, "b1");
    assert_eq!(
        (later.status, later.replayed),
        (200, false),
        "{}",
        later.body
    );
    assert_eq!(n.rows("notes"), 3);
}
