//! fennecdb's dials over TLS (`fennec_http::dial`): an `https://` address
//! is dialed with its handshake the dial's last step, bounded as a whole,
//! the server's certificate held to the process's CAs (`--tls-ca`,
//! `trust_ca`) -- a name it does not hold refused before a byte of a
//! request is sent -- and a replica's stream, the MCP bridge and the rest
//! carried over it. The private CA and the server's certificate are
//! `tests/certs/`, good for `localhost`, `127.0.0.1` and `::1`.
#![cfg(feature = "tls")]

use fennec_core::engine::Database;
use fennec_http::conn::Tls;
use fennec_http::dial;
use fennec_http::replication::{self, fresh_id, Follower, Replication, Upstream};
use fennec_http::{Config, Server};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs")
}

/// The test CA, trusted by every dial of this process: the same for every
/// test that sets it.
fn trust_test_ca() {
    dial::trust_ca(certs().join("ca.pem").to_str().unwrap()).unwrap();
}

fn tls() -> Arc<Tls> {
    Arc::new(Tls::from_pem_files(&certs().join("server.pem"), &certs().join("server.key")).unwrap())
}

/// A listener taking one TLS connection with the test certificate: what
/// its handshake came to, and the plaintext it read after, sent back.
fn stand_in() -> (u16, mpsc::Receiver<Result<Vec<u8>, String>>) {
    let server = fennec_tls::Server::from_pem_files(
        &certs().join("server.pem"),
        &certs().join("server.key"),
    )
    .unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (sock, _) = l.accept().unwrap();
        let got = match server.accept_within(sock, Duration::from_secs(10)) {
            Err(e) => Err(e.to_string()),
            Ok(mut s) => {
                s.get_ref()
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut got = Vec::new();
                let _ = s.read_to_end(&mut got);
                Ok(got)
            }
        };
        let _ = tx.send(got);
    });
    (port, rx)
}

/// A name the certificate does not hold, looked up to the stand-in's
/// address, is refused in the handshake: the server reads the client's
/// alert and not a byte of the request; the dial says the name. The name
/// it holds is dialed, and the request reaches it.
#[test]
fn a_name_the_certificate_does_not_hold_is_refused_before_a_byte_is_sent() {
    trust_test_ca();
    let (port, served) = stand_in();
    let name = format!("wrong.example:{port}");
    let to: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    dial::stand_in(&name, move || Ok(vec![to]));
    let e = dial::connect(&format!("https://{name}"), Duration::from_secs(10)).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::InvalidData, "{e}");
    assert!(e.to_string().contains("not valid for name"), "{e}");
    let refused = served.recv().unwrap().unwrap_err();
    assert!(refused.contains("alert"), "{refused}");

    // Through a replica's stream too: its token never goes out.
    let (port, served) = stand_in();
    let name = format!("other.example:{port}");
    let to: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    dial::stand_in(&name, move || Ok(vec![to]));
    let up = Upstream::new(&format!("https://{name}"), "secret".into()).unwrap();
    let e = up.open(0, 0, false).err().expect("a refusal");
    assert!(e.to_string().contains("certificate refused"), "{e}");
    assert!(served.recv().unwrap().is_err());

    let (port, served) = stand_in();
    let mut s = dial::connect(
        &format!("https://localhost:{port}"),
        Duration::from_secs(10),
    )
    .unwrap();
    assert!(s.is_tls());
    s.write_all(b"ping").unwrap();
    s.flush().unwrap();
    drop(s);
    assert_eq!(served.recv().unwrap().unwrap(), b"ping");
}

/// A server that takes the connection and never answers the handshake
/// holds a dial no longer than its bound.
#[test]
fn a_handshake_never_answered_is_a_timeout_at_the_dial_s_bound() {
    trust_test_ca();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let within = Duration::from_millis(500);
    let started = Instant::now();
    let e = dial::connect(&format!("https://localhost:{port}"), within).unwrap_err();
    let took = started.elapsed();
    assert_eq!(e.kind(), ErrorKind::TimedOut, "{e}");
    assert!(took >= within, "{took:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
    drop(l);
}

/// A replica follows a primary serving HTTPS: its stream over TLS, the
/// primary's writes arriving as over plain HTTP, and its halt -- the socket
/// beneath the session shut -- ends the stream at once.
#[test]
fn a_replica_follows_its_primary_over_https() {
    trust_test_ca();
    let dir = std::env::temp_dir().join(format!("fennec-dials-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut db, feed) =
        replication::open(dir.join("p.fennec").to_str().unwrap(), 8 << 20).unwrap();
    db.fork(fresh_id()).unwrap();
    let primary = Arc::new(RwLock::new(db));
    let server = Server::new(
        Arc::clone(&primary),
        Config {
            addr: "127.0.0.1:0".into(),
            sync_on_write: true,
            tls: Some(tls()),
            ..Config::default()
        },
    )
    .with_replication(Replication::new(Some("t".into()), Some(feed), None));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });

    let (mut db, feed) =
        replication::open(dir.join("r.fennec").to_str().unwrap(), 8 << 20).unwrap();
    let lineage = db.history().lineage.clone();
    db.follow(lineage).unwrap();
    let replica: Arc<RwLock<Database>> = Arc::new(RwLock::new(db));
    let follower = Follower::new(
        &format!("https://localhost:{port}"),
        "t".into(),
        Arc::clone(&replica),
        Some(feed),
        true,
    )
    .unwrap();
    let f = Arc::clone(&follower);
    let running = std::thread::spawn(move || f.run());

    // Over HTTPS, synced as answered: a replica is sent what is on disk.
    for q in [
        "create collection items (name text)",
        r#"put items {name: "a"}"#,
    ] {
        let body = format!("{{\"query\":{q:?}}}");
        let mut s = dial::connect(
            &format!("https://localhost:{port}"),
            Duration::from_secs(10),
        )
        .unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let head = format!(
            "POST /query HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        fennec_http::http::write_parts(&mut s, head.as_bytes(), body.as_bytes()).unwrap();
        let mut answer = Vec::new();
        let _ = s.read_to_end(&mut answer);
        let answer = String::from_utf8_lossy(&answer);
        assert!(answer.starts_with("HTTP/1.1 200"), "{q}: {answer}");
    }
    let want = primary.read().unwrap().change_seq();
    let started = Instant::now();
    while replica.read().unwrap().change_seq() != want {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the replica stayed at {} of {want}: {:?}",
            replica.read().unwrap().change_seq(),
            follower.progress()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(follower.progress().0, "connected");
    let halted = Instant::now();
    follower.halt();
    running.join().unwrap();
    assert!(halted.elapsed() < Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&dir);
}
