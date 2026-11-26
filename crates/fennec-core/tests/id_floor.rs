//! A collection whose ids climb while its oldest rows go -- sessions under
//! `@ttl`, a queue -- holds an id index of the ids it holds, not a slot of
//! 12 bytes for every id it ever gave: the index starts at a floor its
//! writes move up as the oldest rows go, and a compact and an open cut
//! (docs/design/storage.md#the-id-indexs-floor). An id below the floor reads
//! as absent and takes a write; a block put back across the floor's move,
//! a pin taken before it and a replica applying the writes read as the
//! rows say.

#![cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]

use fennec_core::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The rows a session collection holds: each deleted this many writes
/// after it was made.
const LIVE: u64 = 500;

/// What the id index may hold for `LIVE` rows whose ids climb: about
/// twice the span its first held id to its last, cut once its empty front
/// is 4 096 slots and half of it, the room past four times that given back
/// -- under 200 KB, where from 1 it held 12 bytes for each of 100 000 ids.
const BOUND: usize = 4 * (LIVE as usize + 4_096) * 12;

fn stmt(sql: &str) -> Statement {
    fennec_ql::parse_one(sql).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn exec(db: &mut Database, sql: &str) -> Response {
    db.execute(&stmt(sql))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// The rows of `s`, by id with their `k`, as a scan answers them.
fn rows(db: &Database) -> Vec<(u64, Value)> {
    let Response::Rows(rs) = db.query(&stmt("get s select k"), &[]).unwrap() else {
        panic!("rows");
    };
    rs.rows
        .iter()
        .map(|r| (r.id, r.values[0].clone()))
        .collect()
}

fn ids(db: &Database, sql: &str) -> Vec<u64> {
    let Response::Rows(rs) = db.query(&stmt(sql), &[]).unwrap() else {
        panic!("{sql}: rows");
    };
    rs.rows.iter().map(|r| r.id).collect()
}

/// The bytes `s`'s id index holds: what a pin copies of it.
fn index_bytes(db: &Database) -> usize {
    db.collection("s").unwrap().store.pinned_bytes()
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fennecdb-id-floor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{tag}.fennec"));
    let _ = std::fs::remove_file(&path);
    path
}

/// Sessions made from id `from` to `to`, each deleted `LIVE` writes after
/// it was made, a block of a thousand writes at a time; the most the id
/// index held on the way.
fn churn(db: &mut Database, from: u64, to: u64) -> usize {
    let put = stmt("put s {k: $1}");
    let del = stmt("del s where id = $1");
    let mut most = 0;
    db.begin().unwrap();
    for id in from..=to {
        db.execute_with(&put, &[Value::Int(id as i64)]).unwrap();
        if id > LIVE {
            db.execute_with(&del, &[Value::Int((id - LIVE) as i64)])
                .unwrap();
        }
        if id % 1_000 == 0 {
            db.commit().unwrap();
            most = most.max(index_bytes(db));
            db.begin().unwrap();
        }
    }
    db.commit().unwrap();
    most.max(index_bytes(db))
}

/// The rows `churn` leaves at `to`.
fn churned(to: u64) -> Vec<(u64, Value)> {
    (to + 1 - LIVE..=to)
        .map(|id| (id, Value::Int(id as i64)))
        .collect()
}

/// 100 000 sessions, 500 live: the index stays under `BOUND` as they go,
/// a compact leaves it the live rows' span, and so does an open, which
/// goes on from there.
#[test]
fn ids_climbing_through_deletes_hold_the_index_through_a_compact_and_an_open() {
    let path = tmp("climb");
    let mut db = fennec_core::fs::open(&path).unwrap();
    exec(&mut db, "create collection s (k int)");
    let most = churn(&mut db, 1, 100_000);
    assert!(most <= BOUND, "{most} bytes, past {BOUND}");
    assert_eq!(rows(&db), churned(100_000));
    assert!(ids(&db, "get s where id = 7").is_empty());
    assert_eq!(
        ids(&db, "get s where id >= 3 limit 2"),
        [100_001 - LIVE, 100_002 - LIVE]
    );

    exec(&mut db, "compact");
    let compacted = index_bytes(&db);
    assert!(compacted <= BOUND / 2, "{compacted} bytes after a compact");
    assert_eq!(rows(&db), churned(100_000));
    drop(db);

    let mut db = fennec_core::fs::open(&path).unwrap();
    let opened = index_bytes(&db);
    assert!(
        opened <= 2 * LIVE as usize * 12,
        "{opened} bytes after an open"
    );
    assert_eq!(rows(&db), churned(100_000));
    assert!(ids(&db, "get s where id = 7").is_empty());
    let most = churn(&mut db, 100_001, 120_000);
    assert!(most <= BOUND, "{most} bytes, past {BOUND}");
    assert_eq!(rows(&db), churned(120_000));
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(rows(&db), churned(120_000));
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// After a compact the floor is the first live id: one written just below
/// it lowers the floor, one far below goes to the sparse map, and both are
/// read, listed in order, counted and held to `insert`'s refusal.
#[test]
fn an_id_below_the_floor_reads_as_absent_and_takes_a_write() {
    let path = tmp("below");
    let mut db = fennec_core::fs::open(&path).unwrap();
    exec(&mut db, "create collection s (k int)");
    churn(&mut db, 1, 20_000);
    exec(&mut db, "compact");
    let first = 20_001 - LIVE;
    for id in [first - 1, first - 5, 2] {
        assert!(ids(&db, &format!("get s where id = {id}")).is_empty());
    }
    exec(&mut db, &format!("put s {{id: {}, k: -5}}", first - 5));
    exec(&mut db, "put s {id: 2, k: -2}");
    let want = [2, first - 5, first, first + 1];
    assert_eq!(ids(&db, "get s select id limit 4"), want);
    assert_eq!(ids(&db, "get s where id >= 1 limit 4"), want);
    assert_eq!(ids(&db, "get s where id >= 3 limit 2"), [first - 5, first]);
    assert_eq!(
        ids(&db, &format!("get s where id = {}", first - 5)),
        [first - 5]
    );
    assert_eq!(rows(&db).len(), LIVE as usize + 2);
    assert!(matches!(
        db.execute(&stmt("insert s {id: 2, k: 0}")),
        Err(Error::Duplicate(_))
    ));
    let before = rows(&db);
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(rows(&db), before);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// Rows `from` to `to` written in one block, none deleted.
fn grown(db: &mut Database, from: i64, to: i64) {
    let put = stmt("put s {k: $1}");
    db.begin().unwrap();
    for id in from..=to {
        db.execute_with(&put, &[Value::Int(id)]).unwrap();
    }
    db.commit().unwrap();
}

/// A block that deletes all but the last rows of the collection -- the
/// floor cut past them as it goes, the room given back -- and writes ids
/// below where it ends, put back: every row is there as it was, read by
/// id and in order. One that lands keeps what it wrote.
#[test]
fn a_block_put_back_across_the_floors_move_leaves_the_rows_as_they_were() {
    let mut db = Database::new();
    exec(&mut db, "create collection s (k int)");
    churn(&mut db, 1, 10_000);
    exec(&mut db, "compact");
    grown(&mut db, 10_001, 70_000);
    let before = rows(&db);
    let first = before[0].0;
    let bytes = index_bytes(&db);
    for lands in [false, true] {
        db.begin().unwrap();
        exec(&mut db, &format!("del s where id < {}", first + 60_400));
        let cut = index_bytes(&db);
        assert!(cut < bytes / 2, "the block cut {bytes} bytes to {cut}");
        exec(&mut db, &format!("put s {{id: {}, k: -1}}", first + 60_390));
        exec(&mut db, &format!("put s {{id: {}, k: -2}}", first - 3));
        exec(&mut db, &format!("put s {{id: {}, k: -3}}", first + 100));
        match lands {
            false => db.rollback(),
            true => db.commit().unwrap(),
        }
        let want: Vec<(u64, Value)> = match lands {
            false => before.clone(),
            true => [
                (first - 3, Value::Int(-2)),
                (first + 100, Value::Int(-3)),
                (first + 60_390, Value::Int(-1)),
            ]
            .into_iter()
            .chain(before.iter().filter(|r| r.0 >= first + 60_400).cloned())
            .collect(),
        };
        assert_eq!(rows(&db), want, "lands: {lands}");
        for (id, k) in &want {
            let Response::Rows(rs) = db
                .query(&stmt(&format!("get s select k where id = {id}")), &[])
                .unwrap()
            else {
                panic!("rows");
            };
            assert_eq!(rs.rows[0].values[0], *k, "{id}, lands: {lands}");
        }
    }
}

/// A long read pinned before the front rows go and the floor moves -- up
/// past them, then down for an id written just below it -- answers as at
/// the pin.
#[test]
fn a_pinned_read_answers_as_at_the_pin_across_the_floors_move() {
    let path = tmp("pinned");
    let mut db = fennec_core::fs::open(&path).unwrap();
    db.set_pin_at(0);
    exec(&mut db, "create collection s (k int)");
    churn(&mut db, 1, 10_000);
    grown(&mut db, 10_001, 70_000);
    let read = stmt("get s select k where k > 0");
    let want = db.query(&read, &[]).unwrap();
    let pin = db.pin(&[(&read, &[][..])]).expect("a long read is pinned");
    let bytes = index_bytes(&db);
    exec(&mut db, "del s where id < 69900");
    let cut = index_bytes(&db);
    assert!(cut < bytes / 2, "the delete cut {bytes} bytes to {cut}");
    exec(&mut db, "put s {id: 9000, k: 1}");
    exec(&mut db, "put s {id: 69890, k: 2}");
    assert_eq!(
        pin.query(&read, &[])
            .expect("a read the pin holds")
            .unwrap(),
        want
    );
    assert_ne!(db.query(&read, &[]).unwrap(), want);
    drop(pin);
    drop(db);
    let _ = std::fs::remove_file(&path);
}

/// A file in memory that keeps the records it was handed, in turn: what a
/// primary's feed passes on.
#[derive(Clone, Default)]
struct Tap {
    records: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Sink for Tap {
    fn append(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
    fn record(&mut self, _: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.records.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
    fn rewrite(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
}

/// A replica applying a session collection's writes holds the index its
/// primary holds, and answers as it does.
#[test]
fn a_replica_applying_climbing_ids_holds_the_primarys_index() {
    let tap = Tap::default();
    let mut primary = Database::with_sink(Box::new(tap.clone()));
    exec(&mut primary, "create collection s (k int)");
    churn(&mut primary, 1, 30_000);
    let mut replica = Database::new();
    replica.follow(vec![(7, 0)]).unwrap();
    for record in tap.records.lock().unwrap().iter() {
        replica.apply(record).unwrap();
    }
    assert_eq!(rows(&replica), churned(30_000));
    assert_eq!(index_bytes(&replica), index_bytes(&primary));
    assert!(index_bytes(&replica) <= BOUND);
}
