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

/// The migration `_migrations` records in [`start`]'s database.
const FIRST: &str = "alter collection notes add field u int";

/// A server keeping keys for `ttl`, holding `notes` and each collection of
/// [`OWN`] but `_idempotency`, which its first keyed write makes, a row in
/// each -- `_migrations` as a schema's apply makes it, recording [`FIRST`],
/// which a plan reads; `setup` run after.
fn start(ttl: Duration, setup: &[&str]) -> Node {
    start_with(POLICY, ttl, setup)
}

/// [`start`] under `policy`.
fn start_with(policy: &str, ttl: Duration, setup: &[&str]) -> Node {
    let mut sql = vec!["create collection notes (owner text, t text)".to_string()];
    for c in OWN
        .iter()
        .filter(|c| **c != fennec_core::declared::IDEMPOTENCY)
    {
        if *c == fennec_core::declared::MIGRATIONS {
            sql.push(fennec_core::declared::MIGRATIONS_DDL.into());
            sql.push(format!("put {c} {{n: 1, text: \"{FIRST}\"}}"));
            continue;
        }
        sql.push(format!("create collection {c} (key text @hash, n int)"));
        sql.push(format!("put {c} {{key: \"k\", n: 1}}"));
    }
    sql.extend(setup.iter().map(|s| s.to_string()));
    serve(policy, ttl, &sql)
}

/// A server under `policy` keeping keys for `ttl`, over a database `sql`
/// makes.
fn serve(policy: &str, ttl: Duration, sql: &[String]) -> Node {
    let mut db = Database::new();
    for s in sql {
        db.execute(&fennec_ql::parse_one(s).unwrap()).unwrap();
    }
    serve_db(policy, ttl, db)
}

/// A server under `policy` keeping keys for `ttl`, over `db`.
fn serve_db(policy: &str, ttl: Duration, db: Database) -> Node {
    fennec_http::audit::set_delay(0);
    let access = Arc::new(Access::new(SECRET, policy).unwrap());
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

/// What a scoped token sends at a collection of the server's own, or any
/// it is granted nothing on, by every route and verb -- a REST read
/// reaching it by a `lookup` level too, the level's `on`, `parent`,
/// `select`, `order` and conditions naming its fields or not: each must be
/// answered as a collection no database holds.
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
        ("GET", format!("/notes?lookup={c}"), String::new()),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=key"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=nosuch"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=key&{c}.parent=t"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=key&{c}.parent=t&{c}.select=nosuch"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=key&{c}.select=n&{c}.order=nosuch"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=key&{c}.nosuch=eq.1"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup={c}&{c}.on=key&{c}.n=eq.1&nosuch=eq.1"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup=notes,{c}&notes.on=owner&{c}.on=key"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?lookup=notes,{c}&notes.on=owner&{c}.on=nosuch"),
            String::new(),
        ),
        (
            "GET",
            format!("/{c}?lookup=notes&notes.on=t"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes?where=t+in+(get+{c}+select+key)"),
            String::new(),
        ),
        (
            "GET",
            format!("/notes/changes?where=t+in+(get+{c}+select+key)"),
            String::new(),
        ),
    ]
}

/// Each of [`attempts`] at `c` answered as at `none`, a collection no
/// database holds, byte for byte but for the name: a 404.
fn as_missing(n: &Node, token: &str, c: &str, none: &str) {
    for ((method, target, sent), (_, gone, unsent)) in attempts(c).iter().zip(&attempts(none)) {
        let a = n.call(token, None, method, target, sent);
        let b = n.call(token, None, method, gone, unsent);
        assert_eq!(a.status, 404, "{method} {target} {sent}: {}", a.body);
        assert_eq!(
            (a.status, a.body.replace(c, none)),
            (b.status, b.body),
            "{method} {target} {sent}: told apart from a missing collection"
        );
    }
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
    for c in OWN {
        as_missing(&n, &alice, c, "_none");
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

/// `_hook_events` as a webhook event taken makes it (`Database::take_event`,
/// F4's slice 3), its row the event's key, kept so that a retry of the
/// event runs nothing: an unfiltered `*` rule granting every verb reaches
/// none of it -- each read, insert, update, put and delete, by every route,
/// answered as the same statement over a collection no database holds,
/// byte for byte but for the name -- and the key holds, the event sent
/// again answered as its first taking.
#[test]
fn a_star_rule_reaches_no_webhook_events_key() {
    use fennec_core::engine::{Event, Took, HOOK_EVENTS};
    // The reader a server installs, which a hook's statement is read by.
    fennec_ql::install();
    let mut db = Database::new();
    db.allow_hooks();
    for q in [
        "create collection orders (ref text @hash, status text)",
        "put orders {ref: 'o-1', status: 'open'}",
        "create statement paid($order text from event.order) on hook stripe as set orders \
         {status: 'paid'} where ref = $order require 1",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    let body = fennec_core::json::parse_json(r#"{"id":"evt_1","order":"o-1"}"#).unwrap();
    let event = Event {
        hook: "stripe",
        key: "evt_1",
        body: &body,
        digest: 1,
        at: None,
        event: None,
        keep: 30 * 86_400_000,
    };
    assert!(matches!(db.take_event(&event).unwrap(), Took::Ran(1)));
    let n = serve_db(POLICY, Duration::from_secs(3600), db);
    assert_eq!(n.rows(HOOK_EVENTS), 1);
    let alice = n.token("alice");
    as_missing(&n, &alice, HOOK_EVENTS, "_none");
    // Nothing was read into an answer, written or taken, and the key holds.
    assert_eq!(n.rows(HOOK_EVENTS), 1);
    let again = n.db.write().unwrap().take_event(&event).unwrap();
    assert!(matches!(again, Took::Held(_)), "{again:?}");
}

/// A collection a token's rules grant nothing on is answered as one no
/// database holds by every route, an ordinary one as the server's own are:
/// on main `GET /notes?lookup=secrets` asked for `secrets.on` where
/// `lookup=nonesuch` was 404, and `secrets.on=nosuch` named the field it
/// lacked where `on=key` was the plain 404 -- its fields, one by one.
#[test]
fn a_collection_granted_nothing_on_is_missing_by_every_route() {
    let n = start_with(
        "notes  read,write  where owner = $jwt.sub\n",
        Duration::from_secs(3600),
        &[
            "create collection secrets (key text @hash, n int)",
            "put secrets {key: \"k\", n: 1}",
        ],
    );
    let alice = n.token("alice");
    as_missing(&n, &alice, "secrets", "nonesuch");
    for c in OWN {
        as_missing(&n, &alice, c, "nonesuch");
    }
    assert_eq!(n.rows("secrets"), 1);
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
            q(format!("compact {c}")),
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
    let migrates = format!(
        r#"{{"format":1,"collections":[],"migrations":["{FIRST}","create collection b (t text)","drop collection _idempotency"]}}"#
    );
    for sent in [declares, &migrates] {
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

/// Only a migration still to run is held to the rule: one the database
/// recorded ran before the server's collections were its own -- a `drop
/// collection _old` an app's list keeps, as the list only grows -- and
/// runs never again, and a database holding none of its own collections
/// records the list unrun. On the first review every plan and apply of
/// such a list was refused for good.
#[test]
fn a_migration_the_database_recorded_is_not_refused_again() {
    let n = start(
        Duration::from_secs(3600),
        &["put _migrations {n: 2, text: \"drop collection _old\"}"],
    );
    let b = r#"{"name":"b","fields":[{"name":"t","type":"text"}]}"#;
    let description = |more: &str| {
        format!(
            r#"{{"format":1,"collections":[{b}],"migrations":["{FIRST}","drop collection _old"{more}]}}"#
        )
    };
    // What the database recorded, and one migration more: it runs alone.
    let grown = description(r#","create collection b (t text)""#);
    let a = n.call(ROOT, None, "POST", "/_schema/plan", &grown);
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(a.body.contains(r#""migrations":[3]"#), "{}", a.body);
    let a = n.call(ROOT, None, "POST", "/_schema/apply", &grown);
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(n.db.read().unwrap().collection("b").is_ok());
    // A migration still to run reaching one is refused, by the plan and
    // the apply, before anything runs.
    let reaching = description(
        r#","create collection b (t text)","alter collection b add field v int","del _consumers""#,
    );
    for path in ["/_schema/plan", "/_schema/apply"] {
        let a = n.call(ROOT, None, "POST", path, &reaching);
        assert_eq!(a.status, 403, "{path}: {}", a.body);
        assert!(
            a.body.contains("`_consumers` is the server's own"),
            "{}",
            a.body
        );
    }
    let b = n.db.read().unwrap().collection("b").unwrap().schema.clone();
    assert!(b.field("v").is_none());
    assert_eq!(n.rows("_consumers"), 1);
    // A database holding none of its own collections is made from the
    // description, its migrations recorded and none run: the list holding
    // one is taken.
    let empty = serve(POLICY, Duration::from_secs(3600), &[]);
    let a = empty.call(ROOT, None, "POST", "/_schema/apply", &grown);
    assert_eq!(a.status, 200, "{}", a.body);
    let a = empty.query(ROOT, None, "get _migrations count");
    assert_eq!(a.body, "[{\"count\":3}]");
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
