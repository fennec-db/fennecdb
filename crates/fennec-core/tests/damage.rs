//! Records no write makes -- a damaged or hostile file's, or a primary's
//! record a replica is handed -- made here by hand, and refused as
//! `Error::Corrupt` where a file is opened and read and where a replica
//! applies them, the process whole.
//!
//! A value read off the bytes nests no deeper than a json value may be
//! written (`codec::MAX_DEPTH`, 64 levels). No write makes one deeper, so a
//! record holding 100 000 nested lists, 200 KB, is refused naming the
//! limit, never decoded a level at a time until the stack ran out, which
//! took the process down with it. And no write puts document 0.

use fennec_core::codec::{get_uvarint, put_uvarint, TAG_LIST, TAG_NULL};
use fennec_core::prelude::*;
use std::sync::{Arc, Mutex};

const LEVELS: usize = 100_000;
const REC_DATA: u8 = 3;
const DEEP: &str = "a value nests more than 64 levels deep";

/// Every record a database appends, in order.
#[derive(Clone, Default)]
struct Records(Arc<Mutex<Vec<Vec<u8>>>>);

impl Sink for Records {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
    fn rewrite(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }
}

fn exec(db: &mut Database, sql: &str) {
    db.execute(&fennec_ql::parse_one(sql).unwrap())
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn query(db: &Database, sql: &str) -> Result<Response> {
    db.query(&fennec_ql::parse_one(sql).unwrap(), &[])
}

fn deep_refusal<T: std::fmt::Debug>(r: Result<T>) {
    match r {
        Err(Error::Corrupt(m)) => assert_eq!(m, DEEP),
        other => panic!("{other:?}"),
    }
}

/// A document of a collection whose first field is `meta json`: `meta`
/// `levels` lists deep, then `rest` -- the fields after it, as written.
fn payload(levels: usize, rest: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(levels * 2 + 1 + rest.len());
    for _ in 0..levels {
        p.extend_from_slice(&[TAG_LIST, 1]);
    }
    p.push(TAG_NULL);
    p.extend_from_slice(rest);
    p
}

/// A data record of collection `cid` putting `payload` as document `id`.
fn put_record(cid: u32, id: u64, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![fennec_core::store::OP_PUT];
    put_uvarint(&mut body, id);
    put_uvarint(&mut body, payload.len() as u64);
    body.extend_from_slice(payload);
    fennec_core::engine::record(REC_DATA, cid, &body)
}

/// The records a primary writes for `schema` and one row of it, and the
/// collection's id, read off the row's data record: `[kind][cid][len]`.
fn made(schema: &str, row: &str) -> (Vec<Vec<u8>>, u32) {
    let sink = Records::default();
    let mut db = Database::with_sink(Box::new(sink.clone()));
    exec(&mut db, schema);
    let before = sink.0.lock().unwrap().len();
    exec(&mut db, row);
    let records = sink.0.lock().unwrap().clone();
    let data = records[before..]
        .iter()
        .find(|r| fennec_core::engine::record_body(r, REC_DATA).is_some())
        .expect("the row's data record");
    let cid = get_uvarint(data, &mut 1).unwrap() as u32;
    (records[..before].to_vec(), cid)
}

fn file(records: &[Vec<u8>]) -> Vec<u8> {
    let mut f = fennec_core::engine::MAGIC.to_vec();
    for r in records {
        f.extend_from_slice(r);
    }
    f
}

#[test]
fn a_record_nesting_past_the_limit_is_refused_where_a_file_is_opened() {
    let (create, cid) = made(
        "create collection d (meta json, n int)",
        "put d {meta: 'x', n: 1}",
    );
    let mut n = Vec::new();
    fennec_core::codec::encode_value(&mut n, &Value::Int(7));
    let mut records = create.clone();
    records.push(put_record(cid, 1, &payload(LEVELS, &n)));
    // Within the limit, the same file reads.
    let mut fine = create.clone();
    fine.push(put_record(cid, 1, &payload(64, &n)));

    let dir = std::env::temp_dir().join(format!("fennecdb-damage-deep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in [("deep", file(&records)), ("fine", file(&fine))] {
        let path = dir.join(format!("{name}.fennec"));
        std::fs::write(&path, &bytes).unwrap();
        // An open reads no document (docs/design/indexes.md#lazy-indexes);
        // what reads it whole or past it meets the limit.
        let db = fennec_core::fs::open(&path).unwrap();
        let mut memory = Database::new();
        memory.load(&bytes).unwrap();
        for db in [&db, &memory] {
            let reads = [
                "get d",
                "get d select n",
                "get d where n = 7",
                "get d where meta = 1",
            ];
            for sql in reads {
                match name {
                    "deep" => deep_refusal(query(db, sql)),
                    _ => assert!(query(db, sql).is_ok(), "{sql}"),
                }
            }
        }
        drop(db);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A vector index is linked at the open, each document's vector read past
/// the fields before it: there the open itself meets the limit.
#[test]
fn an_open_reading_past_a_value_nesting_past_the_limit_is_refused() {
    let (create, cid) = made(
        "create collection w (meta json, v vector<2> @hnsw)",
        "put w {meta: 'x', v: [1, 2]}",
    );
    let mut v = Vec::new();
    fennec_core::codec::encode_value(&mut v, &Value::Vector(vec![1.0, 2.0]));
    let mut records = create;
    records.push(put_record(cid, 1, &payload(LEVELS, &v)));
    deep_refusal(Database::new().load(&file(&records)));
}

#[test]
fn a_primarys_record_nesting_past_the_limit_is_refused_by_its_replica() {
    let (create, cid) = made("create collection d (meta json)", "put d {meta: 'x'}");
    let mut replica = Database::new();
    replica.apply(&create.concat()).unwrap();
    deep_refusal(replica.apply(&put_record(cid, 1, &payload(LEVELS, &[]))));
    // Put back whole, and the replica goes on: the row within the limit
    // lands and reads.
    let rows = |db: &Database| query(db, "get d").unwrap().rows().unwrap().rows.len();
    assert_eq!(rows(&replica), 0);
    replica
        .apply(&put_record(cid, 1, &payload(64, &[])))
        .unwrap();
    assert_eq!(rows(&replica), 1);
}

/// A frame running past the data record holding it is damage, refused
/// where a file is opened, read into memory or mapped as a server maps it,
/// and where a replica applies the record: a record reaches the replay
/// whole by its length, and by its sum, so a crash's torn tail is the
/// record the open's walk cuts, never a frame inside one. Stopped there as
/// at a torn tail, the frames after it vanished with no word -- the open
/// answered every row but them. A length near 2^64 is one such: added
/// unchecked, it wrapped past the test and the frame was sliced backwards,
/// a panic (the fuzzer's `image` target found it).
#[test]
fn a_frame_running_past_its_record_is_refused_where_a_file_is_opened_and_applied() {
    let (create, cid) = made("create collection d (n int)", "put d {n: 1}");
    let mut n = Vec::new();
    fennec_core::codec::encode_value(&mut n, &Value::Int(1));
    let dir = std::env::temp_dir().join(format!("fennecdb-damage-len-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rows = |db: &Database| query(db, "get d").unwrap().rows().unwrap().rows.len();
    for (i, len) in [u64::MAX - 1, n.len() as u64 + 1].into_iter().enumerate() {
        // A whole put before it in the record, and one after: neither is
        // taken.
        let mut body = Vec::new();
        for (id, len) in [(1, n.len() as u64), (2, len)] {
            body.push(fennec_core::store::OP_PUT);
            put_uvarint(&mut body, id);
            put_uvarint(&mut body, len);
            body.extend_from_slice(&n);
        }
        let mut records = create.clone();
        records.push(fennec_core::engine::record(REC_DATA, cid, &body));
        let refused = |r: Result<()>| match r {
            Err(Error::Corrupt(m)) => assert_eq!(m, "a write record cut short", "{len}"),
            other => panic!("{len}: {other:?}"),
        };
        refused(Database::new().load(&file(&records)).map(|_| ()));
        let path = dir.join(format!("{i}.fennec"));
        std::fs::write(&path, file(&records)).unwrap();
        refused(fennec_core::fs::open(&path).map(|_| ()));

        let mut replica = Database::new();
        replica.apply(&create.concat()).unwrap();
        refused(replica.apply(&records[create.len()]).map(|_| ()));
        assert_eq!(rows(&replica), 0);

        // The same record cut short by the file's end is a torn tail: the
        // open cuts it, and answers the rows before it.
        let mut torn = file(&records[..create.len()]);
        torn.extend_from_slice(&put_record(cid, 3, &n));
        let whole = torn.len();
        let last = &records[create.len()];
        torn.extend_from_slice(&last[..last.len() - 1]);
        let mut memory = Database::new();
        assert_eq!(memory.load(&torn).unwrap(), whole);
        assert_eq!(rows(&memory), 1);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What counts a record's writes -- a replica's feed, an archive's
/// (`engine::writes_in`) -- holds a frame's length to its record as the
/// replay does: added, a length near 2^64 wrapped back into the record in
/// a release build, its frames counted again, and overflowed in a debug
/// one.
#[test]
fn a_frame_running_past_its_record_is_refused_where_writes_are_counted() {
    let mut whole = vec![fennec_core::store::OP_PUT];
    put_uvarint(&mut whole, 1);
    put_uvarint(&mut whole, 1);
    whole.push(TAG_NULL);
    let count = |body: &[u8]| {
        fennec_core::engine::writes_in(&fennec_core::engine::record(REC_DATA, 1, body))
    };
    assert_eq!(count(&whole).unwrap(), 1);
    for len in [u64::MAX - 1, 1 << 63, 2] {
        let mut body = whole.clone();
        body.push(fennec_core::store::OP_PUT);
        put_uvarint(&mut body, 2);
        put_uvarint(&mut body, len);
        body.push(TAG_NULL);
        match count(&body) {
            Err(Error::Corrupt(m)) => assert_eq!(m, "a frame runs past its record", "{len}"),
            other => panic!("{len}: {other:?}"),
        }
    }
}

/// No write gives a document the id 0 (`allocate_id` starts at 1, and a
/// put naming 0 is given an id) or `u64::MAX` (past which the next id has
/// nowhere to go), so a frame putting either is damage. Taken, 0 went into
/// the id index's sparse map, and the dense array growing over it for the
/// next document indexed slot -1, a panic; `u64::MAX` overflowed the next
/// id, which a release build wrapped to 0 and gave the next write. At the
/// open of a file and at a replica's apply alike (the fuzzer's `image`
/// target found both, crates/fennec-fuzz).
#[test]
fn a_frame_putting_an_id_no_write_gives_is_refused_where_a_file_is_opened_and_applied() {
    let (create, cid) = made("create collection d (n int)", "put d {n: 1}");
    let mut n = Vec::new();
    fennec_core::codec::encode_value(&mut n, &Value::Int(1));
    let dir = std::env::temp_dir().join(format!("fennecdb-damage-ids-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for id in [0, u64::MAX] {
        let refused = |r: Result<()>| match r {
            Err(Error::Corrupt(m)) => {
                assert_eq!(
                    m,
                    format!("a frame puts document {id}, an id no write gives")
                )
            }
            other => panic!("{id}: {other:?}"),
        };
        let mut records = create.clone();
        records.push(put_record(cid, id, &n));
        records.push(put_record(cid, 1, &n));

        // Read into memory, and mapped where a server opens its file.
        refused(Database::new().load(&file(&records)).map(|_| ()));
        let path = dir.join(format!("{id}.fennec"));
        std::fs::write(&path, file(&records)).unwrap();
        refused(fennec_core::fs::open(&path).map(|_| ()));

        // A replica refuses it before it changes anything, and goes on.
        let mut replica = Database::new();
        replica.apply(&create.concat()).unwrap();
        refused(replica.apply(&records[create.len()..].concat()).map(|_| ()));
        let rows = |db: &Database| query(db, "get d").unwrap().rows().unwrap().rows.len();
        assert_eq!(rows(&replica), 0);
        replica.apply(&records[create.len() + 1]).unwrap();
        assert_eq!(rows(&replica), 1);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A schema read off the bytes holds its numbers to the ranges the
/// language reads them in (`Parser::int_in`): an `@hnsw` whose `m` is past
/// 32 767, or a dimension past 10^9 or of none, is damage, refused naming
/// the field and its range where a file is opened -- read into memory and
/// mapped -- and where a replica applies the record, before a document is
/// read. Read as written, a file of a few dozen bytes declaring `m` = 2^40
/// and holding one vector had the open's index ask for 2^41 links a node,
/// and the process was killed. No `create` makes such a field, so one a
/// version before the bounds made is refused alike: fennecdb is alpha.
#[test]
fn a_schema_past_the_languages_ranges_is_refused_where_a_file_is_opened_and_applied() {
    let hnsw = |dim: usize, m: usize| {
        let spec = VectorIndexSpec {
            m,
            ..VectorIndexSpec::default()
        };
        Field::new("v", DataType::Vector(dim, VecPrec::F32)).indexed(IndexKind::Vector(spec))
    };
    let dims = "`v`: a vector's dimension is 1 to 1000000000, not";
    let ms = "`v`: @hnsw's `m` is 2 to 32767, not";
    let dir = std::env::temp_dir().join(format!("fennecdb-damage-ranges-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, (field, says)) in [
        (hnsw(2, 1 << 40), format!("{ms} 1099511627776")),
        (hnsw(2, 32_768), format!("{ms} 32768")),
        (hnsw(2, 1), format!("{ms} 1")),
        (hnsw(1 << 40, 16), format!("{dims} 1099511627776")),
        (
            Field::new("v", DataType::Vector(1_000_000_001, VecPrec::F16)),
            format!("{dims} 1000000001"),
        ),
        (
            Field::new("v", DataType::Sparse(1 << 40)),
            format!("{dims} 1099511627776"),
        ),
        (
            Field::new(
                "v",
                DataType::List(Box::new(DataType::Vector(0, VecPrec::F32))),
            ),
            format!("{dims} 0"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        // Made as no `create` makes it -- `Schema::new` refuses the field
        // -- and as a crafted file holds it: a create takes the schema it
        // is handed, and its record is the file's.
        let sink = Records::default();
        let mut db = Database::with_sink(Box::new(sink.clone()));
        let mut schema = Schema::new("w", vec![Field::new("n", DataType::Int)]).unwrap();
        schema.fields.push(field);
        db.execute(&Statement::CreateCollection {
            schema,
            if_not_exists: false,
        })
        .unwrap();
        let mut records = sink.0.lock().unwrap().clone();
        // The create's record, kind 1 in a checked record's low five bits:
        // `[kind][cid][len]`.
        let create = records.iter().find(|r| r[0] & 0x1f == 1).expect("a create");
        let cid = get_uvarint(create, &mut 1).unwrap() as u32;
        // And a document holding a vector, put by hand: the in-memory
        // database above would build the index it names.
        let mut doc = Vec::new();
        fennec_core::codec::encode_value(&mut doc, &Value::Int(1));
        fennec_core::codec::encode_value(&mut doc, &Value::Vector(vec![1.0, 2.0]));
        records.push(put_record(cid, 1, &doc));

        let refused = |r: Result<()>| match r {
            Err(Error::Corrupt(m)) => assert_eq!(m, says),
            other => panic!("{says}: {other:?}"),
        };
        refused(Database::new().load(&file(&records)).map(|_| ()));
        let path = dir.join(format!("{i}.fennec"));
        std::fs::write(&path, file(&records)).unwrap();
        refused(fennec_core::fs::open(&path).map(|_| ()));
        refused(Database::new().apply(&records.concat()).map(|_| ()));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
