//! `--tls-cert` and `--tls-key`: both or neither, read and matched before
//! anything is opened; with them the listener says `https://`, answers
//! over TLS alone, `--metrics` speaks it too, and `--ping` -- a container's
//! health check, which names no flag -- asks over TLS once the port says it
//! speaks it alone.
#![cfg(feature = "tls")]

use crate::support::{start, tmp};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn cert(name: &str) -> String {
    let p: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fennec-http/tests/certs")
        .join(name);
    p.to_string_lossy().into_owned()
}

/// The binary run with `args` to its end: its exit code and its log.
fn refused(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fennec-server"))
        .args(["--http", "127.0.0.1:0"])
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn tls_flags_go_together_and_are_read_at_the_start() {
    let (pem, key, other) = (cert("server.pem"), cert("server.key"), cert("other.key"));
    let (code, log) = refused(&["--tls-cert", &pem]);
    assert_eq!(code, 2, "{log}");
    assert!(log.contains("go together"), "{log}");
    let (code, log) = refused(&["--tls-key", &key]);
    assert_eq!(code, 2, "{log}");

    let (code, log) = refused(&["--tls-cert", &pem, "--tls-key", &other]);
    assert_eq!(code, 2, "{log}");
    assert!(log.contains("KeyMismatch"), "{log}");

    let missing = tmp("https", "absent.pem");
    let missing = missing.to_string_lossy();
    let (code, log) = refused(&["--tls-cert", &missing, "--tls-key", &key]);
    assert_eq!(code, 2, "{log}");
    assert!(log.contains("absent.pem"), "{log}");
}

/// A key file open to other users is said at the start, its path and mode
/// named and none of it; one open to its owner alone is not.
#[cfg(unix)]
#[test]
fn a_key_open_to_others_is_warned_of() {
    use std::os::unix::fs::PermissionsExt;
    let pem = cert("server.pem");
    let key = tmp("https-key", "server.key");
    std::fs::copy(cert("server.key"), &key).unwrap();
    let bytes = std::fs::read_to_string(&key).unwrap();
    for (mode, warned) in [(0o644, true), (0o600, false)] {
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(mode)).unwrap();
        let server = start(&["--tls-cert", &pem, "--tls-key", key.to_str().unwrap()]);
        let (_, log) = server.terminate();
        assert_eq!(
            log.contains("warning: --tls-key"),
            warned,
            "{mode:o}: {log}"
        );
        if warned {
            assert!(log.contains("every user (mode 0644)"), "{log}");
        }
        for line in bytes.lines() {
            assert!(!log.contains(line), "the key in the log: {log}");
        }
    }
}

/// `GET path` over TLS to `port`, trusting the test CA: the answer whole.
fn get_tls(port: u16, path: &str) -> String {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let client = fennec_tls::Client::trusting_pem_file(Path::new(&cert("ca.pem"))).unwrap();
    let mut s = client.dial(s, "localhost").unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn serves_https_alone_and_pings_over_it() {
    let (pem, key) = (cert("server.pem"), cert("server.key"));
    let server = start(&[
        "--tls-cert",
        &pem,
        "--tls-key",
        &key,
        "--metrics",
        "127.0.0.1:0",
    ]);
    let log = server.log.lock().unwrap().clone();
    assert!(log.contains("listening on: https://"), "{log}");

    let health = get_tls(server.port, "/_health");
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");

    // Plain HTTP is told the scheme.
    let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(b"GET /_health HTTP/1.1\r\nHost: t\r\n\r\n")
        .unwrap();
    let mut plain = String::new();
    s.read_to_string(&mut plain).unwrap();
    assert!(plain.starts_with("HTTP/1.1 400"), "{plain}");

    // The probe names no flag but the address, as a container's does.
    let addr = format!("127.0.0.1:{}", server.port);
    let ping = Command::new(env!("CARGO_BIN_EXE_fennec-server"))
        .args(["--ping", "--http", &addr])
        .output()
        .unwrap();
    assert!(
        ping.status.success(),
        "{}",
        String::from_utf8_lossy(&ping.stderr)
    );

    // `--metrics` speaks TLS too: its line is its own thread's, said
    // beside the HTTP listener's.
    let started = std::time::Instant::now();
    let log = loop {
        let log = server.log.lock().unwrap().clone();
        if log.contains("metrics on: ") || started.elapsed() > Duration::from_secs(20) {
            break log;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let metrics = log
        .lines()
        .find_map(|l| l.split_once("metrics on: https://"))
        .map(|(_, rest)| rest.split('/').next().unwrap().to_string())
        .unwrap_or_else(|| panic!("no TLS metrics listener: {log}"));
    let port: u16 = metrics.rsplit_once(':').unwrap().1.parse().unwrap();
    let scrape = get_tls(port, "/_metrics");
    assert!(scrape.starts_with("HTTP/1.1 200"), "{scrape}");
    assert!(scrape.contains("fennec_"), "{scrape}");
}

/// Every TLS connection ends with its `close_notify`: one closed after its
/// answer, and at the one shutdown a stream of changes, woken to end, and
/// a connection waiting for its request. Each reads an end the server
/// meant -- `read_to_end` is `Ok` -- where the exit's TCP end alone read as
/// one cut short (rustls's `UnexpectedEof`).
#[test]
fn every_tls_end_is_a_close_notify_the_shutdown_s_too() {
    let (pem, key) = (cert("server.pem"), cert("server.key"));
    let file = tmp("https-close", "data.fennec");
    let server = start(&[
        "--tls-cert",
        &pem,
        "--tls-key",
        &key,
        "--file",
        file.to_str().unwrap(),
    ]);
    let port = server.port;
    let dial = || {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let client = fennec_tls::Client::trusting_pem_file(Path::new(&cert("ca.pem"))).unwrap();
        client.dial(s, "localhost").unwrap()
    };
    let q = r#"{"query":"create collection notes (t text)"}"#;
    let mut made = dial();
    write!(
        made,
        "POST /query HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{q}",
        q.len()
    )
    .unwrap();
    let mut answer = Vec::new();
    let ended = made.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    assert!(ended.is_ok(), "{ended:?} after {answer}");
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");

    let mut stream = std::io::BufReader::new(dial());
    stream
        .get_mut()
        .write_all(b"GET /notes/changes HTTP/1.1\r\nHost: t\r\n\r\n")
        .unwrap();
    let mut events = String::new();
    while !events.contains("event: seed") {
        let mut line = String::new();
        assert!(
            std::io::BufRead::read_line(&mut stream, &mut line).unwrap() > 0,
            "{events}"
        );
        events.push_str(&line);
    }
    // A connection whose handshake is done, its request not sent.
    let mut waiting = dial();

    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
    let mut rest = Vec::new();
    let ended = stream.read_to_end(&mut rest);
    assert!(ended.is_ok(), "the stream: {ended:?}\n{log}");
    let ended = waiting.read_to_end(&mut rest);
    assert!(ended.is_ok(), "the waiting connection: {ended:?}\n{log}");
}

/// A request over TLS is `"proto":"https"` in the audit log, a refusal
/// here.
#[test]
fn an_audit_line_names_https() {
    let (pem, key) = (cert("server.pem"), cert("server.key"));
    let audit = tmp("https", "audit.log");
    let server = start(&[
        "--tls-cert",
        &pem,
        "--tls-key",
        &key,
        "--http-token",
        "right",
        "--audit",
        audit.to_str().unwrap(),
        "--auth-delay",
        "0",
    ]);
    let refused = get_tls(server.port, "/notes");
    assert!(refused.starts_with("HTTP/1.1 401"), "{refused}");
    let started = std::time::Instant::now();
    let line = loop {
        let log = std::fs::read_to_string(&audit).unwrap_or_default();
        if let Some(l) = log.lines().find(|l| l.contains("\"refused\"")) {
            break l.to_string();
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no audit line: {log}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(line.contains("\"proto\":\"https\""), "{line}");
}

/// A request over HTTPS to `port`, trusting the test CA: its whole answer.
fn ask_tls(port: u16, method: &str, path: &str, body: &str) -> String {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let client = fennec_tls::Client::trusting_pem_file(Path::new(&cert("ca.pem"))).unwrap();
    let mut s = client.dial(s, "localhost").unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// A statement over HTTPS, which must be answered 200.
fn run_tls(port: u16, q: &str) {
    let body = format!("{{\"query\":{}}}", crate::support::json_string(q));
    let answer = ask_tls(port, "POST", "/query", &body);
    assert!(answer.starts_with("HTTP/1.1 200"), "{q}: {answer}");
}

/// What a scrape over HTTPS says `name` is, 0 where it says nothing.
fn scraped(port: u16, name: &str) -> f64 {
    let scrape = ask_tls(port, "GET", "/_metrics", "");
    scrape
        .lines()
        .find_map(|l| l.strip_prefix(name))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0.0)
}

/// `--replica-of https://...`: the replica dials its primary over TLS, the
/// primary's certificate held to `--tls-ca`, and follows it as over plain
/// HTTP. Without `--tls-ca`, Mozilla's roots refuse the test CA's
/// certificate before the token goes out -- the primary counts the
/// replica's alert, a failed handshake -- and the replica says which flag
/// names a private CA.
#[test]
fn a_replica_follows_its_primary_over_https() {
    const TOKEN: &str = "repl-tls";
    let (pem, key, ca) = (cert("server.pem"), cert("server.key"), cert("ca.pem"));
    let pf = tmp("https", "primary.fennec");
    let primary = start(&[
        "--file",
        pf.to_str().unwrap(),
        "--tls-cert",
        &pem,
        "--tls-key",
        &key,
        "--replication-token",
        TOKEN,
    ]);
    run_tls(primary.port, "create collection items (name text)");
    run_tls(primary.port, r#"put items {name: "a"}"#);
    let upstream = format!("https://localhost:{}", primary.port);
    let alerts = "fennec_tls_handshakes_failed_total{reason=\"alert\"}";

    let before = scraped(primary.port, alerts);
    let rf = tmp("https", "unknown-issuer.fennec");
    let stranger = start(&[
        "--file",
        rf.to_str().unwrap(),
        "--replica-of",
        &upstream,
        "--replication-token",
        TOKEN,
    ]);
    assert!(
        stranger.logged("UnknownIssuer", Duration::from_secs(30)),
        "{}",
        stranger.log.lock().unwrap()
    );
    assert!(stranger.log.lock().unwrap().contains("--tls-ca"));
    let after = scraped(primary.port, alerts);
    assert!(after > before, "{before} -> {after}");
    drop(stranger);

    let rf = tmp("https", "replica.fennec");
    let replica = start(&[
        "--file",
        rf.to_str().unwrap(),
        "--replica-of",
        &upstream,
        "--replication-token",
        TOKEN,
        "--tls-ca",
        &ca,
    ]);
    run_tls(primary.port, r#"put items {name: "b"}"#);
    let mut c = replica.http();
    let started = std::time::Instant::now();
    while !c
        .query("get items count")
        .is_ok_and(|b| crate::support::count(&b) == 2)
    {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{}",
            replica.log.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `--otlp-endpoint https://...`: spans go to a collector over TLS, its
/// certificate held to `--tls-ca`.
#[test]
fn spans_go_to_an_https_collector() {
    let (pem, key, ca) = (cert("server.pem"), cert("server.key"), cert("ca.pem"));
    let collector = crate::otlp::Receiver::start_tls(&pem, &key);
    assert!(collector.url().starts_with("https://localhost:"));
    let server = start(&[
        "--otlp-endpoint",
        &collector.url(),
        "--trace-sample",
        "1",
        "--tls-ca",
        &ca,
    ]);
    server.http().run("create collection notes (title text)");
    collector.until("a request's span", |s| !s.is_empty());
    assert!(collector.posts().iter().all(|p| p.path == "/v1/traces"));
}
