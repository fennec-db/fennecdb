//! The perf gate's workloads: what `tools/perf_gate.py` counts in
//! instructions an op under callgrind, main's build against a pull
//! request's, on every pull request (docs/design/benchmarks.md#the-gate).
//!
//!     cargo run --release -p fennec-http --example gate -- --list
//!     cargo run --release -p fennec-http --example gate -- core-get-id
//!
//! Each workload sets its data up from a fixed seed, then runs its ops in a
//! loop, and only the loop is counted: under `valgrind --tool=callgrind
//! --instr-atstart=no --collect-atstart=no` the program asks callgrind to
//! instrument and count as the loop starts and to stop as it ends
//! (`callgrind::request`), requests that are no work run natively. The
//! setup runs under valgrind's core alone, some five times slower than
//! native, where instrumented it would run some eighty times slower.
//!
//! In process and on one thread, as the engine answers a request: nothing
//! over a socket -- the wire's figures are `make requests-bench`'s and `make
//! ycsb`'s -- and nothing that runs on a clock, which valgrind slows. Each
//! op's inputs are made before the loop, so what is counted is the engine's
//! work. A name is its family's: `core-*` the engine through `Database`,
//! `ql-*` the parser, `http-*` a request through `fennec_http::handle`,
//! `file-*` a file's open and compaction. A workload's statements are
//! FennecQL texts, which 1.x keeps, so the same program builds against main
//! and a pull request alike; each workload stands for the claim the table
//! in docs/design/benchmarks.md names.

use fennec_core::prelude::*;
use fennec_http::access::{Access, CheckPlugin};
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// A workload's setup: given its scratch directory and its ops, it makes
/// its data and hands back the loop's body, called with each op's number.
type Setup = fn(&Path, usize) -> Box<dyn FnMut(usize)>;

/// Every workload: its name, the ops its loop runs -- enough for a
/// million instructions and more -- and its setup.
const WORKLOADS: &[(&str, usize, Setup)] = &[
    ("core-put", 2_000, core_put),
    ("core-insert", 2_000, core_insert),
    ("core-get-id", 4_000, core_get_id),
    ("core-filter-limit", 1_000, core_filter_limit),
    ("core-order", 1_000, core_order),
    ("core-group", 10, core_group),
    ("core-lookup", 50, core_lookup),
    ("core-match", 200, core_match),
    ("core-fuzzy", 200, core_fuzzy),
    ("core-facet", 512, core_facet),
    ("core-near", 100, core_near),
    ("core-near-filtered", 100, core_near_filtered),
    ("core-json-page", 10, core_json_page),
    ("ql-parse", 20, ql_parse),
    ("http-get-row", 1_000, http_get_row),
    ("http-patch", 1_000, http_patch),
    ("http-put", 1_000, http_put),
    ("http-put-keyed", 1_000, http_put_keyed),
    ("http-batch", 3, http_batch),
    ("http-scoped-get", 1_000, http_scoped_get),
    ("http-scoped-set", 1_000, http_scoped_set),
    ("http-scoped-insert", 1_000, http_scoped_insert),
    ("file-open", 1, file_open),
    ("file-compact", 1, file_compact),
];

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "--list" {
        for (name, ops, _) in WORKLOADS {
            println!("{name} {ops}");
        }
        return;
    }
    let Some(&(name, ops, setup)) = WORKLOADS.iter().find(|w| w.0 == arg) else {
        eprintln!("usage: gate --list | gate WORKLOAD");
        std::process::exit(2);
    };
    let dir = std::env::temp_dir().join(format!("fennec-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = setup(&dir, ops);
    let t = Instant::now();
    callgrind::request(callgrind::START_INSTRUMENTATION);
    callgrind::request(callgrind::TOGGLE_COLLECT);
    for i in 0..ops {
        body(i);
    }
    callgrind::request(callgrind::TOGGLE_COLLECT);
    callgrind::request(callgrind::STOP_INSTRUMENTATION);
    let took = t.elapsed();
    println!(
        "{name} {ops} ops, {:.0} ns an op",
        took.as_nanos() as f64 / ops as f64
    );
    // The data is let go of uncounted, and natively in no time worth one.
    drop(body);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Callgrind's client requests (valgrind's `callgrind.h`): a word of magic
/// instructions valgrind reads as a call to the tool and a processor runs
/// as no work -- a register rotated through a full turn, then moved onto
/// itself. The sequences are `valgrind.h`'s, byte for byte, or valgrind
/// passes them by and counts nothing, which the gate's driver refuses.
mod callgrind {
    /// `VG_USERREQ_TOOL_BASE('C', 'T')`, callgrind's first request.
    const BASE: u64 = ((b'C' as u64) << 24) | ((b'T' as u64) << 16);
    pub const TOGGLE_COLLECT: u64 = BASE + 2;
    pub const START_INSTRUMENTATION: u64 = BASE + 4;
    pub const STOP_INSTRUMENTATION: u64 = BASE + 5;

    /// Hands valgrind `code`, its arguments none; natively nothing.
    #[inline(never)]
    pub fn request(code: u64) -> u64 {
        let args = [code, 0, 0, 0, 0, 0];
        magic(&args)
    }

    #[cfg(target_arch = "x86_64")]
    fn magic(args: &[u64; 6]) -> u64 {
        let mut out = 0u64;
        // SAFETY: `rdi` rotated by 3, 13, 61 and 51 bits is rotated by 128,
        // as it was; `xchg rbx, rbx` changes nothing. Valgrind answers in
        // `rdx`, reading the arguments `rax` points at.
        unsafe {
            std::arch::asm!(
                "rol rdi, 3",
                "rol rdi, 13",
                "rol rdi, 61",
                "rol rdi, 51",
                "xchg rbx, rbx",
                inout("rdx") out,
                in("rax") args.as_ptr(),
                options(nostack),
            );
        }
        out
    }

    #[cfg(target_arch = "aarch64")]
    fn magic(args: &[u64; 6]) -> u64 {
        let mut out = 0u64;
        // SAFETY: `x12` rotated by 3, 13, 51 and 61 bits is rotated by 128,
        // as it was; `orr x10, x10, x10` changes nothing. Valgrind answers
        // in `x3`, reading the arguments `x4` points at.
        unsafe {
            std::arch::asm!(
                "ror x12, x12, #3",
                "ror x12, x12, #13",
                "ror x12, x12, #51",
                "ror x12, x12, #61",
                "orr x10, x10, x10",
                inout("x3") out,
                in("x4") args.as_ptr(),
                options(nostack),
            );
        }
        out
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn magic(_: &[u64; 6]) -> u64 {
        0
    }
}

// ------------------------------------------------------------------ data

/// xorshift64*: the same values every run and every build.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        ((self.next() as u128 * n as u128) >> 64) as u64
    }

    /// In [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Roughly normal, by the central limit.
    fn gauss(&mut self) -> f32 {
        (0..6).map(|_| self.unit()).sum::<f32>() - 3.0
    }

    /// A word's rank, the first ranks the commonest (as `search.rs` draws
    /// them): a cube of a uniform draw.
    fn rank(&mut self, words: u64) -> u64 {
        let u = self.unit() as f64;
        (u * u * u * words as f64) as u64
    }
}

/// The vocabulary's words: syllables, so a typo is a word's typo.
fn word(rank: u64) -> String {
    const SYLLABLES: [&str; 20] = [
        "ka", "lo", "mi", "re", "tu", "sa", "ne", "po", "di", "ga", "ve", "zu", "ri", "fo", "ba",
        "le", "ko", "mu", "si", "ta",
    ];
    let (a, b, c) = (rank % 20, rank / 20 % 20, rank / 400 % 20);
    format!(
        "{}{}{}",
        SYLLABLES[a as usize], SYLLABLES[b as usize], SYLLABLES[c as usize]
    )
}

const WORDS: u64 = 5_000;
const DOCS: usize = 10_000;

fn stmt(text: &str) -> Statement {
    fennec_ql::parse_one(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn run(db: &mut Database, text: &str, params: &[Value]) {
    db.execute_with(&stmt(text), params)
        .unwrap_or_else(|e| panic!("{text}: {e}"));
}

fn file_db(dir: &Path, name: &str) -> Database {
    fennec_core::fs::open(dir.join(name)).unwrap()
}

/// `t (n int, k text @hash)`, `rows` rows: `n` the id, `k` one of 100.
fn keyed_rows(db: &mut Database, rows: i64) {
    run(db, "create collection t (n int, k text @hash)", &[]);
    let put = stmt("put t {id: $1, n: $2, k: $3}");
    for id in 1..=rows {
        let k = Value::Text(format!("k{}", id % 100));
        db.execute_with(&put, &[Value::Int(id), Value::Int(id), k])
            .unwrap();
    }
}

/// A read's loop: `text` with each op's parameters, answered by `query`.
fn reads(db: Database, text: &str, params: Vec<Vec<Value>>) -> Box<dyn FnMut(usize)> {
    let stmt = stmt(text);
    // The first answer builds what a read builds once: an index an open
    // left unbuilt, the text index, the words' sorted copy.
    db.query(&stmt, &params[0]).unwrap();
    Box::new(move |i| {
        black_box(db.query(&stmt, &params[i % params.len()]).unwrap());
    })
}

// ------------------------------------------------------------------ core

/// A row written over by id, a `@hash` field's bucket moved with it, into
/// a file as a server writes it.
fn core_put(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let mut db = file_db(dir, "put.fennec");
    keyed_rows(&mut db, 1_000);
    let put = stmt("put t {id: $1, n: $2, k: $3}");
    let params: Vec<[Value; 3]> = (0..ops as i64)
        .map(|i| {
            let k = Value::Text(format!("k{}", i * 7 % 100));
            [Value::Int(i % 1_000 + 1), Value::Int(i), k]
        })
        .collect();
    Box::new(move |i| {
        black_box(db.execute_with(&put, &params[i]).unwrap());
    })
}

/// A new row, its id never held, into a file.
fn core_insert(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let mut db = file_db(dir, "insert.fennec");
    keyed_rows(&mut db, 1_000);
    let insert = stmt("insert t {id: $1, n: $2, k: $3}");
    let params: Vec<[Value; 3]> = (0..ops as i64)
        .map(|i| {
            let k = Value::Text(format!("k{}", i % 100));
            [Value::Int(1_001 + i), Value::Int(i), k]
        })
        .collect();
    Box::new(move |i| {
        black_box(db.execute_with(&insert, &params[i]).unwrap());
    })
}

fn core_get_id(_: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let mut db = Database::new();
    keyed_rows(&mut db, DOCS as i64);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let ids = (0..ops)
        .map(|_| vec![Value::Int(rng.below(DOCS as u64) as i64 + 1)])
        .collect();
    reads(db, "get t where id = $1", ids)
}

/// A `@hash` bucket of some 100 rows, the first ten.
fn core_filter_limit(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let mut db = Database::new();
    keyed_rows(&mut db, DOCS as i64);
    let keys = (0..100)
        .map(|k| vec![Value::Text(format!("k{}", k * 37 % 100))])
        .collect();
    reads(db, "get t where k = $1 limit 10", keys)
}

/// A range of a `@sorted` field, its first ten in order.
fn core_order(_: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let mut db = Database::new();
    run(&mut db, "create collection t (n int @sorted, k text)", &[]);
    let put = stmt("put t {id: $1, n: $2, k: $3}");
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    for id in 1..=DOCS as i64 {
        let n = Value::Int(rng.below(1_000_000) as i64);
        let k = Value::Text(format!("k{}", id % 100));
        db.execute_with(&put, &[Value::Int(id), n, k]).unwrap();
    }
    let bounds = (0..ops)
        .map(|_| vec![Value::Int(rng.below(990_000) as i64)])
        .collect();
    reads(db, "get t where n >= $1 order n limit 10", bounds)
}

/// Every row grouped by a field of 100 values, counted and summed.
fn core_group(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let mut db = Database::new();
    keyed_rows(&mut db, DOCS as i64);
    reads(
        db,
        "get t select k, count(*), sum(n) group k",
        vec![Vec::new()],
    )
}

/// Ten parents in id order, each with its children through a `@hash`.
fn core_lookup(_: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let mut db = Database::new();
    run(&mut db, "create collection a (name text)", &[]);
    run(&mut db, "create collection b (p int @hash, v int)", &[]);
    let (put_a, put_b) = (
        stmt("put a {id: $1, name: $2}"),
        stmt("put b {p: $1, v: $2}"),
    );
    for id in 1..=1_000i64 {
        let name = Value::Text(format!("parent {id}"));
        db.execute_with(&put_a, &[Value::Int(id), name]).unwrap();
    }
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for v in 0..DOCS as i64 {
        let p = Value::Int(rng.below(1_000) as i64 + 1);
        db.execute_with(&put_b, &[p, Value::Int(v)]).unwrap();
    }
    let starts = (0..ops)
        .map(|_| vec![Value::Int(rng.below(990) as i64 + 1)])
        .collect();
    reads(
        db,
        "get a where id >= $1 order id limit 10 lookup b on p",
        starts,
    )
}

/// `d`: DOCS documents of 30 words over WORDS, the commoner more common,
/// and a `brand` under `@hash` of 40 values.
fn documents() -> Database {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection d (body text @text, brand text @hash)",
        &[],
    );
    let put = stmt("put d {body: $1, brand: $2}");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..DOCS {
        let body = (0..30)
            .map(|_| word(rng.rank(WORDS)))
            .collect::<Vec<_>>()
            .join(" ");
        let brand = Value::Text(format!("brand {}", rng.below(40)));
        db.execute_with(&put, &[Value::Text(body), brand]).unwrap();
    }
    db
}

/// BM25 over two words, the ten best (`benchmarks-text.html`).
fn core_match(_: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let db = documents();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let queries = (0..ops)
        .map(|_| {
            let q = format!("{} {}", word(rng.rank(WORDS)), word(rng.rank(WORDS)));
            vec![Value::Text(q)]
        })
        .collect();
    reads(db, "get d match body $1 limit 10", queries)
}

/// A word with a typo -- two letters swapped, or one changed -- found
/// among the words within an edit.
fn core_fuzzy(_: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let db = documents();
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    let queries = (0..ops)
        .map(|_| {
            let mut w: Vec<u8> = word(rng.rank(WORDS)).into_bytes();
            let at = rng.below(w.len() as u64 - 1) as usize;
            match rng.below(2) {
                0 => w.swap(at, at + 1),
                _ => w[at] = b'a' + rng.below(26) as u8,
            }
            vec![Value::Text(String::from_utf8(w).unwrap())]
        })
        .collect();
    reads(db, "get d match body $1 fuzzy limit 10", queries)
}

/// The databases `core-facet` asks in turn.
const FACETED: usize = 256;

/// The brands of every row, counted through the `@hash` buckets: a
/// bucket's length a brand, the ten most picked by `order_rows`. The
/// buckets come in the order of the index's table, which its hash keys
/// lay out -- a draw a map, std's `RandomState` -- and the pick compares
/// as many times as that order makes it: one database's facet costs from
/// one process to the next some 15% apart. So each op asks the next of
/// FACETED databases, each with its own keys, and a run counts the mean of
/// as many draws: a sixteenth of one's spread. A facet's cost is its
/// buckets', not its rows': each holds 40 brands, brand `b` on `b + 1` rows.
fn core_facet(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let dbs: Vec<Database> = (0..FACETED)
        .map(|_| {
            let mut db = Database::new();
            run(&mut db, "create collection f (brand text @hash)", &[]);
            let mut brands: Vec<u64> = (0..40).flat_map(|b| (0..=b).map(move |_| b)).collect();
            for i in (1..brands.len()).rev() {
                brands.swap(i, rng.below(i as u64 + 1) as usize);
            }
            let put = stmt("put f {brand: $1}");
            for b in brands {
                db.execute_with(&put, &[Value::Text(format!("brand {b}"))])
                    .unwrap();
            }
            db
        })
        .collect();
    let facet = stmt("get f select id limit 0 facet brand top 10");
    for db in &dbs {
        db.query(&facet, &[]).unwrap();
    }
    Box::new(move |i| {
        black_box(dbs[i % FACETED].query(&facet, &[]).unwrap());
    })
}

const DIM: usize = 64;

/// `v`: 4 000 vectors of DIM around 40 centres, under `@hnsw`, and a
/// `c` under `@hash` keeping 1% of them each; and 100 queries drawn alike.
fn vectors() -> (Database, Vec<Value>) {
    let mut db = Database::new();
    run(
        &mut db,
        &format!(
            "create collection v (c int @hash, e vector<{DIM}> @hnsw(cosine, m=16, ef_construction=64))"
        ),
        &[],
    );
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let centres: Vec<Vec<f32>> = (0..40)
        .map(|_| (0..DIM).map(|_| rng.gauss()).collect())
        .collect();
    let near = |rng: &mut Rng| {
        let c = &centres[rng.below(40) as usize];
        Value::Vector(c.iter().map(|x| x + 0.3 * rng.gauss()).collect())
    };
    let put = stmt("put v {c: $1, e: $2}");
    for i in 0..4_000i64 {
        let e = near(&mut rng);
        db.execute_with(&put, &[Value::Int(i % 100), e]).unwrap();
    }
    let queries = (0..100).map(|_| near(&mut rng)).collect();
    (db, queries)
}

/// The ten nearest by the graph, a beam of 100 (`benchmarks-vector.html`).
fn core_near(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let (db, queries) = vectors();
    let params = queries.into_iter().map(|q| vec![q]).collect();
    reads(db, "get v near e $1 ef 100 limit 10", params)
}

/// The same under a filter keeping 1%: the walk's filter, and its fallback.
fn core_near_filtered(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let (db, queries) = vectors();
    let params = (queries.into_iter().enumerate())
        .map(|(i, q)| vec![q, Value::Int(i as i64 * 7 % 100)])
        .collect();
    reads(db, "get v where c = $2 near e $1 ef 100 limit 10", params)
}

/// A page of 1 000 rows of four floats written as JSON off the stored
/// documents, as a server answers a plain `get` (`json_numbers.rs`'s
/// `e15`): half the rows whole floats in [1e15, 1e16), half fractions.
fn core_json_page(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection t (a float, b float, c float, d float)",
        &[],
    );
    let put = stmt("put t {a: $1, b: $2, c: $3, d: $4}");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for row in 0..1_000 {
        let params: Vec<Value> = (0..4)
            .map(|_| {
                let r = rng.next();
                Value::Float(match row % 2 {
                    0 => (1e15 + (r % 9_000_000_000_000_000) as f64).floor(),
                    _ => (r % 100_000_000) as f64 / 7.0 + 0.5,
                })
            })
            .collect();
        db.execute_with(&put, &params).unwrap();
    }
    let get = stmt("get t");
    let mut out = String::new();
    db.query_json(&get, &[], &mut out).unwrap();
    Box::new(move |_| {
        // A fresh `String` each time, as a server answers each request.
        let mut out = String::new();
        black_box(db.query_json(&get, &[], &mut out).unwrap());
        black_box(out);
    })
}

// ------------------------------------------------------------------ ql

/// Texts of every kind of statement, each read into its tree: what every
/// request's first sight of a text pays.
const TEXTS: &[&str] = &[
    "create collection articles (title text @text, year int @sorted, views int, tags [text], embed vector<384> @hnsw(cosine))",
    "create collection orders (customer text @hash, total float, status text, placed timestamp @sorted)",
    "create index if not exists on orders (status) @hash",
    "get articles where year >= 2020 and views > 100 order year desc limit 20",
    "get articles select title, year where views > 10 order views desc, id limit 10 offset 20",
    "get articles near embed $1 ef 100 limit 10",
    "get articles where year >= 2020 near embed $1 limit 5",
    "get articles match title 'vector database' limit 10",
    "get articles select title, highlight(title) match title $1 limit 10",
    "get articles match title $1 fuzzy limit 10",
    "get articles select title limit 0 facet year top 10",
    "get orders select customer, count(*), sum(total) group customer having count(*) > 2",
    "get orders select status, avg(total) as mean where placed >= $1 group status",
    "get articles select title order year limit 2 lookup remarks on article_id order stars limit 1",
    "get articles where id in (get remarks where stars = 5 select article_id) limit 10",
    "get orders where status = 'open' and total between 10 and 100 or customer = $1 limit 50",
    "put articles {id: 1, title: 'A fox', year: 2024, views: 0, tags: ['animals', 'rust']}",
    "put orders [{customer: 'ada', total: 12.5, status: 'open'}, {customer: 'bob', total: 3, status: 'paid'}]",
    "insert orders {customer: $1, total: $2, status: 'open'} returning id",
    "set orders {status: 'paid', total: total + 1} where id = $1 require 1",
    "set articles {views: views + 1} where year < 2000 order views limit 100",
    "del orders where status = 'cancelled' and placed < $1",
    "del articles where id = 42 returning title",
    "get orders where customer = 'ada' count",
    "explain get articles where year >= 2020 order year limit 10",
    "SELECT title, year FROM articles WHERE year >= 2020 ORDER BY year DESC LIMIT 10",
    "SELECT customer, COUNT(*) FROM orders WHERE total > 10 GROUP BY customer",
    "SELECT a.title, r.stars FROM articles a JOIN remarks r ON r.article_id = a.id WHERE a.year = 2024",
    "INSERT INTO orders (customer, total, status) VALUES ('cy', 4.5, 'open')",
    "UPDATE orders SET status = 'paid' WHERE id = 7",
    "DELETE FROM orders WHERE status = 'cancelled'",
    "create statement paid after set orders where new.status = 'paid' as insert ledger {o: new.id, amount: new.total}",
    "call paid",
    "drop collection scratch",
];

fn ql_parse(_: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    for text in TEXTS {
        fennec_ql::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
    }
    Box::new(|_| {
        for text in TEXTS {
            black_box(fennec_ql::parse(black_box(text)).unwrap());
        }
    })
}

// ------------------------------------------------------------------ http

const ROOT: &str = "root-token";
const SECRET: &[u8] = b"thirty-two bytes and a few more, for HS256";
/// The rows the HTTP workloads read and write over, in turn.
const ROWS: usize = 500;

fn request(method: Method, target: &str, headers: &[(&str, &str)], body: &str) -> Request {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = (query.split('&'))
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    Request {
        method,
        target: target.into(),
        path: path.into(),
        query,
        headers: (headers.iter())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.as_bytes().to_vec(),
        keep_alive: true,
    }
}

fn query_body(text: &str, params: &str) -> String {
    let mut body = String::from("{\"query\":");
    fennec_core::json::escape_into(&mut body, text);
    body.push_str(",\"params\":[");
    body.push_str(params);
    body.push_str("]}");
    body
}

/// A server's handler over a file, its config `cfg`: a request answered
/// as the connection's thread answers it, its status held to `ok`.
struct Server {
    db: Arc<RwLock<Database>>,
    cfg: Config,
}

impl Server {
    fn new(dir: &Path, cfg: Config) -> Server {
        let db = file_db(dir, "http.fennec");
        Server {
            db: Arc::new(RwLock::new(db)),
            cfg,
        }
    }

    fn send(&self, req: &Request) -> u16 {
        // The thread's flags, as the connection resets them for each request.
        fennec_http::metrics::begin_request();
        let resp = fennec_http::handle(&self.db, &self.cfg, req);
        assert!(
            resp.status < 300,
            "{} {}: {} {}",
            req.method.name(),
            req.target,
            resp.status,
            String::from_utf8_lossy(&resp.body)
        );
        resp.status
    }

    fn query(&self, auth: &[(&str, &str)], text: &str) {
        self.send(&request(
            Method::Post,
            "/query",
            auth,
            &query_body(text, ""),
        ));
    }

    /// The loop's body: each op's request, made before it, in turn.
    fn requests(self, reqs: Vec<Request>) -> Box<dyn FnMut(usize)> {
        Box::new(move |i| {
            black_box(self.send(&reqs[i % reqs.len()]));
        })
    }
}

/// A config whose idempotency keys outlive any run under valgrind.
fn config() -> Config {
    Config {
        idempotency_ttl: Duration::from_secs(86_400),
        ..Config::default()
    }
}

/// `t (n int, name text)`, ROWS rows of a 40-byte name.
fn http_rows(server: &Server, auth: &[(&str, &str)]) {
    server.query(auth, "create collection t (n int, name text)");
    let name = "x".repeat(40);
    for id in 1..=ROWS {
        server.query(auth, &format!("put t {{id: {id}, n: 0, name: '{name}'}}"));
    }
}

/// `GET /t?id=eq.<id>`: the read of a row by id a filter makes (`rows.rs`).
fn http_get_row(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let server = Server::new(dir, config());
    http_rows(&server, &[]);
    let reqs = (0..ops)
        .map(|i| request(Method::Get, &format!("/t?id=eq.{}", i % ROWS + 1), &[], ""))
        .collect();
    server.requests(reqs)
}

/// `PATCH /t?id=eq.<id>` with `{"n": <i>}`: a row written by a filter over
/// REST (`rows.rs`'s `patch`).
fn http_patch(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let server = Server::new(dir, config());
    http_rows(&server, &[]);
    let reqs = (0..ops)
        .map(|i| {
            let target = format!("/t?id=eq.{}", i % ROWS + 1);
            request(Method::Patch, &target, &[], &format!(r#"{{"n": {i}}}"#))
        })
        .collect();
    server.requests(reqs)
}

/// `put t {id: $1, n: $2}` through `/query`, the ids written over in turn
/// (`replays.rs`'s `put`); with `keyed`, each with a key of its own.
fn http_puts(dir: &Path, ops: usize, keyed: bool) -> Box<dyn FnMut(usize)> {
    let server = Server::new(dir, config());
    server.query(&[], "create collection t (n int)");
    let reqs = (0..ops)
        .map(|i| {
            let body = query_body("put t {id: $1, n: $2}", &format!("{}, {i}", i % ROWS + 1));
            let key = format!("k-{i}");
            let headers: &[(&str, &str)] = match keyed {
                true => &[("Idempotency-Key", key.as_str())],
                false => &[],
            };
            request(Method::Post, "/query", headers, &body)
        })
        .collect();
    server.requests(reqs)
}

fn http_put(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    http_puts(dir, ops, false)
}

fn http_put_keyed(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    http_puts(dir, ops, true)
}

/// A `/batch` of 1 000 puts, one block.
fn http_batch(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let server = Server::new(dir, config());
    server.query(&[], "create collection t (n int)");
    let reqs = (0..ops)
        .map(|i| {
            let lines: Vec<String> = (1..=1_000)
                .map(|id| query_body("put t {id: $1, n: $2}", &format!("{id}, {i}")))
                .collect();
            request(Method::Post, "/batch", &[], &lines.join("\n"))
        })
        .collect();
    server.requests(reqs)
}

/// The subjects the scoped workloads ask as, in turn: each well under
/// `--request-rate`'s default, a bucket of 1 000 it never empties, and
/// each asking again within the rate's window (a second) however slowly
/// callgrind runs, so the window's sweep, which runs on the clock, forgets
/// none. A thousand subjects took two seconds a turn under callgrind,
/// and a hundred, on a loaded machine, more than one at times: how many of
/// them each sweep forgot and the next request took in anew was the
/// clock's, and moved a scoped write's count by 2 to 6% from one run to
/// the next.
const CALLERS: usize = 4;

/// A server holding a token and `policy`, with every caller's token, each
/// checked once before the loop as a server's users' are by the time they
/// ask again; and the server token's header.
fn scoped_server(
    dir: &Path,
    policy: &str,
    claims: impl Fn(usize) -> String,
) -> (Server, Vec<String>) {
    let access = Arc::new(Access::new(SECRET, policy).unwrap());
    let tokens = (0..CALLERS)
        .map(|i| format!("Bearer {}", access.mint(&claims(i)).unwrap()))
        .collect();
    let mut db = file_db(dir, "scoped.fennec");
    db.install_plugin(&CheckPlugin).unwrap();
    let server = Server {
        db: Arc::new(RwLock::new(db)),
        cfg: Config {
            token: Some(ROOT.into()),
            access: Some(access),
            ..config()
        },
    };
    (server, tokens)
}

/// `get t where id = ...` under a scoped token (`t read`), one of 1 000
/// subjects in turn (`rows.rs`'s `scoped`).
fn http_scoped_get(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let (server, tokens) = scoped_server(dir, "t read\n", |i| format!(r#"{{"sub":"user-{i}"}}"#));
    let root = format!("Bearer {ROOT}");
    http_rows(&server, &[("Authorization", root.as_str())]);
    let get = |i: usize| {
        let auth = [("Authorization", tokens[i % CALLERS].as_str())];
        request(
            Method::Get,
            &format!("/t?id=eq.{}", i % ROWS + 1),
            &auth,
            "",
        )
    };
    for i in 0..CALLERS {
        server.send(&get(i));
    }
    server.requests((0..ops).map(get).collect())
}

/// `wakes.rs`'s policy: a customer's own rows, and `orders` bound to a
/// statement the customer may wake.
const WAKES: &str = "plain  read,insert,update  where by = $jwt.sub  for customer
orders  read,update(status)  where by = $jwt.sub  for customer
ledger  read  for customer
paid  wake  for customer
";

/// `wakes.rs`'s scoped writes: a customer's into `plain`, which no
/// statement is bound to, while `orders` beside it is; each caller owns
/// its row of `plain`.
fn wakes_server(dir: &Path) -> (Server, Vec<String>) {
    fennec_ql::install();
    let (server, tokens) = scoped_server(dir, WAKES, |i| {
        format!(r#"{{"sub":"u{i}","role":"customer"}}"#)
    });
    let root = format!("Bearer {ROOT}");
    let auth = [("Authorization", root.as_str())];
    for q in [
        "create collection plain (n int, by text)",
        "create collection orders (status text, total int, by text)",
        "create collection ledger (o int, amount int)",
        "create statement paid after set orders where new.status = 'paid'
         as insert ledger {o: new.id, amount: new.total}",
        "insert orders {status: 'open', total: 10, by: 'u0'}",
    ] {
        server.query(&auth, q);
    }
    for i in 0..CALLERS {
        server.query(&auth, &format!("insert plain {{n: 0, by: 'u{i}'}}"));
    }
    (server, tokens)
}

/// A scoped write's loop: `text` with `params(caller)`, caller after
/// caller, each caller's first request sent once before it.
fn scoped_writes(
    (server, tokens): (Server, Vec<String>),
    ops: usize,
    text: &str,
    params: fn(usize) -> String,
) -> Box<dyn FnMut(usize)> {
    let req = |i: usize| {
        let c = i % CALLERS;
        let auth = [("Authorization", tokens[c].as_str())];
        request(Method::Post, "/query", &auth, &query_body(text, &params(c)))
    };
    for i in 0..CALLERS {
        server.send(&req(i));
    }
    let reqs = (0..ops).map(req).collect();
    server.requests(reqs)
}

/// A customer's `set` of its own row by id (`wakes.rs`'s `scoped set`).
fn http_scoped_set(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let set = "set plain {n: n + 1} where id = $1";
    scoped_writes(wakes_server(dir), ops, set, |c| (c + 1).to_string())
}

/// A customer's `insert` of a row of its own (`wakes.rs`'s `scoped insert`).
fn http_scoped_insert(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let insert = "insert plain {n: 1, by: $1}";
    scoped_writes(wakes_server(dir), ops, insert, |c| format!("\"u{c}\""))
}

// ------------------------------------------------------------------ file

/// A file of `rows` rows of a few fields, written and let go of.
fn written(dir: &Path, name: &str, rows: i64) -> PathBuf {
    let path = dir.join(name);
    let mut db = fennec_core::fs::open(&path).unwrap();
    run(
        &mut db,
        "create collection t (n int, k text @hash, body text)",
        &[],
    );
    let put = stmt("put t {id: $1, n: $2, k: $3, body: $4}");
    for id in 1..=rows {
        let params = [
            Value::Int(id),
            Value::Int(id * 31 % 1_000),
            Value::Text(format!("k{}", id % 100)),
            Value::Text(format!("row {id}, some forty bytes of its own text")),
        ];
        db.execute_with(&put, &params).unwrap();
    }
    path
}

/// A file of 100 000 rows opened: its records read into the offsets, no
/// index built (`make open-bench`).
fn file_open(dir: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let path = written(dir, "open.fennec", 100_000);
    let mut opened = Vec::new();
    Box::new(move |_| opened.push(fennec_core::fs::open(&path).unwrap()))
}

/// A file of 10 000 rows each written twice, compacted: its live half
/// written into a new file.
fn file_compact(dir: &Path, _: usize) -> Box<dyn FnMut(usize)> {
    let path = written(dir, "compact.fennec", 10_000);
    let mut db = fennec_core::fs::open(&path).unwrap();
    let set = stmt("set t {n: n + 1} where id = $1");
    for id in 1..=10_000 {
        db.execute_with(&set, &[Value::Int(id)]).unwrap();
    }
    let compact = stmt("compact");
    Box::new(move |_| {
        black_box(db.execute(&compact).unwrap());
    })
}
