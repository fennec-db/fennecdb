//! Webhook consumers on a node of tenants (`docs/design/named-statements.md`,
//! F4's slice 4, *Where the secret lives*): each tenant's secrets set
//! through the node's admin routes and kept beside its file, never in it;
//! an event to `/t/<tenant>/_hooks/<name>` verified against its own
//! tenant's secrets before the tenant is looked up, a forged one opening
//! none; `<tenant>.hooks` bound to its file, put away before every way a
//! file is made, replaced or followed, kept by a promotion, never arming a
//! later tenant of the name; and a read replica's stream no standby a
//! hook's answer waits for.

use fennec_http::crypto::hmac_sha256;
use fennec_http::tenants::{Replicated, Tenants};
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ADMIN: &str = "Bearer adm";
const REPL: &str = "repl";

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
        "fennec-tenant-hooks-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A node of tenants, its admin token `adm`, every refusal waiting
/// nothing; replicating its tenants where `replicated`.
fn node(tag: &str, replicated: bool) -> Node {
    fennec_http::audit::set_delay(0);
    let dir = scratch(tag);
    let mut tenants = Tenants::new(&dir).expect("dir");
    if replicated {
        tenants = tenants.with_replication(Replicated {
            token: REPL.into(),
            buffer: 1 << 20,
            upstream: None,
            sync_on_write: true,
        });
    }
    let tenants = Arc::new(tenants);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        sync_on_write: replicated,
        ..Config::default()
    };
    let server = Server::with_tenants(Arc::clone(&tenants), cfg);
    let listener = server.bind().expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { port, tenants, dir }
}

struct Res {
    status: u16,
    head: String,
    body: String,
}

fn call(port: u16, method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!(
        "{method} {target} HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let split = (out.windows(4))
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&out[..split]).into_owned();
    let status = (head.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Res {
        status,
        head,
        body: String::from_utf8_lossy(&out[split + 4..]).into_owned(),
    }
}

fn admin(port: u16, method: &str, target: &str, body: &str) -> Res {
    call(
        port,
        method,
        target,
        &[("Authorization", ADMIN)],
        body.as_bytes(),
    )
}

fn query(port: u16, tenant: &str, q: &str) -> Res {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    call(
        port,
        "POST",
        &format!("/t/{tenant}/query"),
        &[],
        body.as_bytes(),
    )
}

/// A tenant with an open order `o-1` and a statement taking hook `stripe`.
fn tenant(port: u16, name: &str) {
    let r = admin(port, "PUT", &format!("/_admin/tenants/{name}"), "");
    assert_eq!(r.status, 201, "{}", r.body);
    for q in [
        "create collection orders (ref text @hash, status text)",
        "put orders {ref: 'o-1', status: 'open'}",
        "create statement paid($order text from event.order) on hook stripe as set orders \
         {status: 'paid'} where ref = $order and status = 'open' require 1",
    ] {
        let r = query(port, name, q);
        assert_eq!(r.status, 200, "{q}: {}", r.body);
    }
}

fn set_secret(port: u16, tenant: &str, secret: &str) -> Res {
    admin(
        port,
        "PUT",
        &format!("/_admin/tenants/{tenant}/hooks/stripe"),
        &format!(r#"{{"scheme":"stripe","secrets":["{secret}"],"tolerance":"5m"}}"#),
    )
}

/// Event `id` for order `order`, signed as Stripe signs one under `secret`
/// and sent to `path`.
fn event(port: u16, path: &str, secret: &str, id: &str, order: &str) -> Res {
    let body = format!(r#"{{"id":"{id}","order":"{order}"}}"#);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac = hmac_sha256(secret.as_bytes(), format!("{t}.{body}").as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    let sig = format!("t={t},v1={hex}");
    call(
        port,
        "POST",
        path,
        &[
            ("Content-Type", "application/json"),
            ("Stripe-Signature", &sig),
        ],
        body.as_bytes(),
    )
}

fn status_of(port: u16, tenant: &str) -> String {
    query(port, tenant, "get orders select status where ref = 'o-1'").body
}

fn is_open(n: &Node, tenant: &str) -> bool {
    (n.tenants.stats().open.iter()).any(|(name, _, _)| name == tenant)
}

/// Each tenant's events verify by its own secrets alone: one tenant's
/// secret signs no event another takes, whatever hook it names. Every
/// refusal before a verified signature is the one 401 -- another tenant's
/// secret, an unknown hook, an unknown tenant -- answered byte for byte
/// alike, and none opens a tenant: a forged event's verification asks the
/// node's memory alone.
#[test]
fn each_tenant_verifies_its_events_by_its_own_secrets() {
    let n = node("own", false);
    tenant(n.port, "acme");
    tenant(n.port, "beta");
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    assert_eq!(set_secret(n.port, "beta", "whsec_beta").status, 200);
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 2);
    assert!(!is_open(&n, "acme"));

    let refused = [
        event(
            n.port,
            "/t/acme/_hooks/stripe",
            "whsec_beta",
            "evt_1",
            "o-1",
        ),
        event(
            n.port,
            "/t/acme/_hooks/nosuch",
            "whsec_acme",
            "evt_1",
            "o-1",
        ),
        event(
            n.port,
            "/t/nobody/_hooks/stripe",
            "whsec_acme",
            "evt_1",
            "o-1",
        ),
        event(n.port, "/t/acme/_hooks/", "whsec_acme", "evt_1", "o-1"),
    ];
    for r in &refused {
        assert_eq!(r.status, 401, "{}", r.body);
        assert_eq!(r.body, refused[0].body);
        assert!(
            !r.head.to_ascii_lowercase().contains("www-authenticate"),
            "{}",
            r.head
        );
    }
    assert!(!is_open(&n, "acme"), "a forged event opened its tenant");
    assert!(!is_open(&n, "nobody"));
    assert!(status_of(n.port, "acme").contains("open"));

    let taken = event(
        n.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(taken.status, 200, "{}", taken.body);
    assert_eq!(taken.body, r#"{"event":"evt_1","taken":1}"#);
    assert!(status_of(n.port, "acme").contains("paid"));
    assert!(
        status_of(n.port, "beta").contains("open"),
        "beta's order untouched"
    );
    let again = event(
        n.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(again.status, 200, "{}", again.body);
    assert!(
        again.head.contains("Idempotent-Replayed: true"),
        "{}",
        again.head
    );
    // Beta's own, by its own secret.
    let beta = event(
        n.port,
        "/t/beta/_hooks/stripe",
        "whsec_beta",
        "evt_1",
        "o-1",
    );
    assert_eq!(beta.status, 200, "{}", beta.body);
    // Every method but POST, every path alike: 405.
    let get = call(n.port, "GET", "/t/acme/_hooks/stripe", &[], b"");
    assert_eq!(get.status, 405);
    assert!(get.head.contains("Allow: POST"), "{}", get.head);
}

/// The admin routes: a hook's `PUT` refuses (400) what a hooks file is
/// refused at a server's start, naming the field and never its value; the
/// listing holds no secret; `hook-secrets` answers the set whole to the
/// admin token alone, never to a client's; a `DELETE` takes a hook away,
/// the last one its sidecar; and an installed set keeps its version.
#[test]
fn a_tenants_secrets_are_set_listed_and_exported_by_its_admin_alone() {
    let n = node("admin", false);
    tenant(n.port, "acme");
    let put = |body: &str| admin(n.port, "PUT", "/_admin/tenants/acme/hooks/stripe", body);
    for (body, says) in [
        (r#"{"scheme":"stripe","secrets":[""]}"#, "empty"),
        (r#"{"scheme":"stripe","secrets":["\n"]}"#, "empty"),
        (
            r#"{"scheme":"standard","secrets":["not-whsec-at-all"]}"#,
            "Standard Webhooks",
        ),
        (r#"{"scheme":"stripe","secrets":[]}"#, "one secret"),
        (
            r#"{"scheme":"stripe","secrets":["a","b","c"]}"#,
            "one secret",
        ),
        (
            r#"{"scheme":"stripe","secrets":["whsec_x"],"keep":"1h"}"#,
            "keep",
        ),
        (
            r#"{"scheme":"stripe","secrets":["whsec_x"],"tolerance":"2h"}"#,
            "tolerance",
        ),
        (
            r#"{"scheme":"stripe","secrets":["whsec_x"],"key":"delivery"}"#,
            "github",
        ),
        (r#"{"scheme":"nosuch","secrets":["whsec_x"]}"#, "scheme"),
        (
            r#"{"scheme":"stripe","secrets":["whsec_x"],"color":"red"}"#,
            "color",
        ),
    ] {
        let r = put(body);
        assert_eq!(r.status, 400, "{body}: {}", r.body);
        assert!(r.body.contains(says), "{body}: {}", r.body);
        assert!(
            !r.body.contains("not-whsec") && !r.body.contains("red"),
            "{}",
            r.body
        );
    }
    assert!(
        !n.dir.join("acme.hooks").exists(),
        "nothing refused was kept"
    );

    const SECRET: &str = "a word its user chose";
    let made = admin(
        n.port,
        "PUT",
        "/_admin/tenants/acme/hooks/pushes",
        &format!(r#"{{"scheme":"github","secrets":["{SECRET}"],"key":"delivery"}}"#),
    );
    assert_eq!(made.status, 200, "{}", made.body);
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    let listing = admin(n.port, "GET", "/_admin/tenants/acme/hooks", "");
    assert_eq!(listing.status, 200);
    for part in [
        r#""name":"pushes""#,
        r#""key":"delivery""#,
        r#""name":"stripe""#,
    ] {
        assert!(listing.body.contains(part), "{part}: {}", listing.body);
    }
    assert!(!listing.body.contains(SECRET) && !listing.body.contains("whsec_acme"));
    let sidecar = std::fs::read_to_string(n.dir.join("acme.hooks")).unwrap();
    assert!(
        !sidecar.contains(SECRET),
        "the sidecar holds the secrets' bytes in base64"
    );

    // A client's way in reaches no secret: the admin routes take the admin
    // token alone.
    for auth in [None, Some("Bearer data")] {
        let headers: Vec<(&str, &str)> = auth.map(|a| ("Authorization", a)).into_iter().collect();
        let r = call(
            n.port,
            "GET",
            "/_admin/tenants/acme/hook-secrets",
            &headers,
            b"",
        );
        assert_eq!(r.status, 401, "{}", r.body);
        assert!(!r.body.contains("whsec_acme"));
    }
    let export = admin(n.port, "GET", "/_admin/tenants/acme/hook-secrets", "");
    assert_eq!(export.status, 200);
    assert!(
        export.head.contains("Cache-Control: no-store"),
        "{}",
        export.head
    );
    assert!(export.body.contains("whsec_acme") && export.body.contains(SECRET));

    // Installed whole on another tenant, its version kept.
    tenant(n.port, "copy");
    let installed = admin(
        n.port,
        "PUT",
        "/_admin/tenants/copy/hook-secrets",
        &export.body,
    );
    assert_eq!(installed.status, 200, "{}", installed.body);
    let version = |t: &str| n.tenants.hooks().version(t);
    assert_eq!(version("copy"), version("acme"));
    assert!(version("acme").is_some());
    let taken = event(
        n.port,
        "/t/copy/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(taken.status, 200, "{}", taken.body);

    // Taken away: a hook not held is 404; the last one takes the sidecar.
    let gone = admin(n.port, "DELETE", "/_admin/tenants/acme/hooks/nosuch", "");
    assert_eq!(gone.status, 404, "{}", gone.body);
    let one = admin(n.port, "DELETE", "/_admin/tenants/acme/hooks/pushes", "");
    assert_eq!(one.status, 200, "{}", one.body);
    let last = admin(n.port, "DELETE", "/_admin/tenants/acme/hooks/stripe", "");
    assert_eq!(last.status, 200, "{}", last.body);
    assert!(last.body.contains(r#""version":null"#), "{}", last.body);
    assert!(!n.dir.join("acme.hooks").exists());
    let refused = event(
        n.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_2",
        "o-1",
    );
    assert_eq!(refused.status, 401);
    // A tenant the node has no file of has no secrets to set.
    let none = set_secret(n.port, "nobody", "whsec_x");
    assert_eq!(none.status, 404, "{}", none.body);
}

/// A set a tenant left never arms a later tenant of its name: a delete
/// takes it with the file, a create puts away one left beside the place,
/// and one naming another file than the tenant's -- a file replaced while
/// the node was down -- is put away as the node starts and never read;
/// a restore puts away the one beside the file it replaces.
#[test]
fn a_set_left_behind_never_arms_a_later_tenant_of_the_name() {
    let n = node("left", false);
    tenant(n.port, "acme");
    assert_eq!(set_secret(n.port, "acme", "whsec_first_owner").status, 200);
    let sidecar = n.dir.join("acme.hooks");
    let left = std::fs::read(&sidecar).unwrap();
    let r = admin(n.port, "DELETE", "/_admin/tenants/acme", "");
    assert_eq!(r.status, 204, "{}", r.body);
    assert!(!sidecar.exists(), "a delete takes the set with the file");
    // Left by a failed remove or a crash between the two removals: a
    // create of the name puts it away before it makes the file.
    std::fs::write(&sidecar, &left).unwrap();
    tenant(n.port, "acme");
    assert!(
        !sidecar.exists(),
        "a create puts a set left beside the place away"
    );
    let refused = event(
        n.port,
        "/t/acme/_hooks/stripe",
        "whsec_first_owner",
        "e1",
        "o-1",
    );
    assert_eq!(refused.status, 401, "{}", refused.body);
    assert!(status_of(n.port, "acme").contains("open"));

    // The second owner's set, then the node down and the file replaced
    // beside it by hand: the node starting again reads no set naming
    // another file, and puts it away.
    assert_eq!(set_secret(n.port, "acme", "whsec_second_owner").status, 200);
    let dir = n.dir.clone();
    n.tenants.shutdown();
    let file = dir.join("acme.fennec");
    let aside = dir.join("acme.aside");
    std::fs::copy(&file, &aside).unwrap();
    std::fs::rename(&aside, &file).unwrap();
    let again = Tenants::new(&dir).unwrap();
    assert_eq!(again.hooks().version("acme"), None);
    assert!(!sidecar.exists(), "put away as the node started");
    drop(again);

    // A restore puts away the set beside the file it replaces.
    std::fs::write(&sidecar, &left).unwrap();
    fennec_http::tenant_hooks::put_away_beside(&file).unwrap();
    assert!(!sidecar.exists());
    drop(n);
}

/// A compact of the node's own makes another file of the tenant's, and the
/// node's tick carries the set to it: a crash before the tenant's close
/// leaves the set bound to the file it is, which the next start keeps.
#[test]
fn a_compact_of_the_nodes_own_carries_the_set_to_the_file_it_made() {
    let n = node("compact", false);
    tenant(n.port, "acme");
    assert_eq!(set_secret(n.port, "acme", "whsec_compact").status, 200);
    let held = n.tenants.hooks().version("acme");
    let sidecar = n.dir.join("acme.hooks");
    let bound = |text: &str| text.lines().next().unwrap().to_string();
    let before = bound(&std::fs::read_to_string(&sidecar).unwrap());
    // Nothing compacted: the tick writes nothing.
    n.tenants.rebind_hooks();
    assert_eq!(bound(&std::fs::read_to_string(&sidecar).unwrap()), before);
    let r = query(n.port, "acme", "compact");
    assert_eq!(r.status, 200, "{}", r.body);
    n.tenants.rebind_hooks();
    let after = bound(&std::fs::read_to_string(&sidecar).unwrap());
    assert_ne!(after, before, "bound to the file the compact made");
    // A start over the directory, as after a crash with the tenant open.
    let again = Tenants::new(&n.dir).unwrap();
    assert_eq!(again.hooks().version("acme"), held);
    assert!(sidecar.exists());
    let taken = event(
        n.port,
        "/t/acme/_hooks/stripe",
        "whsec_compact",
        "evt_1",
        "o-1",
    );
    assert_eq!(taken.status, 200, "{}", taken.body);
}

/// A follow drops a copy's set -- it follows another primary's file now
/// -- and the router sends it the set again; a promotion keeps the set the
/// copy holds, its events taken there.
#[test]
fn a_follow_drops_the_set_and_a_promotion_keeps_it() {
    let primary = node("primary", true);
    let copy = node("copy", true);
    tenant(primary.port, "acme");
    assert_eq!(set_secret(primary.port, "acme", "whsec_acme").status, 200);
    let made = admin(copy.port, "PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    let set = admin(primary.port, "GET", "/_admin/tenants/acme/hook-secrets", "").body;
    assert_eq!(
        admin(copy.port, "PUT", "/_admin/tenants/acme/hook-secrets", &set).status,
        200
    );
    assert!(copy.dir.join("acme.hooks").exists());

    let from = format!(r#"{{"from":"http://127.0.0.1:{}"}}"#, primary.port);
    let followed = admin(copy.port, "POST", "/_admin/tenants/acme/follow", &from);
    assert_eq!(followed.status, 200, "{}", followed.body);
    assert_eq!(
        copy.tenants.hooks().version("acme"),
        None,
        "a follow drops the set"
    );
    assert!(!copy.dir.join("acme.hooks").exists());
    // Sent again, as a router sends it once the copy follows.
    assert_eq!(
        admin(copy.port, "PUT", "/_admin/tenants/acme/hook-secrets", &set).status,
        200
    );
    let held = primary.tenants.hooks().version("acme");
    assert_eq!(copy.tenants.hooks().version("acme"), held);

    // The primary takes an event; the copy holds its block once it has
    // caught up.
    let taken = event(
        primary.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(taken.status, 200, "{}", taken.body);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let t = copy.tenants.get("acme").unwrap();
        let caught = t.ready() && {
            let q = fennec_ql::parse_one("get orders count where status = 'paid'").unwrap();
            let r = t.db.read().unwrap().query(&q, &[]).unwrap();
            fennec_core::json::response_to_string(&r).contains(r#""count":1"#)
        };
        if caught {
            break;
        }
        assert!(Instant::now() < deadline, "the copy never caught up");
        std::thread::sleep(Duration::from_millis(20));
    }
    let promoted = admin(copy.port, "POST", "/_admin/tenants/acme/promote", "");
    assert_eq!(promoted.status, 200, "{}", promoted.body);
    assert_eq!(
        copy.tenants.hooks().version("acme"),
        held,
        "a promotion keeps the set"
    );
    // The event sent again to the promoted copy: answered from its key,
    // which came with the block.
    let again = event(
        copy.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(again.status, 200, "{}", again.body);
    assert!(
        again.head.contains("Idempotent-Replayed: true"),
        "{}",
        again.head
    );
}

/// A read replica's copy says so on its stream, kept in `.follows`: the
/// primary's feed counts it no standby, so a hook's 2xx never waits on a
/// copy no failover promotes, while a standby's copy is one it waits for.
#[test]
fn a_read_replicas_stream_is_no_standby() {
    let primary = node("readp", true);
    let copy = node("readc", true);
    tenant(primary.port, "acme");
    assert_eq!(
        admin(copy.port, "PUT", "/_admin/tenants/acme", "").status,
        201
    );
    let feed = || {
        let t = primary.tenants.get("acme").unwrap();
        let feed = Arc::clone(t.repl.as_ref().unwrap().feed().unwrap());
        feed
    };
    let follow = |reader: bool| {
        let body = format!(
            r#"{{"from":"http://127.0.0.1:{}","reader":{reader}}}"#,
            primary.port
        );
        let r = admin(copy.port, "POST", "/_admin/tenants/acme/follow", &body);
        assert_eq!(r.status, 200, "{}", r.body);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !copy.tenants.get("acme").unwrap().ready() {
            assert!(
                Instant::now() < deadline,
                "the copy never heard its primary"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    follow(true);
    let follows = std::fs::read_to_string(copy.dir.join("acme.follows")).unwrap();
    assert!(follows.lines().any(|l| l == "reader"), "{follows}");
    assert!(
        feed().standby_sent(u64::MAX, Duration::ZERO),
        "a read replica's stream is no standby"
    );
    follow(false);
    let follows = std::fs::read_to_string(copy.dir.join("acme.follows")).unwrap();
    assert!(!follows.lines().any(|l| l == "reader"), "{follows}");
    assert!(
        !feed().standby_sent(u64::MAX, Duration::ZERO),
        "a standby's stream is waited for"
    );
}
