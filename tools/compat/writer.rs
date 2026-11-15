//! The part of a release's fixture set (`tools/compat_fixtures.py`) its
//! binaries cannot write at a few kilobytes, or on the machine writing the
//! set, written by that release's own engine: built as an example of the
//! release's `fennec-core` (`compat_fixtures.py build` copies it into the
//! tag's tree), so every byte it writes is that release's code's. Where it
//! turns a knob, the knob is one the engine keeps for its own tests, and
//! the records are the ones the threshold it lowers would have written.
//!
//! - `spill <file>`: a block that spills and lands (kinds 10 and 11), and
//!   one that spills and is put back, its spills left dead in the file
//!   with a write after them. A server spills a block only past 16 MB
//!   (`SPILL_AT`); `set_spill` is the engine's own knob, so the records
//!   are the ones a 16 MB block writes, at 2 KB.
//! - `synclog <file>`: a file and the sync log beside it after the machine
//!   went down. The log is Linux's and Android's (`synclog::ENABLED`);
//!   `keep_sync_log` makes this build keep it, as the release's own tests
//!   do on macOS. The machine's crash is the process ending with no close
//!   (`mem::forget`) and the file cut back to where the log's generation
//!   began, as a power cut leaves a file whose last fsync was there.
//! - `kept <file>`: a graph kept in the file's tail (kind 4, version 10),
//!   tombstones in it, which a server appends once 10 000 nodes changed and
//!   the file grew by three times the graph (`GRAPH_SAVE_CHANGES`);
//!   `set_graph_saves` lowers both, `save_graphs` is what the server's
//!   keeper calls.
//! - `waiting <file>`: an image whose graphs hold nodes waiting to be
//!   linked (flag 2): a server opens its file leaving the tail's vectors
//!   unlinked (`fs::open_serving`) and a checkpoint before its linker took
//!   them writes them so. Opened as a server opens it and checkpointed at
//!   once, before any thread links one.
//! - `torn-tail <file>`, `torn-mark <file>`: what a crash leaves -- a last
//!   record cut short (the process ended, no close, the file cut inside the
//!   record it was appending), and a close's mark that the file was synced
//!   (kind 14) whose bytes past its head never reached the disk, read back
//!   as zeros.
//! - `answers <file> <statements> <out>`: the release's answer to each
//!   statement of a list, a line each as `json::response_to_string` writes
//!   it (or `{"error": ...}`), the file opened read-only.
//!
//! The Rust crates' API is not frozen: a later release's copy of this file
//! follows its engine's names.

use fennec_core::prelude::*;
use std::io::Write;

fn fail(m: &str) -> ! {
    eprintln!("compat_writer: {m}");
    std::process::exit(1)
}

fn run(db: &mut Database, text: &str) {
    for stmt in fennec_ql::parse_for(db, text).unwrap_or_else(|e| fail(&format!("{text}: {e}"))) {
        db.execute_with(&stmt, &[])
            .unwrap_or_else(|e| fail(&format!("{text}: {e}")));
    }
}

fn durable(db: &mut Database) {
    if let Some(d) = db.flush().unwrap_or_else(|e| fail(&e.to_string())) {
        d().unwrap_or_else(|e| fail(&e.to_string()));
    }
}

fn open(path: &str) -> Database {
    fennec_core::fs::open(path).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

/// A vector of `dim` numbers from `seed` alone, none two alike.
fn vector(seed: i64, dim: usize) -> String {
    let mut x = (seed as u64)
        .wrapping_mul(2_654_435_761)
        .wrapping_add(12_345)
        & 0xFFFF_FFFF;
    let v: Vec<String> = (0..dim)
        .map(|_| {
            x = (x.wrapping_mul(1_103_515_245).wrapping_add(12_345)) & 0x7FFF_FFFF;
            format!("{:.4}", (x % 10_000) as f64 / 10_000.0 - 0.5)
        })
        .collect();
    format!("[{}]", v.join(", "))
}

fn spill(path: &str) {
    let mut db = open(path);
    run(&mut db, "create collection notes (n int @hash, body text)");
    run(&mut db, "create collection tags (n int, tag text @hash)");
    db.set_spill(2048);
    let note = fennec_ql::parse_one("put notes {n: $1, body: $2}").unwrap();
    let tag = fennec_ql::parse_one("put tags {n: $1, tag: $2}").unwrap();
    let params: Vec<Vec<Value>> = (0..60)
        .map(|k| vec![Value::Int(k), Value::Text(format!("note {k} ").repeat(12))])
        .collect();
    let tags: Vec<Vec<Value>> = (0..60)
        .map(|k| vec![Value::Int(k), Value::Text(format!("t{}", k % 7))])
        .collect();
    let mut block: Vec<(&Statement, &[Value])> = Vec::new();
    for (p, t) in params.iter().zip(&tags) {
        block.push((&note, &p[..]));
        block.push((&tag, &t[..]));
    }
    db.execute_block(&block)
        .unwrap_or_else(|(at, e)| fail(&format!("the block's statement {at}: {e}")));
    // A block that spills as it grows and is then put back: its last
    // statement writes over an id the collection holds (`insert`), so the
    // spills before it are left in the file, dead, no land naming them.
    let later: Vec<Vec<Value>> = (100..160)
        .map(|k| {
            vec![
                Value::Int(k),
                Value::Text(format!("put back {k} ").repeat(12)),
            ]
        })
        .collect();
    let clash = fennec_ql::parse_one("insert notes {id: 1, n: 1000, body: \"a held id\"}").unwrap();
    let mut back: Vec<(&Statement, &[Value])> = later.iter().map(|p| (&note, &p[..])).collect();
    back.push((&clash, &[]));
    match db.execute_block(&back) {
        Err((at, _)) if at == back.len() - 1 => {}
        Err((at, e)) => fail(&format!(
            "the block put back failed at {at}, not its last: {e}"
        )),
        Ok(_) => fail("the block meant to be put back landed"),
    }
    // A write after them, so the dead spills stay in the file: the ones a
    // file ends with an open cuts off, as after a torn record.
    db.set_spill(u64::MAX);
    run(&mut db, "del notes where n >= 50");
    db.sync().unwrap_or_else(|e| fail(&e.to_string()));
}

/// Where the file stood synced when the log's generation began, from its
/// header: the place the machine's crash cuts the file back to.
fn base(path: &str) -> u64 {
    let log = std::fs::read(format!("{path}.sync")).unwrap_or_else(|e| fail(&e.to_string()));
    if log.get(..8) != Some(&b"FENECSYN"[..]) {
        fail("no sync log in force");
    }
    u64::from_le_bytes(log[24..32].try_into().unwrap())
}

fn synclog(path: &str) {
    fennec_core::fs::keep_sync_log(true);
    let mut db = open(path);
    run(
        &mut db,
        "create collection events (n int @hash, what text, at timestamp @sorted)",
    );
    durable(&mut db);
    for k in 0..12 {
        run(
            &mut db,
            &format!(
                "put events {{n: {k}, what: \"event {k}\", at: {}}}",
                1_767_225_600_000i64 + k * 60_000
            ),
        );
        durable(&mut db);
    }
    run(&mut db, "set events {what: \"seen\"} where n < 3");
    durable(&mut db);
    run(&mut db, "del events where n = 11");
    durable(&mut db);
    // The process ends with no close, and the machine with it: the file
    // keeps what its own fsync covered, the log the rest.
    std::mem::forget(db);
    let at = base(path);
    cut(path, at);
}

fn cut(path: &str, len: u64) {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| fail(&e.to_string()));
    f.set_len(len).unwrap_or_else(|e| fail(&e.to_string()));
    f.sync_all().unwrap_or_else(|e| fail(&e.to_string()));
}

fn kept(path: &str) {
    let mut db = open(path);
    run(
        &mut db,
        "create collection points (n int @hash, v vector<4> @hnsw(cosine, m=8), w vector<8> @hnsw(l2, m=8, quant=int8))",
    );
    for k in 0..48i64 {
        let f = |i: i64| ((k * 7 + i * 13) % 17) as f64 / 17.0 - 0.5;
        let v: Vec<String> = (0..4).map(|i| format!("{:.4}", f(i))).collect();
        let w: Vec<String> = (0..8).map(|i| format!("{:.4}", f(i + 4))).collect();
        run(
            &mut db,
            &format!(
                "put points {{n: {k}, v: [{}], w: [{}]}}",
                v.join(", "),
                w.join(", ")
            ),
        );
    }
    // Rows of a vector of their own (17 apart from every other), deleted
    // before the graph is kept: tombstones in the kept record.
    run(&mut db, "put points {n: 60, v: [0.9, 0.1, -0.3, 0.2], w: [0.9, 0.1, -0.3, 0.2, 0.4, -0.4, 0.3, 0.1]}");
    run(&mut db, "put points {n: 61, v: [-0.2, 0.8, 0.5, -0.1], w: [-0.2, 0.8, 0.5, -0.1, 0.3, 0.2, -0.6, 0.7]}");
    run(&mut db, "del points where n in [60, 61]");
    durable(&mut db);
    db.set_graph_saves(1, 0);
    let (n, d) = db.save_graphs().unwrap_or_else(|e| fail(&e.to_string()));
    if n == 0 {
        fail("no graph was due");
    }
    if let Some(d) = d {
        d().unwrap_or_else(|e| fail(&e.to_string()));
    }
    // Writes after the kept graphs: an open restores each where its record
    // is and links only what came after it.
    run(
        &mut db,
        "put points {n: 100, v: [0.1, 0.2, 0.3, 0.4], w: [0, 0, 0, 0, 1, 1, 1, 1]}",
    );
    run(&mut db, "del points where n = 3");
    db.sync().unwrap_or_else(|e| fail(&e.to_string()));
}

fn waiting(path: &str) {
    let mut db = open(path);
    run(
        &mut db,
        "create collection wait (n int @hash, v vector<4> @hnsw(cosine, m=8), q vector<4> @hnsw(l2, m=8, quant=int8))",
    );
    let put = |db: &mut Database, k: i64| {
        let (v, q) = (vector(300 + k, 4), vector(400 + k, 4));
        run(db, &format!("put wait {{n: {k}, v: {v}, q: {q}}}"));
    };
    for k in 0..20 {
        put(&mut db, k);
    }
    // The graphs in the image, then writes after it: what a server opened
    // on the file leaves for its linker.
    db.checkpoint().unwrap_or_else(|e| fail(&e.to_string()));
    for k in 20..30 {
        put(&mut db, k);
    }
    db.sync().unwrap_or_else(|e| fail(&e.to_string()));
    drop(db);
    let mut db = fennec_core::fs::open_serving(path, true, Box::new(Ok))
        .unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    if db.unlinked() == 0 {
        fail("an open as a server's left no vector waiting");
    }
    db.checkpoint().unwrap_or_else(|e| fail(&e.to_string()));
    db.sync().unwrap_or_else(|e| fail(&e.to_string()));
}

/// A collection and rows, each its own record, the last written into the
/// file past the last fsync: a crash may leave it short.
fn rows(path: &str) -> Database {
    let mut db = open(path);
    run(&mut db, "create collection t (n int @hash, s text)");
    for k in 0..5 {
        run(&mut db, &format!("put t {{n: {k}, s: \"row {k}\"}}"));
    }
    durable(&mut db);
    run(&mut db, "put t {n: 5, s: \"the row a crash tore\"}");
    // Written behind the lock and not synced: the durability is dropped.
    drop(db.flush().unwrap_or_else(|e| fail(&e.to_string())));
    db
}

fn torn_tail(path: &str) {
    let db = rows(path);
    // The process ends inside the append: no close, and the last record's
    // last bytes never written.
    std::mem::forget(db);
    let len = std::fs::metadata(path)
        .unwrap_or_else(|e| fail(&e.to_string()))
        .len();
    cut(path, len - 3);
}

fn torn_mark(path: &str) {
    let mut db = rows(path);
    durable(&mut db);
    let before = std::fs::metadata(path)
        .unwrap_or_else(|e| fail(&e.to_string()))
        .len();
    // The close appends its mark after an fsync covered everything, with no
    // fsync of its own: lost in part, its later bytes read back as zeros.
    drop(db);
    let mut bytes = std::fs::read(path).unwrap_or_else(|e| fail(&e.to_string()));
    let mark = &mut bytes[before as usize..];
    if mark.first().map(|k| k & 0x1f) != Some(14) {
        fail("the close appended no mark");
    }
    // Its head -- the kind, collection 0 and a length under 128, a byte
    // each -- reached the disk; its claim and sum did not.
    if mark.len() < 8 || mark[1] != 0 || mark[2] as usize != mark.len() - 3 {
        fail("the mark is not a head of three bytes and its claim and sum");
    }
    mark[3..].iter_mut().for_each(|b| *b = 0);
    std::fs::write(path, bytes).unwrap_or_else(|e| fail(&e.to_string()));
}

fn answers(path: &str, statements: &str, out: &str) {
    fennec_ql::install();
    let db =
        fennec_core::fs::open_read_only(path).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    let text = std::fs::read_to_string(statements).unwrap_or_else(|e| fail(&e.to_string()));
    let mut lines = String::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("--"))
    {
        let answer = match fennec_ql::parse_for(&db, line) {
            Ok(stmts) if stmts.len() == 1 => match db.query(&stmts[0], &[]) {
                Ok(r) => fennec_core::json::response_to_string(&r),
                Err(e) => fennec_core::json::error_to_string(&e),
            },
            Ok(_) => fail(&format!("{line}: one statement a line")),
            Err(e) => fennec_core::json::error_to_string(&e),
        };
        lines.push_str(&answer);
        lines.push('\n');
    }
    std::fs::File::create(out)
        .and_then(|mut f| f.write_all(lines.as_bytes()))
        .unwrap_or_else(|e| fail(&e.to_string()));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["spill", file] => spill(file),
        ["synclog", file] => synclog(file),
        ["kept", file] => kept(file),
        ["waiting", file] => waiting(file),
        ["torn-tail", file] => torn_tail(file),
        ["torn-mark", file] => torn_mark(file),
        ["answers", file, statements, out] => answers(file, statements, out),
        _ => fail(
            "usage: compat_writer spill|synclog|kept|waiting|torn-tail|torn-mark <file> | answers <file> <statements> <out>",
        ),
    }
}
