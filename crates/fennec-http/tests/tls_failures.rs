//! The handshakes an HTTPS listener saw fail, counted by why:
//! `fennec_tls_handshakes_failed_total{reason}`. A binary of its own: the
//! counts are the process's, and the other tests' servers fail some too.
#![cfg(feature = "tls")]

use fennec_core::engine::Database;
use fennec_http::conn::{handshakes_failed, Failed, Tls};
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs")
}

/// Waits for the count of `why` to reach `n`: each is counted on its
/// connection's thread, after the client's part is done.
fn until(why: Failed, n: u64) {
    let started = Instant::now();
    while handshakes_failed(why) < n {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{why:?}: {} of {n}",
            handshakes_failed(why)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(handshakes_failed(why), n, "{why:?}");
}

fn connect(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s
}

#[test]
fn a_failed_handshake_is_counted_by_why() {
    let within = Duration::from_millis(400);
    let tls = Tls::from_pem_files(&certs().join("server.pem"), &certs().join("server.key"))
        .unwrap()
        .handshake_within(within);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        tls: Some(Arc::new(tls)),
        ..Config::default()
    };
    let server = Arc::new(Server::new(Arc::new(RwLock::new(Database::new())), cfg));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = serving.serve_on(listener);
    });
    let mut rest = Vec::new();

    // A plain request, answered 400.
    let mut plain = connect(port);
    plain
        .write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n")
        .unwrap();
    let _ = plain.read_to_end(&mut rest);
    until(Failed::PlainHttp, 1);

    // A connection ended before its first byte: a TCP probe.
    drop(connect(port));
    until(Failed::Closed, 1);

    // A first byte no TLS record begins with, then a handshake's record
    // holding no handshake.
    let mut junk = connect(port);
    junk.write_all(b"\x00junk").unwrap();
    let _ = junk.read_to_end(&mut rest);
    until(Failed::Protocol, 1);
    let mut junk = connect(port);
    junk.write_all(&[0x16, 0x03, 0x01, 0x00, 0x04, 0xff, 0x00, 0x00, 0x00])
        .unwrap();
    let _ = junk.read_to_end(&mut rest);
    until(Failed::Protocol, 2);

    // A client that does not trust the certificate: its alert.
    let refused = fennec_tls::Client::new().dial(connect(port), "localhost");
    assert!(refused.is_err());
    until(Failed::Alert, 1);

    // A handshake begun and left: past the bound.
    let mut stalled = connect(port);
    stalled
        .write_all(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01])
        .unwrap();
    let _ = stalled.read_to_end(&mut rest);
    until(Failed::Timeout, 1);

    // None counted under another reason meanwhile, and a good client none.
    // and a good client's, asking the counts, none.
    let good = fennec_tls::Client::trusting_pem_file(&certs().join("ca.pem")).unwrap();
    let mut scrape = good.dial(connect(port), "localhost").unwrap();
    scrape
        .write_all(b"GET /_metrics HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut text = Vec::new();
    let ended = scrape.read_to_end(&mut text);
    let text = String::from_utf8_lossy(&text);
    assert!(ended.is_ok(), "{ended:?}: no close_notify after\n{text}");
    for (why, reason, n) in [
        (Failed::Timeout, "timeout", 1),
        (Failed::Closed, "closed", 1),
        (Failed::Alert, "alert", 1),
        (Failed::Protocol, "protocol", 2),
        (Failed::PlainHttp, "plain_http", 1),
    ] {
        assert_eq!(handshakes_failed(why), n, "{why:?}");
        let line = format!("fennec_tls_handshakes_failed_total{{reason=\"{reason}\"}} {n}\n");
        assert!(text.contains(&line), "{line} in:\n{text}");
    }
}
