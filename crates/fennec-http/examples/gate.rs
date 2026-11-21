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
//! `file-*` a file's open and compaction, `sync-*` a replica's sync. A
//! workload's statements are FennecQL texts, which 1.x keeps, so the same
//! program builds against main and a pull request alike; each workload
//! stands for the claim the table in docs/design/benchmarks.md names.
//!
//! Beside the count, the program tallies what its loop asks of the
//! allocator (`Counting`): the calls, and the bytes they ask for, printed
//! as a `heap:` line the gate's driver reads.
//!
//! It is also the client of the gate's I/O workloads (`IO_WORKLOADS`,
//! `--list-io`): requests to a real `fennec-server` that the driver runs
//! under `strace -f`, counting the server's fsyncs, renames and bytes
//! written to files, never this program's.
//!
//!     gate --list-io
//!     gate io-setup io-put 127.0.0.1:8080; gate io io-put 127.0.0.1:8080

use fennec_core::prelude::*;
use fennec_http::access::{Access, CheckPlugin};
use fennec_http::http::{Method, Request};
use fennec_http::Config;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
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
    ("sync-apply", 20, sync_apply),
];

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "--list" {
        for (name, ops, _) in WORKLOADS {
            println!("{name} {ops}");
        }
        return;
    }
    // Apart from `--list`, which a base's checker before the I/O gate reads
    // whole and would count under callgrind.
    if arg == "--list-io" {
        for (name, requests, _, _) in IO_WORKLOADS {
            println!("{name} {requests}");
        }
        return;
    }
    if arg == "io-setup" || arg == "io" {
        io_main(&arg);
        return;
    }
    let Some(&(name, ops, setup)) = WORKLOADS.iter().find(|w| w.0 == arg) else {
        eprintln!("usage: gate --list | gate WORKLOAD");
        std::process::exit(2);
    };
    if let Some(bytes) = leak_asked() {
        leak(bytes);
        println!("leaked {bytes} bytes before the setup");
    }
    let dir = std::env::temp_dir().join(format!("fennec-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = setup(&dir, ops);
    let t = Instant::now();
    let before = Heap::now();
    callgrind::request(callgrind::START_INSTRUMENTATION);
    callgrind::request(callgrind::TOGGLE_COLLECT);
    for i in 0..ops {
        body(i);
    }
    callgrind::request(callgrind::TOGGLE_COLLECT);
    callgrind::request(callgrind::STOP_INSTRUMENTATION);
    let heap = Heap::now().since(&before);
    let took = t.elapsed();
    println!(
        "{name} {ops} ops, {:.0} ns an op",
        took.as_nanos() as f64 / ops as f64
    );
    println!(
        "heap: calls={} bytes={} zeroed={} grown={}",
        heap.calls, heap.bytes, heap.zeroed, heap.grown
    );
    // The data is let go of uncounted, and natively in no time worth one.
    drop(body);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The global allocator: `System`'s -- musl's in the build the gate counts --
/// with every call counted, and the bytes each asks for. Callgrind's figure
/// prices a call into the allocator at what the base's runs paid one
/// (docs/design/benchmarks.md#the-gate), so what grows inside a call with
/// the calls the same goes unseen there: an allocation past its size class,
/// a `Vec` grown by `realloc`'s copies, `calloc`'s zeroing. The bytes see
/// it, and depend on the code alone, never on where the heap put a block:
/// an allocation's size, a `realloc`'s growth and a zeroed allocation's
/// size each count once. A counter is an atomic, as a workload's threads
/// share them, and the same few instructions a call in both builds.
struct Counting;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static ZEROED: AtomicU64 = AtomicU64::new(0);
static GROWN: AtomicU64 = AtomicU64::new(0);

// SAFETY: each call is `System`'s own, under the caller's contract; the
// counters beside it touch no memory the allocator hands out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc` takes it.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        ZEROED.fetch_add(layout.size() as u64, Relaxed);
        // SAFETY: as `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CALLS.fetch_add(1, Relaxed);
        // SAFETY: `ptr` came from this allocator with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        let grown = new_size.saturating_sub(layout.size()) as u64;
        BYTES.fetch_add(grown, Relaxed);
        GROWN.fetch_add(grown, Relaxed);
        // SAFETY: `ptr` came from this allocator with `layout`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocator's tallies at a moment; two of them, a stretch's.
struct Heap {
    calls: u64,
    bytes: u64,
    zeroed: u64,
    grown: u64,
}

impl Heap {
    fn now() -> Heap {
        Heap {
            calls: CALLS.load(Relaxed),
            bytes: BYTES.load(Relaxed),
            zeroed: ZEROED.load(Relaxed),
            grown: GROWN.load(Relaxed),
        }
    }

    fn since(&self, start: &Heap) -> Heap {
        Heap {
            calls: self.calls - start.calls,
            bytes: self.bytes - start.bytes,
            zeroed: self.zeroed - start.zeroed,
            grown: self.grown - start.grown,
        }
    }
}

/// The heap-shift control's leak (docs/design/benchmarks.md#the-gate),
/// asked for by `FENNEC_GATE_LEAK=<bytes>`: this program alone reads it,
/// and the gate's runs never set it.
fn leak_asked() -> Option<usize> {
    let v = std::env::var("FENNEC_GATE_LEAK").ok()?;
    Some(
        v.parse()
            .unwrap_or_else(|_| panic!("FENNEC_GATE_LEAK is a count of bytes: {v:?}")),
    )
}

/// `bytes` held for good before the setup, in blocks of six size classes,
/// so that every later allocation lands elsewhere in musl's groups: the
/// same program over another heap layout, which a change anywhere makes.
/// The figure with the allocator priced, and the bytes, must not move.
fn leak(bytes: usize) {
    const SIZES: [usize; 6] = [24, 56, 120, 248, 504, 1_016];
    let (mut left, mut i) = (bytes, 0);
    while left > 0 {
        let n = SIZES[i % SIZES.len()].min(left);
        // Out of the optimizer's sight, or an allocation never read is
        // one it may leave out.
        std::mem::forget(black_box(Vec::<u8>::with_capacity(n)));
        left -= n;
        i += 1;
    }
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

/// The brands of every row, counted through the `@hash` buckets -- a
/// bucket's length a brand -- and the commonest picked by `order_rows`.
/// The buckets come in the order of the index's table, which its hash keys
/// lay out, a draw a map (std's `RandomState`). Picking ten (`top 10`),
/// `select_nth_unstable_by` and the sort compare as many times as that
/// order makes them, 87 to 92 a facet: one database's facet cost 9.7%
/// apart over ten processes, the mean of 256 of them still 1.2%. Picking
/// one is a pass of 39 comparisons whatever the order, the walk and every
/// value's decoding counted as before: 0.003% apart over ten runs. The
/// pick of ten is counted by `core-lookup`, the first ten of some 500 rows
/// in id order, which a seed fixes, and the sort by `core-group`'s 100
/// groups. Each op still asks the next of FACETED databases, each with its
/// own keys: what is left still steps with the table's layout, 12
/// instructions an op apart over ten runs of one database, 2 of 256. A
/// facet's cost is its buckets', not its rows': each holds 40 brands,
/// brand `b` on `b + 1` rows.
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
    let facet = stmt("get f select id limit 0 facet brand top 1");
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

/// `get t where id = ...` under a scoped token (`t read`), one of CALLERS
/// subjects in turn (`rows.rs`'s `scoped` asks as 1 000: CALLERS says why
/// the gate does not).
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

// ------------------------------------------------------------------ sync

/// A sync replica's row as the server's stream sends it (`sync_bench.rs`'s
/// text row), its `n` the change that wrote it.
fn synced_row(id: usize, n: usize) -> String {
    format!(
        r#"{{"id":{id},"key":"k{id}","title":"a title of some forty characters, {id}","n":{n}}}"#
    )
}

/// A change event of 100 rows, ids `first` on, as `seq`.
fn change_event(seq: usize, first: usize) -> Vec<u8> {
    let rows: Vec<String> = (first..first + 100).map(|id| synced_row(id, seq)).collect();
    format!(
        "event: change\ndata: {{\"seq\":{seq},\"puts\":[{}],\"dels\":[],\"schema\":false}}\n\n",
        rows.join(",")
    )
    .into_bytes()
}

/// A replica's sync applying a change of 100 rows (`make sync-bench`'s
/// "of 100 rows"): the event's bytes read off the stream, its rows parsed
/// and written over the 1 000 the replica holds, with the shape's cursor,
/// in one block, into a file as an app keeps its replica.
fn sync_apply(dir: &Path, ops: usize) -> Box<dyn FnMut(usize)> {
    let mut db = file_db(dir, "replica.fennec");
    run(
        &mut db,
        "create collection t (key text @hash, title text, n int)",
        &[],
    );
    let config = r#"{"url":"http://s","seed":"00","shapes":[{"collection":"t","key":"key"}]}"#;
    let mut sync = fennec_abi::sync::Sync::start(&mut db, config).unwrap();
    sync.actions();
    sync.opened(&mut db, 1, 200, "");
    sync.bytes(
        &mut db,
        1,
        b"event: seed\ndata: {\"seq\":1,\"rows\":[]}\n\n",
    );
    sync.actions();
    for b in 0..10 {
        sync.bytes(&mut db, 1, &change_event(2 + b, 1 + b * 100));
        sync.actions();
    }
    // A change refused stops the stream and says why in the status, and
    // the loop would count a refusal's work: held to the rows it wrote.
    let held = db.query(&stmt("get t select id"), &[]).unwrap();
    let held = held.rows().map_or(0, |r| r.rows.len());
    assert!(
        held == 1_000 && sync.status().contains("\"error\":null"),
        "the replica took its changes: {held} rows, {}",
        sync.status()
    );
    let events: Vec<Vec<u8>> = (0..ops)
        .map(|i| change_event(12 + i, 1 + i % 10 * 100))
        .collect();
    Box::new(move |i| {
        sync.bytes(&mut db, 1, &events[i]);
        black_box(sync.actions());
    })
}

// ------------------------------------------------------------------ io

/// The I/O gate's workloads (docs/design/benchmarks.md#the-gate): requests
/// to a real `fennec-server` under `--sync always`, which
/// `tools/perf_gate.py io` runs under `strace -f`, counting the fsyncs,
/// renames and bytes written to files of the server alone. Each its name,
/// the requests its run sends -- each answered before the next, one
/// writer -- its setup and its run. `io-setup` makes the data over one
/// connection and `io` sends the run's requests over the next: the driver
/// counts from the server taking that second connection to its taking a
/// third, its own, once the run is answered. Nothing here is drawn or read
/// off a clock -- the ids, values and texts are fixed, and so is the
/// `Host`, not the port -- so a build's counts are the same run after run.
const IO_WORKLOADS: &[(&str, usize, IoPhase, IoPhase)] = &[
    ("io-put", 500, io_collection, io_put),
    ("io-batch", 1, io_collection, io_batch),
    ("io-spill", 1, io_collection, io_spill),
    ("io-compact", 1, io_half_dead, io_compact),
];

/// A phase of an I/O workload: its requests, over the one connection.
type IoPhase = fn(&mut Client);

/// `gate io-setup|io WORKLOAD ADDRESS`: a phase of an I/O workload against
/// the server at ADDRESS, and a line of what it sent, which the driver
/// reads: the requests answered, the rows they wrote and the bytes sent.
fn io_main(phase: &str) {
    let mut args = std::env::args().skip(2);
    let (Some(name), Some(addr)) = (args.next(), args.next()) else {
        eprintln!("usage: gate io-setup|io WORKLOAD ADDRESS");
        std::process::exit(2);
    };
    let Some(&(_, requests, setup, run)) = IO_WORKLOADS.iter().find(|w| w.0 == name) else {
        eprintln!("no I/O workload {name}: gate --list-io");
        std::process::exit(2);
    };
    let mut client = Client::connect(&addr);
    if phase == "io-setup" {
        setup(&mut client);
    } else {
        run(&mut client);
        assert_eq!(
            client.answered, requests,
            "{name} sends {requests} requests"
        );
    }
    println!(
        "{phase}: {name} answered={} rows={} sent={}",
        client.answered, client.rows, client.sent
    );
}

/// One connection to the server, a request in flight at a time, and what
/// went over it: std's socket alone, so the program builds against any
/// base's crates.
struct Client {
    stream: TcpStream,
    answers: BufReader<TcpStream>,
    answered: usize,
    rows: u64,
    sent: u64,
}

impl Client {
    fn connect(addr: &str) -> Client {
        let stream = TcpStream::connect(addr).unwrap_or_else(|e| panic!("{addr}: {e}"));
        // Each request goes in one write; Nagle's wait for the answer
        // before a second segment would only slow the run.
        stream.set_nodelay(true).unwrap();
        // A server that never answers fails the run rather than hold it.
        stream
            .set_read_timeout(Some(Duration::from_secs(300)))
            .unwrap();
        let answers = BufReader::new(stream.try_clone().unwrap());
        Client {
            stream,
            answers,
            answered: 0,
            rows: 0,
            sent: 0,
        }
    }

    /// `POST path` with `body`, writing `rows` rows, its answer read and
    /// held to a 2xx. The `Host` is the name, not the port the system
    /// picked, so the bytes sent are the same every run.
    fn post(&mut self, path: &str, body: &str, rows: u64) {
        let mut req = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        req.extend_from_slice(body.as_bytes());
        self.stream.write_all(&req).unwrap();
        self.sent += req.len() as u64;
        let (status, answer) = self.answer();
        assert!(
            (200..300).contains(&status),
            "POST {path}: {status} {}",
            String::from_utf8_lossy(&answer[..answer.len().min(2_000)])
        );
        self.answered += 1;
        self.rows += rows;
    }

    /// The next answer's status and body, by its `Content-Length`.
    fn answer(&mut self) -> (u16, Vec<u8>) {
        let mut line = String::new();
        self.answers.read_line(&mut line).unwrap();
        let status = (line.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {line:?}"));
        let mut length = None;
        loop {
            let mut header = String::new();
            self.answers.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse::<usize>().ok();
                }
            }
        }
        let n = length.unwrap_or_else(|| panic!("an answer without a Content-Length: {line:?}"));
        let mut body = vec![0; n];
        self.answers.read_exact(&mut body).unwrap();
        (status, body)
    }

    fn query(&mut self, text: &str, rows: u64) {
        let mut body = String::from("{\"query\":");
        fennec_core::json::escape_into(&mut body, text);
        body.push('}');
        self.post("/query", &body, rows);
    }
}

/// Row `id`'s text, `len` bytes: its id, then filler. Digits and letters,
/// which JSON writes as they are.
fn io_text(id: usize, len: usize) -> String {
    let mut text = format!("{id:06} ");
    while text.len() < len {
        text.push_str("fennec ");
    }
    text.truncate(len);
    text
}

/// A `/batch` line, or a `/query` body: `put t` of row `id`, its `n` and a
/// text of `len` bytes, as parameters.
fn io_put_line(id: usize, n: usize, len: usize) -> String {
    format!(
        r#"{{"query":"put t {{id: $1, n: $2, body: $3}}","params":[{id},{n},"{}"]}}"#,
        io_text(id, len)
    )
}

/// A `/batch` of a put for each of `ids`, each of a text of `len` bytes.
fn io_batch_of(client: &mut Client, ids: std::ops::RangeInclusive<usize>, n: usize, len: usize) {
    let rows = ids.clone().count() as u64;
    let lines: Vec<String> = ids.map(|id| io_put_line(id, n, len)).collect();
    client.post("/batch", &lines.join("\n"), rows);
}

fn io_collection(client: &mut Client) {
    client.query("create collection t (n int, body text)", 0);
}

/// 500 puts of a row of a 530-byte text, each answered -- durable -- before
/// the next: an fsync each, of the sync log, whose 256 KB of entries fill
/// some 450 puts in -- the file fsynced in its place and a generation of
/// the log begun -- so a change to the log's room moves the counts either
/// way. Each put waits for its fsync, most of a run's time: 1 000 puts of
/// 200-byte rows paid twice the fsyncs and never filled the log.
fn io_put(client: &mut Client) {
    for id in 1..=500 {
        client.post("/query", &io_put_line(id, id, 530), 1);
    }
}

/// A `/batch` of 1 000 puts of a row of a 16-byte text: one block of some
/// 24 KB, under the 64 KB a sync log's entry takes.
fn io_batch(client: &mut Client) {
    io_batch_of(client, 1..=1_000, 0, 16);
}

/// A `/batch` of 2 000 rows of a 9 000-byte text, some 18 MB: one block,
/// which spills into the file past 16 MB before it lands. Rows this long
/// keep the statements to parse and answer few, and the run short: the
/// spill's I/O is the block's bytes, however many rows hold them.
fn io_spill(client: &mut Client) {
    io_batch_of(client, 1..=2_000, 0, 9_000);
}

/// 10 000 rows of a 200-byte text written, then each written again: a
/// file half of which is dead.
fn io_half_dead(client: &mut Client) {
    io_collection(client);
    io_batch_of(client, 1..=10_000, 0, 200);
    io_batch_of(client, 1..=10_000, 1, 200);
}

/// The half-dead file compacted: its live half written into a new file,
/// renamed over it.
fn io_compact(client: &mut Client) {
    client.query("compact", 0);
}
