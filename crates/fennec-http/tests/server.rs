//! What the server owes every client whatever it asks: writes on disk when
//! it says so, a disk that fails reported and no write taken after it, an
//! fsync that holds up neither the readers nor the other writers, a deep
//! query that does not take the process down, and its ceilings on
//! connections and silence. These were held over the pg wire until it went;
//! HTTP is every client's way in now.

use fennec_core::prelude::*;
use fennec_http::{Config, Server};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

/// How long a client waits for an answer: a bound on a hang, never a
/// pace. What it waits on is a request's work, which a loaded runner
/// slows: 10 s ran out once beside a load of 33 on twelve searches at
/// once (`concurrent_vector_search_matches_serial`), and read as the
/// server closing the connection.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

struct Harness {
    port: u16,
    db: Arc<RwLock<Database>>,
}

fn start(cfg: Config, db: Database) -> Harness {
    start_with(cfg, db, false)
}

/// [`start`], keeping a change feed for `/_changes` where `cdc` is set, as
/// `--cdc` does: no replica is fed, and the database's sink is its own.
fn start_with(mut cfg: Config, db: Database, cdc: bool) -> Harness {
    use fennec_http::replication::{Feed, Replication};
    cfg.addr = "127.0.0.1:0".into();
    let db = Arc::new(RwLock::new(db));
    let mut server = Server::new(Arc::clone(&db), cfg);
    if cdc {
        let feed = Some(Feed::new(1 << 20));
        server = server.with_replication(Replication::new(None, feed, None));
    }
    let listener = server.bind().expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Harness { port, db }
}

/// A keep-alive connection: requests one after another, as a client's pool
/// keeps one.
struct Conn {
    r: BufReader<TcpStream>,
    w: TcpStream,
}

impl Conn {
    fn open(port: u16) -> Conn {
        let s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        s.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
        Conn {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
        }
    }

    /// `(status, body)`. The connection closed before the answer was whole
    /// is an `UnexpectedEof` saying so, and a read that waited out
    /// [`ANSWER_WITHIN`] an error naming the bound: told apart, where each
    /// was `None`, and a timeout was reported as a close.
    fn ask(&mut self, method: &str, target: &str, body: &str) -> io::Result<(u16, String)> {
        let req = format!(
            "{method} {target} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        self.w.write_all(req.as_bytes())?;
        let line = self.line()?;
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| unreadable(&line))?;
        let mut len = 0;
        loop {
            let h = self.line()?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().map_err(|_| unreadable(h))?;
                }
            }
        }
        let mut body = vec![0; len];
        self.r.read_exact(&mut body).map_err(told)?;
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }

    /// The answer's next line, its end an error ([`told`]).
    fn line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        match self.r.read_line(&mut line).map_err(told)? {
            0 => Err(closed()),
            _ => Ok(line),
        }
    }

    /// A write sent with `Idempotency-Key: key`: `(status, whether it was
    /// a replay, body)`.
    fn keyed(&mut self, target: &str, key: &str, body: &str) -> (u16, bool, String) {
        self.sent(target, &format!("Idempotency-Key: {key}\r\n"), body)
    }

    /// A `POST` with the header lines `extra`, each ending `\r\n`, answered
    /// as [`Self::keyed`] is.
    fn sent(&mut self, target: &str, extra: &str, body: &str) -> (u16, bool, String) {
        let req = format!(
            "POST {target} HTTP/1.1\r\nHost: localhost\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        self.w.write_all(req.as_bytes()).unwrap();
        let mut line = String::new();
        self.r.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let (mut len, mut replayed) = (0, false);
        loop {
            let mut h = String::new();
            self.r.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap();
                }
                replayed |= k.eq_ignore_ascii_case("idempotent-replayed");
            }
        }
        let mut body = vec![0; len];
        self.r.read_exact(&mut body).unwrap();
        (
            status,
            replayed,
            String::from_utf8_lossy(&body).into_owned(),
        )
    }

    /// A statement through `POST /query`.
    fn query(&mut self, q: &str) -> (u16, String) {
        let body = format!("{{\"query\": {}}}", json_string(q));
        self.ask("POST", "/query", &body)
            .unwrap_or_else(|e| panic!("POST /query {q:?}: {e}"))
    }
}

fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the server closed the connection",
    )
}

/// A read's error as a test reports it: the connection's end a close, and
/// a read timing out -- `WouldBlock` where a Unix socket's timeout runs
/// out, `TimedOut` on Windows -- the bound it waited out.
fn told(e: io::Error) -> io::Error {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => closed(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no answer within {ANSWER_WITHIN:?} ({e})"),
        ),
        _ => e,
    }
}

fn unreadable(line: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("not an answer: {line:?}"),
    )
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn ok(r: (u16, String)) -> String {
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fennec-http-server-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    path
}

/// Under `--sync always` a write is on disk when it is answered: the file
/// opened on its own afterwards, as after a crash, holds it.
#[test]
fn sync_always_persists_every_write() {
    let path = tmp("always.fennec");
    let db = fennec_core::fs::open(&path).unwrap();
    let h = start(
        Config {
            sync_on_write: true,
            ..Config::default()
        },
        db,
    );
    let mut c = Conn::open(h.port);
    ok(c.query("create collection t (name text)"));
    ok(c.query(r#"put t {name: "durable"}"#));

    let reopened = fennec_core::fs::open(&path).unwrap();
    let rows = reopened
        .query(&fennec_ql::parse_one("get t select name").unwrap(), &[])
        .unwrap();
    let rs = rows.rows().unwrap();
    assert_eq!(rs.rows.len(), 1);
    assert_eq!(rs.rows[0].values[0], Value::Text("durable".into()));
}

/// A sink whose sync always fails, as a disk that went away does.
struct SyncFails;

impl fennec_core::engine::Sink for SyncFails {
    fn append(&mut self, _bytes: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
    fn rewrite(&mut self, _bytes: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
    fn sync(&mut self) -> fennec_core::error::Result<()> {
        Err(fennec_core::error::Error::Io("input/output error".into()))
    }
}

/// A sync the disk refuses is the answer, not the success the write had
/// not yet been given; every write after it is refused until the file is
/// opened again, and reads go on from memory. A failed fsync is never
/// retried: the kernel may already have dropped the pages.
#[test]
fn sync_failure_is_reported_and_stops_writes() {
    let h = start(
        Config {
            sync_on_write: true,
            ..Config::default()
        },
        Database::with_sink(Box::new(SyncFails)),
    );
    let mut c = Conn::open(h.port);
    let (status, body) = c.query("create collection t (name text)");
    assert_eq!(status, 500, "the failed sync must reach the client: {body}");

    let (status, body) = c.query(r#"put t {name: "a"}"#);
    assert_eq!(status, 500, "{body}");
    assert!(body.contains("refused"), "{body}");

    ok(c.query("collections"));
    assert!(h.db.read().unwrap().failure().is_some());
}

/// A disk whose fsync fails when `fail` is set -- the half of a sync that
/// runs outside the lock -- and waits at a [`Gate`] the test holds. It
/// keeps `FileSink`'s protocol: an fsync covers every byte appended before
/// it starts, one that finds its bytes covered does not run, and after one
/// failed every durability fails without an fsync (`Disk::failed`): the
/// kernel may have dropped the pages it could not write. It counts the
/// fsyncs it ran.
#[derive(Clone)]
struct SlowDisk {
    fail: bool,
    /// An fsync failed: never tried again.
    failed: Arc<AtomicBool>,
    fsyncs: Arc<AtomicUsize>,
    appended: Arc<AtomicU64>,
    /// Bytes on disk; held through an fsync, as the file is.
    synced: Arc<Mutex<u64>>,
    gate: Arc<(Mutex<Gate>, Condvar)>,
}

/// What an fsync waits for: the test letting it go, or the appends it
/// is told to wait for -- writes that could land only if the fsync held no
/// lock. Ordered by events, not by the clock: timed with a 200 ms fsync
/// and a 50 ms head start, the test failed on a loaded machine with no
/// lock held anywhere. An fsync that waits out `GATE_LIMIT` goes on and
/// says so in `timed_out`, which is the failure: what it waited for could
/// not happen beside it.
#[derive(Default)]
struct Gate {
    /// Appends so far: a write's record each.
    appends: u64,
    /// While set, an fsync waits until `appends` reaches it.
    hold_until: Option<u64>,
    /// Fsyncs that reached the gate.
    reached: usize,
    /// Durabilities that began to run, before they take the disk: a
    /// replay waiting for the first try's fsync is one.
    asked: usize,
    /// Replays a test's client was answered, and the other writes it
    /// counts as their answers come ([`SlowDiskServer::send`]).
    replays: usize,
    timed_out: bool,
}

const GATE_LIMIT: Duration = Duration::from_secs(20);

impl SlowDisk {
    fn wait_at_gate(&self) {
        let (lock, cv) = &*self.gate;
        let mut g = lock.lock().unwrap();
        g.reached += 1;
        cv.notify_all();
        let (mut g, waited) = cv
            .wait_timeout_while(g, GATE_LIMIT, |g| {
                g.hold_until.is_some_and(|n| g.appends < n)
            })
            .unwrap();
        if waited.timed_out() {
            g.timed_out = true;
        }
    }
}

impl fennec_core::engine::Sink for SlowDisk {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.appended
            .fetch_add(bytes.len() as u64, Ordering::SeqCst);
        let (lock, cv) = &*self.gate;
        lock.lock().unwrap().appends += 1;
        cv.notify_all();
        Ok(())
    }
    fn rewrite(&mut self, _bytes: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
    fn flush(&mut self) -> fennec_core::error::Result<Option<fennec_core::engine::Durability>> {
        let disk = self.clone();
        let upto = self.appended.load(Ordering::SeqCst);
        Ok(Some(Box::new(move || {
            {
                let (lock, cv) = &*disk.gate;
                lock.lock().unwrap().asked += 1;
                cv.notify_all();
            }
            let mut synced = disk.synced.lock().unwrap();
            if disk.failed.load(Ordering::SeqCst) {
                return Err(fennec_core::error::Error::Io("input/output error".into()));
            }
            if *synced >= upto {
                return Ok(());
            }
            let covers = disk.appended.load(Ordering::SeqCst);
            disk.wait_at_gate();
            disk.fsyncs.fetch_add(1, Ordering::SeqCst);
            if disk.fail {
                disk.failed.store(true, Ordering::SeqCst);
                return Err(fennec_core::error::Error::Io("input/output error".into()));
            }
            *synced = covers;
            Ok(())
        })))
    }
}

struct SlowDiskServer {
    h: Harness,
    fsyncs: Arc<AtomicUsize>,
    gate: Arc<(Mutex<Gate>, Condvar)>,
}

impl SlowDiskServer {
    /// Holds every fsync from now on until `appends` more writes landed;
    /// `u64::MAX` until [`Self::open`].
    fn hold_for(&self, appends: u64) {
        let mut g = self.gate.0.lock().unwrap();
        g.hold_until = Some(g.appends.saturating_add(appends));
    }

    fn open(&self) {
        self.gate.0.lock().unwrap().hold_until = None;
        self.gate.1.notify_all();
    }

    /// Waits until `n` fsyncs in all reached the gate.
    fn reached(&self, n: usize) {
        let (lock, cv) = &*self.gate;
        let (g, waited) = cv
            .wait_timeout_while(lock.lock().unwrap(), GATE_LIMIT, |g| g.reached < n)
            .unwrap();
        assert!(
            !waited.timed_out(),
            "{} fsyncs reached the disk, not {n}",
            g.reached
        );
    }

    fn timed_out(&self) -> bool {
        self.gate.0.lock().unwrap().timed_out
    }

    /// `(asked, replays)` as they stand.
    fn counts(&self) -> (usize, usize) {
        let g = self.gate.0.lock().unwrap();
        (g.asked, g.replays)
    }

    /// Waits until `asked` durabilities in all began or a replay past
    /// `replays` was answered, whichever comes first; whether one was.
    fn replayed_or_waiting(&self, asked: usize, replays: usize) -> bool {
        let (lock, cv) = &*self.gate;
        let (g, waited) = cv
            .wait_timeout_while(lock.lock().unwrap(), GATE_LIMIT, |g| {
                g.asked < asked && g.replays <= replays
            })
            .unwrap();
        assert!(!waited.timed_out(), "neither the replay nor its wait came");
        g.replays > replays
    }

    /// Sends a keyed write again, as a retry, and counts its answer in.
    fn replay(&self, target: &'static str, key: String, body: &'static str) -> Answer {
        self.send(target, format!("Idempotency-Key: {key}\r\n"), body)
    }

    /// Sends a `POST` with the header lines `extra` and counts its answer
    /// in, as a replay's.
    fn send(&self, target: &'static str, extra: String, body: &'static str) -> Answer {
        let (port, gate) = (self.h.port, Arc::clone(&self.gate));
        std::thread::spawn(move || {
            let answer = Conn::open(port).sent(target, &extra, body);
            gate.0.lock().unwrap().replays += 1;
            gate.1.notify_all();
            answer
        })
    }
}

type Answer = std::thread::JoinHandle<(u16, bool, String)>;

/// Every way a keyed write comes, and its body: a REST put, `/query`,
/// `/batch` and a call of a statement that writes, each under the write
/// lock; and a call of `r`, which writes as the first try runs and reads
/// as it is sent again ([`now_reading`]), answered under the read lock.
const KEYED: [(&str, &str); 5] = [
    ("/t", r#"{"n": 1}"#),
    ("/query", r#"{"query": "put t {n: 2}"}"#),
    (
        "/batch",
        "{\"query\": \"put t {n: 3}\"}\n{\"query\": \"put t {n: 4}\"}",
    ),
    ("/_call/w", r#"{"params":{}}"#),
    ("/_call/r", r#"{"params":{}}"#),
];

/// `r` made a statement that reads, as it is called again: written into
/// the database alone, so no durability of its own runs.
fn now_reading(db: &RwLock<Database>) {
    let mut g = db.write().unwrap();
    for q in [
        "drop statement r",
        "create statement r() public as get t count",
    ] {
        g.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
}

fn slow_disk_server(fail: bool) -> SlowDiskServer {
    slow_disk_server_with(fail, false)
}

/// [`slow_disk_server`] serving as `cfg` says, `--sync always` whatever
/// it does.
fn slow_disk_server_cfg(fail: bool, cfg: Config) -> SlowDiskServer {
    slow_disk_server_as(fail, false, cfg)
}

/// [`slow_disk_server`], keeping a change feed where `cdc` is set
/// ([`start_with`]).
fn slow_disk_server_with(fail: bool, cdc: bool) -> SlowDiskServer {
    slow_disk_server_as(fail, cdc, Config::default())
}

fn slow_disk_server_as(fail: bool, cdc: bool, cfg: Config) -> SlowDiskServer {
    let fsyncs = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
    let mut db = Database::with_sink(Box::new(SlowDisk {
        fail,
        failed: Arc::new(AtomicBool::new(false)),
        fsyncs: Arc::clone(&fsyncs),
        appended: Arc::new(AtomicU64::new(0)),
        synced: Arc::new(Mutex::new(0)),
        gate: Arc::clone(&gate),
    }));
    for q in [
        "create collection t (n int)",
        "create statement w() public as insert t {n: 5}",
        "create statement r() public as insert t {n: 6}",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    let cfg = Config {
        sync_on_write: true,
        ..cfg
    };
    let h = start_with(cfg, db, cdc);
    SlowDiskServer { h, fsyncs, gate }
}

/// The fsync of a durable write runs outside the write lock: a read
/// arriving meanwhile is answered at once, and writers arriving together
/// share one fsync -- the group commit -- rather than take one each in
/// turn under the lock.
///
/// Held by order, not by the clock: the fsync is held until the read is
/// answered, or until the other writers' records have landed, which under
/// the lock neither could be -- the fsync then waits out `GATE_LIMIT` and
/// the test fails on it. Timed (a 200 ms fsync, the read under 100 ms, the
/// eight writes under a second) it failed once on a loaded machine.
#[test]
fn a_durable_write_does_not_hold_the_readers_or_the_other_writers() {
    let s = slow_disk_server(false);
    let port = s.h.port;
    s.hold_for(u64::MAX);
    let writer = std::thread::spawn(move || ok(Conn::open(port).query("put t {n: 1}")));
    s.reached(1);
    // The write's fsync is under way, and stays so until the read is in.
    ok(Conn::open(port).query("get t count"));
    s.open();
    writer.join().unwrap();
    assert!(!s.timed_out(), "the read waited for the fsync");

    // Eight writers at once, the first fsync held until all eight records
    // are in: with an fsync each under the lock, the second writer could
    // not append. Sharing, the one fsync after it covers the other seven.
    let before = s.fsyncs.load(Ordering::SeqCst);
    s.hold_for(8);
    let writers: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || ok(Conn::open(port).query(&format!("put t {{n: {i}}}"))))
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    assert!(!s.timed_out(), "the writers waited for each other's fsync");
    let ran = s.fsyncs.load(Ordering::SeqCst) - before;
    assert!(ran <= 2, "{ran} fsyncs for 8 writes");
}

/// An fsync that fails after the lock was let go takes the answer back:
/// the write is reported failed, and the engine refuses every write after
/// it, as after a failure of its own.
#[test]
fn a_failed_fsync_outside_the_lock_is_reported_and_stops_writes() {
    let SlowDiskServer { h, .. } = slow_disk_server(true);
    let mut c = Conn::open(h.port);
    let (status, body) = c.query("put t {n: 1}");
    assert_eq!(
        status, 500,
        "the failed fsync must reach the client: {body}"
    );

    let (status, body) = c.query("put t {n: 2}");
    assert_eq!(status, 500, "{body}");
    assert!(body.contains("refused"), "{body}");

    ok(c.query("get t count"));
    assert!(h.db.read().unwrap().failure().is_some());
}

/// A keyed write's first try sent, its fsync held at the gate, and the
/// same request sent again: whether the replay was answered while the
/// fsync was held, and the two answers to come once the test opens the
/// gate. By the event: the replay's own durability began -- waiting at the
/// disk the held fsync has -- or the replay was answered.
fn race(
    s: &SlowDiskServer,
    target: &'static str,
    body: &'static str,
    key: &str,
) -> (bool, Answer, Answer) {
    let port = s.h.port;
    let reached = s.gate.0.lock().unwrap().reached;
    let (asked, replays) = s.counts();
    s.hold_for(u64::MAX);
    let first = {
        let key = key.to_string();
        std::thread::spawn(move || Conn::open(port).keyed(target, &key, body))
    };
    s.reached(reached + 1);
    if target == "/_call/r" {
        now_reading(&s.h.db);
    }
    let replay = s.replay(target, key.to_string(), body);
    let early = s.replayed_or_waiting(asked + 2, replays);
    (early, first, replay)
}

/// A write sent again with its `Idempotency-Key` while the first try's
/// fsync runs is answered once that fsync is done, never before: its key
/// is read from the moment its block lands, under the write lock, and the
/// fsync runs after the lock -- answered then, a client's retry or the
/// router's replay of a cut connection held a success a power loss took
/// back, and was never sent again. Every way a keyed write comes, a call
/// made to read meanwhile too (answered under the read lock).
#[test]
fn a_replay_is_answered_once_its_first_tries_fsync_is_done() {
    let s = slow_disk_server(false);
    for (i, &(target, body)) in KEYED.iter().enumerate() {
        let (early, first, replay) = race(&s, target, body, &format!("held-{i}"));
        s.open();
        assert!(
            !early,
            "{target}: the replay was answered before the first try's fsync"
        );
        let (status, replayed, made) = first.join().unwrap();
        assert!(status < 300 && !replayed, "{target}: {status} {made}");
        let again = replay.join().unwrap();
        assert_eq!(again, (status, true, made), "{target}");
    }
    assert!(!s.timed_out(), "an fsync waited out the gate");
}

/// A replay of a write whose fsync fails is not told it was made: it waits
/// for that fsync and is answered its failure, the first try's 500, and
/// sent after it the 500 every write is refused with until the file is
/// opened again. Answered at once, as it was, it was the success the first
/// try was never given. Every way a keyed write comes, each on a server of
/// its own, since a failure stops the writes. The replay's durability
/// fails as the disk's failure sticks, running no fsync of its own: one
/// tried again might report pages the kernel dropped as written.
#[test]
fn a_replay_of_a_write_whose_fsync_failed_is_no_success() {
    for (i, &(target, body)) in KEYED.iter().enumerate() {
        let s = slow_disk_server(true);
        let key = format!("failed-{i}");
        let before = s.fsyncs.load(Ordering::SeqCst);
        let (early, first, replay) = race(&s, target, body, &key);
        s.open();
        assert!(
            !early,
            "{target}: the replay was answered before the fsync failed"
        );
        let (status, _, made) = first.join().unwrap();
        assert_eq!(status, 500, "{target}: {made}");
        let (status, replayed, again) = replay.join().unwrap();
        assert_eq!((status, replayed), (500, false), "{target}: {again}");
        assert_eq!(
            s.fsyncs.load(Ordering::SeqCst),
            before + 1,
            "{target}: the failed fsync was tried again"
        );
        let (status, replayed, after) = Conn::open(s.h.port).keyed(target, &key, body);
        assert_eq!((status, replayed), (500, false), "{target}: {after}");
        assert!(after.contains("refused"), "{target}: {after}");
    }
}

/// After a storage error a replay is the 500 every write is refused with,
/// under either `--sync`: the kept success may be of a write the disk lost
/// with the error -- a failed fsync, or under `--sync <ms>` a failed write
/// of the buffer it went out in -- whose outcome no one here knows.
/// Answered the success, as `--sync <ms>` did, its client held as made a
/// write the next open may not have. Every way a keyed write comes.
#[test]
fn a_replay_after_a_storage_error_is_no_success_under_either_sync() {
    for sync_on_write in [true, false] {
        let mut db = Database::new();
        for q in [
            "create collection t (n int)",
            "create statement w() public as insert t {n: 5}",
            "create statement r() public as insert t {n: 6}",
        ] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        let h = start(
            Config {
                sync_on_write,
                ..Config::default()
            },
            db,
        );
        let mut c = Conn::open(h.port);
        for (i, &(target, body)) in KEYED.iter().enumerate() {
            let (status, replayed, made) = c.keyed(target, &format!("lost-{i}"), body);
            assert!(status < 300 && !replayed, "{target}: {status} {made}");
            let (status, replayed, again) = c.keyed(target, &format!("lost-{i}"), body);
            assert_eq!(
                (status, replayed, &again),
                (status, true, &made),
                "{target}"
            );
        }
        now_reading(&h.db);
        h.db.write()
            .unwrap()
            .fail(&fennec_core::error::Error::Io("input/output error".into()));
        for (i, &(target, body)) in KEYED.iter().enumerate() {
            let (status, replayed, after) = c.keyed(target, &format!("lost-{i}"), body);
            assert_eq!(
                (status, replayed),
                (500, false),
                "{target}, sync_on_write {sync_on_write}: {after}"
            );
            assert!(after.contains("refused"), "{target}: {after}");
        }
    }
}

/// The child of [`a_key_a_killed_process_landed_is_replayed_once_on_disk`]:
/// the file it opens.
const KILLED: &str = "FENNEC_TEST_KILLED_BEFORE_FSYNC";
/// Its exit, inside its keyed write's durability.
const KILLED_CODE: i32 = 42;

/// A file's sink whose durability ends the process before its fsync, the
/// block written into the file first as a durability writes it: a process
/// killed between the two, its write in the page cache alone.
struct DiesAtFsync(Box<dyn fennec_core::engine::Sink>);

impl fennec_core::engine::Sink for DiesAtFsync {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0.append(bytes)
    }
    fn record(&mut self, seq: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0.record(seq, bytes)
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0.rewrite(bytes)
    }
    fn remapped(&mut self) -> Option<fennec_core::store::Base> {
        self.0.remapped()
    }
    fn written_through(&mut self) -> fennec_core::error::Result<Option<fennec_core::store::Base>> {
        self.0.written_through()
    }
    fn sync(&mut self) -> fennec_core::error::Result<()> {
        self.0.sync()
    }
    fn flush(&mut self) -> fennec_core::error::Result<Option<fennec_core::engine::Durability>> {
        self.0.write_out()?;
        Ok(Some(Box::new(|| std::process::exit(KILLED_CODE))))
    }
}

/// What a replay's durability went through on a file opened after the
/// kill, in order, a test's client's answer last.
#[derive(Default)]
struct Reopened {
    /// The durability held here until set.
    open: bool,
    events: Vec<&'static str>,
}

type ReopenedGate = Arc<(Mutex<Reopened>, Condvar)>;

/// A file's sink whose durabilities wait at a gate and say what they did:
/// asked, then the file's own durability run.
struct Held {
    inner: Box<dyn fennec_core::engine::Sink>,
    gate: ReopenedGate,
}

impl fennec_core::engine::Sink for Held {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.inner.append(bytes)
    }
    fn record(&mut self, seq: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.inner.record(seq, bytes)
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.inner.rewrite(bytes)
    }
    fn remapped(&mut self) -> Option<fennec_core::store::Base> {
        self.inner.remapped()
    }
    fn written_through(&mut self) -> fennec_core::error::Result<Option<fennec_core::store::Base>> {
        self.inner.written_through()
    }
    fn sync(&mut self) -> fennec_core::error::Result<()> {
        self.inner.sync()
    }
    fn flush(&mut self) -> fennec_core::error::Result<Option<fennec_core::engine::Durability>> {
        let inner = self.inner.flush()?;
        let gate = Arc::clone(&self.gate);
        Ok(Some(Box::new(move || {
            {
                let (m, cv) = &*gate;
                let mut g = m.lock().unwrap();
                g.events.push("asked");
                cv.notify_all();
                let _ = cv.wait_timeout_while(g, GATE_LIMIT, |g| !g.open).unwrap();
            }
            let r = inner.map_or(Ok(()), |d| d());
            gate.0.lock().unwrap().events.push("durable");
            gate.1.notify_all();
            r
        })))
    }
}

/// A key landed by a process killed before its fsync is in the file, and
/// for all anyone knows in the page cache alone: a server opening the file
/// answers a replay of it once the file's durability ran -- which fsyncs
/// what the process left though it covers nothing this one appended
/// (`fennec_core::fs`'s `a_servers_first_durability_fsyncs_what_a_killed_process_left`)
/// -- never before. Answered at once, a power loss after took back a write
/// its client held as made. The test binary run again as the process
/// killed: its keyed write's durability writes the block and exits.
#[test]
fn a_key_a_killed_process_landed_is_replayed_once_on_disk() {
    let body = r#"{"query": "put t {n: 9}"}"#;
    if let Ok(p) = std::env::var(KILLED) {
        let db = fennec_core::fs::open_with(
            &p,
            true,
            Box::new(
                |inner| Ok(Box::new(DiesAtFsync(inner)) as Box<dyn fennec_core::engine::Sink>),
            ),
        )
        .unwrap();
        let h = start(
            Config {
                sync_on_write: true,
                ..Config::default()
            },
            db,
        );
        let answer = Conn::open(h.port).keyed("/query", "killed", body);
        panic!("answered before its fsync: {answer:?}");
    }
    let path = tmp("killed.fennec");
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        db.execute(&fennec_ql::parse_one("create collection t (n int)").unwrap())
            .unwrap();
        db.sync().unwrap();
    }
    let before = std::fs::metadata(&path).unwrap().len();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "server::a_key_a_killed_process_landed_is_replayed_once_on_disk",
            "--exact",
            "--test-threads=1",
        ])
        .env(KILLED, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(KILLED_CODE),
        "the child was not killed at its fsync"
    );
    assert!(
        std::fs::metadata(&path).unwrap().len() > before,
        "the killed process's block is not in the file"
    );

    let gate: ReopenedGate = Arc::default();
    let held = Arc::clone(&gate);
    let db = fennec_core::fs::open_serving(
        &path,
        true,
        Box::new(move |inner| {
            Ok(Box::new(Held { inner, gate: held }) as Box<dyn fennec_core::engine::Sink>)
        }),
    )
    .unwrap();
    let h = start(
        Config {
            sync_on_write: true,
            ..Config::default()
        },
        db,
    );
    let (port, told) = (h.port, Arc::clone(&gate));
    let replay = std::thread::spawn(move || {
        let answer = Conn::open(port).keyed("/query", "killed", body);
        told.0.lock().unwrap().events.push("answered");
        told.1.notify_all();
        answer
    });
    {
        let (m, cv) = &*gate;
        let (mut g, waited) = cv
            .wait_timeout_while(m.lock().unwrap(), GATE_LIMIT, |g| g.events.is_empty())
            .unwrap();
        assert!(!waited.timed_out(), "the replay asked for no durability");
        assert_eq!(
            g.events,
            ["asked"],
            "the replay was answered before its durability"
        );
        g.open = true;
        cv.notify_all();
    }
    let (status, replayed, answer) = replay.join().unwrap();
    assert_eq!((status, replayed), (200, true), "{answer}");
    assert_eq!(
        gate.0.lock().unwrap().events,
        ["asked", "durable", "answered"]
    );
}

/// A write refused by what another write left: the statements that make
/// its place, written into the database alone; the write that lands what
/// the refusal reads, whose fsync the test holds; the refused write, with
/// the header lines it is sent with; and the refusal's status -- or the
/// status of an answer read from it as a refusal is, an apply finding it
/// had nothing to do.
struct HeldRefusal {
    what: &'static str,
    setup: &'static [&'static str],
    first: (&'static str, &'static str),
    refused: (&'static str, &'static str, &'static str),
    status: u16,
}

/// A held write sent with `Fennec-Wait`, its wait short: answered at its
/// end with its last run's answer.
const WAIT: &str = "Fennec-Wait: 20\r\n";

/// Every write path that can be refused by what another write left, each
/// with a refusal of its own.
const HELD_REFUSALS: [HeldRefusal; 17] = [
    HeldRefusal {
        what: "an insert of a held id",
        setup: &["create collection d (n int)"],
        first: ("/query", r#"{"query": "insert d {id: 7, n: 1}"}"#),
        refused: ("/query", "", r#"{"query": "insert d {id: 7, n: 2}"}"#),
        status: 409,
    },
    HeldRefusal {
        what: "a @unique value held",
        setup: &["create collection u (e text @unique)"],
        first: ("/query", r#"{"query": "put u {e: \"a\"}"}"#),
        refused: ("/query", "", r#"{"query": "put u {e: \"a\"}"}"#),
        status: 409,
    },
    HeldRefusal {
        what: "a collection made",
        setup: &[],
        first: ("/query", r#"{"query": "create collection made (n int)"}"#),
        refused: (
            "/query",
            "",
            r#"{"query": "create collection made (n int)"}"#,
        ),
        status: 409,
    },
    HeldRefusal {
        what: "a statement made",
        setup: &[],
        first: (
            "/query",
            r#"{"query": "create statement s() as get t count"}"#,
        ),
        refused: (
            "/query",
            "",
            r#"{"query": "create statement s() as get t count"}"#,
        ),
        status: 409,
    },
    HeldRefusal {
        what: "a require's count",
        setup: &["create collection q (n int)", "put q {n: 1}"],
        first: ("/query", r#"{"query": "del q where n = 1"}"#),
        refused: (
            "/query",
            "",
            r#"{"query": "set q {n: 2} where n = 1 require 1"}"#,
        ),
        status: 412,
    },
    HeldRefusal {
        what: "an index made",
        setup: &["create collection ix (n int)"],
        first: ("/query", r#"{"query": "create index on ix (n) @hash"}"#),
        refused: ("/query", "", r#"{"query": "create index on ix (n) @hash"}"#),
        status: 409,
    },
    HeldRefusal {
        what: "a keyed insert of a held id",
        setup: &["create collection k (n int)"],
        first: ("/query", r#"{"query": "insert k {id: 7}"}"#),
        refused: (
            "/query",
            "Idempotency-Key: dup\r\n",
            r#"{"query": "insert k {id: 7}"}"#,
        ),
        status: 409,
    },
    HeldRefusal {
        what: "a /batch's insert of a held id",
        setup: &["create collection b (n int)"],
        first: ("/query", r#"{"query": "insert b {id: 7}"}"#),
        refused: ("/batch", "", r#"{"query": "insert b {id: 7}"}"#),
        status: 409,
    },
    HeldRefusal {
        what: "a /batch holding a compact, its first line refused",
        setup: &["create collection bc (n int)"],
        first: ("/query", r#"{"query": "insert bc {id: 7}"}"#),
        refused: (
            "/batch",
            "",
            "{\"query\": \"insert bc {id: 7}\"}\n{\"query\": \"compact bc\"}",
        ),
        status: 409,
    },
    HeldRefusal {
        what: "a REST write of a @unique value held",
        setup: &["create collection ru (e text @unique)"],
        first: ("/query", r#"{"query": "put ru {e: \"a\"}"}"#),
        refused: ("/ru", "", r#"{"e": "a"}"#),
        status: 409,
    },
    HeldRefusal {
        what: "a REST write to a collection dropped",
        setup: &["create collection gone (n int)"],
        first: ("/query", r#"{"query": "drop collection gone"}"#),
        refused: ("/gone", "", r#"{"n": 1}"#),
        status: 404,
    },
    HeldRefusal {
        what: "/_schema/apply's plan refused by a field another added",
        setup: &["create collection sp (n int)"],
        first: (
            "/query",
            r#"{"query": "alter collection sp add field m int"}"#,
        ),
        refused: ("/_schema/apply", "", SP_DECLARED),
        status: 409,
    },
    HeldRefusal {
        what: "/_schema/apply finding another applied it",
        setup: &[],
        first: ("/_schema/apply", SN_DECLARED),
        refused: ("/_schema/apply", "", SN_DECLARED),
        status: 200,
    },
    HeldRefusal {
        what: "a consumer's since ahead of it",
        setup: &[],
        first: ("/_changes/consumers/sink", ""),
        refused: ("/_changes/consumers/sink", "", r#"{"since": 99}"#),
        status: 409,
    },
    // Held, a run that wrote nothing waits for nothing: its answer at its
    // wait's end does.
    HeldRefusal {
        what: "a held require's count",
        setup: &["create collection hq (n int)", "put hq {n: 1}"],
        first: ("/query", r#"{"query": "del hq where n = 1"}"#),
        refused: (
            "/query",
            WAIT,
            r#"{"query": "set hq {n: 2} where n = 1 require 1"}"#,
        ),
        status: 412,
    },
    HeldRefusal {
        what: "a held keyed require's count",
        setup: &["create collection hk (n int)", "put hk {n: 1}"],
        first: ("/query", r#"{"query": "del hk where n = 1"}"#),
        refused: (
            "/query",
            "Fennec-Wait: 20\r\nIdempotency-Key: held\r\n",
            r#"{"query": "set hk {n: 2} where n = 1 require 1"}"#,
        ),
        status: 412,
    },
    HeldRefusal {
        what: "a held /batch's require's count",
        setup: &["create collection hb (n int)", "put hb {n: 1}"],
        first: ("/query", r#"{"query": "del hb where n = 1"}"#),
        refused: (
            "/batch",
            WAIT,
            r#"{"query": "set hb {n: 2} where n = 1 require 1"}"#,
        ),
        status: 412,
    },
];

/// Under `--sync always` a write refused by what another write left is
/// answered once that write's fsync is done, as its success would be, never
/// before: the other write's block is read from the moment it lands, under
/// the write lock, and its fsync runs after the lock. Answered at once, an
/// `insert` was told 409 of a row a power loss before that fsync took
/// back, and its client never sent it again. Every way a write is refused
/// so, each through its path: `/query`, a keyed `/query`, `/batch` as one
/// block and as statements on their own, a REST write refused as it runs
/// and as it is routed, `create index` beside the database, a `/_changes`
/// consumer, and a held write's answer at its wait's end. By the event:
/// the refusal's own durability began -- waiting at the disk the held fsync
/// has -- or it was answered.
#[test]
fn a_refusal_is_answered_once_what_it_read_is_on_disk() {
    let s = slow_disk_server_with(false, true);
    let mut early = Vec::new();
    for case in &HELD_REFUSALS {
        let what = case.what;
        {
            let mut g = s.h.db.write().unwrap();
            for q in case.setup {
                g.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
            }
        }
        let (target, body) = case.first;
        let (was_early, (status, _, made), (refused, _, told)) =
            refused_beside(&s, (target, "", body), case.refused);
        if was_early {
            early.push(what);
        }
        assert!(status < 300, "{what}: the first write {status} {made}");
        assert_eq!(refused, case.status, "{what}: {told}");
        if case.status == 200 {
            assert!(told.contains(r#""applied":false"#), "{what}: {told}");
        }
    }
    assert!(
        early.is_empty(),
        "refused before the fsync of what they read: {early:?}"
    );
    assert!(!s.timed_out(), "an fsync waited out the gate");
}

/// `sp` declared with `n` alone: refused (409) where another write added a
/// field, applying nothing where none did.
const SP_DECLARED: &str =
    r#"{"format":1,"collections":[{"name":"sp","fields":[{"name":"n","type":"int"}]}]}"#;

/// `sn` declared: made by the first apply, there for the second.
const SN_DECLARED: &str =
    r#"{"format":1,"collections":[{"name":"sn","fields":[{"name":"n","type":"int"}]}]}"#;

/// Sends `first` and holds its fsync at the gate, then `refused` beside it,
/// each `(target, header lines, body)`: whether `refused` was answered
/// before a durability of its own began -- the one that waits at the disk
/// with the held fsync -- and, once the gate is opened, the two answers.
fn refused_beside(
    s: &SlowDiskServer,
    first: (&'static str, &str, &'static str),
    refused: (&'static str, &str, &'static str),
) -> (bool, (u16, bool, String), (u16, bool, String)) {
    let reached = s.gate.0.lock().unwrap().reached;
    let (asked, replays) = s.counts();
    s.hold_for(u64::MAX);
    let made = s.send(first.0, first.1.to_string(), first.2);
    s.reached(reached + 1);
    let answer = s.send(refused.0, refused.1.to_string(), refused.2);
    let early = s.replayed_or_waiting(asked + 2, replays);
    s.open();
    (early, made.join().unwrap(), answer.join().unwrap())
}

/// A refusal of the request alone reads nothing another write left, and
/// waits for no fsync: a REST write whose body is no JSON, or sent to a
/// path no route takes, routed under the write lock while another write's
/// fsync is held, is answered then -- it waited for that fsync, which
/// changes nothing it says.
#[test]
fn a_refusal_of_the_request_alone_waits_for_no_fsync() {
    let s = slow_disk_server(false);
    s.h.db
        .write()
        .unwrap()
        .execute(&fennec_ql::parse_one("create collection pz (n int)").unwrap())
        .unwrap();
    let first = ("/query", "", r#"{"query": "put pz {n: 1}"}"#);
    for (what, refused, status) in [
        ("a body that is no JSON", ("/pz", "", "{not json"), 400),
        ("a path no route takes", ("/pz/a/b", "", r#"{"n": 2}"#), 404),
    ] {
        let (early, (made, _, _), (told, _, body)) = refused_beside(&s, first, refused);
        assert!(made < 300, "{what}: the first write {made}");
        assert_eq!(told, status, "{what}: {body}");
        assert!(early, "{what}: waited for an fsync it never read");
    }
    assert!(!s.timed_out(), "an fsync waited out the gate");
}

/// A held write's run asks no durability: its answer is kept back, and the
/// wait's end asks it once (`held_answered`). Asked at each run too, a
/// `require` refused waited at the disk on every look of its group, whose
/// other requests waited behind it. Counted by the durabilities begun: the
/// first write's and the held one's at its end, never a third.
#[test]
fn a_held_run_asks_no_durability_its_wait_end_does() {
    let s = slow_disk_server(false);
    {
        let mut g = s.h.db.write().unwrap();
        for q in ["create collection hr (n int)", "put hr {n: 1}"] {
            g.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
    }
    let (asked, _) = s.counts();
    let (early, (made, _, _), (told, _, body)) = refused_beside(
        &s,
        ("/query", "", r#"{"query": "del hr where n = 1"}"#),
        (
            "/query",
            WAIT,
            r#"{"query": "set hr {n: 2} where n = 1 require 1"}"#,
        ),
    );
    assert!(made < 300 && !early, "{made} {early}");
    assert_eq!(told, 412, "{body}");
    assert_eq!(s.counts().0, asked + 2, "durabilities begun");
}

/// The stop of a `/batch` run statement by statement says `completed`
/// statements were made: answered once they are durable, as a write is,
/// and where the fsync fails, the 500 that failure is -- never the 409 of
/// the line after, `completed: 1`, of a write the disk may have lost.
#[test]
fn a_batch_stop_after_a_statement_made_is_its_fsync_failure() {
    let s = slow_disk_server(true);
    s.h.db
        .write()
        .unwrap()
        .execute(&fennec_ql::parse_one("create collection bf (n int)").unwrap())
        .unwrap();
    s.open();
    let lines = "{\"query\": \"insert bf {id: 1}\"}\n\
                 {\"query\": \"insert bf {id: 1}\"}\n\
                 {\"query\": \"compact bf\"}";
    let (status, _, body) = s.send("/batch", String::new(), lines).join().unwrap();
    assert_eq!(status, 500, "{body}");
    assert!(!body.contains("completed"), "{body}");
    assert!(s.h.db.read().unwrap().failure().is_some());
}

/// A block a quota refuses as it lands is refused by what other writes
/// left -- the rows another landed -- and is answered once those are
/// durable, as every other such refusal: a `/batch`'s and an MCP call
/// tool's, whose block lands at `mcp::land`. The quota is armed once the
/// first write landed, which it would have refused too.
#[test]
fn a_quota_refused_at_land_is_answered_once_what_it_read_is_on_disk() {
    let armed = Arc::new(AtomicBool::new(false));
    let cfg = Config {
        sync_on_write: true,
        token: Some("root".into()),
        mcp: Some(Arc::new(fennec_http::mcp::Options {
            write: true,
            calls_per_sec: 0,
            ..fennec_http::mcp::Options::default()
        })),
        ..Config::default()
    };
    let s = slow_disk_server_cfg(false, cfg);
    {
        let mut g = s.h.db.write().unwrap();
        for q in [
            "create collection ql (n int)",
            "create statement grow() public as insert ql {n: 3}",
        ] {
            g.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        let on = Arc::clone(&armed);
        g.set_limit(Some(Arc::new(
            move |_: &Database, l: &fennec_core::engine::Landing| match on.load(Ordering::SeqCst)
                && l.grows
            {
                true => Err("rows: at most 1".into()),
                false => Ok(()),
            },
        )));
    }
    const AUTH: &str = "Authorization: Bearer root\r\n";
    const MCP: &str = "Authorization: Bearer root\r\nMCP-Protocol-Version: 2026-07-28\r\n\
                       Mcp-Method: tools/call\r\nMcp-Name: call_grow\r\n\
                       Content-Type: application/json\r\n\
                       Accept: application/json, text/event-stream\r\n";
    const CALL: &str = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"call_grow","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"}}}}"#;
    for (what, refused) in [
        (
            "/batch",
            ("/batch", AUTH, r#"{"query": "insert ql {n: 2}"}"#),
        ),
        ("an MCP call tool", ("/_mcp", MCP, CALL)),
    ] {
        armed.store(false, Ordering::SeqCst);
        let reached = s.gate.0.lock().unwrap().reached;
        let (asked, replays) = s.counts();
        s.hold_for(u64::MAX);
        let made = s.send(
            "/query",
            AUTH.to_string(),
            r#"{"query": "insert ql {n: 1}"}"#,
        );
        s.reached(reached + 1);
        armed.store(true, Ordering::SeqCst);
        let answer = s.send(refused.0, refused.1.to_string(), refused.2);
        let early = s.replayed_or_waiting(asked + 2, replays);
        s.open();
        let (status, _, body) = made.join().unwrap();
        assert!(status < 300, "{what}: the first write {status} {body}");
        let (status, _, told) = answer.join().unwrap();
        // A tool's refusal is its result's, the transport's answer a 200.
        let refused = status == 507 || told.contains(r#""status":507"#);
        assert!(refused, "{what}: {status} {told}");
        assert!(!early, "{what}: refused before the fsync of what it read");
    }
    assert!(!s.timed_out(), "an fsync waited out the gate");
}

/// A refusal whose reading fsync fails is no refusal: an `insert` of an id
/// another write holds, that write's fsync failing, is answered the 500 the
/// failure is -- whether the row is there, no one here knows -- as the first
/// write is, never the 409 of a row the disk may have lost. Its durability
/// fails as the disk's failure sticks, running no fsync of its own.
#[test]
fn a_refusal_read_from_a_write_whose_fsync_failed_is_no_refusal() {
    let s = slow_disk_server(true);
    s.h.db
        .write()
        .unwrap()
        .execute(&fennec_ql::parse_one("create collection d (n int)").unwrap())
        .unwrap();
    let (asked, replays) = s.counts();
    let before = s.fsyncs.load(Ordering::SeqCst);
    s.hold_for(u64::MAX);
    let first = s.send("/query", String::new(), r#"{"query": "insert d {id: 7}"}"#);
    s.reached(1);
    let refused = s.send("/query", String::new(), r#"{"query": "insert d {id: 7}"}"#);
    let early = s.replayed_or_waiting(asked + 2, replays);
    s.open();
    assert!(!early, "refused before the fsync of what it read");
    let (status, _, made) = first.join().unwrap();
    assert_eq!(status, 500, "{made}");
    let (status, _, told) = refused.join().unwrap();
    assert_eq!(status, 500, "{told}");
    assert_eq!(
        s.fsyncs.load(Ordering::SeqCst),
        before + 1,
        "the failed fsync was tried again"
    );
}

/// The deepest expression the parser takes runs on a connection's stack:
/// one past the limit is a 400, one just under it an answer, and the
/// server goes on. A stack overflow is no panic but the process's abort.
#[test]
fn deep_expression_does_not_kill_the_server() {
    let h = start(Config::default(), Database::new());
    let mut c = Conn::open(h.port);
    ok(c.query("create collection t (n int)"));
    let nested = |d: usize| format!("get t where {}n = 1{}", "(".repeat(d), ")".repeat(d));

    let (status, body) = c.query(&nested(fennec_ql::MAX_EXPR_DEPTH + 64));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("too deep"), "{body}");

    ok(c.query(&nested(fennec_ql::MAX_EXPR_DEPTH - 1)));
    ok(c.query("collections"));
}

/// Reads take the shared lock: one held elsewhere does not keep a
/// client's read waiting.
#[test]
fn reads_share_the_lock() {
    let h = start(Config::default(), Database::new());
    let mut setup = Conn::open(h.port);
    ok(setup.query("create collection t (name text)"));
    ok(setup.query(r#"put t {name: "a"}"#));

    let held = h.db.read().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let d2 = Arc::clone(&done);
    let port = h.port;
    let t = std::thread::spawn(move || {
        let r = ok(Conn::open(port).query("get t select name"));
        d2.store(true, Ordering::SeqCst);
        r
    });
    let start = Instant::now();
    while !done.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        done.load(Ordering::SeqCst),
        "the client's read blocked while a read lock was held"
    );
    assert!(t.join().unwrap().contains(r#""name":"a""#));
    drop(held);
}

/// Searches running at once on many connections answer as they do one
/// after another.
#[test]
fn concurrent_vector_search_matches_serial() {
    let h = start(Config::default(), Database::new());
    let mut c = Conn::open(h.port);
    ok(c.query(
        "create collection v (name text, e vector<8> @hnsw(cosine, m=16, ef_construction=100))",
    ));
    let mut seed = 1u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((seed >> 33) as f32) / (u32::MAX as f32 / 2.0)
    };
    let mut docs = Vec::new();
    for i in 0..2000 {
        let v: Vec<String> = (0..8).map(|_| format!("{:.4}", rnd())).collect();
        docs.push(format!("{{name: \"v{i}\", e: [{}]}}", v.join(",")));
    }
    ok(c.query(&format!("put v [{}]", docs.join(", "))));

    let queries: Vec<String> = (0..12)
        .map(|_| {
            let v: Vec<String> = (0..8).map(|_| format!("{:.4}", rnd())).collect();
            format!("get v select name near e [{}] limit 5", v.join(","))
        })
        .collect();
    let serial: Vec<String> = queries.iter().map(|q| ok(c.query(q))).collect();
    let port = h.port;
    let parallel: Vec<String> = queries
        .iter()
        .cloned()
        .map(|q| std::thread::spawn(move || ok(Conn::open(port).query(&q))))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    for (i, (s, p)) in serial.iter().zip(&parallel).enumerate() {
        assert!(s.contains("\"name\""), "query {i} returned nothing: {s}");
        assert_eq!(
            s, p,
            "query {i}: the parallel search differs from the serial one"
        );
    }
}

/// Every connection is a thread: past the ceiling one is refused with 503,
/// the live one goes on, and its slot is free again once it closes.
#[test]
fn connection_limit_refuses_extra_connections() {
    let h = start(
        Config {
            max_connections: 1,
            ..Config::default()
        },
        Database::new(),
    );
    let mut first = Conn::open(h.port);
    ok(first.query("collections"));
    let (status, body) = Conn::open(h.port)
        .ask("GET", "/collections", "")
        .expect("the refusal is an answer");
    assert_eq!(status, 503, "{body}");
    assert!(body.ends_with(r#""retry_after":1}"#), "{body}");
    ok(first.query("collections"));

    drop(first);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Conn::open(h.port).ask("GET", "/collections", "") {
            Ok((200, _)) => break,
            other => {
                assert!(Instant::now() < deadline, "no slot freed up: {other:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Panics as a write reaches the collection `boom`: a connection's
/// handling panicking, its thread unwound, as `fennec-server`'s profile
/// lets a connection's thread unwind alone.
struct Boom;

impl fennec_core::plugin::Hook for Boom {
    fn name(&self) -> &str {
        "boom"
    }
    fn rewrites(&self) -> bool {
        false
    }
    fn before_write(
        &self,
        schema: &fennec_core::schema::Schema,
        _op: fennec_core::plugin::WriteOp,
        _doc: &mut Document,
    ) -> Result<()> {
        if schema.name == "boom" {
            panic!("a connection's handling panicking, as the test means it to");
        }
        Ok(())
    }
}

/// A connection whose handling panics gives its slot back as its thread
/// unwinds, before its socket closes: at a ceiling of one, connection
/// after connection panics, each served, and a request after them is
/// answered. The slot was given back by a line after the connection's
/// work, which a panic skipped: the first panic kept it for good, and
/// every connection after was refused 503.
#[test]
fn a_connection_whose_handling_panics_gives_its_slot_back() {
    let mut db = Database::new();
    db.registry_mut().register_hook(Arc::new(Boom));
    db.execute(&fennec_ql::parse_one("create collection boom (n int)").unwrap())
        .unwrap();
    let h = start(
        Config {
            max_connections: 1,
            ..Config::default()
        },
        db,
    );
    let body = r#"{"query": "put boom {n: 1}"}"#;
    for i in 0..5 {
        // The close is the event: read once the panicking thread let go
        // of its slot, so the next connection finds it free.
        let asked = Conn::open(h.port).ask("POST", "/query", body);
        match asked {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {}
            other => panic!("connection {i}: closed unanswered, not {other:?}"),
        }
    }
    let (status, rows) = Conn::open(h.port).query("get boom count");
    assert_eq!(status, 200, "{rows}");
    assert!(
        rows.contains(r#""count":0"#),
        "every write put back: {rows}"
    );
}

/// A keep-alive connection silent past `--idle-timeout` is closed, and
/// closed with nothing said: a `400 read error` written as it went was read
/// by Python's `http.client` as the answer to the next request it sent on
/// the connection, a `put` that never ran.
#[test]
fn idle_timeout_closes_the_connection_without_a_word() {
    let h = start(
        Config {
            idle_timeout: Some(Duration::from_millis(200)),
            ..Config::default()
        },
        Database::new(),
    );
    let mut c = Conn::open(h.port);
    ok(c.query("collections"));
    // The server's close is the event waited for: the read ends at it, and
    // the client's own read timeout ([`ANSWER_WITHIN`]) bounds only a
    // server that never closes.
    let mut sent = Vec::new();
    let read = c.r.read_to_end(&mut sent);
    assert!(read.is_ok(), "the connection was kept: {read:?}");
    assert_eq!(
        String::from_utf8_lossy(&sent),
        "",
        "bytes no request asked for"
    );

    // A connection opened and never written to is closed the same way.
    let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut sent = Vec::new();
    assert!(s.read_to_end(&mut sent).is_ok());
    assert!(sent.is_empty(), "{}", String::from_utf8_lossy(&sent));

    // A request cut off part way is answered, since one was asked: 408.
    let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(b"GET /collections HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    let mut sent = String::new();
    let _ = s.read_to_string(&mut sent);
    assert!(sent.starts_with("HTTP/1.1 408"), "{sent}");
}

/// A request whose first `sent` bytes go at once, then `trickled` a byte
/// every `every`, until the server answers and ends the connection: its
/// answer, and how long after the first byte the end came. The trickle
/// stops at the answer or after 30 s, a bound on a server that never cuts.
fn trickled(port: u16, sent: &[u8], trickled: Vec<u8>, every: Duration) -> (String, Duration) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
    let started = Instant::now();
    s.write_all(sent).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (mut w, stopped) = (s.try_clone().unwrap(), Arc::clone(&stop));
    let trickle = std::thread::spawn(move || {
        for b in trickled {
            std::thread::sleep(every);
            if stopped.load(Ordering::SeqCst)
                || started.elapsed() > Duration::from_secs(30)
                || w.write_all(&[b]).is_err()
            {
                return;
            }
        }
    });
    let mut answer = Vec::new();
    let _ = s.read_to_end(&mut answer);
    let took = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    trickle.join().unwrap();
    (String::from_utf8_lossy(&answer).into_owned(), took)
}

/// The slot of a ceiling of one given back: a request on a connection of
/// its own answered 200, each refused one (503) asked again, 30 s at most.
fn served_again(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match Conn::open(port).ask("GET", "/collections", "") {
            Ok((200, _)) => return,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "the slot was never given back: {other:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// A head trickled a byte at a time, each well inside the idle timeout, is
/// cut at `--header-timeout` from its first byte: 408, the connection
/// ended and its slot given back. The idle timeout bounded each read alone,
/// so a client sending a byte a while held its thread and its
/// `--max-connections` slot as long as it liked.
#[test]
fn a_trickled_head_is_cut_at_its_deadline() {
    let within = Duration::from_millis(600);
    let h = start(
        Config {
            max_connections: 1,
            header_timeout: Some(within),
            ..Config::default()
        },
        Database::new(),
    );
    let header = b"X-Trickled: ".iter().copied().chain([b'a'; 600]).collect();
    let (answer, took) = trickled(
        h.port,
        b"GET /collections HTTP/1.1\r\nHost: localhost\r\n",
        header,
        Duration::from_millis(50),
    );
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(took >= within, "{took:?}");
    served_again(h.port);
}

/// A body trickled a byte at a time is cut as a head is: at
/// `--header-timeout` from the request's first byte and a second more for
/// each KiB that came (`conn::MIN_RATE`), head and body alike, from the
/// first byte. A body coming at twice that rate is read whole, short of
/// 64 KiB as past it: a 100 KiB write from a 32 kbit/s link was cut at ten
/// seconds where the first 64 KiB earned nothing.
#[test]
fn a_trickled_body_is_cut() {
    use fennec_http::conn::MIN_RATE;
    let within = Duration::from_millis(600);
    let h = start(
        Config {
            max_connections: 1,
            header_timeout: Some(within),
            ..Config::default()
        },
        Database::new(),
    );
    let every = Duration::from_millis(50);
    let declared = |len: usize| {
        format!("POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Length: {len}\r\n\r\n")
            .into_bytes()
    };
    // From the start.
    let (answer, took) = trickled(h.port, &declared(1 << 20), vec![b' '; 600], every);
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(took >= within, "{took:?}");
    served_again(h.port);

    // A KiB of it at once: a second more earned.
    let mut sent = declared(1 << 20);
    sent.resize(sent.len() + MIN_RATE as usize, b' ');
    let (answer, took) = trickled(h.port, &sent, vec![b' '; 600], every);
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(took >= within + Duration::from_secs(1), "{took:?}");
    served_again(h.port);

    // At twice the rate: read whole and answered, its 10 KiB short of the
    // 64 KiB a head may be. Its first 2 KiB at once, then 128 bytes every
    // 62.5 ms for four seconds: the time its bytes earn runs ahead of them
    // by `within` and the two seconds the first 2 KiB earned, and grows, so
    // a stall of this client's on a loaded machine has seconds to spare.
    // A statement padded with the whitespace JSON allows before its `}`.
    let len = 10 * MIN_RATE as usize;
    let ahead = 2 * MIN_RATE as usize;
    let mut body = br#"{"query":"collections""#.to_vec();
    body.resize(len - 1, b' ');
    body.push(b'}');
    let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
    s.write_all(&declared(len)).unwrap();
    s.write_all(&body[..ahead]).unwrap();
    let paced = Instant::now();
    for (i, part) in body[ahead..].chunks(MIN_RATE as usize / 8).enumerate() {
        // Each part at its own time: a sleep a loaded machine stretched is
        // never carried on to the next.
        let at = paced + Duration::from_millis(1000 / 16) * (i as u32 + 1);
        std::thread::sleep(at.saturating_duration_since(Instant::now()));
        s.write_all(part).unwrap();
    }
    let mut answer = String::new();
    let mut r = BufReader::new(s);
    r.read_line(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
}

/// An answer its client never reads ends its connection once a write of it
/// takes nothing for `--write-timeout`, and gives its slot back: a client
/// that asked for answers past what the sockets hold and read none held
/// its thread and its `--max-connections` slot for good.
#[test]
fn an_unread_answer_frees_its_slot() {
    let mut db = Database::new();
    db.execute(&fennec_ql::parse_one("create collection t (s text)").unwrap())
        .unwrap();
    let row = format!("put t {{id: 1, s: '{}'}}", "x".repeat(1 << 20));
    db.execute(&fennec_ql::parse_one(&row).unwrap()).unwrap();
    let write_within = Duration::from_millis(300);
    let h = start(
        Config {
            max_connections: 1,
            write_timeout: Some(write_within),
            ..Config::default()
        },
        db,
    );
    // Forty answers of a mebibyte asked at once, and none read.
    let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    s.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
    let started = Instant::now();
    s.write_all(&b"GET /t/1 HTTP/1.1\r\nHost: localhost\r\n\r\n".repeat(40))
        .unwrap();
    served_again(h.port);
    assert!(started.elapsed() >= write_within, "{:?}", started.elapsed());
    // The unread client's connection was ended: what was sent is read off,
    // then its end.
    let mut rest = Vec::new();
    let read = s.read_to_end(&mut rest);
    assert!(
        !matches!(&read, Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)),
        "{read:?}"
    );
    assert!(rest.len() < 40 << 20, "{}", rest.len());
}

/// A `--header-timeout` past what the clock can name is no bound, never a
/// panic at `+` in the connection's thread: a body read in many reads, the
/// first of them 8 KiB at most, is answered.
#[test]
fn a_header_timeout_past_the_clock_is_none() {
    let h = start(
        Config {
            header_timeout: Some(Duration::MAX),
            ..Config::default()
        },
        Database::new(),
    );
    let mut body = String::from(r#"{"query":"collections""#);
    body.extend(std::iter::repeat_n(' ', (1 << 20) - body.len() - 1));
    body.push('}');
    assert_eq!(
        Conn::open(h.port).ask("POST", "/query", &body).unwrap().0,
        200
    );
}

/// `GET /_health` answers with no token and takes no lock: a probe that
/// queried would wait out a long `compact`, and a healthy server would look
/// dead.
#[test]
fn health_answers_with_no_token_and_no_lock() {
    let h = start(
        Config {
            token: Some("secret".into()),
            ..Config::default()
        },
        Database::new(),
    );
    let held = h.db.write().unwrap();
    let (status, body) = Conn::open(h.port)
        .ask("GET", "/_health", "")
        .expect("an answer");
    assert_eq!((status, body.as_str()), (200, r#"{"ok":true}"#));
    drop(held);
    let (status, _) = Conn::open(h.port).ask("GET", "/collections", "").unwrap();
    assert_eq!(status, 401, "the rest still wants the token");
}

/// A read sent with `If-None-Match` is tagged with the change it answered
/// at, and the tag sent back is answered 304 -- the query not run -- until
/// a write touches a collection it reads; a read whose answer moves with
/// the clock is never tagged.
#[test]
fn a_polled_read_is_answered_304_until_what_it_reads_is_written() {
    let h = start(Config::default(), Database::new());
    let mut c = Conn::open(h.port);
    for q in [
        "create collection stock (sku text @hash, n int)",
        "create collection other (x int)",
        "create collection seen (at timestamp @ttl(1h))",
        r#"put stock {sku: "a", n: 3}"#,
    ] {
        ok(c.query(q));
    }
    let ask = |tag: &str, q: &str| -> (u16, Option<String>, String) {
        let mut s = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
        let body = format!("{{\"query\": {}}}", json_string(q));
        write!(
            s,
            "POST /query HTTP/1.1\r\nHost: localhost\r\nIf-None-Match: {tag}\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        let etag = head
            .lines()
            .find_map(|l| l.strip_prefix("ETag: ").map(str::to_string));
        (head[9..12].parse().unwrap(), etag, body.to_string())
    };
    let read = r#"get stock where sku = "a""#;
    let (status, tag, body) = ask("\"0\"", read);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""n":3"#), "{body}");
    let tag = tag.expect("tagged");
    let (status, again, body) = ask(&tag, read);
    assert_eq!((status, body.as_str()), (304, ""));
    assert_eq!(again.as_deref(), Some(tag.as_str()));
    // A write elsewhere leaves it as it was.
    ok(c.query("put other {x: 1}"));
    assert_eq!(ask(&tag, read).0, 304);
    // One to what it reads answers it again, under a new tag.
    ok(c.query(r#"set stock {n: 2} where sku = "a""#));
    let (status, fresh, body) = ask(&tag, read);
    assert_eq!(status, 200);
    assert!(body.contains(r#""n":2"#), "{body}");
    assert_ne!(fresh, Some(tag));
    // Without the header, no tag; rows that expire, or `now()`, none either.
    let (_, none, _) = ask("\"0\"", "get seen");
    assert_eq!(none, None);
    let (_, none, _) = ask("\"0\"", "get stock where n < now()");
    assert_eq!(none, None);
}

// ------------------------------------------------------------ webhook events

/// The secret hook `stripe` of these servers verifies Stripe's events
/// with.
const HOOK_SECRET: &str = "whsec_held_fsync_tests";

/// Hook `stripe`, as a hooks file names it.
fn stripe_hooks() -> Arc<fennec_http::hooks::Hooks> {
    let read = |_: &str| Ok(HOOK_SECRET.as_bytes().to_vec());
    Arc::new(fennec_http::hooks::Hooks::read("stripe stripe s\n", read, 64 << 20).unwrap())
}

/// The orders, and the statement taking hook `stripe`'s payments that
/// pays one, defined over `db`.
fn hooked(db: &mut Database) {
    // A definition is read back into its tree by the process's reader,
    // which `Server::new` installs only once `db` is defined: without one
    // here, `paid` is held unread and every event 409 with no fsync. Run
    // alone, the tests over a slow disk failed so on every system, an
    // earlier test in the binary having installed it in the suite.
    fennec_ql::install();
    db.allow_hooks();
    for q in [
        "create collection orders (ref text @hash, status text, intent text)",
        "put orders {ref: 'o-1', status: 'open'}",
        "put orders {ref: 'o-2', status: 'open'}",
        "create statement paid($intent text from event.data.object.id, $order text from \
         event.data.object.metadata.order) on hook stripe as set orders {status: 'paid', \
         intent: $intent} where ref = $order require 1",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
}

/// Event `id` paying `order`: its body, and the header lines its delivery
/// carries, signed now as Stripe signs one.
fn stripe_event(id: &str, order: &str) -> (&'static str, String) {
    let body = format!(
        r#"{{"id":"{id}","type":"payment_intent.succeeded","data":{{"object":{{"id":"pi_{id}","metadata":{{"order":"{order}"}}}}}}}}"#
    );
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mac =
        fennec_http::crypto::hmac_sha256(HOOK_SECRET.as_bytes(), format!("{t}.{body}").as_bytes());
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    (
        Box::leak(body.into_boxed_str()),
        format!("Content-Type: application/json\r\nStripe-Signature: t={t},v1={hex}\r\n"),
    )
}

/// A server over a slow disk taking hook `stripe`'s events, its other
/// writes durable (`--sync always`) or buffered as `durable` says.
fn slow_disk_hooked(durable: bool) -> SlowDiskServer {
    let fsyncs = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
    let mut db = Database::with_sink(Box::new(SlowDisk {
        fail: false,
        failed: Arc::new(AtomicBool::new(false)),
        fsyncs: Arc::clone(&fsyncs),
        appended: Arc::new(AtomicU64::new(0)),
        synced: Arc::new(Mutex::new(0)),
        gate: Arc::clone(&gate),
    }));
    hooked(&mut db);
    let cfg = Config {
        sync_on_write: durable,
        hooks: Some(stripe_hooks()),
        ..Config::default()
    };
    let h = start_with(cfg, db, false);
    SlowDiskServer { h, fsyncs, gate }
}

impl SlowDiskServer {
    /// Waits until `n` fsyncs in all reached the gate or an answer past
    /// `replays` came, whichever first; whether one came.
    fn reached_or_answered(&self, n: usize, replays: usize) -> bool {
        let (lock, cv) = &*self.gate;
        let (g, waited) = cv
            .wait_timeout_while(lock.lock().unwrap(), GATE_LIMIT, |g| {
                g.reached < n && g.replays <= replays
            })
            .unwrap();
        assert!(!waited.timed_out(), "neither the fsync nor the answer came");
        g.replays > replays
    }
}

/// A webhook event is answered 2xx once its block is on disk, whatever
/// `--sync` says: its sender never sends it again, so a crash after a
/// buffered 2xx would lose a payment no one tells the server of. Held by
/// order: the fsync is held at the gate, and the answer comes only once it
/// is let go -- while a buffered write beside it asks no fsync at all.
#[test]
fn an_event_is_answered_once_on_disk_whatever_sync_says() {
    for durable in [false, true] {
        let s = slow_disk_hooked(durable);
        if !durable {
            let before = s.fsyncs.load(Ordering::SeqCst);
            ok(Conn::open(s.h.port).query("put orders {ref: 'o-9', status: 'open'}"));
            assert_eq!(
                s.fsyncs.load(Ordering::SeqCst),
                before,
                "a buffered write synced"
            );
        }
        let (body, extra) = stripe_event("evt_1", "o-1");
        let reached = s.gate.0.lock().unwrap().reached;
        let (_, replays) = s.counts();
        s.hold_for(u64::MAX);
        let answer = s.send("/_hooks/stripe", extra, body);
        assert!(
            !s.reached_or_answered(reached + 1, replays),
            "durable {durable}: the event was answered before its fsync"
        );
        s.open();
        let (status, replayed, said) = answer.join().unwrap();
        assert_eq!(
            (status, replayed, said.as_str()),
            (200, false, r#"{"event":"evt_1","taken":1}"#)
        );
        assert!(!s.timed_out());
    }
}

/// A duplicate of an event sent while the first taking's fsync runs --
/// Stripe's retry after a slow answer, a duplicate delivery -- finds its
/// key under the read lock and is answered once that fsync is done, never
/// before, as the first is: answered at once, a power loss after took back
/// a payment its sender held as delivered and never sent again. Whatever
/// `--sync` says.
#[test]
fn a_duplicate_event_waits_for_its_first_takings_fsync() {
    for durable in [false, true] {
        let s = slow_disk_hooked(durable);
        let (body, extra) = stripe_event("evt_2", "o-2");
        let reached = s.gate.0.lock().unwrap().reached;
        let (asked, replays) = s.counts();
        s.hold_for(u64::MAX);
        let first = s.send("/_hooks/stripe", extra.clone(), body);
        s.reached(reached + 1);
        let again = s.send("/_hooks/stripe", extra, body);
        assert!(
            !s.replayed_or_waiting(asked + 2, replays),
            "durable {durable}: the duplicate was answered before the first's fsync"
        );
        s.open();
        let taken = r#"{"event":"evt_2","taken":1}"#;
        let (status, replayed, said) = first.join().unwrap();
        assert_eq!((status, replayed, said.as_str()), (200, false, taken));
        let (status, replayed, said) = again.join().unwrap();
        assert_eq!((status, replayed, said.as_str()), (200, true, taken));
        assert!(!s.timed_out());
    }
}

/// The child of [`an_event_a_killed_process_landed_is_taken_once`]: the
/// file it opens.
const HOOK_KILLED: &str = "FENNEC_TEST_HOOK_KILLED_BEFORE_FSYNC";

/// An event a process killed between its landing and its fsync took is
/// answered nothing by it; sent again to a server opening the file, it is
/// taken once -- answered from its key once the file's durability ran, the
/// block being in the file the kill left -- never twice, and never before
/// that durability. The test binary run again as the process killed,
/// under `--sync 250`: an event is durable whatever `--sync` says.
#[test]
fn an_event_a_killed_process_landed_is_taken_once() {
    fennec_ql::install();
    let (body, _) = stripe_event("evt_k", "o-1");
    if let Ok(p) = std::env::var(HOOK_KILLED) {
        let db = fennec_core::fs::open_with(
            &p,
            true,
            Box::new(
                |inner| Ok(Box::new(DiesAtFsync(inner)) as Box<dyn fennec_core::engine::Sink>),
            ),
        )
        .unwrap();
        let cfg = Config {
            hooks: Some(stripe_hooks()),
            ..Config::default()
        };
        let h = start(cfg, db);
        let (_, extra) = stripe_event("evt_k", "o-1");
        let answer = Conn::open(h.port).sent("/_hooks/stripe", &extra, body);
        panic!("answered before its fsync: {answer:?}");
    }
    let path = tmp("hook-killed.fennec");
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        hooked(&mut db);
        db.sync().unwrap();
    }
    let before = std::fs::metadata(&path).unwrap().len();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "server::an_event_a_killed_process_landed_is_taken_once",
            "--exact",
            "--test-threads=1",
        ])
        .env(HOOK_KILLED, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(KILLED_CODE),
        "the child was not killed at its fsync"
    );
    assert!(
        std::fs::metadata(&path).unwrap().len() > before,
        "the killed process's block is not in the file"
    );

    let gate: ReopenedGate = Arc::default();
    let held = Arc::clone(&gate);
    let db = fennec_core::fs::open_serving(
        &path,
        true,
        Box::new(move |inner| {
            Ok(Box::new(Held { inner, gate: held }) as Box<dyn fennec_core::engine::Sink>)
        }),
    )
    .unwrap();
    let cfg = Config {
        hooks: Some(stripe_hooks()),
        ..Config::default()
    };
    let h = start(cfg, db);
    let (port, told) = (h.port, Arc::clone(&gate));
    let (_, extra) = stripe_event("evt_k", "o-1");
    let resent = std::thread::spawn(move || {
        let answer = Conn::open(port).sent("/_hooks/stripe", &extra, body);
        told.0.lock().unwrap().events.push("answered");
        told.1.notify_all();
        answer
    });
    {
        let (m, cv) = &*gate;
        let (mut g, waited) = cv
            .wait_timeout_while(m.lock().unwrap(), GATE_LIMIT, |g| g.events.is_empty())
            .unwrap();
        assert!(
            !waited.timed_out(),
            "the resent event asked for no durability"
        );
        assert_eq!(g.events, ["asked"], "answered before its durability");
        g.open = true;
        cv.notify_all();
    }
    let (status, replayed, said) = resent.join().unwrap();
    assert_eq!(
        (status, replayed, said.as_str()),
        (200, true, r#"{"event":"evt_k","taken":1}"#)
    );
    assert_eq!(
        gate.0.lock().unwrap().events,
        ["asked", "durable", "answered"]
    );
    let mut c = Conn::open(h.port);
    let (_, paid) = c.query("get orders count where status = 'paid'");
    assert!(paid.contains(r#""count":1"#), "{paid}");
    let (_, keys) = c.query("get _hook_events count");
    assert!(keys.contains(r#""count":1"#), "{keys}");
}
