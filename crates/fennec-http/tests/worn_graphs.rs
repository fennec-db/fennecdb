//! The server's graph keeper (`link::keep`) builds a worn graph again beside
//! the queries -- one whose tombstones outnumber its live nodes -- and
//! appends the rebuilt graph's record at the same look: the file opened
//! again restores the graph without them. Without it, a collection whose
//! vectors were written over carried every tombstone it ever made, in the
//! heap and in each record the keeper appended, through every open.

use fennec_core::prelude::*;
use fennec_http::link;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn ok(db: &mut Database, sql: &str) {
    let stmt = fennec_ql::parse_one(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    db.execute(&stmt).unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// The graph's tombstones and live nodes.
fn graph(db: &Database) -> (usize, usize) {
    let ix = &db.collection("docs").expect("docs").vectors["v"];
    (ix.dead(), ix.len())
}

#[test]
fn the_keeper_rebuilds_a_worn_graph_and_writes_it_into_the_file() {
    let dir = std::env::temp_dir().join(format!("fennecdb-worn-graphs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    // Worn from 100 tombstones, and a graph's record due once 100 of its
    // nodes changed, whatever the file grew by: a few hundred rows show
    // what a server's thousands do.
    db.set_worn_floor(100);
    db.set_graph_saves(100, 0);
    ok(
        &mut db,
        "create collection docs (v vector<4> @hnsw(cosine))",
    );
    // Each row written three times: two tombstones a live node.
    for gen in 0..3 {
        for id in 1..=200 {
            let v = [id as f32, gen as f32 + 1.0, (id % 7) as f32, 1.0];
            ok(
                &mut db,
                &format!(
                    "put docs {{id: {id}, v: [{}, {}, {}, {}]}}",
                    v[0], v[1], v[2], v[3]
                ),
            );
        }
    }
    assert_eq!(graph(&db), (400, 200));
    assert!(db.graphs_worn());
    db.sync().unwrap();
    let db = Arc::new(RwLock::new(db));
    link::keep("worn.fennec", &db);
    // The keeper looks every few seconds: the graph rebuilt and its record
    // written are the events waited for, at most a minute.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        {
            let g = db.read().unwrap();
            let ix = &g.collection("docs").unwrap().vectors["v"];
            let written = ix.persisted().at.load(std::sync::atomic::Ordering::Relaxed);
            if ix.dead() == 0 && written > 0 {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the keeper never rebuilt the graph"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(graph(&db.read().unwrap()), (0, 200));
    assert!(!db.read().unwrap().graphs_worn());
    // Let go -- once the keeper's look let go of it too -- and opened
    // again: the graph is the record the keeper wrote.
    let mut db = db;
    let db = loop {
        match Arc::try_unwrap(db) {
            Ok(db) => break db,
            Err(held) => db = held,
        }
        assert!(Instant::now() < deadline, "the keeper held the database");
        std::thread::sleep(Duration::from_millis(1));
    };
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(graph(&db), (0, 200));
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
