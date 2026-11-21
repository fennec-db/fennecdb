//! What a dropped field's kept place costs (docs/design/storage.md#dropped-places):
//! `cargo run --release -p fennec-core --example places -- MODE`, one mode
//! a process, two builds held to each other by `make instr-compare
//! PKG=fennec-core EXAMPLE=places MODES=...` -- or by `tools/instr/compare.py
//! --bin`, a build of the base's engine with this file beside it, as the
//! pull request that kept the places was measured.
//!
//! `x (a int, b text, c int, d text)`, 100 000 rows in memory, `b` dropped
//! and the collection compacted: a base that takes the places out holds
//! `x` as `(a, c, d)`, this tree keeps `b`'s place, a `null` in each
//! document, which every read of `c` or `d` passes. `p (a int, c int, d
//! text)` is the same data never given a field to drop, which must read
//! and write alike in both. Each mode counts the instructions its loop
//! retired (`proc_pid_rusage`, macOS), the setup left out, and prints
//! `a request: <n> instructions`, an op's:
//!
//!   * `x-get-id`, `p-get-id`: `get x where id = $1`, 20 000 random ids;
//!   * `x-scan`, `p-scan`: `get x where c < $1 limit 10`, `c` random below
//!     a million and `$1` 100: every row read for its `c`;
//!   * `x-json`, `p-json`: `get x where id >= $1 limit 100` written as JSON
//!     from the stored documents (`Database::query_json`, the server's
//!     plain `get`);
//!   * `x-put`, `p-put`: `put x {id: $1, a: $2, c: $3, d: $4}`, 20 000 rows
//!     written over;
//!   * `x-compact`, `p-compact`: a file of the rows, a tenth of them written
//!     over, compacted beside the database as a server does
//!     (`Database::maintain`), in the process that compacted it last;
//!     `x-compact-open` the same once the file is opened again, which walks
//!     the documents for what it can reclaim once;
//!   * `gate-compact`: `gate.rs`'s `file-compact`, 10 000 rows each set
//!     once, then a `compact`;
//!   * `x-agg`, `p-agg`: `get x select k, count(*), sum(v) group k` over
//!     `x (a int, b text, k int, v int)`, 100 000 rows in 100 groups, `b`
//!     dropped and compacted (`p`: never there), both fields read past it;
//!   * `x-order`, `p-order`: `get x order c limit 10`, `c` read of every row
//!     as its sort key.
//!
//! Two that count something done once a process: `x-warm`, `p-warm` the
//! hash index of `c int @hash` built from the documents of a file read in,
//! as the first read after an open or a server's warm-up builds it
//! (`Database::warm`); `x-open-vec`, `p-open-vec` the open, read into
//! memory, of a file of 100 000 rows `x (a int, b text, v vector<8>
//! @hnsw(cosine))` checkpointed with its graph, which copies every vector
//! into the graph's arena past the dropped place (`Store::field_at`).
//!
//! And two that print what they measure: `bytes-<k>` the record bytes a
//! document of `x` with `k` text fields dropped between `a` and `c` holds
//! once compacted; `peak-mapped` and `peak-memory` how far a compact took
//! the process's footprint past the open, sampled every millisecond, over
//! two collections of 500 000 rows (mapped) or 200 000 (read into memory),
//! a dropped field each: the first fix's two cases.

use fennec_core::prelude::*;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, RwLock};

const ROWS: i64 = 100_000;

fn stmt(text: &str) -> Statement {
    fennec_ql::parse_one(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn run(db: &mut Database, text: &str) {
    db.execute(&stmt(text))
        .unwrap_or_else(|e| panic!("{text}: {e}"));
}

/// xorshift64*: the same values every run and every build.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n
    }
}

/// `name`'s rows written into `db`: with `dropped` text fields between `a`
/// and `c` that are then dropped, or none, and compacted either way.
fn rows(db: &mut Database, name: &str, dropped: usize, n: i64) {
    rows_indexed(db, name, dropped, n, "");
}

/// [`rows`], `c` declared with `index` after its type.
fn rows_indexed(db: &mut Database, name: &str, dropped: usize, n: i64, index: &str) {
    let gone: Vec<String> = (0..dropped).map(|i| format!("b{i} text")).collect();
    let mut fields = vec!["a int".to_string()];
    fields.extend(gone.iter().cloned());
    fields.extend([format!("c int {index}"), "d text".to_string()]);
    run(
        db,
        &format!("create collection {name} ({})", fields.join(", ")),
    );
    let mut text = format!("put {name} {{id: $1, a: $2, c: $3, d: $4");
    for i in 0..dropped {
        text.push_str(&format!(", b{i}: 'a dropped value, {i}'"));
    }
    text.push('}');
    let put = stmt(&text);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for id in 1..=n {
        let params = [
            Value::Int(id),
            Value::Int(id),
            Value::Int(rng.below(1_000_000) as i64),
            Value::Text(format!("row {id}, some text of its own")),
        ];
        db.execute_with(&put, &params).unwrap();
    }
    for i in 0..dropped {
        run(db, &format!("alter collection {name} drop field b{i}"));
    }
    run(db, &format!("compact {name}"));
}

/// `x (a int, b text, k int, v int)` -- `b` dropped and compacted, or
/// never there -- 100 000 rows in 100 groups by `k`.
fn grouped(db: &mut Database, dropped: usize) {
    let (fields, put) = match dropped {
        0 => ("a int, k int, v int", "put x {id: $1, a: $1, k: $2, v: $3}"),
        _ => (
            "a int, b text, k int, v int",
            "put x {id: $1, a: $1, b: 'a dropped value', k: $2, v: $3}",
        ),
    };
    run(db, &format!("create collection x ({fields})"));
    let put = stmt(put);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for id in 1..=ROWS {
        let params = [
            Value::Int(id),
            Value::Int(id % 100),
            Value::Int(rng.below(1_000) as i64),
        ];
        db.execute_with(&put, &params).unwrap();
    }
    if dropped > 0 {
        run(db, "alter collection x drop field b");
    }
    run(db, "compact x");
}

/// A file of `x (a int, b text, v vector<8> @hnsw(cosine))`'s 100 000
/// rows, `b` dropped and compacted or never there, checkpointed with its
/// graph.
fn vectors(dir: &Path, dropped: usize) -> PathBuf {
    let path = dir.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    let (fields, put) = match dropped {
        0 => (
            "a int, v vector<8> @hnsw(cosine)",
            "put x {id: $1, a: $1, v: $2}",
        ),
        _ => (
            "a int, b text, v vector<8> @hnsw(cosine)",
            "put x {id: $1, a: $1, b: 'a dropped value', v: $2}",
        ),
    };
    run(&mut db, &format!("create collection x ({fields})"));
    let put = stmt(put);
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for id in 1..=ROWS {
        let v: Vec<f32> = (0..8).map(|_| rng.below(1_000) as f32 + 1.0).collect();
        db.execute_with(&put, &[Value::Int(id), Value::Vector(v)])
            .unwrap();
    }
    if dropped > 0 {
        run(&mut db, "alter collection x drop field b");
    }
    run(&mut db, "compact");
    db.checkpoint().unwrap();
    path
}

/// The instructions this process retired so far
/// (`proc_pid_rusage`, `RUSAGE_INFO_V4`), and its footprint now.
#[cfg(target_os = "macos")]
fn rusage() -> Option<(u64, u64)> {
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
    }
    // `rusage_info_v4`: a 16-byte uuid, then its counters: the footprint
    // the 8th, `ri_instructions` the 30th.
    let mut info = [0u64; 40];
    // SAFETY: `info` is 40 words, more than the 37 of `rusage_info_v4`
    // (above) that flavor 4 writes; the call keeps no pointer to it.
    let r = unsafe { proc_pid_rusage(std::process::id() as i32, 4, info.as_mut_ptr()) };
    (r == 0).then_some((info[31], info[9]))
}

#[cfg(not(target_os = "macos"))]
fn rusage() -> Option<(u64, u64)> {
    None
}

/// Runs `body` `ops` times, and prints the instructions an op.
fn count(mode: &str, ops: usize, mut body: impl FnMut(usize)) {
    let before = rusage();
    for i in 0..ops {
        body(i);
    }
    let after = rusage();
    println!("mode {mode}, {ops} ops");
    if let (Some((i0, _)), Some((i1, _))) = (before, after) {
        println!(
            "a request: {:.1} instructions",
            (i1 - i0) as f64 / ops as f64
        );
    }
}

fn scratch(mode: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fennec-places-{mode}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A file of `name`'s rows, a tenth of them written over: what a compact
/// takes out.
fn file_rows(dir: &Path, name: &str, dropped: usize) -> PathBuf {
    let path = dir.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    rows(&mut db, name, dropped, ROWS);
    let put = stmt(&format!("put {name} {{id: $1, a: $1, c: 7, d: 'again'}}"));
    for id in (1..=ROWS).step_by(10) {
        db.execute_with(&put, &[Value::Int(id)]).unwrap();
    }
    path
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let (name, dropped) = match mode.split_once('-') {
        Some(("x", _)) => ("x", 1),
        Some(("p", _)) => ("x", 0),
        _ => ("x", 0),
    };
    let kind = mode.split_once('-').map_or("", |(_, k)| k);
    match (mode.as_str(), kind) {
        ("gate-compact", _) => {
            let dir = scratch(&mode);
            let path = dir.join("compact.fennec");
            let mut db = fennec_core::fs::open(&path).unwrap();
            run(
                &mut db,
                "create collection t (n int, k text @hash, body text)",
            );
            let put = stmt("put t {id: $1, n: $2, k: $3, body: $4}");
            for id in 1..=10_000i64 {
                let params = [
                    Value::Int(id),
                    Value::Int(id * 31 % 1_000),
                    Value::Text(format!("k{}", id % 100)),
                    Value::Text(format!("row {id}, some forty bytes of its own text")),
                ];
                db.execute_with(&put, &params).unwrap();
            }
            drop(db);
            let mut db = fennec_core::fs::open(&path).unwrap();
            let set = stmt("set t {n: n + 1} where id = $1");
            for id in 1..=10_000 {
                db.execute_with(&set, &[Value::Int(id)]).unwrap();
            }
            let compact = stmt("compact");
            count(&mode, 1, |_| {
                black_box(db.execute(&compact).unwrap());
            });
            drop(db);
            let _ = std::fs::remove_dir_all(&dir);
        }
        (_, "get-id") => {
            let mut db = Database::new();
            rows(&mut db, name, dropped, ROWS);
            let get = stmt("get x where id = $1");
            let mut rng = Rng(7);
            let ids: Vec<[Value; 1]> = (0..20_000)
                .map(|_| [Value::Int(rng.below(ROWS as u64) as i64 + 1)])
                .collect();
            count(&mode, ids.len(), |i| {
                black_box(db.query(&get, &ids[i]).unwrap());
            });
        }
        (_, "scan") => {
            let mut db = Database::new();
            rows(&mut db, name, dropped, ROWS);
            let scan = stmt("get x where c < $1 limit 10");
            let below = [Value::Int(100)];
            db.query(&scan, &below).unwrap();
            count(&mode, 20, |_| {
                black_box(db.query(&scan, &below).unwrap());
            });
        }
        (_, "json") => {
            let mut db = Database::new();
            rows(&mut db, name, dropped, ROWS);
            let get = stmt("get x where id >= $1 limit 100");
            let mut rng = Rng(11);
            let from: Vec<[Value; 1]> = (0..2_000)
                .map(|_| [Value::Int(rng.below(ROWS as u64 - 100) as i64 + 1)])
                .collect();
            let mut out = String::new();
            count(&mode, from.len(), |i| {
                out.clear();
                let n = db.query_json(&get, &from[i], &mut out).unwrap();
                black_box(n.expect("a plain get"));
            });
        }
        (_, "put") => {
            let mut db = Database::new();
            rows(&mut db, name, dropped, ROWS);
            let put = stmt("put x {id: $1, a: $2, c: $3, d: $4}");
            let mut rng = Rng(13);
            let params: Vec<[Value; 4]> = (0..20_000)
                .map(|i| {
                    [
                        Value::Int(rng.below(ROWS as u64) as i64 + 1),
                        Value::Int(i),
                        Value::Int(rng.below(1_000_000) as i64),
                        Value::Text(format!("written over, {i}")),
                    ]
                })
                .collect();
            count(&mode, params.len(), |i| {
                black_box(db.execute_with(&put, &params[i]).unwrap());
            });
        }
        (_, "compact") | (_, "compact-open") => {
            let dir = scratch(&mode);
            let path = file_rows(&dir, name, dropped);
            let db = match kind {
                "compact-open" => fennec_core::fs::open(&path).unwrap(),
                _ => {
                    // Compacted once more in this process: the state a
                    // server's compaction finds its file in, the dead
                    // records written again after it.
                    let mut db = fennec_core::fs::open(&path).unwrap();
                    run(&mut db, "compact");
                    let put = stmt("put x {id: $1, a: $1, c: 7, d: 'again'}");
                    for id in (1..=ROWS).step_by(10) {
                        db.execute_with(&put, &[Value::Int(id)]).unwrap();
                    }
                    db
                }
            };
            let db = RwLock::new(db);
            let compact = stmt("compact");
            count(&mode, 1, |_| {
                black_box(Database::maintain(&db, &compact).unwrap().unwrap());
            });
            drop(db);
            let _ = std::fs::remove_dir_all(&dir);
        }
        (_, "agg") => {
            let mut db = Database::new();
            grouped(&mut db, dropped);
            let agg = stmt("get x select k, count(*), sum(v) group k");
            db.query(&agg, &[]).unwrap();
            count(&mode, 20, |_| {
                black_box(db.query(&agg, &[]).unwrap());
            });
        }
        (_, "order") => {
            let mut db = Database::new();
            rows(&mut db, name, dropped, ROWS);
            let order = stmt("get x order c limit 10");
            db.query(&order, &[]).unwrap();
            count(&mode, 20, |_| {
                black_box(db.query(&order, &[]).unwrap());
            });
        }
        (_, "warm") => {
            let dir = scratch(&mode);
            let path = dir.join("db.fennec");
            {
                let mut db = fennec_core::fs::open(&path).unwrap();
                rows_indexed(&mut db, name, dropped, ROWS, "@hash");
                db.checkpoint().unwrap();
            }
            let db = fennec_core::fs::open_in_memory(&path).unwrap();
            count(&mode, 1, |_| {
                assert_eq!(db.warm(&[]).unwrap(), 1);
            });
            drop(db);
            let _ = std::fs::remove_dir_all(&dir);
        }
        (_, "open-vec") => {
            let dir = scratch(&mode);
            let path = vectors(&dir, dropped);
            let mut db = None;
            count(&mode, 1, |_| {
                db = Some(fennec_core::fs::open_in_memory(&path).unwrap());
            });
            let db = db.unwrap();
            let x = db.collection("x").unwrap();
            assert_eq!(x.store.len(), ROWS as usize);
            drop(db);
            let _ = std::fs::remove_dir_all(&dir);
        }
        (m, _) if m.starts_with("bytes-") => {
            let k: usize = m["bytes-".len()..].parse().expect("bytes-<k>");
            let mut db = Database::new();
            rows(&mut db, "x", k, 10_000);
            let store = &db.collection("x").unwrap().store;
            println!(
                "{k} dropped: {:.2} bytes a document, {} places",
                store.total_bytes() as f64 / store.len() as f64,
                db.collection("x").unwrap().schema.width()
            );
        }
        ("peak-mapped", _) | ("peak-memory", _) => peak(&mode),
        _ => {
            eprintln!(
                "modes: x- or p-: get-id, scan, json, put, compact, agg, order, warm, \
                 open-vec; x-compact-open, gate-compact, bytes-<k>, peak-mapped, peak-memory"
            );
            std::process::exit(2);
        }
    }
}

/// Two collections of `n` rows, a dropped field each a field follows, in a
/// file: compacted as `compact` runs in a shell, under the lock, the
/// footprint sampled every millisecond meanwhile.
fn peak(mode: &str) {
    let mapped = mode == "peak-mapped";
    let n = if mapped { 500_000 } else { 200_000 };
    let dir = scratch(mode);
    let path = dir.join("db.fennec");
    {
        let mut db = fennec_core::fs::open(&path).unwrap();
        for c in ["x", "y"] {
            run(
                &mut db,
                &format!("create collection {c} (a int, b text, c int, d text)"),
            );
            let put = stmt(&format!(
                "put {c} {{id: $1, a: $1, b: 'a dropped value of some length', c: $1, d: $2}}"
            ));
            for id in 1..=n {
                let d = Value::Text(format!("row {id}, some text of its own"));
                db.execute_with(&put, &[Value::Int(id), d]).unwrap();
            }
            run(&mut db, &format!("alter collection {c} drop field b"));
        }
        db.checkpoint().unwrap();
    }
    let mut db = match mapped {
        true => fennec_core::fs::open(&path).unwrap(),
        false => fennec_core::fs::open_in_memory(&path).unwrap(),
    };
    let Some((i0, open)) = rusage() else {
        println!("{mode}: no footprint off macOS");
        return;
    };
    let top = Arc::new(AtomicU64::new(open));
    let done = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (top, done) = (Arc::clone(&top), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Relaxed) {
                if let Some((_, now)) = rusage() {
                    top.fetch_max(now, Relaxed);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    run(&mut db, "compact");
    let (i1, after) = rusage().unwrap();
    done.store(true, Relaxed);
    sampler.join().unwrap();
    let top = top.load(Relaxed).max(after);
    println!(
        "{mode}: 2 x {n} rows, the compact peaked {:.1} MB above the open ({:.1} MB), \
         {:.1} M instructions",
        (top - open) as f64 / 1e6,
        open as f64 / 1e6,
        (i1 - i0) as f64 / 1e6
    );
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
