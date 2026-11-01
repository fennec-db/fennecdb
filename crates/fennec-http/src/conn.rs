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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "tls")]
use std::cell::{Cell, RefCell};
use std::sync::atomic::AtomicBool;

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
        (Some(cert), Some(key)) => {
            let tls = Tls::from_pem_files(cert.as_ref(), key.as_ref())
                .map_err(|e| format!("--tls-cert and --tls-key: {e}"))?;
            #[cfg(unix)]
            warn_if_open(key);
            Ok(Some(Arc::new(tls)))
        }
        #[cfg(not(feature = "tls"))]
        (Some(_), Some(_)) => Err(
            "--tls-cert and --tls-key: this build has no TLS (built without the `tls` feature)"
                .into(),
        ),
        _ => Err("--tls-cert and --tls-key go together: give both, or neither".into()),
    }
}

/// Says at the start where the key file is open to more than its owner,
/// naming the file and its mode, never a byte of it: whoever reads the key
/// can pose as the server. Warned, not refused as PostgreSQL refuses: a
/// Kubernetes secret volume mounts its files 0644 and Docker's secrets are
/// 0444 unless told otherwise, inside a container no other user shares, and
/// a server that would not start there would be fixed by a `chmod` in an
/// entrypoint, or by dropping `--tls-key` for a proxy's.
#[cfg(all(feature = "tls", unix))]
fn warn_if_open(key: &str) {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(key) else {
        return;
    };
    if let Some(who) = open_to(m.mode(), m.uid()) {
        crate::log!(
            "warning: --tls-key {key} is open to {who} (mode {:04o}): whoever reads it can pose \
             as this server. chmod 600 it, or 640 where root owns it and the server reads it \
             as its group",
            m.mode() & 0o7777
        );
    }
}

/// Who besides its owner a key file of `mode`, owned by `uid`, is open
/// to, by PostgreSQL's rule for its own key: no one, or its group where
/// root owns it and the group may only read it.
#[cfg(any(test, all(feature = "tls", unix)))]
fn open_to(mode: u32, uid: u32) -> Option<&'static str> {
    if mode & 0o007 != 0 {
        Some("every user")
    } else if mode & 0o070 != 0 && !(uid == 0 && mode & 0o070 == 0o040) {
        Some("its group")
    } else {
        None
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
///
/// Its end sends `close_notify` ([`Drop`]), so that a client reading a
/// stream to its end -- SSE, a replica's, a node's answer with no length
/// relayed -- or a connection closed after its answer reads an end the
/// server meant, where a TCP end alone reads as one an attacker could
/// have cut short. Not where a write failed, or the caller cut it
/// ([`Conn::cut`]): what went out may stop short, and a `close_notify`
/// after it would vouch for it. Never waiting: the socket is made
/// non-blocking first, and a `close_notify` the socket has no room for is
/// let go of -- a client not reading holds no thread at its end.
#[cfg(feature = "tls")]
pub struct Secured {
    tls: RefCell<fennec_tls::TlsStream>,
    sock: TcpStream,
    /// No `close_notify` at the end: a write failed or the caller cut the
    /// connection, or its end sent one already ([`Conn::end_refused`]).
    quiet: Cell<bool>,
}

#[cfg(feature = "tls")]
impl Secured {
    /// `r`, a write's or a flush's, noted: a failure leaves the connection
    /// quiet at its end.
    #[inline]
    fn noted<T>(&self, r: io::Result<T>) -> io::Result<T> {
        if r.is_err() {
            self.quiet.set(true);
        }
        r
    }
}

#[cfg(feature = "tls")]
impl Drop for Secured {
    fn drop(&mut self) {
        if !self.quiet.get() && self.sock.set_nonblocking(true).is_ok() {
            let _ = self.tls.get_mut().close();
        }
        open::forget(&self.sock);
    }
}

#[cfg(not(feature = "tls"))]
pub enum Secured {}

/// Set by [`end_all`]: the one shutdown is ending every TLS connection.
static ENDING: AtomicBool = AtomicBool::new(false);

/// How long [`end_all`] waits for the connections it ends to send their
/// `close_notify`: each sends its own, never waiting, once its thread
/// sees the shutdown.
pub const END_GRACE: Duration = Duration::from_millis(500);

/// Whether the one shutdown is ending the connections ([`end_all`]): a
/// stream looks as it wakes, and ends as it ends at its client's leaving.
#[inline]
pub fn ending() -> bool {
    ENDING.load(Ordering::Relaxed)
}

/// Ends every open TLS connection as the server goes down (the one
/// shutdown), each from its own thread, which owns its session: the read
/// side of each socket is shut, so a connection waiting for its next
/// request reads its end, and each stream's waker set by [`Conn::on_ending`]
/// is called, so a stream waiting for a change looks at [`ending`]. Each
/// thread's end then sends its `close_notify` ([`Secured`]'s `Drop`); this
/// waits for them up to `grace`. A connection in the middle of an answer
/// sends its end after it, if the process is still there. Before, the
/// process exiting closed them all with a TCP end alone, which a TLS client
/// reads as a stream cut short.
pub fn end_all(grace: Duration) {
    ENDING.store(true, Ordering::Relaxed);
    #[cfg(all(feature = "tls", unix))]
    {
        for wake in open::shut_reading() {
            wake();
        }
        let until = std::time::Instant::now() + grace;
        while !open::none() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[cfg(not(all(feature = "tls", unix)))]
    let _ = grace;
}

/// The open TLS connections, each by its socket's descriptor, for the one
/// shutdown to end ([`end_all`]). A descriptor is let go of here before it
/// closes, both under the one lock, so [`end_all`] never shuts one that a
/// new connection took after; one lock a TLS connection's start and end,
/// beside its handshake's milliseconds.
#[cfg(feature = "tls")]
mod open {
    use std::net::TcpStream;
    #[cfg(unix)]
    use std::sync::{Arc, Mutex};

    /// What wakes a stream waiting on something other than its socket.
    #[cfg(unix)]
    pub(super) type Waker = Arc<dyn Fn() + Send + Sync>;

    /// By descriptor, a map: thousands open, a list's search was each
    /// close's, under the lock every connection's start and end take.
    #[cfg(unix)]
    type Open = std::collections::BTreeMap<i32, Option<Waker>>;

    #[cfg(unix)]
    static OPEN: Mutex<Open> = Mutex::new(Open::new());

    #[cfg(unix)]
    fn open() -> std::sync::MutexGuard<'static, Open> {
        OPEN.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(unix)]
    pub(super) fn note(sock: &TcpStream) {
        use std::os::fd::AsRawFd;
        open().insert(sock.as_raw_fd(), None);
    }

    #[cfg(unix)]
    pub(super) fn wake_with(sock: &TcpStream, wake: Waker) {
        use std::os::fd::AsRawFd;
        if let Some(w) = open().get_mut(&sock.as_raw_fd()) {
            *w = Some(wake);
        }
    }

    #[cfg(unix)]
    pub(super) fn forget(sock: &TcpStream) {
        use std::os::fd::AsRawFd;
        open().remove(&sock.as_raw_fd());
    }

    /// The read side of every open connection shut, and their wakers.
    #[cfg(unix)]
    pub(super) fn shut_reading() -> Vec<Waker> {
        use std::os::fd::FromRawFd;
        let open = open();
        let mut wakers = Vec::new();
        for (fd, wake) in open.iter() {
            // SAFETY: the descriptor is open while it is listed, and is let
            // go of under this lock before it closes; `ManuallyDrop` keeps
            // it from being closed here.
            let s = std::mem::ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(*fd) });
            let _ = s.shutdown(std::net::Shutdown::Read);
            wakers.extend(wake.clone());
        }
        wakers
    }

    #[cfg(unix)]
    pub(super) fn none() -> bool {
        open().is_empty()
    }

    #[cfg(not(unix))]
    pub(super) fn note(_: &TcpStream) {}

    #[cfg(not(unix))]
    pub(super) fn forget(_: &TcpStream) {}
}

/// What a client did, looked at without waiting ([`Conn::peer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    /// Nothing arrived -- or, over TLS, records with no plaintext.
    Quiet,
    /// Bytes of a next request wait: the client is there.
    Sent,
    /// Its end -- over TLS its `close_notify` or its end -- with nothing
    /// before it: closed, or half-closed and still reading.
    Ended,
}

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
                failed(
                    Failed::PlainHttp,
                    &stream,
                    &"a plain HTTP request, answered 400",
                );
                plain_refused(stream);
                return None;
            }
            Ok(1) => {
                failed(
                    Failed::Protocol,
                    &stream,
                    &"its first byte is no TLS record's",
                );
                return None;
            }
            Ok(_) => {
                failed(Failed::Closed, &stream, &"it ended before its first byte");
                return None;
            }
            Err(e) => {
                failed(failure_of(&e), &stream, &e);
                return None;
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        // A `dup` refused -- no descriptor left -- is the process's
        // failure, not the client's handshake's: not counted among them.
        let sock = stream.try_clone().ok()?;
        let tls = match tls.server.accept_within(stream, left) {
            Ok(t) => t,
            Err(e) => {
                let why = match &e {
                    fennec_tls::Error::Io(io) => failure_of(io),
                    fennec_tls::Error::Alert(_) => Failed::Alert,
                    _ => Failed::Protocol,
                };
                failed(why, &sock, &e);
                return None;
            }
        };
        open::note(&sock);
        Some(Conn::Tls(Box::new(Secured {
            tls: RefCell::new(tls),
            sock,
            quiet: Cell::new(false),
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

    /// What the client did, looked at without waiting: the router's watch
    /// of a held request's client once its socket polls readable
    /// (`fennec_shard::upstream`). Over TLS what arrived is read as TLS, so
    /// a `close_notify` is the end it is -- a peek at the socket took its
    /// record for bytes of a next request, and the router stopped watching
    /// a client that had ended. An error is a reset, or a record the
    /// session refused: nobody is left to answer.
    pub fn peer(&self) -> io::Result<Peer> {
        match self {
            Conn::Plain(s) => {
                s.set_nonblocking(true)?;
                let seen = s.peek(&mut [0u8; 1]);
                s.set_nonblocking(false)?;
                match seen {
                    Ok(0) => Ok(Peer::Ended),
                    Ok(_) => Ok(Peer::Sent),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(Peer::Quiet),
                    Err(e) => Err(e),
                }
            }
            #[cfg(feature = "tls")]
            Conn::Tls(t) => Ok(match t.tls.borrow_mut().peer()? {
                fennec_tls::Peer::Quiet => Peer::Quiet,
                fennec_tls::Peer::Sent => Peer::Sent,
                fennec_tls::Peer::Ended => Peer::Ended,
            }),
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
            Conn::Tls(t) => t.noted((|| {
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
            })()),
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
                let sock = t.sock.try_clone();
                let closed = t.tls.borrow_mut().close().is_ok();
                t.quiet.set(true);
                drop(t);
                if let (true, Ok(sock)) = (closed, sock) {
                    crate::accept::end_refused(sock);
                }
            }
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match t {},
        }
    }

    /// Ends what this connection sends with no `close_notify`: what went
    /// out stops short of what was meant -- the router relaying a stream
    /// its node broke off -- and the client is to read it so. Plain, a
    /// TCP end is all there is.
    pub fn cut(&self) {
        #[cfg(feature = "tls")]
        if let Conn::Tls(t) = self {
            t.quiet.set(true);
        }
    }

    /// `wake` called at the one shutdown ([`end_all`]): a stream waiting on
    /// something other than its socket -- a hub's changes, a feed's writes
    /// -- is woken to look at [`ending`], and ends, its `close_notify`
    /// sent. A plain connection is let be: no end of TLS to send.
    pub fn on_ending(&self, wake: impl Fn() + Send + Sync + 'static) {
        #[cfg(all(feature = "tls", unix))]
        if let Conn::Tls(t) = self {
            open::wake_with(&t.sock, Arc::new(wake));
        }
        #[cfg(not(all(feature = "tls", unix)))]
        let _ = wake;
    }
}

/// Why a connection to an HTTPS listener ended before its handshake did,
/// as `reason` labels it in `/_metrics` (`fennec_tls_handshakes_failed_total`,
/// `fennec_router_tls_handshakes_failed_total`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failed {
    /// No handshake within [`Tls::handshake`]: a client trickling or
    /// stalled.
    Timeout = 0,
    /// The client ended or reset it first: a TCP probe (a load balancer's
    /// check, a scan) among them.
    Closed = 1,
    /// The client ended it with an alert: most often its refusal of the
    /// certificate (`unknown_ca`, `bad_certificate`).
    Alert = 2,
    /// Bytes that are not TLS, a record out of the protocol, or nothing in
    /// common -- versions, cipher suites, ALPN (a client offering `h2`
    /// alone).
    Protocol = 3,
    /// A plain HTTP request, answered 400 ([`PLAIN_TO_TLS`]).
    PlainHttp = 4,
}

/// [`Failed`] as `reason` labels it, in its order.
pub const FAILURES: [(Failed, &str); 5] = [
    (Failed::Timeout, "timeout"),
    (Failed::Closed, "closed"),
    (Failed::Alert, "alert"),
    (Failed::Protocol, "protocol"),
    (Failed::PlainHttp, "plain_http"),
];

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);

/// Handshakes failed, by [`Failed`]: the process's, a node's and the
/// router's alike, as [`crate::accept`]'s refusals are. One counter for
/// every connection's thread, not a shard each as a request's counts are:
/// a failure is a connection's once, after a handshake's milliseconds.
static FAILED: [AtomicU64; 5] = [ZERO; 5];

/// When a failed handshake was last said in the log, a moment of the
/// monotonic clock.
#[cfg(feature = "tls")]
static SAID: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// How many handshakes failed for `why`.
pub fn handshakes_failed(why: Failed) -> u64 {
    FAILED[why as usize].load(Ordering::Relaxed)
}

/// What `e`, a socket's error before the handshake ended, says of it: a
/// timeout, or the client gone.
#[cfg(feature = "tls")]
fn failure_of(e: &io::Error) -> Failed {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Failed::Timeout,
        _ => Failed::Closed,
    }
}

/// Counts a handshake that failed, and says it in the log a minute apart
/// at most: a flood of them -- a scan, a client with the wrong CA -- is a
/// line a minute, not a line each, `/_metrics` counting each by reason. The
/// line names the client's address and what failed, never a byte of the
/// key or of what the client sent; no request is begun, so it carries no
/// request id.
#[cfg(feature = "tls")]
#[cold]
#[inline(never)]
fn failed(why: Failed, sock: &TcpStream, what: &dyn std::fmt::Display) {
    FAILED[why as usize].fetch_add(1, Ordering::Relaxed);
    let due = {
        let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        let due = said.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(60));
        if due {
            *said = Some(now);
        }
        due
    };
    if due {
        let reason = FAILURES[why as usize].1;
        let from = sock
            .peer_addr()
            .map_or_else(|_| "?".into(), |a| a.to_string());
        crate::log!(
            "a TLS handshake from {from} failed ({reason}): {what} (said a minute apart at \
             most; /_metrics counts each by reason)"
        );
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
            Conn::Tls(t) => t.noted(t.tls.borrow_mut().write(buf)),
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
            Conn::Tls(t) => t.noted(t.tls.borrow_mut().write_vectored(bufs)),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        match *self {
            Conn::Plain(_) => Ok(()),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.noted(t.tls.borrow_mut().flush()),
            #[cfg(not(feature = "tls"))]
            Conn::Tls(t) => match *t {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::open_to;

    /// A key open to more than its owner is told, by PostgreSQL's rule:
    /// its owner's alone, or its group's to read where root owns it.
    #[test]
    fn a_key_open_beyond_its_owner_is_told() {
        assert_eq!(open_to(0o100600, 1000), None);
        assert_eq!(open_to(0o100400, 1000), None);
        assert_eq!(open_to(0o100640, 0), None);
        assert_eq!(open_to(0o100640, 1000), Some("its group"));
        assert_eq!(open_to(0o100660, 0), Some("its group"));
        assert_eq!(open_to(0o100644, 0), Some("every user"));
        assert_eq!(open_to(0o100444, 1000), Some("every user"));
    }
}
