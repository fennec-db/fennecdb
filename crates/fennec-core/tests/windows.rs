//! The mapped file on Windows (docs/design/windows.md, decision 1) through
//! the database: what a Unix's mapping gives -- the handover's file the
//! same byte for byte, a rewrite pointing the stores at the new file --
//! held where Windows maps it otherwise: with no placeholders, each
//! handover mapping the file anew as a Windows before 1803 does; with a
//! room a file outgrows; and with another process mapping the file while
//! a checkpoint, a compact and a compact beside the database rename over
//! it, and while an open finds a torn tail to cut.

#![cfg(all(mapped, windows))]

use fennec_core::fs::{assume_room, open_in_memory, open_mapped, open_read_only};
use fennec_core::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::RwLock;

fn run(db: &mut Database, sql: &str) {
    for stmt in fennec_ql::parse(sql).expect("parse") {
        db.execute(&stmt).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

fn answer(db: &Database, sql: &str) -> ResultSet {
    db.query(&fennec_ql::parse_one(sql).expect("parse"), &[])
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .rows()
        .expect("rows")
        .clone()
}

fn count(db: &Database) -> usize {
    answer(db, "get docs order id").rows.len()
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fennecdb-windows-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.fennec"));
    let _ = std::fs::remove_file(&path);
    path
}

/// Rows enough for many a handover of a few pages, each a page or so.
fn fill(db: &mut Database, from: usize, n: usize) {
    for i in from..from + n {
        let body = format!("row {i} ").repeat(40);
        run(db, &format!("put docs {{n: {i}, body: \"{body}\"}}"));
    }
}

fn same(a: &Database, b: &Database) {
    for q in [
        "get docs order id",
        "get docs where n >= 100 and n < 180 order n desc limit 7",
        "get docs match body \"row\" limit 5",
        "get docs count",
    ] {
        assert_eq!(answer(a, q), answer(b, q), "{q}");
    }
}

fn same_bytes(a: &Path, b: &Path) {
    let (x, y) = (std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
    assert!(x == y, "{} and {} bytes differ", x.len(), y.len());
}

/// A database mapped with `room` that hands over after every write, and a
/// twin that never does, through writes, a checkpoint, a compact and a
/// reopen: the same answers, the same bytes.
fn twins_through(tag: &str, room: Option<Option<usize>>) {
    let (a_path, b_path) = (tmp(&format!("{tag}-a")), tmp(&format!("{tag}-b")));
    assume_room(room);
    let mut a = open_mapped(&a_path).unwrap();
    a.set_handover(0);
    let mut b = open_mapped(&b_path).unwrap();
    b.set_handover(u64::MAX);
    for db in [&mut a, &mut b] {
        run(
            db,
            "create collection docs (n int @sorted, body text @text)",
        );
        fill(db, 0, 400);
    }
    same(&a, &b);
    a.sync().unwrap();
    b.sync().unwrap();
    same_bytes(&a_path, &b_path);
    for db in [&mut a, &mut b] {
        db.checkpoint().unwrap();
        fill(db, 400, 200);
        run(db, "del docs where n < 50");
        run(db, "compact");
        fill(db, 600, 200);
    }
    same(&a, &b);
    a.sync().unwrap();
    b.sync().unwrap();
    same_bytes(&a_path, &b_path);
    assume_room(None);
    drop((a, b));
    let (a, b) = (
        open_mapped(&a_path).unwrap(),
        open_in_memory(&b_path).unwrap(),
    );
    same(&a, &b);
    assert_eq!(count(&a), 750);
    drop((a, b));
    let _ = std::fs::remove_file(&a_path);
    let _ = std::fs::remove_file(&b_path);
}

/// Where the system refuses placeholders -- a Windows before 1803 -- the
/// file is mapped whole and each handover maps it anew.
#[test]
fn with_no_placeholders_each_handover_maps_the_file_anew() {
    twins_through("anew", Some(None));
}

/// A file outgrowing its room is mapped anew, as on a Unix, and the views
/// before it go when nothing reads them.
#[test]
fn a_file_outgrowing_its_room_is_mapped_anew() {
    twins_through("room", Some(Some(64 << 10)));
}

/// The views laid end to end in a placeholder, as `windows-latest` maps.
#[test]
fn views_laid_end_to_end_hold_what_a_unix_mapping_holds() {
    assert!(fennec_core::fs::placeholders());
    twins_through("laid", None);
}

const READ_FOR: &str = "FENNEC_TEST_READ_FOR";
const READING: &str = "read only, until stdin ends";

/// Another process -- this test binary again -- holding `path` open as
/// `fennec types` or `fennec --read-only` does: mapped, taking no lock.
fn read_elsewhere(path: &Path) -> (Child, BufReader<ChildStdout>, usize) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "windows::reads_for_another_process"])
        .args(["--nocapture", "--test-threads=1"])
        .env(READ_FOR, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut said = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains(READING) {
        line.clear();
        let read = said.read_line(&mut line).unwrap();
        assert!(read > 0, "the other process ended before it opened");
    }
    let rows = line.trim().rsplit(' ').next().unwrap().parse().unwrap();
    (child, said, rows)
}

/// The other process [`read_elsewhere`] starts: the rows it read as it
/// opened, and again once its stdin ends; nothing in a test run of its own.
#[test]
fn reads_for_another_process() {
    let Some(path) = std::env::var_os(READ_FOR) else {
        return;
    };
    let db = open_read_only(&path).unwrap();
    println!("{READING}: {}", count(&db));
    std::io::stdout().flush().unwrap();
    let mut rest = String::new();
    let _ = std::io::stdin().read_line(&mut rest);
    println!("still: {}", count(&db));
}

/// A checkpoint, a compact under the write lock and one beside the
/// database (`Database::maintain`, its side file adopted) each rename over
/// a file another process maps, which no view refuses (decision 2); the
/// reader keeps the file it mapped, and the writer's file holds every row.
#[test]
fn a_rewrite_renames_over_a_file_another_process_maps() {
    let path = tmp("other");
    let mut db = open_mapped(&path).unwrap();
    run(&mut db, "create collection docs (n int, body text)");
    fill(&mut db, 0, 100);
    db.sync().unwrap();
    let (mut child, mut said, seen) = read_elsewhere(&path);
    assert_eq!(seen, 100);

    db.checkpoint().unwrap();
    fill(&mut db, 100, 50);
    run(&mut db, "del docs where n < 20");
    run(&mut db, "compact");
    fill(&mut db, 150, 50);
    run(&mut db, "del docs where n < 40");
    let lock = RwLock::new(db);
    let compact = fennec_ql::parse_one("compact").unwrap();
    Database::maintain(&lock, &compact)
        .expect("a compact is maintenance")
        .unwrap();
    let mut db = lock.into_inner().unwrap();
    fill(&mut db, 200, 10);
    assert_eq!(count(&db), 170);

    drop(child.stdin.take());
    let mut line = String::new();
    said.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "still: 100", "the reader's view changed");
    assert!(child.wait().unwrap().success());
    drop(db);
    assert_eq!(count(&open_mapped(&path).unwrap()), 170);
    let _ = std::fs::remove_file(&path);
}

/// A torn tail is cut by a second open, once the first's views are gone
/// (`ERROR_USER_MAPPED_FILE` otherwise), every record before it read and
/// the writes after it kept -- a server's open's second open a server's
/// too, its vectors left to link beside the queries. Another process
/// mapping the file refuses the cut, and the open fails rather than write
/// past a tail it left.
#[test]
fn a_torn_tail_is_cut_by_a_second_open() {
    let path = tmp("torn");
    {
        let mut db = open_mapped(&path).unwrap();
        run(
            &mut db,
            "create collection docs (n int, body text, v vector<4> @hnsw(cosine, m=8))",
        );
        for i in 0..30 {
            let v = [(i % 7) as f32, (i % 5) as f32, (i % 3) as f32, 1.0];
            run(
                &mut db,
                &format!(
                    "put docs {{n: {i}, body: \"row {i}\", v: [{}, {}, {}, {}]}}",
                    v[0], v[1], v[2], v[3]
                ),
            );
        }
        db.sync().unwrap();
    }
    let whole = std::fs::metadata(&path).unwrap().len();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    // A data record promising 100 bytes, 4 of them there.
    f.write_all(&[3, 1, 100, 0, 1, 2, 3]).unwrap();
    drop(f);

    let (mut child, _said, seen) = read_elsewhere(&path);
    assert_eq!(seen, 30);
    let refused = fennec_core::fs::open(&path)
        .err()
        .expect("cut under a view");
    println!("a torn tail another process maps: {refused}");
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), whole + 7);

    {
        let mut db = fennec_core::fs::open_serving(&path, true, Box::new(Ok)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), whole);
        assert_eq!(count(&db), 30);
        assert_eq!(db.link_pending(0), 30, "the second open linked them");
        fill(&mut db, 30, 5);
        db.sync().unwrap();
    }
    let db = open_mapped(&path).unwrap();
    assert_eq!(count(&db), 35);
    drop(db);
    let _ = std::fs::remove_file(&path);
}
