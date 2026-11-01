//! A served connection's socket, plain or TLS: the server's and the
//! router's, read through a `BufReader` and written by the answers, both
//! borrowing the one [`Conn`] -- docs/design/http.md#tls.
//!
//! With `--tls-cert` and `--tls-key` a listener speaks HTTPS alone. The
//! handshake runs on the connection's own thread, never on the accepting
//! one, bounded as a whole ([`Tls::handshake`]); a connection no thread is
//! made for is told [`fennec_tls::REFUSAL`], an alert in the clear, where a
//! plain one is answered 503 ([`crate::accept`]). Without the flags a
//! connection is [`Conn::Plain`], and with the `tls` feature off the TLS
//! variant is a type of no value: a branch the compiler drops, and a plain
//! connection the same `TcpStream` it was, read and written through it,
//! with one `dup` fewer than the reader and writer apart took.

use std::io::{self, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "tls")]
use std::cell::RefCell;

/// How long a client has for its handshake, from its connection's thread
/// starting to its handshake done: a client that never finishes one holds
/// a thread for no longer.
pub const HANDSHAKE: Duration = Duration::from_secs(10);

/// What a plain HTTP request to an HTTPS listener is answered, as a 400 in
/// the clear: it names the scheme to use, where a TLS alert's bytes read
/// as a garbled answer. `fennec-server --ping` asks again over TLS on it.
pub const PLAIN_TO_TLS: &str =
    "this port speaks HTTPS alone (--tls-cert): ask it at https:// rather than http://";

/// A listener's certificate chain and key (`--tls-cert`, `--tls-key`), and
/// the bound on a handshake.
#[cfg(feature = "tls")]
pub struct Tls {
    server: fennec_tls::Server,
    /// [`HANDSHAKE`] unless a test asks for less.
    pub handshake: Duration,
}

/// A build without the `tls` feature has no TLS: no value of it.
#[cfg(not(feature = "tls"))]
pub enum Tls {}

impl Tls {
    /// The certificate chain and key in two PEM files, refused -- naming
    /// the file -- where either cannot be read or parsed or the key is not
    /// the certificate's. ALPN offers `http/1.1` alone: a client offering
    /// only `h2` is refused in the handshake rather than spoken to in a
    /// protocol it did not ask for.
    #[cfg(feature = "tls")]
    pub fn from_pem_files(cert: &Path, key: &Path) -> Result<Tls, String> {
        let server = fennec_tls::Server::from_pem_files(cert, key)
            .map_err(|e| e.to_string())?
            .alpn(&[b"http/1.1"]);
        Ok(Tls {
            server,
            handshake: HANDSHAKE,
        })
    }

    #[cfg(not(feature = "tls"))]
    pub fn from_pem_files(_: &Path, _: &Path) -> Result<Tls, String> {
        Err("this build has no TLS: it was built without the `tls` feature".into())
    }

    /// The bound on a handshake, for a test to shorten.
    #[doc(hidden)]
    pub fn handshake_within(self, within: Duration) -> Tls {
        #[cfg(feature = "tls")]
        {
            Tls {
                handshake: within,
                ..self
            }
        }
        #[cfg(not(feature = "tls"))]
        {
            let _ = within;
            match self {}
        }
    }
}

/// `--tls-cert` and `--tls-key` as a listener's TLS, for fennec-server and
/// fennec-shard alike: both or neither, each read and the key the
/// certificate's, else the error the start is refused with -- a server
/// told to speak HTTPS never falls back to plain HTTP.
pub fn from_flags(cert: Option<&str>, key: Option<&str>) -> Result<Option<Arc<Tls>>, String> {
    match (cert, key) {
        (None, None) => Ok(None),
        #[cfg(feature = "tls")]
        (Some(cert), Some(key)) => Tls::from_pem_files(cert.as_ref(), key.as_ref())
            .map(|t| Some(Arc::new(t)))
            .map_err(|e| format!("--tls-cert and --tls-key: {e}")),
        #[cfg(not(feature = "tls"))]
        (Some(_), Some(_)) => Err(
            "--tls-cert and --tls-key: this build has no TLS (built without the `tls` feature)"
                .into(),
        ),
        _ => Err("--tls-cert and --tls-key go together: give both, or neither".into()),
    }
}

/// The scheme a listener's `listening on:` line names: harnesses find a
/// server by that line (CLAUDE.md's ports), and a client by its scheme.
pub fn scheme(tls: Option<&Tls>) -> &'static str {
    match tls {
        Some(_) => "https",
        None => "http",
    }
}

/// A connection's TLS session, and a `dup` of its socket for what is asked
/// of the socket while the session is borrowed: its options, its address,
/// and the router's `poll` of a held request's client.
#[cfg(feature = "tls")]
pub struct Secured {
    tls: RefCell<fennec_tls::TlsStream>,
    sock: TcpStream,
}

#[cfg(not(feature = "tls"))]
pub enum Secured {}

/// A served connection. Read and written through `&Conn`, as std reads and
/// writes a `&TcpStream`, so a request's reader and its answers' writer
/// borrow one value.
pub enum Conn {
    Plain(TcpStream),
    /// Boxed: a TLS session is a kilobyte, a plain connection a descriptor.
    #[cfg(feature = "tls")]
    Tls(Box<Secured>),
    #[cfg(not(feature = "tls"))]
    Tls(Secured),
}

impl Conn {
    /// `stream` as a connection: itself where `tls` is `None`, else once
    /// its handshake is done within [`Tls::handshake`], on the calling
    /// thread -- the connection's own. `None` where it failed, timed out,
    /// or was not TLS: a plain HTTP request is answered 400
    /// ([`PLAIN_TO_TLS`]) in the clear and its connection ended as a
    /// refused one is, other bytes closed on.
    #[inline]
    pub fn open(stream: TcpStream, tls: Option<&Tls>) -> Option<Conn> {
        match tls {
            None => Some(Conn::Plain(stream)),
            Some(tls) => Conn::secured(stream, tls),
        }
    }

    #[cfg(not(feature = "tls"))]
    fn secured(_: TcpStream, tls: &Tls) -> Option<Conn> {
        match *tls {}
    }

    #[cfg(feature = "tls")]
    #[cold]
    #[inline(never)]
    fn secured(stream: TcpStream, tls: &Tls) -> Option<Conn> {
        use std::time::Instant;
        let deadline = Instant::now() + tls.handshake;
        // The first byte, looked at before rustls reads it: a handshake's
        // record is 0x16, and a plain request's method a capital letter,
        // which rustls would answer with an alert its client prints as a
        // garbled answer.
        let mut first = [0u8; 1];
        let looked = stream
            .set_read_timeout(Some(tls.handshake.max(Duration::from_millis(1))))
            .and_then(|()| stream.peek(&mut first));
        match looked {
            Ok(1) if first[0] == 0x16 => {}
            Ok(1) if first[0].is_ascii_uppercase() => {
                plain_refused(stream);
                return None;
            }
            _ => return None,
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let sock = stream.try_clone().ok()?;
        let tls = tls.server.accept_within(stream, left).ok()?;
        Some(Conn::Tls(Box::new(Secured {
            tls: RefCell::new(tls),
            sock,
        })))
    }

    /// The socket beneath: for a `poll`, an option or an address, never
    /// for reading or writing past a TLS session.
    #[inline]
    pub fn socket(&self) -> &TcpStream {
        match self {
            Conn::Plain(s) => s,
            #[cfg(feature = "tls")]
            Conn::Tls(t) => &t.sock,
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }

    pub fn is_tls(&self) -> bool {
        matches!(self, Conn::Tls(_))
    }

    #[inline]
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.socket().set_read_timeout(d)
    }

    #[inline]
    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.socket().set_write_timeout(d)
    }

    #[inline]
    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        self.socket().set_nonblocking(on)
    }

    #[inline]
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.socket().peer_addr()
    }

    #[inline]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket().local_addr()
    }

    /// Whether the client is gone while its request is held, looked at
    /// without waiting: its end arrived with nothing before it. A plain
    /// socket is peeked; a TLS one takes in what arrived, so a
    /// `close_notify` -- bytes, to a peek -- reads as the end it is. A
    /// reset reads as not gone, as before: the answer's write finds it.
    pub fn gone(&self) -> bool {
        match self {
            Conn::Plain(s) => {
                if s.set_nonblocking(true).is_err() {
                    return false;
                }
                let closed = matches!(s.peek(&mut [0u8; 1]), Ok(0));
                let _ = s.set_nonblocking(false);
                closed
            }
            #[cfg(feature = "tls")]
            Conn::Tls(t) => matches!(t.tls.borrow_mut().peer_gone(), Ok(true)),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }

    /// Writes `buf` on a non-blocking socket, and where the socket would
    /// block, `hold()` is taken -- a stream marking itself held, which no
    /// durable write waits for -- and the write finished blocking. Over
    /// TLS the plaintext is taken into records at once and only their
    /// sending can block, so it is that which is finished under the hold.
    pub fn write_or_hold<G>(&self, buf: &[u8], hold: impl FnOnce() -> G) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => match (&*s).write(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let _held = hold();
                    s.set_nonblocking(false)?;
                    let r = (&*s).write(buf);
                    s.set_nonblocking(true)?;
                    r
                }
                r => r,
            },
            #[cfg(feature = "tls")]
            Conn::Tls(t) => {
                let mut tls = t.tls.borrow_mut();
                let n = tls.write(buf)?;
                if tls.has_unsent() {
                    let _held = hold();
                    t.sock.set_nonblocking(false)?;
                    let r = tls.flush();
                    t.sock.set_nonblocking(true)?;
                    r?;
                }
                Ok(n)
            }
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }

    /// Ends a connection whose request was refused before it was read
    /// whole, once its answer went out: as [`crate::accept::end_refused`]
    /// ends a plain one, a TLS one after its `close_notify`.
    pub fn end_refused(self) {
        match self {
            Conn::Plain(s) => crate::accept::end_refused(s),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => {
                let Secured { tls, sock } = *t;
                let mut tls = tls.into_inner();
                if tls.close().is_ok() {
                    drop(tls.into_socket());
                    crate::accept::end_refused(sock);
                }
            }
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match t {},
        }
    }
}

/// A plain HTTP request to an HTTPS listener: answered 400 in the clear,
/// on the connection's own thread, and ended as a refused connection is,
/// so its client reads the answer rather than a reset.
#[cfg(feature = "tls")]
fn plain_refused(mut stream: TcpStream) {
    crate::request_id::begin(None);
    let answered = crate::http::Response::error(400, PLAIN_TO_TLS)
        .write(&mut stream, false, false)
        .is_ok();
    crate::request_id::end();
    if answered {
        crate::accept::end_refused(stream);
    }
}

impl Read for &Conn {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match *self {
            Conn::Plain(s) => (&*s).read(buf),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.tls.borrow_mut().read(buf),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }
}

impl Write for &Conn {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match *self {
            Conn::Plain(s) => (&*s).write(buf),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.tls.borrow_mut().write(buf),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }

    /// Plain, one `writev`; over TLS the slices are one plaintext, a small
    /// answer one record, and the records one `writev`.
    #[inline]
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match *self {
            Conn::Plain(s) => (&*s).write_vectored(bufs),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.tls.borrow_mut().write_vectored(bufs),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        match *self {
            Conn::Plain(_) => Ok(()),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.tls.borrow_mut().flush(),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }
}
