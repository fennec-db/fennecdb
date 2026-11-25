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
use std::sync::atomic::{
    AtomicIsize, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Counting;

/// The count of every thread's allocations under way, by its number (0:
/// none), the bytes the blocks made since it began hold now, and the most
/// they held. Each count takes a number of its own ([`count_all`]), which
/// every block made carries before it ([`TAG`]): a block made before the
/// count, or in another, is neither added nor taken away as it is let go
/// of, and moved, it is the count's from there. Taken away, as every block
/// was until 2026-11-25, what was let go of in a count lowered its peak by
/// as much whenever it was made: the servers of the 8 000 rows' move, which
/// serve on through the 32 000 rows', or a block the setup made, and
/// fennec-http's `message_peak.rs`, which counted the same way, read a
/// replica's growth of 20 bytes a row as 64.12 on macOS CI once a stream
/// an earlier test left let go of its 1 MiB writer in its count.
static ALL: AtomicU64 = AtomicU64::new(0);
static COUNTS: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

/// `grow` bytes more held by the count under way, `held` of them at once at
/// the most -- a move holds the old block and the new together.
fn counted(grow: isize, held: isize) {
    let live = LIVE.fetch_add(grow, Relaxed);
    PEAK.fetch_max(live + held.max(grow), Relaxed);
}

/// The bytes before every block, the number of the count it was made in
/// (or 0) in the last 8 of them; a block aligned past them has its
/// alignment's bytes before it.
const TAG: usize = 16;

/// The bytes before a block of `l`, and the layout of the block the
/// system's allocator is asked for: those and the block.
fn tagged(l: Layout) -> Option<(usize, Layout)> {
    let before = l.align().max(TAG);
    let outer = Layout::from_size_align(l.size().checked_add(before)?, before).ok()?;
    Some((before, outer))
}

/// [`tagged`]'s answer for the layout of a block this allocator made.
///
/// # Safety
///
/// `l` is the layout a block this allocator handed out was made with.
unsafe fn tagged_made(l: Layout) -> (usize, Layout) {
    let before = l.align().max(TAG);
    // SAFETY: `tagged` made this layout of `l` as the block was made,
    // which `Layout::from_size_align` took.
    (before, unsafe {
        Layout::from_size_align_unchecked(l.size() + before, before)
    })
}

/// The block `before` bytes into `base`, carrying the count `n`; null where
/// `base` is.
///
/// # Safety
///
/// `base` is null or a block of a layout [`tagged`] gave, `before` the
/// bytes it gave before it.
unsafe fn marked(base: *mut u8, before: usize, n: u64) -> *mut u8 {
    if base.is_null() {
        return base;
    }
    // SAFETY: `before` is a power of two of 16 or more and the block's
    // alignment, so the block's start and the 8 bytes before it are within
    // what `System` made, and aligned for a `u64`.
    unsafe {
        let p = base.add(before);
        p.cast::<u64>().sub(1).write(n);
        p
    }
}

/// The count the block at `p` was made in, or 0.
///
/// # Safety
///
/// `p` is a block this allocator handed out.
unsafe fn tag_of(p: *mut u8) -> u64 {
    // SAFETY: the 8 bytes before every block this allocator hands out are
    // its tag, aligned for a `u64` ([`marked`]).
    unsafe { p.cast::<u64>().sub(1).read() }
}

// SAFETY: every call is handed to the system's allocator for a block of the
// layout `tagged` gives, the caller's block within it; the counting reads
// and writes the counts' atomics alone, allocating nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let Some((before, outer)) = tagged(l) else {
            return std::ptr::null_mut();
        };
        let now = ALL.load(Acquire);
        if now != 0 {
            counted(l.size() as isize, 0);
        }
        // SAFETY: `outer` is of a nonzero size, `l`'s and the tag's, and the
        // block `System` makes of it is what `marked` takes.
        unsafe { marked(System.alloc(outer), before, now) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let Some((before, outer)) = tagged(l) else {
            return std::ptr::null_mut();
        };
        let now = ALL.load(Acquire);
        if now != 0 {
            counted(l.size() as isize, 0);
        }
        // SAFETY: as `alloc`'s.
        unsafe { marked(System.alloc_zeroed(outer), before, now) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` is a block this allocator made with `l`, as
        // `GlobalAlloc::dealloc`'s caller guarantees.
        let ((before, outer), made) = unsafe { (tagged_made(l), tag_of(p)) };
        let now = ALL.load(Acquire);
        if now != 0 && made == now {
            counted(-(l.size() as isize), 0);
        }
        // SAFETY: `before` bytes before `p` is the block `System` made of
        // `outer` ([`marked`]).
        unsafe { System.dealloc(p.sub(before), outer) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, to: usize) -> *mut u8 {
        let Some((before, _)) = Layout::from_size_align(to, l.align()).ok().and_then(tagged) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `p` is a block this allocator made with `l`, as
        // `GlobalAlloc::realloc`'s caller guarantees.
        let ((_, outer), made) = unsafe { (tagged_made(l), tag_of(p)) };
        let now = ALL.load(Acquire);
        if now != 0 {
            match made == now {
                true => counted(to as isize - l.size() as isize, to as isize),
                // Made before the count: the block it moves into is the
                // count's.
                false => counted(to as isize, to as isize),
            }
        }
        // SAFETY: `before` bytes before `p` is the block `System` made of
        // `outer`, and `to` and the tag's bytes are nonzero and fit `isize`
        // once rounded to `before`, which `tagged` took; the block it moves
        // into is what `marked` takes.
        unsafe {
            marked(
                System.realloc(p.sub(before), outer, to + before),
                before,
                now,
            )
        }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Counts every thread's allocations from here, the bytes the blocks made
/// since hold and the most they held at once ([`peak_now`]), under a number
/// no count before took ([`ALL`]).
///
/// The number is stored with `Release` and read with `Acquire` by the
/// allocator: a thread that sees the new number sees the reset before it,
/// or on aarch64 a block it made would be wiped by the reset and its free
/// in the count still taken away.
fn count_all() {
    ALL.store(0, Relaxed);
    LIVE.store(0, Relaxed);
    PEAK.store(0, Relaxed);
    ALL.store(COUNTS.fetch_add(1, Relaxed) + 1, Release);
}

/// Stops the count [`count_all`] began: the most the blocks every thread
/// made since held at once.
fn peak_now() -> isize {
    ALL.store(0, Relaxed);
    PEAK.load(Relaxed)
}

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
        count_all();
        let r = call(port, "POST", "/_shard/tenants/big/move", r#"{"to":"n2"}"#);
        let peak = peak_now();
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
    // 20 bytes a row measured (3 401 121 bytes at 8 000 rows, 3 881 456 at
    // 32 000): never the image, some 1 000 bytes a row.
    assert!(
        peaks[1] - peaks[0] <= 96 * 24_000,
        "{peaks:?}: grown by more than 96 bytes a row"
    );
}
