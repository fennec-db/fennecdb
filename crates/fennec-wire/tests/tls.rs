//! The client's TLS against a stand-in that takes PostgreSQL's `SSLRequest`:
//! each `sslmode`, a server's `S`, `N` or error, a handshake cut short, and a
//! certificate refused before the startup message -- user, database and
//! all -- is sent, and a replication stream's end held to its wait. A live
//! PostgreSQL with `ssl = on` is `make import-test`'s.
//!
//! A dial that checks a certificate and names no CA file reads libpq's
//! `~/.postgresql/root.crt`, the machine's: such a dial is made only in a
//! child whose HOME is the test's own
//! (`libpqs_root_file_is_read_where_none_is_named`).
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
use std::sync::atomic::AtomicBool;
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
                (b'M', "words of whoever is in the path"),
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
    url_as(
        host,
        port,
        &format!("sslmode={}", sslmode.name()),
        rootcert,
        None,
    )
}

/// A URL of the stand-in's, `query` its parameters, `rootcert` a file of
/// `tests/certs`, `password` the user's: read with no variable of libpq's,
/// so a `PGPASSWORD` or `PGSSLMODE` this process inherited changes nothing.
fn url_as(
    host: &str,
    port: u16,
    query: &str,
    rootcert: Option<&str>,
    password: Option<&str>,
) -> Url {
    let root = rootcert.map_or_else(String::new, |f| {
        let path = cert(f).display().to_string();
        let path = path
            .replace('%', "%25")
            .replace('&', "%26")
            .replace(' ', "%20")
            .replace('?', "%3F")
            .replace('#', "%23");
        format!("&sslrootcert={path}")
    });
    let pw = password.map_or_else(String::new, |p| format!(":{p}"));
    Url::parse_in(
        &format!("postgres://fennec{pw}@{host}:{port}/fennec?{query}{root}"),
        |_| None,
    )
    .unwrap()
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
}

/// `verify-ca` checks the chain and not the name; a CA file named makes
/// `require` a `verify-ca`, as libpq does. Without one, `require` checks
/// nothing: `libpqs_root_file_is_read_where_none_is_named`, whose HOME is
/// its own.
#[test]
fn verify_ca_checks_the_chain_and_require_checks_nothing() {
    for (mode, root, ok) in [
        (SslMode::VerifyCa, Some("ca.pem"), true),
        (SslMode::VerifyCa, Some("other-ca.pem"), false),
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
/// handshake fails (fe-connect.c's CONNECTION_FAILED after the handshake,
/// plain text the next method `prefer` allows); `require` does not.
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
    assert!(via(&url("localhost", port, SslMode::Require, Some("ca.pem"))).is_err());
    assert_eq!(seen(port, &rx), vec![Seen::HungUp]);
}

/// An error in answer to the request is said to be one, its text never
/// shown: it comes before TLS and any password, from whoever is in the path
/// (libpq 17.2, CVE-2024-10977). Nor is it tried again in plain text,
/// `prefer` or not, as libpq does not.
#[test]
fn an_error_in_answer_to_the_request_is_not_shown() {
    for (mode, root) in [(SslMode::Require, Some("ca.pem")), (SslMode::Prefer, None)] {
        let (port, rx) = standin(Answer::Error);
        let e = via(&url("localhost", port, mode, root)).unwrap_err();
        assert!(e.contains("answered the TLS request with an error"), "{e}");
        assert!(
            !e.contains("whoever is in the path") && !e.contains("08P01"),
            "{mode:?}: {e}"
        );
        assert_eq!(seen(port, &rx), vec![Seen::Errored], "{mode:?}");
    }
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

/// Set in a child of `libpqs_root_file_is_read_where_none_is_named`: what it
/// dials, `<host> <port> <query>`, its HOME (and APPDATA) the parent's
/// directory, so no other test of this process reads that file.
const DIAL_IN_HOME: &str = "FENNEC_WIRE_DIAL_IN_HOME";

/// The child's half: one dial, its outcome on a line of its own. Run alone,
/// it does nothing.
#[test]
fn dial_in_a_home_of_its_own() {
    let Ok(what) = std::env::var(DIAL_IN_HOME) else {
        return;
    };
    let mut it = what.splitn(3, ' ');
    let (host, port, query) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    let got = Url::parse_in(
        &format!("postgres://fennec@{host}:{port}/fennec?{query}"),
        |_| None,
    )
    .map_err(|e| e.to_string())
    .and_then(|u| via(&u));
    println!("dialed: {got:?}");
}

/// libpq's root file, `~/.postgresql/root.crt` (under `%APPDATA%` on
/// Windows), is read where no `sslrootcert` is named: there, it
/// makes `require` a `verify-ca` and is what `verify-ca` and `verify-full`
/// trust; not there, `require` checks nothing, `verify-ca` is refused before
/// the dial and `verify-full` takes Mozilla's roots. `prefer` reads none, and
/// a file named goes before it.
#[test]
fn libpqs_root_file_is_read_where_none_is_named() {
    use std::process::Command;
    struct Home(PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let home =
        Home(std::env::temp_dir().join(format!("fennec-wire-root-crt-{}", std::process::id())));
    let lay = |ca: Option<&str>| {
        let _ = std::fs::remove_dir_all(&home.0);
        for dir in [".postgresql", "postgresql"] {
            let dir = home.0.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some(ca) = ca {
                std::fs::copy(cert(ca), dir.join("root.crt")).unwrap();
            }
        }
    };
    let dial = |host: &str, query: &str| -> (Result<String, String>, Vec<Seen>) {
        let (port, rx) = standin(Answer::Tls);
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tls::dial_in_a_home_of_its_own"])
            .args(["--nocapture", "--test-threads=1"])
            .env(DIAL_IN_HOME, format!("{host} {port} {query}"))
            .env("HOME", &home.0)
            .env("APPDATA", &home.0)
            .env_remove("PGSSLROOTCERT")
            .env_remove("PGSSLMODE")
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{said}");
        // The harness writes the test's name ahead of it on the line.
        let line = said
            .lines()
            .find_map(|l| l.split_once("dialed: ").map(|(_, d)| d))
            .unwrap_or_else(|| panic!("no dial in: {said}"));
        let got = match line.strip_prefix("Ok(\"") {
            Some(ok) => Ok(ok.trim_end_matches("\")").to_string()),
            None => Err(line.to_string()),
        };
        (got, seen(port, &rx))
    };
    let tls = || (Ok("tls".to_string()), vec![Seen::Tls { startup: true }]);
    let refused = |(got, saw): (Result<String, String>, Vec<Seen>), why: &str| {
        let e = got.unwrap_err();
        assert!(e.contains(why), "{e}");
        assert!(matches!(&saw[..], [Seen::HandshakeFailed(_)]), "{saw:?}");
    };

    // No root file.
    lay(None);
    assert_eq!(dial("127.0.0.1", "sslmode=require"), tls());
    let (got, saw) = dial("127.0.0.1", "sslmode=verify-ca");
    assert!(
        got.as_ref().unwrap_err().contains("needs sslrootcert"),
        "{got:?}"
    );
    assert_eq!(saw, vec![], "refused before the dial");
    // Mozilla's roots never signed the test CA.
    refused(dial("localhost", "sslmode=verify-full"), "UnknownIssuer");

    // The test CA's: the chain is checked, and the name by verify-full alone.
    lay(Some("ca.pem"));
    for (host, mode) in [
        ("127.0.0.1", "require"),
        ("127.0.0.1", "verify-ca"),
        ("localhost", "verify-full"),
    ] {
        assert_eq!(dial(host, &format!("sslmode={mode}")), tls(), "{mode}");
    }

    // A CA that signed nothing here: `require` refused, as `verify-ca` is.
    lay(Some("other-ca.pem"));
    refused(dial("127.0.0.1", "sslmode=require"), "UnknownIssuer");
    refused(dial("localhost", "sslmode=verify-full"), "UnknownIssuer");
    assert_eq!(dial("127.0.0.1", "sslmode=prefer"), tls());
    let named = cert("ca.pem").display().to_string().replace(' ', "%20");
    assert_eq!(
        dial("127.0.0.1", &format!("sslmode=require&sslrootcert={named}")),
        tls()
    );
}

/// One message, its tag and body, or `None` at the end of the connection.
fn message(s: &mut impl Read) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    s.read_exact(&mut head).ok()?;
    let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize - 4;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some((head[0], body))
}

/// Over TLS a read in the middle of a message is held to the wait for the
/// stream's end as well: a stand-in walsender that answers the client's
/// `CopyDone` with the first bytes of a message and then nothing is waited
/// for no longer than asked, where rustls's read of the rest waited out the
/// socket's own timeout, 30 s. The connection is then closed.
#[test]
fn ending_a_stream_over_tls_is_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server =
        fennec_tls::Server::from_pem_files(&cert("server.pem"), &cert("server.key")).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut request = [0u8; 8];
        s.read_exact(&mut request).unwrap();
        s.write_all(b"S").unwrap();
        let mut t = server.accept(s).unwrap();
        assert!(startup(&mut t));
        let mut out = framed(b'R', &0i32.to_be_bytes());
        out.extend_from_slice(&framed(b'Z', b"I"));
        t.write_all(&out).unwrap();
        t.flush().unwrap();
        // START_REPLICATION, then up to the CopyDone.
        while let Some((tag, _)) = message(&mut t) {
            match tag {
                b'Q' => {
                    t.write_all(&framed(b'W', &[0, 0, 0])).unwrap();
                    t.flush().unwrap();
                }
                b'c' => break,
                _ => {}
            }
        }
        let mut out = vec![b'd'];
        out.extend_from_slice(&1004i32.to_be_bytes());
        out.extend_from_slice(&[b'w'; 10]);
        t.write_all(&out).unwrap();
        t.flush().unwrap();
        tx.send(message(&mut t).map(|(tag, _)| tag)).unwrap();
    });
    let u = url("localhost", port, SslMode::VerifyFull, Some("ca.pem"));
    let wal = Client::connect_replication(&u)
        .unwrap()
        .start_replication("fennec_docs", "fennec_docs")
        .unwrap();
    let e = wal
        .end(Duration::from_millis(200), &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("did not end the replication stream within"),
        "{e}"
    );
    assert_eq!(rx.recv().unwrap(), Some(b'X'), "no Terminate after the end");
}

// ---------------------------------------------------- channel binding

/// What a stand-in signing in by SCRAM over TLS saw of the client's
/// exchange: the mechanism it chose, its GS2 header, and whether its `c=`
/// was that header followed -- for `p=tls-server-end-point` -- by the hash
/// of the stand-in's own certificate, RFC 5929's binding.
#[derive(Debug, PartialEq)]
struct Bound {
    mechanism: String,
    gs2: String,
    cbind_ok: bool,
    signed_in: bool,
}

/// RFC 5929's `tls-server-end-point` of the test server's certificate: its
/// signature is ECDSA with SHA-256 (rcgen's P-256 key, `wire_certs`), so
/// SHA-256 of its DER.
fn end_point() -> Vec<u8> {
    let pem = std::fs::read(cert("server.pem")).unwrap();
    let der = fennec_tls::pem_certs(&pem).unwrap().remove(0);
    fennec_http::crypto::sha256(&der).to_vec()
}

/// A stand-in over TLS that offers the SASL mechanisms `offer` and runs
/// SCRAM's server half for password `p`: what it saw of the client's
/// exchange, `None` where the client sent none.
fn scram_over_tls(offer: &'static [&'static str]) -> (u16, mpsc::Receiver<Option<Bound>>) {
    use crate::client::{attr, Exchange, Verifier};
    use fennec_http::crypto::{b64_encode, nonce, system_random};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server =
        fennec_tls::Server::from_pem_files(&cert("server.pem"), &cert("server.key")).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut request = [0u8; 8];
        s.read_exact(&mut request).unwrap();
        s.write_all(b"S").unwrap();
        let mut t = server.accept(s).unwrap();
        assert!(startup(&mut t));
        let mut b = 10i32.to_be_bytes().to_vec();
        for m in offer {
            b.extend_from_slice(m.as_bytes());
            b.push(0);
        }
        b.push(0);
        t.write_all(&framed(b'R', &b)).unwrap();
        t.flush().unwrap();
        let Some((b'p', body)) = message(&mut t) else {
            return tx.send(None).unwrap();
        };
        let name_end = body.iter().position(|&c| c == 0).unwrap();
        let mechanism = String::from_utf8_lossy(&body[..name_end]).into_owned();
        let first = String::from_utf8_lossy(&body[name_end + 1 + 4..]).into_owned();
        let mut parts = first.splitn(3, ',');
        let gs2 = format!("{},{},", parts.next().unwrap(), parts.next().unwrap());
        let v = Verifier::new("p", system_random(16).unwrap());
        let (ex, server_first) = Exchange::client_first(&v, &first, &nonce(18).unwrap());
        let mut b = 11i32.to_be_bytes().to_vec();
        b.extend_from_slice(server_first.as_bytes());
        t.write_all(&framed(b'R', &b)).unwrap();
        t.flush().unwrap();
        let Some((b'p', last)) = message(&mut t) else {
            return tx.send(None).unwrap();
        };
        let last = String::from_utf8_lossy(&last).into_owned();
        let mut header = gs2.as_bytes().to_vec();
        if gs2.starts_with("p=") {
            header.extend_from_slice(&end_point());
        }
        let cbind_ok = attr(&last, 'c') == Some(b64_encode(&header).as_str());
        let signed_in = match ex.client_final(&last) {
            Ok(server_final) if cbind_ok => {
                let mut b = 12i32.to_be_bytes().to_vec();
                b.extend_from_slice(server_final.as_bytes());
                let mut out = framed(b'R', &b);
                out.extend_from_slice(&framed(b'R', &0i32.to_be_bytes()));
                out.extend_from_slice(&framed(b'Z', b"I"));
                t.write_all(&out).unwrap();
                t.flush().unwrap();
                true
            }
            _ => false,
        };
        tx.send(Some(Bound {
            mechanism,
            gs2,
            cbind_ok,
            signed_in,
        }))
        .unwrap();
        // Up to the client's close.
        while message(&mut t).is_some() {}
    });
    (port, rx)
}

const BOTH: &[&str] = &["SCRAM-SHA-256-PLUS", "SCRAM-SHA-256"];

/// Over TLS, a server offering `SCRAM-SHA-256-PLUS` is signed in to with it:
/// the exchange carries the hash of the certificate the client was shown
/// (`tls-server-end-point`), which a server compares with its own, so a
/// sign-in relayed through someone else's TLS -- `require` and `prefer`
/// check no certificate -- fails at the server. The client sent `n,,` and
/// plain SCRAM whatever was offered.
#[test]
fn a_sign_in_over_tls_is_bound_to_the_servers_certificate() {
    for (mode, root) in [
        ("sslmode=verify-full", Some("ca.pem")),
        ("sslmode=require", None),
    ] {
        let (port, rx) = scram_over_tls(BOTH);
        let c = Client::connect(&url_as("localhost", port, mode, root, Some("p")));
        assert!(c.is_ok(), "{mode}: {:?}", c.err());
        assert_eq!(
            rx.recv().unwrap(),
            Some(Bound {
                mechanism: "SCRAM-SHA-256-PLUS".into(),
                gs2: "p=tls-server-end-point,,".into(),
                cbind_ok: true,
                signed_in: true,
            }),
            "{mode}"
        );
    }
}

/// A `-PLUS` offer taken out on the way: the client says it could have
/// bound (`y,,`), which the real server, having offered it, refuses; under
/// `channel_binding=require` it signs in to no such server at all, sending
/// nothing of SCRAM. `disable` binds nothing whatever is offered (`n,,`).
#[test]
fn a_plus_offer_taken_out_is_told_the_client_could_bind() {
    let (port, rx) = scram_over_tls(&["SCRAM-SHA-256"]);
    let u = url_as(
        "localhost",
        port,
        "sslmode=verify-full",
        Some("ca.pem"),
        Some("p"),
    );
    assert!(Client::connect(&u).is_ok());
    let seen = rx.recv().unwrap().unwrap();
    assert_eq!(
        (seen.mechanism.as_str(), seen.gs2.as_str()),
        ("SCRAM-SHA-256", "y,,")
    );
    assert!(seen.cbind_ok && seen.signed_in, "{seen:?}");

    let (port, rx) = scram_over_tls(&["SCRAM-SHA-256"]);
    let u = url_as(
        "localhost",
        port,
        "sslmode=verify-full&channel_binding=require",
        Some("ca.pem"),
        Some("p"),
    );
    let e = Client::connect(&u).unwrap_err().to_string();
    assert!(e.contains("does not offer SCRAM-SHA-256-PLUS"), "{e}");
    assert_eq!(rx.recv().unwrap(), None, "SCRAM began");

    let (port, rx) = scram_over_tls(BOTH);
    let u = url_as(
        "localhost",
        port,
        "sslmode=verify-full&channel_binding=disable",
        Some("ca.pem"),
        Some("p"),
    );
    assert!(Client::connect(&u).is_ok());
    let seen = rx.recv().unwrap().unwrap();
    assert_eq!(
        (seen.mechanism.as_str(), seen.gs2.as_str()),
        ("SCRAM-SHA-256", "n,,")
    );
}

/// Bytes a server -- or someone on the path -- sends after its `S`, before
/// the handshake, go into the handshake, which refuses them, and never
/// into what the client reads as the server's: here an `AuthenticationOk`
/// and a `ReadyForQuery`, which taken as the server's would have signed
/// the client in (libpq's CVE-2021-23222). The stand-in goes on to a real
/// handshake, with a certificate the client trusts: a client that kept the
/// bytes aside would finish it and read them as the server's first words
/// over TLS, signed in by them -- with no password held, nothing else is
/// asked of an `AuthenticationOk`.
#[test]
fn bytes_after_the_servers_s_are_never_read_as_the_servers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server =
        fennec_tls::Server::from_pem_files(&cert("server.pem"), &cert("server.key")).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut request = [0u8; 8];
        s.read_exact(&mut request).unwrap();
        let mut out = b"S".to_vec();
        out.extend_from_slice(&framed(b'R', &0i32.to_be_bytes()));
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();
        let seen = match server.accept(s) {
            // Over TLS the stand-in answers nothing: what signs a client
            // in here is the bytes it sent before the handshake.
            Ok(mut t) => {
                let startup = startup(&mut t);
                let _ = t.read_to_end(&mut Vec::new());
                Seen::Tls { startup }
            }
            Err(e) => Seen::HandshakeFailed(e.to_string()),
        };
        let _ = tx.send(seen);
    });
    let u = url_as(
        "localhost",
        port,
        "sslmode=verify-full",
        Some("ca.pem"),
        None,
    );
    let e = match Client::connect(&u) {
        Ok(_) => panic!("signed in by the bytes sent before the handshake"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("TLS") || e.contains("handshake"), "{e}");
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Seen::HandshakeFailed(_) => {}
        other => panic!("{other:?}: {e}"),
    }
}

/// Off this machine `verify-full` is the default: a certificate it cannot
/// verify is refused before a byte of the client's goes out, in words
/// naming the cause, what this client trusts and the ways out -- the CA to
/// trust, or TLS that checks nothing -- and a server refusing TLS is told
/// how to speak plain text to it. `prefer`, the default before, spoke to
/// either, checking nothing. On this machine the default is `prefer` still.
#[test]
fn off_this_machine_the_default_checks_the_certificate() {
    let far = |port: u16| {
        let name = format!("pg-remote-{port}.test:{port}");
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        fennec_http::dial::stand_in(&name, move || Ok(vec![addr]));
        format!("pg-remote-{port}.test")
    };
    let (port, rx) = standin(Answer::Tls);
    let u = url_as(&far(port), port, "", Some("other-ca.pem"), None);
    assert_eq!(u.sslmode, SslMode::VerifyFull);
    let e = via(&u).unwrap_err();
    for says in [
        "could not be verified",
        "UnknownIssuer",
        "sslmode=verify-full is the default for a host off this machine",
        "other-ca.pem",
        "sslrootcert=<file>",
        "sslmode=require",
    ] {
        assert!(e.contains(says), "{says}: {e}");
    }
    let saw = seen(port, &rx);
    assert!(matches!(&saw[..], [Seen::HandshakeFailed(_)]), "{saw:?}");

    let (port, rx) = standin(Answer::No);
    let e = via(&url_as(&far(port), port, "", None, None)).unwrap_err();
    assert!(
        e.contains("verify-full is the default for a host off this machine")
            && e.contains("sslmode=disable"),
        "{e}"
    );
    assert_eq!(seen(port, &rx), vec![Seen::Refused { startup: false }]);

    let (port, rx) = standin(Answer::No);
    assert_eq!(
        via(&url_as("127.0.0.1", port, "", None, None)).unwrap(),
        "plain"
    );
    assert_eq!(seen(port, &rx), vec![Seen::Refused { startup: true }]);
}
