//! The server's own compaction (`link`, what `--auto-compact` sets) of a
//! file holding a dropped field and, in a collection made after it, a
//! document no decoder reads: a value nesting 100 lists deep, whole by its
//! sum. A compact used to take the dropped field's places out a collection
//! at a time, in memory as it went, and was refused at the deep document:
//! the first collection renumbered in memory, the file not, and the server
//! met the same refusal at every look, logging it every five seconds. A
//! compact now keeps every place and copies a document it cannot read as
//! it is: the server's look compacts once, and nothing is due after it.
//!
//! A binary of its own: the policy the compacter looks with and its counts
//! are the process's.

use fennec_core::codec::{put_uvarint, TAG_LIST, TAG_NULL};
use fennec_core::engine::CompactPolicy;
use fennec_core::prelude::*;
use fennec_http::link;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const REC_DATA: u8 = 3;
const DEEP: &str = "a value nests more than 64 levels deep";

/// Every record a database appends: the file past its signature.
#[derive(Clone, Default)]
struct Records(Arc<Mutex<Vec<u8>>>);

impl Sink for Records {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        *self.0.lock().unwrap() = bytes[fennec_core::engine::MAGIC.len()..].to_vec();
        Ok(())
    }
}

fn run(db: &mut Database, sql: &str) -> Result<Response> {
    db.execute(&fennec_ql::parse_one(sql).unwrap_or_else(|e| panic!("{sql}: {e}")))
}

fn ok(db: &mut Database, sql: &str) {
    run(db, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn rows_of(db: &Database, sql: &str) -> Result<Vec<(u64, Vec<Value>)>> {
    let r = db.query(&fennec_ql::parse_one(sql).unwrap(), &[])?;
    Ok(r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| (r.id, r.values.clone()))
        .collect())
}

/// `x` with its field `b` dropped, then `deep` with its field `gone`
/// dropped and a row whose `meta` nests 100 lists deep, written as a
/// record of its own.
fn file() -> Vec<u8> {
    let sink = Records::default();
    let mut db = Database::with_sink(Box::new(sink.clone()));
    for sql in [
        "create collection x (a int, b int, c int, d int)",
        "put x {a: 1, b: 2, c: 3, d: 4}",
        "alter collection x drop field b",
        "create collection deep (meta json, gone int, n int)",
        "put deep {meta: 'm', gone: 0, n: 1}",
        "alter collection deep drop field gone",
    ] {
        ok(&mut db, sql);
    }
    let cid = db.collection("deep").unwrap().id;
    let mut payload = Vec::new();
    for _ in 0..100 {
        payload.extend_from_slice(&[TAG_LIST, 1]);
    }
    payload.push(TAG_NULL);
    payload.push(TAG_NULL);
    fennec_core::codec::encode_value(&mut payload, &Value::Int(2));
    let mut body = vec![fennec_core::store::OP_PUT];
    put_uvarint(&mut body, 2);
    put_uvarint(&mut body, payload.len() as u64);
    body.extend_from_slice(&payload);
    let mut file = fennec_core::engine::MAGIC.to_vec();
    file.extend_from_slice(&sink.0.lock().unwrap());
    file.extend_from_slice(&fennec_core::engine::record(REC_DATA, cid, &body));
    file
}

fn xs(db: &Database) -> Vec<(u64, Vec<Value>)> {
    rows_of(db, "get x select a, c, d order id").unwrap()
}

/// The deep row is refused where it is read, in the same words.
fn deep_refused(db: &Database) {
    for sql in ["get deep", "get deep select n where id == 2"] {
        match rows_of(db, sql) {
            Err(Error::Corrupt(m)) => assert_eq!(m, DEEP, "{sql}"),
            other => panic!("{sql}: {other:?}"),
        }
    }
    assert_eq!(
        rows_of(db, "get deep select n where id == 1").unwrap(),
        [(1, vec![Value::Int(1)])]
    );
}

#[test]
fn the_servers_compaction_compacts_once_past_a_document_it_cannot_read() {
    let dir = std::env::temp_dir().join(format!(
        "fennecdb-auto-compact-places-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.fennec");
    std::fs::write(&path, file()).unwrap();
    let db = Arc::new(RwLock::new(fennec_core::fs::open(&path).unwrap()));
    deep_refused(&db.read().unwrap());
    // A row written over: dead bytes for the policy to find.
    ok(&mut db.write().unwrap(), "put x {id: 1, a: 1, c: 3, d: 4}");
    let policy = CompactPolicy {
        ratio: f64::MIN_POSITIVE,
        floor: 1,
        memory: false,
    };
    assert!(db.read().unwrap().compact_due(&policy));
    link::auto_compact(Some(policy));
    link::keep("db.fennec", &db);
    // The compacter looks every `AUTO_COMPACT_EVERY`: the compact it runs,
    // or the refusal it counts, is the event waited for.
    let deadline = Instant::now() + Duration::from_secs(60);
    while link::AUTO_COMPACTIONS.load(Ordering::Relaxed) == 0 {
        let refused = link::AUTO_COMPACTIONS_REFUSED.load(Ordering::Relaxed);
        assert_eq!(refused, 0, "the server's compaction was refused");
        assert!(Instant::now() < deadline, "the server never compacted");
        std::thread::sleep(Duration::from_millis(10));
    }
    {
        let g = db.read().unwrap();
        assert_eq!(g.compactions(), 1);
        assert_eq!(link::AUTO_COMPACTIONS.load(Ordering::Relaxed), 1);
        assert_eq!(link::AUTO_COMPACTIONS_REFUSED.load(Ordering::Relaxed), 0);
        // What the next look asks: nothing is due.
        assert!(!g.compact_due(&policy), "{:?}", g.garbage());
        assert_eq!(g.collection("x").unwrap().schema.dropped, [1]);
        assert_eq!(g.collection("deep").unwrap().schema.dropped, [1]);
        deep_refused(&g);
    }
    ok(&mut db.write().unwrap(), "put x {a: 10, c: 30, d: 40}");
    let want = vec![
        (1, vec![Value::Int(1), Value::Int(3), Value::Int(4)]),
        (2, vec![Value::Int(10), Value::Int(30), Value::Int(40)]),
    ];
    assert_eq!(xs(&db.read().unwrap()), want);
    link::auto_compact(None);
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(xs(&db), want);
    deep_refused(&db);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
