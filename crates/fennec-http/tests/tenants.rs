//! Tenant mode: one file per tenant under `/t/<tenant>/`, and `/_admin/`.
//!
//! The server runs in-process over a scratch directory; the client is raw
//! TCP, as in the other suites.

use fennec_http::access::Access;
use fennec_http::tenants::Tenants;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ADMIN: &str = "Bearer adm";

struct Node {
    port: u16,
    tenants: Arc<Tenants>,
    dir: PathBuf,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fennec-tenants-{tag}-{}-{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(tag: &str, cfg: Config) -> Node {
    start_with(tag, cfg, |t| t)
}

fn start_with(tag: &str, mut cfg: Config, build: impl FnOnce(Tenants) -> Tenants) -> Node {
    // The wait after a refusal is the process's, counted by address and
    // doubled to 5 s: every test here asks from 127.0.0.1, so the refusals
    // of the tests running at once held one's 401 past its read timeout.
    // tests/audit.rs and fennec-shard's tests/refusals.rs hold the wait.
    fennec_http::audit::set_delay(0);
    let dir = scratch(tag);
    let tenants = Arc::new(build(Tenants::new(&dir).expect("dir")));
    cfg.addr = "127.0.0.1:0".into();
    cfg.stream_keepalive = Duration::from_millis(80);
    let server = Server::with_tenants(Arc::clone(&tenants), cfg);
    let listener = server.bind().expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { port, tenants, dir }
}

fn admin_cfg() -> Config {
    Config {
        admin_token: Some("adm".into()),
        ..Config::default()
    }
}

struct Res {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn call(port: u16, method: &str, target: &str, body: &[u8], auth: Option<&str>) -> Res {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(a) = auth {
        req.push_str(&format!("Authorization: {a}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&out[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Res {
        status,
        head,
        body: out[split + 4..].to_vec(),
    }
}

fn query(port: u16, tenant: &str, q: &str) -> Res {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    call(
        port,
        "POST",
        &format!("/t/{tenant}/query"),
        body.as_bytes(),
        None,
    )
}

fn create(port: u16, tenant: &str) {
    let r = call(
        port,
        "PUT",
        &format!("/_admin/tenants/{tenant}"),
        b"",
        Some(ADMIN),
    );
    assert_eq!(r.status, 201, "{}", r.text());
}

#[test]
fn tenants_do_not_see_each_other() {
    let n = start("isolation", admin_cfg());
    create(n.port, "acme");
    create(n.port, "beta");
    for t in ["acme", "beta"] {
        let r = query(n.port, t, "create collection notes (title text)");
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let r = call(
        n.port,
        "POST",
        "/t/acme/notes",
        br#"{"title":"acme only"}"#,
        None,
    );
    assert_eq!(r.status, 201, "{}", r.text());

    let acme = call(n.port, "GET", "/t/acme/notes", b"", None);
    assert_eq!(acme.text(), r#"[{"id":1,"title":"acme only"}]"#);
    let beta = call(n.port, "GET", "/t/beta/notes", b"", None);
    assert_eq!(beta.text(), "[]");

    // Empty segments do not shift what the prefix is taken to be.
    let again = call(n.port, "GET", "//t//acme//notes", b"", None);
    assert_eq!(again.text(), acme.text());

    // Ids are counted per file: beta's first document is 1 as well.
    call(
        n.port,
        "POST",
        "/t/beta/notes",
        br#"{"title":"beta"}"#,
        None,
    );
    let beta = call(n.port, "GET", "/t/beta/notes", b"", None);
    assert_eq!(beta.text(), r#"[{"id":1,"title":"beta"}]"#);
}

/// The parse cache and the hints are the process's, every tenant's: a
/// text's hint one tenant's request left changes no answer another's
/// gets. `set rh {xs: $1} where v = $1` reads its list where the hint
/// asks, as a list whose `f32`s are the integers written the vector the
/// quick read makes of it, and a vector compares with a list of numbers by
/// its `f32`s: 1 row each, whichever tenant asked first and with what.
#[test]
fn a_hint_another_tenant_left_changes_no_answer() {
    let n = start("hints", admin_cfg());
    for t in ["acme", "beta"] {
        create(n.port, t);
        for q in [
            "create collection rh (xs [float], v vector<2>)",
            "put rh [{id: 1, v: [1, 2]}, {id: 2, v: [0.5, 2]}, {id: 3, v: [0.1, 2]}]",
        ] {
            let r = query(n.port, t, q);
            assert_eq!(r.status, 200, "{}", r.text());
        }
    }
    let set = |tenant: &str, p: &str| {
        let body =
            format!(r#"{{"query":"set rh {{xs: $1}} where v = $1 and id > 0","params":[{p}]}}"#);
        let r = call(
            n.port,
            "POST",
            &format!("/t/{tenant}/query"),
            body.as_bytes(),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.text());
        r.text()
    };
    for (tenant, p) in [
        ("acme", "[0.1, 2]"),
        ("beta", "[1, 2]"),
        ("acme", "[1, 2]"),
        ("beta", "[0.5, 2]"),
        ("beta", "[0.1, 2]"),
        ("acme", "[0.5, 2]"),
    ] {
        let r = set(tenant, p);
        assert!(r.contains("\"affected\":1"), "{tenant} {p}: {r}");
    }
    for t in ["acme", "beta"] {
        let r = query(n.port, t, "get rh select xs order id");
        assert_eq!(
            r.text(),
            r#"[{"xs":[1,2]},{"xs":[0.5,2]},{"xs":[0.1,2]}]"#,
            "{t}"
        );
    }
}

#[test]
fn the_metrics_are_the_nodes() {
    let n = start("metrics", admin_cfg());
    create(n.port, "acme");
    create(n.port, "beta");
    let r = query(n.port, "acme", "create collection notes (title text)");
    assert_eq!(r.status, 200, "{}", r.text());
    // The node has an admin token, so a scrape needs it.
    assert_eq!(call(n.port, "GET", "/_metrics", b"", None).status, 401);
    let r = call(n.port, "GET", "/_metrics", b"", Some(ADMIN));
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.head.contains("text/plain; version=0.0.4"), "{}", r.head);
    let text = r.text();
    assert!(text.contains("\nfennec_tenants 2\n"), "{text}");
    // Creating a tenant opens it.
    assert!(text.contains("\nfennec_tenants_open 2\n"), "{text}");
    // Per tenant it counts, not what a tenant holds: its collections are
    // not the node's to publish.
    assert!(!text.contains("notes"), "{text}");
}

#[test]
fn unknown_and_invalid_tenants_are_refused() {
    let n = start("refusals", admin_cfg());
    assert_eq!(call(n.port, "GET", "/t/nope/x", b"", None).status, 404);
    assert_eq!(call(n.port, "GET", "/t/Bad/x", b"", None).status, 400);
    assert_eq!(call(n.port, "GET", "/t/../x", b"", None).status, 400);
    assert_eq!(call(n.port, "GET", "/notes", b"", None).status, 404);
    // A request never brings a tenant into being.
    assert!(!n.dir.join("nope.fennec").exists());
}

#[test]
fn the_data_token_is_checked_before_the_tenant_is_looked_up() {
    let cfg = Config {
        token: Some("data".into()),
        ..admin_cfg()
    };
    let n = start("token", cfg);
    create(n.port, "acme");
    // Same status for a tenant that exists and one that does not: the
    // answer must not tell an unauthenticated caller which names are taken.
    assert_eq!(call(n.port, "GET", "/t/acme/", b"", None).status, 401);
    assert_eq!(call(n.port, "GET", "/t/nope/", b"", None).status, 401);
    assert_eq!(
        call(n.port, "GET", "/t/acme/", b"", Some("Bearer data")).status,
        200
    );
    // The data token is not an admin token.
    let r = call(n.port, "PUT", "/_admin/tenants/x", b"", Some("Bearer data"));
    assert_eq!(r.status, 401);
}

#[test]
fn admin_is_off_without_its_token() {
    let n = start("no-admin", Config::default());
    let r = call(n.port, "PUT", "/_admin/tenants/acme", b"", Some(ADMIN));
    assert_eq!(r.status, 404);
    assert!(n.tenants.names().is_empty());
}

#[test]
fn a_frozen_tenant_refuses_writes_with_retry_after_and_keeps_reading() {
    let n = start("freeze", admin_cfg());
    create(n.port, "acme");
    query(n.port, "acme", "create collection notes (title text)");
    call(n.port, "POST", "/t/acme/notes", br#"{"title":"a"}"#, None);

    let r = call(
        n.port,
        "POST",
        "/_admin/tenants/acme/freeze",
        b"",
        Some(ADMIN),
    );
    assert_eq!(r.status, 200, "{}", r.text());

    for (method, target, body) in [
        ("POST", "/t/acme/notes", &br#"{"title":"b"}"#[..]),
        ("PATCH", "/t/acme/notes/all", &br#"{"title":"c"}"#[..]),
        ("POST", "/t/acme/query", &br#"{"query":"del notes"}"#[..]),
        (
            "POST",
            "/t/acme/batch",
            &br#"{"query":"put notes {title: \"d\"}"}"#[..],
        ),
    ] {
        let r = call(n.port, method, target, body, None);
        assert_eq!(r.status, 503, "{method} {target}: {}", r.text());
        assert!(r.head.contains("Retry-After: 1"), "{}", r.head);
        // The body says it too: a replica's sync reads the wait there.
        assert!(r.text().contains(r#""retry_after":1}"#), "{}", r.text());
    }
    // Reads, including a read through /query, still work.
    let r = query(n.port, "acme", "get notes");
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"a\""));

    call(
        n.port,
        "POST",
        "/_admin/tenants/acme/thaw",
        b"",
        Some(ADMIN),
    );
    let r = call(n.port, "POST", "/t/acme/notes", br#"{"title":"b"}"#, None);
    assert_eq!(r.status, 201, "{}", r.text());
}

#[test]
fn an_exported_image_imports_as_a_working_tenant() {
    let n = start("image", admin_cfg());
    create(n.port, "acme");
    query(
        n.port,
        "acme",
        "create collection docs (title text, v vector<2> @hnsw(cosine))",
    );
    query(
        n.port,
        "acme",
        r#"put docs [{title: "x", v: [1, 0]}, {title: "y", v: [0, 1]}]"#,
    );

    let image = call(n.port, "GET", "/_admin/tenants/acme/file", b"", Some(ADMIN));
    assert_eq!(image.status, 200);
    assert!(image.head.contains("application/octet-stream"));

    // A sync log a tenant of the name left (`fennec_core::fs`) is not the
    // image's, and goes before the image is renamed into the place -- where
    // the system keeps logs: Windows keeps and reads none (windows.md, 3).
    let stale = fennec_core::fs::beside(&n.dir.join("copy.fennec"), "sync");
    std::fs::write(&stale, b"FENECSYN another file's log").unwrap();
    let r = call(
        n.port,
        "PUT",
        "/_admin/tenants/copy/file",
        &image.body,
        Some(ADMIN),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(stale.exists(), cfg!(windows));
    let r = call(
        n.port,
        "POST",
        "/t/copy/docs/near",
        br#"{"vector":[0,1],"limit":1,"select":["title"]}"#,
        None,
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"y\""), "{}", r.text());

    // A name that is taken is not overwritten, and a truncated image never
    // becomes a tenant.
    let r = call(
        n.port,
        "PUT",
        "/_admin/tenants/copy/file",
        &image.body,
        Some(ADMIN),
    );
    assert_eq!(r.status, 409);
    let cut = &image.body[..image.body.len() / 2];
    let r = call(n.port, "PUT", "/_admin/tenants/cut/file", cut, Some(ADMIN));
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(!n.dir.join("cut.fennec").exists());
    assert!(!n.dir.join("cut.fennec.importing").exists());
}

#[test]
fn each_tenant_has_its_own_change_sequence() {
    let n = start("changes", admin_cfg());
    create(n.port, "acme");
    create(n.port, "beta");
    for t in ["acme", "beta"] {
        query(n.port, t, "create collection notes (title text)");
    }
    for i in 0..5 {
        let body = format!(r#"{{"title":"{i}"}}"#);
        call(n.port, "POST", "/t/acme/notes", body.as_bytes(), None);
    }
    call(n.port, "POST", "/t/beta/notes", br#"{"title":"b"}"#, None);

    let seed = |t: &str| -> String {
        let mut s = TcpStream::connect(("127.0.0.1", n.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(
            format!("GET /t/{t}/notes/changes HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        )
        .unwrap();
        let mut buf = String::new();
        let mut chunk = [0u8; 4096];
        while !buf.contains("event: seed") || !buf.ends_with("\n\n") {
            let k = s.read(&mut chunk).unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk[..k]));
        }
        buf
    };
    let acme = seed("acme");
    let beta = seed("beta");
    // The schema mark plus five writes on one side, plus one on the other:
    // the counters are the tenants' own, not a node-wide clock (which
    // would read 8 on both).
    assert!(acme.contains("\"seq\":6"), "{acme}");
    assert!(beta.contains("\"seq\":2"), "{beta}");
}

#[test]
fn deleting_a_tenant_ends_its_streams_and_removes_the_file() {
    let n = start("delete", admin_cfg());
    create(n.port, "acme");
    query(n.port, "acme", "create collection notes (title text)");

    let mut s = TcpStream::connect(("127.0.0.1", n.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET /t/acme/notes/changes HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut buf = String::new();
    let mut chunk = [0u8; 4096];
    while !buf.contains("event: seed") {
        let k = s.read(&mut chunk).unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk[..k]));
    }

    let r = call(n.port, "DELETE", "/_admin/tenants/acme", b"", Some(ADMIN));
    assert_eq!(r.status, 204, "{}", r.text());
    assert!(!n.dir.join("acme.fennec").exists());

    // The stream says why it ends, then closes.
    let mut rest = String::new();
    let _ = s.read_to_string(&mut rest);
    assert!(rest.contains("event: error"), "{rest}");
    assert_eq!(call(n.port, "GET", "/t/acme/notes", b"", None).status, 404);
}

#[test]
fn an_idle_tenant_closes_and_reopens_with_its_data() {
    let n = start("idle", admin_cfg());
    create(n.port, "acme");
    query(n.port, "acme", "create collection notes (title text)");
    call(
        n.port,
        "POST",
        "/t/acme/notes",
        br#"{"title":"kept"}"#,
        None,
    );

    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
    assert!(n.tenants.stats().open.is_empty());

    let r = call(n.port, "GET", "/t/acme/notes", b"", None);
    assert_eq!(r.text(), r#"[{"id":1,"title":"kept"}]"#);
}

#[test]
fn a_tenant_in_use_is_not_closed() {
    let n = start("busy", admin_cfg());
    create(n.port, "acme");
    let held = n.tenants.get("acme").unwrap();
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 0);
    drop(held);
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
}

#[test]
fn over_the_memory_ceiling_idle_tenants_make_room() {
    let n = start_with("memory", admin_cfg(), |t| t.with_max_memory(1));
    create(n.port, "a");
    query(n.port, "a", "create collection notes (title text)");
    call(n.port, "POST", "/t/a/notes", br#"{"title":"x"}"#, None);
    create(n.port, "b");
    // Opening `b` over a one-byte ceiling closed `a`, which nothing held.
    let open: Vec<String> = n.tenants.stats().open.into_iter().map(|o| o.0).collect();
    assert_eq!(open, ["b"]);
    // And `a` comes back intact on the next request.
    let r = call(n.port, "GET", "/t/a/notes", b"", None);
    assert_eq!(r.text(), r#"[{"id":1,"title":"x"}]"#);
}

/// With mapping off (`fennec-server --no-mmap`), a tenant's documents are read
/// into memory and counted, as a single file's are: the flag reached the
/// single-file server alone, and a tenant node on a network file system
/// mapped its files all the same.
#[test]
fn tenant_files_are_read_into_memory_with_mapping_off() {
    let dir = scratch("nommap");
    {
        let tenants = Tenants::new(&dir).unwrap();
        let t = tenants.create("acme").unwrap();
        let mut g = t.db.write().unwrap();
        g.execute(&fennec_ql::parse_one("create collection notes (body text)").unwrap())
            .unwrap();
        let body = "x".repeat(200);
        for _ in 0..500 {
            g.execute(&fennec_ql::parse_one(&format!("put notes {{body: \"{body}\"}}")).unwrap())
                .unwrap();
        }
        g.sync().unwrap();
    }
    let held = |tenants: Tenants| {
        tenants
            .get("acme")
            .unwrap()
            .db
            .read()
            .unwrap()
            .memory_bytes()
    };
    let mapped = held(Tenants::new(&dir).unwrap());
    let read = held(Tenants::new(&dir).unwrap().with_mmap(false));
    assert!(read > mapped + 500 * 150, "read {read}, mapped {mapped}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A node taking JSON Web Tokens, its policy the usual `owner = $jwt.sub`,
/// and the tenants `acme` and `globex` each holding a note of alice's.
fn scoped_node(tag: &str, demands: fennec_http::access::Demands) -> (Node, Arc<Access>) {
    let access = Arc::new(
        Access::new(JWT_SECRET, "notes  read,write  where owner = $jwt.sub\n")
            .unwrap()
            .demanding(demands),
    );
    let cfg = Config {
        token: Some("root".into()),
        access: Some(Arc::clone(&access)),
        ..admin_cfg()
    };
    let n = start(tag, cfg);
    for t in ["acme", "globex"] {
        create(n.port, t);
        for q in [
            "create collection notes (owner text @hash, title text, v vector<2> @hnsw)".to_string(),
            format!("put notes {{owner: \"alice\", title: \"{t} secret\", v: [1, 0]}}"),
        ] {
            let mut body = String::from("{\"query\":");
            fennec_core::json::escape_into(&mut body, &q);
            body.push('}');
            let r = call(
                n.port,
                "POST",
                &format!("/t/{t}/query"),
                body.as_bytes(),
                Some("Bearer root"),
            );
            assert_eq!(r.status, 200, "{}", r.text());
        }
    }
    (n, access)
}

const JWT_SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";

/// Every route under a tenant's prefix, as `(method, path, body)`.
const ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "query", r#"{"query":"get notes"}"#),
    ("POST", "batch", r#"{"query":"get notes"}"#),
    ("GET", "notes", ""),
    ("GET", "notes?select=title", ""),
    ("POST", "notes", r#"{"title":"planted"}"#),
    ("PATCH", "notes?title=eq.x", r#"{"title":"y"}"#),
    ("DELETE", "notes?title=eq.x", ""),
    (
        "POST",
        "notes/near",
        r#"{"field":"v","vector":[1,0],"limit":1}"#,
    ),
    ("GET", "collections", ""),
    ("GET", "notes/changes", ""),
    ("GET", "_changes", ""),
    ("GET", "_schema", ""),
    (
        "POST",
        "_schema/plan?mode=follow",
        r#"{"format":1,"fennecql":""}"#,
    ),
    ("GET", "_stats/statements", ""),
    ("POST", "_call/peek", r#"{"params":{}}"#),
];

/// A token is held to the tenant it names: a JWT minted for one tenant read
/// and subscribed to every other on the node, its policy's `owner =
/// $jwt.sub` matching alice's rows in each tenant's file alike.
#[test]
fn a_token_reaches_only_the_tenant_it_names() {
    let (n, access) = scoped_node("bound", Default::default());
    let acme = access.mint(r#"{"sub":"alice","tenant":"acme"}"#).unwrap();
    let both = access
        .mint(r#"{"sub":"alice","tenant":["acme","globex"]}"#)
        .unwrap();
    let unbound = access.mint(r#"{"sub":"alice"}"#).unwrap();
    let odd = access.mint(r#"{"sub":"alice","tenant":7}"#).unwrap();
    let bearer = |t: &str| format!("Bearer {t}");

    // Its own tenant: read and written as before.
    let r = call(
        n.port,
        "GET",
        "/t/acme/notes?select=title",
        b"",
        Some(&bearer(&acme)),
    );
    assert_eq!(
        (r.status, r.text().as_str()),
        (200, r#"[{"title":"acme secret"}]"#)
    );

    for (method, path, body) in ROUTES {
        let target = format!("/t/globex/{path}");
        for (who, token) in [
            ("acme's", &acme),
            ("an unbound", &unbound),
            ("a number's", &odd),
        ] {
            let r = call(
                n.port,
                method,
                &target,
                body.as_bytes(),
                Some(&bearer(token)),
            );
            assert_eq!(
                r.status,
                403,
                "{who} token, {method} {target}: {}",
                r.text()
            );
            assert!(
                !r.text().contains("secret"),
                "{method} {target}: {}",
                r.text()
            );
        }
    }
    // A tenant that does not exist answers the same: the token is judged
    // before the tenant is looked up, so a 404 tells nothing of who exists.
    let r = call(n.port, "GET", "/t/nobody/notes", b"", Some(&bearer(&acme)));
    assert_eq!(r.status, 403, "{}", r.text());

    // A list names each tenant it holds.
    let r = call(
        n.port,
        "GET",
        "/t/globex/notes?select=title",
        b"",
        Some(&bearer(&both)),
    );
    assert_eq!(
        (r.status, r.text().as_str()),
        (200, r#"[{"title":"globex secret"}]"#)
    );
    // The node's own token is unaffected.
    let r = call(
        n.port,
        "GET",
        "/t/globex/notes?select=title",
        b"",
        Some("Bearer root"),
    );
    assert_eq!(r.status, 200, "{}", r.text());

    // And nothing was written to globex by the refused writes.
    let r = call(
        n.port,
        "GET",
        "/t/globex/notes?select=title",
        b"",
        Some("Bearer root"),
    );
    assert_eq!(r.text(), r#"[{"title":"globex secret"}]"#);
}

/// `--jwt-unbound-tenants` takes a token naming no tenant for every tenant,
/// the way a node took every token before; a named tenant still binds, and
/// another claim name is read where the node is told so.
#[test]
fn unbound_tokens_are_taken_only_when_the_node_is_told() {
    let demands = fennec_http::access::Demands {
        unbound_tenants: true,
        tenant_claim: "org".into(),
        ..Default::default()
    };
    let (n, access) = scoped_node("unbound", demands);
    let unbound = access.mint(r#"{"sub":"alice"}"#).unwrap();
    let r = call(
        n.port,
        "GET",
        "/t/globex/notes?select=title",
        b"",
        Some(&format!("Bearer {unbound}")),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let acme = access.mint(r#"{"sub":"alice","org":"acme"}"#).unwrap();
    let r = call(
        n.port,
        "GET",
        "/t/globex/notes",
        b"",
        Some(&format!("Bearer {acme}")),
    );
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(
        n.port,
        "GET",
        "/t/acme/notes",
        b"",
        Some(&format!("Bearer {acme}")),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    // Only a token without the claim is unbound: one holding it as `null`
    // -- `"org": user.org` minted for a user without one -- or as a value
    // naming no tenant meant to be bound, and reaches none.
    for org in ["null", "\"\"", "5", "{}", "[]", "[5, null, \"\"]"] {
        let token = access
            .mint(&format!(r#"{{"sub":"alice","org":{org}}}"#))
            .unwrap();
        let r = call(
            n.port,
            "GET",
            "/t/globex/notes",
            b"",
            Some(&format!("Bearer {token}")),
        );
        assert_eq!(r.status, 403, "{org}: {}", r.text());
        assert!(r.text().contains("names no tenant"), "{org}: {}", r.text());
    }
}

/// A scoped write to a tenant is checked as one to a single database: the
/// `WITH CHECK` hook was installed on a single database alone, and a
/// tenant took alice writing a row of bob's.
#[test]
fn a_scoped_write_to_a_tenant_is_checked() {
    let (n, access) = scoped_node("check", Default::default());
    let alice = access.mint(r#"{"sub":"alice","tenant":"acme"}"#).unwrap();
    let auth = format!("Bearer {alice}");
    let r = call(
        n.port,
        "POST",
        "/t/acme/notes",
        br#"{"owner":"bob","title":"planted"}"#,
        Some(&auth),
    );
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(
        n.port,
        "POST",
        "/t/acme/notes",
        br#"{"title":"mine"}"#,
        Some(&auth),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let r = call(
        n.port,
        "GET",
        "/t/acme/notes?select=owner,title&order=title",
        b"",
        Some("Bearer root"),
    );
    assert_eq!(
        r.text(),
        r#"[{"owner":"alice","title":"acme secret"},{"owner":"alice","title":"mine"}]"#
    );
}

/// The `instance` a node's answer gives a tenant's file.
fn instance_of(r: &Res) -> Option<String> {
    let text = r.text();
    let rest = text.split("\"instance\":\"").nth(1)?;
    Some(rest.split('"').next()?.to_string())
}

/// `POST /_admin/tenants/<tenant>/reclaim` naming `ids`.
fn reclaim(port: u16, tenant: &str, ids: &[&str]) -> Res {
    let named: Vec<String> = ids.iter().map(|i| format!("\"{i}\"")).collect();
    let body = format!("{{\"instances\":[{}]}}", named.join(","));
    let target = format!("/_admin/tenants/{tenant}/reclaim");
    call(port, "POST", &target, body.as_bytes(), Some(ADMIN))
}

/// Waits for `/t/<tenant>/notes/changes` to send its seed: the stream holds
/// the tenant from then.
fn stream_on(port: u16, tenant: &str) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let ask = format!("GET /t/{tenant}/notes/changes HTTP/1.1\r\nHost: localhost\r\n\r\n");
    s.write_all(ask.as_bytes()).unwrap();
    let mut buf = String::new();
    let mut chunk = [0u8; 4096];
    while !buf.contains("event: seed") {
        let k = s.read(&mut chunk).unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk[..k]));
    }
    s
}

/// A file a router left is taken back by the identity its node gave it,
/// and by no other: one named wrongly is answered with what the file
/// carries and kept, rows and all, its stream untouched. The identity
/// outlives a close; a reclaim of an open file ends its streams before the
/// file goes, and one of a file gone says so.
#[test]
fn a_file_is_taken_back_only_by_the_identity_it_carries() {
    let n = start("reclaim", admin_cfg());
    let r = call(n.port, "PUT", "/_admin/tenants/acme", b"", Some(ADMIN));
    assert_eq!(r.status, 201, "{}", r.text());
    let id = instance_of(&r).expect("an identity");
    query(n.port, "acme", "create collection notes (title text)");
    query(n.port, "acme", "put notes {title: \"kept\"}");

    let r = reclaim(n.port, "acme", &["00000000000000ab"]);
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(
        instance_of(&r).as_deref(),
        Some(id.as_str()),
        "{}",
        r.text()
    );
    assert!(n.dir.join("acme.fennec").exists());

    // Closed and opened again: the same file, the same identity.
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
    let r = call(n.port, "GET", "/t/acme/notes", b"", None);
    assert!(r.text().contains("kept"), "{}", r.text());
    assert_eq!(
        n.tenants.instance("acme").map(fennec_http::instance::text),
        Some(id.clone())
    );

    let mut s = stream_on(n.port, "acme");
    let r = reclaim(n.port, "acme", &["00000000000000ab", &id]);
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"taken\":true"), "{}", r.text());
    assert!(!n.dir.join("acme.fennec").exists());
    assert!(!n.dir.join("acme.instance").exists());
    let mut rest = String::new();
    let _ = s.read_to_string(&mut rest);
    assert!(rest.contains("event: error"), "{rest}");

    let r = reclaim(n.port, "acme", &[&id]);
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.text().contains("\"taken\":false"), "{}", r.text());
    let bad = call(
        n.port,
        "POST",
        "/_admin/tenants/acme/reclaim",
        b"{}",
        Some(ADMIN),
    );
    assert_eq!(bad.status, 400, "{}", bad.text());
}

/// A file at the same place is another file: a tenant deleted and made
/// again carries another identity -- the one a router proposed for it,
/// here -- and one put in place while the node did not hold it -- a
/// restore, a binary from before identities making it again -- carries
/// none, which nothing takes back.
#[test]
fn a_later_file_at_the_place_is_not_taken_back() {
    let n = start("reclaim-later", admin_cfg());
    let r = call(n.port, "PUT", "/_admin/tenants/acme", b"", Some(ADMIN));
    let first = instance_of(&r).expect("an identity");
    let r = call(n.port, "DELETE", "/_admin/tenants/acme", b"", Some(ADMIN));
    assert_eq!(r.status, 204, "{}", r.text());
    let mut s = TcpStream::connect(("127.0.0.1", n.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        b"PUT /_admin/tenants/acme HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
          Authorization: Bearer adm\r\nFennec-Instance: 00000000000000cd\r\n\
          Content-Length: 0\r\n\r\n",
    )
    .unwrap();
    let mut answer = String::new();
    let _ = s.read_to_string(&mut answer);
    assert!(
        answer.contains("\"instance\":\"00000000000000cd\""),
        "{answer}"
    );
    query(n.port, "acme", "create collection notes (title text)");
    query(n.port, "acme", "put notes {title: \"made again\"}");
    let mut stream = stream_on(n.port, "acme");
    let r = reclaim(n.port, "acme", &[&first]);
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(instance_of(&r).as_deref(), Some("00000000000000cd"));
    // Judged before anything holding it was let go: its stream goes on.
    query(n.port, "acme", "put notes {title: \"after\"}");
    let mut buf = String::new();
    let mut chunk = [0u8; 4096];
    while !buf.contains("after") {
        let k = stream.read(&mut chunk).unwrap();
        assert!(k > 0, "the stream ended: {buf}");
        buf.push_str(&String::from_utf8_lossy(&chunk[..k]));
    }
    drop(stream);

    // Put in place by another hand while closed: a file of no identity.
    let deadline = Instant::now() + Duration::from_secs(10);
    while n.tenants.close_idle(Duration::ZERO) == 0 {
        assert!(Instant::now() < deadline, "acme never closed");
        std::thread::sleep(Duration::from_millis(10));
    }
    let file = n.dir.join("acme.fennec");
    let aside = n.dir.join("aside");
    std::fs::copy(&file, &aside).unwrap();
    std::fs::rename(&aside, &file).unwrap();
    assert_eq!(n.tenants.instance("acme"), None);
    let r = reclaim(n.port, "acme", &["00000000000000cd"]);
    assert_eq!(r.status, 409, "{}", r.text());
    assert!(r.text().contains("\"instance\":null"), "{}", r.text());
    let r = call(n.port, "GET", "/t/acme/notes", b"", None);
    assert!(r.text().contains("made again"), "{}", r.text());
    // Opened, it is held as the file it is, of no identity; the close
    // forgets the sidecar rather than bind it to this file.
    assert_eq!(n.tenants.instance("acme"), None);
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
    assert!(!n.dir.join("acme.instance").exists());
    assert_eq!(n.tenants.instance("acme"), None);
}

/// [`call`] with `body` sent `Transfer-Encoding: chunked`, in chunks of
/// `size`, each a write of its own.
fn chunked_call(
    port: u16,
    method: &str,
    target: &str,
    body: &[u8],
    auth: &str,
    size: usize,
) -> Res {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.set_nodelay(true).unwrap();
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: {auth}\r\n\
         Transfer-Encoding: chunked\r\n\r\n"
    )
    .unwrap();
    for c in body.chunks(size) {
        let mut one = format!("{:x}\r\n", c.len()).into_bytes();
        one.extend_from_slice(c);
        one.extend_from_slice(b"\r\n");
        // A refusal before the body is read closes the connection under
        // the writes still going: its answer is read all the same.
        if s.write_all(&one).is_err() {
            break;
        }
    }
    let _ = s.write_all(b"0\r\n\r\n");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&out[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Res {
        status,
        head,
        body: out[split + 4..].to_vec(),
    }
}

/// `/_admin`'s bodies -- a tenant's image, read to the import's ceiling
/// past the data's where its head carries the admin token, and its quota --
/// and a tenant's statement come chunked as they come by their length.
#[test]
fn admin_and_tenant_bodies_come_chunked() {
    let n = start(
        "chunked",
        Config {
            max_body: 4 << 10,
            max_import: 4 << 20,
            ..admin_cfg()
        },
    );
    create(n.port, "acme");
    let make = br#"{"query":"create collection docs (title text, pad text)"}"#;
    let r = chunked_call(n.port, "POST", "/t/acme/query", make, "Bearer x", 7);
    assert_eq!(r.status, 200, "{}", r.text());
    let pad = "p".repeat(3000);
    for i in 0..10 {
        let r = query(
            n.port,
            "acme",
            &format!("put docs {{title: 't{i}', pad: '{pad}'}}"),
        );
        assert_eq!(r.status, 200, "{}", r.text());
    }
    // Past the data's ceiling and within the import's, a tenant's
    // statement is refused at the chunk that passes the data's, as by its
    // length: no request but an import carrying the admin token is read
    // to the import's.
    let big = format!(
        r#"{{"query":"get docs count where pad = '{}'"}}"#,
        "q".repeat(5000)
    );
    let r = chunked_call(
        n.port,
        "POST",
        "/t/acme/query",
        big.as_bytes(),
        "Bearer x",
        1000,
    );
    assert_eq!(r.status, 413, "{}", r.text());

    let image = call(n.port, "GET", "/_admin/tenants/acme/file", b"", Some(ADMIN));
    assert_eq!(image.status, 200);
    assert!(image.body.len() > 4 << 10, "{}", image.body.len());
    let r = chunked_call(
        n.port,
        "PUT",
        "/_admin/tenants/copy/file",
        &image.body,
        ADMIN,
        4096,
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let r = query(n.port, "copy", "get docs count");
    assert!(r.text().contains(r#""count":10"#), "{}", r.text());

    let r = chunked_call(
        n.port,
        "PUT",
        "/_admin/tenants/copy/quota",
        br#"{"rows": 10}"#,
        ADMIN,
        3,
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let r = query(n.port, "copy", "put docs {title: 'eleventh'}");
    assert_eq!(r.status, 507, "{}", r.text());
}

/// A request whose head declares `framing` -- `Content-Length: <n>` or
/// `Transfer-Encoding: chunked` -- with `sent` bytes of its body after it
/// and no more, answered before the rest comes: the answer, read to the
/// server's end, within a bound only what must happen waits for.
fn declared(
    port: u16,
    method: &str,
    target: &str,
    auth: Option<&str>,
    framing: &str,
    sent: &[u8],
) -> Res {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let auth = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    let head = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\n{auth}{framing}\r\n\r\n");
    s.write_all(head.as_bytes()).unwrap();
    // The answer may come before the body is written: a refusal from the
    // head ends the connection under the write.
    let _ = s.write_all(sent);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("an answer before the body came");
    let head = String::from_utf8_lossy(&out[..split]).into_owned();
    Res {
        status: head
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0),
        head,
        body: out[split + 4..].to_vec(),
    }
}

/// Only an image's import carrying the admin token is read past
/// `max_body`, and only once its head said so: any other request's body is
/// held to `max_body` before a byte of it is read, and an import carrying
/// another token, or none, is refused from its head, 401, its body never
/// read -- a gibibyte declared by an unauthenticated peer is answered with
/// 64 KiB of it sent, where a node read it whole, to the import's ceiling.
#[test]
fn only_an_import_carrying_the_admin_token_is_read_past_the_data_ceiling() {
    let max_body = 4 << 10;
    let n = start(
        "ceiling",
        Config {
            max_body,
            ..admin_cfg()
        },
    );
    create(n.port, "acme");
    let gib = format!("Content-Length: {}", 1u64 << 30);
    let some = vec![b'x'; 64 << 10];
    for auth in [None, Some("Bearer guessed"), Some("adm")] {
        let r = declared(n.port, "PUT", "/_admin/tenants/x/file", auth, &gib, &some);
        assert_eq!(r.status, 401, "{auth:?}: {}", r.text());
        assert!(r.head.contains("WWW-Authenticate: Bearer"), "{}", r.head);
        assert!(r.head.contains("Connection: close"), "{}", r.head);
        // Chunked, a gibibyte's chunk declared.
        let r = declared(
            n.port,
            "PUT",
            "//_admin/tenants/x/file",
            auth,
            "Transfer-Encoding: chunked",
            b"40000000\r\nxxxx",
        );
        assert_eq!(r.status, 401, "{auth:?}: {}", r.text());
    }
    // Past `max_body` by a byte, every other route is refused from its
    // head: a tenant's data, a tenant's own statement, `/_admin/`'s other
    // routes with the admin token, and the image read with it.
    let over = format!("Content-Length: {}", max_body + 1);
    for (method, target, auth) in [
        ("POST", "/t/acme/query", None),
        ("PUT", "/t/acme/docs/1", None),
        ("PUT", "/_admin/tenants/acme/quota", Some(ADMIN)),
        ("POST", "/_admin/tenants/acme/query", Some(ADMIN)),
        ("GET", "/_admin/tenants/acme/file", Some(ADMIN)),
        ("PUT", "/_admin/tenants/acme/file/more", Some(ADMIN)),
    ] {
        let r = declared(n.port, method, target, auth, &over, b"");
        assert_eq!(r.status, 413, "{method} {target}: {}", r.text());
    }
    let r = declared(
        n.port,
        "POST",
        "/t/acme/query",
        None,
        "Transfer-Encoding: chunked",
        format!("{:x}\r\n", max_body + 1).as_bytes(),
    );
    assert_eq!(r.status, 413, "{}", r.text());

    // An image past `max_body`, carrying the token, by its length and
    // chunked, installs a tenant that works.
    let make = "create collection docs (title text, pad text)";
    assert_eq!(query(n.port, "acme", make).status, 200);
    for i in 0..4 {
        let put = format!("put docs {{title: 't{i}', pad: '{}'}}", "p".repeat(3000));
        assert_eq!(query(n.port, "acme", &put).status, 200);
    }
    let image = call(n.port, "GET", "/_admin/tenants/acme/file", b"", Some(ADMIN));
    assert_eq!(image.status, 200);
    assert!(image.body.len() > max_body, "{}", image.body.len());
    let r = call(
        n.port,
        "PUT",
        "/_admin/tenants/copy/file",
        &image.body,
        Some(ADMIN),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let r = chunked_call(
        n.port,
        "PUT",
        "/_admin/tenants/chunked/file",
        &image.body,
        ADMIN,
        1000,
    );
    assert_eq!(r.status, 201, "{}", r.text());
    for copy in ["copy", "chunked"] {
        let r = query(n.port, copy, "get docs count");
        assert!(r.text().contains(r#""count":4"#), "{copy}: {}", r.text());
    }
}

/// A node whose registry no scheduler runs -- `--http-read-only` -- takes
/// no scheduled statement: its definition is refused (400) in the engine's
/// words, never a schedule no one runs; a callable statement is defined as
/// ever. A node running its scheduler takes both (`node_scheduled.rs`).
#[test]
fn a_node_running_no_scheduler_takes_no_scheduled_statement() {
    let n = start("scheduled", admin_cfg());
    create(n.port, "acme");
    let r = query(n.port, "acme", "create collection holds (state text @hash)");
    assert_eq!(r.status, 200, "{}", r.text());
    let r = query(
        n.port,
        "acme",
        "create statement tidy every 1d as del holds where state = 'x'",
    );
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(
        r.text()
            .contains("no scheduler runs this database's statements"),
        "{}",
        r.text()
    );
    let r = query(n.port, "acme", "statements");
    assert!(!r.text().contains("tidy"), "{}", r.text());
    let r = query(
        n.port,
        "acme",
        "create statement tidy() public as del holds where state = 'x'",
    );
    assert_eq!(r.status, 200, "{}", r.text());

    // Nor an image holding one -- an install, a move's target: refused,
    // never a 409 a move would read as a file left there, and no file.
    fennec_ql::install();
    let mut db = fennec_core::engine::Database::new();
    db.allow_schedules();
    for q in [
        "create collection holds (state text @hash)",
        "create statement nightly every 1d as del holds where state = 'x'",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    let image = db.snapshot();
    let r = call(
        n.port,
        "PUT",
        "/_admin/tenants/held/file",
        &image,
        Some(ADMIN),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(
        r.text()
            .contains("holds the scheduled statement `nightly`, and this node runs no scheduler"),
        "{}",
        r.text()
    );
    assert!(!n.dir.join("held.fennec").exists());
    // Dropped, it installs.
    db.execute(&fennec_ql::parse_one("drop statement nightly").unwrap())
        .unwrap();
    let r = call(
        n.port,
        "PUT",
        "/_admin/tenants/held/file",
        &db.snapshot(),
        Some(ADMIN),
    );
    assert_eq!(r.status, 201, "{}", r.text());
}

/// An image of `xs` as rows of `c`, as a move's source sends one.
fn image_of(xs: &[i64]) -> Vec<u8> {
    let mut db = fennec_core::engine::Database::new();
    let mut run = |q: &str| {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    };
    run("create collection c (x int)");
    for x in xs {
        run(&format!("put c {{x: {x}}}"));
    }
    db.snapshot()
}

/// The rows of tenant file `file`'s `c`, read as a tool that only looks.
fn xs_of(file: &std::path::Path) -> Vec<String> {
    let db = fennec_core::fs::open_read_only(file).unwrap();
    let q = fennec_ql::parse_one("get c select x order id").unwrap();
    let r = db.query(&q, &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| format!("{:?}", r.values))
        .collect()
}

/// An image whose second half waits for `open` once its first half was
/// read, saying so on `told`.
struct Gated {
    bytes: Vec<u8>,
    at: usize,
    told: Option<std::sync::mpsc::Sender<()>>,
    open: std::sync::mpsc::Receiver<()>,
}

impl Read for Gated {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let half = self.bytes.len() / 2;
        if self.at == half {
            if let Some(told) = self.told.take() {
                told.send(()).unwrap();
                self.open.recv().unwrap();
            }
        }
        let end = if self.at < half {
            half
        } else {
            self.bytes.len()
        };
        let n = buf.len().min(end - self.at);
        buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// An install of `image` as `acme`, under the identity `proposed`, on a
/// thread of its own, returned once half its image was taken, held there
/// until the sender is told to let it go on: its thread and that sender.
#[allow(clippy::type_complexity)]
fn installing_half_taken(
    tenants: &Arc<Tenants>,
    image: Vec<u8>,
    proposed: Option<u64>,
) -> (
    std::thread::JoinHandle<Result<Option<u64>, fennec_http::tenants::Refused>>,
    std::sync::mpsc::Sender<()>,
) {
    use std::sync::mpsc;
    let (told, half_read) = mpsc::channel();
    let (open, gate) = mpsc::channel();
    let tenants = Arc::clone(tenants);
    let mut body = Gated {
        bytes: image,
        at: 0,
        told: Some(told),
        open: gate,
    };
    let installing = std::thread::spawn(move || {
        let len = body.bytes.len() as u64;
        tenants.import_from("acme", &mut body, len, proposed)
    });
    half_read.recv().unwrap();
    (installing, open)
}

/// Two installs of one name at once: the first, part of its image taken,
/// holds the name -- the file beside the tenant's its own, made under the
/// tenant's lock and its claim of the name -- and the second is refused
/// before a byte of its body is read, as a create of the name is: 503,
/// come back in a second, `Retry-After` and `retry_after` saying so --
/// nothing of it ran, and the install ends -- never the 409 that says a
/// file is there, which a router took for one, judging a place no file was
/// in yet and forgetting its note of the install that then put one there.
/// The first lands whole, its rows the tenant's, nothing left beside it,
/// and an install after it finds the tenant there (409). With the side file
/// made before the tenant's slot was taken, as up to the image's review,
/// the second made `.importing` anew over the first's and was installed,
/// the first refused as its file was no longer the one written -- and on
/// Windows, where no rename was held to its writer's handle, one could put
/// the other's half-written image in the tenant's place.
#[test]
fn an_install_under_way_holds_its_name_and_another_is_refused() {
    /// An image read off as it comes, counting what was read.
    struct Counted<'a>(&'a [u8], usize);
    impl Read for Counted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (&self.0[self.1..]).read(buf)?;
            self.1 += n;
            Ok(n)
        }
    }

    let dir = scratch("installs");
    let tenants = Arc::new(Tenants::new(&dir).expect("dir"));
    let (first, second) = (image_of(&[1, 2, 3]), image_of(&[7, 8]));
    let (installing, open) = installing_half_taken(&tenants, first, None);
    let side = dir.join("acme.fennec.importing");
    assert!(
        side.exists(),
        "the first install's image goes beside the tenant's file"
    );

    let mut body = Counted(&second, 0);
    let refused = tenants
        .import_from("acme", &mut body, second.len() as u64, None)
        .expect_err("a second install of a name under way");
    assert_eq!(refused.0, 503, "{}", refused.1);
    assert!(refused.1.contains("being installed"), "{}", refused.1);
    assert_eq!(body.1, 0, "the second install's body was read");
    // As it goes out: come back in a second, in its head and its body.
    let answer = refused.response();
    assert_eq!(answer.status, 503);
    assert!(
        answer
            .extra
            .iter()
            .any(|(k, v)| k == "Retry-After" && v == "1"),
        "{:?}",
        answer.extra
    );
    let text = String::from_utf8_lossy(&answer.body);
    assert!(text.contains(r#""retry_after":1"#), "{text}");
    let made = tenants.create("acme").map(|_| ());
    let refused = made.expect_err("a create of a name an install holds");
    assert_eq!(refused.0, 503, "{}", refused.1);
    assert!(refused.1.contains("being installed"), "{}", refused.1);
    assert!(side.exists(), "the first install's file went");

    open.send(()).unwrap();
    installing
        .join()
        .unwrap()
        .unwrap_or_else(|e| panic!("the first install: {} {}", e.0, e.1));
    assert!(
        !side.exists(),
        "the installed image left beside the tenant's file"
    );
    assert_eq!(
        xs_of(&dir.join("acme.fennec")),
        ["[Int(1)]", "[Int(2)]", "[Int(3)]"],
        "the first install's image, whole"
    );
    let mut body = Counted(&second, 0);
    let refused = tenants
        .import_from("acme", &mut body, second.len() as u64, None)
        .expect_err("an install of a tenant there");
    assert_eq!(refused.0, 409, "{}", refused.1);
    assert_eq!(body.1, 0, "a body read for a tenant there");
    drop(tenants);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A reclaim of a name an install is taking an image for judges nothing:
/// refused 503, as a second install is, for the install may put the very
/// file the router noted there yet -- the router's own, its answer lost.
/// Judged, the place held no file, the reclaim answered it gone (200,
/// `"taken": false`), the router forgot its note, and the file the install
/// then put in place was one no router could take back. Once in place, the
/// file the install was given the noted identity is taken back by it.
#[test]
fn a_reclaim_during_an_install_of_its_name_judges_nothing() {
    let dir = scratch("reclaim-installing");
    let tenants = Arc::new(Tenants::new(&dir).expect("dir"));
    let noted = 0x1111_2222_3333_4444;
    let (installing, open) = installing_half_taken(&tenants, image_of(&[1, 2]), Some(noted));
    match tenants.reclaim("acme", &[noted]) {
        Err(fennec_http::tenants::Kept::Refused(r)) => {
            assert_eq!(r.0, 503, "{}", r.1);
            assert!(r.1.contains("being installed"), "{}", r.1);
        }
        other => panic!("a reclaim during the install: {other:?}"),
    }
    open.send(()).unwrap();
    let id = installing
        .join()
        .unwrap()
        .unwrap_or_else(|e| panic!("the install: {} {}", e.0, e.1));
    assert_eq!(id, Some(noted), "the install's identity");
    assert!(dir.join("acme.fennec").exists());
    assert!(matches!(tenants.reclaim("acme", &[noted]), Ok(true)));
    assert!(!dir.join("acme.fennec").exists(), "the noted file stayed");
    drop(tenants);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a node's last process left beside its tenants' files as it ended
/// -- an install's `.importing`, a copy's `.taking`, each as large as the
/// tenant -- goes as the node starts, under each tenant's lock taken
/// without waiting: one whose lock another holds stays, its own (this
/// process stands in for another's here: the lock refuses a second holder
/// in one process as in two, fennec-abi's tests holding the second). A file
/// of no tenant's name is let be. Before, a crash's `.importing` stayed
/// until an install of the name came again.
#[test]
fn a_node_starts_by_removing_what_its_last_process_left_beside_its_tenants() {
    let dir = scratch("left");
    std::fs::create_dir_all(&dir).unwrap();
    let left = |f: &str| {
        let p = dir.join(f);
        std::fs::write(&p, b"part of an image").unwrap();
        p
    };
    let (importing, taking) = (left("a.fennec.importing"), left("b.fennec.taking"));
    let (held, other) = (left("c.fennec.importing"), left("notes.importing"));
    let lock = fennec_abi::lock::hold(dir.join("c.fennec")).unwrap();
    let tenants = Tenants::new(&dir).expect("dir");
    assert!(!importing.exists(), "an install's file left");
    assert!(!taking.exists(), "a copy's image left");
    assert!(
        held.exists(),
        "a file whose tenant's lock another holds removed"
    );
    assert!(other.exists(), "a file of no tenant's removed");
    drop((tenants, lock));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tenant whose file is a link has an install's image and a copy's
/// written beside the file the link leads to, as its open writes that one
/// and its lock is taken there (`fs::resolved`): what a crash left there
/// goes as the node starts too, under the tenant's lock -- one whose lock
/// another holds stays -- the file and the link as they were. Looked for in
/// the node's directory alone, they stayed for good.
#[cfg(unix)]
#[test]
fn a_node_starts_by_removing_what_was_left_beside_a_linked_tenants_file() {
    let (dir, files) = (scratch("left-linked"), scratch("left-linked-files"));
    for d in [&dir, &files] {
        std::fs::create_dir_all(d).unwrap();
    }
    let image = image_of(&[1]);
    let linked = |t: &str| {
        let file = files.join(format!("{t}.fennec"));
        std::fs::write(&file, &image).unwrap();
        std::os::unix::fs::symlink(&file, dir.join(format!("{t}.fennec"))).unwrap();
        let left = |side: &str| {
            let p = files.join(format!("{t}.fennec.{side}"));
            std::fs::write(&p, b"part of an image").unwrap();
            p
        };
        (file, left("importing"), left("taking"))
    };
    let (file, importing, taking) = linked("a");
    let (_, held, _) = linked("b");
    let lock = fennec_abi::lock::hold(dir.join("b.fennec")).unwrap();
    let tenants = Tenants::new(&dir).expect("dir");
    assert!(
        !importing.exists(),
        "an install's file left beside the link's"
    );
    assert!(!taking.exists(), "a copy's image left beside the link's");
    assert!(
        held.exists(),
        "a file whose tenant's lock another holds removed"
    );
    assert_eq!(std::fs::read(&file).unwrap(), image, "the tenant's file");
    let link = std::fs::symlink_metadata(dir.join("a.fennec")).unwrap();
    assert!(link.file_type().is_symlink(), "the link");
    drop((tenants, lock));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&files);
}
