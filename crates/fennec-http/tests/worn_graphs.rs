//! A server builds a worn graph again beside the queries -- one whose
//! tombstones outnumber its live nodes, 10 000 of them at least -- on its
//! compacter's thread (`link::keep`), and its keeper appends the rebuilt
//! graph's record: the file opened again restores the graph without them,
//! where it held a record of the worn one. Without it, a collection whose
//! vectors were written over carried every tombstone it ever made, in the
//! heap and in each record the keeper appended, through every open.

use fennec_core::prelude::*;
use fennec_http::link;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const ROWS: i64 = 200;

/// The graph's tombstones and live nodes.
fn graph(db: &Database) -> (usize, usize) {
    let ix = &db.collection("docs").expect("docs").vectors["v"];
    (ix.dead(), ix.len())
}

/// Every row given its `gen`th vector, in one statement.
fn written(db: &mut Database, gen: i64) {
    let docs = (1..=ROWS)
        .map(|id| {
            let v = vec![id as f32, gen as f32 + 1.0, (id % 7) as f32, 1.0];
            vec![
                ("id".to_string(), Expr::Lit(Value::Int(id))),
                ("v".to_string(), Expr::Lit(Value::Vector(v))),
            ]
        })
        .collect();
    db.execute(&Statement::Put {
        collection: "docs".into(),
        docs,
        docs_param: None,
        insert: false,
        if_absent: false,
        else_set: None,
        require: None,
        pick: None,
    })
    .expect("put");
}

/// The server's own floor and record pace, untouched: 200 live nodes and
/// 10 000 tombstones, so the rebuilt graph's 200 nodes are fewer changes
/// than a record waits for, and only the tombstones it dropped
/// (`changed_by`) make its record due.
#[test]
fn a_worn_graph_is_rebuilt_beside_the_queries_and_its_record_written() {
    let dir = std::env::temp_dir().join(format!("fennecdb-worn-graphs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.fennec");
    let mut db = fennec_core::fs::open(&path).unwrap();
    let stmt = fennec_ql::parse_one("create collection docs (v vector<4> @hnsw(cosine))").unwrap();
    db.execute(&stmt).unwrap();
    // Each row written 51 times: 50 tombstones a live node.
    for gen in 0..51 {
        written(&mut db, gen);
    }
    assert_eq!(graph(&db), (10_000, 200));
    assert!(db.graphs_worn());
    // A record of the worn graph in the file: what an open restores until
    // a record of the rebuilt one follows it.
    db.checkpoint().unwrap();
    {
        let ix = &db.collection("docs").unwrap().vectors["v"];
        assert!(
            ix.persisted().at.load(Relaxed) > 0,
            "the worn graph's record"
        );
    }
    let db = Arc::new(RwLock::new(db));
    link::keep("worn.fennec", &db);
    // The compacter and the keeper look every few seconds: the graph
    // rebuilt and its record written are the events waited for, at most a
    // minute. A graph rebuilt is a graph of its own, which no record holds
    // until the keeper writes one.
    let deadline = Instant::now() + Duration::from_secs(60);
    let rebuilt = loop {
        {
            let g = db.read().unwrap();
            let ix = &g.collection("docs").unwrap().vectors["v"];
            if ix.dead() == 0 && ix.persisted().at.load(Relaxed) > 0 {
                break ix.serialize_graph();
            }
        }
        assert!(
            Instant::now() < deadline,
            "the graph was never rebuilt and its record written"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(graph(&db.read().unwrap()), (0, 200));
    assert!(!db.read().unwrap().graphs_worn());
    // Let go -- once the compacter and the keeper let go of it too -- and
    // opened again: the graph is the record the keeper wrote, after the
    // worn one, to the byte.
    let mut db = db;
    let db = loop {
        match Arc::try_unwrap(db) {
            Ok(db) => break db,
            Err(held) => db = held,
        }
        assert!(Instant::now() < deadline, "the server held the database");
        std::thread::sleep(Duration::from_millis(1));
    };
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(graph(&db), (0, 200));
    let ix = &db.collection("docs").unwrap().vectors["v"];
    assert_eq!(ix.nodes(), 200, "no tombstone restored");
    assert_eq!(ix.serialize_graph(), rebuilt);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
