//! The part of a release's fixture set (`tools/compat_fixtures.py`) its
//! binaries cannot write at a few kilobytes, or on the machine writing the
//! set, written by that release's own engine: built as an example of the
//! release's `fennec-core` (`compat_fixtures.py build` copies it into the
//! tag's tree), so every byte it writes is that release's code's. Where it
//! turns a knob, the knob is one the engine keeps for its own tests, and
//! the records are the ones the threshold it lowers would have written.
//!
//! - `spill <file>`: a block that spills and lands (kinds 10 and 11) --
//!   every schema change a block takes at its front, spilled with its first
//!   writes, and at its end, landing with it -- and one that spills and is
//!   put back, its spills left dead in the file with a write after them. A
//!   server spills a block only past 16 MB (`SPILL_AT`); `set_spill` is the
//!   engine's own knob, so the records are the ones a 16 MB block writes,
//!   at 2 KB.
//! - `synclog <file>`: a file and the sync log beside it after the machine
//!   went down, the log in its first pass, zeros after its entries. The
//!   log is Linux's and Android's (`synclog::ENABLED`); `keep_sync_log`
//!   makes this build keep it, as the release's own tests do on macOS. The
//!   machine's crash is the process ending with no close (`mem::forget`)
//!   and the file cut back to where the log's generation began, as a power
//!   cut leaves a file whose last fsync was there.
//! - `synclog-torn <file>`: the log past its first pass -- syncs of 60 KB
//!   filled its room, the file was synced and a generation begun, and the
//!   syncs after wrote over the front of the last pass's entries -- and the
//!   same crash inside its last write: that entry's head on the disk, the
//!   back half of its bytes zeros, the last pass's bytes after it. The
//!   write that entry held is lost, the ones before it put back.
//! - `torn-wide <file>`: what no small file holds -- a record's claim of
//!   two bytes (one more than 1 MiB past the last fsync), a record's length
//!   of four bytes (a write of 2.3 MB), collection ids of two, three and
//!   four bytes (2 097 152 collections made and dropped first, gone with a
//!   compact), and an image's index of a data record in two shares (16 400
//!   documents, past `INDEX_SHARE`) -- and two-byte claims that decide what
//!   an open reads: a synced row with a bit a disk flipped, below the point
//!   the claims name, which an open reads unchecked, and the first write
//!   after that fsync with a page a power cut lost, past it, where an open
//!   cuts the file; the process gone with no close, every record an open
//!   looks at for the point claiming in two bytes. `fennec verify` refuses
//!   the file.
//!   No knob: the writes as a server makes them between fsyncs, their text
//!   repeated so git keeps them small.
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
    run(&mut db, "create collection gone (n int)");
    run(&mut db, "create collection gone_too (n int)");
    db.set_spill(2048);
    let parse = |q: &str| fennec_ql::parse_one(q).unwrap_or_else(|e| fail(&format!("{q}: {e}")));
    // Every schema change a block takes -- a collection made and dropped,
    // an index, a field, a statement -- at the block's front, spilled with
    // its first writes, and again at its end, landing with it.
    let front: Vec<Statement> = [
        "create collection side (n int, s text)",
        "create index on side (n) @hash",
        "alter collection side add field m int",
        "create statement side_n($n int) as get side select n, s, m where n = $n",
        "drop collection gone",
        "put side {n: 1, s: \"spilled\", m: 1}",
    ]
    .map(parse)
    .into_iter()
    .collect();
    let tail: Vec<Statement> = [
        "create collection late (n int)",
        "create index on late (n) @hash",
        "alter collection late add field t text",
        "create statement late_n($n int) as get late select n, t where n = $n",
        "drop collection gone_too",
        "put late {n: 1, t: \"landed\"}",
    ]
    .map(parse)
    .into_iter()
    .collect();
    let note = parse("put notes {n: $1, body: $2}");
    let tag = parse("put tags {n: $1, tag: $2}");
    let params: Vec<Vec<Value>> = (0..60)
        .map(|k| vec![Value::Int(k), Value::Text(format!("note {k} ").repeat(12))])
        .collect();
    let tags: Vec<Vec<Value>> = (0..60)
        .map(|k| vec![Value::Int(k), Value::Text(format!("t{}", k % 7))])
        .collect();
    let mut block: Vec<(&Statement, &[Value])> = front.iter().map(|s| (s, &[][..])).collect();
    for (p, t) in params.iter().zip(&tags) {
        block.push((&note, &p[..]));
        block.push((&tag, &t[..]));
    }
    block.extend(tail.iter().map(|s| (s, &[][..])));
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
    let clash = parse("insert notes {id: 1, n: 1000, body: \"a held id\"}");
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

/// The events a sync log holds once the machine went down, each its own
/// sync: what `synclog` and `synclog-torn` write after their first part.
fn events(db: &mut Database) {
    run(
        db,
        "create collection events (n int @hash, what text, at timestamp @sorted)",
    );
    durable(db);
    for k in 0..12 {
        run(
            db,
            &format!(
                "put events {{n: {k}, what: \"event {k}\", at: {}}}",
                1_767_225_600_000i64 + k * 60_000
            ),
        );
        durable(db);
    }
    run(db, "set events {what: \"seen\"} where n < 3");
    durable(db);
    run(db, "del events where n = 11");
    durable(db);
}

/// `n` rows of 1 050 bytes of text into `bulk`, one sync: an entry of the
/// log that size.
fn bulk(db: &mut Database, from: i64, n: i64) {
    let rows: Vec<String> = (from..from + n)
        .map(|k| {
            format!(
                "{{n: {k}, body: \"{}\"}}",
                format!("bulk row {k:05} ").repeat(70)
            )
        })
        .collect();
    run(db, &format!("put bulk [{}]", rows.join(", ")));
    durable(db);
}

fn synclog(path: &str) {
    fennec_core::fs::keep_sync_log(true);
    let mut db = open(path);
    run(&mut db, "create collection bulk (n int, body text)");
    durable(&mut db);
    bulk(&mut db, 0, 2);
    events(&mut db);
    // The process ends with no close, and the machine with it: the file
    // keeps what its own fsync covered, the log the rest, its first pass,
    // zeros after it.
    std::mem::forget(db);
    let at = base(path);
    cut(path, at);
}

fn synclog_torn(path: &str) {
    fennec_core::fs::keep_sync_log(true);
    let mut db = open(path);
    run(&mut db, "create collection bulk (n int, body text)");
    durable(&mut db);
    // Syncs of 60 KB, an entry each, short of an entry's 64 KB: four fill
    // the log's 256 KB, the fifth finds no room, the file is synced and a
    // generation begun, and the events after write over the front of the
    // last pass's first entry.
    for pass in 0..5 {
        bulk(&mut db, pass * 57, 57);
    }
    let first = base(path);
    events(&mut db);
    if base(path) == first {
        fail("the log began no generation past its first pass");
    }
    std::mem::forget(db);
    // This generation's last entry: its head reached the disk, the back
    // half of its bytes did not and read back as zeros; the last pass's
    // bytes after it as they were.
    let log_path = format!("{path}.sync");
    let mut log = std::fs::read(&log_path).unwrap_or_else(|e| fail(&e.to_string()));
    let generation = &log[16..24].to_vec();
    let (mut pos, mut last) = (4096usize, None);
    while log[pos..pos + 4] == 0x4E59_5346u32.to_le_bytes()
        && log[pos + 8..pos + 16] == generation[..]
    {
        let len = u32::from_le_bytes(log[pos + 4..pos + 8].try_into().unwrap()) as usize;
        last = Some((pos, len));
        pos += 32 + len;
    }
    let (pos, len) = last.unwrap_or_else(|| fail("the log holds no entry of its generation"));
    log[pos + 32 + len / 2..pos + 32 + len].fill(0);
    std::fs::write(&log_path, log).unwrap_or_else(|e| fail(&e.to_string()));
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

fn torn_wide(path: &str) {
    let mut db = open(path);
    // Collection ids past 127, 16 383 and 2 097 151 take two, three and
    // four bytes in every record's head: 2 097 152 collections made and
    // dropped, gone with the compact below, which keeps each live one's id.
    // A dropped collection leaves nothing behind but the count: 12
    // seconds, and 92 bytes of file after the compact.
    for k in 0..2_097_152 {
        if k == 200 {
            run(&mut db, "create collection mid (n int @hash, s text)");
            run(&mut db, "put mid [{n: 1, s: \"one\"}, {n: 2, s: \"two\"}]");
        }
        if k == 16_400 {
            run(&mut db, "create collection deep (n int)");
            run(&mut db, "put deep {n: 1}");
        }
        run(&mut db, &format!("create collection c{k} (n int)"));
        run(&mut db, &format!("drop collection c{k}"));
    }
    run(&mut db, "create collection wide (n int @hash, body text)");
    // Past 16 384 documents an image's index of the data record is in two
    // shares, each an open reads on a thread of its own.
    for from in (0..16_400).step_by(1_000) {
        let rows: Vec<String> = (from..(from + 1_000).min(16_400))
            .map(|k| format!("{{n: {k}}}"))
            .collect();
        run(&mut db, &format!("put wide [{}]", rows.join(", ")));
    }
    run(&mut db, "compact");
    durable(&mut db);
    // A row synced, and more than a page after it: a bit of it a disk
    // flips later, below the point the claims after it say was durable.
    run(
        &mut db,
        "put wide {n: 40000, body: \"the row a disk flipped a bit of\"}",
    );
    run(
        &mut db,
        &format!(
            "put wide {{n: 40001, body: \"{}\"}}",
            "a synced row after it ".repeat(240)
        ),
    );
    durable(&mut db);
    // The first write past that fsync: a page of it the power cut will
    // lose below.
    run(
        &mut db,
        "put wide {n: 40002, body: \"the row a power cut lost a page of\"}",
    );
    // One write of 2.3 MB: its record's length takes four bytes. Not
    // synced, so each record after it is more than 1 MiB past the file's
    // durable point, its claim two bytes. A record past 1 MiB (`WRITE_BUF`)
    // goes into the file where it lies, what the sink held before it
    // first.
    let rows: Vec<String> = (20_000..22_300)
        .map(|k| {
            format!(
                "{{n: {k}, body: \"{}\"}}",
                format!("wide row {k} ").repeat(64)
            )
        })
        .collect();
    run(&mut db, &format!("put wide [{}]", rows.join(", ")));
    // More records after it than an open looks at for its durable point
    // (16, `RECENT`): every one it looks at claims in two bytes, none of
    // them older and claiming in one.
    run(
        &mut db,
        "put wide {n: 30000, body: \"after the wide write\"}",
    );
    run(&mut db, "set wide {body: \"set after\"} where n = 1");
    run(&mut db, "del wide where n = 2");
    for k in 3..20 {
        run(&mut db, &format!("put mid {{n: {k}, s: \"row {k}\"}}"));
    }
    // Past 64 KB held, the sink writes it all behind, unsynced; then the
    // process is gone: no close's mark, so the newest claim is the last
    // record's two bytes.
    run(
        &mut db,
        &format!(
            "put mid {{n: 20, s: \"{}\"}}",
            "written behind ".repeat(4_600)
        ),
    );
    behind(&mut db);
    std::mem::forget(db);
    let mut bytes = std::fs::read(path).unwrap_or_else(|e| fail(&e.to_string()));
    let mut flip = |row: &[u8], k: usize| {
        let at = bytes
            .windows(row.len())
            .position(|w| w == row)
            .unwrap_or_else(|| fail("a row to flip is not in the file"));
        bytes[at + k] ^= 0x20;
    };
    // The disk flips a bit of the synced row: below the durable point the
    // claims name, an open reads it unchecked. A claim read as naming an
    // earlier point has it checked, and the open refused.
    flip(b"the row a disk flipped", 15);
    // And the power cut lost a page of the first write after the fsync,
    // the later pages written: past the durable point, which no record
    // after it claims past, the open cuts the file there. A two-byte claim
    // read as naming a later point, even by a mebibyte, would read it
    // unchecked and keep it. `fennec verify`, which checks every record,
    // refuses the file.
    flip(b"the row a power cut lost", 25);
    std::fs::write(path, bytes).unwrap_or_else(|e| fail(&e.to_string()));
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
    run(
        &mut db,
        &format!(
            "put t {{n: 5, s: \"{}\"}}",
            "the row a crash tore ".repeat(3_200)
        ),
    );
    behind(&mut db);
    db
}

/// What the sink holds written into the file with no fsync, as a server
/// writes behind its lock: past `BEHIND_AT` (64 KB) it writes all of it.
/// A `flush`'s durability dropped writes nothing: the bytes stay the
/// sink's, and a crash then leaves none of them in the file.
fn behind(db: &mut Database) {
    let w = db
        .write_behind()
        .unwrap_or_else(|| fail("under 64 KB held: nothing written behind"));
    w().unwrap_or_else(|e| fail(&e.to_string()));
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
        ["synclog-torn", file] => synclog_torn(file),
        ["torn-wide", file] => torn_wide(file),
        ["kept", file] => kept(file),
        ["waiting", file] => waiting(file),
        ["torn-tail", file] => torn_tail(file),
        ["torn-mark", file] => torn_mark(file),
        ["answers", file, statements, out] => answers(file, statements, out),
        _ => fail(
            "usage: compat_writer spill|synclog|synclog-torn|torn-wide|kept|waiting|torn-tail|torn-mark <file> | answers <file> <statements> <out>",
        ),
    }
}
