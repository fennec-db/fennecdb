//! A name looked up, and a connection made, within a bound the caller sets
//! -- and walked away from past it.
//!
//! `getaddrinfo` blocks for as long as the system's resolver takes, and no
//! timeout reaches it: std's `to_socket_addrs` is that call, and
//! `TcpStream::connect_timeout` bounds the connect alone. A name whose
//! resolver is slow to refuse it held each caller for as long as the
//! resolver took -- musl's 5 s, glibc's 5 s a try and a server -- and the
//! slow refusal is what a container's name meets once the container is
//! gone: Docker's resolver no longer knows it and forwards it upstream,
//! which may answer late or never. A router's lease round waited on every
//! grant, so the grant to such a node held up every other node's renewal:
//! in V5's drill on GitHub's runner the region's healthy nodes went 18 s
//! without a write and were failed over themselves, and the dead node's
//! tenants wrote again 30 s after it went.
//!
//! So a name is looked up on a thread of its own ([`resolve`]), waited for
//! no longer than the caller's bound: past it the caller has a timeout, and
//! the lookup runs to its end with nobody waiting, its answer dropped. A
//! caller asking a name whose lookup began less than [`JOIN_WITHIN`] ago,
//! or that has [`MAX_IN_FLIGHT`] lookups out already, waits on the newest
//! of them rather than start another: a dead node's name asked by every
//! request to its tenants would otherwise leave a thread a request
//! lingering for the resolver's 5 s, and a name that answers in a
//! millisecond is asked once for a burst of connects. One that comes back
//! after hanging is asked afresh within [`JOIN_WITHIN`] of the newest
//! lookup still out, not at that lookup's end.
//!
//! An address written out (`10.0.0.5:8081`, `[::1]:8081`) is read where it
//! stands, with no thread and no lookup, as `to_socket_addrs` reads one: a
//! router whose nodes are recorded by address pays nothing for any of this.
//! Nothing is cached: the addresses a lookup gave go to the connects that
//! asked, and the next connect asks again, as before. Kept past their
//! lookup, a node's old address sends a router's grant to whatever machine
//! took it after the node left -- Docker hands a stopped container's
//! address to the next one it starts.
//!
//! [`connect_unless`] is a connect a caller can walk away from at a word:
//! the whole dial, lookup and connect, on a thread of its own, the caller
//! looking at its stop flag while it waits. A replica's follower dials its
//! primary so: halted -- a promotion halts it before the fork -- in the
//! middle of reconnecting to a dead primary, it waited out that connect's
//! 5 s, and the failover with it. A follower reconnecting holds a dial
//! thread beside its own, and one walked away from runs on to its bound
//! (the lookup's 5 s, then the connect's): a `--dir` standby whose N
//! tenants follow a primary gone holds N of them at most, each for that
//! long, and the follower waits out its backoff before the next.
//!
//! An address dialed over TLS says so: `https://host:port` where a plain
//! one is `host:port` ([`tls_of`]) -- what a router keeps for a node, a
//! replica for its primary, the exporter for its collector -- so every
//! dial of fennecdb's HTTP takes HTTPS by its URL alone
//! (docs/design/http.md#dialing-https). Its handshake is the dial's last
//! step, bounded as a whole ([`secure`]), so a certificate refused is
//! refused before a byte of a request goes out, and a [`Stream`] is
//! either. The CAs it is held to are the process's ([`trust_ca`],
//! `--tls-ca`): Mozilla's roots unless a file names others, which replace
//! them. A plain dial pays a prefix compare a connect and a branch a read
//! or write.

use std::io::{self, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// What an address dialed over TLS begins with ([`tls_of`]).
pub const HTTPS: &str = "https://";

/// `addr` as a dial takes it: whether over TLS, and the `host:port`
/// dialed. `https://host:port` is TLS; `host:port`, or `http://host:port`,
/// plain HTTP.
#[inline]
pub fn tls_of(addr: &str) -> (bool, &str) {
    match addr.strip_prefix(HTTPS) {
        Some(a) => (true, a),
        None => (false, addr.strip_prefix("http://").unwrap_or(addr)),
    }
}

/// The `host:port` an address dials, for a `Host` header: its scheme
/// left out.
#[inline]
pub fn authority(addr: &str) -> &str {
    tls_of(addr).1
}

/// The URL an address stands for: `https://host:port` as it is, and
/// `http://` before a plain one -- what a node is told to follow, a
/// router's directory records of a copy.
pub fn url_of(addr: &str) -> String {
    match tls_of(addr) {
        (true, _) => addr.to_string(),
        (false, a) => format!("http://{a}"),
    }
}

/// An `http://` or `https://` URL as a dial takes it: its address --
/// `host:port`, or `https://host:port` over TLS, a port left out being 80
/// or 443 -- and its path, `""` for none, no `/` at its end. Another
/// scheme, no host, or `https://` in a build without TLS is an error.
pub fn split_url(url: &str) -> Result<(String, String), String> {
    let (tls, rest) = if let Some(r) = url.strip_prefix(HTTPS) {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("`{url}` is no http:// or https:// URL"));
    };
    if tls && !cfg!(feature = "tls") {
        return Err(format!(
            "`{url}` is dialed over TLS, and this build has none (built without the `tls` \
             feature)"
        ));
    }
    let rest = rest.trim_end_matches('/');
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() || authority.starts_with(':') {
        return Err(format!("`{url}` names no host"));
    }
    let ported = matches!(
        authority.rsplit_once(':'),
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && !h.ends_with(':')
    );
    let port = match (ported, tls) {
        (true, _) => "",
        (false, true) => ":443",
        (false, false) => ":80",
    };
    let scheme = if tls { HTTPS } else { "" };
    Ok((format!("{scheme}{authority}{port}"), path.to_string()))
}

/// The name a certificate is held to: the authority's host, its port left
/// out (brackets kept: `fennec-tls` reads `[::1]` as the address).
#[cfg_attr(not(feature = "tls"), allow(dead_code))]
fn host_of(authority: &str) -> &str {
    match authority.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !h.ends_with(':') => h,
        _ => authority,
    }
}

/// The CAs an `https://` dial of this process trusts: `None` until asked,
/// then Mozilla's roots ([`fennec_tls::Client::new`]) or `--tls-ca`'s, and
/// whether a file named them -- a refusal under Mozilla's roots says which
/// flag names a private CA. One client for every dial, so their sessions
/// are resumed across connections to one server.
#[cfg(feature = "tls")]
static TRUST: Mutex<Option<(fennec_tls::Client, bool)>> = Mutex::new(None);

#[cfg(feature = "tls")]
fn trust() -> (fennec_tls::Client, bool) {
    lock(&TRUST)
        .get_or_insert_with(|| (fennec_tls::Client::new().alpn(&[b"http/1.1"]), false))
        .clone()
}

/// `--tls-ca <pem>`: every `https://` dial of this process trusts the CAs
/// of `pem` and no others -- Mozilla's roots replaced, not added to, so a
/// private CA's servers are reached and nothing a public CA signs for an
/// address of the network passes for them. A file unread, or holding no
/// certificate, is refused naming it. Asked again it replaces the CAs, for
/// the dials made after.
pub fn trust_ca(pem: &str) -> Result<(), String> {
    #[cfg(feature = "tls")]
    {
        let client = fennec_tls::Client::trusting_pem_file(std::path::Path::new(pem))
            .map_err(|e| format!("--tls-ca: {e}"))?
            .alpn(&[b"http/1.1"]);
        *lock(&TRUST) = Some((client, true));
        Ok(())
    }
    #[cfg(not(feature = "tls"))]
    {
        let _ = pem;
        Err("--tls-ca: this build has no TLS (built without the `tls` feature)".into())
    }
}

/// TLS over `sock`, a connection just made to `authority`, its handshake
/// done within `within` however the server trickles it
/// ([`fennec_tls::Client::dial_within`]): a certificate refused -- an
/// unknown issuer, another name -- is an `InvalidData` error with nothing
/// of the caller's sent, and a handshake past its bound `TimedOut`. The
/// socket is left with no timeout set.
#[cfg(feature = "tls")]
pub fn secure(sock: TcpStream, authority: &str, within: Duration) -> io::Result<Stream> {
    let (client, named) = trust();
    match client.dial_within(sock, host_of(authority), within) {
        Ok(s) => Ok(Stream::Tls(Box::new(s))),
        Err(fennec_tls::Error::Certificate(m)) if !named => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("certificate refused: {m} (Mozilla's roots; --tls-ca names a private CA)"),
        )),
        Err(e) => Err(e.into()),
    }
}

/// No TLS in this build: an `https://` address is refused, unsent.
#[cfg(not(feature = "tls"))]
pub fn secure(sock: TcpStream, authority: &str, _: Duration) -> io::Result<Stream> {
    drop(sock);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{authority} is dialed over TLS, and this build has none"),
    ))
}

/// A connection a dial made: the socket itself, or a TLS session over it
/// whose handshake is done. Boxed: a session is a kilobyte, a plain
/// connection a descriptor. Without the `tls` feature the TLS variant is a
/// type of no value, a branch the compiler drops.
pub enum Stream {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Tls(Box<fennec_tls::TlsStream>),
    #[cfg(not(feature = "tls"))]
    Tls(NoTls),
}

/// No TLS session in a build without the `tls` feature.
#[cfg(not(feature = "tls"))]
pub enum NoTls {}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("socket", self.socket())
            .field("tls", &self.is_tls())
            .finish()
    }
}

impl Stream {
    /// The socket beneath: for an option, an address, a `poll` or a
    /// shutdown -- one that wakes a thread reading through the session too
    /// -- never for reading or writing past a TLS session.
    #[inline]
    pub fn socket(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            #[cfg(feature = "tls")]
            Stream::Tls(t) => t.get_ref(),
            #[cfg(not(feature = "tls"))]
            Stream::Tls(t) => match *t {},
        }
    }

    #[inline]
    pub fn is_tls(&self) -> bool {
        !matches!(self, Stream::Plain(_))
    }

    #[inline]
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.socket().set_read_timeout(d)
    }

    #[inline]
    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.socket().set_write_timeout(d)
    }

    /// What the server did, looked at without waiting: a plain socket
    /// peeked, a TLS one's arrivals read as TLS -- a session ticket or a
    /// key update is no answer ([`crate::conn::Peer::Quiet`]), and a
    /// `close_notify` the end it is, where a peek took either for bytes of
    /// one. An error is a reset, or a record the session refused.
    pub fn peer(&mut self) -> io::Result<crate::conn::Peer> {
        use crate::conn::Peer;
        match self {
            Stream::Plain(s) => {
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
            Stream::Tls(t) => Ok(match t.peer()? {
                fennec_tls::Peer::Quiet => Peer::Quiet,
                fennec_tls::Peer::Sent => Peer::Sent,
                fennec_tls::Peer::Ended => Peer::Ended,
            }),
            #[cfg(not(feature = "tls"))]
            Stream::Tls(t) => match *t {},
        }
    }

    /// The end of what this side writes, the server reading on: over TLS
    /// a `close_notify` first, then the socket shut for writing, so a
    /// server watching its client through the session reads the end it is.
    pub fn end_writing(&mut self) -> io::Result<()> {
        #[cfg(feature = "tls")]
        if let Stream::Tls(t) = self {
            t.close()?;
        }
        self.socket().shutdown(std::net::Shutdown::Write)
    }
}

impl Read for Stream {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            #[cfg(feature = "tls")]
            Stream::Tls(t) => t.read(buf),
            #[cfg(not(feature = "tls"))]
            Stream::Tls(t) => match *t {},
        }
    }
}

impl Write for Stream {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            #[cfg(feature = "tls")]
            Stream::Tls(t) => t.write(buf),
            #[cfg(not(feature = "tls"))]
            Stream::Tls(t) => match *t {},
        }
    }

    /// Plain, one `writev`; over TLS the slices are one plaintext -- a
    /// request's head and body one record under 16 KiB -- and its records
    /// one `writev`, a failure sending them [`Write::flush`]'s to tell.
    #[inline]
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write_vectored(bufs),
            #[cfg(feature = "tls")]
            Stream::Tls(t) => t.write_vectored(bufs),
            #[cfg(not(feature = "tls"))]
            Stream::Tls(t) => match *t {},
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(_) => Ok(()),
            #[cfg(feature = "tls")]
            Stream::Tls(t) => t.flush(),
            #[cfg(not(feature = "tls"))]
            Stream::Tls(t) => match *t {},
        }
    }
}

/// A lookup of a name begun less than this long ago is waited on by the
/// next caller asking it, not begun again: a burst of connects asks once.
pub const JOIN_WITHIN: Duration = Duration::from_millis(500);

/// The most lookups of one name out at once: past it a caller waits on the
/// newest. A resolver that never answers costs this many threads a name,
/// each until the system's resolver gives up on it.
pub const MAX_IN_FLIGHT: usize = 8;

/// How often [`connect_unless`] looks at its stop flag while it waits.
const STOP_LOOK: Duration = Duration::from_millis(10);

/// What a lookup came to: `io::Error` is no `Clone`, and each caller
/// waiting on the lookup takes one of its own.
type Found = Result<Vec<SocketAddr>, (io::ErrorKind, String)>;

struct Lookup {
    addr: String,
    began: Instant,
    found: Mutex<Option<Found>>,
    came: Condvar,
}

/// The lookups out now, by name: a handful at most, each there only while
/// its thread runs.
static OUT: Mutex<Vec<Arc<Lookup>>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The addresses `addr` (`host:port`) stands for, looked up within
/// `within`: a timeout past it, whatever the resolver is doing (the
/// module's comment).
pub fn resolve(addr: &str, within: Duration) -> io::Result<Vec<SocketAddr>> {
    if let Ok(a) = addr.parse::<SocketAddr>() {
        return Ok(vec![a]);
    }
    let timed_out = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("`{addr}` did not resolve within {within:?}"),
        )
    };
    // No time at all: no lookup begun for nobody to wait on.
    if within.is_zero() {
        return Err(timed_out());
    }
    let now = Instant::now();
    // A bound past what an `Instant` holds is no bound.
    let deadline = now.checked_add(within);
    let lookup = joined(addr, now)?;
    let mut found = lock(&lookup.found);
    loop {
        if let Some(f) = &*found {
            return f.clone().map_err(|(kind, why)| io::Error::new(kind, why));
        }
        found = match deadline {
            None => lookup.came.wait(found).unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(timed_out());
                }
                lookup
                    .came
                    .wait_timeout(found, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
        };
    }
}

/// The lookup of `addr` a caller asking `at` waits on: the newest one out
/// where it began less than [`JOIN_WITHIN`] before, or where as many are
/// out as may be; a new one otherwise.
fn joined(addr: &str, at: Instant) -> io::Result<Arc<Lookup>> {
    let mut out = lock(&OUT);
    let same = out.iter().filter(|l| l.addr == addr);
    let count = same.clone().count();
    if let Some(newest) = same.max_by_key(|l| l.began) {
        if count >= MAX_IN_FLIGHT || at.saturating_duration_since(newest.began) < JOIN_WITHIN {
            return Ok(Arc::clone(newest));
        }
    }
    let lookup = Arc::new(Lookup {
        addr: addr.to_string(),
        began: Instant::now(),
        found: Mutex::new(None),
        came: Condvar::new(),
    });
    let theirs = Arc::clone(&lookup);
    std::thread::Builder::new()
        .name("fennec-resolve".into())
        .spawn(move || {
            let found = system_lookup(&theirs.addr).map_err(|e| (e.kind(), e.to_string()));
            *lock(&theirs.found) = Some(found);
            theirs.came.notify_all();
            lock(&OUT).retain(|l| !Arc::ptr_eq(l, &theirs));
        })?;
    out.push(Arc::clone(&lookup));
    Ok(lookup)
}

/// The system's resolver, or a test's stand-in for a name it holds.
fn system_lookup(addr: &str) -> io::Result<Vec<SocketAddr>> {
    if STOOD_IN.load(Ordering::Relaxed) {
        let f = lock(&STAND_INS)
            .iter()
            .find(|(name, _)| name == addr)
            .map(|(_, f)| Arc::clone(f));
        if let Some(f) = f {
            return f();
        }
    }
    Ok(addr.to_socket_addrs()?.collect())
}

type StandIn = Arc<dyn Fn() -> io::Result<Vec<SocketAddr>> + Send + Sync>;

static STAND_INS: Mutex<Vec<(String, StandIn)>> = Mutex::new(Vec::new());
/// Whether any name is stood in for: one load a lookup where none is.
static STOOD_IN: AtomicBool = AtomicBool::new(false);

/// Has `addr` looked up by `f` in place of the system's resolver: what a
/// test makes a slow or silent resolver of, for one name of its own, the
/// rest of the process's lookups untouched. A second stand-in for the name
/// replaces the first.
#[doc(hidden)]
pub fn stand_in(addr: &str, f: impl Fn() -> io::Result<Vec<SocketAddr>> + Send + Sync + 'static) {
    let mut all = lock(&STAND_INS);
    all.retain(|(name, _)| name != addr);
    all.push((addr.to_string(), Arc::new(f)));
    STOOD_IN.store(true, Ordering::Relaxed);
}

/// A connection to `addr` -- `host:port`, or `https://host:port` over TLS
/// ([`tls_of`]): its name looked up within `within` ([`resolve`]), then
/// each address it stands for tried within `within`, as
/// `TcpStream::connect_timeout` tries one, and over TLS the handshake
/// done within `within` ([`secure`]).
pub fn connect(addr: &str, within: Duration) -> io::Result<Stream> {
    let (tls, authority) = tls_of(addr);
    let mut last = io::Error::new(
        io::ErrorKind::NotFound,
        format!("{authority} does not resolve"),
    );
    for a in resolve(authority, within)? {
        match TcpStream::connect_timeout(&a, within) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return match tls {
                    false => Ok(Stream::Plain(s)),
                    true => secure(s, authority, within),
                };
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// [`connect`], given up on as soon as `stop` is set: the dial -- lookup,
/// connect and handshake -- runs on a thread of its own and is left to end
/// there, its connection, if it makes one, closed unused. An `Interrupted`
/// error when stopped. Its answer comes back through a slot and a condvar,
/// as a lookup's does: an `mpsc` channel was 13 to 14 KB more of each
/// binary's code.
pub fn connect_unless(addr: &str, within: Duration, stop: &AtomicBool) -> io::Result<Stream> {
    type Slot = (Mutex<Option<io::Result<Stream>>>, Condvar);
    let slot: Arc<Slot> = Arc::new((Mutex::new(None), Condvar::new()));
    let (theirs, to) = (Arc::clone(&slot), addr.to_string());
    std::thread::Builder::new()
        .name("fennec-dial".into())
        .spawn(move || {
            let dialed = connect(&to, within);
            *lock(&theirs.0) = Some(dialed);
            theirs.1.notify_all();
        })?;
    let mut dialed = lock(&slot.0);
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!("stopped while connecting to {addr}"),
            ));
        }
        if let Some(d) = dialed.take() {
            return d;
        }
        dialed = slot
            .1
            .wait_timeout(dialed, STOP_LOOK)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A stand-in for `name` that answers `to` once its gate is opened.
    fn gated(name: &str, to: SocketAddr) -> Arc<(Mutex<bool>, Condvar)> {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = Arc::clone(&gate);
        stand_in(name, move || {
            let (open, opened) = &*g;
            let mut open = lock(open);
            while !*open {
                open = opened.wait(open).unwrap_or_else(|e| e.into_inner());
            }
            Ok(vec![to])
        });
        gate
    }

    fn open(gate: &(Mutex<bool>, Condvar)) {
        *lock(&gate.0) = true;
        gate.1.notify_all();
    }

    fn out_for(name: &str) -> usize {
        lock(&OUT).iter().filter(|l| l.addr == name).count()
    }

    /// A name whose lookup hangs is a timeout at the caller's bound, and a
    /// caller asking it meanwhile shares the lookup; once it answers, a
    /// caller has the address and connects to it.
    #[test]
    fn a_lookup_that_hangs_is_a_timeout_and_is_shared() {
        let name = "hangs.dial.test:1";
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let to = l.local_addr().unwrap();
        let gate = gated(name, to);
        let asked = Instant::now();
        let e = resolve(name, Duration::from_millis(50)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        // Bounded from below only: the caller waited its bound.
        assert!(asked.elapsed() >= Duration::from_millis(50));
        // Asked at the moment the first was, the lookup out is joined, not
        // begun again -- asked by the clock, a loaded machine could put the
        // second ask past JOIN_WITHIN.
        let shared = joined(name, asked).unwrap();
        assert_eq!(out_for(name), 1);
        assert!(lock(&shared.found).is_none());
        let waiting = std::thread::spawn(move || resolve(name, Duration::from_secs(30)));
        open(&gate);
        assert_eq!(waiting.join().unwrap().unwrap(), vec![to]);
        assert!(connect(name, Duration::from_secs(5)).is_ok());
    }

    /// Past [`MAX_IN_FLIGHT`] lookups of a name out at once, a caller waits
    /// on the newest rather than begin another: the threads a name that
    /// never answers costs are bounded.
    #[test]
    fn a_name_that_never_answers_costs_a_bounded_number_of_lookups() {
        let name = "silent.dial.test:1";
        let gate = gated(name, "127.0.0.1:9".parse().unwrap());
        // Each ask a JOIN_WITHIN after the one before, as callers asking
        // that far apart find the newest lookup too old to join.
        let start = Instant::now();
        for i in 1..=(MAX_IN_FLIGHT as u32 * 3) {
            joined(name, start + JOIN_WITHIN * i).unwrap();
        }
        assert_eq!(out_for(name), MAX_IN_FLIGHT);
        open(&gate);
    }

    /// A connect walked away from returns as soon as its stop flag is set,
    /// the lookup it was waiting on hanging still.
    #[test]
    fn a_connect_is_given_up_at_the_word() {
        let name = "stopped.dial.test:1";
        let gate = gated(name, "127.0.0.1:9".parse().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let dialing = std::thread::spawn(move || connect_unless(name, Duration::from_secs(60), &s));
        while out_for(name) == 0 {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::SeqCst);
        let e = dialing.join().unwrap().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted, "{e}");
        open(&gate);
    }

    /// Asked within no time at all, a name is a timeout with no lookup
    /// begun for nobody to wait on; asked within more than an `Instant`
    /// holds, it is waited for as long as it takes, not a panic.
    #[test]
    fn no_bound_and_a_bound_past_any_instant_are_both_taken() {
        let name = "bounds.dial.test:1";
        let to: SocketAddr = "127.0.0.1:9".parse().unwrap();
        stand_in(name, move || Ok(vec![to]));
        let e = resolve(name, Duration::ZERO).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        assert_eq!(out_for(name), 0);
        assert_eq!(resolve(name, Duration::MAX).unwrap(), vec![to]);
    }

    /// An address written out is read where it stands: asked within no
    /// time at all, a lookup would be a timeout.
    #[test]
    fn an_address_written_out_is_not_looked_up() {
        for a in ["127.0.0.1:8081", "[::1]:8081"] {
            assert_eq!(
                resolve(a, Duration::ZERO).unwrap(),
                vec![a.parse::<SocketAddr>().unwrap()]
            );
        }
    }

    /// A URL is read into what a dial takes -- its scheme kept where it is
    /// TLS, a port it leaves out its scheme's -- and an address back into
    /// the URL it stands for, the name a certificate is held to its host.
    #[test]
    fn a_url_is_read_into_an_address_and_back() {
        let split = |u: &str| split_url(u).unwrap();
        assert_eq!(split("http://h:1"), ("h:1".into(), "".into()));
        assert_eq!(
            split("http://h:1/t/acme/"),
            ("h:1".into(), "/t/acme".into())
        );
        assert_eq!(split("http://h"), ("h:80".into(), "".into()));
        assert_eq!(split("http://[::1]:9"), ("[::1]:9".into(), "".into()));
        assert_eq!(split("http://[::1]"), ("[::1]:80".into(), "".into()));
        for bad in ["h:1", "ftp://h:1", "http://", "http:///x", "http://:1"] {
            assert!(split_url(bad).is_err(), "{bad}");
        }
        if cfg!(feature = "tls") {
            assert_eq!(split("https://h:1/x"), ("https://h:1".into(), "/x".into()));
            assert_eq!(split("https://h"), ("https://h:443".into(), "".into()));
        } else {
            assert!(split_url("https://h:1").unwrap_err().contains("TLS"));
        }
        assert_eq!(tls_of("https://h:1"), (true, "h:1"));
        assert_eq!(tls_of("http://h:1"), (false, "h:1"));
        assert_eq!(tls_of("h:1"), (false, "h:1"));
        assert_eq!(authority("https://h:1"), "h:1");
        assert_eq!(url_of("h:1"), "http://h:1");
        assert_eq!(url_of("https://h:1"), "https://h:1");
        assert_eq!(host_of("h:1"), "h");
        assert_eq!(host_of("[::1]:1"), "[::1]");
        assert_eq!(host_of("127.0.0.1:1"), "127.0.0.1");
    }
}
