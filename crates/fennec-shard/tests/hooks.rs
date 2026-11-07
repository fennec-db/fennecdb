//! A tenant's webhooks behind the router (`docs/design/named-statements.md`,
//! F4's slice 4): its secrets set through `/_shard/tenants/<t>/hooks/<h>`,
//! taken by its primary and copied whole to each copy a failover could
//! promote, the directory recording names and a version, never a secret;
//! an event sent to `/t/<t>/_hooks/<h>` through the router, verified by its
//! own tenant's secrets and taken once; the set going with the tenant
//! through a move, a failover and a delete; and a quota refusing an
//! event's write (507), keeping no key.

use fennec_http::crypto::hmac_sha256;
use fennec_http::tenants::{Replicated, Tenants};
use fennec_shard::directory::Directory;
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const REPL: &str = "tenant-replication";

struct Node {
    addr: String,
    port: u16,
    dir: PathBuf,
    tenants: Arc<Tenants>,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A node; `follows` makes it the standby of the node at that address.
fn node(tag: &str, follows: Option<&str>) -> Node {
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!("fennec-shard-hooks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tenants = Arc::new(Tenants::new(&dir).unwrap().with_replication(Replicated {
        token: REPL.into(),
        buffer: 8 << 20,
        upstream: follows.map(|a| format!("http://{a}")),
        sync_on_write: true,
    }));
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some(format!("adm-{tag}")),
        sync_on_write: true,
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(Arc::clone(&tenants), cfg);
    let listener = server.bind().unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node {
        addr: addr.to_string(),
        port: addr.port(),
        dir,
        tenants,
    }
}

fn router(replicas: bool) -> u16 {
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        upstream_timeout: Duration::from_secs(10),
        replicas,
        ..Config::default()
    };
    let router = Router::new(Directory::in_memory(), cfg);
    let listener = router.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    port
}

fn register(port: u16, name: &str, nd: &Node, token: &str, standby: Option<&str>) {
    let standby = standby.map_or(String::new(), |s| format!(r#","standby":"{s}""#));
    let body = format!(r#"{{"addr":"{}","token":"{token}"{standby}}}"#, nd.addr);
    let (status, body, _) = call(port, "PUT", &format!("/_shard/nodes/{name}"), &[], &body);
    assert_eq!(status, 201, "{body}");
}

/// The status, body and head of `method target`.
fn call(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out[9..12].parse().unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (status, body.to_string(), head.to_string())
}

fn query(port: u16, tenant: &str, sql: &str) -> (u16, String) {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, sql);
    body.push('}');
    let (status, body, _) = call(port, "POST", &format!("/t/{tenant}/query"), &[], &body);
    (status, body)
}

/// Waits until `sql` on `tenant` at `port` answers with `want` in it.
fn until(port: u16, tenant: &str, sql: &str, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, body) = query(port, tenant, sql);
        if body.contains(want) {
            return;
        }
        assert!(Instant::now() < deadline, "{tenant} {sql}: {body}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Makes `tenant` on node `on` through the router: open orders `o-1` to
/// `o-3`, a journal, and statement `paid` taking hook `stripe`, `noted`
/// taking `notes`.
fn create(port: u16, tenant: &str, on: &str) {
    let (status, body, _) = call(
        port,
        "PUT",
        &format!("/_shard/tenants/{tenant}"),
        &[],
        &format!(r#"{{"node":"{on}"}}"#),
    );
    assert_eq!(status, 201, "{body}");
    for q in [
        "create collection orders (ref text @hash, status text)",
        "create collection journal (what text)",
        "put orders {ref: 'o-1', status: 'open'}",
        "put orders {ref: 'o-2', status: 'open'}",
        "put orders {ref: 'o-3', status: 'open'}",
        "create statement paid($order text from event.order) on hook stripe as set orders \
         {status: 'paid'} where ref = $order require 1",
        "create statement noted($what text from event.order) on hook notes as \
         insert journal {what: $what}",
    ] {
        let (status, body) = query(port, tenant, q);
        assert_eq!(status, 200, "{q}: {body}");
    }
}

/// Sets hook `hook` of `tenant` through the router to `secret`: its
/// answer's status and body.
fn set_hook(port: u16, tenant: &str, hook: &str, secret: &str) -> (u16, String) {
    let body = format!(r#"{{"scheme":"stripe","secrets":["{secret}"]}}"#);
    let target = format!("/_shard/tenants/{tenant}/hooks/{hook}");
    let (status, body, _) = call(port, "PUT", &target, &[], &body);
    (status, body)
}

/// Event `id` for order `order` to `tenant`'s hook `hook` through the
/// router, signed as Stripe signs one under `secret`: its status, body and
/// whether it was answered from its key.
fn event(
    port: u16,
    tenant: &str,
    hook: &str,
    secret: &str,
    id: &str,
    order: &str,
) -> (u16, String, bool) {
    let body = format!(r#"{{"id":"{id}","order":"{order}"}}"#);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac = hmac_sha256(secret.as_bytes(), format!("{t}.{body}").as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    let sig = format!("t={t},v1={hex}");
    let headers = [
        ("Content-Type", "application/json"),
        ("Stripe-Signature", sig.as_str()),
    ];
    let target = format!("/t/{tenant}/_hooks/{hook}");
    let (status, body, head) = call(port, "POST", &target, &headers, &body);
    (status, body, head.contains("Idempotent-Replayed: true"))
}

/// The version line of `tenant`'s set on `nd`, if it holds one.
fn version_on(nd: &Node, tenant: &str) -> Option<u64> {
    nd.tenants.hooks().version(tenant)
}

/// Three nodes in no pair, `n1` to `n3`, behind a replicating router.
fn three(tag: &str) -> ([Node; 3], u16) {
    let nodes = [1, 2, 3].map(|i| node(&format!("{tag}{i}"), None));
    let port = router(true);
    for (i, nd) in nodes.iter().enumerate() {
        let name = format!("n{}", i + 1);
        register(port, &name, nd, &format!("adm-{tag}{}", i + 1), None);
    }
    (nodes, port)
}

fn by_name<'a>(nodes: &'a [Node; 3], name: &str) -> &'a Node {
    &nodes[name[1..].parse::<usize>().unwrap() - 1]
}

/// Where the directory keeps `tenant`: its node, and its replica's.
fn placed(port: u16, tenant: &str) -> (String, Option<String>) {
    let (_, list, _) = call(port, "GET", "/_shard/tenants", &[], "");
    let entry = (list.split("},{"))
        .find(|e| e.contains(&format!("\"name\":\"{tenant}\"")))
        .unwrap_or_else(|| panic!("no {tenant} in {list}"));
    let value = |key: &str| {
        let rest = entry.split(&format!("\"{key}\":\"")).nth(1)?;
        Some(rest.split('"').next()?.to_string())
    };
    (value("node").unwrap(), value("replica"))
}

/// Set through the router, a tenant's secrets reach its primary and its
/// replica, each holding the version the directory records, which names
/// the hooks and holds no secret; an event through the router is verified
/// by its own tenant's secrets -- another tenant's signing nothing -- and
/// taken once; the set goes with the tenant through a move, its new
/// replica sent it, and a failover onto that replica keeps its events once
/// only; a delete takes every copy's set.
#[test]
fn a_tenants_secrets_follow_it_and_its_events_are_taken_once() {
    let (nodes, port) = three("own");
    create(port, "acme", "n1");
    create(port, "beta", "n2");

    // A secret refused at the primary records nothing, its value unsaid.
    let (status, body) = set_hook(port, "acme", "stripe", "");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("empty"), "{body}");
    assert_eq!(set_hook(port, "nobody", "stripe", "whsec_x").0, 404);
    let (status, body, _) = call(port, "GET", "/_shard/tenants/acme/hooks", &[], "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""version":null"#), "{body}");

    let (status, body) = set_hook(port, "acme", "stripe", "whsec_acme");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""behind":[]"#), "{body}");
    assert_eq!(set_hook(port, "beta", "stripe", "whsec_beta").0, 200);
    let (primary, replica) = placed(port, "acme");
    let replica = replica.expect("a replica of its own");
    let held = version_on(by_name(&nodes, &primary), "acme");
    assert!(held.is_some());
    assert_eq!(version_on(by_name(&nodes, &replica), "acme"), held);
    let (status, listing, _) = call(port, "GET", "/_shard/tenants/acme/hooks", &[], "");
    assert_eq!(status, 200, "{listing}");
    assert!(listing.contains(r#""name":"stripe""#), "{listing}");
    assert!(listing.contains(r#""behind":[]"#), "{listing}");
    assert!(!listing.contains("whsec_acme"), "{listing}");
    let (_, all, _) = call(port, "GET", "/_shard/tenants", &[], "");
    assert!(!all.contains("whsec_"), "{all}");

    // Each tenant's events by its own secret alone.
    let (status, body, _) = event(port, "acme", "stripe", "whsec_beta", "evt_1", "o-1");
    assert_eq!(status, 401, "{body}");
    let (status, body, _) = event(port, "beta", "stripe", "whsec_acme", "evt_1", "o-1");
    assert_eq!(status, 401, "{body}");
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_acme", "evt_1", "o-1");
    assert_eq!((status, replayed), (200, false), "{body}");
    assert_eq!(body, r#"{"event":"evt_1","taken":1}"#);
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_acme", "evt_1", "o-1");
    assert_eq!((status, replayed), (200, true), "{body}");
    let (_, body) = query(port, "beta", "get orders count where status = 'paid'");
    assert!(body.contains(r#""count":0"#), "{body}");

    // Moved: the target holds the set before the flip, the source's goes
    // with its copy, and the new replica is sent it.
    let to = ["n2", "n3"].into_iter().find(|n| *n != replica).unwrap();
    let (status, body, _) = call(
        port,
        "POST",
        "/_shard/tenants/acme/move",
        &[],
        &format!(r#"{{"to":"{to}"}}"#),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(version_on(by_name(&nodes, to), "acme"), held);
    assert_eq!(
        version_on(&nodes[0], "acme"),
        None,
        "gone with the source copy"
    );
    let (_, replica) = placed(port, "acme");
    let replica = replica.expect("a replica after the move");
    assert_eq!(version_on(by_name(&nodes, &replica), "acme"), held);
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_acme", "evt_1", "o-1");
    assert_eq!(
        (status, replayed),
        (200, true),
        "the key came with the image: {body}"
    );
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_acme", "evt_2", "o-2");
    assert_eq!((status, replayed), (200, false), "{body}");

    // Its node lost: promoted on its replica, which takes its events once.
    until(
        by_name(&nodes, &replica).port,
        "acme",
        "get orders count where status = 'paid'",
        r#""count":2"#,
    );
    let (status, body, _) = call(
        port,
        "POST",
        &format!("/_shard/nodes/{to}/failover"),
        &[],
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(placed(port, "acme").0, replica);
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_acme", "evt_2", "o-2");
    assert_eq!((status, replayed), (200, true), "{body}");
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_acme", "evt_3", "o-3");
    assert_eq!((status, replayed), (200, false), "{body}");

    // Taken away through the router: a hook not held is the node's 404.
    let (status, body, _) = call(port, "DELETE", "/_shard/tenants/acme/hooks/nosuch", &[], "");
    assert_eq!(status, 404, "{body}");
    // A delete takes the set with each copy it removes -- the node failed
    // over off keeps its file, and the set bound to it, until the router
    // takes that file back by its identity.
    let (status, body, _) = call(port, "DELETE", "/_shard/tenants/acme", &[], "");
    assert_eq!(status, 204, "{body}");
    for nd in &nodes {
        if nd.tenants.names().contains(&"acme".to_string()) {
            assert_eq!(
                nd.addr,
                by_name(&nodes, to).addr,
                "only the copy failed over off is left"
            );
            continue;
        }
        assert_eq!(version_on(nd, "acme"), None, "{}", nd.addr);
        assert!(!nd.dir.join("acme.hooks").exists(), "{}", nd.addr);
    }
}

/// A pair's standby holds its tenant's set and each event's block: the
/// router sends it the set, the event's 2xx waits for the standby, and the
/// failover onto it keeps the set and the event's key.
#[test]
fn a_standby_holds_the_set_and_the_event_and_keeps_both_after_a_failover() {
    let n1 = node("pair1", None);
    let n2 = node("pair2", Some(&n1.addr));
    let port = router(false);
    register(port, "n1", &n1, "adm-pair1", None);
    register(port, "n2", &n2, "adm-pair2", None);
    register(port, "n1", &n1, "adm-pair1", Some("n2"));
    create(port, "acme", "n1");
    let (status, body) = set_hook(port, "acme", "stripe", "whsec_pair");
    assert_eq!(status, 200, "{body}");
    assert_eq!(version_on(&n2, "acme"), version_on(&n1, "acme"));
    let (status, body, _) = event(port, "acme", "stripe", "whsec_pair", "evt_1", "o-1");
    assert_eq!(status, 200, "{body}");
    until(
        n2.port,
        "acme",
        "get orders count where status = 'paid'",
        r#""count":1"#,
    );
    let (status, body, _) = call(port, "POST", "/_shard/nodes/n1/failover", &[], "");
    assert_eq!(status, 200, "{body}");
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_pair", "evt_1", "o-1");
    assert_eq!((status, replayed), (200, true), "{body}");
    let (status, body, replayed) = event(port, "acme", "stripe", "whsec_pair", "evt_2", "o-2");
    assert_eq!((status, replayed), (200, false), "{body}");
    drop(n1);
}

/// An event whose write its tenant's quota refuses is 507, its key kept by
/// nothing: sent again once the quota has room, it is taken.
#[test]
fn an_event_past_its_tenants_quota_is_refused_and_kept_by_nothing() {
    let (_nodes, port) = three("quota");
    create(port, "acme", "n1");
    assert_eq!(set_hook(port, "acme", "notes", "whsec_quota").0, 200);
    let (_, body) = query(port, "acme", "get journal count");
    assert!(body.contains(r#""count":0"#), "{body}");
    let quota = |rows: u64| {
        let (status, body, _) = call(
            port,
            "PUT",
            "/_shard/tenants/acme/quota",
            &[],
            &format!(r#"{{"rows":{rows}}}"#),
        );
        assert_eq!(status, 200, "{body}");
    };
    // Every row it holds now, and not one more.
    let (_, stats, _) = call(port, "GET", "/_shard/tenants/acme/quota", &[], "");
    let usage = stats.split(r#""usage":{"#).nth(1).unwrap_or("");
    let rows: u64 = (usage.split(r#""rows":"#).nth(1))
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no usage in {stats}"));
    quota(rows);
    let (status, body, _) = event(port, "acme", "notes", "whsec_quota", "evt_q", "o-1");
    assert_eq!(status, 507, "{body}");
    assert!(body.contains(r#""quota":"rows""#), "{body}");
    quota(rows + 10);
    let (status, body, replayed) = event(port, "acme", "notes", "whsec_quota", "evt_q", "o-1");
    assert_eq!((status, replayed), (200, false), "{body}");
    let (_, body) = query(port, "acme", "get journal count");
    assert!(body.contains(r#""count":1"#), "{body}");
}

/// A schema declaring a hook is read by the router with the grammar, as
/// one declaring a binding or a schedule is, and made by the node: its
/// statement takes the hook's events once its secret is set.
#[test]
fn a_tenant_made_with_a_schema_declaring_a_hook_takes_its_events() {
    let (_nodes, port) = three("schema");
    let schema = "create collection orders (ref text @hash, status text); \
                  create statement paid($order text from event.order) on hook stripe as \
                  set orders {status: 'paid'} where ref = $order require 1";
    let mut body = String::from(r#"{"node":"n1","schema":"#);
    fennec_core::json::escape_into(&mut body, schema);
    body.push('}');
    let (status, answer, _) = call(port, "PUT", "/_shard/tenants/acme", &[], &body);
    assert_eq!(status, 201, "{answer}");
    assert_eq!(
        query(port, "acme", "put orders {ref: 'o-1', status: 'open'}").0,
        200
    );
    let (status, body, _) = event(port, "acme", "stripe", "whsec_schema", "evt_1", "o-1");
    assert_eq!(status, 401, "no secret set yet: {body}");
    assert_eq!(set_hook(port, "acme", "stripe", "whsec_schema").0, 200);
    let (status, body, _) = event(port, "acme", "stripe", "whsec_schema", "evt_1", "o-1");
    assert_eq!(status, 200, "{body}");
    let (_, body) = query(port, "acme", "get orders count where status = 'paid'");
    assert!(body.contains(r#""count":1"#), "{body}");
}

/// A node as the router reaches it, through a link the test cuts: every
/// connection then ended as it is taken, and the ones open shut.
struct Link {
    addr: String,
    cut: Arc<AtomicBool>,
    open: Arc<Mutex<Vec<TcpStream>>>,
}

impl Link {
    fn to(addr: &str) -> Link {
        let target = addr.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let cut = Arc::new(AtomicBool::new(false));
        let open = Arc::new(Mutex::new(Vec::new()));
        let (flag, streams) = (Arc::clone(&cut), Arc::clone(&open));
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { continue };
                if flag.load(Ordering::SeqCst) {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                }
                let Ok(server) = TcpStream::connect(&target) else {
                    continue;
                };
                let mut held = streams.lock().unwrap();
                held.push(client.try_clone().unwrap());
                held.push(server.try_clone().unwrap());
                drop(held);
                let (a, b) = (client.try_clone().unwrap(), server.try_clone().unwrap());
                std::thread::spawn(move || pipe(a, b));
                std::thread::spawn(move || pipe(server, client));
            }
        });
        Link { addr, cut, open }
    }

    fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        for s in self.open.lock().unwrap().drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    fn heal(&self) {
        self.cut.store(false, Ordering::SeqCst);
    }
}

fn pipe(mut from: TcpStream, mut to: TcpStream) {
    let _ = std::io::copy(&mut from, &mut to);
    let _ = to.shutdown(Shutdown::Write);
}

/// A copy a failover could promote that does not take the set is named in
/// the `PUT`'s answer and counted, never a 2xx hiding it, until the
/// repair copies the set from one holding its version.
#[test]
fn a_copy_behind_is_named_counted_and_repaired() {
    let n1 = node("behind1", None);
    let n2 = node("behind2", None);
    let link = Link::to(&n2.addr);
    let port = router(true);
    register(port, "n1", &n1, "adm-behind1", None);
    let body = format!(r#"{{"addr":"{}","token":"adm-behind2"}}"#, link.addr);
    let (status, body, _) = call(port, "PUT", "/_shard/nodes/n2", &[], &body);
    assert_eq!(status, 201, "{body}");
    create(port, "acme", "n1");
    assert_eq!(placed(port, "acme").1.as_deref(), Some("n2"));

    link.cut();
    let (status, body) = set_hook(port, "acme", "stripe", "whsec_behind");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""behind":["n2"]"#), "{body}");
    let (_, listing, _) = call(port, "GET", "/_shard/tenants/acme/hooks", &[], "");
    assert!(listing.contains(r#""behind":["n2"]"#), "{listing}");
    let (_, scrape, _) = call(port, "GET", "/_metrics", &[], "");
    assert!(
        scrape.contains("\nfennec_router_tenants_hooks_behind 1\n"),
        "{scrape}"
    );
    assert_eq!(version_on(&n2, "acme"), None);

    // Reached again, the repair copies the set from the primary -- one
    // asked here, or the one the failure scheduled, which holds the
    // tenant's claim while it runs (a repair meeting it is 502, asked
    // again).
    link.heal();
    let deadline = Instant::now() + Duration::from_secs(30);
    while version_on(&n2, "acme") != version_on(&n1, "acme") {
        assert!(Instant::now() < deadline, "the repair never copied the set");
        let (status, body, _) = call(port, "POST", "/_shard/replicas", &[], "");
        assert!(status == 200 || status == 502, "{body}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, scrape, _) = call(port, "GET", "/_metrics", &[], "");
    assert!(
        scrape.contains("\nfennec_router_tenants_hooks_behind 0\n"),
        "{scrape}"
    );
    let (_, listing, _) = call(port, "GET", "/_shard/tenants/acme/hooks", &[], "");
    assert!(listing.contains(r#""behind":[]"#), "{listing}");
}

/// A node as a router reaches it, through a gate the test closes on a
/// request line's part: each request is read whole as it comes, one after
/// another on a connection -- a pooled one's included -- and one naming
/// that part is held until the gate opens, the test told it is there, or
/// its connection ended unsent where the gate cuts it; every other goes on
/// to the node. What holds the router in the middle of a call, its claim
/// with it: a move held at its install.
pub(crate) struct Gate {
    pub(crate) addr: String,
    state: Arc<(Mutex<Gating>, Condvar)>,
}

#[derive(Default)]
struct Gating {
    /// What a request line holds to be held, and whether one is held.
    hold: Option<String>,
    held: bool,
    /// What a request line holds to have its connection ended unsent.
    cut: Option<String>,
}

impl Gate {
    pub(crate) fn to(target: &str) -> Gate {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let state = Arc::new((Mutex::new(Gating::default()), Condvar::new()));
        let (target, shared) = (target.to_string(), Arc::clone(&state));
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { continue };
                let (target, state) = (target.clone(), Arc::clone(&shared));
                std::thread::spawn(move || {
                    let _ = Gate::carry(client, &target, &state);
                });
            }
        });
        Gate { addr, state }
    }

    /// One connection's requests, each read whole and held where the gate
    /// says, the node's answers carried back as they come.
    fn carry(
        client: TcpStream,
        target: &str,
        state: &(Mutex<Gating>, Condvar),
    ) -> std::io::Result<()> {
        let mut server = TcpStream::connect(target)?;
        let (back, to) = (server.try_clone()?, client.try_clone()?);
        std::thread::spawn(move || pipe(back, to));
        let mut from = BufReader::new(client);
        loop {
            let mut head = Vec::new();
            loop {
                let mut line = Vec::new();
                if from.read_until(b'\n', &mut line)? == 0 {
                    let _ = server.shutdown(Shutdown::Write);
                    return Ok(());
                }
                head.extend_from_slice(&line);
                if line == b"\r\n" {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&head).into_owned();
            let len = (text.lines())
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())?
                })
                .unwrap_or(0);
            let mut body = vec![0; len];
            from.read_exact(&mut body)?;
            let first = text.lines().next().unwrap_or("").to_string();
            let (lock, wake) = state;
            let mut g = lock.lock().unwrap();
            if g.cut.as_deref().is_some_and(|p| first.contains(p)) {
                let _ = server.shutdown(Shutdown::Both);
                return Ok(());
            }
            if g.hold.as_deref().is_some_and(|p| first.contains(p)) {
                g.held = true;
                wake.notify_all();
                while g.hold.as_deref().is_some_and(|p| first.contains(p)) {
                    g = wake.wait(g).unwrap();
                }
                g.held = false;
            }
            drop(g);
            server.write_all(&head)?;
            server.write_all(&body)?;
        }
    }

    /// Holds every request whose line holds `part` from now.
    pub(crate) fn close(&self, part: &str) {
        self.state.0.lock().unwrap().hold = Some(part.to_string());
    }

    /// Ends unsent every request whose line holds `part` from now.
    fn cut(&self, part: &str) {
        self.state.0.lock().unwrap().cut = Some(part.to_string());
    }

    /// Waits until a request the gate holds is there: bounded, as only
    /// that wait is.
    pub(crate) fn held(&self) {
        let (lock, wake) = &*self.state;
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut g = lock.lock().unwrap();
        while !g.held {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "nothing reached the gate");
            g = wake.wait_timeout(g, left).unwrap().0;
        }
    }

    pub(crate) fn open(&self) {
        let (lock, wake) = &*self.state;
        let mut g = lock.lock().unwrap();
        (g.hold, g.cut) = (None, None);
        wake.notify_all();
    }
}

/// A move holds its tenant's claim from its freeze to its flip: a hook's
/// or a quota's change through the router meanwhile is refused (409),
/// never landing on the frozen source between the copy of its set and the
/// flip, where the target would come up without it -- and the source
/// itself refuses either while frozen, asked past the router. The set the
/// move carried is the one the target holds.
#[test]
fn a_hook_or_a_quota_set_while_its_tenant_moves_is_refused() {
    let n1 = node("moving1", None);
    let n2 = node("moving2", None);
    let gate = Gate::to(&n2.addr);
    let port = router(false);
    register(port, "n1", &n1, "adm-moving1", None);
    let body = format!(r#"{{"addr":"{}","token":"adm-moving2"}}"#, gate.addr);
    let (status, body, _) = call(port, "PUT", "/_shard/nodes/n2", &[], &body);
    assert_eq!(status, 201, "{body}");
    create(port, "acme", "n1");
    assert_eq!(set_hook(port, "acme", "stripe", "whsec_before").0, 200);
    let held = version_on(&n1, "acme");

    gate.close("/_admin/tenants/acme/file ");
    let moving = std::thread::spawn(move || {
        call(
            port,
            "POST",
            "/_shard/tenants/acme/move",
            &[],
            r#"{"to":"n2"}"#,
        )
    });
    gate.held();
    let (status, body) = set_hook(port, "acme", "stripe", "whsec_during");
    assert_eq!(status, 409, "{body}");
    let (status, body, _) = call(
        port,
        "PUT",
        "/_shard/tenants/acme/quota",
        &[],
        r#"{"rows":5}"#,
    );
    assert_eq!(status, 409, "{body}");
    let admin = [("Authorization", "Bearer adm-moving1")];
    for (target, body) in [
        (
            "/_admin/tenants/acme/hooks/stripe",
            r#"{"scheme":"stripe","secrets":["whsec_during"]}"#,
        ),
        ("/_admin/tenants/acme/quota", r#"{"rows":5}"#),
    ] {
        let (status, answer, _) = call(n1.port, "PUT", target, &admin, body);
        assert_eq!(status, 409, "{target}: {answer}");
        assert!(answer.contains("frozen"), "{answer}");
    }
    gate.open();
    let (status, body, _) = moving.join().unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(version_on(&n2, "acme"), held, "the set the move carried");
    let (status, body, _) = event(port, "acme", "stripe", "whsec_before", "evt_1", "o-1");
    assert_eq!(status, 200, "{body}");
    let (status, body, _) = event(port, "acme", "stripe", "whsec_during", "evt_2", "o-2");
    assert_eq!(status, 401, "{body}");
}

/// A copy that lost its set with no record saying so -- a node putting it
/// away as it started, its binding lost to a crash between a compact's
/// rename and its tick -- is found by the repair, which asks each copy the
/// version it lists: recorded behind and counted, then copied the set
/// from its primary.
#[test]
fn a_copy_that_lost_its_set_unrecorded_is_found_and_repaired() {
    let n1 = node("lost1", None);
    let n2 = node("lost2", None);
    let gate = Gate::to(&n2.addr);
    let port = router(true);
    register(port, "n1", &n1, "adm-lost1", None);
    let body = format!(r#"{{"addr":"{}","token":"adm-lost2"}}"#, gate.addr);
    let (status, body, _) = call(port, "PUT", "/_shard/nodes/n2", &[], &body);
    assert_eq!(status, 201, "{body}");
    create(port, "acme", "n1");
    assert_eq!(placed(port, "acme").1.as_deref(), Some("n2"));
    let (status, body) = set_hook(port, "acme", "stripe", "whsec_lost");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""behind":[]"#), "{body}");
    let held = version_on(&n1, "acme");
    assert_eq!(version_on(&n2, "acme"), held);

    let admin = [("Authorization", "Bearer adm-lost2")];
    let (status, body, _) = call(
        n2.port,
        "DELETE",
        "/_admin/tenants/acme/hooks/stripe",
        &admin,
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(version_on(&n2, "acme"), None);
    gate.close("PUT /_admin/tenants/acme/hook-secrets ");
    let repairing = std::thread::spawn(move || call(port, "POST", "/_shard/replicas", &[], ""));
    gate.held();
    let (_, scrape, _) = call(port, "GET", "/_metrics", &[], "");
    assert!(
        scrape.contains("\nfennec_router_tenants_hooks_behind 1\n"),
        "{scrape}"
    );
    let (_, listing, _) = call(port, "GET", "/_shard/tenants/acme/hooks", &[], "");
    assert!(listing.contains(r#""behind":["n2"]"#), "{listing}");
    gate.open();
    let (status, body, _) = repairing.join().unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""hook_secrets":"n2""#), "{body}");
    assert_eq!(version_on(&n2, "acme"), held);
    let (_, scrape, _) = call(port, "GET", "/_metrics", &[], "");
    assert!(
        scrape.contains("\nfennec_router_tenants_hooks_behind 0\n"),
        "{scrape}"
    );
}

/// A set its primary took that cannot be read back records every copy a
/// failover could promote behind -- named and counted, the primary's own
/// word giving the version -- never a 502 that records nothing, those
/// copies then promoted without the set no one knows they lack; the
/// repair copies it once the primary answers.
#[test]
fn a_set_not_read_back_records_every_copy_behind() {
    let n1 = node("unread1", None);
    let n2 = node("unread2", None);
    let gate = Gate::to(&n1.addr);
    let port = router(true);
    let body = format!(r#"{{"addr":"{}","token":"adm-unread1"}}"#, gate.addr);
    let (status, body, _) = call(port, "PUT", "/_shard/nodes/n1", &[], &body);
    assert_eq!(status, 201, "{body}");
    register(port, "n2", &n2, "adm-unread2", None);
    create(port, "acme", "n1");
    assert_eq!(placed(port, "acme").1.as_deref(), Some("n2"));

    gate.cut("GET /_admin/tenants/acme/hook-secrets ");
    let (status, body) = set_hook(port, "acme", "stripe", "whsec_unread");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""behind":["n2"]"#), "{body}");
    let held = version_on(&n1, "acme").expect("the primary took it");
    let version = format!(r#""version":"{held:016x}""#);
    assert!(body.contains(&version), "{body}");
    let (_, scrape, _) = call(port, "GET", "/_metrics", &[], "");
    assert!(
        scrape.contains("\nfennec_router_tenants_hooks_behind 1\n"),
        "{scrape}"
    );
    assert_eq!(version_on(&n2, "acme"), None);
    gate.open();
    let deadline = Instant::now() + Duration::from_secs(30);
    while version_on(&n2, "acme") != Some(held) {
        assert!(Instant::now() < deadline, "the repair never copied the set");
        let (status, body, _) = call(port, "POST", "/_shard/replicas", &[], "");
        assert!(status == 200 || status == 502, "{body}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
