//! A compact of a file past the page cache reads the old file in turn
//! (`fennec_core::store::InTurn`, docs/design/storage.md#reading-the-file-in-turn)
//! and writes the file a compact walking the records by id writes, byte for
//! byte: under the lock and beside the database, through a scratch file and
//! in memory, over records an image, a spilled block, a tail the stores took
//! in from the file and writes still in memory hold. A scratch file that
//! fails fails the compact as a write of its new file does, and none leaves
//! a name beside the database.

#![cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]

use fennec_core::fs::{beside, open};
use fennec_core::prelude::*;
use fennec_core::store::{assume_in_turn, break_scratch, copied, Broken};
use std::path::{Path, PathBuf};
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

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fennecdb-in-turn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.fennec"));
    for side in ["", ".sorting", ".beside", ".compacting", ".lock"] {
        let _ = std::fs::remove_file(format!("{}{side}", path.display()));
    }
    path
}

const ALL: [&str; 2] = ["get t order id", "get u order id"];

/// A document of `t`, `k`'s `v`th version: a body of its own length.
fn doc(k: u64, v: u64) -> String {
    let body = "x".repeat(((k * 37 + v * 101) % 600) as usize);
    format!("{{id: {k}, tag: \"<{k}:{v}>\", n: {v}, body: \"{body}\"}}")
}

/// A database whose live records stand in no id order across every place a
/// store reads them from: the image a checkpoint wrote, a block that
/// spilled into the file before it landed, writes the stores took in from
/// the file one by one, and writes still held in memory -- documents
/// rewritten within each and from one to the next, deleted, deleted and
/// written again, one past the dense array of ids -- and a second
/// collection, small enough for one bucket. Made alike, two such files are
/// alike, and so are their compacts.
fn scattered(path: &Path) -> Database {
    let mut db = open(path).unwrap();
    run(&mut db, "create collection t (tag text, n int, body text)");
    run(&mut db, "create collection u (k int)");
    for k in 1..=300 {
        run(&mut db, &format!("put t {}", doc(k, 0)));
    }
    for k in 1..=40 {
        run(&mut db, &format!("put u {{k: {k}}}"));
    }
    db.checkpoint().unwrap();
    // A block past 4 KB spills its frames into the file before it lands.
    db.set_spill(4 << 10);
    db.begin().unwrap();
    for k in (2..=300).rev().step_by(9) {
        run(&mut db, &format!("put t {}", doc(k, 1)));
    }
    db.commit().unwrap();
    db.set_spill(u64::MAX);
    // Each write taken in from the file as it lands.
    db.set_handover(0);
    for k in (5..=300).step_by(7) {
        run(&mut db, &format!("put t {}", doc(k, 2)));
    }
    for k in (4..=300).step_by(19) {
        run(&mut db, &format!("del t where id = {k}"));
    }
    run(&mut db, "set u {k: k + 100} where k > 30");
    // Then held in memory.
    db.set_handover(u64::MAX);
    for k in (3..=300).step_by(11) {
        run(&mut db, &format!("put t {}", doc(k, 3)));
    }
    for k in (12..=300).step_by(29) {
        run(&mut db, &format!("del t where id = {k}"));
    }
    run(&mut db, &format!("put t {}", doc(4, 4)));
    run(&mut db, &format!("put t {}", doc(1 << 40, 5)));
    assert!(db.collection("t").unwrap().store.heap_bytes() > 0);
    db.sync().unwrap();
    db
}

/// The compact, under the lock or beside the database.
fn compact(db: Database, beside_it: bool) -> (Database, Result<Response>) {
    let q = fennec_ql::parse_one("compact").unwrap();
    match beside_it {
        false => {
            let mut db = db;
            let r = db.execute(&q);
            (db, r)
        }
        true => {
            let db = RwLock::new(db);
            let r = Database::maintain(&db, &q).unwrap();
            (db.into_inner().unwrap(), r)
        }
    }
}

/// Nothing a compact made is left beside the file.
fn nothing_beside(path: &Path) {
    for side in ["sorting", "beside", "compacting"] {
        assert!(!beside(path, side).exists(), "{side} left beside");
    }
}

/// A compact copying in turn -- through a scratch file for `t`, whose
/// buckets of 16 KB are many, and in memory for `u`, which one holds --
/// writes the file a compact by id writes, byte for byte, under the lock
/// and beside the database; both answer as before, and as before once
/// opened again.
#[test]
fn a_compact_in_turn_writes_the_file_a_compact_by_id_writes() {
    for beside_it in [false, true] {
        let mut files = Vec::new();
        let mut before = None;
        for budget in [None, Some(16 << 10)] {
            let path = tmp(&format!("same-{beside_it}-{}", budget.is_some()));
            let db = scattered(&path);
            let answers: Vec<ResultSet> = ALL.iter().map(|q| answer(&db, q)).collect();
            assert_eq!(before.get_or_insert_with(|| answers.clone()), &answers);
            let was = copied();
            assume_in_turn(Some(budget));
            let (db, r) = compact(db, beside_it);
            assume_in_turn(None);
            r.unwrap();
            let (s, m) = copied();
            let expected = match budget {
                None => (0, 0),
                Some(_) => (1, 1),
            };
            assert_eq!((s - was.0, m - was.1), expected, "beside {beside_it}");
            for (q, a) in ALL.iter().zip(&answers) {
                assert_eq!(&answer(&db, q), a, "{q}");
            }
            drop(db);
            nothing_beside(&path);
            files.push(std::fs::read(&path).unwrap());
            let db = open(&path).unwrap();
            for (q, a) in ALL.iter().zip(&answers) {
                assert_eq!(&answer(&db, q), a, "{q}, opened again");
            }
            drop(db);
            let _ = std::fs::remove_file(&path);
        }
        assert!(
            files[0] == files[1],
            "beside {beside_it}: the compact in turn wrote another file"
        );
    }
}

/// A scratch file that refuses a write, or reads back otherwise than it was
/// written, fails the compact as a write of its new file does: beside the
/// database the compact alone, which leaves the file as it was and the
/// database writing; under the lock the database too, as any failed rewrite
/// there does, its writes refused until it is opened again. Either way the
/// file opens to what it held, and nothing is left beside it.
#[test]
fn a_scratch_file_that_fails_fails_the_compact_as_its_write_would() {
    for broken in [Broken::Write, Broken::Byte] {
        for beside_it in [false, true] {
            let how = format!("{broken:?}, beside {beside_it}");
            let path = tmp(&format!("broken-{broken:?}-{beside_it}"));
            let db = scattered(&path);
            let answers: Vec<ResultSet> = ALL.iter().map(|q| answer(&db, q)).collect();
            let was = std::fs::read(&path).unwrap();
            assume_in_turn(Some(Some(16 << 10)));
            break_scratch(Some(broken));
            let (mut db, r) = compact(db, beside_it);
            break_scratch(None);
            assume_in_turn(None);
            let e = r.expect_err(&how);
            assert!(matches!(e, Error::Io(_)), "{how}: {e}");
            nothing_beside(&path);
            assert!(
                std::fs::read(&path).unwrap() == was,
                "{how}: the file changed"
            );
            let put = fennec_ql::parse_one("put u {k: 999}").unwrap();
            match beside_it {
                true => {
                    assert!(db.failure().is_none(), "{how}");
                    db.execute(&put).unwrap();
                }
                false => {
                    assert!(db.failure().is_some(), "{how}");
                    assert!(matches!(db.execute(&put), Err(Error::Io(_))), "{how}");
                }
            }
            drop(db);
            let db = open(&path).unwrap();
            for (q, a) in ALL.iter().zip(&answers) {
                let got = answer(&db, q);
                match (beside_it, *q) {
                    (true, "get u order id") => assert_eq!(got.rows.len(), a.rows.len() + 1),
                    _ => assert_eq!(&got, a, "{how}: {q}"),
                }
            }
            drop(db);
            let _ = std::fs::remove_file(&path);
        }
    }
}
