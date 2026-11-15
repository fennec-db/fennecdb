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
//! five but `allow`: by default `verify-full` for a host off this machine,
//! which alone keeps a stand-in for the server out, and `prefer` on it.
//! TLS is fennec-tls's, behind the `tls` feature.
//!
//! What a server may ask of a password is the string's `require_auth`
//! ([`RequireAuth`]), by default SCRAM alone where the client holds one,
//! bound to the TLS session where the server offers `SCRAM-SHA-256-PLUS`
//! (`channel_binding`). A whole sign-in is held to `connect_timeout`; no
//! word of a server not proven to be the server is shown ([`Client`]).

use crate::crypto::{b64_decode, b64_encode, ct_eq, hmac_sha256, nonce, pbkdf2_sha256, sha256};
use crate::proto::{put_cstr, read_message, Message, Writer, MAX_MESSAGE, PROTOCOL_V3};
use fennec_core::error::{Error, Result};
use std::io::{self as stdio, BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a read waits for the server's next bytes once signed in: a
/// query's answer, a COPY's, a replication stream's own reads aside
/// ([`WalStream::next`]).
const TIMEOUT: Duration = Duration::from_secs(30);

/// A whole sign-in's bound where the string names no `connect_timeout`:
/// the name looked up, the connection made, TLS's handshake, the startup and
/// the password's exchange up to the server's first `ReadyForQuery`, each
/// read and wait held to what is left of it -- PBKDF2's own time apart, which
/// is the client's, and held to [`SCRAM_MAX_ITERATIONS`]. A server that
/// takes the connection and answers nothing, or a byte at a time, held a
/// dial for as long as it liked, 30 s a read.
pub const CONNECT_WAIT: Duration = Duration::from_secs(30);

/// How often a COPY's read looks at its stop ([`CopyOut::next_line_until`]),
/// the socket's read timeout while it streams: a stop wakes a read waiting
/// on the server within it.
pub const LOOK: Duration = Duration::from_millis(200);

/// How much of a server's text is shown, in bytes ([`peer_text`]): an
/// error's message, a name. A cell's is shorter (fennec-import's
/// `parse_cell`).
pub const SHOWN: usize = 512;

fn io(e: impl std::fmt::Display) -> Error {
    Error::Io(format!("postgres: {e}"))
}

/// Text a peer sent, made fit to log or print: a control character, and the
/// characters that reorder a terminal's line (the bidirectional overrides,
/// embeddings, isolates and marks), escaped as Rust writes them (`\u{1b}`,
/// `\n`), and no more than `cap` bytes of it kept, its whole length said
/// after them. A server's words reach `fennec-server`'s log and `fennec
/// import`'s terminal: an `ErrorResponse`, a column's name, a cell that
/// failed to parse -- up to a gigabyte, escape sequences and all. Only the
/// first `cap` bytes and a few more are read: a gigabyte's cell is not.
pub fn peer_text(b: &[u8], cap: usize) -> String {
    let head = String::from_utf8_lossy(&b[..b.len().min(cap.saturating_add(4))]);
    let mut out = String::with_capacity(head.len().min(cap) + 24);
    for c in head.chars() {
        if out.len() >= cap {
            out.push_str(&format!("... ({} bytes)", b.len()));
            return out;
        }
        let reorders = matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        );
        if c.is_control() {
            out.extend(c.escape_debug());
        } else if reorders {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

// -------------------------------------------------------------------- url

/// Whether TLS is asked for, and what of the server's certificate is
/// checked: libpq's `sslmode`, each as libpq defines it. Only `verify-full`
/// keeps out someone standing in for the server; `require` checks nothing
/// without a CA file, as libpq's does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    /// Plain text; no `SSLRequest` is sent.
    Disable,
    /// TLS where the server takes it, plain text where it answers `N` or
    /// its handshake fails; no certificate checked. libpq's default, here
    /// a host on this machine's, and plain text alone in a build without
    /// the `tls` feature, as in libpq built without SSL.
    Prefer,
    /// TLS or no connection; no certificate checked -- unless there is a
    /// CA file, `sslrootcert`'s or libpq's own `~/.postgresql/root.crt`,
    /// which makes it `verify-ca`, as libpq does.
    Require,
    /// TLS, the chain checked up to the CA file's CAs (`sslrootcert`, else
    /// `~/.postgresql/root.crt`), the name not.
    VerifyCa,
    /// TLS, the chain checked and the host dialed held to the certificate:
    /// against the CA file, else Mozilla's roots. The default for a host
    /// off this machine.
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
                    "unknown sslmode {} (disable, prefer, require, verify-ca or verify-full)",
                    said(other)
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

/// Where a connection's `sslmode` came from, which the dial says
/// ([`Url::tls_said`]) and a refusal of the default explains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslModeFrom {
    /// `sslmode=` in the connection string, which always wins.
    Written,
    /// `PGSSLMODE`, libpq's, where the string names none.
    Environment,
    /// `sslrootcert=system` named with no mode: `verify-full`, as libpq
    /// makes it.
    SystemRoots,
    /// Neither: `verify-full` for a host off this machine, `prefer` for
    /// one on it ([`on_this_machine`]).
    Default,
}

/// Whether `host` is this machine: `localhost`, or an address of the
/// loopback (127.0.0.0/8, `::1`, the first as IPv6 maps it). On it there is
/// no path for anyone to stand on, and whoever answers at the port is the
/// server as far as this machine can tell, so its default `sslmode` stays
/// libpq's `prefer`; anywhere else it is `verify-full`.
pub fn on_this_machine(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// Whether a sign-in may use channel binding: libpq's `channel_binding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelBinding {
    /// Never: `n,,`, the TLS session unbound.
    Disable,
    /// Where the server offers `SCRAM-SHA-256-PLUS` over TLS; where it
    /// does not, the client says it could have (`y,,`), so a server that
    /// did offer it, its offer taken out on the way, refuses the sign-in.
    Prefer,
    /// Always: a server that does not offer it, a connection without TLS,
    /// and any request but SCRAM's are refused, as an `AuthenticationOk`
    /// before a bound exchange finished is.
    Require,
}

/// What a server may ask of a client signing in: libpq's `require_auth`,
/// spelled and read as libpq reads it (`fe-connect.c`): a list of methods,
/// `password`, `md5`, `gss`, `sspi`, `scram-sha-256` and `none`, all named
/// or all negated (`!password,!md5`), none twice. The methods named are the
/// requests the server may make; `none` lets it ask for none, an
/// `AuthenticationOk` before any exchange. A negated list starts from all
/// and takes away, `none` among them (`!none`).
///
/// Where the string names none ([`RequireAuth::default_for`]): a client
/// holding a password -- the string's or `PGPASSWORD` -- signs in by SCRAM
/// alone, so neither a request for the password in clear nor an
/// `AuthenticationOk` before SCRAM finished, its server signature
/// verified, is taken from whoever answers; one holding none takes what it
/// took, there being no password to give away. The client answers
/// `password` (cleartext) and `scram-sha-256` (with `-PLUS`); `md5`,
/// `gss` and `sspi` may be named, as libpq's string names them, and a
/// server asking for them is still refused, as unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireAuth {
    /// The requests (`R` codes) the server may make, a bit each: libpq's
    /// `allowed_auth_methods`.
    allowed: u32,
    /// Whether an `AuthenticationOk` is refused before the client finished
    /// an exchange: libpq's `auth_required`.
    required: bool,
    /// As written, or the default's name, for a refusal to say.
    said: String,
}

const AUTH_OK: i32 = 0;
const AUTH_PASSWORD: i32 = 3;
const AUTH_MD5: i32 = 5;
const AUTH_GSS: i32 = 7;
const AUTH_GSS_CONT: i32 = 8;
const AUTH_SSPI: i32 = 9;
const AUTH_SASL: i32 = 10;
const AUTH_SASL_CONT: i32 = 11;
const AUTH_SASL_FIN: i32 = 12;
const SASL_BITS: u32 = 1 << AUTH_SASL | 1 << AUTH_SASL_CONT | 1 << AUTH_SASL_FIN;

impl RequireAuth {
    /// The rule where the string names none: SCRAM alone for a client
    /// holding a password, anything for one holding none.
    pub fn default_for(password: bool) -> RequireAuth {
        match password {
            true => RequireAuth {
                allowed: SASL_BITS,
                required: true,
                said: "scram-sha-256, the default where a password is held".into(),
            },
            false => RequireAuth {
                allowed: u32::MAX,
                required: false,
                said: String::new(),
            },
        }
    }

    /// libpq's `require_auth` list.
    fn parse(list: &str) -> Result<RequireAuth> {
        let mut out = RequireAuth {
            allowed: 0,
            required: true,
            said: list.to_string(),
        };
        let refused = |why: String| Error::Query(format!("require_auth: {why}"));
        let mut negated = false;
        for (i, part) in list.split(',').enumerate() {
            let (bang, method) = match part.strip_prefix('!') {
                Some(m) => (true, m),
                None => (false, part),
            };
            if i == 0 && bang {
                // A negated list starts from everything, `none` included.
                negated = true;
                out.required = false;
                out.allowed = u32::MAX;
            }
            if bang != negated {
                return Err(refused(match bang {
                    true => format!(
                        "a negated method, {}, cannot be mixed with methods named",
                        said(method)
                    ),
                    false => format!("{} cannot be mixed with negated methods", said(method)),
                }));
            }
            let bits = match method {
                "password" => 1 << AUTH_PASSWORD,
                "md5" => 1 << AUTH_MD5,
                "gss" => 1 << AUTH_GSS | 1 << AUTH_GSS_CONT,
                "sspi" => 1 << AUTH_SSPI | 1 << AUTH_GSS_CONT,
                "scram-sha-256" => SASL_BITS,
                "none" => {
                    // `none` lets the server ask for nothing, `!none` takes
                    // that away: libpq's `auth_required` turned, once.
                    if out.required == negated {
                        return Err(refused(format!("{} is named more than once", said(method))));
                    }
                    out.required = negated;
                    continue;
                }
                other => {
                    return Err(refused(format!(
                        "unknown method {} (password, md5, gss, sspi, scram-sha-256, none, \
                         each negated with `!`)",
                        said(other)
                    )))
                }
            };
            let twice = match negated {
                true => out.allowed & bits == 0,
                false => out.allowed & bits == bits,
            };
            if twice {
                return Err(refused(format!("{} is named more than once", said(method))));
            }
            match negated {
                true => out.allowed &= !bits,
                false => out.allowed |= bits,
            }
        }
        Ok(out)
    }

    /// Whether the server may make request `code`, the client having
    /// finished an exchange or not: libpq's `check_expected_areq`, its words.
    fn allows(&self, code: i32, finished: bool) -> Result<()> {
        let reason = match code {
            AUTH_OK if self.required && !finished => "the server did not complete authentication",
            AUTH_OK => return Ok(()),
            c if (0..32).contains(&c) && self.allowed & (1 << c) != 0 => return Ok(()),
            AUTH_PASSWORD => "the server asked for the password in clear text",
            AUTH_MD5 => "the server asked for an MD5 password",
            AUTH_GSS | AUTH_GSS_CONT => "the server asked for GSSAPI authentication",
            AUTH_SSPI => "the server asked for SSPI authentication",
            AUTH_SASL | AUTH_SASL_CONT | AUTH_SASL_FIN => {
                "the server asked for SASL (SCRAM) authentication"
            }
            _ => "the server asked for an authentication method this client does not know",
        };
        Err(Error::Query(format!(
            "postgres: authentication method requirement \"{}\" failed: {reason}{}",
            self.said,
            match code {
                AUTH_PASSWORD | AUTH_OK =>
                    " -- refused, as whoever stands in for the server would ask; \
                     require_auth=password (or none) in the connection string takes it",
                _ => "",
            }
        )))
    }
}

/// A connection parameter's value, or part of a key, shown in a refusal
/// only where it is a word -- letters, digits, `_`, `-`, `.`, at most 32 --
/// and never otherwise: a `?` left unescaped in a password makes the rest
/// of the password a "parameter", whose text must not reach a log.
fn said(v: &str) -> String {
    let word = !v.is_empty()
        && v.len() <= 32
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    match word {
        true => format!("`{v}`"),
        false => "(a value not shown)".into(),
    }
}

/// The keywords libpq takes (17's): one of them that this client does not
/// is refused by its name; any other text is not shown, being as likely a
/// password's.
const LIBPQ_KEYWORDS: [&str; 41] = [
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "passfile",
    "require_auth",
    "channel_binding",
    "connect_timeout",
    "client_encoding",
    "options",
    "application_name",
    "fallback_application_name",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_count",
    "tcp_user_timeout",
    "replication",
    "gssencmode",
    "sslmode",
    "sslnegotiation",
    "sslcompression",
    "sslcert",
    "sslkey",
    "sslpassword",
    "sslcertmode",
    "sslrootcert",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "requirepeer",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "krbsrvname",
    "gsslib",
    "gssdelegation",
    "service",
    "target_session_attrs",
    "load_balance_hosts",
];

/// `postgres://user[:password]@host[:port]/database[?param=value&...]`, the
/// parameters `sslmode`, `sslrootcert`, `require_auth`, `channel_binding`
/// and `connect_timeout`, each libpq's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub user: String,
    pub password: Option<String>,
    pub host: String,
    pub port: u16,
    pub database: String,
    /// `sslmode`, else `PGSSLMODE`, else `verify-full` off this machine and
    /// `prefer` on it ([`SslModeFrom`]).
    pub sslmode: SslMode,
    /// Where `sslmode` came from.
    pub sslmode_from: SslModeFrom,
    /// `sslrootcert`, else `PGSSLROOTCERT`: a PEM file of the CAs the
    /// `verify-*` modes and `require` trust, or `system` for Mozilla's
    /// roots compiled in (webpki-roots). Where none is named, libpq's
    /// `~/.postgresql/root.crt` is read at the dial if it is there, and
    /// `verify-full` takes Mozilla's roots if it is not.
    pub sslrootcert: Option<String>,
    /// `require_auth`; `None` where the string names none, the rule then
    /// [`RequireAuth::default_for`] whether a password is held.
    pub require_auth: Option<RequireAuth>,
    /// `channel_binding`, `prefer` where the string names none.
    pub channel_binding: ChannelBinding,
    /// `connect_timeout`, a whole sign-in's bound ([`CONNECT_WAIT`] where
    /// the string names none).
    pub connect_timeout: Duration,
}

impl Url {
    /// Parses the connection string. Without a password `PGPASSWORD` is
    /// used, and without a user `PGUSER` or `USER`; without `sslmode` or
    /// `sslrootcert`, `PGSSLMODE` and `PGSSLROOTCERT`. Any other parameter
    /// is refused rather than passed over. No refusal shows the string, nor
    /// any part of it that may be a password's.
    pub fn parse(s: &str) -> Result<Url> {
        let rest = s
            .strip_prefix("postgresql://")
            .or_else(|| s.strip_prefix("postgres://"))
            .ok_or_else(|| {
                // The scheme alone, where it is one: the whole string, or
                // what follows its scheme, holds the password.
                let scheme = s
                    .split_once("://")
                    .map(|(scheme, _)| scheme)
                    .filter(|scheme| {
                        !scheme.is_empty()
                            && scheme.len() <= 16
                            && scheme.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')
                            })
                    });
                Error::Query(match scheme {
                    Some(scheme) => format!(
                        "a connection string beginning {scheme}:// is not a PostgreSQL one \
                         (expected postgres://user@host:port/database)"
                    ),
                    None => "the connection string is not a PostgreSQL one \
                             (expected postgres://user@host:port/database)"
                        .into(),
                })
            })?;
        // The parameters are read; any other is refused, never swallowed:
        // a `sslcert` passed over would leave a server asking for a client
        // certificate to refuse a connection for no reason the user could
        // see.
        let (rest, params) = match rest.split_once('?') {
            Some((r, p)) => (r, p),
            None => (rest, ""),
        };
        let (mut sslmode, mut sslrootcert) = (None, None);
        let (mut require_auth, mut channel_binding, mut connect_timeout) = (None, None, None);
        for pair in params.split('&').filter(|p| !p.is_empty()) {
            let (key, v) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "sslmode" => sslmode = Some(SslMode::parse(&decode(v, "sslmode")?)?),
                // An empty value is libpq's default, as no value.
                "sslrootcert" | "require_auth" if v.is_empty() => {}
                "sslrootcert" => sslrootcert = Some(decode(v, "sslrootcert")?),
                "require_auth" => {
                    require_auth = Some(RequireAuth::parse(&decode(v, "require_auth")?)?)
                }
                "channel_binding" => {
                    channel_binding = Some(match decode(v, "channel_binding")?.as_str() {
                        "disable" => ChannelBinding::Disable,
                        "prefer" => ChannelBinding::Prefer,
                        "require" => ChannelBinding::Require,
                        other => {
                            return Err(Error::Query(format!(
                                "unknown channel_binding {} (disable, prefer or require)",
                                said(other)
                            )))
                        }
                    })
                }
                "connect_timeout" => {
                    // libpq's: whole seconds, 1 read as 2. Its 0, waiting
                    // for ever, is refused: every dial here is bounded.
                    let v = decode(v, "connect_timeout")?;
                    match v.trim().parse::<u32>() {
                        Ok(n @ 1..) => connect_timeout = Some(Duration::from_secs(n.max(2) as u64)),
                        _ => {
                            return Err(Error::Query(format!(
                                "connect_timeout is a whole number of seconds from 1, not {}: \
                                 every dial here is bounded",
                                said(&v)
                            )))
                        }
                    }
                }
                key if LIBPQ_KEYWORDS.contains(&key) => {
                    return Err(Error::Query(format!(
                        "connection parameter `{key}` is not supported (sslmode, sslrootcert, \
                         require_auth, channel_binding and connect_timeout are)"
                    )))
                }
                _ => {
                    return Err(Error::Query(
                        "the connection string holds a parameter this client does not know \
                         (sslmode, sslrootcert, require_auth, channel_binding and \
                         connect_timeout are); a `?` in a password is written %3F"
                            .into(),
                    ))
                }
            }
        }
        let sslrootcert = sslrootcert.or_else(|| {
            std::env::var("PGSSLROOTCERT")
                .ok()
                .filter(|r| !r.is_empty())
        });
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
                // Never shown: in a string missing its `@`, the text after
                // the last `:` is the password's.
                p.parse::<u16>().map_err(|_| {
                    Error::Query("the connection string's port is not a number to 65535".into())
                })?,
            ),
            None => (hostport, 5432),
        };
        let host = if host.is_empty() { "127.0.0.1" } else { host }.to_string();

        // Written, else libpq's variable, else `verify-full` off this
        // machine -- where a stand-in for the server could take the
        // password's proof, the rows and the slot's position -- and libpq's
        // `prefer` on it; `sslrootcert=system` alone makes `verify-full`,
        // as libpq makes it.
        let (sslmode, sslmode_from) = match sslmode {
            Some(m) => (m, SslModeFrom::Written),
            None => match std::env::var("PGSSLMODE") {
                Ok(m) if !m.is_empty() => (SslMode::parse(&m)?, SslModeFrom::Environment),
                _ if sslrootcert.as_deref() == Some("system") => {
                    (SslMode::VerifyFull, SslModeFrom::SystemRoots)
                }
                _ if on_this_machine(&host) => (SslMode::Prefer, SslModeFrom::Default),
                _ => (SslMode::VerifyFull, SslModeFrom::Default),
            },
        };
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

        let (user, password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((u, p)) => (decode(u, "user name")?, Some(decode(p, "password")?)),
                None => (decode(u, "user name")?, None),
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
            host,
            port,
            database: decode(database, "database name")?,
            sslmode,
            sslmode_from,
            sslrootcert,
            require_auth,
            channel_binding: channel_binding.unwrap_or(ChannelBinding::Prefer),
            connect_timeout: connect_timeout.unwrap_or(CONNECT_WAIT),
        })
    }

    /// Whether the host is this machine ([`on_this_machine`]).
    pub fn on_this_machine(&self) -> bool {
        on_this_machine(&self.host)
    }

    /// What the dial asks of TLS, and why, in a few words for a log line or
    /// the terminal: `sslmode=verify-full (the default for a host off this
    /// machine)`. Said once a dial's program, before its first.
    pub fn tls_said(&self) -> String {
        let why = match self.sslmode_from {
            SslModeFrom::Written => "as the connection string says",
            SslModeFrom::Environment => "as PGSSLMODE says",
            SslModeFrom::SystemRoots => "as sslrootcert=system makes it",
            SslModeFrom::Default if self.on_this_machine() => {
                "the default for a host on this machine"
            }
            SslModeFrom::Default => "the default for a host off this machine",
        };
        let checks = match self.sslmode {
            SslMode::Disable => "plain text",
            SslMode::Prefer => "TLS where the server takes it, its certificate unchecked",
            SslMode::Require => "TLS, its certificate unchecked unless a CA file is there",
            SslMode::VerifyCa => "TLS, its certificate's chain checked",
            SslMode::VerifyFull => "TLS, its certificate's chain and name checked",
        };
        format!(
            "{}:{} with sslmode={} ({why}): {checks}",
            self.host,
            self.port,
            self.sslmode.name()
        )
    }
}

/// Decodes percent escapes; this is how `@` and `:` travel in passwords. A
/// `%` not followed by two hex digits is refused, as libpq refuses it,
/// naming the part it is in and none of its text: a password's. Read by
/// bytes, so a `%` before a character of several bytes is that refusal,
/// where slicing the text there panicked.
fn decode(s: &str, part: &str) -> Result<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = |at: usize| b.get(at).and_then(|&d| (d as char).to_digit(16));
            let (Some(hi), Some(lo)) = (hex(i + 1), hex(i + 2)) else {
                return Err(Error::Query(format!(
                    "the connection string's {part} holds a `%` not followed by two hex \
                     digits: a `%` itself is written %25"
                )));
            };
            out.push((hi << 4 | lo) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
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

/// An `ErrorResponse` as this client says it: `postgres <SQLSTATE>: ...`,
/// the code held to its five digits and capitals (`?????` otherwise) so a
/// follower's `retryable` reads it. Its message is the server's words where
/// the server is proven ([`Client`]: TLS checked against a CA, a sign-in
/// bound to the TLS session, or a host on this machine), escaped and cut
/// ([`peer_text`]); elsewhere they could be anyone's on the path, and what
/// is said is this client's own words for the code ([`sqlstate_words`]):
/// libpq stopped showing the error that answers its TLS request for that
/// reason (CVE-2024-10977), and an unverified connection's later errors
/// are the same stranger's.
fn server_error(m: &Message, proven: bool) -> Error {
    let code = field(m, b'C')
        .filter(|c| {
            c.len() == 5
                && c.bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
        })
        .unwrap_or_else(|| "?????".into());
    if proven {
        let msg = field(m, b'M').unwrap_or_default();
        return Error::Query(format!(
            "postgres {code}: {}",
            peer_text(msg.as_bytes(), SHOWN)
        ));
    }
    Error::Query(format!(
        "postgres {code}: {} (the server's own words are not shown: this connection is not \
         verified, and anyone on its path could have written them; sslmode=verify-full, or a \
         sign-in bound to TLS, shows them)",
        sqlstate_words(&code)
    ))
}

/// This client's words for an SQLSTATE, by its code, else by its class:
/// what is said of an error whose text is not shown ([`server_error`]).
fn sqlstate_words(code: &str) -> &'static str {
    match code {
        "28P01" => "password authentication failed",
        "28000" => "the role may not sign in so (pg_hba.conf, or no such role)",
        "3D000" => "the database does not exist",
        "42P01" => "the table does not exist",
        "42703" => "a column does not exist",
        "42704" => "an object named does not exist",
        "42501" => "permission denied",
        "42601" => "a syntax error",
        "53300" => "too many connections",
        "55006" => "the object is in use (a slot held by another connection)",
        "57P01" | "57P02" => "the server is shutting down",
        "57P03" => "the server is not accepting connections yet",
        _ => match code.get(..2).unwrap_or_default() {
            "08" => "a connection exception",
            "0A" => "a feature not supported",
            "22" => "a data exception",
            "23" => "an integrity constraint violation",
            "28" => "the authorization was refused",
            "3D" => "the catalog name is not valid",
            "40" => "a transaction rolled back",
            "42" => "a syntax error or an access rule violation",
            "53" => "insufficient resources",
            "54" => "a program limit exceeded",
            "55" => "an object not in the state needed",
            "57" => "an operator intervened",
            "58" => "a system error",
            "XX" => "an internal error",
            _ => "an error",
        },
    }
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

    /// Records rustls holds go out here, a send that a signal's handler
    /// may interrupt: nothing of a record is lost, and the flush is asked
    /// again here, since `Writer::flush_to` asks no flush again as
    /// `write_all` asks a write, and failed on it.
    fn flush(&mut self) -> stdio::Result<()> {
        loop {
            match self.flush_once() {
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                done => return done,
            }
        }
    }
}

impl Conn {
    /// Whether a message has begun to arrive, waiting up to `wait`; an
    /// `UnexpectedEof` error once the server closed. Never a read that can
    /// time out halfway through a message, which would leave the rest of it
    /// to be taken for the start of the next one: on a plain socket a peek,
    /// over TLS rustls taking whole records in (`TlsStream::readable`).
    ///
    /// A signal is no answer: a wait a signal's handler interrupted goes
    /// on, for what is left of `wait`. Linux never restarts a `recv` on a
    /// socket with a timeout, `SA_RESTART` or not (signal(7)), and every
    /// socket here has one; a SIGINT or SIGTERM lands on the thread that
    /// runs `fennec import --follow`'s follower, its main one, which waits
    /// here nearly all the time it follows an idle table. Returned, the
    /// `EINTR` was a broken stream: the follower closed it without ending
    /// it, its slot left held, and said it would connect again.
    fn readable(&mut self, wait: Duration) -> stdio::Result<bool> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.readable_within(left) {
                Err(e) if e.kind() == ErrorKind::Interrupted => {
                    if left.is_zero() {
                        return Ok(false);
                    }
                }
                got => return got,
            }
        }
    }

    /// [`Conn::readable`]'s one wait, which a signal may interrupt.
    fn readable_within(&mut self, wait: Duration) -> stdio::Result<bool> {
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

    /// The socket's write timeout, beneath TLS too.
    fn set_write_timeout(&self, wait: Duration) -> stdio::Result<()> {
        // A zero timeout is refused by the socket.
        let wait = Some(wait.max(Duration::from_millis(1)));
        match self {
            Conn::Plain(s) => s.set_write_timeout(wait),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.get_ref().set_write_timeout(wait),
        }
    }

    /// One flush, which a signal may interrupt: `Until` looks at its
    /// deadline between two, where [`Write::flush`]'s asking again would
    /// not.
    fn flush_once(&mut self) -> stdio::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.flush(),
        }
    }

    /// The socket made non-blocking, beneath TLS too: a write then takes
    /// what fits at once and waits for nothing.
    fn set_nonblocking(&self) -> stdio::Result<()> {
        match self {
            Conn::Plain(s) => s.set_nonblocking(true),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.get_ref().set_nonblocking(true),
        }
    }

    /// The socket's read timeout, beneath TLS too.
    fn set_read_timeout(&self, wait: Duration) -> stdio::Result<()> {
        // A zero timeout is refused by the socket.
        let wait = Some(wait.max(Duration::from_millis(1)));
        match self {
            Conn::Plain(s) => s.set_read_timeout(wait),
            #[cfg(feature = "tls")]
            Conn::Tls(s) => s.get_ref().set_read_timeout(wait),
        }
    }

    /// RFC 5929's `tls-server-end-point` of this connection: `None` over
    /// plain text, an error where the server's certificate gives none.
    fn channel_binding(&self) -> Option<Result<Vec<u8>>> {
        match self {
            Conn::Plain(_) => None,
            #[cfg(feature = "tls")]
            Conn::Tls(s) => Some(s.server_end_point().map_err(|e| {
                Error::Query(format!(
                    "postgres: the server offers SCRAM-SHA-256-PLUS, and its certificate binds \
                     no channel ({e}): channel_binding=disable signs in without it"
                ))
            })),
        }
    }
}

/// What is left of a sign-in's wait, or the error that says it ran out.
fn left(deadline: Instant, wait: Duration) -> Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Error::Io(format!(
            "postgres: no sign-in within {wait:?} (connect_timeout)"
        )));
    }
    Ok(left)
}

/// The connection made within the sign-in's wait: the name looked up
/// within it (`fennec_http::dial::resolve`, on a thread of its own: no
/// timeout reaches the system's resolver), then each address it stands for
/// tried within what is left. std's `connect` waited on the resolver for
/// as long as it took and on the system's own connect timeout, 75 s and
/// more.
fn dial(url: &Url, deadline: Instant) -> Result<TcpStream> {
    let addr = format!("{}:{}", url.host, url.port);
    let wait = url.connect_timeout;
    let found = fennec_http::dial::resolve(&addr, left(deadline, wait)?)
        .map_err(|e| io(format!("{addr} -- {e}")))?;
    let mut last = None;
    for a in found {
        match TcpStream::connect_timeout(&a, left(deadline, wait)?) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TIMEOUT)).ok();
                stream.set_nodelay(true).ok();
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(io(format!(
        "{addr} -- {}",
        last.map_or_else(|| "no address".to_string(), |e| e.to_string())
    )))
}

/// The stream a connection speaks over: plain, or TLS as `sslmode` asks,
/// its handshake done before the startup message -- user, database and
/// all -- is sent; and whether the server's certificate was checked
/// against a CA.
#[cfg(not(feature = "tls"))]
fn negotiate(url: &Url, deadline: Instant) -> Result<(Conn, bool)> {
    match url.sslmode {
        // libpq built without SSL takes `prefer` and sends no SSLRequest.
        SslMode::Disable | SslMode::Prefer => Ok((Conn::Plain(dial(url, deadline)?), false)),
        mode => Err(Error::Query(format!(
            "sslmode={} needs TLS, which this build was made without (the `tls` feature){}",
            mode.name(),
            match url.sslmode_from {
                SslModeFrom::Default =>
                    ": it is the default for a host off this machine; sslmode=prefer or \
                     disable speaks plain text to it",
                _ => "",
            }
        ))),
    }
}

/// The stream a connection speaks over: plain, or TLS as `sslmode` asks,
/// its handshake done before the startup message -- user, database and
/// all -- is sent, within the sign-in's wait; and whether the server's
/// certificate was checked against a CA.
#[cfg(feature = "tls")]
fn negotiate(url: &Url, deadline: Instant) -> Result<(Conn, bool)> {
    if url.sslmode == SslMode::Disable {
        return Ok((Conn::Plain(dial(url, deadline)?), false));
    }
    let wait = url.connect_timeout;
    // The CA file is read before anything is dialed.
    let (tls, checks, trusts) = tls_client(url)?;
    let mut sock = dial(url, deadline)?;
    let mut request = 8i32.to_be_bytes().to_vec();
    request.extend_from_slice(&crate::proto::SSL_REQUEST.to_be_bytes());
    sock.set_write_timeout(Some(left(deadline, wait)?)).ok();
    sock.write_all(&request).map_err(io)?;
    sock.set_write_timeout(None).ok();
    // One byte, read alone off the socket: whatever a server, or someone
    // in the path, put after an `S` goes into the handshake, which refuses
    // it, never into what is read as the server's (libpq's
    // CVE-2021-23222).
    let mut answer = [0u8; 1];
    sock.set_read_timeout(Some(left(deadline, wait)?)).ok();
    sock.read_exact(&mut answer).map_err(|e| match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => io(format!(
            "no answer to the TLS request within {wait:?} (connect_timeout)"
        )),
        _ => io(e),
    })?;
    let defaulted = url.sslmode_from == SslModeFrom::Default;
    match answer[0] {
        b'S' => match tls.dial_within(sock, &url.host, left(deadline, wait)?) {
            Ok(s) => {
                // `dial_within` leaves the socket with no timeout.
                s.get_ref().set_read_timeout(Some(TIMEOUT)).ok();
                Ok((Conn::Tls(Box::new(s)), checks))
            }
            // libpq's `prefer` goes again without TLS when the handshake
            // fails, on a new connection: fe-connect.c's CONNECTION_FAILED
            // after `pqsecure_open_client` ("SSL handshake failed. We will
            // retry with a plaintext connection, if permitted by sslmode"),
            // whose next method for `prefer` is plain text. Failing here
            // instead would keep out no one: whoever can break the
            // handshake can as well answer `N`, which `prefer` takes.
            Err(_) if url.sslmode == SslMode::Prefer => {
                Ok((Conn::Plain(dial(url, deadline)?), false))
            }
            // The network's failure is an `Io`, which a follower waits out
            // and dials again after; a certificate refused or a handshake
            // the server would not finish is a setting to change, said once.
            Err(fennec_tls::Error::Io(e)) => Err(io(e)),
            // The default refused a certificate: what this client trusts,
            // and the two ways out, a CA to trust or TLS that checks
            // nothing.
            Err(e @ fennec_tls::Error::Certificate(_)) => Err(Error::Query(format!(
                "postgres: the server's certificate could not be verified ({e}): {}it must be \
                 signed by a CA this client trusts -- {trusts} -- and name the host dialed. \
                 Name the CA that signed it with sslrootcert=<file>, or take TLS that checks \
                 nothing of the server with sslmode=require",
                match defaulted {
                    true => "sslmode=verify-full is the default for a host off this machine, and ",
                    false => "",
                }
            ))),
            Err(e) => Err(Error::Query(format!("postgres: {e}"))),
        },
        b'N' if url.sslmode == SslMode::Prefer => Ok((Conn::Plain(sock), false)),
        b'N' => Err(Error::Query(format!(
            "postgres: the server does not take TLS (its `ssl` is off), and sslmode={} asks for \
             it{}",
            url.sslmode.name(),
            match defaulted {
                true =>
                    ": verify-full is the default for a host off this machine; \
                          sslmode=disable speaks plain text to it, which anyone on the path \
                          can read and answer",
                false => "",
            }
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
/// libpq's root file ([`default_root`]); whether it checks the
/// certificate against a CA; and what it trusts, in words for a refusal.
#[cfg(feature = "tls")]
fn tls_client(url: &Url) -> Result<(fennec_tls::Client, bool, String)> {
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
    let trusts = match (&file, url.sslrootcert.as_deref()) {
        (Some(f), Some(_)) => format!("the CAs of sslrootcert's {}", f.display()),
        (Some(f), None) => format!("the CAs of libpq's root file, {}", f.display()),
        (None, _) => "Mozilla's public CAs, compiled in; no sslrootcert is named, and libpq's \
                      ~/.postgresql/root.crt is not there"
            .to_string(),
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
    let client = match url.sslmode {
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
    };
    let verified = match url.sslmode {
        SslMode::Require => file.is_some(),
        mode => matches!(mode, SslMode::VerifyCa | SslMode::VerifyFull),
    };
    Ok((client, verified, trusts))
}

// ------------------------------------------------------------------- SCRAM

/// The most PBKDF2 iterations SCRAM runs for a server: 2^22, 1 024 times
/// PostgreSQL's 4 096, its `scram_iterations` default, with room above a
/// hardened setting near a million (OWASP asks 600 000 of
/// PBKDF2-HMAC-SHA256), which the first bound, 2^20, barely had. The
/// count is the server's to choose -- and, under any `sslmode` short of
/// `verify-full`, whoever stands in for it -- and is run before the proof
/// goes out: `i=4294967295` was 54 trillion instructions of PBKDF2, more
/// than half an hour of an M1 core even at its widest (eight a cycle at
/// 3.2 GHz), `fennec import`, or `--follow` connecting again, waiting on
/// it. The bound is 28.6 thousand million, seconds of a core. A count past
/// it is refused naming the range and how to come under it, never run, nor
/// cut to fit.
pub const SCRAM_MAX_ITERATIONS: u32 = 1 << 22;

/// The fewest PBKDF2 iterations SCRAM runs for a server: 4 096, RFC 7677's
/// floor ("at least 4096") and PostgreSQL's default. A server, or whoever
/// stands in for it, asking for fewer makes the proof that goes out cheap
/// to guess the password from: `i=1` was about four HMACs a guess.
pub const SCRAM_MIN_ITERATIONS: u32 = 4096;

/// What a client-first message says of channel binding: its GS2 header
/// (RFC 5802, section 7), and the binding's data the client-final message
/// carries with it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    /// `n,,`: the client binds no channel -- none here, or
    /// `channel_binding=disable`.
    None,
    /// `y,,`: the client could bind its TLS session, and the server
    /// offered no `-PLUS`. A server that did offer one, the offer taken
    /// out on the way, sees this and refuses the sign-in.
    Unoffered,
    /// `p=tls-server-end-point,,`, with the hash of the server's
    /// certificate as the data (RFC 5929): `SCRAM-SHA-256-PLUS`.
    Plus(Vec<u8>),
}

/// The client half of one SCRAM-SHA-256 exchange (RFC 5802, RFC 7677), its
/// messages made and checked apart from the connection, the nonce given:
/// [`Client::scram`] gives the system's, and `scram_rfc7677_exchange` RFC
/// 7677's, holding the messages to the RFC's byte for byte.
struct Scram {
    /// The client-first message less its GS2 header: `n=<user>,r=<nonce>`.
    first_bare: String,
    /// The client's nonce, which the server's must begin with.
    nonce: String,
    binding: Binding,
}

/// What the server-final message must be: `v=` and the server's
/// signature, which only a holder of the password's verifier can make.
struct ServerFinal(String);

impl Scram {
    /// `user` is written as given: the client sends none, and RFC 7677's
    /// example `user`, neither with a `,` or `=` to escape. No channel
    /// binding (`n,,`), as the RFC's example has none.
    #[cfg(test)]
    fn new(user: &str, nonce: &str) -> Scram {
        Scram::bound(user, nonce, Binding::None)
    }

    fn bound(user: &str, nonce: &str, binding: Binding) -> Scram {
        Scram {
            first_bare: format!("n={user},r={nonce}"),
            nonce: nonce.to_string(),
            binding,
        }
    }

    /// The GS2 header: what the client says of channel binding.
    fn gs2(&self) -> &'static str {
        match self.binding {
            Binding::None => "n,,",
            Binding::Unoffered => "y,,",
            Binding::Plus(_) => "p=tls-server-end-point,,",
        }
    }

    /// The client-first message, its GS2 header first.
    fn first(&self) -> String {
        format!("{}{}", self.gs2(), self.first_bare)
    }

    /// The client-final message answering `server_first` -- the proof that
    /// `password` is known -- and the server-final message to expect.
    ///
    /// Refused before PBKDF2 runs: a nonce not the client's followed by the
    /// server's own (equal is no nonce of the server's), an iteration count
    /// outside [`SCRAM_MIN_ITERATIONS`] to [`SCRAM_MAX_ITERATIONS`], and an
    /// `m=` attribute, which RFC 5802 says to refuse (section 5.1: "its
    /// presence in a client or a server message MUST cause authentication
    /// failure"). The password goes into PBKDF2 as its UTF-8 bytes, with no
    /// SASLprep: what libpq sends where its SASLprep refuses a password, and
    /// what the server made the verifier of where its own did -- every ASCII
    /// password, and every other one SASLprep leaves as it is (NFKC, no
    /// space but U+0020, no character it maps to nothing), signs in alike.
    fn last(&self, server_first: &str, password: &str) -> Result<(String, ServerFinal)> {
        let (mut snonce, mut salt, mut iterations) = (None, None, None);
        for kv in server_first.split(',') {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k {
                "m" => {
                    return Err(Error::Query(
                        "postgres: the server's SCRAM message carries an `m` extension, which \
                         RFC 5802 says to refuse"
                            .into(),
                    ))
                }
                "r" => snonce = Some(v),
                "s" => salt = Some(b64_decode(v).ok_or_else(scram_unparsed)?),
                "i" => iterations = Some(scram_iterations(v)?),
                _ => {}
            }
        }
        // The server has to carry our nonce as a prefix and add its own;
        // otherwise the response belongs to another session, or is ours
        // sent back.
        let (Some(snonce), Some(salt), Some(iterations)) = (snonce, salt, iterations) else {
            return Err(scram_invalid());
        };
        if !snonce.starts_with(&self.nonce) || snonce.len() <= self.nonce.len() || salt.is_empty() {
            return Err(scram_invalid());
        }

        // `c=`: the GS2 header and the binding's data, base64 -- `biws`
        // for `n,,`. The proof covers it, so a header changed on the way
        // fails the proof at the server.
        let mut header = self.gs2().as_bytes().to_vec();
        if let Binding::Plus(data) = &self.binding {
            header.extend_from_slice(data);
        }
        let without_proof = format!("c={},r={snonce}", b64_encode(&header));
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

/// A server-first message's iteration count, held to
/// [`SCRAM_MIN_ITERATIONS`] to [`SCRAM_MAX_ITERATIONS`]: a count outside,
/// however many digits it has, is refused naming the range before PBKDF2
/// runs, and what brings a server's within it: the role's password set
/// again, its verifier made under another `scram_iterations`.
fn scram_iterations(v: &str) -> Result<u32> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(scram_unparsed());
    }
    // A count no `u32` holds is past the bound too; one past twenty digits
    // is shown by its length, the message being the server's, as long as a
    // gigabyte.
    let digits = v.trim_start_matches('0');
    let n = match digits {
        "" => Some(0),
        d => d.parse::<u32>().ok(),
    };
    if let Some(n @ SCRAM_MIN_ITERATIONS..=SCRAM_MAX_ITERATIONS) = n {
        return Ok(n);
    }
    let shown = if digits.len() > 20 {
        format!("a count of {} digits", digits.len())
    } else {
        v.to_string()
    };
    let way = match n {
        Some(n) if n < SCRAM_MIN_ITERATIONS => {
            "RFC 7677's floor: set the role's password again under a scram_iterations of \
             4096 or more"
        }
        _ => "set the role's password again under a lower scram_iterations",
    };
    Err(Error::Query(format!(
        "postgres: the server's SCRAM iteration count is {SCRAM_MIN_ITERATIONS} to \
         {SCRAM_MAX_ITERATIONS}, not {shown}: {way}"
    )))
}

fn scram_unparsed() -> Error {
    Error::Corrupt("postgres: the SCRAM response could not be parsed".into())
}

fn no_password() -> Error {
    Error::Query(
        "the server wants a password; add it to the connection string or set PGPASSWORD".into(),
    )
}

/// The SASL mechanisms an `AuthenticationSASL` offers: names, each ended by
/// a zero byte, the list by an empty one.
fn mechanisms(body: &[u8]) -> Vec<String> {
    body.split(|&b| b == 0)
        .take_while(|m| !m.is_empty())
        .map(|m| String::from_utf8_lossy(m).into_owned())
        .collect()
}

/// The most rows a simple query's answer may hold ([`Client::query`]).
/// This client's queries ask a catalog for a row or a few -- a setting, a
/// table's name and columns, the extension types' oids, a publication, a
/// slot, a count, a row read back by its key -- so 100 000 is past any of
/// them by far, and the rows of a table come by COPY.
pub const QUERY_ROWS: usize = 100_000;

fn scram_invalid() -> Error {
    Error::Query("postgres: invalid SCRAM server response".into())
}

/// A connection to a live PostgreSQL server.
#[derive(Debug)]
pub struct Client {
    stream: BufReader<Conn>,
    /// The version the server reports; useful in error text.
    pub server_version: String,
    /// Whether what comes over the connection is the server's: its
    /// certificate checked against a CA (`verify-ca`, `verify-full`,
    /// `require` with a CA file), a sign-in by `SCRAM-SHA-256-PLUS` -- whose
    /// binding ties this TLS session to whoever holds the password's
    /// verifier -- or a host on this machine, where there is no path to
    /// stand on. Elsewhere anyone on the path may write what the client
    /// reads: an error's words are not shown ([`server_error`]), and a
    /// replication stream's keepalive moves nothing it confirms
    /// ([`WalStream::proven`]).
    proven: bool,
}

/// A flag nothing sets: a wait with no stop to look at.
static NEVER: AtomicBool = AtomicBool::new(false);

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

    /// Whether what comes over the connection is the server's (see the
    /// field).
    pub fn proven(&self) -> bool {
        self.proven
    }

    /// The connection, signed in: everything up to the server's first
    /// `ReadyForQuery` within `connect_timeout`, PBKDF2's own time apart;
    /// what the server asks for held to `require_auth` and
    /// `channel_binding`, libpq's rules.
    fn open(url: &Url, replication: bool) -> Result<Client> {
        let began = Instant::now();
        let wait = url.connect_timeout;
        // A bound past what an `Instant` holds is a year's: no bound at all.
        let mut deadline = began
            .checked_add(wait)
            .unwrap_or_else(|| began + Duration::from_secs(365 * 86_400));
        let (conn, verified) = negotiate(url, deadline)?;
        let mut c = Client {
            stream: BufReader::new(conn),
            server_version: String::new(),
            proven: verified || url.on_this_machine(),
        };
        let policy = url
            .require_auth
            .clone()
            .unwrap_or_else(|| RequireAuth::default_for(url.password.is_some()));

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
        // The sign-in's writes are a few hundred bytes into an empty send
        // buffer, which no server not reading holds up; its reads are held
        // to the deadline.
        c.stream.get_mut().write_all(&packet).map_err(io)?;
        c.stream.get_mut().flush().map_err(io)?;

        // Whether the client finished an exchange -- the password sent, or
        // SCRAM's server signature verified -- and whether that exchange
        // was bound to the TLS session.
        let (mut finished, mut bound) = (false, false);
        loop {
            let m = c.signing_in(deadline, wait)?;
            match m.tag {
                b'R' => {
                    if m.body.len() < 4 {
                        return Err(Error::Corrupt("postgres: short auth message".into()));
                    }
                    let code = be_i32(&m.body, 0);
                    policy.allows(code, finished)?;
                    if url.channel_binding == ChannelBinding::Require
                        && !matches!(code, AUTH_OK | AUTH_SASL)
                    {
                        return Err(Error::Query(
                            "postgres: channel binding is required, and the server asked for \
                             another authentication than SCRAM (channel_binding=require)"
                                .into(),
                        ));
                    }
                    match code {
                        AUTH_OK if url.channel_binding == ChannelBinding::Require && !bound => {
                            return Err(Error::Query(
                                "postgres: channel binding is required, and the server signed \
                                 the client in without it (channel_binding=require)"
                                    .into(),
                            ))
                        }
                        AUTH_OK => {}
                        AUTH_PASSWORD => {
                            let pw = url.password.as_deref().ok_or_else(no_password)?;
                            let mut w = Writer::new();
                            w.msg(b'p', |b| put_cstr(b, pw));
                            w.flush_to(c.stream.get_mut()).map_err(io)?;
                            finished = true;
                        }
                        AUTH_SASL => {
                            let pw = url.password.as_deref().ok_or_else(no_password)?;
                            let offered = mechanisms(&m.body[4..]);
                            bound = c.scram(url, &offered, pw, &mut deadline)?;
                            finished = true;
                            // A sign-in bound to this TLS session: the
                            // session's other end holds the verifier.
                            c.proven |= bound;
                        }
                        AUTH_SASL_CONT | AUTH_SASL_FIN => {
                            return Err(Error::Corrupt(
                                "postgres: a SASL message outside a SASL exchange".into(),
                            ))
                        }
                        AUTH_MD5 => {
                            return Err(Error::Query(
                                "the server wants md5 authentication; that is not supported. \
                                 Switch the server to scram-sha-256"
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
                        c.server_version = peer_text(v.as_bytes(), 64);
                    }
                }
                b'K' | b'N' => {}
                b'E' => return Err(server_error(&m, c.proven)),
                b'Z' => return Ok(c),
                other => {
                    return Err(Error::Corrupt(format!(
                        "postgres: unexpected message `{}` while connecting",
                        other.escape_ascii()
                    )))
                }
            }
        }
    }

    /// A message of the sign-in, read within what is left of its wait: a
    /// server that answers nothing, or a byte at a time, is given no more.
    fn signing_in(&mut self, deadline: Instant, wait: Duration) -> Result<Message> {
        let mut r = Until {
            stream: &mut self.stream,
            deadline,
            cut: &NEVER,
            gave: None,
        };
        let read = read_message(&mut r);
        match (read, r.gave) {
            (Ok(m), _) => Ok(m),
            (Err(_), Some(_)) => Err(io(format!(
                "no sign-in within {wait:?} (connect_timeout): the server answered too \
                 little, too slowly"
            ))),
            (Err(e), None) => Err(io(e)),
        }
    }

    /// The SCRAM-SHA-256 client half (RFC 5802 + RFC 7677) over the
    /// connection, its messages [`Scram`]'s: whether the sign-in was bound
    /// to the TLS session.
    ///
    /// The password is never sent over the wire; only nonces and HMAC proofs
    /// travel. The server signature is verified too, which proves the other
    /// side really knows the password's verifier. The mechanism is the
    /// server's list's (`offered`) and `channel_binding`'s, libpq's choice:
    /// `-PLUS` over TLS where offered and allowed, its binding the hash of
    /// the server's certificate; plain SCRAM otherwise, its header saying
    /// whether the client could have bound (`y,,`) or not (`n,,`).
    fn scram(
        &mut self,
        url: &Url,
        offered: &[String],
        password: &str,
        deadline: &mut Instant,
    ) -> Result<bool> {
        let wait = url.connect_timeout;
        let plus = offered.iter().any(|m| m == "SCRAM-SHA-256-PLUS");
        let plain = offered.iter().any(|m| m == "SCRAM-SHA-256");
        let tls = self.stream.get_ref().channel_binding();
        let (mechanism, binding) = match (url.channel_binding, tls, plus, plain) {
            (ChannelBinding::Disable, _, _, true) => ("SCRAM-SHA-256", Binding::None),
            (ChannelBinding::Prefer | ChannelBinding::Require, Some(data), true, _) => {
                ("SCRAM-SHA-256-PLUS", Binding::Plus(data?))
            }
            (ChannelBinding::Require, None, ..) => return Err(Error::Query(
                "postgres: channel binding is required, and this connection is plain text, with \
                     nothing to bind (channel_binding=require needs an sslmode that asks for TLS)"
                    .into(),
            )),
            (ChannelBinding::Require, Some(_), false, _) => {
                return Err(Error::Query(
                    "postgres: channel binding is required, and the server does not offer \
                     SCRAM-SHA-256-PLUS (channel_binding=require)"
                        .into(),
                ))
            }
            (ChannelBinding::Prefer, Some(_), false, true) => ("SCRAM-SHA-256", Binding::Unoffered),
            (ChannelBinding::Prefer, None, _, true) => ("SCRAM-SHA-256", Binding::None),
            _ => {
                let list = offered.join(", ");
                return Err(Error::Query(format!(
                    "postgres: the server offers no SASL mechanism this client speaks \
                     (SCRAM-SHA-256 and its -PLUS): {}",
                    peer_text(list.as_bytes(), SHOWN)
                )));
            }
        };
        let bound = matches!(binding, Binding::Plus(_));
        // The system's randomness or no sign-in: a nonce of the clock's could
        // come again, and a proof with it.
        let cnonce =
            nonce(24).ok_or_else(|| io("no randomness from the system for SCRAM's nonce"))?;
        // No user name: PostgreSQL reads the startup message's, and libpq
        // sends `n=,` too.
        let scram = Scram::bound("", &cnonce, binding);
        let initial = scram.first();
        let mut w = Writer::new();
        w.msg(b'p', |b| {
            put_cstr(b, mechanism);
            b.extend_from_slice(&(initial.len() as i32).to_be_bytes());
            b.extend_from_slice(initial.as_bytes());
        });
        w.flush_to(self.stream.get_mut()).map_err(io)?;

        let server_first = self.sasl_answer(AUTH_SASL_CONT, *deadline, wait)?;
        // PBKDF2 is the client's own time, not the server's to spend: the
        // wait goes on after it for what was left before it.
        let pbkdf2 = Instant::now();
        let (last, server_final) = scram.last(&server_first, password)?;
        *deadline += pbkdf2.elapsed();
        let mut w = Writer::new();
        w.msg(b'p', |b| b.extend_from_slice(last.as_bytes()));
        w.flush_to(self.stream.get_mut()).map_err(io)?;

        server_final.verify(&self.sasl_answer(AUTH_SASL_FIN, *deadline, wait)?)?;
        Ok(bound)
    }

    /// The text of a SASL answer -- the server-first (`code` 11) or the
    /// server-final (12) message -- or the server's error. Any other answer
    /// ends the exchange unfinished, an `AuthenticationOk` among them: a
    /// server that signs the client in before it proved it holds the
    /// verifier is no server this client signs in to.
    fn sasl_answer(&mut self, code: i32, deadline: Instant, wait: Duration) -> Result<String> {
        let m = self.signing_in(deadline, wait)?;
        if m.tag == b'E' {
            return Err(server_error(&m, self.proven));
        }
        match (m.tag, m.body.get(..4).map(|b| be_i32(b, 0))) {
            (b'R', Some(c)) if c == code => Ok(String::from_utf8_lossy(&m.body[4..]).into_owned()),
            (b'R', Some(AUTH_OK)) => Err(Error::Query(
                "postgres: the server signed the client in before SCRAM's exchange was done, \
                 its signature unproven: refused, as a stand-in for the server would do it"
                    .into(),
            )),
            _ => Err(scram_unparsed()),
        }
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
    ///
    /// An answer is held to [`QUERY_ROWS`] rows and to what one message
    /// may be, [`MAX_MESSAGE`], counted as it is kept: a cell kept is 24
    /// bytes besides its text, where a null one costs the server 4, so a
    /// gigabyte of nulls was six of memory, and rows were kept with no
    /// bound at all. Past either the answer is refused in words, not cut.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.send_query(sql)?;
        let mut out = QueryResult::default();
        let mut failed = None;
        let mut kept = 0usize;
        loop {
            let m = self.read()?;
            match m.tag {
                b'T' => out.columns = row_description(&m)?,
                b'D' => {
                    let row = data_row(&m)?;
                    kept = kept.saturating_add(24 + 24 * row.len() + m.body.len());
                    if out.rows.len() >= QUERY_ROWS || kept > MAX_MESSAGE {
                        return Err(Error::Query(format!(
                            "postgres: a query's answer past {QUERY_ROWS} rows or \
                             {MAX_MESSAGE} bytes kept: refused, as this client's queries ask \
                             a catalog for a row or a few"
                        )));
                    }
                    out.rows.push(row);
                }
                b'E' => failed = Some(server_error(&m, self.proven)),
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
                    let e = server_error(&m, self.proven);
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
        // From here every read is a `Watched` one, which waits on the
        // socket a `LOOK` at a time, looking at its stop between.
        self.stream.get_ref().set_read_timeout(LOOK).map_err(io)?;
        Ok(CopyOut {
            client: self,
            buf: Vec::new(),
            pos: 0,
            scanned: 0,
            done: false,
            limit: MAX_MESSAGE,
            looked: 0,
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
                b'E' => return Err(server_error(&m, self.proven)),
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
    /// Whether what the stream says is the server's ([`Client`]'s field):
    /// only then is a keepalive's position the walsender's word, which
    /// PostgreSQL's own subscriber confirms where nothing is pending.
    /// Elsewhere a keepalive may be anyone's on the path, naming any
    /// position, and the follower confirms only what it received and put
    /// on disk, as `pg_recvlogical` does.
    pub fn proven(&self) -> bool {
        self.client.proven
    }

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
            b'E' => Err(server_error(&m, self.client.proven)),
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

    /// Ends the stream as the protocol ends one: `CopyDone`, then the
    /// server's messages read up to its `ReadyForQuery`. The walsender lets
    /// the slot go before it answers (`ReplicationSlotRelease`, then
    /// `CommandComplete`), so once this returns `Ok` the slot is free: to
    /// drop, or to stream again.
    ///
    /// Closing the connection instead left the slot held until the
    /// walsender next read its socket, which it does not while it decodes
    /// a transaction -- any table's, published or not -- so a slot dropped,
    /// or streamed again, just after its follower stopped was refused,
    /// `55006`, "active for PID". What the server still sends of the stream
    /// is dropped unread: nothing of it was applied, so nothing of it is
    /// confirmed, and it comes again from the slot.
    ///
    /// `wait` bounds it all: the `CopyDone`'s write and every read -- the
    /// rest of a message already begun and what was buffered with it
    /// included, over TLS too, where a read of the rest of a message waited
    /// out the socket's own `TIMEOUT`, 30 s, a read (`Until`) -- and the
    /// `Terminate` of the close after it, which the drop sends only where
    /// the socket takes it at once. A signal neither ends the wait nor
    /// starts it again. It ends sooner, within [`CUT_LOOK`], once `cut` is
    /// set: the caller's second stop. Either way the connection is then
    /// closed, and the server lets the slot go once it reads that, if it
    /// still holds it ([`Unended`] says which).
    pub fn end(mut self, wait: Duration, cut: &AtomicBool) -> std::result::Result<(), Unended> {
        let proven = self.client.proven;
        let mut r = Until {
            stream: &mut self.client.stream,
            deadline: Instant::now() + wait,
            cut,
            gave: None,
        };
        let mut failed = None;
        let ended = ending(&mut r, &mut failed, proven);
        match (ended, r.gave) {
            (Ok(()), _) => failed.map_or(Ok(()), |e| Err(Unended::Failed(e))),
            (Err(_), Some(Gave::Late)) => Err(Unended::Held(io(format!(
                "the server did not end the replication stream within {wait:?}"
            )))),
            (Err(_), Some(Gave::Cut)) => Err(Unended::Held(io(
                "the wait for the server to end the replication stream was cut short",
            ))),
            // The server's own error, when it closed after it.
            (Err(e), None) => Err(Unended::Failed(failed.unwrap_or_else(|| {
                if e.kind() == ErrorKind::UnexpectedEof {
                    io("the server closed the replication stream")
                } else {
                    io(e)
                }
            }))),
        }
    }
}

/// [`WalStream::end`]'s exchange: `CopyDone` out, then the server's messages
/// up to its `ReadyForQuery`, an error it sent kept in `failed`.
fn ending(r: &mut Until<'_>, failed: &mut Option<Error>, proven: bool) -> stdio::Result<()> {
    let mut w = Writer::new();
    w.msg(b'c', |_| {});
    w.flush_to(r)?;
    loop {
        let m = read_message(r)?;
        match m.tag {
            b'E' => *failed = Some(server_error(&m, proven)),
            b'Z' => return Ok(()),
            // CopyData still on its way, the server's CopyDone, its
            // CommandComplete, a notice.
            _ => {}
        }
    }
}

/// Why [`WalStream::end`] returned short of the server's answer. The
/// connection is closed either way, and the server lets the slot go once
/// it reads that -- if it still holds it.
#[derive(Debug)]
pub enum Unended {
    /// The server said nothing within the wait, or the wait was cut short:
    /// it is busy -- decoding a transaction -- and holds the slot until it
    /// next reads the connection.
    Held(Error),
    /// The connection ended or failed first, or the server answered with
    /// an error: its walsender may have let the slot go, as one that exits
    /// or fails does, or not yet -- a proxy's close, a network's failure.
    Failed(Error),
}

impl std::fmt::Display for Unended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unended::Held(e) | Unended::Failed(e) => e.fmt(f),
        }
    }
}

/// How often [`WalStream::end`] looks at its `cut` while it waits.
pub const CUT_LOOK: Duration = Duration::from_millis(50);

/// Why a read or write of [`Until`] gave up.
#[derive(Clone, Copy)]
enum Gave {
    Late,
    Cut,
}

/// A connection read and written with every wait held to a deadline and a
/// flag. A read that would go to the socket -- the reader's buffer empty --
/// waits first for what is left (`Conn::readable`: a peek, or rustls taking
/// whole records in), and reads only once bytes are there, which it then
/// takes without waiting; a write waits for room no longer than what is
/// left, the socket's write timeout set to it before each try. Either
/// waits in slices of [`CUT_LOOK`], so that `cut` is looked at, and goes
/// on after a signal for what is left: no read or write blocks past the
/// deadline, then, even in the middle of a message, whose rest a plain
/// read would wait for under the socket's timeout, nor at a full send
/// buffer, nor starts its wait again at each signal, as `read_exact` and
/// `write_all`, asking again, would. What it stops in the middle of is
/// never read or written on: the connection is closed after it.
struct Until<'a> {
    stream: &'a mut BufReader<Conn>,
    deadline: Instant,
    cut: &'a AtomicBool,
    gave: Option<Gave>,
}

impl Until<'_> {
    /// The next wait: a slice of what is left, or, cut or past the
    /// deadline, the error that gives up, `gave` saying why.
    fn slice(&mut self) -> stdio::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        let gave = if self.cut.load(Ordering::SeqCst) {
            Gave::Cut
        } else if left.is_zero() {
            Gave::Late
        } else {
            return Ok(left.min(CUT_LOOK));
        };
        self.gave = Some(gave);
        // Not `Interrupted`, which `read_exact` and `write_all` would ask
        // again.
        Err(ErrorKind::TimedOut.into())
    }

    /// `step` -- a write, or a flush of what rustls holds -- tried until
    /// it is done or the wait is over: a try that took nothing as the
    /// slice ran out, or that a signal interrupted, is made again.
    fn sending<T>(
        &mut self,
        mut step: impl FnMut(&mut Conn) -> stdio::Result<T>,
    ) -> stdio::Result<T> {
        loop {
            let wait = self.slice()?;
            let conn = self.stream.get_mut();
            conn.set_write_timeout(wait)?;
            match step(conn) {
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) => {}
                done => return done,
            }
        }
    }
}

impl Read for Until<'_> {
    fn read(&mut self, buf: &mut [u8]) -> stdio::Result<usize> {
        while self.stream.buffer().is_empty() {
            let wait = self.slice()?;
            if self.stream.get_mut().readable(wait)? {
                break;
            }
        }
        self.stream.read(buf)
    }
}

impl Write for Until<'_> {
    fn write(&mut self, buf: &[u8]) -> stdio::Result<usize> {
        self.sending(|c| c.write(buf))
    }

    fn flush(&mut self) -> stdio::Result<()> {
        self.sending(|c| c.flush_once())
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
            other.escape_ascii()
        ))),
        None => Err(short()),
    }
}

impl Drop for Client {
    /// Terminate, which lets the server close the connection cleanly --
    /// sent only where the socket takes it at once. A drop waits for
    /// nothing: where the send buffer is full the server is reading
    /// nothing, and the close tells it as much once it reads. Waiting held
    /// a stop's end a whole `wait` past its bound, and a failed follower's
    /// return -- which `fennec-server`'s mirror stores its exit code after
    /// -- with no bound at all.
    fn drop(&mut self) {
        if self.stream.get_ref().set_nonblocking().is_ok() {
            let mut w = Writer::new();
            w.msg(b'X', |_| {});
            let _ = w.flush_to(self.stream.get_mut());
        }
    }
}

/// A `COPY ... TO STDOUT` stream.
#[derive(Debug)]
pub struct CopyOut {
    client: Client,
    buf: Vec<u8>,
    /// Where the next line begins.
    pos: usize,
    /// How far the buffer has been looked through for the next line's end:
    /// a byte is looked at once, where every `CopyData` looked through the
    /// whole line again -- 1-byte messages of a 10 MB line were some 5e13
    /// comparisons.
    scanned: usize,
    done: bool,
    /// The longest line taken: [`MAX_MESSAGE`], PostgreSQL's bound of the
    /// row it writes as a line (a `StringInfo`, `MaxAllocSize`).
    limit: usize,
    /// The bytes looked at for line ends, all told: what a test holds to
    /// the bytes that came.
    looked: u64,
}

impl CopyOut {
    /// The raw bytes of the next line (without the line ending), or `None`
    /// when the stream is done. `CopyData` frames do not align with line
    /// boundaries: a frame can end mid-line or carry several lines.
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>> {
        self.next_line_until(&NEVER)
    }

    /// [`CopyOut::next_line`], given up once `stop` is set: every read
    /// waits on the server a [`LOOK`] at a time and looks at `stop`
    /// between, so a stop wakes a read the server holds up -- a `fennec-server
    /// --follow` copying a table for its mirror, at the shutdown -- within
    /// it. The server's silence past [`TIMEOUT`] is an error too, as a
    /// read's timeout was. A line longer than [`MAX_MESSAGE`] is refused,
    /// where it was kept growing for as long as the server sent no newline.
    pub fn next_line_until(&mut self, stop: &AtomicBool) -> Result<Option<Vec<u8>>> {
        let too_long = |limit: usize| {
            Error::Corrupt(format!(
                "postgres: a COPY line past {limit} bytes, PostgreSQL's bound of a row: refused"
            ))
        };
        loop {
            let unseen = &self.buf[self.scanned..];
            if let Some(i) = unseen.iter().position(|&b| b == b'\n') {
                self.looked += i as u64 + 1;
                let end = self.scanned + i;
                if end - self.pos > self.limit {
                    return Err(too_long(self.limit));
                }
                let line = self.buf[self.pos..end].to_vec();
                self.pos = end + 1;
                self.scanned = self.pos;
                return Ok(Some(line));
            }
            self.looked += unseen.len() as u64;
            self.scanned = self.buf.len();
            if self.buf.len() - self.pos > self.limit {
                return Err(too_long(self.limit));
            }
            if self.done {
                if self.pos < self.buf.len() {
                    let line = self.buf[self.pos..].to_vec();
                    self.pos = self.buf.len();
                    self.scanned = self.pos;
                    return Ok(Some(line));
                }
                return Ok(None);
            }
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.scanned -= self.pos;
                self.pos = 0;
            }
            let m = self.watched(stop)?;
            match m.tag {
                b'd' => self.buf.extend_from_slice(&m.body),
                b'c' => {
                    self.done = true;
                    // CommandComplete + ReadyForQuery: keep the connection usable.
                    while self.watched(stop)?.tag != b'Z' {}
                }
                b'E' => {
                    let e = server_error(&m, self.client.proven);
                    while self.watched(stop)?.tag != b'Z' {}
                    return Err(e);
                }
                _ => {}
            }
        }
    }

    /// The next message, read through [`Watched`].
    fn watched(&mut self, stop: &AtomicBool) -> Result<Message> {
        let mut r = Watched {
            stream: &mut self.client.stream,
            stop,
            quiet: None,
        };
        read_message(&mut r).map_err(|e| match e.kind() {
            ErrorKind::TimedOut => io(format!("the server sent nothing for {TIMEOUT:?}")),
            _ => io(e),
        })
    }
}

/// A COPY's reads, its socket's read timeout [`LOOK`]: a read the timeout
/// ends is asked again -- nothing of the message is lost, the bytes taken
/// before it being the reader's, rustls's or the socket's -- until `stop`
/// is set, an error then, or the server has been silent for [`TIMEOUT`].
///
/// What the buffer already holds is taken with no look at the flag or the
/// clock (`read_exact`): a message is three reads, a row one message, and a
/// clock read and the generic read path at each were 740 instructions a row,
/// 4 % of a million-row import (19.20 against 18.41 thousand million, now
/// 18.32). The flag is looked at whenever a read goes
/// to the socket, at least every buffer's worth, and the clock only once a
/// read timed out: the silence is counted from the first timeout after the
/// last byte, within `LOOK` of it.
struct Watched<'a> {
    stream: &'a mut BufReader<Conn>,
    stop: &'a AtomicBool,
    /// Since when the server has said nothing: the first timeout after its
    /// last byte.
    quiet: Option<Instant>,
}

impl Read for Watched<'_> {
    fn read(&mut self, buf: &mut [u8]) -> stdio::Result<usize> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(stdio::Error::other("the copy was stopped"));
            }
            match self.stream.read(buf) {
                Ok(n) => {
                    self.quiet = None;
                    return Ok(n);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    let since = *self.quiet.get_or_insert_with(Instant::now);
                    if since.elapsed() >= TIMEOUT {
                        return Err(ErrorKind::TimedOut.into());
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn read_exact(&mut self, mut buf: &mut [u8]) -> stdio::Result<()> {
        let held = self.stream.buffer();
        if held.len() >= buf.len() {
            buf.copy_from_slice(&held[..buf.len()]);
            self.stream.consume(buf.len());
            return Ok(());
        }
        while !buf.is_empty() {
            match self.read(buf)? {
                0 => return Err(ErrorKind::UnexpectedEof.into()),
                n => buf = &mut buf[n..],
            }
        }
        Ok(())
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
            "keepalives=1",
            "sslmode=allow",
            "sslmode=on",
            "connect_timeout=0",
            "connect_timeout=-1",
            "connect_timeout=soon",
            "channel_binding=maybe",
            "require_auth=trust",
        ] {
            assert!(
                Url::parse(&format!("postgres://alice@localhost/t?{p}")).is_err(),
                "{p}"
            );
        }
        // An empty value is libpq's default, as no value.
        let u = Url::parse("postgres://alice@localhost/t?require_auth=&sslrootcert=").unwrap();
        assert_eq!((u.require_auth, u.sslrootcert), (None, None));
    }

    /// No refusal of a connection string shows its password, or any of it:
    /// a wrong scheme printed the whole string, a `?` left unescaped in a
    /// password made its rest a "parameter" printed whole, and a `%` before
    /// a character of several bytes panicked the decoder, which sliced the
    /// text there.
    #[test]
    fn a_refused_connection_string_shows_no_secret() {
        let secret = "hunter2";
        for s in [
            "mysql://alice:hunter2@db.example/t",
            "hunter2@db.example/t",
            "postgres://alice:hunter2?x@db.example/t",
            "postgres://alice:hun?ter2@db.example/t",
            "postgres://alice:x?sslmode=hunter2@db.example/t",
            "postgres://alice:x?connect_timeout=hunter2@db.example/t",
            "postgres://alice:x?hunter2=1/t",
            "postgres://alice:hunter2%zz@db.example/t",
            "postgres://alice:hunter2%@db.example/t",
            "postgres://alice:hunter2%e@db.example/t",
            // No `@`: the text after the last `:` is the password, read
            // where the port would be.
            "postgres://alice:hunter2/t",
        ] {
            let e = Url::parse(s).unwrap_err().to_string();
            assert!(!e.contains(secret) && !e.contains("hunter"), "{s}: {e}");
        }
        let e = Url::parse("mysql://alice:hunter2@db.example/t").unwrap_err();
        assert!(e.to_string().contains("beginning mysql://"), "{e}");
        let e = Url::parse("postgres://alice:x?hunter2=1@db.example/t").unwrap_err();
        assert!(e.to_string().contains("%3F"), "{e}");
        // A `%` before a character of several bytes, or at the end: refused
        // in words naming the part, where the first panicked.
        for s in [
            "postgres://alice:p%a\u{e9}@db.example/t",
            "postgres://alice:p%\u{1F98A}@h/t",
        ] {
            let e = Url::parse(s).unwrap_err().to_string();
            assert!(
                e.contains("password holds a `%` not followed by two hex"),
                "{e}"
            );
        }
        let e = Url::parse("postgres://al%2@h/t").unwrap_err().to_string();
        assert!(e.contains("user name holds a `%`"), "{e}");
        // Escapes still decode: `@`, `:`, `?` and a character of several
        // bytes.
        let u = Url::parse("postgres://alice:a%40b%3Ac%3F%C3%A9@h/t").unwrap();
        assert_eq!(u.password.as_deref(), Some("a@b:c?\u{e9}"));
        // A libpq keyword this client does not take is named; no other
        // text is.
        let e = Url::parse("postgres://alice@h/t?sslcert=c.pem").unwrap_err();
        assert!(e.to_string().contains("`sslcert` is not supported"), "{e}");
    }

    /// libpq's `require_auth`: methods all named or all negated, none twice;
    /// `none` lets the server ask for nothing, `!none` takes that away. The
    /// default, where the string names none: SCRAM alone and an exchange
    /// finished before `AuthenticationOk` for a client holding a password,
    /// anything for one holding none.
    #[test]
    fn require_auth_is_read_as_libpq_reads_it() {
        let auth = |q: &str| {
            Url::parse(&format!("postgres://alice@localhost/t?require_auth={q}"))
                .map(|u| u.require_auth.unwrap())
        };
        let scram = auth("scram-sha-256").unwrap();
        assert!(scram.allows(AUTH_SASL, false).is_ok());
        assert!(scram.allows(AUTH_PASSWORD, false).is_err());
        assert!(scram.allows(AUTH_OK, false).is_err(), "none not named");
        assert!(scram.allows(AUTH_OK, true).is_ok());
        let e = scram.allows(AUTH_PASSWORD, false).unwrap_err().to_string();
        assert!(
            e.contains("requirement \"scram-sha-256\" failed: the server asked for the password"),
            "{e}"
        );
        let both = auth("password,none").unwrap();
        assert!(both.allows(AUTH_PASSWORD, false).is_ok());
        assert!(both.allows(AUTH_OK, false).is_ok());
        assert!(both.allows(AUTH_SASL, false).is_err());
        let not = auth("!password").unwrap();
        assert!(not.allows(AUTH_PASSWORD, false).is_err());
        assert!(not.allows(AUTH_SASL, false).is_ok() && not.allows(AUTH_MD5, false).is_ok());
        assert!(
            not.allows(AUTH_OK, false).is_ok(),
            "a negated list keeps none"
        );
        let not_none = auth("!none").unwrap();
        assert!(not_none.allows(AUTH_OK, false).is_err());
        assert!(not_none.allows(AUTH_PASSWORD, false).is_ok());
        for (bad, says) in [
            ("password,!md5", "`md5`, cannot be mixed with methods named"),
            ("!password,md5", "`md5` cannot be mixed with negated"),
            ("password,password", "named more than once"),
            ("none,none", "named more than once"),
            ("!none,!none", "named more than once"),
            ("trust", "unknown method `trust`"),
        ] {
            let e = auth(bad).unwrap_err().to_string();
            assert!(e.contains(says), "{bad}: {e}");
        }
        let held = RequireAuth::default_for(true);
        assert!(held.allows(AUTH_PASSWORD, false).is_err());
        assert!(held.allows(AUTH_MD5, false).is_err());
        assert!(held.allows(AUTH_OK, false).is_err());
        assert!(held.allows(AUTH_SASL, false).is_ok() && held.allows(AUTH_OK, true).is_ok());
        let none = RequireAuth::default_for(false);
        assert!(none.allows(AUTH_OK, false).is_ok() && none.allows(AUTH_PASSWORD, false).is_ok());

        let u = Url::parse("postgres://alice@localhost/t").unwrap();
        assert_eq!(u.channel_binding, ChannelBinding::Prefer);
        assert_eq!(u.connect_timeout, CONNECT_WAIT);
        let u =
            Url::parse("postgres://alice@localhost/t?channel_binding=require&connect_timeout=1")
                .unwrap();
        assert_eq!(u.channel_binding, ChannelBinding::Require);
        assert_eq!(
            u.connect_timeout,
            Duration::from_secs(2),
            "libpq reads 1 as 2"
        );
    }

    /// A server's words are escaped and cut before anything shows them:
    /// a terminal's escape sequence, a line of its own, a reordering of the
    /// line.
    #[test]
    fn a_peers_text_is_escaped_and_cut() {
        assert_eq!(peer_text(b"plain words", SHOWN), "plain words");
        assert_eq!(
            peer_text(b"a\x1b[2Jb\nc\x7f\xc2\x9b", SHOWN),
            "a\\u{1b}[2Jb\\nc\\u{7f}\\u{9b}"
        );
        assert_eq!(
            peer_text("x\u{202e}y\u{2066}".as_bytes(), SHOWN),
            "x\\u{202e}y\\u{2066}"
        );
        assert_eq!(peer_text("ü".as_bytes(), SHOWN), "ü");
        let long = vec![b'a'; 1 << 20];
        let shown = peer_text(&long, 64);
        assert!(shown.starts_with(&"a".repeat(64)), "{shown}");
        assert!(shown.ends_with("... (1048576 bytes)"), "{shown}");
        assert!(shown.len() < 100);
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
        // The default: `verify-full` off this machine, libpq's `prefer` on
        // it; written, the string wins, here and there.
        if std::env::var_os("PGSSLMODE").is_none() {
            for (host, mode) in [
                ("db.example", SslMode::VerifyFull),
                ("10.0.0.5", SslMode::VerifyFull),
                ("[2001:db8::1]", SslMode::VerifyFull),
                ("127.0.0.2", SslMode::Prefer),
                ("localhost", SslMode::Prefer),
                ("LocalHost", SslMode::Prefer),
                ("[::1]", SslMode::Prefer),
                ("[::ffff:127.0.0.1]", SslMode::Prefer),
                ("", SslMode::Prefer),
            ] {
                let u = Url::parse(&format!("postgres://alice@{host}:5432/t")).unwrap();
                assert_eq!(
                    (u.sslmode, u.sslmode_from),
                    (mode, SslModeFrom::Default),
                    "{host}"
                );
            }
            let u = Url::parse("postgres://alice@db.example/t").unwrap();
            assert!(
                u.tls_said()
                    .contains("sslmode=verify-full (the default for a host off"),
                "{}",
                u.tls_said()
            );
            let u = Url::parse("postgres://alice@db.example/t?sslmode=prefer").unwrap();
            assert_eq!(
                (u.sslmode, u.sslmode_from),
                (SslMode::Prefer, SslModeFrom::Written)
            );
            let u = Url::parse("postgres://alice@localhost/t?sslmode=verify-full").unwrap();
            assert_eq!(u.sslmode, SslMode::VerifyFull);
            // sslrootcert=system alone makes verify-full, as libpq's.
            let u = Url::parse("postgres://alice@localhost/t?sslrootcert=system").unwrap();
            assert_eq!(
                (u.sslmode, u.sslmode_from),
                (SslMode::VerifyFull, SslModeFrom::SystemRoots)
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
        let m = Message { tag: b'E', body };
        let e = server_error(&m, true).to_string();
        assert!(e.contains("42P01"), "{e}");
        assert!(e.contains("relation \"nosuch\" does not exist"), "{e}");
        // From a server not proven, the code and this client's words.
        let e = server_error(&m, false).to_string();
        assert!(
            e.contains(": postgres 42P01: the table does not exist ("),
            "{e}"
        );
        assert!(!e.contains("nosuch") && e.contains("not shown"), "{e}");
    }

    /// An error's words reach a log and a terminal escaped and cut, and a
    /// code that is no SQLSTATE is not shown either.
    #[test]
    fn an_errors_words_are_escaped_and_its_code_held_to_its_shape() {
        let mut body = Vec::new();
        body.push(b'C');
        put_cstr(&mut body, "\x1b[2J!");
        body.push(b'M');
        put_cstr(&mut body, &format!("\x1b]0;owned\x07{}", "x".repeat(4000)));
        body.push(0);
        let e = server_error(&Message { tag: b'E', body }, true).to_string();
        assert!(e.contains(": postgres ?????: \\u{1b}]0;owned\\u{7}"), "{e}");
        assert!(!e.contains('\x1b') && e.len() < SHOWN + 64, "{e}");
    }

    /// The other checks of a server-first message, each before PBKDF2
    /// runs: a nonce that is the client's alone, with nothing of the
    /// server's after it -- the client's own sent back -- and an `m`
    /// extension, which RFC 5802 says to refuse.
    #[test]
    fn scram_refuses_a_nonce_of_its_own_and_an_extension() {
        let scram = Scram::new("", "abc");
        let salt = "s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        assert!(scram.last(&format!("r=abcd,{salt}"), "p").is_ok());
        let e = scram.last(&format!("r=abc,{salt}"), "p").err().unwrap();
        assert!(e.to_string().contains("invalid SCRAM"), "{e}");
        let e = scram
            .last(&format!("m=x,r=abcd,{salt}"), "p")
            .err()
            .unwrap();
        assert!(e.to_string().contains("`m` extension"), "{e}");
    }

    /// The GS2 header says what the client did with channel binding, and
    /// `c=` carries it, the binding's data after it: what the proof covers.
    #[test]
    fn the_gs2_header_says_how_the_client_binds() {
        let salt = "s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let c = |binding: Binding| {
            let scram = Scram::bound("", "abc", binding);
            let (last, _) = scram.last(&format!("r=abcd,{salt}"), "p").unwrap();
            (scram.first(), last.split(",r=").next().unwrap().to_string())
        };
        assert_eq!(c(Binding::None), ("n,,n=,r=abc".into(), "c=biws".into()));
        assert_eq!(
            c(Binding::Unoffered),
            ("y,,n=,r=abc".into(), "c=eSws".into())
        );
        let (first, cbind) = c(Binding::Plus(vec![1, 2, 3]));
        assert_eq!(first, "p=tls-server-end-point,,n=,r=abc");
        let mut want = b"p=tls-server-end-point,,".to_vec();
        want.extend_from_slice(&[1, 2, 3]);
        assert_eq!(cbind, format!("c={}", b64_encode(&want)));
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
        let refused: [&str; 6] = [
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G5=",
            "v=7rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=",
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4",
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4==",
            "",
            "e=invalid-proof",
        ];
        for got in refused {
            let e = server_final.verify(got).unwrap_err().to_string();
            assert!(e.contains("server signature"), "{got:?}: {e}");
        }
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

    /// The iteration count a server asks for is SCRAM_MIN_ITERATIONS to
    /// SCRAM_MAX_ITERATIONS, read as written: both bounds are run, and a
    /// hardened setting past a million, past the first bound of 2^20; one
    /// outside, or any count no `u32` holds, is refused naming the range and
    /// how to come within it, before PBKDF2 runs. Below RFC 7677's floor of
    /// 4 096 -- `i=1` was taken -- a proof is a few HMACs a guess.
    #[test]
    fn scram_iterations_are_held_to_their_range() {
        let taken: [(&str, u32); 5] = [
            ("4096", SCRAM_MIN_ITERATIONS),
            ("04096", 4096),
            ("2000000", 2_000_000),
            ("04194304", 1 << 22),
            ("4194304", SCRAM_MAX_ITERATIONS),
        ];
        for (v, n) in taken {
            assert_eq!(scram_iterations(v).unwrap(), n, "{v}");
        }
        let refused: [(&str, &str); 6] = [
            ("4194305", "not 4194305"),
            ("4294967295", "not 4294967295"),
            ("4294967296", "not 4294967296"),
            ("18446744073709551616", "not 18446744073709551616"),
            ("123456789012345678901", "not a count of 21 digits"),
            ("0", "not 0"),
        ];
        for (v, says) in refused {
            let e = scram_iterations(v).unwrap_err().to_string();
            let way = match v {
                "0" => {
                    "RFC 7677's floor: set the role's password again under a \
                        scram_iterations of 4096 or more"
                }
                _ => "set the role's password again under a lower scram_iterations",
            };
            assert!(
                e.contains("is 4096 to 4194304") && e.contains(says) && e.contains(way),
                "{v}: {e}"
            );
        }
        for v in ["1", "4095", "1000"] {
            let e = scram_iterations(v).unwrap_err().to_string();
            assert!(
                e.contains(&format!("is 4096 to 4194304, not {v}: RFC 7677's floor")),
                "{v}: {e}"
            );
        }
        for v in ["", "-1", "+5", "1e6", "4096 ", "0x10"] {
            let e = scram_iterations(v).unwrap_err().to_string();
            assert!(e.contains("could not be parsed"), "{v:?}: {e}");
        }
        // Refused as the server-first message is read, before its proof is
        // made: no PBKDF2 of four billion rounds runs here.
        let e = Scram::new("", "abc")
            .last("r=abcdef,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4294967295", "p")
            .err()
            .unwrap();
        assert!(e.to_string().contains("not 4294967295"), "{e}");
    }

    // ------------------------------------------------ a full send buffer

    /// A socket's send or receive buffer set to 64 KiB, which stops the
    /// system growing it, and the fill below quick. libc's `setsockopt`,
    /// declared here as the crates declare what they take of it.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    fn fixed_buffer(sock: &impl std::os::fd::AsRawFd, send: bool) {
        extern "C" {
            fn setsockopt(fd: i32, level: i32, name: i32, value: *const i32, len: u32) -> i32;
        }
        #[cfg(target_vendor = "apple")]
        let (level, sndbuf, rcvbuf) = (0xffff, 0x1001, 0x1002);
        #[cfg(target_os = "linux")]
        let (level, sndbuf, rcvbuf) = (1, 7, 8);
        let name = if send { sndbuf } else { rcvbuf };
        let size: i32 = 64 << 10;
        // SAFETY: an `int` the call reads, which outlives it.
        let r = unsafe { setsockopt(sock.as_raw_fd(), level, name, &size, 4) };
        assert_eq!(r, 0, "{}", std::io::Error::last_os_error());
    }

    /// A replication stream whose socket cannot send: the server, the
    /// socket returned beside it, reads nothing, and the client's send
    /// buffer and the server's receive buffer are full. A server decoding
    /// a long transaction reads nothing either; a follower fills nothing
    /// with its little feedback, but nothing else holds its writes then.
    /// Full for seconds, not for good: macOS makes room again some 5 s
    /// later (the server's buffer compacted, the next window probe told of
    /// it), and a blocked write goes through then.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    fn deaf() -> (WalStream, TcpStream) {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        // An accepted socket takes the listener's.
        fixed_buffer(&l, false);
        let mut c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        fixed_buffer(&c, true);
        let (server, _) = l.accept().unwrap();
        c.set_nonblocking(true).unwrap();
        // Until a round takes nothing: what one took drains into the
        // server's buffer until that is full as well, and room a delayed
        // acknowledgement makes comes up to 200 ms later -- after a pause of
        // 20 ms the 5 bytes of a `CopyDone` went through.
        loop {
            let mut took = 0;
            while let Ok(n) = c.write(&[0u8; 4096]) {
                took += n;
            }
            while let Ok(n) = c.write(&[0u8; 1]) {
                took += n;
            }
            if took == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        c.set_nonblocking(false).unwrap();
        c.set_read_timeout(Some(TIMEOUT)).unwrap();
        let client = Client {
            stream: BufReader::new(Conn::Plain(c)),
            server_version: String::new(),
            proven: true,
        };
        (WalStream { client }, server)
    }

    /// A COPY stream from a socket of the test's, its server half writing
    /// `frames` and then whatever `then` writes, `limit` the longest line.
    fn copy_of(frames: Vec<Vec<u8>>, limit: usize) -> CopyOut {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (mut server, _) = l.accept().unwrap();
        std::thread::spawn(move || {
            let mut w = Writer::new();
            for f in &frames {
                w.msg(b'd', |b| b.extend_from_slice(f));
            }
            let _ = w.flush_to(&mut server);
            // Held open until the client is done: a close is no answer.
            let _ = server.read(&mut [0u8; 1]);
        });
        c.set_read_timeout(Some(LOOK)).unwrap();
        CopyOut {
            client: Client {
                stream: BufReader::new(Conn::Plain(c)),
                server_version: String::new(),
                proven: true,
            },
            buf: Vec::new(),
            pos: 0,
            scanned: 0,
            done: false,
            limit,
            looked: 0,
        }
    }

    /// A line the server never ends -- 1-byte `CopyData` messages and no
    /// newline -- is refused once it passes the bound, in words naming it,
    /// where it was kept growing for good; and the bytes are looked at once
    /// each, where every message looked through the whole line again (here
    /// 64 KiB of messages would have been 2^31 comparisons). The bound in
    /// use is `MAX_MESSAGE`, PostgreSQL's of a row; this one is 64 KiB.
    #[test]
    fn a_copy_line_with_no_end_is_refused_at_its_bound_and_read_once() {
        let limit = 64 << 10;
        let mut copy = copy_of(vec![b"x".to_vec(); limit + 2], limit);
        let e = copy.next_line().unwrap_err().to_string();
        assert!(
            e.contains(&format!("a COPY line past {limit} bytes")),
            "{e}"
        );
        assert!(
            copy.looked <= limit as u64 + 2,
            "{} bytes looked at",
            copy.looked
        );

        // Lines split across messages come whole, each byte looked at once.
        let mut frames: Vec<Vec<u8>> = b"ab\ncd\nef".iter().map(|&b| vec![b]).collect();
        frames.push(b"g\nh\n".to_vec());
        let mut copy = copy_of(frames, limit);
        for want in ["ab", "cd", "efg", "h"] {
            assert_eq!(copy.next_line().unwrap().unwrap(), want.as_bytes());
        }
        assert_eq!(copy.looked, 12);
    }

    /// A stop wakes a COPY's read that waits on a server sending nothing:
    /// the follower copying a table for a server's mirror gives up within a
    /// [`LOOK`] or two of the shutdown, where its read waited on the
    /// socket, 30 s at a time and for good under a trickle.
    #[test]
    fn a_stop_wakes_a_copy_that_waits_on_the_server() {
        let mut copy = copy_of(vec![b"a\nb".to_vec()], 1 << 20);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        assert_eq!(copy.next_line_until(&stop).unwrap().unwrap(), b"a");
        let setter = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                stop.store(true, Ordering::SeqCst);
            })
        };
        let began = Instant::now();
        let e = copy.next_line_until(&stop).unwrap_err().to_string();
        setter.join().unwrap();
        assert!(e.contains("the copy was stopped"), "{e}");
        // Bounded from above where the time is the property: by what it
        // stands against, the 30 s of a read's timeout.
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "{:?}",
            began.elapsed()
        );
    }

    /// A server that reads nothing holds the end's `CopyDone` for as long
    /// as the buffers stay full: the end is held to its wait all the same,
    /// the `Terminate` after it too, where the drop's waited a whole wait
    /// more -- twice the bound.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn a_full_send_buffer_holds_the_end_no_longer_than_its_wait() {
        let (wal, _server) = deaf();
        let wait = Duration::from_secs(2);
        let began = Instant::now();
        let e = wal
            .end(wait, &AtomicBool::new(false))
            .unwrap_err()
            .to_string();
        let took = began.elapsed();
        assert!(
            e.contains("did not end the replication stream within"),
            "{e}"
        );
        // Held from above, the time being the property, by what it stands
        // against: the `Terminate` waiting a whole wait more, twice the
        // wait, 2 s to spare over the end's own. The message alone would
        // pass that.
        assert!(took >= wait && took < 2 * wait, "{took:?}");
    }

    /// A client dropped with its send buffer full returns at once, its
    /// `Terminate` unsent: the close says as much to a server once it
    /// reads. A failed follower's drop waited for room, with no bound,
    /// before `fennec-server` could store its exit code: for good on Linux,
    /// and here until macOS made room, seconds after the 2 s allowed.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn a_drop_never_waits_for_the_server() {
        let (wal, _server) = deaf();
        let (dropped, told) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(wal);
            let _ = dropped.send(());
        });
        assert!(
            told.recv_timeout(Duration::from_secs(2)).is_ok(),
            "the drop's Terminate waited for room the server never made"
        );
    }
}
