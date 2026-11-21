//! A `compact` keeps every place a collection's documents hold.
//!
//! `alter collection x drop field b` leaves `b`'s place in the documents,
//! and the writes after it put a `null` there. A compact used to take the
//! dropped places out of every document and renumber the schema's places,
//! which went wrong two ways. Refused at a later collection -- a document
//! no decoder reads, such as a value nested past 64 levels -- it left the
//! collections before it renumbered in memory while the file kept their
//! places, so a later write read one way in the process and another after
//! a reopen, and the server's compaction tried again every five seconds.
//! And a compact that succeeded told no copy: it appends no record, so a
//! replica, an archive or a standby kept the places and read every later
//! write of that collection one field early -- one that compacted itself
//! first read its primary's one field late.
//!
//! Now a compact makes a dropped place's value a one-byte `null` where it
//! can read the document, copies one it cannot read as it is, and renumbers
//! nothing: memory, the file and every copy hold the same places, and a
//! compact is never refused for what a document holds.

use fennec_core::codec::{put_uvarint, TAG_LIST, TAG_NULL};
use fennec_core::engine::{compact_when_due, CompactPolicy, Compactor};
use fennec_core::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const DEEP: &str = "a value nests more than 64 levels deep";
const REC_DATA: u8 = 3;

/// Writes with their numbers, as a sink is handed them.
type Numbered = Vec<(u64, Vec<u8>)>;

/// Every record a database appends, in order -- the file past its
/// signature -- and with the numbers a replica's feed would carry them
/// under.
#[derive(Clone, Default)]
struct Records {
    file: Arc<Mutex<Vec<u8>>>,
    numbered: Arc<Mutex<Numbered>>,
}

impl Sink for Records {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn record(&mut self, seq: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        self.numbered.lock().unwrap().push((seq, bytes.to_vec()));
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        *self.file.lock().unwrap() = bytes[fennec_core::engine::MAGIC.len()..].to_vec();
        Ok(())
    }
}

impl Records {
    /// The records numbered past `from`, in order.
    fn after(&self, from: u64) -> Vec<Vec<u8>> {
        let numbered = self.numbered.lock().unwrap();
        numbered
            .iter()
            .filter(|(seq, _)| *seq > from)
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// The file as a database opens it.
    fn image(&self) -> Vec<u8> {
        let mut file = fennec_core::engine::MAGIC.to_vec();
        file.extend_from_slice(&self.file.lock().unwrap());
        file
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

/// Each of `x`'s rows, field by field.
fn xs(db: &Database) -> Vec<(u64, Vec<Value>)> {
    rows_of(db, "get x select a, c, d order id").unwrap()
}

fn refused<T: std::fmt::Debug>(r: Result<T>) {
    match r {
        Err(Error::Corrupt(m)) => assert_eq!(m, DEEP),
        other => panic!("{other:?}"),
    }
}

fn int(i: i64) -> Value {
    Value::Int(i)
}

/// What `x` holds once its field `b` was dropped, the collection compacted
/// and a row written after it.
fn want() -> Vec<(u64, Vec<Value>)> {
    vec![
        (1, vec![int(1), int(3), int(4)]),
        (2, vec![int(10), int(30), int(40)]),
    ]
}

const LATER: &str = "put x {a: 10, c: 30, d: 40}";

/// `x` with its field `b` dropped, made first; with `deep`, a collection
/// made after it with a dropped field too, holding a row whose `meta`
/// nests 100 lists deep -- whole by its sum, read by no binary since the
/// depth was bounded -- written as a record of its own. The records, and
/// the sink that took them.
fn records(deep: bool) -> (Vec<u8>, Records) {
    let sink = Records::default();
    let mut db = Database::with_sink(Box::new(sink.clone()));
    for sql in [
        "create collection x (a int, b int, c int, d int)",
        "put x {a: 1, b: 2, c: 3, d: 4}",
        "alter collection x drop field b",
    ] {
        ok(&mut db, sql);
    }
    if !deep {
        return (sink.image(), sink);
    }
    for sql in [
        "create collection deep (meta json, gone int, n int)",
        "put deep {meta: 'm', gone: 0, n: 1}",
        "alter collection deep drop field gone",
    ] {
        ok(&mut db, sql);
    }
    let cid = db.collection("deep").unwrap().id;
    // `meta` 100 lists deep, the dropped place a `null`, then `n`.
    let mut payload = Vec::new();
    for _ in 0..100 {
        payload.extend_from_slice(&[TAG_LIST, 1]);
    }
    payload.push(TAG_NULL);
    payload.push(TAG_NULL);
    fennec_core::codec::encode_value(&mut payload, &int(2));
    let mut body = vec![fennec_core::store::OP_PUT];
    put_uvarint(&mut body, 2);
    put_uvarint(&mut body, payload.len() as u64);
    body.extend_from_slice(&payload);
    let mut file = sink.image();
    file.extend_from_slice(&fennec_core::engine::record(REC_DATA, cid, &body));
    (file, sink)
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fennecdb-compact-places-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn written(dir: &Path, deep: bool) -> PathBuf {
    let path = dir.join("db.fennec");
    std::fs::write(&path, records(deep).0).unwrap();
    path
}

fn loaded(bytes: &[u8]) -> Database {
    let mut db = Database::new();
    db.load(bytes).unwrap();
    db
}

/// The deep row is refused where it is read, by every way to it, in the
/// same words.
fn deep_refused(db: &Database) {
    refused(rows_of(db, "get deep"));
    refused(rows_of(db, "get deep select n where id == 2"));
    assert_eq!(
        rows_of(db, "get deep select n where id == 1").unwrap(),
        [(1, vec![int(1)])]
    );
}

fn places(db: &Database, collection: &str) -> Vec<usize> {
    db.collection(collection).unwrap().schema.dropped.clone()
}

/// The first case of the review: a compact meeting a document it cannot
/// read, in a collection made after one holding a dropped field. Over a
/// mapped file and in memory it succeeds, keeps every place, and the row
/// written after it reads the same in the process, after a reopen and
/// read whole into memory.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
#[test]
fn a_compact_past_a_document_it_cannot_read_keeps_every_place() {
    let d = dir("statement");
    let path = written(&d, true);
    let mut db = fennec_core::fs::open(&path).unwrap();
    deep_refused(&db);
    ok(&mut db, "compact");
    assert_eq!((places(&db, "x"), places(&db, "deep")), (vec![1], vec![1]));
    deep_refused(&db);
    ok(&mut db, LATER);
    assert_eq!(xs(&db), want());
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(xs(&db), want());
    deep_refused(&db);
    drop(db);
    let mut memory = loaded(&std::fs::read(&path).unwrap());
    assert_eq!(xs(&memory), want());
    // Read into memory, the compact copies the records into fresh
    // segments: the same again.
    ok(&mut memory, "compact");
    ok(&mut memory, "put x {a: 100, c: 300, d: 400}");
    assert_eq!(xs(&memory)[1], want()[1]);
    assert_eq!(xs(&memory)[2].1, [int(100), int(300), int(400)]);
    deep_refused(&memory);
    std::fs::remove_dir_all(&d).unwrap();
}

/// A copy applying its primary's records holds the places its primary
/// holds, whichever of the two compacts: the primary before its writes,
/// the copy before it applies them, or both.
#[test]
fn a_copy_reads_the_writes_after_a_compact_as_its_primary_does() {
    for deep in [false, true] {
        for (primary_compacts, copy_compacts) in [(true, false), (false, true), (true, true)] {
            let (image, _) = records(deep);
            let sink = Records::default();
            let mut primary = Database::with_sink(Box::new(sink.clone()));
            primary.load(&image).unwrap();
            let mut copy = loaded(&image);
            let from = primary.change_seq();
            if primary_compacts {
                ok(&mut primary, "compact");
            }
            if copy_compacts {
                ok(&mut copy, "compact");
            }
            ok(&mut primary, LATER);
            for r in sink.after(from) {
                copy.apply(&r).unwrap();
            }
            let case = (deep, primary_compacts, copy_compacts);
            assert_eq!(xs(&primary), want(), "{case:?}");
            assert_eq!(xs(&copy), want(), "{case:?}");
        }
    }
}

fn policy() -> CompactPolicy {
    CompactPolicy {
        ratio: f64::MIN_POSITIVE,
        floor: 1,
        memory: false,
    }
}

/// A compaction nobody asked for meets the document: the look a host runs
/// (`compact_when_due`, the read lock's look and the compact beside the
/// database) succeeds once and is not due again, the deep row refused as
/// before; a `Compactor`'s thread, the native library's, alike. The
/// server's own loop is fennec-http's `tests/auto_compact_places.rs`.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
#[test]
fn a_hosts_compaction_runs_once_past_a_document_it_cannot_read() {
    let d = dir("auto");
    let path = written(&d, true);
    let db = RwLock::new(fennec_core::fs::open(&path).unwrap());
    // A row written over: dead bytes for the policy to find.
    ok(&mut db.write().unwrap(), "put x {id: 1, a: 1, c: 3, d: 4}");
    compact_when_due(&db, &policy()).expect("due").unwrap();
    {
        let g = db.read().unwrap();
        assert_eq!(g.compactions(), 1);
        assert!(!g.compact_due(&policy()), "{:?}", g.garbage());
        assert_eq!((places(&g, "x"), places(&g, "deep")), (vec![1], vec![1]));
        deep_refused(&g);
    }
    assert!(compact_when_due(&db, &policy()).is_none());
    ok(&mut db.write().unwrap(), LATER);
    let db = db.into_inner().unwrap();
    assert_eq!(xs(&db), want());
    drop(db);

    // Opened again: dead records once more, and the native library's
    // thread compacts them, once.
    let db = Arc::new(RwLock::new(fennec_core::fs::open(&path).unwrap()));
    ok(
        &mut db.write().unwrap(),
        "put x {id: 2, a: 10, c: 30, d: 40}",
    );
    assert!(db.read().unwrap().compact_due(&policy()));
    let compactor = Compactor::start_every(&db, policy(), Duration::from_millis(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while db.read().unwrap().compactions() == 0 {
        assert!(Instant::now() < deadline, "the compactor never compacted");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(compactor);
    let g = db.read().unwrap();
    assert_eq!(g.compactions(), 1);
    assert!(!g.compact_due(&policy()));
    assert_eq!(xs(&g), want());
    deep_refused(&g);
    drop(g);
    drop(db);
    assert_eq!(xs(&fennec_core::fs::open(&path).unwrap()), want());
    std::fs::remove_dir_all(&d).unwrap();
}

/// A graph holding tombstones is built again by a compact, which reads
/// every document's vector: one it cannot read would leave the graph as it
/// stands (the engine's own test, `engine::maintenance::tests`). No file
/// brings one to it: the open reads every vector the graph holds, past
/// every value before it, and refuses the file in the deep value's words --
/// as `create index`, a write and a replica's apply refuse it.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
#[test]
fn a_vector_past_a_value_none_reads_is_refused_at_the_open() {
    let d = dir("graph");
    let path = d.join("db.fennec");
    let sink = Records::default();
    let mut db = Database::with_sink(Box::new(sink.clone()));
    ok(
        &mut db,
        "create collection g (meta json, v vector<2> @hnsw(cosine))",
    );
    ok(&mut db, "put g {id: 1, meta: 'a', v: [1, 0]}");
    let cid = db.collection("g").unwrap().id;
    let mut payload = Vec::new();
    for _ in 0..100 {
        payload.extend_from_slice(&[TAG_LIST, 1]);
    }
    payload.push(TAG_NULL);
    fennec_core::codec::encode_value(&mut payload, &Value::Vector(vec![0.6, 0.8]));
    let mut body = vec![fennec_core::store::OP_PUT];
    put_uvarint(&mut body, 3);
    put_uvarint(&mut body, payload.len() as u64);
    body.extend_from_slice(&payload);
    let mut file = sink.image();
    file.extend_from_slice(&fennec_core::engine::record(REC_DATA, cid, &body));
    std::fs::write(&path, &file).unwrap();
    refused(fennec_core::fs::open(&path).map(|_| ()));
    let mut memory = Database::new();
    refused(memory.load(&file).map(|_| ()));
    std::fs::remove_dir_all(&d).unwrap();
}

/// A file a release compacted with a dropped field in it -- the places
/// taken out, as every release from 0.1.7 to 0.2.0 did -- opens and reads
/// as it was: there is nothing to undo, and nothing is renumbered back. The
/// format is the same: the records are written here as that compact left
/// them, a schema of three fields and documents of three values.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
#[test]
fn a_file_a_release_compacted_reads_as_it_was() {
    let d = dir("stripped");
    let path = d.join("db.fennec");
    let sink = Records::default();
    let mut db = Database::with_sink(Box::new(sink.clone()));
    ok(&mut db, "create collection x (a int, c int, d int)");
    ok(&mut db, "put x {a: 1, c: 3, d: 4}");
    std::fs::write(&path, sink.image()).unwrap();
    let mut db = fennec_core::fs::open(&path).unwrap();
    assert!(places(&db, "x").is_empty());
    assert_eq!(xs(&db), want()[..1]);
    ok(&mut db, "compact");
    ok(&mut db, LATER);
    assert!(places(&db, "x").is_empty());
    assert_eq!(xs(&db), want());
    drop(db);
    assert_eq!(xs(&fennec_core::fs::open(&path).unwrap()), want());
    std::fs::remove_dir_all(&d).unwrap();
}

/// A file holding dropped places no release compacted keeps them through
/// this compact: the dropped field's value becomes a one-byte `null` in
/// each document a field follows it in, and goes from one it ends, as a
/// write after the drop leaves them.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
#[test]
fn a_file_with_dropped_places_keeps_them_through_a_compact() {
    let d = dir("kept");
    let path = d.join("db.fennec");
    let sink = Records::default();
    let mut db = Database::with_sink(Box::new(sink.clone()));
    ok(&mut db, "create collection x (a int, b text, c int, d int)");
    ok(&mut db, "create collection y (a int, z text)");
    ok(
        &mut db,
        "put x {a: 1, b: 'a long dropped value', c: 3, d: 4}",
    );
    ok(&mut db, "put y {a: 1, z: 'gone at the end'}");
    ok(&mut db, "alter collection x drop field b");
    ok(&mut db, "alter collection y drop field z");
    std::fs::write(&path, sink.image()).unwrap();
    let raw = |db: &Database, c: &str| {
        let store = &db.collection(c).unwrap().store;
        store.raw(1).unwrap().unwrap().to_vec()
    };
    let mut db = fennec_core::fs::open(&path).unwrap();
    let (x_was, y_was) = (raw(&db, "x"), raw(&db, "y"));
    ok(&mut db, "compact");
    assert_eq!((places(&db, "x"), places(&db, "y")), (vec![1], vec![1]));
    // `a`, a null of one byte, `c` and `d`: what a write puts.
    let mut written = Vec::new();
    for v in [int(1), Value::Null, int(3), int(4)] {
        fennec_core::codec::encode_value(&mut written, &v);
    }
    assert_eq!(raw(&db, "x"), written);
    assert!(raw(&db, "x").len() < x_was.len());
    let mut just_a = Vec::new();
    fennec_core::codec::encode_value(&mut just_a, &int(1));
    assert_eq!(raw(&db, "y"), just_a);
    assert!(raw(&db, "y").len() < y_was.len());
    assert_eq!(xs(&db), want()[..1]);
    ok(&mut db, LATER);
    // A second compact finds nothing more to reclaim.
    let x_now = raw(&db, "x");
    ok(&mut db, "compact");
    assert_eq!(raw(&db, "x"), x_now);
    drop(db);
    let db = fennec_core::fs::open(&path).unwrap();
    assert_eq!(places(&db, "x"), [1]);
    assert_eq!(xs(&db), want());
    assert_eq!(rows_of(&db, "get y select a").unwrap(), [(1, vec![int(1)])]);
    std::fs::remove_dir_all(&d).unwrap();
}

/// A copy of a primary that took a dropped field's places out -- a compact
/// of 0.2.0 or before, which no record says -- refuses that primary's next
/// alter of the collection by name, rather than take a schema of other
/// places than its documents hold: what a server's follower takes an image
/// of its primary for.
#[test]
fn a_copy_refuses_an_alter_of_a_primary_that_took_the_places_out() {
    let sink = Records::default();
    // The primary as a release's compact left it: three fields.
    let mut primary = Database::with_sink(Box::new(sink.clone()));
    ok(&mut primary, "create collection x (a int, c int, d int)");
    // The copy as it stood before: `b` dropped, its place kept.
    let mut copy = Database::new();
    ok(
        &mut copy,
        "create collection x (a int, b int, c int, d int)",
    );
    ok(&mut copy, "alter collection x drop field b");
    assert_eq!(
        primary.collection("x").unwrap().id,
        copy.collection("x").unwrap().id
    );
    let from = primary.change_seq();
    for change in [
        "alter collection x add field e int",
        "alter collection x rename field d to dd",
        "alter collection x drop field c",
    ] {
        ok(&mut primary, change);
        let records = sink.after(from);
        let r = copy.apply(records.last().unwrap());
        match r {
            Err(Error::Corrupt(m)) => {
                assert!(m.contains("moved the fields of `x`"), "{change}: {m}");
                assert!(m.contains("seed the copy again"), "{change}: {m}");
            }
            other => panic!("{change}: {other:?}"),
        }
        assert_eq!(places(&copy, "x"), [1], "{change}");
    }
    // A primary keeping its places is followed alter by alter.
    let sink = Records::default();
    let mut primary = Database::with_sink(Box::new(sink.clone()));
    ok(
        &mut primary,
        "create collection x (a int, b int, c int, d int)",
    );
    ok(&mut primary, "alter collection x drop field b");
    ok(&mut primary, "compact");
    let from = primary.change_seq();
    for change in [
        "alter collection x add field e int",
        "alter collection x rename field d to dd",
        "alter collection x drop field c",
    ] {
        ok(&mut primary, change);
    }
    for r in sink.after(from) {
        copy.apply(&r).unwrap();
    }
    assert_eq!(places(&copy, "x"), places(&primary, "x"));
}

/// A copy and a primary holding other places for one collection -- one
/// side's compact of 0.2.0 or before took a dropped field's out, which no
/// record says -- meet at the primary's next write of it: the copy reads a
/// write as it was made, a value at every place up to the last field and
/// none past it, and refuses one that is not, by name, either way round,
/// rather than read it a field off (`[10, 40, null]` where the primary
/// took the place out, `[10, null, 30]` where the copy did). A write of a
/// collection whose places agree lands beside it.
#[test]
fn a_copy_refuses_a_write_made_by_other_places_than_it_holds() {
    let stripped: &[&str] = &[
        "create collection x (a int, c int, d int)",
        "put x {a: 1, c: 3, d: 4}",
    ];
    let kept: &[&str] = &[
        "create collection x (a int, b int, c int, d int)",
        "put x {a: 1, b: 2, c: 3, d: 4}",
        "alter collection x drop field b",
    ];
    for (primary_sql, copy_sql) in [(stripped, kept), (kept, stripped)] {
        let sink = Records::default();
        let mut primary = Database::with_sink(Box::new(sink.clone()));
        let mut copy = Database::new();
        for (db, sql) in [(&mut primary, primary_sql), (&mut copy, copy_sql)] {
            for s in sql {
                ok(db, s);
            }
            ok(db, "create collection y (n int)");
        }
        assert_eq!(
            primary.collection("x").unwrap().id,
            copy.collection("x").unwrap().id
        );
        let held = xs(&copy);
        let from = primary.change_seq();
        ok(&mut primary, "put x {a: 10, c: 30, d: 40}");
        let records = sink.after(from);
        match copy.apply(&records[0]) {
            Err(Error::Corrupt(m)) => {
                assert!(m.contains("moved the fields of `x`"), "{m}");
                assert!(m.contains("seed the copy again"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        // Put back whole: the copy holds what it held, and goes on.
        assert_eq!(xs(&copy), held);
        assert!(copy.failure().is_none());
        let from = primary.change_seq();
        ok(&mut primary, "put y {n: 1}");
        for r in sink.after(from) {
            copy.apply(&r).unwrap();
        }
        assert_eq!(
            rows_of(&copy, "get y select n").unwrap(),
            rows_of(&primary, "get y select n").unwrap()
        );
    }
}

/// A read past a dropped place -- a filter's fields, an aggregate's and its
/// group keys, a sort's keys and a computed column's, which pass a dropped
/// place's `null` by its byte, and a plain `get` written as JSON -- reads
/// the value a write before the drop left there as a value it steps over,
/// and the `null` a write after it or a compact puts there by its byte:
/// the same rows before the compact and after it.
#[test]
fn reads_past_a_dropped_place_answer_alike_before_and_after_a_compact() {
    let mut db = Database::new();
    for sql in [
        "create collection x (a int, b text, c int, d text)",
        "put x {a: 1, b: 'gone', c: 3, d: 'one'}",
        "alter collection x drop field b",
        "put x {a: 2, c: 30, d: 'two'}",
    ] {
        ok(&mut db, sql);
    }
    let text = |s: &str| Value::Text(s.into());
    for compacted in [false, true] {
        if compacted {
            ok(&mut db, "compact");
        }
        assert_eq!(
            rows_of(&db, "get x select a, c, d where c > 1 order id").unwrap(),
            [
                (1, vec![int(1), int(3), text("one")]),
                (2, vec![int(2), int(30), text("two")])
            ],
            "compacted: {compacted}"
        );
        assert_eq!(
            rows_of(&db, "get x select a where d == 'two' and c == 30").unwrap(),
            [(2, vec![int(2)])],
            "compacted: {compacted}"
        );
        for (sql, want) in [
            (
                "get x where id >= 1 limit 10",
                r#"[{"id":1,"a":1,"c":3,"d":"one"},{"id":2,"a":2,"c":30,"d":"two"}]"#,
            ),
            (
                "get x select c, d where id >= 1 limit 10",
                r#"[{"c":3,"d":"one"},{"c":30,"d":"two"}]"#,
            ),
        ] {
            let mut json = String::new();
            let stmt = fennec_ql::parse_one(sql).unwrap();
            assert_eq!(
                db.query_json(&stmt, &[], &mut json).unwrap(),
                Some(2),
                "{sql}"
            );
            assert_eq!(json, want, "{sql}, compacted: {compacted}");
        }
        // An aggregate's fields and its group keys, a sort's keys and a
        // computed column's, read past the place: the same answers.
        for (sql, want) in [
            (
                "get x select d, count(*), sum(c) group d order d",
                vec![
                    vec![text("one"), int(1), int(3)],
                    vec![text("two"), int(1), int(30)],
                ],
            ),
            ("get x select sum(c), max(c)", vec![vec![int(33), int(30)]]),
            (
                "get x select a, d order c desc",
                vec![vec![int(2), text("two")], vec![int(1), text("one")]],
            ),
            (
                "get x select a order c * 2 desc limit 1",
                vec![vec![int(2)]],
            ),
            (
                "get x select a, c + 1 as n order id",
                vec![vec![int(1), int(4)], vec![int(2), int(31)]],
            ),
        ] {
            let got: Vec<Vec<Value>> = rows_of(&db, sql)
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                .into_iter()
                .map(|(_, v)| v)
                .collect();
            assert_eq!(got, want, "{sql}, compacted: {compacted}");
        }
    }
}

/// A collection holds `MAX_PLACES` places over its life, its dropped
/// fields' among them: a field past them is refused, saying how to be rid
/// of them, and so is a `create` declaring more.
#[test]
fn a_collection_takes_no_place_past_its_bound() {
    let max = fennec_core::schema::MAX_PLACES;
    let fields: Vec<String> = (0..max - 1).map(|i| format!("f{i} int")).collect();
    let mut db = Database::new();
    ok(
        &mut db,
        &format!("create collection w ({})", fields.join(", ")),
    );
    ok(&mut db, "alter collection w drop field f0");
    // The last place.
    ok(&mut db, "alter collection w add field last int");
    let schema = db.collection("w").unwrap().schema.clone();
    assert_eq!(schema.width(), max);
    for (change, dropped) in [
        ("alter collection w add field more int", 1),
        ("alter collection w drop field f1", 2),
        ("alter collection w add field more int", 2),
    ] {
        match run(&mut db, change) {
            Ok(_) if change.contains("drop") => continue,
            Err(Error::Query(m)) => {
                assert_eq!(
                    m,
                    format!(
                        "`w` holds {max} places -- its {} fields and the {dropped} its dropped \
                         fields left, which its documents keep -- and takes no field more: make \
                         the collection again (create another and copy the rows into it) to be \
                         rid of the dropped places",
                        max - dropped
                    )
                );
            }
            other => panic!("{change}: {other:?}"),
        }
    }
    // A compact frees none of them.
    ok(&mut db, "compact");
    assert!(run(&mut db, "alter collection w add field more int").is_err());
    let wide: Vec<String> = (0..=max).map(|i| format!("f{i} int")).collect();
    let text = format!("create collection v ({})", wide.join(", "));
    match fennec_ql::parse_one(&text).map(|s| db.execute(&s)) {
        Err(e) | Ok(Err(e)) => assert!(
            e.to_string().contains(&format!(
                "declares {} fields, and a collection holds {max} at most",
                max + 1
            )),
            "{e}"
        ),
        Ok(Ok(r)) => panic!("{r:?}"),
    }
}

/// A collection holding more places than the bound opens, reads, and takes
/// every alter that adds no place -- a rename, a drop -- refusing an add in
/// words that fit it. 0.2.0 had no bound: a collection of more fields than
/// it, or as many and a dropped one before a compact, is a file a release
/// wrote. A rename went through `Schema::new`'s count, and was refused
/// ("declares 4097 fields") though it adds no place. Made here as 0.2.0's
/// parser made one, with nothing counting it: a `Schema` built whole and
/// handed to `create collection`, which counts nothing itself (the bound is
/// `Schema::new`'s, a declared schema's and an add's), and its records read
/// again as an open reads a file.
#[test]
fn a_collection_past_the_bound_takes_every_alter_but_an_add() {
    let max = fennec_core::schema::MAX_PLACES;
    let made = |name: &str, fields: usize, dropped: Vec<usize>| Schema {
        name: name.to_string(),
        fields: (0..fields)
            .map(|i| Field::new(format!("f{i}"), DataType::Int))
            .collect(),
        paths: Vec::new(),
        dropped,
        composites: Vec::new(),
    };
    let sink = Records::default();
    {
        let mut db = Database::with_sink(Box::new(sink.clone()));
        // `w` a field past the bound; `v` within it by its fields, past it
        // by the four places its dropped fields left.
        for schema in [
            made("w", max + 1, Vec::new()),
            made("v", max - 2, vec![1, 3, 5, 7]),
        ] {
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .unwrap();
        }
        ok(&mut db, &format!("put w {{f0: 10, f{max}: 20}}"));
        ok(&mut db, &format!("put v {{f0: 10, f{}: 20}}", max - 3));
    }
    let mut db = loaded(&sink.image());
    for (c, last) in [("w", max), ("v", max - 3)] {
        let read = |db: &Database, field: &str| {
            rows_of(db, &format!("get {c} select f0, {field}")).unwrap()
        };
        assert_eq!(
            read(&db, &format!("f{last}")),
            [(1, vec![int(10), int(20)])]
        );
        ok(
            &mut db,
            &format!("alter collection {c} rename field f{last} to last"),
        );
        assert_eq!(read(&db, "last"), [(1, vec![int(10), int(20)])]);
        ok(&mut db, &format!("alter collection {c} drop field f1"));
        assert_eq!(read(&db, "last"), [(1, vec![int(10), int(20)])]);
        let schema = db.collection(c).unwrap().schema.clone();
        let (n, dropped) = (schema.fields.len(), schema.dropped.len());
        let way = match n < max {
            true => {
                "make the collection again (create another and copy the rows into it) to be \
                 rid of the dropped places"
            }
            // Made again, it would hold its fields still: no way out but
            // fewer.
            false => {
                "its fields alone fill the places a collection holds, and making it again \
                      keeps them"
            }
        };
        let words = format!(
            "`{c}` holds {} places -- its {n} fields and the {dropped} its dropped fields left, \
             which its documents keep -- and takes no field more: {way}",
            n + dropped
        );
        match run(&mut db, &format!("alter collection {c} add field more int")) {
            Err(Error::Query(m)) => assert_eq!(m, words),
            other => panic!("{c}: {other:?}"),
        }
        assert_eq!(db.collection(c).unwrap().schema, schema);
    }
}
