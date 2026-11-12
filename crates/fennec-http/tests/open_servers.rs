//! A server that asks for no token answers only under this machine's name,
//! and a page only from its own origin or the one `--http-cors` names
//! (`cors::foreign`, before any route): a page on any site reached such a
//! server on 127.0.0.1 with a simple request, no preflight, and a page whose
//! name was rebound to 127.0.0.1 read every row. A server with a token is
//! as it was.

use fennec_core::engine::Database;
use fennec_http::tenants::Tenants;
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// What a request was answered.
struct Answer {
    status: u16,
    head: String,
    body: String,
}

fn serve(server: Server) -> u16 {
    fennec_http::audit::set_delay(0);
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    port
}

/// A file's server over a database holding `users`, one row in it.
fn single(cfg: Config) -> u16 {
    let mut db = Database::new();
    for q in [
        "create collection users (name text)",
        "insert users {name: 'ada'}",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    serve(Server::new(
        Arc::new(RwLock::new(db)),
        Config {
            addr: "127.0.0.1:0".into(),
            ..cfg
        },
    ))
}

/// One request on a connection of its own, its `Host` and the rest of
/// its headers as given: no `Host` at all where `host` is `None`.
fn ask(
    port: u16,
    method: &str,
    target: &str,
    host: Option<&str>,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    if let Some(h) = host {
        head.push_str(&format!("Host: {h}\r\n"));
    }
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    s.write_all(head.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    Answer {
        status: head[9..12].parse().unwrap(),
        head: head.to_string(),
        body: body.to_string(),
    }
}

/// A local request's view of whether `users` is still there.
fn has_users(port: u16) -> bool {
    let a = ask(port, "GET", "/collections", Some("localhost"), &[], "");
    assert_eq!(a.status, 200, "{}", a.body);
    a.body.contains("\"users\"")
}

const DROP: &str = r#"{"query":"drop collection users"}"#;

/// `fetch("http://127.0.0.1:8080/query", {method: "POST", mode: "no-cors",
/// body})` from any site: a simple request, text/plain, no preflight. Its
/// `Origin` refuses it before anything of it runs.
#[test]
fn another_origins_post_is_refused_before_it_runs() {
    let port = single(Config::default());
    let host = format!("127.0.0.1:{port}");
    let page = [
        ("Origin", "https://evil.example"),
        ("Content-Type", "text/plain;charset=UTF-8"),
    ];
    let a = ask(port, "POST", "/query", Some(&host), &page, DROP);
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(
        a.body.contains("Origin `https://evil.example`"),
        "{}",
        a.body
    );
    assert!(a.body.contains("--http-cors"), "{}", a.body);
    assert!(has_users(port));
    // Every route, REST's delete and `/batch` among them.
    let a = ask(port, "DELETE", "/users", Some(&host), &page, "");
    assert_eq!(a.status, 403, "{}", a.body);
    let batch = r#"{"statements":[{"query":"drop collection users"}]}"#;
    let a = ask(port, "POST", "/batch", Some(&host), &page, batch);
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(has_users(port));
    // A sandboxed frame's or a file's page is `null`: no origin of the
    // server's.
    let a = ask(
        port,
        "POST",
        "/query",
        Some(&host),
        &[("Origin", "null")],
        DROP,
    );
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(has_users(port));
}

/// A page whose name was rebound to 127.0.0.1 sends its own name as
/// `Host`, and a same-origin `GET` no `Origin`: the `Host` refuses it, a
/// read as much as a write.
#[test]
fn a_rebound_name_is_refused_a_read_included() {
    let port = single(Config::default());
    let rebound = Some("evil.example:8080");
    let a = ask(port, "GET", "/users", rebound, &[], "");
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(!a.body.contains("ada"), "{}", a.body);
    assert!(
        a.body
            .contains("Host `evil.example:8080` is not this machine's"),
        "{}",
        a.body
    );
    assert!(
        a.body
            .contains("reach it as localhost or by its address, or give it --http-token"),
        "{}",
        a.body
    );
    let a = ask(port, "GET", "/_whoami", rebound, &[], "");
    assert_eq!(a.status, 403, "{}", a.body);
    let a = ask(
        port,
        "POST",
        "/query",
        rebound,
        &[("Origin", "http://evil.example:8080")],
        DROP,
    );
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(has_users(port));
    // A stream, which takes the connection over once let in.
    let a = ask(port, "GET", "/users/changes", rebound, &[], "");
    assert_eq!(a.status, 403, "{}", a.body);
}

/// A tenant node given `--admin-token` alone answers its tenants' data to
/// anyone: it is open there, and held to the same rule.
#[test]
fn a_node_with_an_admin_token_alone_is_held_too() {
    let dir = std::env::temp_dir().join(format!("fennec-open-node-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let port = serve(Server::with_tenants(
        tenants,
        Config {
            addr: "127.0.0.1:0".into(),
            admin_token: Some("adm".into()),
            ..Config::default()
        },
    ));
    let local = Some("localhost");
    let admin = [("Authorization", "Bearer adm")];
    let a = ask(port, "PUT", "/_admin/tenants/acme", local, &admin, "");
    assert_eq!(a.status, 201, "{}", a.body);
    let make = r#"{"query":"create collection users (name text)"}"#;
    let a = ask(port, "POST", "/t/acme/query", local, &[], make);
    assert_eq!(a.status, 200, "{}", a.body);
    let a = ask(port, "GET", "/t/acme/users", Some("evil.example"), &[], "");
    assert_eq!(a.status, 403, "{}", a.body);
    let page = [("Origin", "https://evil.example")];
    let a = ask(port, "POST", "/t/acme/query", local, &page, DROP);
    assert_eq!(a.status, 403, "{}", a.body);
    let a = ask(port, "GET", "/t/acme/collections", local, &[], "");
    assert!(a.body.contains("\"users\""), "{}", a.body);
    let _ = std::fs::remove_dir_all(&dir);
}

/// This machine's names pass, and so do a page of the server's own origin,
/// the one `--http-cors` names and a client sending none.
#[test]
fn this_machines_names_and_the_allowed_origins_pass() {
    let port = single(Config::default());
    let q = r#"{"query":"get users"}"#;
    for host in [
        format!("localhost:{port}"),
        "localhost".into(),
        format!("app.localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
        // A machine's address on its network: an address cannot be
        // rebound.
        format!("192.168.1.20:{port}"),
    ] {
        let a = ask(port, "POST", "/query", Some(&host), &[], q);
        assert_eq!(a.status, 200, "{host}: {}", a.body);
        assert!(a.body.contains("ada"), "{}", a.body);
    }
    // HTTP/1.0's request may name no host: no browser's.
    let a = ask(port, "POST", "/query", None, &[], q);
    assert_eq!(a.status, 200, "{}", a.body);
    // A page the server served itself -- its studio's.
    for host in [format!("127.0.0.1:{port}"), format!("localhost:{port}")] {
        let own = format!("http://{host}");
        let a = ask(port, "POST", "/query", Some(&host), &[("Origin", &own)], q);
        assert_eq!(a.status, 200, "{host}: {}", a.body);
    }

    // `--http-cors` names the page's origin.
    let app = "https://app.example";
    let port = single(Config {
        cors: Some(app.into()),
        ..Config::default()
    });
    let host = format!("127.0.0.1:{port}");
    let a = ask(port, "POST", "/query", Some(&host), &[("Origin", app)], q);
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(
        a.head
            .contains(&format!("Access-Control-Allow-Origin: {app}")),
        "{}",
        a.head
    );
    let a = ask(
        port,
        "POST",
        "/query",
        Some(&host),
        &[("Origin", "https://other.example")],
        DROP,
    );
    assert_eq!(a.status, 403, "{}", a.body);
    // The refusal carries the CORS headers: a page of the allowed origin
    // reaching the server by another name reads why.
    let a = ask(
        port,
        "POST",
        "/query",
        Some("db.example"),
        &[("Origin", app)],
        q,
    );
    assert_eq!(a.status, 403, "{}", a.body);
    assert!(a.head.contains("Access-Control-Allow-Origin"), "{}", a.head);
    assert!(has_users(port));
    // `*` lets every page in, on this machine's names.
    let port = single(Config {
        cors: Some("*".into()),
        ..Config::default()
    });
    let host = format!("127.0.0.1:{port}");
    let a = ask(
        port,
        "POST",
        "/query",
        Some(&host),
        &[("Origin", "https://any.example")],
        q,
    );
    assert_eq!(a.status, 200, "{}", a.body);
}

/// The two answers that read nothing are given under any name: the health
/// check, a constant, and a preflight `--http-cors` lets in, the headers
/// every answer carries.
#[test]
fn the_health_check_and_an_allowed_preflight_are_answered_under_any_name() {
    let app = "https://app.example";
    let port = single(Config {
        cors: Some(app.into()),
        ..Config::default()
    });
    let named = Some("fennec:8080");
    let a = ask(port, "GET", "/_health", named, &[], "");
    assert_eq!(a.status, 200, "{}", a.body);
    let preflight = [
        ("Origin", app),
        ("Access-Control-Request-Method", "POST"),
        ("Access-Control-Request-Headers", "content-type"),
    ];
    let a = ask(port, "OPTIONS", "/query", named, &preflight, "");
    assert_eq!(a.status, 204, "{}", a.body);
    // Another origin's preflight is no exception.
    let other = [
        ("Origin", "https://evil.example"),
        ("Access-Control-Request-Method", "POST"),
    ];
    let a = ask(port, "OPTIONS", "/query", Some("localhost"), &other, "");
    assert_eq!(a.status, 403, "{}", a.body);
}

/// A server with a token is as it was: a page holds no token, and sends
/// none across origins without the preflight CORS refuses it.
#[test]
fn a_server_with_a_token_is_unchanged() {
    let port = single(Config {
        token: Some("root".into()),
        ..Config::default()
    });
    let named = Some("db.example:8080");
    let auth = ("Authorization", "Bearer root");
    let a = ask(port, "GET", "/users", named, &[auth], "");
    assert_eq!(a.status, 200, "{}", a.body);
    assert!(a.body.contains("ada"), "{}", a.body);
    let a = ask(
        port,
        "POST",
        "/query",
        named,
        &[auth, ("Origin", "https://evil.example")],
        r#"{"query":"get users"}"#,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    // Without the token, the token's refusal, not the guard's.
    let a = ask(
        port,
        "GET",
        "/users",
        named,
        &[("Origin", "https://evil.example")],
        "",
    );
    assert_eq!(a.status, 401, "{}", a.body);
    assert!(has_users_with(port, "root"));
}

fn has_users_with(port: u16, token: &str) -> bool {
    let auth = format!("Bearer {token}");
    let a = ask(
        port,
        "GET",
        "/collections",
        Some("localhost"),
        &[("Authorization", &auth)],
        "",
    );
    a.body.contains("\"users\"")
}
