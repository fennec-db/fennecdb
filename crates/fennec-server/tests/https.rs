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
