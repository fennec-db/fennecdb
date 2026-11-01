//! The client's TLS against a stand-in that takes PostgreSQL's `SSLRequest`:
//! each `sslmode`, a server's `S`, `N` or error, a handshake cut short, and a
//! certificate refused before the startup message -- user, database and
//! all -- is sent. A live PostgreSQL with `ssl = on` is `make import-test`'s.
//!
//! The certificates are `tests/certs`, made by fennec-tls's `wire_certs`
//! test, so rcgen stays fennec-tls's dev-dependency alone:
//! `FENNEC_WRITE_CERTS=1 cargo test -p fennec-tls wire_certs` writes them
//! again. Good from 2026 to 2126; the server's names `localhost` alone,
//! under `ca.pem`, and `other-ca.pem` signed none of them.

use fennec_wire::client::{Client, SslMode, Url};
use fennec_wire::proto::SSL_REQUEST;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn cert(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/certs")
        .join(name)
}

/// How the stand-in answers an `SSLRequest`.
#[derive(Clone, Copy)]
enum Answer {
    /// `S`, then the handshake with the server's certificate.
    Tls,
    /// `N`: no TLS here.
    No,
    /// `S`, then the connection closed, no handshake.
    HangUp,
    /// An `ErrorResponse`, as a server that cannot take the request sends.
    Error,
}

/// What the stand-in saw of one connection.
#[derive(Debug, PartialEq)]
enum Seen {
    /// No `SSLRequest`: the startup message first, in plain text.
    Plain {
        startup: bool,
    },
    /// `SSLRequest` answered `N`, then the startup in plain text.
    Refused {
        startup: bool,
    },
    /// `SSLRequest` answered `S`, the handshake done, then the startup.
    Tls {
        startup: bool,
    },
    /// `SSLRequest` answered `S`, and the handshake failed: nothing of the
    /// client's was read over it.
    HandshakeFailed(String),
    HungUp,
    Errored,
    /// The test's own last connection: every one before it is done.
    Probe,
}

fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn standin(answer: Answer) -> (u16, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server =
        fennec_tls::Server::from_pem_files(&cert("server.pem"), &cert("server.key")).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for s in listener.incoming() {
            let s = s.unwrap();
            s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            if tx.send(connection(s, answer, &server)).is_err() {
                return;
            }
        }
    });
    (port, rx)
}

fn connection(mut s: TcpStream, answer: Answer, server: &fennec_tls::Server) -> Seen {
    let mut head = [0u8; 8];
    s.read_exact(&mut head).unwrap();
    let len = i32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let code = i32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    if code == -1 {
        return Seen::Probe;
    }
    if code != SSL_REQUEST {
        // A startup message: its length, the version, then the rest.
        let mut rest = vec![0u8; len as usize - 8];
        s.read_exact(&mut rest).unwrap();
        session(&mut s, "plain");
        return Seen::Plain { startup: true };
    }
    match answer {
        Answer::No => {
            s.write_all(b"N").unwrap();
            let startup = startup(&mut s);
            session(&mut s, "plain");
            Seen::Refused { startup }
        }
        Answer::HangUp => {
            s.write_all(b"S").unwrap();
            Seen::HungUp
        }
        Answer::Error => {
            let mut b = Vec::new();
            for (k, v) in [
                (b'S', "FATAL"),
                (b'C', "08P01"),
                (b'M', "no TLS before 7.0"),
            ] {
                b.push(k);
                b.extend_from_slice(v.as_bytes());
                b.push(0);
            }
            b.push(0);
            s.write_all(&framed(b'E', &b)).unwrap();
            Seen::Errored
        }
        Answer::Tls => {
            s.write_all(b"S").unwrap();
            match server.accept(s) {
                Ok(mut t) => {
                    let startup = startup(&mut t);
                    session(&mut t, "tls");
                    Seen::Tls { startup }
                }
                Err(e) => Seen::HandshakeFailed(e.to_string()),
            }
        }
    }
}

/// The startup message, read whole: whether one came.
fn startup(s: &mut impl Read) -> bool {
    let mut len = [0u8; 4];
    if s.read_exact(&mut len).is_err() {
        return false;
    }
    let mut body = vec![0u8; i32::from_be_bytes(len) as usize - 4];
    s.read_exact(&mut body).is_ok() && body.starts_with(&196_608i32.to_be_bytes())
}

/// Trust, then each query answered with one row: `via`, the transport.
fn session(s: &mut (impl Read + Write), via: &str) {
    let mut out = framed(b'R', &0i32.to_be_bytes());
    out.extend_from_slice(&framed(b'S', b"server_version\x0017.0\x00"));
    out.extend_from_slice(&framed(b'Z', b"I"));
    s.write_all(&out).unwrap();
    s.flush().unwrap();
    loop {
        let mut head = [0u8; 5];
        if s.read_exact(&mut head).is_err() {
            return;
        }
        let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize - 4;
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        if head[0] != b'Q' {
            return;
        }
        let mut t = 1i16.to_be_bytes().to_vec();
        t.extend_from_slice(b"via\0");
        t.extend_from_slice(&0i32.to_be_bytes()); // table oid
        t.extend_from_slice(&0i16.to_be_bytes()); // column number
        t.extend_from_slice(&25i32.to_be_bytes()); // text
        t.extend_from_slice(&(-1i16).to_be_bytes());
        t.extend_from_slice(&(-1i32).to_be_bytes());
        t.extend_from_slice(&0i16.to_be_bytes());
        let mut row = 1i16.to_be_bytes().to_vec();
        row.extend_from_slice(&(via.len() as i32).to_be_bytes());
        row.extend_from_slice(via.as_bytes());
        let mut out = framed(b'T', &t);
        out.extend_from_slice(&framed(b'D', &row));
        out.extend_from_slice(&framed(b'C', b"SELECT 1\0"));
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();
        s.flush().unwrap();
    }
}

/// Every connection the stand-in saw, in order: the probe goes in after the
/// client is done, so whatever the client dialed is ahead of it.
fn seen(port: u16, rx: &mpsc::Receiver<Seen>) -> Vec<Seen> {
    let mut probe = TcpStream::connect(("127.0.0.1", port)).unwrap();
    probe
        .write_all(&[0, 0, 0, 8, 0xff, 0xff, 0xff, 0xff])
        .unwrap();
    let mut out = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            Seen::Probe => return out,
            s => out.push(s),
        }
    }
}

fn url(host: &str, port: u16, sslmode: SslMode, rootcert: Option<&str>) -> Url {
    Url {
        user: "fennec".into(),
        password: None,
        host: host.into(),
        port,
        database: "fennec".into(),
        sslmode,
        sslrootcert: rootcert.map(|f| cert(f).display().to_string()),
    }
}

/// Connects, asks which way the query went, and lets go.
fn via(u: &Url) -> Result<String, String> {
    let mut c = Client::connect(u).map_err(|e| e.to_string())?;
    let r = c.query("select via").unwrap();
    Ok(r.rows[0][0].clone().unwrap())
}

#[test]
fn verify_full_speaks_tls_to_the_name_its_ca_signed() {
    let (port, rx) = standin(Answer::Tls);
    let u = url("localhost", port, SslMode::VerifyFull, Some("ca.pem"));
    assert_eq!(via(&u).unwrap(), "tls");
    assert_eq!(seen(port, &rx), vec![Seen::Tls { startup: true }]);
}

/// The address dialed is not the name the certificate holds: refused in the
/// handshake, before the startup message -- the user's name with it -- went
/// out, and not tried again in plain text.
#[test]
fn verify_full_refuses_a_name_the_certificate_does_not_hold_before_a_byte_is_sent() {
    let (port, rx) = standin(Answer::Tls);
    let e = via(&url("127.0.0.1", port, SslMode::VerifyFull, Some("ca.pem"))).unwrap_err();
    assert!(e.contains("not valid for name"), "{e}");
    let saw = seen(port, &rx);
    assert!(matches!(&saw[..], [Seen::HandshakeFailed(_)]), "{saw:?}");
}

#[test]
fn verify_full_refuses_an_unknown_issuer_before_a_byte_is_sent() {
    let (port, rx) = standin(Answer::Tls);
    let e = via(&url(
        "localhost",
        port,
        SslMode::VerifyFull,
        Some("other-ca.pem"),
    ))
    .unwrap_err();
    assert!(e.contains("UnknownIssuer"), "{e}");
    let saw = seen(port, &rx);
    assert!(matches!(&saw[..], [Seen::HandshakeFailed(_)]), "{saw:?}");

    // Mozilla's roots, without an sslrootcert, never signed the test CA.
    let (port, rx) = standin(Answer::Tls);
    let e = via(&url("localhost", port, SslMode::VerifyFull, None)).unwrap_err();
    assert!(e.contains("UnknownIssuer"), "{e}");
    let saw = seen(port, &rx);
    assert!(matches!(&saw[..], [Seen::HandshakeFailed(_)]), "{saw:?}");
}

/// `verify-ca` checks the chain and not the name; `require` checks nothing,
/// as libpq's does not -- unless a CA file is named, which makes it a
/// `verify-ca`, as libpq does.
#[test]
fn verify_ca_checks_the_chain_and_require_checks_nothing() {
    for (mode, root, ok) in [
        (SslMode::VerifyCa, Some("ca.pem"), true),
        (SslMode::VerifyCa, Some("other-ca.pem"), false),
        (SslMode::Require, None, true),
        (SslMode::Require, Some("ca.pem"), true),
        (SslMode::Require, Some("other-ca.pem"), false),
        (SslMode::Prefer, None, true),
    ] {
        let (port, rx) = standin(Answer::Tls);
        let got = via(&url("127.0.0.1", port, mode, root));
        let saw = seen(port, &rx);
        if ok {
            assert_eq!(got.as_deref(), Ok("tls"), "{mode:?} {root:?}");
            assert_eq!(saw, vec![Seen::Tls { startup: true }], "{mode:?} {root:?}");
        } else {
            let e = got.unwrap_err();
            assert!(e.contains("UnknownIssuer"), "{mode:?} {root:?}: {e}");
            assert!(matches!(&saw[..], [Seen::HandshakeFailed(_)]), "{saw:?}");
        }
    }
}

/// A server that answers `N`: `prefer` goes on in plain text on the same
/// connection, every mode that asks for TLS stops there.
#[test]
fn a_server_without_tls_is_spoken_to_in_plain_text_only_where_preferred() {
    let (port, rx) = standin(Answer::No);
    assert_eq!(
        via(&url("localhost", port, SslMode::Prefer, None)).unwrap(),
        "plain"
    );
    assert_eq!(seen(port, &rx), vec![Seen::Refused { startup: true }]);

    for mode in [SslMode::Require, SslMode::VerifyFull] {
        let (port, rx) = standin(Answer::No);
        let e = via(&url("localhost", port, mode, Some("ca.pem"))).unwrap_err();
        assert!(e.contains("does not take TLS"), "{e}");
        assert_eq!(seen(port, &rx), vec![Seen::Refused { startup: false }]);
    }
}

/// `disable` sends no `SSLRequest` at all.
#[test]
fn disable_asks_for_no_tls() {
    let (port, rx) = standin(Answer::Tls);
    assert_eq!(
        via(&url("localhost", port, SslMode::Disable, None)).unwrap(),
        "plain"
    );
    assert_eq!(seen(port, &rx), vec![Seen::Plain { startup: true }]);
}

/// libpq's `prefer` goes again without TLS, on a new connection, when the
/// handshake fails; `require` does not.
#[test]
fn prefer_goes_again_in_plain_text_when_the_handshake_fails() {
    let (port, rx) = standin(Answer::HangUp);
    assert_eq!(
        via(&url("localhost", port, SslMode::Prefer, None)).unwrap(),
        "plain"
    );
    assert_eq!(
        seen(port, &rx),
        vec![Seen::HungUp, Seen::Plain { startup: true }]
    );

    let (port, rx) = standin(Answer::HangUp);
    assert!(via(&url("localhost", port, SslMode::Require, None)).is_err());
    assert_eq!(seen(port, &rx), vec![Seen::HungUp]);
}

#[test]
fn an_error_in_answer_to_the_request_is_the_servers() {
    let (port, rx) = standin(Answer::Error);
    let e = via(&url("localhost", port, SslMode::Require, None)).unwrap_err();
    assert!(
        e.contains("08P01") && e.contains("no TLS before 7.0"),
        "{e}"
    );
    assert_eq!(seen(port, &rx), vec![Seen::Errored]);
}

/// A CA file that is not there, or holds no certificate, is said before
/// anything is dialed.
#[test]
fn a_ca_file_that_cannot_be_read_is_an_error_before_the_dial() {
    let (port, rx) = standin(Answer::Tls);
    let mut u = url("localhost", port, SslMode::VerifyFull, None);
    u.sslrootcert = Some(cert("missing.pem").display().to_string());
    let e = via(&u).unwrap_err();
    assert!(e.contains("missing.pem"), "{e}");
    u.sslrootcert = Some(cert("server.key").display().to_string());
    let e = via(&u).unwrap_err();
    assert!(e.contains("no CA certificate"), "{e}");
    assert_eq!(seen(port, &rx), vec![]);
}
