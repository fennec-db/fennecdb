//! What a sync's seed costs the writers beside it: `make seed-bench`.
//!
//! fennec-server (`--sync 250`) over a collection of [`ROWS`] rows; a
//! client making single writes -- one `insert` a request into the same
//! collection, at `RATE` a second (`SEED_RATE`, 0 for as fast as it is
//! answered) -- for [`RUN`], alone and beside a subscriber seeded over and
//! over: `GET /items/changes`, read to the end of its `seed` event, then on
//! through the stream's first look -- the `change` event telling the writes
//! made while the seed was read, all of them read under the lock, or the
//! seed again where they overran the change ring -- and [`LINGER`] after
//! it, closed. A write's time runs from the moment it was
//! due, not the moment it went out, so a write the stall held back counts
//! the wait of every one queued behind it, as an app making them at that
//! rate meets it. Printed: the writes' rate, p50, p99 and longest over the
//! whole run, how many took past 50 ms, and each seed's time to its last
//! byte.
//!
//! `FENNEC_SERVER` runs another build of the server (a binary from before,
//! to compare in turns); `SEED_ROWS` and `SEED_RATE` change the shape's
//! size and the writes' rate.

#[path = "../http.rs"]
mod http;

use fennec_core::prelude::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROWS: usize = 1_000_000;
const RUN: Duration = Duration::from_secs(10);

/// How long a subscriber stays after its first look's event: the looks
/// after the first, each a write's, are timed beside the writes too.
const LINGER: Duration = Duration::from_millis(100);

/// How long a subscriber waits for its first look's event at most: one
/// comes with the next write once the seed is sent.
const FIRST_LOOK: Duration = Duration::from_secs(5);

fn env_or(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn items_file(path: &Path, rows: usize) {
    let _ = std::fs::remove_file(path);
    let mut db = fennec_core::fs::open(path).unwrap();
    db.execute(
        &fennec_ql::parse_one("create collection items (k int, title text, n int)").unwrap(),
    )
    .unwrap();
    let put = fennec_ql::parse_one("put items $1").unwrap();
    for start in (0..rows).step_by(20_000) {
        let docs = (start..(start + 20_000).min(rows))
            .map(|i| {
                Value::object(vec![
                    ("k".into(), Value::Int(i as i64)),
                    ("title".into(), Value::Text(format!("item number {i}"))),
                    ("n".into(), Value::Int((i % 97) as i64)),
                ])
                .unwrap()
            })
            .collect();
        db.execute_with(&put, &[Value::List(docs)]).unwrap();
    }
    db.checkpoint().unwrap();
}

/// fennec-server over `file` on a port the system picks, and the address
/// its `listening on` line names; its stderr read off after it.
fn start(file: &Path) -> (http::Server, String) {
    let bin = std::env::var("FENNEC_SERVER").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/release/fennec-server"
        )
        .into()
    });
    let mut child = std::process::Command::new(&bin)
        .args(["--http", "127.0.0.1:0"])
        .args(["--file", file.to_str().unwrap(), "--no-checkpoint"])
        .args(["--sync", "250"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{bin}: {e} -- make seed-bench builds it"));
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let server = http::Server::from_child(child);
    let mut line = String::new();
    let addr = loop {
        line.clear();
        assert!(err.read_line(&mut line).unwrap() > 0, "fennec-server ended");
        if let Some((_, rest)) = line.split_once("listening on: http://") {
            break rest.split_whitespace().next().unwrap().to_string();
        }
    };
    std::thread::spawn(move || std::io::copy(&mut err, &mut std::io::sink()));
    (server, addr)
}

/// A subscriber's seed: the stream opened, read to the end of its `seed`
/// event, then on to the end of the event its first look sends -- a
/// `change` telling every write made while the seed was read, read under
/// the lock, or, where those writes overran the change ring, the seed
/// again -- and [`LINGER`] after it, and let go. The seed's time to its
/// last byte, ms; `buf` left holding the seed alone.
fn seed(addr: &str, buf: &mut Vec<u8>) -> f64 {
    let (mut s, took, end) = seed_event(addr, buf);
    let mut after = buf.split_off(end);
    let mut chunk = vec![0u8; 1 << 20];
    s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let first = Instant::now() + FIRST_LOOK;
    // Only the bytes since the last look are searched, a needle's length
    // back: a second seed is 57 MB.
    let (mut from, mut start) = (0, None);
    let mut until = None;
    while until.map_or(Instant::now() < first, |at| Instant::now() < at) {
        if until.is_none() {
            start = start.or_else(|| find(&after, from, b"event: "));
            if start.is_some_and(|at| find(&after, from.max(at), b"\n\n").is_some()) {
                until = Some(Instant::now() + LINGER);
                after = Vec::new();
                continue;
            }
            from = after.len().saturating_sub(7);
        }
        match s.read(&mut chunk) {
            Ok(0) => panic!("the stream ended after its seed"),
            Ok(n) if until.is_none() => after.extend_from_slice(&chunk[..n]),
            _ => {}
        }
    }
    took
}

/// Where `needle` is in `hay` at or after `from`.
fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let at = hay
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)?;
    Some(from + at)
}

/// A stream opened and read to the end of its `seed` event into `buf`,
/// which may hold what came after it too: the stream, the seed's time to
/// its last byte (ms), and where in `buf` the event ends.
fn seed_event(addr: &str, buf: &mut Vec<u8>) -> (TcpStream, f64, usize) {
    let t = Instant::now();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "GET /items/changes HTTP/1.1\r\nHost: bench\r\n\r\n").unwrap();
    buf.clear();
    let mut chunk = vec![0u8; 1 << 20];
    // The seed's data is one line, and the event ends at the first blank
    // line after its start.
    const START: &[u8] = b"event: seed\ndata: ";
    let mut data = None;
    let mut looked = 0;
    loop {
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0, "the stream ended before its seed");
        buf.extend_from_slice(&chunk[..n]);
        let Some(from) = data.or_else(|| {
            let at = buf.windows(START.len()).position(|w| w == START)?;
            Some(at + START.len())
        }) else {
            assert!(buf.len() < 1 << 16, "no seed event");
            continue;
        };
        data = Some(from);
        looked = looked.max(from);
        if let Some(at) = buf[looked..].windows(2).position(|w| w == b"\n\n") {
            let took = t.elapsed().as_secs_f64() * 1e3;
            return (s, took, looked + at + 2);
        }
        looked = buf.len() - 1;
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

/// Writes at `rate` a second (0: as fast as they are answered) for
/// [`RUN`], and seeds back to back beside them when `seeding`: the
/// writes' times and the seeds'.
fn round(addr: &str, rate: usize, seeding: bool, first: usize) -> (Vec<f64>, Vec<f64>) {
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (addr, stop) = (addr.to_string(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut h = http::Http::connect(&addr);
            let mut times = Vec::new();
            let began = Instant::now();
            let mut n = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let due = match rate {
                    0 => Instant::now(),
                    r => began + Duration::from_secs_f64(n as f64 / r as f64),
                };
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                let k = first + n;
                h.query(
                    "insert items {k: $1, title: $2, n: $3}",
                    &format!("{k},\"written {k}\",{}", k % 97),
                );
                times.push(due.elapsed().as_secs_f64() * 1e3);
                n += 1;
            }
            times
        })
    };
    let mut seeds = Vec::new();
    let end = Instant::now() + RUN;
    let mut buf = Vec::new();
    while seeding && Instant::now() < end {
        seeds.push(seed(addr, &mut buf));
    }
    std::thread::sleep(end.saturating_duration_since(Instant::now()));
    stop.store(true, Ordering::Relaxed);
    (writer.join().unwrap(), seeds)
}

fn main() {
    let rows = env_or("SEED_ROWS", ROWS);
    let rate = env_or("SEED_RATE", 1000);
    let dir = std::env::temp_dir().join(format!("fennecbench-seed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("items.fennec");
    let t = Instant::now();
    items_file(&file, rows);
    println!("{rows} rows, made in {:.1} s", t.elapsed().as_secs_f64());
    let (server, addr) = start(&file);
    // The file's pages read in, and the writes' path, before anything is
    // timed.
    let mut buf = Vec::new();
    // No write comes to end a first look here: the seed alone.
    seed_event(&addr, &mut buf);
    round(&addr, rate, false, 1 << 40);
    let rate_name = match rate {
        0 => "back to back".to_string(),
        r => format!("{r} a second"),
    };
    println!(
        "{:<34} {:>9} {:>8} {:>8} {:>8} {:>9}",
        "", "a second", "p50 ms", "p99 ms", "max ms", "over 50ms"
    );
    for (name, seeding, first) in [("alone", false, 1 << 41), ("beside seeds", true, 1 << 42)] {
        let (mut writes, mut seeds) = round(&addr, rate, seeding, first);
        let done = writes.len() as f64 / RUN.as_secs_f64();
        let over = writes.iter().filter(|&&t| t > 50.0).count();
        let (p50, p99, max) = (
            pct(&mut writes, 0.5),
            pct(&mut writes, 0.99),
            pct(&mut writes, 1.0),
        );
        let label = format!("writes ({rate_name}), {name}");
        println!("{label:<34} {done:>9.0} {p50:>8.2} {p99:>8.2} {max:>8.1} {over:>9}");
        if !seeds.is_empty() {
            println!(
                "  {} seeds of {} MB: p50 {:.0} ms, {:.0} to {:.0}",
                seeds.len(),
                buf.len() >> 20,
                pct(&mut seeds, 0.5),
                pct(&mut seeds, 0.0),
                pct(&mut seeds, 1.0)
            );
        }
    }
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
