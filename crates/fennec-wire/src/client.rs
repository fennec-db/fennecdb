//! PostgreSQL v3 wire protocol -- client side.
//!
//! Connects to a real PostgreSQL server. The subset needed for import:
//! connect, authenticate (SCRAM-SHA-256, the client half, or a cleartext
//! password), simple query and `COPY ... TO STDOUT` -- and for `--follow`,
//! a logical replication stream ([`WalStream`]).
//!
//! Framing comes from [`crate::proto`], crypto from [`crate::crypto`].
//!
//! TLS is asked for as libpq asks (`SSLRequest`), and what is checked of the
//! certificate is the connection string's `sslmode` ([`SslMode`]), libpq's
//! five but `allow`: `prefer` by default, which encrypts where the server
//! can and checks nothing, so only `verify-full` keeps a stand-in for the
//! server out. TLS is fennec-tls's, behind the `tls` feature.

use crate::crypto::{b64_decode, b64_encode, ct_eq, hmac_sha256, nonce, pbkdf2_sha256, sha256};
use crate::proto::{put_cstr, read_message, Message, Writer, PROTOCOL_V3};
use fennec_core::error::{Error, Result};
use std::io::{self as stdio, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The timeout used while connecting and while waiting for a message.
const TIMEOUT: Duration = Duration::from_secs(30);

fn io(e: impl std::fmt::Display) -> Error {
    Error::Io(format!("postgres: {e}"))
}

// -------------------------------------------------------------------- url

/// Whether TLS is asked for, and what of the server's certificate is
/// checked: libpq's `sslmode`, each as libpq defines it. Only `verify-full`
/// keeps out someone standing in for the server; `require` checks nothing
/// without a CA file, as libpq's does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SslMode {
    /// Plain text; no `SSLRequest` is sent.
    Disable,
    /// TLS where the server takes it, plain text where it answers `N` or
    /// its handshake fails; no certificate checked. libpq's default, and
    /// plain text alone in a build without the `tls` feature, as in libpq
    /// built without SSL.
    #[default]
    Prefer,
    /// TLS or no connection; no certificate checked -- unless there is a
    /// CA file, `sslrootcert`'s or libpq's own `~/.postgresql/root.crt`,
    /// which makes it `verify-ca`, as libpq does.
    Require,
    /// TLS, the chain checked up to the CA file's CAs (`sslrootcert`, else
    /// `~/.postgresql/root.crt`), the name not.
    VerifyCa,
    /// TLS, the chain checked and the host dialed held to the certificate:
    /// against the CA file, else Mozilla's roots.
    VerifyFull,
}

impl SslMode {
    fn parse(s: &str) -> Result<SslMode> {
        Ok(match s {
            "disable" => SslMode::Disable,
            "prefer" => SslMode::Prefer,
            "require" => SslMode::Require,
            "verify-ca" => SslMode::VerifyCa,
            "verify-full" => SslMode::VerifyFull,
            "allow" => {
                return Err(Error::Query(
                    "sslmode=allow is not supported: use prefer, which tries TLS first, or disable"
                        .into(),
                ))
            }
            other => {
                return Err(Error::Query(format!(
                    "unknown sslmode `{other}` (disable, prefer, require, verify-ca or verify-full)"
                )))
            }
        })
    }

    /// The name as a connection string writes it.
    pub fn name(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        }
    }
}

/// `postgres://user[:password]@host[:port]/database[?sslmode=...&sslrootcert=...]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub user: String,
    pub password: Option<String>,
    pub host: String,
    pub port: u16,
    pub database: String,
    /// `sslmode`, else `PGSSLMODE`, else `prefer`.
    pub sslmode: SslMode,
    /// `sslrootcert`, else `PGSSLROOTCERT`: a PEM file of the CAs the
    /// `verify-*` modes and `require` trust, or `system` for Mozilla's
    /// roots compiled in (webpki-roots). Where none is named, libpq's
    /// `~/.postgresql/root.crt` is read at the dial if it is there, and
    /// `verify-full` takes Mozilla's roots if it is not.
    pub sslrootcert: Option<String>,
}

impl Url {
    /// Parses the connection string. Without a password `PGPASSWORD` is
    /// used, and without a user `PGUSER` or `USER`; without `sslmode` or
    /// `sslrootcert`, `PGSSLMODE` and `PGSSLROOTCERT`. Any other parameter
    /// is refused rather than passed over.
    pub fn parse(s: &str) -> Result<Url> {
        let rest = s
            .strip_prefix("postgresql://")
            .or_else(|| s.strip_prefix("postgres://"))
            .ok_or_else(|| {
                Error::Query(format!(
                    "`{s}` is not a PostgreSQL connection string \
                     (expected postgres://user@host:port/database)"
                ))
            })?;
        // `sslmode` and `sslrootcert` are read; any other parameter is
        // refused, never swallowed: a `sslcert` passed over would leave a
        // server asking for a client certificate to refuse a connection
        // for no reason the user could see.
        let (rest, params) = match rest.split_once('?') {
            Some((r, p)) => (r, p),
            None => (rest, ""),
        };
        let (mut sslmode, mut sslrootcert) = (None, None);
        for pair in params.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some(("sslmode", v)) => sslmode = Some(SslMode::parse(&decode(v))?),
                Some(("sslrootcert", v)) if !v.is_empty() => sslrootcert = Some(decode(v)),
                _ => {
                    return Err(Error::Query(format!(
                        "connection parameter `{pair}` is not supported \
                         (sslmode and sslrootcert are)"
                    )))
                }
            }
        }
        let sslmode = match sslmode {
            Some(m) => m,
            None => match std::env::var("PGSSLMODE") {
                Ok(m) if !m.is_empty() => SslMode::parse(&m)?,
                _ => SslMode::default(),
            },
        };
        let sslrootcert = sslrootcert.or_else(|| {
            std::env::var("PGSSLROOTCERT")
                .ok()
                .filter(|r| !r.is_empty())
        });
        // libpq's rule: Mozilla's roots vouch for every public name, so
        // they are taken only where the name is checked too. `verify-ca`
        // without a file named is answered at the dial, where libpq's own
        // root file is looked for (`tls_client`).
        match (sslmode, sslrootcert.as_deref()) {
            (SslMode::VerifyFull | SslMode::Disable, _) => {}
            (_, Some("system")) => {
                return Err(Error::Query(format!(
                    "sslrootcert=system takes sslmode=verify-full, not {}: \
                     any certificate a public CA signed would pass",
                    sslmode.name()
                )))
            }
            _ => {}
        }
        // The user info runs up to the last `@`; the host contains no `@`.
        let (userinfo, authority) = match rest.rsplit_once('@') {
            Some((u, a)) => (Some(u), a),
            None => (None, rest),
        };
        let (hostport, database) = match authority.split_once('/') {
            Some((h, d)) => (h, d),
            None => (authority, ""),
        };
        if database.is_empty() {
            return Err(Error::Query(
                "the connection string has no database name (`.../database`)".into(),
            ));
        }
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (
                h,
                p.parse::<u16>()
                    .map_err(|_| Error::Query(format!("invalid port `{p}`")))?,
            ),
            None => (hostport, 5432),
        };

        let (user, password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((u, p)) => (decode(u), Some(decode(p))),
                None => (decode(u), None),
            },
            None => (String::new(), None),
        };
        let user = if user.is_empty() {
            std::env::var("PGUSER")
                .or_else(|_| std::env::var("USER"))
                .map_err(|_| Error::Query("the connection string has no user name".into()))?
        } else {
            user
        };
        Ok(Url {
            user,
            password: password.or_else(|| std::env::var("PGPASSWORD").ok()),
            host: if host.is_empty() { "127.0.0.1" } else { host }.to_string(),
            port,
            database: decode(database),
            sslmode,
            sslrootcert,
        })
    }
}

/// Decodes percent escapes; this is how `@` and `:` travel in passwords.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ----------------------------------------------------------------- messages

/// A single column inside `RowDescription`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDesc {
    pub name: String,
    /// The type OID. Extension types (pgvector) are not fixed and are
    /// learned from `pg_type`.
    pub oid: i32,
    /// The type modifier. In pgvector it is the vector dimension; -1 when unknown.
    pub typmod: i32,
}

/// A simple query result; the values always arrive in text format.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<FieldDesc>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// One field from an `ErrorResponse` body.
fn field(m: &Message, key: u8) -> Option<String> {
    let mut pos = 0;
    while pos < m.body.len() && m.body[pos] != 0 {
        let k = m.body[pos];
        pos += 1;
        let start = pos;
        while pos < m.body.len() && m.body[pos] != 0 {
            pos += 1;
        }
        if k == key {
            return Some(String::from_utf8_lossy(&m.body[start..pos]).into_owned());
        }
        pos += 1;
    }
    None
}

fn server_error(m: &Message) -> Error {
    let code = field(m, b'C').unwrap_or_default();
    let msg = field(m, b'M').unwrap_or_default();
    Error::Query(format!("postgres {code}: {msg}"))
}

/// A count of columns, which the protocol writes as an Int16 that is never
/// negative: read as one, a server's `0xFFFF` became `usize::MAX` and the
/// `Vec::with_capacity` sized by it panicked, where as a `u16` it is at most
/// 65 535 columns' room and the body, too short for them, an error.
fn be_count(b: &[u8], at: usize) -> usize {
    u16::from_be_bytes([b[at], b[at + 1]]) as usize
}

fn be_i32(b: &[u8], at: usize) -> i32 {
    i32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `RowDescription` -> columns.
///
/// For each column: name, table oid (4), column number (2), type oid (4),
/// type length (2), type modifier (4), format (2) -- 18 bytes after the name.
fn row_description(m: &Message) -> Result<Vec<FieldDesc>> {
    let short = || Error::Corrupt("postgres: RowDescription too short".into());
    if m.body.len() < 2 {
        return Err(short());
    }
    let n = be_count(&m.body, 0);
    let mut pos = 2;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let start = pos;
        while pos < m.body.len() && m.body[pos] != 0 {
            pos += 1;
        }
        let name = String::from_utf8_lossy(&m.body[start..pos]).into_owned();
        pos += 1;
        if pos + 18 > m.body.len() {
            return Err(short());
        }
        out.push(FieldDesc {
            name,
            oid: be_i32(&m.body, pos + 6),
            typmod: be_i32(&m.body, pos + 12),
        });
        pos += 18;
    }
    Ok(out)
}

/// `DataRow` -> cells.
fn data_row(m: &Message) -> Result<Vec<Option<String>>> {
    let short = || Error::Corrupt("postgres: DataRow too short".into());
    if m.body.len() < 2 {
        return Err(short());
    }
    let n = be_count(&m.body, 0);
    let mut pos = 2;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        if pos + 4 > m.body.len() {
            return Err(short());
        }
        let len = be_i32(&m.body, pos);
        pos += 4;
        if len < 0 {
            out.push(None);
            continue;
        }
        let end = pos + len as usize;
        let cell = m.body.get(pos..end).ok_or_else(short)?;
        out.push(Some(String::from_utf8_lossy(cell).into_owned()));
        pos = end;
    }
    Ok(out)
}

// ------------------------------------------------------------------ client

/// What a client reads and writes: the socket, or TLS over it.
#[derive(Debug)]
enum Conn {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Tls(Box<fennec_tls::TlsStream>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> stdio::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(buf),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> stdio::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> stdio::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.flush(),
        }
    }
}

impl Conn {
    /// Whether a message has begun to arrive, waiting up to `wait`; an
    /// `UnexpectedEof` error once the server closed. Never a read that can
    /// time out halfway through a message, which would leave the rest of it
    /// to be taken for the start of the next one: on a plain socket a peek,
    /// over TLS rustls taking whole records in (`TlsStream::readable`).
    fn readable(&mut self, wait: Duration) -> stdio::Result<bool> {
        match self {
            Conn::Plain(sock) => {
                // A zero timeout is refused by the socket, and means
                // "forever" to the kernel.
                sock.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
                let peeked = sock.peek(&mut [0u8; 1]);
                sock.set_read_timeout(Some(TIMEOUT))?;
                match peeked {
                    Ok(0) => Err(stdio::Error::new(ErrorKind::UnexpectedEof, "closed")),
                    Ok(_) => Ok(true),
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                        Ok(false)
                    }
                    Err(e) => Err(e),
                }
            }
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.readable(wait),
        }
    }
}

fn dial(url: &Url) -> Result<TcpStream> {
    let stream = TcpStream::connect((url.host.as_str(), url.port))
        .map_err(|e| io(format!("{}:{} -- {e}", url.host, url.port)))?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_nodelay(true).ok();
    Ok(stream)
}

/// The stream a connection speaks over: plain, or TLS as `sslmode` asks,
/// its handshake done before the startup message -- user, database and
/// all -- is sent.
#[cfg(not(feature = "tls"))]
fn negotiate(url: &Url) -> Result<Conn> {
    match url.sslmode {
        // libpq built without SSL takes `prefer` and sends no SSLRequest.
        SslMode::Disable | SslMode::Prefer => Ok(Conn::Plain(dial(url)?)),
        mode => Err(Error::Query(format!(
            "sslmode={} needs TLS, which this build was made without (the `tls` feature)",
            mode.name()
        ))),
    }
}

/// The stream a connection speaks over: plain, or TLS as `sslmode` asks,
/// its handshake done before the startup message -- user, database and
/// all -- is sent.
#[cfg(feature = "tls")]
fn negotiate(url: &Url) -> Result<Conn> {
    if url.sslmode == SslMode::Disable {
        return Ok(Conn::Plain(dial(url)?));
    }
    // The CA file is read before anything is dialed.
    let tls = tls_client(url)?;
    let mut sock = dial(url)?;
    let mut request = 8i32.to_be_bytes().to_vec();
    request.extend_from_slice(&crate::proto::SSL_REQUEST.to_be_bytes());
    sock.write_all(&request).map_err(io)?;
    // One byte, read alone off the socket: whatever a server, or someone
    // in the path, put after an `S` goes into the handshake, which refuses
    // it, never into what is read as the server's (libpq's
    // CVE-2021-23222).
    let mut answer = [0u8; 1];
    sock.read_exact(&mut answer).map_err(io)?;
    match answer[0] {
        b'S' => match tls.dial(sock, &url.host) {
            Ok(s) => Ok(Conn::Tls(Box::new(s))),
            // libpq's `prefer` goes again without TLS when the handshake
            // fails, on a new connection: fe-connect.c's CONNECTION_FAILED
            // after `pqsecure_open_client` ("SSL handshake failed. We will
            // retry with a plaintext connection, if permitted by sslmode"),
            // whose next method for `prefer` is plain text. Failing here
            // instead would keep out no one: whoever can break the
            // handshake can as well answer `N`, which `prefer` takes.
            Err(_) if url.sslmode == SslMode::Prefer => Ok(Conn::Plain(dial(url)?)),
            // The network's failure is an `Io`, which a follower waits out
            // and dials again after; a certificate refused or a handshake
            // the server would not finish is a setting to change, said once.
            Err(fennec_tls::Error::Io(e)) => Err(io(e)),
            Err(e) => Err(Error::Query(format!("postgres: {e}"))),
        },
        b'N' if url.sslmode == SslMode::Prefer => Ok(Conn::Plain(sock)),
        b'N' => Err(Error::Query(format!(
            "postgres: the server does not take TLS (its `ssl` is off), and sslmode={} asks for it",
            url.sslmode.name()
        ))),
        // A server that cannot take the request (one that failed to fork a
        // backend, say) answers with an error. Its text is never read, let
        // alone shown: it comes before TLS and before any password, so
        // anyone in the path can write it, and a log or a terminal showing
        // it shows the attacker's words as the server's (libpq stopped
        // showing it in 17.2, CVE-2024-10977). Not tried again in plain
        // text, `prefer` or not, as libpq does not.
        b'E' => Err(Error::Query(
            "postgres: the server answered the TLS request with an error, \
             whose text is not shown: it comes before TLS, from anyone in the path"
                .into(),
        )),
        other => Err(Error::Corrupt(format!(
            "postgres: `{}` in answer to the TLS request",
            other.escape_ascii()
        ))),
    }
}

/// libpq's root file, read where no `sslrootcert` is named:
/// `~/.postgresql/root.crt`, `%APPDATA%\postgresql\root.crt` on Windows,
/// taken where something stands there, as libpq's `stat` takes it -- a file
/// that then does not read as CAs is an error, never passed over. No home
/// is no file, as in libpq.
#[cfg(feature = "tls")]
fn default_root() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let dir = std::env::var_os("APPDATA")
        .filter(|d| !d.is_empty())
        .map(|d| std::path::PathBuf::from(d).join("postgresql"));
    #[cfg(not(windows))]
    let dir = std::env::home_dir()
        .filter(|h| !h.as_os_str().is_empty())
        .map(|h| h.join(".postgresql"));
    let file = dir?.join("root.crt");
    if std::fs::metadata(&file).is_ok() {
        Some(file)
    } else {
        None
    }
}

/// What a dial trusts and checks, by `sslmode` and `sslrootcert`, else
/// libpq's root file ([`default_root`]).
#[cfg(feature = "tls")]
fn tls_client(url: &Url) -> Result<fennec_tls::Client> {
    use std::path::PathBuf;
    // `prefer` checks nothing whatever file there is: libpq's would check a
    // chain against one, then go again in plain text where it fails.
    let checks = !matches!(url.sslmode, SslMode::Disable | SslMode::Prefer);
    let (file, said): (Option<PathBuf>, &str) = match url.sslrootcert.as_deref() {
        Some("system") => (None, "sslrootcert"),
        Some(f) => (Some(PathBuf::from(f)), "sslrootcert"),
        None if checks => (default_root(), "root certificate"),
        None => (None, "sslrootcert"),
    };
    let roots = || -> Result<fennec_tls::Client> {
        let Some(path) = &file else {
            return Ok(fennec_tls::Client::new());
        };
        fennec_tls::Client::trusting_pem_file(path).map_err(|e| match e {
            fennec_tls::Error::Io(e) => Error::Io(format!("{said} {e}")),
            e => Error::Query(format!("{said} {e}")),
        })
    };
    Ok(match url.sslmode {
        SslMode::Disable | SslMode::Prefer => fennec_tls::Client::unverified(),
        // libpq's rule: a CA file, named or its own root file, makes
        // `require` a `verify-ca`.
        SslMode::Require if file.is_some() => roots()?.chain_only(),
        SslMode::Require => fennec_tls::Client::unverified(),
        SslMode::VerifyCa if file.is_none() => {
            return Err(Error::Query(
                "sslmode=verify-ca needs sslrootcert=<file> or ~/.postgresql/root.crt, \
                 the CAs whose chain is checked (verify-full checks against the public \
                 CAs without one)"
                    .into(),
            ))
        }
        SslMode::VerifyCa => roots()?.chain_only(),
        // Mozilla's roots where no file is named nor libpq's is there.
        SslMode::VerifyFull => roots()?,
    })
}

// ------------------------------------------------------------------- SCRAM

/// The most PBKDF2 iterations SCRAM runs for a server: 256 times
/// PostgreSQL's 4 096, its `scram_iterations` default. The count is the
/// server's to choose -- and, under any `sslmode` short of `verify-full`,
/// whoever stands in for it -- and is run before the proof goes out:
/// `i=4294967295` was 54 trillion instructions of PBKDF2, more than half
/// an hour of an M1 core even at its widest (eight a cycle at 3.2 GHz),
/// `fennec import`, or `--follow` connecting again, waiting on it. The
/// bound is 7.1 thousand million. A count past it is refused naming the
/// range, never run, nor cut to fit.
pub const SCRAM_MAX_ITERATIONS: u32 = 1 << 20;

/// The client half of one SCRAM-SHA-256 exchange (RFC 5802, RFC 7677), its
/// messages made and checked apart from the connection, the nonce given:
/// [`Client::scram`] gives the system's, and `scram_rfc7677_exchange` RFC
/// 7677's, holding the messages to the RFC's byte for byte.
struct Scram {
    /// The client-first message less its GS2 header: `n=<user>,r=<nonce>`.
    first_bare: String,
    /// The client's nonce, which the server's must begin with.
    nonce: String,
}

/// What the server-final message must be: `v=` and the server's
/// signature, which only a holder of the password's verifier can make.
struct ServerFinal(String);

impl Scram {
    /// `user` is written as given: the client sends none, and RFC 7677's
    /// example `user`, neither with a `,` or `=` to escape.
    fn new(user: &str, nonce: &str) -> Scram {
        Scram {
            first_bare: format!("n={user},r={nonce}"),
            nonce: nonce.to_string(),
        }
    }

    /// The client-first message: no channel binding (`n,,`).
    fn first(&self) -> String {
        format!("n,,{}", self.first_bare)
    }

    /// The client-final message answering `server_first` -- the proof that
    /// `password` is known -- and the server-final message to expect.
    fn last(&self, server_first: &str, password: &str) -> Result<(String, ServerFinal)> {
        let (mut snonce, mut salt, mut iterations) = (None, None, None);
        for kv in server_first.split(',') {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k {
                "r" => snonce = Some(v),
                "s" => salt = Some(b64_decode(v).ok_or_else(scram_unparsed)?),
                "i" => iterations = Some(scram_iterations(v)?),
                _ => {}
            }
        }
        // The server has to carry our nonce as a prefix; otherwise the
        // response belongs to another session.
        let (Some(snonce), Some(salt), Some(iterations)) = (snonce, salt, iterations) else {
            return Err(scram_invalid());
        };
        if !snonce.starts_with(&self.nonce) || salt.is_empty() {
            return Err(scram_invalid());
        }

        // `c=biws`, base64("n,,") -- no channel binding in use.
        let without_proof = format!("c=biws,r={snonce}");
        let auth = format!("{},{server_first},{without_proof}", self.first_bare);
        let salted = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let sig = hmac_sha256(&sha256(&client_key), auth.as_bytes());
        let proof: Vec<u8> = client_key.iter().zip(sig).map(|(a, b)| a ^ b).collect();
        let server_key = hmac_sha256(&salted, b"Server Key");
        let server_final = format!(
            "v={}",
            b64_encode(&hmac_sha256(&server_key, auth.as_bytes()))
        );
        Ok((
            format!("{without_proof},p={}", b64_encode(&proof)),
            ServerFinal(server_final),
        ))
    }
}

impl ServerFinal {
    /// Whether the server's last message is the one expected, compared as
    /// every signature is (`ct_eq`): one made fresh each exchange gives a
    /// guess nothing to grow, but the rule has no exceptions to tell apart.
    fn verify(&self, got: &str) -> Result<()> {
        if !ct_eq(got.as_bytes(), self.0.as_bytes()) {
            return Err(Error::Query(
                "postgres: the server signature could not be verified".into(),
            ));
        }
        Ok(())
    }
}

/// A server-first message's iteration count, held to 1 to
/// [`SCRAM_MAX_ITERATIONS`]: a count past it, however many digits it has,
/// is refused naming the range before PBKDF2 runs.
fn scram_iterations(v: &str) -> Result<u32> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(scram_unparsed());
    }
    // A count no `u32` holds is past the bound too; one past twenty digits
    // is shown by its length, the message being the server's, as long as a
    // gigabyte.
    let digits = v.trim_start_matches('0');
    if let Ok(n @ 1..=SCRAM_MAX_ITERATIONS) = digits.parse::<u32>() {
        return Ok(n);
    }
    let shown = if digits.len() > 20 {
        format!("a count of {} digits", digits.len())
    } else {
        v.to_string()
    };
    Err(Error::Query(format!(
        "postgres: the server's SCRAM iteration count is 1 to {SCRAM_MAX_ITERATIONS}, not {shown}"
    )))
}

fn scram_unparsed() -> Error {
    Error::Corrupt("postgres: the SCRAM response could not be parsed".into())
}

fn scram_invalid() -> Error {
    Error::Query("postgres: invalid SCRAM server response".into())
}

/// A connection to a live PostgreSQL server.
#[derive(Debug)]
pub struct Client {
    stream: BufReader<Conn>,
    /// The version the server reports; useful in error text.
    pub server_version: String,
}

impl Client {
    /// Connects and completes authentication.
    pub fn connect(url: &Url) -> Result<Client> {
        Client::open(url, false)
    }

    /// A connection in logical replication mode (`replication=database`):
    /// it takes [`Client::start_replication`] besides ordinary SQL.
    pub fn connect_replication(url: &Url) -> Result<Client> {
        Client::open(url, true)
    }

    fn open(url: &Url, replication: bool) -> Result<Client> {
        let mut c = Client {
            stream: BufReader::new(negotiate(url)?),
            server_version: String::new(),
        };

        // A StartupMessage has no tag: length + version + key/value pairs.
        let mut body = PROTOCOL_V3.to_be_bytes().to_vec();
        put_cstr(&mut body, "user");
        put_cstr(&mut body, &url.user);
        put_cstr(&mut body, "database");
        put_cstr(&mut body, &url.database);
        put_cstr(&mut body, "application_name");
        put_cstr(
            &mut body,
            if replication {
                "fennec-follow"
            } else {
                "fennec-import"
            },
        );
        if replication {
            put_cstr(&mut body, "replication");
            put_cstr(&mut body, "database");
        }
        body.push(0);
        let mut packet = ((body.len() + 4) as i32).to_be_bytes().to_vec();
        packet.extend_from_slice(&body);
        c.stream.get_mut().write_all(&packet).map_err(io)?;
        c.stream.get_mut().flush().map_err(io)?;

        loop {
            let m = c.read()?;
            match m.tag {
                b'R' => {
                    if m.body.len() < 4 {
                        return Err(Error::Corrupt("postgres: short auth message".into()));
                    }
                    match be_i32(&m.body, 0) {
                        // AuthenticationOk / SASLContinue / SASLFinal:
                        // the second and third are consumed inside scram().
                        0 | 11 | 12 => {}
                        3 => {
                            let pw = url.password.clone().unwrap_or_default();
                            let mut w = Writer::new();
                            w.msg(b'p', |b| put_cstr(b, &pw));
                            w.flush_to(c.stream.get_mut()).map_err(io)?;
                        }
                        10 => {
                            let pw = url.password.as_deref().ok_or_else(|| {
                                Error::Query("the server wants a password; add it to the connection string or set PGPASSWORD".into())
                            })?;
                            c.scram(pw)?;
                        }
                        5 => {
                            return Err(Error::Query(
                                "the server wants md5 authentication; that is not supported. \
                                 Switch the server to scram-sha-256 or temporarily use \
                                 cleartext/trust"
                                    .into(),
                            ))
                        }
                        other => {
                            return Err(Error::Query(format!(
                                "unsupported authentication method (code {other})"
                            )))
                        }
                    }
                }
                b'S' => {
                    let mut pos = 0;
                    let k = crate::proto::take_cstr(&m.body, &mut pos);
                    let v = crate::proto::take_cstr(&m.body, &mut pos);
                    if k == "server_version" {
                        c.server_version = v;
                    }
                }
                b'K' => {}
                b'E' => return Err(server_error(&m)),
                b'Z' => return Ok(c),
                other => {
                    return Err(Error::Corrupt(format!(
                        "postgres: unexpected message `{}` while connecting",
                        other as char
                    )))
                }
            }
        }
    }

    /// The SCRAM-SHA-256 client half (RFC 5802 + RFC 7677) over the
    /// connection, its messages [`Scram`]'s.
    ///
    /// The password is never sent over the wire; only nonces and HMAC proofs
    /// travel. The server signature is verified too, which proves the other
    /// side really knows the password.
    fn scram(&mut self, password: &str) -> Result<()> {
        // The system's randomness or no sign-in: a nonce of the clock's could
        // come again, and a proof with it.
        let cnonce =
            nonce(24).ok_or_else(|| io("no randomness from the system for SCRAM's nonce"))?;
        // No user name: PostgreSQL reads the startup message's, and libpq
        // sends `n=,` too.
        let scram = Scram::new("", &cnonce);
        let initial = scram.first();
        let mut w = Writer::new();
        w.msg(b'p', |b| {
            put_cstr(b, "SCRAM-SHA-256");
            b.extend_from_slice(&(initial.len() as i32).to_be_bytes());
            b.extend_from_slice(initial.as_bytes());
        });
        w.flush_to(self.stream.get_mut()).map_err(io)?;

        let server_first = self.sasl_answer()?;
        let (last, server_final) = scram.last(&server_first, password)?;
        let mut w = Writer::new();
        w.msg(b'p', |b| b.extend_from_slice(last.as_bytes()));
        w.flush_to(self.stream.get_mut()).map_err(io)?;

        server_final.verify(&self.sasl_answer()?)
    }

    /// The text of a SASL answer -- the server-first or the server-final
    /// message -- or the server's error.
    fn sasl_answer(&mut self) -> Result<String> {
        let m = self.read()?;
        if m.tag == b'E' {
            return Err(server_error(&m));
        }
        let body = m.body.get(4..).ok_or_else(scram_unparsed)?;
        Ok(String::from_utf8_lossy(body).into_owned())
    }

    fn read(&mut self) -> Result<Message> {
        read_message(&mut self.stream).map_err(io)
    }

    fn send_query(&mut self, sql: &str) -> Result<()> {
        let mut w = Writer::new();
        w.msg(b'Q', |b| put_cstr(b, sql));
        w.flush_to(self.stream.get_mut()).map_err(io)
    }

    /// A simple query; the whole result is buffered. For catalog queries and
    /// reading `RowDescription` -- large tables stream through `copy_out`.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.send_query(sql)?;
        let mut out = QueryResult::default();
        let mut failed = None;
        loop {
            let m = self.read()?;
            match m.tag {
                b'T' => out.columns = row_description(&m)?,
                b'D' => out.rows.push(data_row(&m)?),
                b'E' => failed = Some(server_error(&m)),
                // ReadyForQuery is awaited even after an error, otherwise the
                // connection becomes unusable for the next query.
                b'Z' => break,
                _ => {}
            }
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    /// Starts `COPY ... TO STDOUT`. Rows are read as they stream.
    ///
    /// It takes the connection over: no other query can run on the same
    /// connection while the stream is open, so catalog queries have to come
    /// first.
    pub fn copy_out(mut self, sql: &str) -> Result<CopyOut> {
        self.send_query(sql)?;
        // Read up to CopyOutResponse; on an error the stream never starts.
        loop {
            let m = self.read()?;
            match m.tag {
                b'H' => break,
                b'E' => {
                    let e = server_error(&m);
                    // Read up to Z so the connection stays usable.
                    while self.read()?.tag != b'Z' {}
                    return Err(e);
                }
                b'Z' => {
                    return Err(Error::Query(
                        "postgres: the COPY stream did not start (the query may return no rows)"
                            .into(),
                    ))
                }
                _ => {}
            }
        }
        Ok(CopyOut {
            client: self,
            buf: Vec::new(),
            pos: 0,
            done: false,
        })
    }
}

impl Client {
    /// `START_REPLICATION` of a logical slot through `pgoutput`, protocol 1:
    /// the connection turns into a stream of the publication's changes, a
    /// transaction at a time as each commits, from where the slot was last
    /// confirmed. Needs a [`Client::connect_replication`] connection.
    ///
    /// The names go into the command as they are, so they have to be plain
    /// lowercase identifiers; anything else is refused rather than quoted.
    pub fn start_replication(mut self, slot: &str, publication: &str) -> Result<WalStream> {
        for name in [slot, publication] {
            if !plain_name(name) {
                return Err(Error::Query(format!(
                    "`{name}` is not a plain name: lowercase letters, digits and `_`, \
                     63 at most, not starting with a digit"
                )));
            }
        }
        self.send_query(&format!(
            "START_REPLICATION SLOT {slot} LOGICAL 0/0 \
             (proto_version '1', publication_names '{publication}')"
        ))?;
        loop {
            let m = self.read()?;
            match m.tag {
                // CopyBothResponse: from here on both sides send CopyData.
                b'W' => return Ok(WalStream { client: self }),
                b'E' => return Err(server_error(&m)),
                _ => {}
            }
        }
    }
}

/// Whether `name` can go into a replication command unquoted.
pub fn plain_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && !b[0].is_ascii_digit()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// A write-ahead log position as PostgreSQL prints it: `16/B374D848`.
pub fn lsn_text(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn as u32)
}

/// The other way round; `None` when it is not one.
pub fn parse_lsn(s: &str) -> Option<u64> {
    let (hi, lo) = s.trim().split_once('/')?;
    let hi = u32::from_str_radix(hi, 16).ok()?;
    let lo = u32::from_str_radix(lo, 16).ok()?;
    Some((hi as u64) << 32 | lo as u64)
}

/// One message of a replication stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wal {
    /// `XLogData`: one `pgoutput` message, and how far the server's log
    /// reaches.
    Data { end: u64, body: Vec<u8> },
    /// A keepalive: how far the server's log reaches, and whether it wants
    /// a status update now -- it drops a receiver that stays silent past
    /// `wal_sender_timeout`.
    Keepalive { end: u64, reply: bool },
}

/// A logical replication stream, from [`Client::start_replication`].
#[derive(Debug)]
pub struct WalStream {
    client: Client,
}

impl WalStream {
    /// The next message, or `None` when none arrived within `wait`.
    ///
    /// The wait is never a read that can time out halfway through a
    /// message, which would leave the rest of it to be taken for the start
    /// of the next one (`Conn::readable`).
    pub fn next(&mut self, wait: Duration) -> Result<Option<Wal>> {
        if self.client.stream.buffer().is_empty() {
            match self.client.stream.get_mut().readable(wait) {
                Ok(true) => {}
                Ok(false) => return Ok(None),
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Err(io("the server closed the replication stream"))
                }
                Err(e) => return Err(io(e)),
            }
        }
        let m = self.client.read()?;
        match m.tag {
            b'd' => wal_message(&m.body).map(Some),
            b'E' => Err(server_error(&m)),
            b'c' => Err(io("the server ended the replication stream")),
            // A notice, or anything else outside the copy: nothing to act on.
            _ => Ok(None),
        }
    }

    /// Standby status update: everything up to `lsn` is safe with the
    /// receiver, and the slot may let the server forget it. The server takes
    /// the flushed position as the slot's confirmed one, so it must never run
    /// ahead of what is on disk.
    pub fn confirm(&mut self, lsn: u64) -> Result<()> {
        // Microseconds since 2000-01-01, PostgreSQL's epoch; only for the
        // server's lag statistics.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_micros() as i64)
            - 946_684_800_000_000;
        let mut w = Writer::new();
        w.msg(b'd', |b| {
            b.push(b'r');
            for _ in 0..3 {
                // written, flushed, applied
                b.extend_from_slice(&lsn.to_be_bytes());
            }
            b.extend_from_slice(&now.to_be_bytes());
            b.push(0);
        });
        w.flush_to(self.client.stream.get_mut()).map_err(io)
    }
}

/// The body of a `CopyData` on a replication stream.
fn wal_message(b: &[u8]) -> Result<Wal> {
    let short = || Error::Corrupt("postgres: short replication message".into());
    let u64_at = |at: usize| -> Result<u64> {
        let bytes = b.get(at..at + 8).ok_or_else(short)?;
        Ok(u64::from_be_bytes(bytes.try_into().map_err(|_| short())?))
    };
    match b.first() {
        // 'w', start (8), end (8), send time (8), data
        Some(b'w') => Ok(Wal::Data {
            end: u64_at(9)?,
            body: b.get(25..).ok_or_else(short)?.to_vec(),
        }),
        // 'k', end (8), send time (8), reply requested (1)
        Some(b'k') => Ok(Wal::Keepalive {
            end: u64_at(1)?,
            reply: *b.get(17).ok_or_else(short)? == 1,
        }),
        Some(other) => Err(Error::Corrupt(format!(
            "postgres: unknown replication message `{}`",
            *other as char
        ))),
        None => Err(short()),
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Terminate: let the server close the connection cleanly.
        let mut w = Writer::new();
        w.msg(b'X', |_| {});
        let _ = w.flush_to(self.stream.get_mut());
    }
}

/// A `COPY ... TO STDOUT` stream.
#[derive(Debug)]
pub struct CopyOut {
    client: Client,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
}

impl CopyOut {
    /// The raw bytes of the next line (without the line ending), or `None`
    /// when the stream is done. `CopyData` frames do not align with line
    /// boundaries: a frame can end mid-line or carry several lines.
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(i) = self.buf[self.pos..].iter().position(|&b| b == b'\n') {
                let line = self.buf[self.pos..self.pos + i].to_vec();
                self.pos += i + 1;
                return Ok(Some(line));
            }
            if self.done {
                if self.pos < self.buf.len() {
                    let line = self.buf[self.pos..].to_vec();
                    self.pos = self.buf.len();
                    return Ok(Some(line));
                }
                return Ok(None);
            }
            self.buf.drain(..self.pos);
            self.pos = 0;
            let m = self.client.read()?;
            match m.tag {
                b'd' => self.buf.extend_from_slice(&m.body),
                b'c' => {
                    self.done = true;
                    // CommandComplete + ReadyForQuery: keep the connection usable.
                    while self.client.read()?.tag != b'Z' {}
                }
                b'E' => {
                    let e = server_error(&m);
                    while self.client.read()?.tag != b'Z' {}
                    return Err(e);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_parsed() {
        let u = Url::parse("postgres://alice:secret@db.local:6432/production").unwrap();
        assert_eq!(u.user, "alice");
        assert_eq!(u.password.as_deref(), Some("secret"));
        assert_eq!(u.host, "db.local");
        assert_eq!(u.port, 6432);
        assert_eq!(u.database, "production");

        // 5432 when no port is given.
        let u = Url::parse("postgresql://alice@localhost/test").unwrap();
        assert_eq!(u.port, 5432);
        assert_eq!(u.password, None);
    }

    /// `@` and `:` travel percent-escaped in a password; splitting on the
    /// last `@` is essential, otherwise the password is mistaken for the host.
    #[test]
    fn password_escapes_survive() {
        let u = Url::parse("postgres://alice:a%40b%3Ac@localhost/t").unwrap();
        assert_eq!(u.password.as_deref(), Some("a@b:c"));
        assert_eq!(u.host, "localhost");
    }

    #[test]
    fn bad_urls_are_refused() {
        assert!(Url::parse("mysql://x@y/z").is_err());
        assert!(
            Url::parse("postgres://alice@localhost").is_err(),
            "no database"
        );
        assert!(Url::parse("postgres://alice@localhost:abc/t").is_err());
        // A parameter the client does not act on must not be swallowed.
        for p in [
            "sslcert=c.pem",
            "sslmode",
            "connect_timeout=5",
            "sslmode=allow",
            "sslmode=on",
        ] {
            assert!(
                Url::parse(&format!("postgres://alice@localhost/t?{p}")).is_err(),
                "{p}"
            );
        }
    }

    #[test]
    fn sslmode_and_sslrootcert_are_read_as_libpq_reads_them() {
        let parse = |q: &str| Url::parse(&format!("postgres://alice@db.example/t?{q}"));
        for (q, mode) in [
            ("sslmode=disable", SslMode::Disable),
            ("sslmode=prefer", SslMode::Prefer),
            ("sslmode=require", SslMode::Require),
            ("sslmode=verify-full", SslMode::VerifyFull),
            (
                "sslmode=verify-full&sslrootcert=system",
                SslMode::VerifyFull,
            ),
        ] {
            let u = parse(q).unwrap();
            assert_eq!(u.sslmode, mode, "{q}");
            assert_eq!(u.sslmode.name(), q[8..].split('&').next().unwrap());
        }
        let u = parse("sslmode=verify-ca&sslrootcert=%2Fetc%2Fpg%20ca.pem").unwrap();
        assert_eq!(u.sslmode, SslMode::VerifyCa);
        assert_eq!(u.sslrootcert.as_deref(), Some("/etc/pg ca.pem"));
        if std::env::var_os("PGSSLMODE").is_none() {
            assert_eq!(
                Url::parse("postgres://alice@db.example/t").unwrap().sslmode,
                SslMode::Prefer,
                "libpq's default"
            );
        }
        // The public CAs vouch for every public name: taken only where the
        // name is checked too. `verify-ca` without a file named is the
        // dial's to answer, where libpq's root file may be (tests/tls.rs).
        assert!(parse("sslmode=verify-ca").is_ok());
        assert!(parse("sslmode=verify-ca&sslrootcert=system").is_err());
        assert!(parse("sslmode=require&sslrootcert=system").is_err());
        assert!(parse("sslmode=prefer&sslrootcert=system").is_err());
    }

    #[test]
    fn row_description_carries_typmod() {
        // One column: name "embed", type oid 16385, typmod 384.
        let mut body = 1i16.to_be_bytes().to_vec();
        put_cstr(&mut body, "embed");
        body.extend_from_slice(&0i32.to_be_bytes()); // table oid
        body.extend_from_slice(&0i16.to_be_bytes()); // column number
        body.extend_from_slice(&16_385i32.to_be_bytes()); // type oid
        body.extend_from_slice(&(-1i16).to_be_bytes()); // type length
        body.extend_from_slice(&384i32.to_be_bytes()); // type modifier
        body.extend_from_slice(&0i16.to_be_bytes()); // format
        let cols = row_description(&Message { tag: b'T', body }).unwrap();
        assert_eq!(
            cols,
            vec![FieldDesc {
                name: "embed".into(),
                oid: 16_385,
                typmod: 384,
            }]
        );
    }

    #[test]
    fn data_row_handles_nulls() {
        let mut body = 3i16.to_be_bytes().to_vec();
        body.extend_from_slice(&2i32.to_be_bytes());
        body.extend_from_slice(b"hi");
        body.extend_from_slice(&(-1i32).to_be_bytes()); // NULL
        body.extend_from_slice(&0i32.to_be_bytes()); // empty string
        let cells = data_row(&Message { tag: b'D', body }).unwrap();
        assert_eq!(cells, vec![Some("hi".into()), None, Some(String::new())]);
    }

    /// A count of columns past what the body holds -- `0xFFFF`, which read
    /// as an Int16 sized a `Vec` by `usize::MAX` and panicked -- is the
    /// message's error.
    #[test]
    fn a_count_of_columns_the_body_does_not_hold_is_an_error() {
        for body in [vec![0xFF, 0xFF], vec![0xFF, 0xFF, 0, 0, 0, 1, b'x']] {
            let short = |e: Error| matches!(e, Error::Corrupt(_));
            let m = Message { tag: b'D', body };
            assert!(short(data_row(&m).unwrap_err()));
            let m = Message { tag: b'T', ..m };
            assert!(short(row_description(&m).unwrap_err()));
        }
    }

    #[test]
    fn replication_messages_are_parsed() {
        let mut w = vec![b'w'];
        w.extend_from_slice(&1u64.to_be_bytes());
        w.extend_from_slice(&0x16_B374_D848u64.to_be_bytes());
        w.extend_from_slice(&0i64.to_be_bytes());
        w.extend_from_slice(b"BEGIN");
        assert_eq!(
            wal_message(&w).unwrap(),
            Wal::Data {
                end: 0x16_B374_D848,
                body: b"BEGIN".to_vec()
            }
        );
        let mut k = vec![b'k'];
        k.extend_from_slice(&7u64.to_be_bytes());
        k.extend_from_slice(&0i64.to_be_bytes());
        k.push(1);
        assert_eq!(
            wal_message(&k).unwrap(),
            Wal::Keepalive {
                end: 7,
                reply: true
            }
        );
        assert!(wal_message(&k[..10]).is_err());
        assert_eq!(lsn_text(0x16_B374_D848), "16/B374D848");
        assert_eq!(parse_lsn("16/B374D848"), Some(0x16_B374_D848));
        assert_eq!(parse_lsn("0/0"), Some(0));
        assert_eq!(parse_lsn("nope"), None);
        assert!(plain_name("fennec_articles2"));
        for bad in ["", "Fennec", "2a", "a-b", "a b", "a'b"] {
            assert!(!plain_name(bad), "{bad}");
        }
    }

    #[test]
    fn error_fields_are_extracted() {
        let mut body = Vec::new();
        body.push(b'S');
        put_cstr(&mut body, "ERROR");
        body.push(b'C');
        put_cstr(&mut body, "42P01");
        body.push(b'M');
        put_cstr(&mut body, "relation \"nosuch\" does not exist");
        body.push(0);
        let e = server_error(&Message { tag: b'E', body }).to_string();
        assert!(e.contains("42P01"), "{e}");
        assert!(e.contains("does not exist"), "{e}");
    }

    /// RFC 7677, section 3: its exchange, user `user` and password
    /// `pencil`, with its nonce -- the client's messages, the proof among
    /// them, byte for byte as the RFC prints them, and its server signature
    /// verified. The RFC breaks the server-first and the client-final
    /// messages across lines; the messages hold no space.
    #[test]
    fn scram_rfc7677_exchange() {
        let scram = Scram::new("user", "rOprNGfwEbeRWgbNEkqO");
        assert_eq!(scram.first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        let server_first = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
                            s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let (last, server_final) = scram.last(server_first, "pencil").unwrap();
        assert_eq!(
            last,
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
             p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        server_final
            .verify("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
            .unwrap();
        // A server-final message that is not the RFC's: a character of its
        // signature changed, its last cut off, one more, none, or an error.
        let refused = [
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G5=",
            "v=7rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=",
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4",
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4==",
            "",
            "e=invalid-proof",
        ];
        let mut ran = 0;
        for got in refused {
            let e = server_final.verify(got).unwrap_err().to_string();
            assert!(e.contains("server signature"), "{got:?}: {e}");
            ran += 1;
        }
        assert_eq!(ran, 6);
        // Another password is another proof, and expects another signature.
        let (other, other_final) = scram.last(server_first, "pencil!").unwrap();
        assert_ne!(other, last);
        assert!(other_final
            .verify("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
            .is_err());
        // A server nonce not beginning with the client's is another
        // session's.
        let e = scram
            .last(&server_first.replacen("rOpr", "xOpr", 1), "pencil")
            .err()
            .unwrap();
        assert!(e.to_string().contains("invalid SCRAM"), "{e}");
    }

    /// The iteration count a server asks for is 1 to SCRAM_MAX_ITERATIONS,
    /// read as written: the bound itself is run, one past it or any count
    /// no `u32` holds refused naming the range, before PBKDF2 runs.
    #[test]
    fn scram_iterations_are_held_to_their_range() {
        let taken = [
            ("1", 1),
            ("4096", 4096),
            ("01048576", 1 << 20),
            ("1048576", SCRAM_MAX_ITERATIONS),
        ];
        let mut ran = 0;
        for (v, n) in taken {
            assert_eq!(scram_iterations(v).unwrap(), n, "{v}");
            ran += 1;
        }
        let refused = [
            ("0", "not 0"),
            ("1048577", "not 1048577"),
            ("4294967295", "not 4294967295"),
            ("4294967296", "not 4294967296"),
            ("18446744073709551616", "not 18446744073709551616"),
            ("123456789012345678901", "not a count of 21 digits"),
        ];
        for (v, says) in refused {
            let e = scram_iterations(v).unwrap_err().to_string();
            assert!(
                e.contains("is 1 to 1048576") && e.contains(says),
                "{v}: {e}"
            );
            ran += 1;
        }
        for v in ["", "-1", "+5", "1e6", "4096 ", "0x10"] {
            let e = scram_iterations(v).unwrap_err().to_string();
            assert!(e.contains("could not be parsed"), "{v:?}: {e}");
            ran += 1;
        }
        assert_eq!(ran, 16);
        // Refused as the server-first message is read, before its proof is
        // made: no PBKDF2 of four billion rounds runs here.
        let e = Scram::new("", "abc")
            .last("r=abcdef,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4294967295", "p")
            .err()
            .unwrap();
        assert!(e.to_string().contains("not 4294967295"), "{e}");
    }
}
