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
//! database grows (`PinnedImage`). So too what takes one: a replica, which
//! writes it beside its file as it comes and loads it from there, `fennec
//! backup`, sealed or not, and an archive's consolidation, which opens its
//! end mapped and writes its image from a pin.

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

/// A primary's answer: its head, a hello, and a message of writes
/// announced `len` bytes long, `sent` of them sent -- its first and its
/// count of times zeros, a record's bytes after. An image is taken as it
/// comes, never read whole ([`a_replica_takes_an_image_holding_a_constant_however_large_the_database`]);
/// writes are, a block's record past 16 MB going alone however long.
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
    out.push(b'W');
    out.extend_from_slice(&(len as u64).to_le_bytes());
    let at = out.len();
    out.resize(at + sent, 0xab);
    out[at..at + 16].fill(0);
    out
}

/// The most a message read whole held, against its length: twice it at
/// the most, 1.8 times for one of 40 MiB, and twice for one a byte past
/// where the last growth begins -- `FIRST` (2 MiB + 16) doubled twice, and
/// one byte. A message cut short held three times what had come of it as
/// it grew at the most: one of a terabyte announced and 3 MiB sent,
/// refused as cut short, grew once, from `FIRST`. The few bytes besides --
/// the head's line, the hello -- are let go before the message is read.
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

/// This process's own directory for the tests' files.
fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fennec-image-peak-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Documents `from` to `to` put into `docs`, a kilobyte each, a thousand
/// a block, and made durable.
fn put_rows(db: &mut fennec_core::engine::Database, from: usize, to: usize) {
    let put = fennec_ql::parse_one("put docs {n: $1, body: $2}").unwrap();
    for at in (from..to).step_by(1_000) {
        db.begin().unwrap();
        for n in at..to.min(at + 1_000) {
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
}

/// The rows of `docs`.
fn rows_in(db: &fennec_core::engine::Database) -> usize {
    let q = fennec_ql::parse_one("get docs select n").unwrap();
    db.query(&q, &[])
        .unwrap()
        .rows()
        .map_or(0, |r| r.rows.len())
}

/// A primary over a file of `rows` documents of a kilobyte each, durable,
/// served on a port of its own: its port and the database's image's
/// length.
fn primary(rows: usize) -> (u16, usize) {
    let (port, image, _) = primary_named(&format!("primary-{rows}"), rows);
    (port, image)
}

/// [`primary`] over a file named `name`, and its database.
fn primary_named(
    name: &str,
    rows: usize,
) -> (
    u16,
    usize,
    std::sync::Arc<std::sync::RwLock<fennec_core::engine::Database>>,
) {
    use fennec_http::replication::{self, Replication};
    let file = scratch().join(format!("{name}.fennec"));
    let _ = std::fs::remove_file(&file);
    let (mut db, feed) = replication::open(file.to_str().unwrap(), 1 << 20).unwrap();
    db.fork(replication::fresh_id()).unwrap();
    let parse = |q: &str| fennec_ql::parse_one(q).unwrap();
    db.execute(&parse("create collection docs (n int @sorted, body text)"))
        .unwrap();
    put_rows(&mut db, 0, rows);
    let image = db.snapshot().len();
    let db = std::sync::Arc::new(std::sync::RwLock::new(db));
    let kept = std::sync::Arc::clone(&db);
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
    (port, image, kept)
}

/// Counts every thread's allocations from here, the bytes held and the most
/// held at once ([`peak_now`]).
fn count_all() {
    ALL_LIVE.store(0, Relaxed);
    ALL_PEAK.store(0, Relaxed);
    ALL.store(true, Relaxed);
}

/// Stops the count [`count_all`] began: the most every thread held at once
/// past what they held as it began.
fn peak_now() -> isize {
    ALL.store(false, Relaxed);
    ALL_PEAK.load(Relaxed)
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

/// The most a replica holds as it takes its primary's first image, every
/// thread of the process counted -- the primary's sending it among them --
/// is a constant beside what the copy's own open of the image builds (its
/// id index, 12 bytes a row): the image is written into the file beside
/// the copy's as it comes, checked as it streams, and the copy loaded from
/// there mapped (`Database::from_side`), the file renamed into its place.
/// Read whole and loaded into memory, as every release to 0.2.0 took it,
/// the image was held twice and more -- 30 MB for one of 8 -- and a
/// database larger than the replica's memory never followed.
#[test]
fn a_replica_takes_an_image_holding_a_constant_however_large_the_database() {
    use fennec_http::replication::{self, Follower};
    use std::sync::{Arc, RwLock};
    let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    const HELD: isize = 8 << 20;
    let mut peaks = Vec::new();
    for rows in [8_000, 32_000] {
        let (port, len, _primary) = primary_named(&format!("followed-{rows}"), rows);
        let file = scratch().join(format!("replica-{rows}.fennec"));
        let _ = std::fs::remove_file(&file);
        let (mut db, feed) = replication::open(file.to_str().unwrap(), 1 << 20).unwrap();
        let lineage = db.history().lineage.clone();
        db.follow(lineage).unwrap();
        let db = Arc::new(RwLock::new(db));
        let url = format!("http://127.0.0.1:{port}");
        let follower =
            Follower::new(&url, TOKEN.into(), Arc::clone(&db), Some(feed), true).unwrap();
        count_all();
        let f = Arc::clone(&follower);
        let run = std::thread::spawn(move || f.run());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while follower.images() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "{rows} rows: the replica took no image"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let peak = peak_now();
        follower.halt();
        run.join().unwrap();
        assert_eq!(
            rows_in(&db.read().unwrap()),
            rows,
            "the copy holds every row"
        );
        assert!(
            !fennec_core::fs::beside(&file, "taking").exists(),
            "the image's file is the copy's now"
        );
        eprintln!("a replica took an image of {len} bytes ({rows} rows): {peak} held at the most");
        assert!(
            peak <= HELD,
            "{rows} rows: {peak} bytes held taking an image of {len}"
        );
        peaks.push(peak);
    }
    // What grows with the rows is what the copy's open builds and what the
    // primary's pin copies, a few dozen bytes a row: never the image.
    assert!(
        peaks[1] - peaks[0] <= 64 * 24_000,
        "{peaks:?}: grown by more than 64 bytes a row"
    );
}

/// The most `fennec backup` holds taking a database, into a file, plain and
/// sealed, and into an archive's directory: a constant, the same for a
/// database four times as large, the primary's sending it counted too. The
/// image is written into its file as it comes, sealed a frame at a time,
/// every record held to its sum as it streams. Read whole, it was the
/// image and its fork record, and sealed a second copy beside them.
#[test]
fn a_backup_takes_an_image_holding_a_constant_however_large_the_database() {
    use fennec_http::archive;
    use fennec_http::replication::Upstream;
    let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    const HELD: isize = 10 << 20;
    let key = fennec_http::seal::Key::from_hex(&"5a".repeat(32)).unwrap();
    for rows in [8_000, 32_000] {
        let (port, len, _primary) = primary_named(&format!("backed-{rows}"), rows);
        let upstream = Upstream::new(&format!("http://127.0.0.1:{port}"), TOKEN.into()).unwrap();
        for (how, sealed, into_dir) in [
            ("a file", false, false),
            ("a sealed file", true, false),
            ("a directory", false, true),
        ] {
            let out = scratch().join(format!("backup-{rows}-{sealed}-{into_dir}"));
            let _ = std::fs::remove_file(&out);
            let _ = std::fs::remove_dir_all(&out);
            if into_dir {
                std::fs::create_dir_all(&out).unwrap();
            }
            count_all();
            let seq = archive::backup(&upstream, &out, sealed.then_some(&key)).unwrap();
            let peak = peak_now();
            let restored = scratch().join(format!("restored-{rows}-{sealed}-{into_dir}.fennec"));
            let _ = std::fs::remove_file(&restored);
            match (sealed, into_dir) {
                (true, _) => {
                    assert_eq!(archive::unseal(&out, &key, &restored).unwrap(), seq);
                }
                (false, true) => {
                    let r = archive::Archive::new(&out)
                        .unwrap()
                        .restore(&restored, archive::Target::End)
                        .unwrap();
                    assert_eq!(r.seq, seq);
                }
                (false, false) => std::fs::copy(&out, &restored).map(|_| ()).unwrap(),
            }
            let db = fennec_core::fs::open(&restored).unwrap();
            assert_eq!(rows_in(&db), rows, "{how}: every row backed up");
            drop(db);
            eprintln!("a backup into {how} of {len} bytes ({rows} rows): {peak} held at the most");
            assert!(
                peak <= HELD,
                "{rows} rows, into {how}: {peak} bytes held backing up {len}"
            );
        }
    }
}

/// The most an archive holds taking an image of its end -- its image and
/// the writes after it assembled, opened, and an image written -- is a
/// constant beside what the open builds and a segment, read whole, of a few
/// writes here: the end is opened mapped through the file it was assembled
/// into, and its image written from a pin as it is read. Read whole,
/// loaded into memory and snapshotted, it was the database three times
/// over.
#[test]
fn an_archive_takes_an_image_of_its_end_holding_a_constant_however_large_the_database() {
    use fennec_http::archive::{Archive, Target};
    use fennec_http::replication::Upstream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    const HELD: isize = 10 << 20;
    for rows in [8_000, 32_000] {
        let (port, _, primary) = primary_named(&format!("archived-{rows}"), rows);
        let dir = scratch().join(format!("archive-{rows}"));
        let _ = std::fs::remove_dir_all(&dir);
        let shared = Arc::new(Archive::new(&dir).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let archiver = {
            let (a, flag) = (Arc::clone(&shared), Arc::clone(&stop));
            let url = format!("http://127.0.0.1:{port}");
            std::thread::spawn(move || {
                let upstream = Upstream::new(&url, TOKEN.into()).unwrap();
                a.follow(&upstream, &flag, &|_| {})
            })
        };
        let images = |dir: &std::path::Path| {
            std::fs::read_dir(dir).map_or(0, |d| {
                d.filter(|e| {
                    e.as_ref()
                        .is_ok_and(|e| e.file_name().to_string_lossy().starts_with("image-"))
                })
                .count()
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while images(&dir) == 0 {
            assert!(std::time::Instant::now() < deadline, "no image archived");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let end = {
            let mut db = primary.write().unwrap();
            put_rows(&mut db, rows, rows + 10);
            db.change_seq()
        };
        let probe = scratch().join(format!("probe-{rows}.fennec"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if shared
                .restore(&probe, Target::End)
                .is_ok_and(|r| r.seq >= end)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the archive did not reach {end}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        stop.store(true, Ordering::SeqCst);
        archiver.join().unwrap().unwrap();
        count_all();
        let took = shared.consolidate().unwrap();
        let peak = peak_now();
        assert_eq!(took, Some(end));
        let restored = scratch().join(format!("consolidated-{rows}.fennec"));
        let _ = std::fs::remove_file(&restored);
        let r = shared.restore(&restored, Target::End).unwrap();
        assert_eq!((r.seq, r.image), (end, end), "restored from the new image");
        assert_eq!(
            rows_in(&fennec_core::fs::open(&restored).unwrap()),
            rows + 10
        );
        eprintln!("an archive's end of {rows} rows taken as an image: {peak} held at the most");
        assert!(
            peak <= HELD,
            "{rows} rows: {peak} bytes held taking the end's image"
        );
    }
}
