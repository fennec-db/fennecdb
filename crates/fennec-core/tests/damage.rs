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

/// A frame whose length is near 2^64 is a frame cut short, as one past its
/// record's end is: an open's replay stops there, as at a torn tail, and a
/// replica refuses the record. Added unchecked, the length wrapped past the
/// test and the frame was sliced backwards, a panic (the fuzzer's `image`
/// target found it).
#[test]
fn a_frame_whose_length_passes_the_address_space_is_cut_short() {
    let (create, cid) = made("create collection d (n int)", "put d {n: 1}");
    let mut body = vec![fennec_core::store::OP_PUT];
    put_uvarint(&mut body, 1);
    put_uvarint(&mut body, u64::MAX - 1);
    body.extend_from_slice(&[0; 8]);
    let mut records = create.clone();
    records.push(fennec_core::engine::record(REC_DATA, cid, &body));
    let rows = |db: &Database| query(db, "get d").unwrap().rows().unwrap().rows.len();

    let mut memory = Database::new();
    memory.load(&file(&records)).unwrap();
    assert_eq!(rows(&memory), 0);
    let dir = std::env::temp_dir().join(format!("fennecdb-damage-len-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("len.fennec");
    std::fs::write(&path, file(&records)).unwrap();
    let mapped = fennec_core::fs::open(&path).unwrap();
    assert_eq!(rows(&mapped), 0);
    drop(mapped);
    std::fs::remove_dir_all(&dir).unwrap();

    let mut replica = Database::new();
    replica.apply(&create.concat()).unwrap();
    assert!(matches!(
        replica.apply(&records[create.len()]),
        Err(Error::Corrupt(_))
    ));
    assert_eq!(rows(&replica), 0);
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
