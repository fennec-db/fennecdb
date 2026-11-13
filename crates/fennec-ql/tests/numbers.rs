//! A number a statement writes -- a dimension, an index's parameter, a
//! beam, a count of candidates or words, `fuse`'s `k` -- is read as written
//! or refused naming its range, never cut or clamped to fit
//! (`Parser::int_in`). The ranges are within `u32`, so what is refused here
//! past `u32::MAX` is what a wasm32 build's `as usize` read as its low 32
//! bits: the same code reads it on every target.

use fennec_core::prelude::*;
use fennec_ql::parse_one;

fn refused(sql: &str) -> String {
    parse_one(sql).expect_err(sql).to_string()
}

fn run(db: &mut Database, sql: &str) -> Result<Response> {
    db.execute_with(&parse_one(sql)?, &[])
}

#[test]
fn a_number_past_its_range_is_refused_naming_it() {
    let v = "create collection v (e vector<3>";
    for (sql, says) in [
        // A wasm32 build described this as `vector<1>`, and took `[0.5]`.
        (
            "create collection v (e vector<4294967297>)".to_string(),
            "a vector's dimension is 1 to 1000000000, not 4294967297",
        ),
        (
            "create collection v (e vector<1000000001, f16>)".into(),
            "a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
        (
            "create collection v (e vector<0>)".into(),
            "a vector's dimension is 1 to 1000000000, not 0",
        ),
        (
            "create collection v (s sparse<4294967297>)".into(),
            "a vector's dimension is 1 to 1000000000, not 4294967297",
        ),
        // Clamped, `m=1` built an index of 2; past `MAX_M` a node's count
        // of links was cut to 16 bits.
        (format!("{v} @hnsw(m=1))"), "`m` is 2 to 32767, not 1"),
        (
            format!("{v} @hnsw(m=32768))"),
            "`m` is 2 to 32767, not 32768",
        ),
        (format!("{v} @hnsw(m=-1))"), "`m` is 2 to 32767, not -1"),
        // pgvector's floor, which SQL's `with (..)` takes: 3 was built as
        // 8, and 4 to 7 are read as written.
        (
            format!("{v} @hnsw(cosine, ef_construction=3))"),
            "`ef_construction` is 4 to 4294967295, not 3",
        ),
        (
            format!("{v} @hnsw(ef_c=4294967296))"),
            "`ef_construction` is 4 to 4294967295, not 4294967296",
        ),
        (
            format!("{v} @hnsw(ef_search=0))"),
            "`ef_search` is 1 to 4294967295, not 0",
        ),
        // An unknown one still says so, its number read first.
        (
            format!("{v} @hnsw(eff=99999999999))"),
            "unknown hnsw parameter `eff`",
        ),
        // `ef 0` was read as 1, and past 32 bits a wasm32 build's low bits.
        (
            "get v near e [1, 0, 0] ef 0".into(),
            "`ef` is 1 to 4294967295, not 0",
        ),
        (
            "get v near e [1, 0, 0] ef 4294967297".into(),
            "`ef` is 1 to 4294967295, not 4294967297",
        ),
        // A place past `usize` names no item of the list.
        (
            "select a, b from d order by 4294967297".into(),
            "the list holds 2",
        ),
    ] {
        let e = refused(&sql);
        assert!(e.contains(says), "{sql}: {e}");
    }
}

#[test]
fn a_number_in_its_range_is_read_as_written() {
    let spec = |sql: &str| match parse_one(sql).unwrap() {
        Statement::CreateCollection { schema, .. } => match &schema.fields[0].index {
            IndexKind::Vector(s) => (schema.fields[0].ty.clone(), *s),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    // Each bound itself.
    let (ty, s) = spec(
        "create collection v (e vector<1000000000> @hnsw(m=32767, ef_construction=4, \
         ef_search=4294967295))",
    );
    assert_eq!(ty, DataType::Vector(1_000_000_000, VecPrec::F32));
    assert_eq!(
        (s.m, s.ef_construction, s.ef_search),
        (32767, 4, 4294967295)
    );
    let (_, s) = spec("create collection v (e vector<3> @hnsw(m=2, ef_c=4294967295, ef=1))");
    assert_eq!((s.m, s.ef_construction, s.ef_search), (2, 4294967295, 1));

    let get = |sql: &str| match parse_one(sql).unwrap() {
        Statement::Select(s) => s,
        other => panic!("{other:?}"),
    };
    let s = get(r#"get d match body "x" near e [1, 0] fuse k 1000000 candidates 10000"#);
    let f = s.fuse.unwrap();
    assert_eq!((f.k, f.candidates), (Some(1_000_000), Some(10_000)));
    let s = get(r#"get d match body "x" near e [1, 0] fuse k 0 candidates 1"#);
    let f = s.fuse.unwrap();
    assert_eq!((f.k, f.candidates), (Some(0), Some(1)));
    let s = get("get v near e [1, 0, 0] ef 4294967295");
    assert_eq!(s.near.unwrap().ef, Some(4_294_967_295));
    let s = get(r#"get d match body "x" rerank e [1, 0] candidates 10000"#);
    assert_eq!(s.rerank.unwrap().candidates, Some(10_000));
    let s = get(r#"get d select snippet(body, 4294967295) match body "x""#);
    assert_eq!(s.marks[0].snippet, Some(4_294_967_295));
}

/// SQL's `vector(n)` and a schema built in code reach no parser's bound:
/// the schema's own check holds them (`IndexKind::check`), where SQL's
/// `vector(4294967297)` was `vector(1)` on a 32-bit build.
#[test]
fn a_schema_is_held_to_the_same_bounds_where_no_parser_read_it() {
    let mut db = Database::new();
    for (sql, says) in [
        (
            "create table t (e vector(4294967297))",
            "`e`: a vector's dimension is 1 to 1000000000",
        ),
        (
            "create table t (e halfvec(1000000001))",
            "`e`: a vector's dimension is 1 to 1000000000",
        ),
        (
            "create table t (s sparsevec(1000000001))",
            "`s`: a vector's dimension is 1 to 1000000000",
        ),
    ] {
        let e = run(&mut db, sql).expect_err(sql).to_string();
        assert!(e.contains(says), "{sql}: {e}");
    }
    run(&mut db, "create table t (e vector(1000000000))").unwrap();

    let field = |m| {
        let spec = VectorIndexSpec {
            m,
            ..VectorIndexSpec::default()
        };
        Field::new("e", DataType::Vector(3, VecPrec::F32)).indexed(IndexKind::Vector(spec))
    };
    for m in [0, 1, 32768] {
        let e = Schema::new("c", vec![field(m)])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("`e`: @hnsw's `m` is 2 to 32767"), "{m}: {e}");
    }
    assert!(Schema::new("c", vec![field(32767)]).is_ok());
    let dim = |d| {
        Schema::new(
            "c",
            vec![Field::new("e", DataType::Vector(d, VecPrec::F32))],
        )
    };
    assert!(dim(0).is_err() && dim(1_000_000_001).is_err() && dim(1_000_000_000).is_ok());
    // An index on it asks its dimension first.
    let indexed = Field::new("e", DataType::Vector(0, VecPrec::F32))
        .indexed(IndexKind::Vector(VectorIndexSpec::default()));
    let e = Schema::new("c", vec![indexed]).err().map(|e| e.to_string());
    assert!(e
        .unwrap_or_default()
        .contains("a vector's dimension is 1 to"));
}

/// `near ... ef 4294967295` -- as wide as a graph's node ids reach -- runs
/// natively and answers the nearest, reserving no more than the walk found
/// (`VectorIndex::search`): `Vec::with_capacity(ef)` asked for 64 GB.
#[test]
fn the_widest_beam_answers() {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection v (n int, e vector<3> @hnsw(l2, m=2, ef_construction=4))",
    )
    .unwrap();
    run(
        &mut db,
        "put v [{n: 1, e: [1, 0, 0]}, {n: 2, e: [0, 1, 0]}, {n: 3, e: [0, 0, 1]}]",
    )
    .unwrap();
    let rows = |db: &mut Database, sql: &str| -> Vec<Value> {
        let r = run(db, sql).unwrap();
        let rows = r.rows().expect("rows");
        let n = rows.columns.iter().position(|c| c == "n").unwrap();
        rows.rows.iter().map(|r| r.values[n].clone()).collect()
    };
    let near = "get v select n near e [0.9, 0.1, 0] ef 4294967295";
    assert_eq!(
        rows(&mut db, &format!("{near} limit 2")),
        [Value::Int(1), Value::Int(2)]
    );
    // Past tombstones the beam widens by their number, saturating.
    run(&mut db, "del v where n = 2").unwrap();
    assert_eq!(
        rows(&mut db, &format!("{near} limit 2")),
        [Value::Int(1), Value::Int(3)]
    );
}

/// SQL's `with (m = 2, ef_construction = 4)`, pgvector's floor, builds an
/// index every reader of its description takes back as it is: the
/// `@hnsw(..)` the schema's FennecQL writes and the declaration its JSON
/// is -- `fennec types --fennecql`, `fennec_abi::schema` and `follow`,
/// `GET /_schema` handed to `/_schema/plan` -- where `ef_construction`
/// from 8 refused what SQL made.
#[test]
fn pgvectors_floor_reads_back_as_it_was_made() {
    let mut db = Database::new();
    run(&mut db, "create table p (e vector(3))").unwrap();
    run(
        &mut db,
        "create index on p using hnsw (e vector_l2_ops) with (m = 2, ef_construction = 4)",
    )
    .unwrap();
    let made = db.collection("p").unwrap().schema.clone();
    let IndexKind::Vector(spec) = made.fields[0].index else {
        panic!("{:?}", made.fields[0].index)
    };
    assert_eq!((spec.m, spec.ef_construction), (2, 4));
    let text = fennec_core::declared::fennecql(&[&*made]);
    assert!(
        text.contains("@hnsw(l2, m=2, ef_construction=4, "),
        "{text}"
    );
    let Statement::CreateCollection { schema, .. } = parse_one(text.trim()).unwrap() else {
        panic!("{text}")
    };
    assert_eq!(schema.fields, made.fields);
    let json = fennec_core::declared::describe(&[&*made]);
    let declared = fennec_core::declared::parse(&json).unwrap();
    assert_eq!(declared.collections[0].fields, made.fields);
}

/// A collection a version before the bounds made out of them -- an `@hnsw`
/// `m` past 32 767, which a node's 16-bit count of links already cut, or a
/// `vector<N>` past 10^9 -- is refused at the open as damage, naming the
/// field and its range: a schema is read off the bytes within the
/// language's ranges (`Schema::decode`), the same ranges this file's
/// parser reads, since a crafted file's `m` of 2^40 killed the open.
#[test]
fn a_field_made_out_of_range_before_is_refused_at_the_open() {
    for (field, says) in [
        (
            {
                let spec = VectorIndexSpec {
                    m: 40_000,
                    ..VectorIndexSpec::default()
                };
                Field::new("e", DataType::Vector(3, VecPrec::F32)).indexed(IndexKind::Vector(spec))
            },
            "`e`: @hnsw's `m` is 2 to 32767, not 40000",
        ),
        (
            Field::new("e", DataType::Vector(1_000_000_001, VecPrec::F32)),
            "`e`: a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
    ] {
        // Made as a version before made it: `Schema::new` refuses it now,
        // and a create takes the schema it is handed.
        let mut schema = Schema::new("b", vec![Field::new("n", DataType::Int)]).unwrap();
        schema.fields.push(field);
        let mut db = Database::new();
        db.execute(&Statement::CreateCollection {
            schema,
            if_not_exists: false,
        })
        .unwrap();
        match Database::new().load(&db.snapshot()) {
            Err(Error::Corrupt(m)) => assert_eq!(m, says),
            other => panic!("{says}: {other:?}"),
        }
    }
}
