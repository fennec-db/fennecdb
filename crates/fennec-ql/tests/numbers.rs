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
        // A list's, at any depth: each dimension is read where it is
        // written, in `create` and `alter` alike.
        (
            "create collection v (e [vector<0>])".into(),
            "a vector's dimension is 1 to 1000000000, not 0",
        ),
        (
            "create collection v (e [[sparse<1000000001>]])".into(),
            "a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
        (
            "alter collection v add field e [vector<4294967297, f16>]".into(),
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
/// `vector(4294967297)` was `vector(1)` on a 32-bit build. A list of them
/// too, at any depth, by the test a file's schema is read with
/// (`schema::out_of_range`): the check held a field's own vector alone, so
/// `vector(1000000001)[]` was taken, written, and its file refused as
/// damage at its next open, by every replica and every page replicating it.
#[test]
fn a_schema_is_held_to_the_same_bounds_where_no_parser_read_it() {
    let mut db = Database::new();
    for (sql, says) in [
        (
            "create table t (e vector(4294967297))",
            "`e`: a vector's dimension is 1 to 1000000000, not 4294967297",
        ),
        (
            "create table t (e halfvec(1000000001))",
            "`e`: a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
        (
            "create table t (s sparsevec(1000000001))",
            "`s`: a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
        // SQL's arrays of them: `[]`, `[3]` and `array`.
        (
            "create table t (e vector(1000000001)[])",
            "`e`: a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
        (
            "create table t (e halfvec(4294967297)[3])",
            "`e`: a vector's dimension is 1 to 1000000000, not 4294967297",
        ),
        (
            "create table t (s sparsevec(2000000000) array)",
            "`s`: a vector's dimension is 1 to 1000000000, not 2000000000",
        ),
    ] {
        let e = run(&mut db, sql).expect_err(sql).to_string();
        assert!(e.contains(says), "{sql}: {e}");
    }
    run(&mut db, "create table t (e vector(1000000000))").unwrap();
    run(&mut db, "create table u (e vector(1000000000)[])").unwrap();

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
        let says = format!("`e`: @hnsw's `m` is 2 to 32767, not {m}");
        assert!(e.contains(&says), "{m}: {e}");
    }
    assert!(Schema::new("c", vec![field(32767)]).is_ok());

    // A list's vectors in code, at any depth a type may nest.
    let listed = |mut ty: DataType, lists: usize| {
        for _ in 0..lists {
            ty = DataType::List(Box::new(ty));
        }
        Schema::new("c", vec![Field::new("e", ty)])
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    for (inner, n) in [
        (DataType::Vector(0, VecPrec::F32), 0usize),
        (DataType::Vector(1_000_000_001, VecPrec::F16), 1_000_000_001),
        (DataType::Sparse(0), 0),
        (DataType::Sparse(1 << 40), 1 << 40),
    ] {
        for lists in [1, 2, 64] {
            let e = listed(inner.clone(), lists).expect_err("refused");
            let says = format!("`e`: a vector's dimension is 1 to 1000000000, not {n}");
            assert!(e.contains(&says), "{lists}: {e}");
        }
    }
    assert!(listed(DataType::Vector(1_000_000_000, VecPrec::F32), 64).is_ok());
    assert!(listed(DataType::Sparse(1), 1).is_ok());

    // A beam built in code, past what a 32-bit reader of a file's schema
    // holds whole (`codec::get_usize`): taken natively, a page replicating
    // the file refused its schema. Every parser holds it there already.
    for (spec, says) in [
        (
            VectorIndexSpec {
                ef_construction: 1 << 32,
                ..VectorIndexSpec::default()
            },
            "`e`: @hnsw's `ef_construction` is at most 4294967295, not 4294967296",
        ),
        (
            VectorIndexSpec {
                ef_search: 1 << 32,
                ..VectorIndexSpec::default()
            },
            "`e`: @hnsw's `ef_search` is at most 4294967295, not 4294967296",
        ),
    ] {
        let f = Field::new("e", DataType::Vector(3, VecPrec::F32)).indexed(IndexKind::Vector(spec));
        let e = Schema::new("c", vec![f]).expect_err(says).to_string();
        assert!(e.contains(says), "{e}");
    }
    let widest = VectorIndexSpec {
        ef_construction: u32::MAX as usize,
        ef_search: u32::MAX as usize,
        ..VectorIndexSpec::default()
    };
    let f = Field::new("e", DataType::Vector(3, VecPrec::F32)).indexed(IndexKind::Vector(widest));
    assert!(Schema::new("c", vec![f]).is_ok());
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
/// `m` past 32 767, which a node's 16-bit count of links already cut, a
/// `vector<N>` past 10^9, or a list of them, which SQL's
/// `vector(1000000001)[]` made until the check held a list's vectors -- is
/// refused at the open as damage, naming the field and its range: a schema
/// is read off the bytes within the language's ranges (`Schema::decode`),
/// the same ranges this file's parser reads, since a crafted file's `m` of
/// 2^40 killed the open.
#[test]
fn a_field_made_out_of_range_before_is_refused_at_the_open() {
    for (field, says) in [
        (
            Field::new(
                "e",
                DataType::List(Box::new(DataType::Vector(1_000_000_001, VecPrec::F32))),
            ),
            "`e`: a vector's dimension is 1 to 1000000000, not 1000000001",
        ),
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

/// Whatever a write takes, an open reads back as it was made: every way a
/// schema is made -- `Schema::new` in code, FennecQL's `create collection`
/// and `alter collection`, SQL's `create table` and its arrays, a declared
/// schema -- at the edges of a dimension's range and an `@hnsw`'s `m`, a
/// field's own vector and a list's at every depth a type nests. A taken one
/// is read back from the database's image as it was made, a refused one
/// refused naming the range, and the field where no parser read the
/// number. In code the write and the read take exactly the same fields,
/// both asking `schema::out_of_range`: with a test each, the check held a
/// field's own vector alone, and `[vector<0>]` was taken and written, and
/// its file refused as damage at the next open.
#[test]
fn whatever_a_write_takes_an_open_reads_back() {
    // `c`'s fields as an open makes them of `db`'s image.
    let reopened = |db: &Database, c: &str| -> Result<Vec<Field>> {
        let mut opened = Database::new();
        opened.load(&db.snapshot())?;
        Ok(opened.collection(c)?.schema.fields.clone())
    };
    let made = |db: &Database, c: &str| db.collection(c).unwrap().schema.fields.clone();
    let dims = [
        0,
        1,
        3,
        1_000_000_000,
        1_000_000_001,
        4_294_967_297,
        1 << 40,
    ];
    let within = |d: usize| (1..=1_000_000_000).contains(&d);

    // In code: each field taken by the write exactly where the read takes
    // it. One the write refuses is made as a crafted file or a version
    // before holds it, for a create takes the schema it is handed.
    let n = || Field::new("n", DataType::Int);
    let mut fields = Vec::new();
    for d in dims {
        for inner in [
            DataType::Vector(d, VecPrec::F32),
            DataType::Vector(d, VecPrec::F16),
            DataType::Sparse(d),
        ] {
            let mut ty = inner;
            for lists in 0..=65 {
                if [0, 1, 2, 64, 65].contains(&lists) {
                    fields.push(Field::new("e", ty.clone()));
                }
                ty = DataType::List(Box::new(ty));
            }
        }
        fields.push(Field::new("e", DataType::Sparse(d)).indexed(IndexKind::Inverted));
        for m in [0, 1, 2, 16, 32_767, 32_768, 1 << 40] {
            let spec = VectorIndexSpec {
                m,
                ..VectorIndexSpec::default()
            };
            let ty = DataType::Vector(d, VecPrec::F32);
            fields.push(Field::new("e", ty).indexed(IndexKind::Vector(spec)));
        }
    }
    for field in fields {
        let written = Schema::new("c", vec![n(), field.clone()]);
        let schema = match &written {
            Ok(s) => s.clone(),
            Err(_) => {
                let mut s = Schema::new("c", vec![n()]).unwrap();
                s.fields.push(field.clone());
                s
            }
        };
        let mut db = Database::new();
        db.execute(&Statement::CreateCollection {
            schema,
            if_not_exists: false,
        })
        .unwrap();
        match (written, reopened(&db, "c")) {
            (Ok(w), Ok(read)) => assert_eq!(read, w.fields),
            (Err(Error::Type(_)), Err(Error::Corrupt(_))) => {}
            (w, r) => panic!(
                "{} {:?}: written {:?}, read {:?}",
                field.ty.name(),
                field.index,
                w.err(),
                r.err()
            ),
        }
    }

    // Through each statement that makes a schema, the last of each run the
    // one that matters, and through a declared schema.
    for d in dims {
        let range = format!("a vector's dimension is 1 to 1000000000, not {d}");
        for (texts, sql) in [
            (vec![format!("create table t (e vector({d})[])")], true),
            (vec![format!("create table t (e halfvec({d})[3])")], true),
            (
                vec![format!("create table t (e sparsevec({d}) array)")],
                true,
            ),
            (
                vec![format!("create table t (e vector({d}) array[2])")],
                true,
            ),
            (
                vec![format!("create collection t (e [[vector<{d}, f16>]])")],
                false,
            ),
            (
                vec![
                    "create collection t (n int)".into(),
                    format!("alter collection t add field e [sparse<{d}>]"),
                ],
                false,
            ),
        ] {
            let mut db = Database::new();
            let (last, before) = texts.split_last().unwrap();
            for text in before {
                run(&mut db, text).unwrap();
            }
            match run(&mut db, last) {
                Ok(_) => {
                    assert!(within(d), "{last}");
                    assert_eq!(reopened(&db, "t").unwrap(), made(&db, "t"), "{last}");
                }
                // SQL's `vector(0)` has no dimension at all.
                Err(e) if sql && d == 0 => assert!(e.to_string().contains("has its dimension")),
                Err(e) => {
                    let e = e.to_string();
                    assert!(!within(d), "{last}: {e}");
                    let says = match sql {
                        true => format!("`e`: {range}"),
                        false => range.clone(),
                    };
                    assert!(e.contains(&says), "{last}: {e}");
                }
            }
        }
        let json = format!(
            r#"{{"format":1,"collections":[{{"name":"t","fields":[{{"name":"e","type":"[[vector<{d}>]]"}}]}}]}}"#
        );
        match fennec_core::declared::parse(&json) {
            Ok(declared) => {
                assert!(within(d), "{json}");
                let schema = declared.collections[0].clone();
                let mut db = Database::new();
                db.execute(&Statement::CreateCollection {
                    schema,
                    if_not_exists: false,
                })
                .unwrap();
                assert_eq!(reopened(&db, "t").unwrap(), made(&db, "t"));
            }
            Err(e) => {
                let e = e.to_string();
                assert!(!within(d), "{json}: {e}");
                let says = "t.e has a vector's dimension that is not 1 to 1000000000";
                assert!(e.ends_with(says), "{json}: {e}");
            }
        }
    }
}
