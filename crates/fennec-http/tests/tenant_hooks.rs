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
        "fennec-tenant-hooks-{tag}-{}-{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A node of tenants, its admin token `adm`, every refusal waiting
/// nothing; replicating its tenants where `replicated`.
fn node(tag: &str, replicated: bool) -> Node {
    serve(scratch(tag), replicated)
}

/// A node as [`node`] makes one, over `dir`: started again over the
/// directory a node shut down left.
fn serve(dir: PathBuf, replicated: bool) -> Node {
    fennec_http::audit::set_delay(0);
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
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
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

/// `body`, signed as GitHub signs a delivery under `secret`, sent to
/// `path` as a `push` under one `X-GitHub-Delivery`.
fn github(port: u16, path: &str, secret: &str, body: &str) -> Res {
    let mac = hmac_sha256(secret.as_bytes(), body.as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    let sig = format!("sha256={hex}");
    call(
        port,
        "POST",
        path,
        &[
            ("Content-Type", "application/json"),
            ("X-Hub-Signature-256", &sig),
            ("X-GitHub-Event", "push"),
            ("X-GitHub-Delivery", "72d3162e-cc78-11e3-81ab-4c9367dc0958"),
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

/// What `tenant`'s `statements`, `describe statement` and `GET /_schema`
/// each say of `statement`'s `armed`, the last field of its entry: `None`
/// where one does not list it or says neither.
fn armed_in(port: u16, tenant: &str, statement: &str) -> [Option<bool>; 3] {
    let of = |body: &str| {
        let at = body.find(&format!(r#""name":"{statement}""#))?;
        let rest = &body[at..];
        let value = &rest[rest.find(r#""armed":"#)? + r#""armed":"#.len()..];
        match () {
            _ if value.starts_with("true") => Some(true),
            _ if value.starts_with("false") => Some(false),
            _ => None,
        }
    };
    [
        of(&query(port, tenant, "statements").body),
        of(&query(port, tenant, &format!("describe statement {statement}")).body),
        of(&call(port, "GET", &format!("/t/{tenant}/_schema"), &[], b"").body),
    ]
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

/// What a tenant's listings say of its hooks (`docs/design/named-statements.md`,
/// *Watching hooks*): its `statements` and `GET /_schema` say whether the
/// node holds a secret for each statement's hook, asked as each is written
/// -- a `PUT` arms it while the tenant stays open, a `DELETE` disarms it.
/// What an event ran is counted in `tests/hook_counts.rs`, a binary of its
/// own: it reads the process's counts.
#[test]
fn a_tenants_listings_say_whether_its_hook_is_armed() {
    let n = node("armed", false);
    tenant(n.port, "acme");
    let armed = |q: &str| {
        let r = query(n.port, "acme", q);
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(r.body.contains(r#""hook":"stripe""#), "{}", r.body);
        r.body.contains(r#""armed":true"#)
    };
    assert!(!armed("statements"));
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    assert!(is_open(&n, "acme"));
    assert!(armed("statements"));
    assert!(armed("describe statement paid"));
    let schema = call(n.port, "GET", "/t/acme/_schema", &[], b"");
    assert!(
        schema
            .body
            .contains(r#""hooks":[{"name":"paid","hook":"stripe","#)
            && schema.body.contains(r#""armed":true}]"#),
        "{}",
        schema.body
    );
    assert!(!schema.body.contains("whsec_acme"), "{}", schema.body);

    let gone = admin(n.port, "DELETE", "/_admin/tenants/acme/hooks/stripe", "");
    assert_eq!(gone.status, 200, "{}", gone.body);
    assert!(!armed("statements"));
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
            r#"{"scheme":"stripe","secrets":["whsec_x"],"key":"body"}"#,
            "github",
        ),
        (
            r#"{"scheme":"github","secrets":["x"],"key":"delivery"}"#,
            "does not sign X-GitHub-Delivery",
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
        &format!(r#"{{"scheme":"github","secrets":["{SECRET}"],"key":"body"}}"#),
    );
    assert_eq!(made.status, 200, "{}", made.body);
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    let listing = admin(n.port, "GET", "/_admin/tenants/acme/hooks", "");
    assert_eq!(listing.status, 200);
    for part in [
        r#""name":"pushes""#,
        r#""key":"body""#,
        r#""name":"stripe""#,
    ] {
        assert!(listing.body.contains(part), "{part}: {}", listing.body);
    }
    assert!(!listing.body.contains(SECRET) && !listing.body.contains("whsec_acme"));

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
    // The sidecar holds the set as the export answers it, under the file it
    // is bound to and its version: one reader for both, readable by the
    // node's user alone.
    let sidecar = std::fs::read_to_string(n.dir.join("acme.hooks")).unwrap();
    let lines: Vec<&str> = sidecar.lines().collect();
    assert_eq!(lines.len(), 3, "{}", lines[..2].join("\n"));
    assert!(lines[0].starts_with("1 ") && lines[1].starts_with("version "));
    assert_eq!(lines[2], export.body);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(n.dir.join("acme.hooks")).unwrap();
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
    }
    // On Windows a protected DACL of the node's user alone, the directory's
    // inherited entries gone (docs/design/windows.md, 7h).
    #[cfg(windows)]
    assert!(fennec_http::owner_only::owner_alone(&n.dir.join("acme.hooks")).unwrap());

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

/// A file put in a closed tenant's place under the running node -- `fennec
/// unseal`, `restore --force` or `backup --force` over it, or a hand -- is
/// armed by none of the secrets the node holds in its memory, which no
/// such tool reaches: an event signed with one verifies against memory
/// before its tenant is looked up, and is the one 401 once the open has
/// put that set away, naming another file; a hook set through the admin
/// routes carries none of the set before; and no close binds the set
/// before to the file that took its place.
#[test]
fn a_file_put_in_place_under_a_running_node_is_armed_by_no_secret_before_it() {
    let n = node("restored", false);
    tenant(n.port, "acme");
    assert_eq!(set_secret(n.port, "acme", "whsec_before").status, 200);
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
    let file = n.dir.join("acme.fennec");
    let sidecar = n.dir.join("acme.hooks");

    // Unsealed over it: the tool puts the sidecar away, the memory holds
    // the set still.
    let key_hex = fennec_http::seal::Key::generate().unwrap();
    let key = fennec_http::seal::Key::from_hex(&key_hex).unwrap();
    let sealed = n.dir.join("acme.sealed");
    let plain = std::fs::read(&file).unwrap();
    std::fs::write(
        &sealed,
        fennec_http::seal::seal_whole(&key, &fennec_http::seal::Binding::backup(), &plain).unwrap(),
    )
    .unwrap();
    fennec_http::archive::unseal(&sealed, &key, &file).unwrap();
    assert!(!sidecar.exists(), "put away beside the file it replaced");
    assert!(
        n.tenants.hooks().version("acme").is_some(),
        "in memory still"
    );
    let r = event(
        n.port,
        "/t/acme/_hooks/stripe",
        "whsec_before",
        "evt_1",
        "o-1",
    );
    assert_eq!(r.status, 401, "{}", r.body);
    assert_eq!(
        r.body,
        event(n.port, "/t/nobody/_hooks/stripe", "x", "e", "o").body
    );
    assert!(status_of(n.port, "acme").contains("open"), "nothing taken");
    assert_eq!(
        n.tenants.hooks().version("acme"),
        None,
        "put away as it opened"
    );
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
    assert!(
        !sidecar.exists(),
        "no close binds the set before to the file"
    );
    assert_eq!(Tenants::new(&n.dir).unwrap().hooks().version("acme"), None);

    // Set again, then the file put in place by hand while the tenant is
    // closed: a hook set through the admin routes carries none of the set
    // before, and its listing and export name no hook of it.
    assert_eq!(set_secret(n.port, "acme", "whsec_second").status, 200);
    assert!(!is_open(&n, "acme"), "a hook's change opens no tenant");
    let aside = n.dir.join("acme.aside");
    std::fs::copy(&file, &aside).unwrap();
    std::fs::rename(&aside, &file).unwrap();
    let made = admin(
        n.port,
        "PUT",
        "/_admin/tenants/acme/hooks/pushes",
        r#"{"scheme":"github","secrets":["a later secret"]}"#,
    );
    assert_eq!(made.status, 200, "{}", made.body);
    let export = admin(n.port, "GET", "/_admin/tenants/acme/hook-secrets", "");
    assert!(export.body.contains("pushes"), "{}", export.body);
    assert!(
        !export.body.contains("stripe") && !export.body.contains("whsec_second"),
        "{}",
        export.body
    );
}

/// A sidecar a crash left half written -- its secrets in no set -- goes as
/// the node starts and with its tenant's set, a delete's included: never
/// kept past them.
#[test]
fn a_sidecar_left_half_written_is_removed() {
    let n = node("writing", false);
    tenant(n.port, "acme");
    let writing = n.dir.join("acme.hooks.writing");
    std::fs::write(&writing, "1 0 0 0\nversion 00000000000000ab\n{}").unwrap();
    let again = Tenants::new(&n.dir).unwrap();
    assert!(!writing.exists(), "removed as the node starts");
    drop(again);
    std::fs::write(&writing, "half").unwrap();
    assert_eq!(set_secret(n.port, "acme", "whsec_writing").status, 200);
    std::fs::write(&writing, "half").unwrap();
    let r = admin(n.port, "DELETE", "/_admin/tenants/acme", "");
    assert_eq!(r.status, 204, "{}", r.body);
    assert!(!writing.exists(), "removed with the set, at a delete");
    std::fs::write(&writing, "half").unwrap();
    fennec_http::tenant_hooks::put_away_beside(&n.dir.join("acme.fennec")).unwrap();
    assert!(!writing.exists(), "and beside a file a restore replaces");
}

/// A tenant holds 64 hooks at most, its set 32 KiB at most written out --
/// so a set always fits the head a move across regions carries it in --
/// each refused (400) naming its bound; and a frozen tenant's hooks and
/// quota are refused a change (409) while its set is still read, as a
/// move's export reads it.
#[test]
fn a_set_is_bounded_and_a_frozen_tenants_takes_no_change() {
    let n = node("bounds", false);
    tenant(n.port, "acme");
    for i in 0..fennec_http::tenant_hooks::MOST_HOOKS {
        let r = admin(
            n.port,
            "PUT",
            &format!("/_admin/tenants/acme/hooks/h{i}"),
            r#"{"scheme":"stripe","secrets":["whsec_x"]}"#,
        );
        assert_eq!(r.status, 200, "{i}: {}", r.body);
    }
    let r = admin(
        n.port,
        "PUT",
        "/_admin/tenants/acme/hooks/one_more",
        r#"{"scheme":"stripe","secrets":["whsec_x"]}"#,
    );
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(r.body.contains("64 hooks at most"), "{}", r.body);
    let long = "s".repeat(fennec_http::tenant_hooks::MOST_BYTES);
    let r = admin(
        n.port,
        "PUT",
        "/_admin/tenants/acme/hooks/h0",
        &format!(r#"{{"scheme":"github","secrets":["{long}"]}}"#),
    );
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(r.body.contains("32 KiB at most"), "{}", r.body);
    assert!(!r.body.contains("sss"), "{}", r.body);

    let r = admin(n.port, "POST", "/_admin/tenants/acme/freeze", "");
    assert_eq!(r.status, 200, "{}", r.body);
    for (method, target, body) in [
        (
            "PUT",
            "/_admin/tenants/acme/hooks/h1",
            r#"{"scheme":"stripe","secrets":["whsec_y"]}"#,
        ),
        ("DELETE", "/_admin/tenants/acme/hooks/h1", ""),
        (
            "PUT",
            "/_admin/tenants/acme/hook-secrets",
            r#"{"version":null,"hooks":{}}"#,
        ),
        ("PUT", "/_admin/tenants/acme/quota", r#"{"rows":10}"#),
    ] {
        let r = admin(n.port, method, target, body);
        assert_eq!(r.status, 409, "{method} {target}: {}", r.body);
        assert!(r.body.contains("frozen"), "{}", r.body);
    }
    let read = admin(n.port, "GET", "/_admin/tenants/acme/hook-secrets", "");
    assert_eq!(read.status, 200, "a move's export reads it: {}", read.body);
    let r = admin(n.port, "POST", "/_admin/tenants/acme/thaw", "");
    assert_eq!(r.status, 200, "{}", r.body);
    let r = admin(n.port, "DELETE", "/_admin/tenants/acme/hooks/h1", "");
    assert_eq!(r.status, 200, "{}", r.body);
}

/// A set 0.2.0 saved with a `github` hook keyed `key delivery` -- which a
/// hook's `PUT` and a hooks file refuse now -- still reads as the node
/// starts again: every secret kept, its other hook armed, the export
/// writing `delivery` back so a copy and a move carry it, and each event of
/// that hook that verifies refused (409) naming `key body`, a forged one
/// the one 401 any forged event is, until one `PUT` with `key body` sets it
/// again -- listed not armed meanwhile, which a listing that asked only
/// whether the set named it said it was. The first cut of the refusal read
/// the set as one that does not read and removed the sidecar, the tenant's
/// every secret with it.
#[test]
fn a_set_saved_with_key_delivery_survives_a_restart_its_hook_refused_until_set_again() {
    let n = node("delivery", false);
    tenant(n.port, "acme");
    for q in [
        "create collection pushes (sha text)",
        "create statement pushed($sha text from event.sha) on hook pushes as insert pushes \
         {sha: $sha}",
    ] {
        let r = query(n.port, "acme", q);
        assert_eq!(r.status, 200, "{q}: {}", r.body);
    }
    const GH: &str = "a word its user chose";
    let pushes = |port: u16, key: &str| {
        admin(
            port,
            "PUT",
            "/_admin/tenants/acme/hooks/pushes",
            &format!(r#"{{"scheme":"github","secrets":["{GH}"],"key":"{key}"}}"#),
        )
    };
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    assert_eq!(pushes(n.port, "body").status, 200);
    let held = n.tenants.hooks().version("acme");
    assert!(held.is_some());
    // The sidecar as 0.2.0 wrote it for `key delivery`, in the layout it
    // shares with this one, the node stopped: its tenant closed, so the
    // node started again over the directory opens it.
    assert_eq!(n.tenants.close_idle(Duration::ZERO), 1);
    n.tenants.shutdown();
    let sidecar = n.dir.join("acme.hooks");
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert_eq!(text.matches(r#""key":"body""#).count(), 1);
    let saved = text.replace(r#""key":"body""#, r#""key":"delivery""#);
    std::fs::write(&sidecar, &saved).unwrap();

    let again = serve(n.dir.clone(), false);
    assert_eq!(
        again.tenants.hooks().version("acme"),
        held,
        "read, not put away"
    );
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), saved);
    let export = admin(again.port, "GET", "/_admin/tenants/acme/hook-secrets", "");
    assert_eq!(export.status, 200, "{}", export.body);
    for part in ["whsec_acme", GH, r#""key":"delivery""#] {
        assert!(export.body.contains(part), "{part} not exported");
    }
    let listing = admin(again.port, "GET", "/_admin/tenants/acme/hooks", "");
    assert!(
        listing.body.contains(r#""key":"delivery""#),
        "{}",
        listing.body
    );

    // Its other hook is armed.
    let taken = event(
        again.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(taken.status, 200, "{}", taken.body);
    // The `delivery` hook's verified event: refused naming `key body`,
    // nothing run; a forged one is the 401 of a hook that is not there.
    let push = r#"{"sha":"abc123"}"#;
    let refused = github(again.port, "/t/acme/_hooks/pushes", GH, push);
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(refused.body.contains("key body"), "{}", refused.body);
    assert!(!refused.body.contains(GH), "{}", refused.body);
    let forged = github(again.port, "/t/acme/_hooks/pushes", "a guess", push);
    let nosuch = github(again.port, "/t/acme/_hooks/nosuch", GH, push);
    assert_eq!(nosuch.status, 401, "{}", nosuch.body);
    assert_eq!((forged.status, &forged.body), (401, &nosuch.body));
    let count = |port: u16| query(port, "acme", "get pushes count").body;
    assert!(count(again.port).contains(r#""count":0"#));
    // Its listings say so: a hook whose events are refused is not armed,
    // whatever secret its set holds; its other hook is.
    assert_eq!(armed_in(again.port, "acme", "pushed"), [Some(false); 3]);
    assert_eq!(armed_in(again.port, "acme", "paid"), [Some(true); 3]);

    // A change of its other hook keeps it as it was kept.
    assert_eq!(set_secret(again.port, "acme", "whsec_acme").status, 200);
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert!(text.contains(r#""key":"delivery""#) && text.contains(GH));
    // `key delivery` is refused at a hook's `PUT` still; `key body` sets
    // it again, its events taken.
    let r = pushes(again.port, "delivery");
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(
        r.body.contains("does not sign X-GitHub-Delivery"),
        "{}",
        r.body
    );
    // Not armed, but held: its `DELETE` takes it away as any hook's does.
    let gone = admin(
        again.port,
        "DELETE",
        "/_admin/tenants/acme/hooks/pushes",
        "",
    );
    assert_eq!(gone.status, 200, "{}", gone.body);
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert!(!text.contains(GH) && text.contains("whsec_acme"));
    assert_eq!(pushes(again.port, "body").status, 200);
    assert_eq!(armed_in(again.port, "acme", "pushed"), [Some(true); 3]);
    let taken = github(again.port, "/t/acme/_hooks/pushes", GH, push);
    assert_eq!(taken.status, 200, "{}", taken.body);
    assert!(count(again.port).contains(r#""count":1"#));
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert!(!text.contains("delivery") && text.contains("whsec_acme"));
}

/// A sidecar that does not read -- cut short by hand, of a later layout --
/// is set aside as the node starts, under a name nothing reads, never
/// removed nor written over: it may hold the only copy of a secret, which
/// no router keeps. Its hooks answer 401 until set again, a `PUT` writing a
/// set of its own beside it; one naming another file is put away, as
/// `a_set_left_behind_never_arms_a_later_tenant_of_the_name` holds.
#[test]
fn a_sidecar_that_does_not_read_is_set_aside_never_removed() {
    let n = node("unread", false);
    tenant(n.port, "acme");
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    n.tenants.close_idle(Duration::ZERO);
    n.tenants.shutdown();
    let sidecar = n.dir.join("acme.hooks");
    let text = std::fs::read_to_string(&sidecar).unwrap();
    let set = text.find("\n{").unwrap() + 1;
    let cut = text[..set + (text.len() - set) / 2].to_string();
    std::fs::write(&sidecar, &cut).unwrap();

    let again = serve(n.dir.clone(), false);
    assert_eq!(again.tenants.hooks().version("acme"), None);
    assert!(!sidecar.exists(), "not read as the tenant's");
    let aside = n.dir.join("acme.hooks.unread");
    assert_eq!(
        std::fs::read_to_string(&aside).unwrap(),
        cut,
        "set aside whole"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&aside).unwrap();
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
    }
    let refused = event(
        again.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(refused.status, 401, "{}", refused.body);
    // Set again: a set of its own, the one set aside untouched.
    assert_eq!(set_secret(again.port, "acme", "whsec_again").status, 200);
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), cut);

    // One of a later layout: set aside under the next free name.
    again.tenants.close_idle(Duration::ZERO);
    again.tenants.shutdown();
    let later = std::fs::read_to_string(&sidecar)
        .unwrap()
        .replacen("1 ", "2 ", 1);
    std::fs::write(&sidecar, &later).unwrap();
    let third = serve(n.dir.clone(), false);
    assert_eq!(third.tenants.hooks().version("acme"), None);
    assert!(!sidecar.exists());
    assert_eq!(
        std::fs::read_to_string(&aside).unwrap(),
        cut,
        "never written over"
    );
    let next = n.dir.join("acme.hooks.unread.2");
    assert_eq!(std::fs::read_to_string(next).unwrap(), later);
}

/// Only a tenant's file that is not there puts its sidecar away: one the
/// node cannot look at as it starts -- a directory on its way it may not
/// search, a file where a directory should be -- says nothing of whose the
/// set is, and an error that passes may leave the tenant's file as it was.
/// So the sidecar is set aside, as one that does not read is, never
/// removed, and put back by hand it arms the tenant again. Both errors are
/// made through a link standing in for the tenant's file, its directory
/// read as the sidecar is; the first is passed over where the process
/// searches the directory anyway, as root does.
#[cfg(unix)]
#[test]
fn a_tenants_file_the_node_cannot_look_at_sets_its_sidecar_aside() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let n = node("unseen", false);
    tenant(n.port, "acme");
    assert_eq!(set_secret(n.port, "acme", "whsec_acme").status, 200);
    n.tenants.close_idle(Duration::ZERO);
    n.tenants.shutdown();
    let (file, sidecar) = (n.dir.join("acme.fennec"), n.dir.join("acme.hooks"));
    let text = std::fs::read_to_string(&sidecar).unwrap();
    // The file moved, which keeps its device, inode and birth: the sidecar
    // still names it, wherever the link leads.
    let held = n.dir.join("held");
    std::fs::create_dir(&held).unwrap();
    std::fs::rename(&file, held.join("acme.fennec")).unwrap();
    let plain = n.dir.join("plain");
    std::fs::write(&plain, b"a file, not a directory").unwrap();
    let mode = |m: u32| std::fs::set_permissions(&held, std::fs::Permissions::from_mode(m));

    // Each node started is kept: a node's drop removes the directory.
    let mut started = Vec::new();
    for (to, why) in [
        (held.join("acme.fennec"), "a directory it may not search"),
        (
            plain.join("acme.fennec"),
            "a file where a directory should be",
        ),
    ] {
        symlink(&to, &file).unwrap();
        mode(0o000).unwrap();
        let unseen = std::fs::metadata(&file);
        let node = unseen.is_err().then(|| serve(n.dir.clone(), false));
        mode(0o755).unwrap();
        std::fs::remove_file(&file).unwrap();
        let Some(node) = node else {
            continue;
        };
        let kind = unseen.unwrap_err().kind();
        assert_ne!(kind, std::io::ErrorKind::NotFound, "{why}");
        assert_eq!(node.tenants.hooks().version("acme"), None, "{why}");
        assert!(!sidecar.exists(), "{why}: left where a PUT writes over it");
        node.tenants.shutdown();
        started.push(node);
        let unread = match started.len() {
            1 => n.dir.join("acme.hooks.unread"),
            k => n.dir.join(format!("acme.hooks.unread.{k}")),
        };
        let kept = std::fs::read_to_string(&unread).ok();
        assert_eq!(kept.as_deref(), Some(&*text), "{why}: not set aside whole");
        std::fs::write(&sidecar, &text).unwrap();
    }
    assert!(!started.is_empty(), "no error was made");

    // The file back where it was, the sidecar put back by hand: the
    // tenant's secret arms it again.
    std::fs::rename(held.join("acme.fennec"), &file).unwrap();
    let back = serve(n.dir.clone(), false);
    assert!(back.tenants.hooks().version("acme").is_some());
    let taken = event(
        back.port,
        "/t/acme/_hooks/stripe",
        "whsec_acme",
        "evt_1",
        "o-1",
    );
    assert_eq!(taken.status, 200, "{}", taken.body);
}
