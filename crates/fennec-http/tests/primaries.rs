//! What a replica reads from its primary, held against a stand-in that
//! answers at the primary's address with bytes no primary of this code
//! sends (G14, area D): a count of writes past its message, a length no
//! message of its kind reaches, a head without an end, a panic in the
//! image's path. Whoever answers at the address a replica follows -- or
//! stands between them over plain HTTP -- sends what they like; each is
//! refused in words, the reader's thread living on, and nothing waits for
//! ever on a follower: a halt, a promotion, a tenant's release and the
//! node's shutdown each end. And a tenant's copy is promoted by the node's
//! admin alone, never through `/t/<t>/_replication/promote`; a copy behind
//! its primary hears how far, and one no follower runs for is counted.

use fennec_core::engine::Watcher;
use fennec_core::prelude::*;
use fennec_http::archive::{self, Archive};
use fennec_http::replication::{self, fresh_id, Follower, Message, Replication, Upstream};
use fennec_http::tenants::{Replicated, Tenants};
use fennec_http::{Config, Server};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

const TOKEN: &str = "replication-secret";
const ADMIN: &str = "Bearer adm";

/// How long a test waits for what must happen: a reconnect, a halt, an
/// answer. A follower's back-off is 5 s at most, its silence 10 s.
const WITHIN: Duration = Duration::from_secs(30);

/// A stream's answer as a primary writes its head.
const ANSWER: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\
Cache-Control: no-cache, no-transform\r\nConnection: close\r\n\r\n";

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fennec-primaries-{tag}-{}-{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stand-in primary on a port of its own: each connection's request read
/// to the end of its head, its number (from 0) said on the channel, then
/// `serve` handed it. What `serve` does with the connection -- writes,
/// holds it, drops it -- is the test's.
fn stand_in(
    serve: impl Fn(usize, TcpStream) + Send + Sync + 'static,
) -> (String, mpsc::Receiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let serve = Arc::new(serve);
    std::thread::spawn(move || {
        for (n, s) in listener.incoming().enumerate() {
            let Ok(mut s) = s else { return };
            let (serve, tx) = (Arc::clone(&serve), tx.clone());
            std::thread::spawn(move || {
                s.set_read_timeout(Some(WITHIN)).unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match s.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                let _ = tx.send(n);
                serve(n, s);
            });
        }
    });
    (url, rx)
}

/// The `n`th connection's number, within [`WITHIN`].
fn connection(rx: &mpsc::Receiver<usize>, n: usize, what: &str) {
    let deadline = Instant::now() + WITHIN;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(k) if k >= n => return,
            Ok(_) => {}
            Err(_) => panic!("{what}: connection {n} never came within {WITHIN:?}"),
        }
    }
}

/// A message as a primary frames one: its kind, its length, its body.
fn message(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&(body.len() as u64).to_le_bytes());
    m.extend_from_slice(body);
    m
}

/// A message's head alone, announcing `len` bytes.
fn announced(tag: u8, len: u64) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&len.to_le_bytes());
    m
}

/// A primary's hello: at change `seq`, an image to follow or not.
fn hello(image: bool, seq: u64) -> Vec<u8> {
    let mut b = vec![1, image as u8];
    for v in [seq, seq, 0u64] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let lineage = History {
        lineage: vec![((7u64 << 32) | 1, 0)],
        following: false,
    };
    b.extend_from_slice(&lineage.encode());
    message(b'H', &b)
}

/// A `W` message counting 2^61 writes: `16 + 8 * n` wraps to 16 in a
/// release build, which a body of 32 bytes passes.
fn wrapping_writes(first: u64) -> Vec<u8> {
    let mut b = first.to_le_bytes().to_vec();
    b.extend_from_slice(&(1u64 << 61).to_le_bytes());
    b.extend_from_slice(&[0u8; 16]);
    message(b'W', &b)
}

/// A database's image as a primary sends one, and the change it is at.
fn image() -> (Vec<u8>, u64) {
    let mut db = Database::new();
    for q in [
        "create collection c (x int)",
        "put c {x: 1}",
        "put c {x: 2}",
    ] {
        db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
    }
    (db.snapshot(), db.change_seq())
}

/// A thread's end: what it answered, its panic named as one.
fn joined<T>(what: &str, h: std::thread::JoinHandle<T>) -> T {
    match h.join() {
        Ok(t) => t,
        Err(p) => {
            let words = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            panic!("{what} panicked on the stand-in's bytes: {words}")
        }
    }
}

/// `f` run on a thread of its own, its panic caught: what a reader of the
/// stand-in's bytes did, a panic named as one.
fn unpanicked<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    joined(what, std::thread::spawn(f))
}

/// The stream's messages after its hello, read until one is refused.
fn refused_after_hello(url: &str) -> String {
    let upstream = Upstream::new(url, TOKEN.into()).unwrap();
    let mut rx = upstream.open(0, 0, false).expect("the head");
    assert!(matches!(rx.receive(), Ok(Message::Hello { .. })));
    loop {
        match rx.receive() {
            Ok(_) => {}
            Err(e) => return e.to_string(),
        }
    }
}

/// A `W` message's count of writes is held to the bytes the message holds:
/// 2^61 of them wrapped `16 + 8 * n` in a release build and had the reader
/// allocate 2^61 times, a panic of the follower's thread -- in a debug
/// build the multiplication's own -- and in `fennec archive`, built with
/// `panic = abort`, the process's end. Refused in words, the reader lives.
#[test]
fn a_count_of_writes_past_its_message_is_refused_in_words() {
    let (url, _) = stand_in(|_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(false, 0));
        out.extend_from_slice(&wrapping_writes(1));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let why = unpanicked("the receiver", move || refused_after_hello(&url));
    assert!(why.contains("writes"), "{why}");
}

/// A message's length is held to what its kind can be before a byte of it
/// is allocated: a keep-alive is 24 bytes, a hello a lineage's few, and a
/// length of a terabyte announced for one is refused naming the bound, the
/// connection never read further.
#[test]
fn a_length_no_message_of_its_kind_reaches_is_refused_by_its_bound() {
    let (url, _) = stand_in(|_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(false, 0));
        out.extend_from_slice(&announced(b'K', 1 << 40));
        out.extend_from_slice(&[0u8; 64]);
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let why = unpanicked("the receiver", move || refused_after_hello(&url));
    assert!(why.contains("1048576"), "the bound named: {why}");
}

/// A status line with no end, and a header line with none, are refused at
/// the head's bound, the server's own (`http::MAX_HEADER_BYTES`): read on,
/// a replica's memory grew with every byte a stand-in sent it. The
/// stand-in writes until its writes fail -- the replica gone -- and that
/// is long before the 64 MiB it would write to a reader with no bound.
#[test]
fn a_head_without_an_end_is_refused_at_its_bound() {
    for (case, start) in [
        ("a status line", &b"HTTP/1.1 200 "[..]),
        ("a header line", &b"HTTP/1.1 200 OK\r\nX-Endless: "[..]),
    ] {
        let sent = Arc::new(AtomicUsize::new(0));
        let (done_tx, done) = mpsc::channel();
        let counted = Arc::clone(&sent);
        let (url, _) = stand_in(move |_, mut s| {
            let chunk = vec![b'a'; 64 << 10];
            let mut total = 0;
            let _ = s.write_all(start);
            while total < 64 << 20 {
                if s.write_all(&chunk).is_err() {
                    break;
                }
                total += chunk.len();
            }
            counted.store(total, Ordering::SeqCst);
            let _ = done_tx.send(());
            std::thread::sleep(WITHIN);
        });
        let opened = unpanicked("the receiver", move || {
            Upstream::new(&url, TOKEN.into())
                .unwrap()
                .open(0, 0, false)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        let why = opened.expect_err("an answer whose head never ends");
        assert!(why.contains("head"), "{case}: {why}");
        done.recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("{case}: the stand-in never stopped writing"));
        let total = sent.load(Ordering::SeqCst);
        assert!(
            total < 16 << 20,
            "{case}: the replica read {total} bytes of a head"
        );
    }
}

/// `fennec backup` reads a hello and an image; `fennec archive` the writes
/// after them too. Each is built with `panic = abort`: a primary's message
/// that panicked the reader ended the process. Here each meets a count
/// past its message and a head with no end, and says so.
#[test]
fn a_backup_and_an_archive_refuse_a_primarys_bad_bytes_and_live() {
    // A backup of a stand-in whose hello promises an image of a terabyte,
    // sending a few bytes of it: refused as cut short, a few mebibytes
    // held at the most -- the whole terabyte was asked of the allocator
    // before a byte of it came, which Linux refuses, and a refused
    // allocation aborts the process.
    let (url, _) = stand_in(|_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(true, 0));
        out.extend_from_slice(&announced(b'I', 1 << 40));
        out.extend_from_slice(&[0u8; 1024]);
        let _ = s.write_all(&out);
    });
    let dir = scratch("backup");
    let out = dir.join("taken.fennec");
    let taken = unpanicked("the backup", move || {
        archive::backup(&Upstream::new(&url, TOKEN.into()).unwrap(), &out, None)
            .map_err(|e| e.to_string())
    });
    let why = taken.expect_err("an image cut short");
    assert!(why.contains("1099511627776"), "the length named: {why}");

    // An archiver over a stand-in sending an image and then a count of
    // writes past its message: the session refuses it, says so, and the
    // archiver connects again -- to nothing, the stand-in hanging up --
    // until it is told to stop.
    let (image, seq) = image();
    let (url, _) = stand_in(move |n, mut s| {
        if n > 0 {
            return;
        }
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(true, seq));
        out.extend_from_slice(&message(b'I', &image));
        out.extend_from_slice(&wrapping_writes(seq + 1));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let arch = scratch("archive");
    let stop = Arc::new(AtomicBool::new(false));
    let said = Arc::new(Mutex::new(Vec::<String>::new()));
    let (flag, heard) = (Arc::clone(&stop), Arc::clone(&said));
    let upstream = Upstream::new(&url, TOKEN.into()).unwrap();
    let archiver = std::thread::spawn(move || {
        Archive::new(&arch)
            .unwrap()
            .follow(&upstream, &flag, &|line| {
                heard.lock().unwrap().push(line.to_string())
            })
            .map_err(|e| e.to_string())
    });
    let deadline = Instant::now() + WITHIN;
    while !said.lock().unwrap().iter().any(|l| l.contains("writes")) {
        if archiver.is_finished() || Instant::now() > deadline {
            stop.store(true, Ordering::SeqCst);
            let ended = joined("the archiver", archiver);
            panic!(
                "the archiver never said the refusal: {ended:?}, {:?}",
                said.lock().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::SeqCst);
    let ended = joined("the archiver", archiver);
    assert!(ended.is_ok(), "{ended:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A replica's database over a file, made to follow, and its follower of
/// `url` -- not started.
fn follower_of(
    url: &str,
    tag: &str,
) -> (
    Arc<RwLock<Database>>,
    Arc<replication::Feed>,
    Arc<Follower>,
    PathBuf,
) {
    let dir = scratch(tag);
    let file = dir.join("replica.fennec");
    let (mut db, feed) =
        replication::open(file.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    let lineage = db.history().lineage.clone();
    db.follow(lineage).unwrap();
    let db = Arc::new(RwLock::new(db));
    let f = Follower::new(
        url,
        TOKEN.into(),
        Arc::clone(&db),
        Some(Arc::clone(&feed)),
        true,
    )
    .unwrap();
    (db, feed, f, dir)
}

/// `f.halt()` on a thread of its own, waited for [`WITHIN`].
fn halted(f: &Arc<Follower>, what: &str) {
    let (tx, rx) = mpsc::channel();
    let f = Arc::clone(f);
    std::thread::spawn(move || {
        f.halt();
        let _ = tx.send(());
    });
    rx.recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("{what}: the halt did not return within {WITHIN:?}"));
}

/// A follower meeting a count of writes past its message connects again,
/// as after any stream that broke, and halts: its thread, dead of the
/// panic, never connected again, and the halt -- a promotion's -- waited
/// for it for good.
#[test]
fn a_follower_refusing_a_message_connects_again_and_halts() {
    let (url, connections) = stand_in(|_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(false, 0));
        out.extend_from_slice(&wrapping_writes(1));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let (_db, _feed, f, dir) = follower_of(&url, "wraps");
    f.start("fennec-test-wraps".into()).unwrap();
    connection(
        &connections,
        1,
        "the follower, after a count past its message",
    );
    assert!(f.running());
    halted(&f, "a follower that refused a message");
    assert!(!f.running());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A watcher that panics once armed: a panic inside `Database::adopt`,
/// under the write lock, after the image replaced the file.
struct Boom(AtomicBool);

impl Watcher for Boom {
    fn notify(&self, _seq: u64) {
        if self.0.swap(false, Ordering::SeqCst) {
            panic!("a watcher's panic as the copy adopted its image");
        }
    }
}

/// A panic in the image's path with the copy's write lock held -- the file
/// replaced, the memory part way -- leaves a copy no one can trust: it is
/// failed in words, its status saying so, its promotion refused, and its
/// thread ends, so a halt returns. The thread used to end with nothing
/// set: the copy served what it held for good, `running` said it ran, and
/// a halt waited for ever.
#[test]
fn a_panic_adopting_an_image_fails_the_copy_in_words() {
    let (image, seq) = image();
    let (url, _) = stand_in(move |_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(true, seq));
        out.extend_from_slice(&message(b'I', &image));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let (db, feed, f, dir) = follower_of(&url, "adopt");
    db.write()
        .unwrap()
        .set_watcher(Arc::new(Boom(AtomicBool::new(true))));
    f.start("fennec-test-adopt".into()).unwrap();
    let deadline = Instant::now() + WITHIN;
    while f.running() {
        assert!(
            Instant::now() < deadline,
            "the follower's thread ran on, or ended unsaid, {WITHIN:?} after the panic"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let failure = db
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .failure()
        .map(String::from)
        .expect("the copy failed");
    assert!(failure.contains("panic"), "{failure}");
    halted(&f, "a follower whose copy failed");
    // Its status says so, and its promotion is refused.
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        ..Config::default()
    };
    let server = Server::new(Arc::clone(&db), cfg).with_replication(Replication::new(
        Some(TOKEN.into()),
        Some(feed),
        Some(Arc::clone(&f)),
    ));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let auth = format!("Bearer {TOKEN}");
    let status = call(
        port,
        "GET",
        "/_replication/status",
        &[("Authorization", &auth)],
        b"",
    );
    assert_eq!(status.status, 200, "{}", status.body);
    assert!(status.body.contains("panic"), "{}", status.body);
    assert!(f.promote(fresh_id()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A panic in a session with no lock held -- here the hello's callback,
/// as a tenant's marks its copy heard -- changed nothing of the copy: the
/// session failed, and the follower connects again and follows.
#[test]
fn a_panic_outside_the_copy_is_a_session_that_failed() {
    let (url, connections) = stand_in(|_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(false, 0));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let (_db, _feed, f, dir) = follower_of(&url, "hello");
    let once = AtomicBool::new(true);
    f.on_hello(Box::new(move |_| {
        if once.swap(false, Ordering::SeqCst) {
            panic!("a hello's callback panicking");
        }
    }));
    f.start("fennec-test-hello".into()).unwrap();
    connection(
        &connections,
        1,
        "the follower, after a panic in its session",
    );
    let deadline = Instant::now() + WITHIN;
    while !f.progress().0 {
        assert!(
            Instant::now() < deadline,
            "the follower never followed again"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    halted(&f, "a follower following again");
    let _ = std::fs::remove_dir_all(&dir);
}

struct Res {
    status: u16,
    body: String,
}

fn call(port: u16, method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(WITHIN)).unwrap();
    let mut req = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let Some(split) = out.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Res {
            status: 0,
            body: format!("no answer within {WITHIN:?}"),
        };
    };
    let head = String::from_utf8_lossy(&out[..split]).into_owned();
    let status = (head.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Res {
        status,
        body: String::from_utf8_lossy(&out[split + 4..]).into_owned(),
    }
}

struct Node {
    port: u16,
    tenants: Arc<Tenants>,
    dir: PathBuf,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A node of tenants replicating them, its admin token `adm`.
fn node(tag: &str) -> Node {
    node_with(tag, 1 << 20)
}

/// [`node`], each tenant's feed holding `buffer` bytes of writes.
fn node_with(tag: &str, buffer: usize) -> Node {
    fennec_http::audit::set_delay(0);
    let dir = scratch(tag);
    let tenants = Arc::new(
        Tenants::new(&dir)
            .expect("dir")
            .with_replication(Replicated {
                token: TOKEN.into(),
                buffer,
                upstream: None,
                sync_on_write: true,
            }),
    );
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some("adm".into()),
        sync_on_write: true,
        ..Config::default()
    };
    let server = Server::with_tenants(Arc::clone(&tenants), cfg);
    let listener = server.bind().expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { port, tenants, dir }
}

fn admin(port: u16, method: &str, target: &str, body: &str) -> Res {
    call(
        port,
        method,
        target,
        &[("Authorization", ADMIN)],
        body.as_bytes(),
    )
}

fn query(port: u16, tenant: &str, q: &str) -> Res {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, q);
    body.push('}');
    call(
        port,
        "POST",
        &format!("/t/{tenant}/query"),
        &[],
        body.as_bytes(),
    )
}

/// A tenant `acme` holding `c {x: 1}`.
fn tenant(port: u16) {
    let made = admin(port, "PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    for q in ["create collection c (x int)", "put c {x: 1}"] {
        let r = query(port, "acme", q);
        assert_eq!(r.status, 200, "{q}: {}", r.body);
    }
}

/// Waits for `acme` on `node` to hold `x`.
fn holds(node: &Node, x: i64) {
    let deadline = Instant::now() + WITHIN;
    loop {
        let t = node.tenants.get("acme").unwrap();
        let q = fennec_ql::parse_one(&format!("get c count where x = {x}")).unwrap();
        // No collection `c` until the copy took its primary's image.
        let r = t.db.read().unwrap().query(&q, &[]);
        let r = r.map(|r| fennec_core::json::response_to_string(&r));
        if r.is_ok_and(|r| r.contains(r#""count":1"#)) {
            return;
        }
        assert!(Instant::now() < deadline, "x = {x} never reached the copy");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A tenant's copy is promoted by the node's admin, through the checks
/// that make a promotion one -- the lease's fence, the lineage and history
/// it is held to, the identity, the node's stream, the audit -- never by
/// the replication token at `/t/<t>/_replication/promote`, which forked a
/// standby past all of them, unknown to the router. That path is 404, as
/// any a node does not serve, and the copy follows on.
#[test]
fn a_tenants_copy_is_promoted_by_the_admin_alone() {
    let primary = node("promote-primary");
    let copy = node("promote-copy");
    tenant(primary.port);
    let made = admin(copy.port, "PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    let from = format!(r#"{{"from":"http://127.0.0.1:{}"}}"#, primary.port);
    let followed = admin(copy.port, "POST", "/_admin/tenants/acme/follow", &from);
    assert_eq!(followed.status, 200, "{}", followed.body);
    holds(&copy, 1);

    let auth = format!("Bearer {TOKEN}");
    let bypass = call(
        copy.port,
        "POST",
        "/t/acme/_replication/promote",
        &[("Authorization", &auth)],
        b"",
    );
    assert_eq!(bypass.status, 404, "{}", bypass.body);
    let status = call(
        copy.port,
        "GET",
        "/t/acme/_replication/status",
        &[("Authorization", &auth)],
        b"",
    );
    assert!(
        status.body.contains(r#""role":"replica""#),
        "{}",
        status.body
    );
    let put = query(primary.port, "acme", "put c {x: 2}");
    assert_eq!(put.status, 200, "{}", put.body);
    holds(&copy, 2);

    // Promoted by its admin, it says what it knew of its primary, as its
    // status names it: connected, nothing behind, heard from a moment ago.
    // A router's failover logs it and counts a copy promoted not connected.
    let promoted = admin(copy.port, "POST", "/_admin/tenants/acme/promote", "");
    assert_eq!(promoted.status, 200, "{}", promoted.body);
    let fields = fennec_core::json::parse_object(&promoted.body).unwrap();
    let field = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(
        field("connected"),
        Some(Value::Bool(true)),
        "{}",
        promoted.body
    );
    assert_eq!(field("behind"), Some(Value::Int(0)), "{}", promoted.body);
    let heard = match field("last_contact_ms") {
        Some(Value::Int(ms)) => ms,
        _ => panic!("no last_contact_ms: {}", promoted.body),
    };
    // A keep-alive every 2 s: well within a test's wait.
    assert!((0..30_000).contains(&heard), "{}", promoted.body);
}

/// What a relay in front of a primary has passed on to the copy behind it,
/// and whether it holds.
#[derive(Default)]
struct Relayed {
    /// The copy's change as what was passed on leaves it: a hello's, then
    /// each `W`'s last write.
    at: u64,
    /// Whether the next keep-alive saying the primary's disk is past `at`
    /// holds the relay after it.
    armed: bool,
    /// Such a keep-alive passed on, the relay holding until it is taken:
    /// where the copy stood and where its primary said its disk did.
    ahead: Option<(u64, u64)>,
    /// Every keep-alive passed on since it was armed: `at` beside the disk.
    alive: Vec<(u64, u64)>,
}

type Gate = Arc<(Mutex<Relayed>, Condvar)>;

/// A relay on a port of its own in front of the node at `primary`: each
/// connection's request passed on, then the node's answer a message at a
/// time, the next read off the node only once the last was written to the
/// copy -- so while the copy reads nothing, or the relay holds, the node's
/// socket fills behind it as behind a copy that stopped reading.
fn relay(primary: u16) -> (String, Gate) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let gate = Gate::default();
    let g = Arc::clone(&gate);
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(copy) = s else { return };
            let g = Arc::clone(&g);
            std::thread::spawn(move || {
                let _ = relayed(copy, primary, &g);
            });
        }
    });
    (url, gate)
}

fn relayed(mut copy: TcpStream, primary: u16, gate: &Gate) -> std::io::Result<()> {
    copy.set_read_timeout(Some(WITHIN))?;
    let mut node = TcpStream::connect(("127.0.0.1", primary))?;
    node.set_read_timeout(Some(WITHIN))?;
    let head = |from: &mut TcpStream| -> std::io::Result<Vec<u8>> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            from.read_exact(&mut byte)?;
            head.push(byte[0]);
        }
        Ok(head)
    };
    // A stream's request is a GET: its head is all of it.
    let request = head(&mut copy)?;
    node.write_all(&request)?;
    let answer = head(&mut node)?;
    copy.write_all(&answer)?;
    if !answer.starts_with(b"HTTP/1.1 200") {
        return Ok(());
    }
    let (m, cv) = &**gate;
    loop {
        {
            let mut r = m.lock().unwrap();
            let deadline = Instant::now() + WITHIN;
            while r.ahead.is_some() && Instant::now() < deadline {
                r = cv.wait_timeout(r, WITHIN).unwrap().0;
            }
        }
        let mut msg = vec![0u8; 9];
        node.read_exact(&mut msg)?;
        let len = u64::from_le_bytes(msg[1..9].try_into().unwrap()) as usize;
        msg.resize(9 + len, 0);
        node.read_exact(&mut msg[9..])?;
        copy.write_all(&msg)?;
        let body = &msg[9..];
        let u64_at = |i: usize| u64::from_le_bytes(body[i..i + 8].try_into().unwrap());
        let mut r = m.lock().unwrap();
        match msg[0] {
            b'H' => r.at = u64_at(2),
            b'W' => r.at = u64_at(0) + u64_at(8) - 1,
            b'K' if r.armed => {
                let (at, durable) = (r.at, u64_at(8));
                r.alive.push((at, durable));
                if durable > at {
                    r.armed = false;
                    r.ahead = Some((at, durable));
                }
            }
            _ => {}
        }
        cv.notify_all();
    }
}

/// A number a JSON answer gives after `"key":`, the first.
fn number_after(text: &str, key: &str) -> Option<u64> {
    let at = text.find(&format!("\"{key}\":"))? + key.len() + 3;
    let digits: String = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// A copy behind its primary hears how far: a stream behind says where its
/// primary's disk stands every `ALIVE` too, after the writes it sends, so
/// the copy's `behind`, its node's `fennec_copies_behind`, a promotion's
/// answer and a router's line count the writes it lacks. Said only once a
/// stream had caught up, a copy as far behind as the primary's buffer --
/// its reads slower than the writes, its socket full -- read `behind` 0
/// throughout. The copy's reads stop on its own write lock, a read of which
/// the test holds, until the primary's stream has handed its socket
/// nothing for longer than `ALIVE`; a relay in front of the primary then
/// holds after the first keep-alive saying more than the copy has, so what
/// it heard is read before it catches up.
#[test]
fn a_copy_behind_its_primary_reads_how_far() {
    let primary = node_with("behind-primary", 64 << 20);
    let copy = node("behind-copy");
    tenant(primary.port);
    let big = query(primary.port, "acme", "create collection big (pad text)");
    assert_eq!(big.status, 200, "{}", big.body);
    let (url, gate) = relay(primary.port);
    let (m, cv) = &*gate;
    let made = admin(copy.port, "PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    let from = format!(r#"{{"from":"{url}"}}"#);
    let followed = admin(copy.port, "POST", "/_admin/tenants/acme/follow", &from);
    assert_eq!(followed.status, 200, "{}", followed.body);
    holds(&copy, 1);
    m.lock().unwrap().armed = true;

    // The copy stops reading as it lands the next write. The primary is
    // sent writes of a mebibyte each until its stream is held back -- its
    // socket, the relay's and the copy's full, three writes past what it
    // has handed its socket -- and has handed it nothing for longer than
    // `ALIVE`: whatever it last told the copy of its disk, it told before
    // that, and the next write it hands its socket is followed by where its
    // disk stands.
    let auth = format!("Bearer {TOKEN}");
    let sent_and_durable = || {
        let status = call(
            primary.port,
            "GET",
            "/t/acme/_replication/status",
            &[("Authorization", &auth)],
            b"",
        );
        let sent = number_after(&status.body, "sent");
        sent.zip(number_after(&status.body, "durable"))
    };
    let t = copy.tenants.get("acme").unwrap();
    let held = t.db.read().unwrap();
    let pad = "x".repeat(1 << 20);
    let mut writes = 0;
    let last = loop {
        assert!(
            writes < 48,
            "the primary's stream was never held back by {writes} MiB of writes"
        );
        let put = query(primary.port, "acme", &format!("put big {{pad: \"{pad}\"}}"));
        assert_eq!(put.status, 200, "{}", put.body);
        writes += 1;
        let Some((sent, durable)) = sent_and_durable() else {
            continue;
        };
        if durable < sent + 3 {
            continue;
        }
        std::thread::sleep(replication::ALIVE + Duration::from_millis(250));
        if sent_and_durable() == Some((sent, durable)) {
            break durable;
        }
    };
    drop(held);

    let deadline = Instant::now() + WITHIN;
    let (at, durable) = {
        let mut r = m.lock().unwrap();
        loop {
            if let Some(ahead) = r.ahead {
                break ahead;
            }
            assert!(
                r.at < last,
                "the copy was sent every write to change {last} and no keep-alive said \
                 where its primary's disk stood while it was behind: it read `behind` 0 \
                 throughout (keep-alives, the copy's change beside the disk: {:?})",
                r.alive
            );
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "nothing passed on within {WITHIN:?}");
            r = cv.wait_timeout(r, left).unwrap().0;
        }
    };
    assert_eq!(durable, last);
    let behind = durable - at;

    // The copy read it: its status and its node's scrape say how far.
    let reads = |want: u64| {
        let deadline = Instant::now() + WITHIN;
        loop {
            let status = call(
                copy.port,
                "GET",
                "/t/acme/_replication/status",
                &[("Authorization", &auth)],
                b"",
            );
            if number_after(&status.body, "behind") == Some(want) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the copy never read {want} behind: {}",
                status.body
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let scrape = admin(copy.port, "GET", "/_metrics", "");
        let line = format!("fennec_copies_behind {want}");
        assert!(
            scrape.body.lines().any(|l| l == line),
            "no `{line}` in the copy's node's scrape"
        );
    };
    reads(behind);

    // Let go, it catches up, and both say so.
    m.lock().unwrap().ahead = None;
    cv.notify_all();
    reads(0);
    holds_seq(&copy, last);
}

/// Waits for `acme` on `node` to stand at change `seq`.
fn holds_seq(node: &Node, seq: u64) {
    let deadline = Instant::now() + WITHIN;
    let t = node.tenants.get("acme").unwrap();
    while t.db.read().unwrap().change_seq() < seq {
        assert!(
            Instant::now() < deadline,
            "change {seq} never reached the copy"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A tenant's copy whose image's adoption panicked is failed in words, and
/// nothing about it waits on its follower: its reads are answered, the
/// admin's promotion answers -- refused, the copy failed -- and the node's
/// shutdown completes. Its follower's thread used to end unsaid, and a
/// promotion's halt, a release's under the tenant's slot, and every `get`
/// of the tenant after that waited for ever.
#[test]
fn a_tenants_copy_failed_by_a_panic_holds_nothing_up() {
    let n = node("adopt-tenant");
    tenant(n.port);
    let (image, seq) = image();
    let again = image.clone();
    let (go_tx, go) = mpsc::channel::<()>();
    let go = Mutex::new(go);
    let (url, connections) = stand_in(move |_, mut s| {
        let _ = go.lock().unwrap().recv_timeout(WITHIN);
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(true, seq));
        out.extend_from_slice(&message(b'I', &image));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let from = format!(r#"{{"from":"{url}"}}"#);
    let followed = admin(n.port, "POST", "/_admin/tenants/acme/follow", &from);
    assert_eq!(followed.status, 200, "{}", followed.body);
    connection(&connections, 0, "the tenant's follower");
    let t = n.tenants.get("acme").unwrap();
    t.db.write()
        .unwrap()
        .set_watcher(Arc::new(Boom(AtomicBool::new(true))));
    go_tx.send(()).unwrap();
    let deadline = Instant::now() + WITHIN;
    while t
        .db
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .failure()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the copy was never failed: its follower's thread ended unsaid"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(t);

    let read = query(n.port, "acme", "get c");
    assert_eq!(read.status, 200, "{}", read.body);
    let auth = format!("Bearer {TOKEN}");
    let status = call(
        n.port,
        "GET",
        "/t/acme/_replication/status",
        &[("Authorization", &auth)],
        b"",
    );
    assert!(status.body.contains("panic"), "{}", status.body);
    let promoted = admin(
        n.port,
        "POST",
        "/_admin/tenants/acme/promote",
        r#"{"force":true}"#,
    );
    assert_eq!(promoted.status, 500, "{}", promoted.body);
    assert!(promoted.body.contains("panic"), "{}", promoted.body);
    // Its follower ended: counted as a copy not following, and kept open,
    // idle or not. Closed, nothing opened it again but a request or a
    // failover, and the node's scrape, which reads its open tenants, no
    // longer counted it while it followed nothing.
    assert_eq!(n.tenants.stats().copies.disconnected, 1);
    assert_eq!(
        n.tenants.close_idle(Duration::ZERO),
        0,
        "the copy whose follower ended was closed as idle, and counted no more"
    );
    assert_eq!(n.tenants.stats().copies.disconnected, 1);

    // A release under the tenant's slot: made to follow another primary,
    // the failed copy is closed -- its follower halted, which ended -- and
    // opened again from its file, which takes that primary's image; then
    // deleted, halting the new follower. Neither waits on the thread the
    // panic ended, each answered within the call's bound.
    let (url, others) = stand_in(move |_, mut s| {
        let mut out = ANSWER.to_vec();
        out.extend_from_slice(&hello(true, seq));
        out.extend_from_slice(&message(b'I', &again));
        let _ = s.write_all(&out);
        std::thread::sleep(WITHIN);
    });
    let from = format!(r#"{{"from":"{url}"}}"#);
    let followed = admin(n.port, "POST", "/_admin/tenants/acme/follow", &from);
    assert_eq!(followed.status, 200, "{}", followed.body);
    connection(&others, 0, "the reopened copy's follower");
    holds(&n, 2);
    let deleted = admin(n.port, "DELETE", "/_admin/tenants/acme", "");
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    tenant(n.port);

    let (tx, rx) = mpsc::channel();
    let tenants = Arc::clone(&n.tenants);
    std::thread::spawn(move || {
        let _ = tx.send(tenants.shutdown_with(false));
    });
    rx.recv_timeout(WITHIN)
        .expect("the node's shutdown waited on the tenant's follower");
}
