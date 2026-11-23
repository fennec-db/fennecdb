//! The most a tenant's move holds -- its source sending the image, the
//! router relaying it, its target taking it in -- counted on every thread
//! of the process, the router and both nodes in it: a constant, the same
//! for a tenant four times as large. The source writes the image from a
//! pin, the router relays it as it comes, and the target writes it beside
//! the tenant's file as it arrives, checked as it streams, and opens it
//! from there mapped. Read whole by each of them, as every release to 0.2.0
//! read it, the move held the tenant five times over and more.

use fennec_http::tenants::Tenants;
use fennec_shard::directory::Directory;
use fennec_shard::{Config, Router};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Counting;

/// Whether every thread's allocations are counted, the bytes they hold now
/// past what they held as the count began, and the most they held.
static ALL: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

/// `grow` bytes more held, `held` of them at once at the most -- a move
/// holds the old block and the new together.
fn counted(grow: isize, held: isize) {
    if ALL.load(Relaxed) {
        let live = LIVE.fetch_add(grow, Relaxed);
        PEAK.fetch_max(live + held.max(grow), Relaxed);
    }
}

// SAFETY: every call is handed to the system's allocator as it came; the
// counting reads and writes atomics alone, allocating nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        counted(l.size() as isize, 0);
        // SAFETY: `l` is this call's layout, of a nonzero size as
        // `GlobalAlloc::alloc`'s caller guarantees, handed on as it came.
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        counted(l.size() as isize, 0);
        // SAFETY: as `alloc`'s.
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        counted(-(l.size() as isize), 0);
        // SAFETY: `p` is `System`'s, made with `l` -- every call here hands
        // on to it -- as `GlobalAlloc::dealloc`'s caller guarantees.
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, to: usize) -> *mut u8 {
        counted(to as isize - l.size() as isize, to as isize);
        // SAFETY: `p` is `System`'s, made with `l`, and `to` is nonzero and
        // fits `isize` once rounded to `l.align()`, as
        // `GlobalAlloc::realloc`'s caller guarantees.
        unsafe { System.realloc(p, l, to) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

struct Node {
    addr: String,
    dir: PathBuf,
    tenants: Arc<Tenants>,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn node(tag: &str) -> Node {
    fennec_http::audit::set_delay(0);
    let dir = std::env::temp_dir().join(format!(
        "fennec-move-peak-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let tenants = Arc::new(Tenants::new(&dir).unwrap());
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        admin_token: Some(format!("adm-{tag}")),
        ..fennec_http::Config::default()
    };
    let server = fennec_http::Server::with_tenants(Arc::clone(&tenants), cfg);
    let listener = server.bind().unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    Node { addr, dir, tenants }
}

fn call(port: u16, method: &str, target: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let req = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: \
         {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}

/// The rows of `docs` in `tenant` on `node`.
fn rows_on(node: &Node, tenant: &str) -> usize {
    let t = node.tenants.get(tenant).unwrap();
    let q = fennec_ql::parse_one("get docs select n").unwrap();
    let db = t.db.read().unwrap();
    db.query(&q, &[])
        .unwrap()
        .rows()
        .map_or(0, |r| r.rows.len())
}

#[test]
fn a_move_holds_a_constant_however_large_the_tenant() {
    const HELD: isize = 12 << 20;
    let mut peaks = Vec::new();
    for rows in [8_000usize, 32_000] {
        let nodes = [node(&format!("a{rows}")), node(&format!("b{rows}"))];
        let router = Router::new(
            Directory::in_memory(),
            Config {
                addr: "127.0.0.1:0".into(),
                upstream_timeout: Duration::from_secs(60),
                ..Config::default()
            },
        );
        let listener = router.bind().unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _ = router.serve_on(listener);
        });
        for (i, nd) in nodes.iter().enumerate() {
            let tag = if i == 0 { 'a' } else { 'b' };
            let body = format!(r#"{{"addr":"{}","token":"adm-{tag}{rows}"}}"#, nd.addr);
            let r = call(port, "PUT", &format!("/_shard/nodes/n{}", i + 1), &body);
            assert_eq!(r.0, 201, "{}", r.1);
        }
        let r = call(port, "PUT", "/_shard/tenants/big", r#"{"node":"n1"}"#);
        assert_eq!(r.0, 201, "{}", r.1);
        {
            let t = nodes[0].tenants.get("big").unwrap();
            let mut db = t.db.write().unwrap();
            let parse = |q: &str| fennec_ql::parse_one(q).unwrap();
            db.execute(&parse("create collection docs (n int @sorted, body text)"))
                .unwrap();
            let put = parse("put docs {n: $1, body: $2}");
            for at in (0..rows).step_by(1_000) {
                db.begin().unwrap();
                for n in at..rows.min(at + 1_000) {
                    let args = [
                        fennec_core::value::Value::Int(n as i64),
                        fennec_core::value::Value::Text(format!("{n:08}{}", "x".repeat(992))),
                    ];
                    db.execute_with(&put, &args).unwrap();
                }
                db.commit().unwrap();
            }
            db.sync().unwrap();
        }
        LIVE.store(0, Relaxed);
        PEAK.store(0, Relaxed);
        ALL.store(true, Relaxed);
        let r = call(port, "POST", "/_shard/tenants/big/move", r#"{"to":"n2"}"#);
        ALL.store(false, Relaxed);
        let peak = PEAK.load(Relaxed);
        assert_eq!(r.0, 200, "{}", r.1);
        assert_eq!(rows_on(&nodes[1], "big"), rows, "every row moved");
        eprintln!(
            "a move of {rows} rows: {peak} bytes held at the most ({})",
            r.1
        );
        assert!(
            peak <= HELD,
            "{rows} rows: {peak} bytes held moving the tenant"
        );
        peaks.push(peak);
    }
    // What grows with the rows is the source's pin and the target's open,
    // a few dozen bytes a row: never the image.
    assert!(
        peaks[1] - peaks[0] <= 96 * 24_000,
        "{peaks:?}: grown by more than 96 bytes a row"
    );
}
