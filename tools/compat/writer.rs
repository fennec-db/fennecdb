//! The part of a release's fixture set (`tools/compat_fixtures.py`) its
//! binaries cannot write at a few kilobytes, written by that release's own
//! engine: built as an example of the release's `fennec-core`
//! (`compat_fixtures.py build` copies it into the tag's tree), so every byte
//! it writes is that release's code's.
//!
//! - `spill <file>`: a block that spills (kinds 10 and 11). A server spills
//!   a block only past 16 MB (`SPILL_AT`); `set_spill` is the engine's own
//!   knob, so the records are the ones a 16 MB block writes, at 2 KB.
//! - `synclog <file>`: a file and the sync log beside it after the machine
//!   went down. The log is Linux's and Android's (`synclog::ENABLED`);
//!   `keep_sync_log` makes this build keep it, as the release's own tests
//!   do on macOS.
//! - `kept <file>`: a graph kept in the file's tail (kind 4, version 10),
//!   which a server appends once 10 000 nodes changed and the file grew by
//!   three times the graph (`GRAPH_SAVE_CHANGES`); `set_graph_saves` lowers
//!   both, `save_graphs` is what the server's keeper calls.
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

fn spill(path: &str) {
    let mut db = fennec_core::fs::open(path).unwrap_or_else(|e| fail(&e.to_string()));
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
    // One more block after it, small enough to land as a block record.
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
    let mut db = fennec_core::fs::open(path).unwrap_or_else(|e| fail(&e.to_string()));
    run(&mut db, "create collection events (n int @hash, what text, at timestamp @sorted)");
    durable(&mut db);
    for k in 0..12 {
        run(
            &mut db,
            &format!("put events {{n: {k}, what: \"event {k}\", at: {}}}", 1_767_225_600_000i64 + k * 60_000),
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
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| fail(&e.to_string()));
    f.set_len(at).unwrap_or_else(|e| fail(&e.to_string()));
    f.sync_all().unwrap_or_else(|e| fail(&e.to_string()));
}

fn kept(path: &str) {
    let mut db = fennec_core::fs::open(path).unwrap_or_else(|e| fail(&e.to_string()));
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
            &format!("put points {{n: {k}, v: [{}], w: [{}]}}", v.join(", "), w.join(", ")),
        );
    }
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
    run(&mut db, "put points {n: 100, v: [0.1, 0.2, 0.3, 0.4], w: [0, 0, 0, 0, 1, 1, 1, 1]}");
    run(&mut db, "del points where n = 3");
    db.sync().unwrap_or_else(|e| fail(&e.to_string()));
}

fn answers(path: &str, statements: &str, out: &str) {
    fennec_ql::install();
    let db = fennec_core::fs::open_read_only(path).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    let text = std::fs::read_to_string(statements).unwrap_or_else(|e| fail(&e.to_string()));
    let mut lines = String::new();
    for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with("--")) {
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
        ["answers", file, statements, out] => answers(file, statements, out),
        _ => fail("usage: compat_writer spill|synclog|kept <file> | answers <file> <statements> <out>"),
    }
}
