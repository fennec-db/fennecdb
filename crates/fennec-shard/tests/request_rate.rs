//! A node's `--request-rate` refusal through the router: the router holds
//! no caller to a rate of its own and parses nothing, so the node's 429
//! comes through as the node wrote it -- `Retry-After`, the body's
//! `retry_after_ms` -- with the router's mark beside it, and another caller
//! through the same router is answered.

use fennec_http::access::Access;
use fennec_http::tenants::Tenants;
use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::{Config, Router};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";

/// One request on a connection of its own: the status, the head, the body.
fn ask(addr: &str, method: &str, target: &str, token: &str, body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (
        out[9..12].parse().unwrap(),
        head.to_string(),
        body.to_string(),
    )
}

#[test]
fn a_callers_429_comes_through_the_router_as_the_node_wrote_it() {
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-shard-request-rate-{}-{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    tenants.create("acme").unwrap();
    let access = Arc::new(Access::new(SECRET, "notes  read  where owner = $jwt.sub\n").unwrap());
    let server = fennec_http::Server::with_tenants(
        Arc::clone(&tenants),
        fennec_http::Config {
            addr: "127.0.0.1:0".into(),
            admin_token: Some("adm".into()),
            token: Some("data".into()),
            access: Some(Arc::clone(&access)),
            request_rate: 1,
            ..fennec_http::Config::default()
        },
    );
    let listener = server.bind().unwrap();
    let node = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let q = r#"{"query":"create collection notes (owner text, title text)"}"#;
    let (status, _, body) = ask(&node, "POST", "/t/acme/query", "data", q);
    assert_eq!(status, 200, "{body}");

    let mut directory = Directory::in_memory();
    directory
        .set_node(
            "n1",
            Node {
                addr: node.clone(),
                token: "adm".into(),
                ..Node::default()
            },
        )
        .unwrap();
    directory.place("acme", "n1", State::Active).unwrap();
    let router = Router::new(
        directory,
        Config {
            addr: "127.0.0.1:0".into(),
            upstream_timeout: Duration::from_secs(10),
            ..Config::default()
        },
    );
    let listener = router.bind().unwrap();
    let router_addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });

    let alice = access.mint(r#"{"sub":"alice","tenant":"acme"}"#).unwrap();
    let bob = access.mint(r#"{"sub":"bob","tenant":"acme"}"#).unwrap();
    // At one a second, fifty tries spend alice's bucket whatever else the
    // machine is doing.
    let mut held = None;
    for _ in 0..50 {
        match ask(&router_addr, "GET", "/t/acme/notes", &alice, "") {
            (200, ..) => continue,
            (429, head, body) => {
                held = Some((head, body));
                break;
            }
            (s, _, body) => panic!("{s}: {body}"),
        }
    }
    let (head, body) = held.expect("alice held past --request-rate through the router");
    assert!(head.contains("Retry-After: 1\r\n"), "{head}");
    assert!(
        head.contains("Fennec-Shard: 1\r\n"),
        "the router's answer: {head}"
    );
    assert!(body.contains("--request-rate"), "{body}");
    assert!(body.contains("\"retry_after_ms\":"), "{body}");
    let (status, _, body) = ask(&router_addr, "GET", "/t/acme/notes", &bob, "");
    assert_eq!(status, 200, "bob through the same router: {body}");
}
