//! The most memory a primary's message holds as a replica reads it: the
//! allocator's high-water mark, counted by an allocator of this binary's
//! own on the reading thread alone, against the message's length and
//! against what had come of it. `replication::read_kind` grows a body as
//! its bytes come, doubling; a growth holds the bytes that came and the
//! buffer twice as long the allocator moves them into, counted here as
//! moved whether or not the allocator grew it in place. Its doc comment
//! and docs/design/replication.md give these numbers.
//!
//! And the most a primary holds as it sends a replica its first image,
//! counted on every thread of the process: a constant, however large the
//! database grows (`PinnedImage`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

struct Counting;

thread_local! {
    /// Whether this thread's allocations are counted, the bytes they hold
    /// now and the most they held.
    static ON: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

fn counted(f: impl FnOnce(usize, usize) -> (usize, usize)) {
    let _ = ON.try_with(|on| {
        if on.get() {
            let (live, peak) = f(LIVE.get(), PEAK.get());
            LIVE.set(live);
            PEAK.set(peak.max(live));
        }
    });
}

/// Whether every thread's allocations are counted, the bytes they hold now
/// past what they held as the count began (less where they let go of
/// older ones), and the most they held.
static ALL: AtomicBool = AtomicBool::new(false);
static ALL_LIVE: AtomicIsize = AtomicIsize::new(0);
static ALL_PEAK: AtomicIsize = AtomicIsize::new(0);

/// `grow` bytes more held, `held` of them at once at the most -- a move
/// holds the old block and the new together.
fn counted_all(grow: isize, held: isize) {
    if ALL.load(Relaxed) {
        let live = ALL_LIVE.fetch_add(grow, Relaxed);
        ALL_PEAK.fetch_max(live + held.max(grow), Relaxed);
    }
}

// SAFETY: every call is handed to the system's allocator as it came; the
// counting reads and writes this thread's cells alone, allocating nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        counted(|live, peak| (live + l.size(), peak));
        counted_all(l.size() as isize, 0);
        // SAFETY: `l` is this call's layout, of a nonzero size as
        // `GlobalAlloc::alloc`'s caller guarantees, handed on as it came.
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        counted(|live, peak| (live + l.size(), peak));
        counted_all(l.size() as isize, 0);
        // SAFETY: as `alloc`'s.
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        counted(|live, peak| (live.saturating_sub(l.size()), peak));
        counted_all(-(l.size() as isize), 0);
        // SAFETY: `p` is `System`'s, made with `l` -- every call here hands
        // on to it -- as `GlobalAlloc::dealloc`'s caller guarantees.
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, to: usize) -> *mut u8 {
        // Moved, the old block and the new are held at once.
        counted(|live, peak| (live - l.size().min(live) + to, peak.max(live + to)));
        counted_all(to as isize - l.size() as isize, to as isize);
        // SAFETY: `p` is `System`'s, made with `l`, and `to` is nonzero and
        // fits `isize` once rounded to `l.align()`, as
        // `GlobalAlloc::realloc`'s caller guarantees.
        unsafe { System.realloc(p, l, to) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Held by each test while it counts: a count of every thread's bytes
/// would count the other test's.
static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The most bytes `f` held at once on this thread.
fn peak_of(f: impl FnOnce()) -> usize {
    LIVE.set(0);
    PEAK.set(0);
    ON.set(true);
    f();
    ON.set(false);
    PEAK.get()
}

const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\r\n";

/// A primary's answer: its head, a hello promising an image, and an image
/// announced `len` bytes long, `sent` of them sent.
fn answer(len: usize, sent: usize) -> Vec<u8> {
    let mut hello = vec![1u8, 1];
    for v in [1u64, 1, 0] {
        hello.extend_from_slice(&v.to_le_bytes());
    }
    let lineage = fennec_core::history::History {
        lineage: vec![((7u64 << 32) | 1, 0)],
        following: false,
    };
    hello.extend_from_slice(&lineage.encode());
    let mut out = HEAD.to_vec();
    out.push(b'H');
    out.extend_from_slice(&(hello.len() as u64).to_le_bytes());
    out.extend_from_slice(&hello);
    out.push(b'I');
    out.extend_from_slice(&(len as u64).to_le_bytes());
    out.resize(out.len() + sent, 0xab);
    out
}

/// The most a message read whole held, against its length: twice it at
/// the most, 1.8 times for an image of 40 MiB, and twice for one a byte
/// past where the last growth begins -- `FIRST` (2 MiB + 16) doubled twice,
/// and one byte. A message cut short held three times what had come of it
/// as it grew at the most: an image of a terabyte announced and 3 MiB sent,
/// refused as cut short, grew once, from `FIRST`. The few bytes besides --
/// the head's line, the hello -- are let go before the image is read.
#[test]
fn a_message_holds_twice_its_length_and_three_times_what_came() {
    let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    const FIRST: usize = (2 << 20) + 16;
    const BESIDE: usize = 4 << 10;
    for len in [40 << 20, 4 * FIRST + 1] {
        let bytes = answer(len, len);
        let mut n = 0;
        let peak = peak_of(|| {
            let (read, end) = fennec_http::replication::read_answer(&mut &bytes[..], None);
            assert_eq!(end.kind(), std::io::ErrorKind::UnexpectedEof, "{end}");
            n = read;
        });
        assert_eq!(n, 2);
        let factor = peak as f64 / len as f64;
        eprintln!("a message of {len} bytes held {peak} at the most: {factor:.3} times it");
        assert!(
            peak <= 2 * len + BESIDE,
            "{peak} bytes held for a message of {len}"
        );
    }
    let sent = 3 << 20;
    let bytes = answer(1 << 40, sent);
    let peak = peak_of(|| {
        let (_, end) = fennec_http::replication::read_answer(&mut &bytes[..], None);
        assert_eq!(end.kind(), std::io::ErrorKind::UnexpectedEof, "{end}");
    });
    eprintln!(
        "a terabyte announced, {sent} bytes sent, held {peak} at the most: {:.3} times it",
        peak as f64 / sent as f64
    );
    assert!(
        peak <= 3 * FIRST + BESIDE,
        "{peak} bytes held for {FIRST} come as it grew"
    );
}

const TOKEN: &str = "image-peak-replication-token";

/// A primary over a file of `rows` documents of a kilobyte each, durable,
/// served on a port of its own: its port and the database's image's
/// length.
fn primary(rows: usize) -> (u16, usize) {
    use fennec_http::replication::{self, Replication};
    let dir = std::env::temp_dir().join(format!("fennec-image-peak-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("primary-{rows}.fennec"));
    let _ = std::fs::remove_file(&file);
    let (mut db, feed) = replication::open(file.to_str().unwrap(), 1 << 20).unwrap();
    db.fork(replication::fresh_id()).unwrap();
    let parse = |q: &str| fennec_ql::parse_one(q).unwrap();
    db.execute(&parse("create collection docs (n int @sorted, body text)"))
        .unwrap();
    let put = parse("put docs {n: $1, body: $2}");
    for at in (0..rows).step_by(1_000) {
        db.begin().unwrap();
        for n in at..rows.min(at + 1_000) {
            let body = format!("{n:08}{}", "x".repeat(992));
            let args = [
                fennec_core::value::Value::Int(n as i64),
                fennec_core::value::Value::Text(body),
            ];
            db.execute_with(&put, &args).unwrap();
        }
        db.commit().unwrap();
    }
    db.sync().unwrap();
    let image = db.snapshot().len();
    let db = std::sync::Arc::new(std::sync::RwLock::new(db));
    let cfg = fennec_http::Config {
        addr: "127.0.0.1:0".into(),
        sync_on_write: true,
        ..fennec_http::Config::default()
    };
    let server = std::sync::Arc::new(
        fennec_http::Server::new(db, cfg).with_replication(Replication::new(
            Some(TOKEN.into()),
            Some(feed),
            None,
        )),
    );
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    (port, image)
}

/// A replica's first follow of the primary on `port`, `sums` or not: the
/// image's length, read and let go of a buffer at a time, and the most
/// every thread of the process held at once past what it held as the
/// request went out -- the primary's connection taking and sending the
/// image above all.
fn followed(port: u16, sums: bool) -> (u64, isize) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    let mut buf = [0u8; 1 << 16];
    ALL_LIVE.store(0, Relaxed);
    ALL_PEAK.store(0, Relaxed);
    ALL.store(true, Relaxed);
    write!(
        s,
        "GET /_replication?since=0&sums={} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {TOKEN}\r\n\r\n",
        u8::from(sums)
    )
    .unwrap();
    // The answer's head, a byte at a time.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut buf[..1]).unwrap();
        head.push(buf[0]);
    }
    assert!(
        head.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    let image = loop {
        let mut m = [0u8; 9];
        s.read_exact(&mut m).unwrap();
        let mut left = u64::from_le_bytes(m[1..].try_into().unwrap());
        let len = left;
        while left > 0 {
            let n = (left as usize).min(buf.len());
            s.read_exact(&mut buf[..n]).unwrap();
            left -= n as u64;
        }
        if m[0] == b'I' {
            break len;
        }
    };
    ALL.store(false, Relaxed);
    (image, ALL_PEAK.load(Relaxed))
}

/// The most a primary holds as it sends a replica its first image is a
/// constant, the same for a database four times as large: the image is
/// pinned (`Database::pin_image`) -- the id index copied, 12 bytes a row,
/// the frames' index, the documents shared with the file and the segments
/// -- and written out as it is read. Taken whole, as every release from
/// 0.1.5 to 0.2.0 took it, it held the image and more: 8 and 33 MB here,
/// and a database larger than the server's memory killed it at its first
/// follow. Checked and plain (a follower that reads no sums) alike.
#[test]
fn a_primary_sends_an_image_holding_a_constant_however_large_the_database() {
    let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    const HELD: isize = 3 << 20;
    for rows in [8_000, 32_000] {
        let (port, len) = primary(rows);
        for sums in [true, false] {
            let (image, peak) = followed(port, sums);
            if sums {
                assert_eq!(image, len as u64, "the image is the snapshot's length");
            }
            eprintln!(
                "{rows} rows, an image of {image} bytes{}: {peak} held at the most",
                if sums { "" } else { " (plain)" }
            );
            assert!(
                peak <= HELD,
                "{rows} rows: the primary held {peak} bytes sending an image of {image}"
            );
        }
    }
}
