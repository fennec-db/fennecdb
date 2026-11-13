//! An `[int]`, a `[float]` or a `[timestamp]` is handed its list's numbers
//! as written, or the statement is refused naming the field, the element
//! and the range -- never an `f32`'s nearest, as the quick read of a list
//! of numbers makes it, nor a number clamped to an int's end. A list and a
//! scalar alike: a statement's literal, a parameter, a document, a `set`, an
//! upsert, a comparison.
//!
//! A list of numbers takes a vector of whole `f32`s short of 2^24 as the
//! integers written, which the quick read never makes of another list, so
//! such a vector is not read again, beside an embedding or alone.

use fennec_core::json;
use fennec_core::prelude::*;

/// `sql` run with `params` (JSON) as `/query` runs it: the text parsed for
/// the database, the parameters read the quick way and again as written
/// where `Database::exactly` names them and their values do not settle it.
fn run(db: &mut Database, sql: &str, params: &str) -> Result<Response> {
    let stmt = fennec_ql::parse_one_for(db, sql)?;
    let mut values = json::parse_params(params)?;
    let mut need = db.exactly(&stmt);
    need.settle(&values);
    for (i, v) in json::exact_params(params, None, &need.params)? {
        values[i] = v;
    }
    match stmt.is_read_only() {
        true => db.query(&stmt, &values),
        false => db.execute_with(&stmt, &values),
    }
}

fn db() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection t (k text @unique, xs [int], fs [float], ts [timestamp], \
         e vector<3>)",
        "",
    )
    .unwrap();
    db
}

fn column(db: &mut Database, field: &str) -> Vec<Value> {
    match run(db, &format!("get t select {field} order id"), "").unwrap() {
        Response::Rows(rs) => rs.rows.iter().map(|r| r.values[0].clone()).collect(),
        other => panic!("{other:?}"),
    }
}

fn ids(db: &mut Database, sql: &str, params: &str) -> Vec<u64> {
    match run(db, sql, params).unwrap_or_else(|e| panic!("{sql}: {e}")) {
        Response::Rows(rs) => rs.rows.iter().map(|r| r.id).collect(),
        other => panic!("{other:?}"),
    }
}

fn ints(xs: &[i64]) -> Value {
    Value::List(xs.iter().map(|&i| Value::Int(i)).collect())
}

fn floats(xs: &[f64]) -> Value {
    Value::List(xs.iter().map(|&f| Value::Float(f)).collect())
}

/// 2^24 + 1, the first int an `f32` does not hold; 2^53 + 1, the first an
/// `f64` does not; the ends, which an `f32` reads as ±2^63.
const WIDE: &str = "[16777217, 9007199254740993, -9223372036854775808, 9223372036854775807]";
const WIDE_INTS: [i64; 4] = [16_777_217, 9_007_199_254_740_993, i64::MIN, i64::MAX];

/// The refusal names the field, the element, and what it was refused for.
#[track_caller]
fn refused(r: Result<Response>, field: &str, at: usize, why: &str) {
    let named = format!("field `{field}`, element {at}: {why}");
    match r {
        Err(Error::Type(m)) if m == named => {}
        other => panic!("{named}: {other:?}"),
    }
}

const RANGE: &str = "an int is from -9223372036854775808 to 9223372036854775807";

#[test]
fn an_int_list_keeps_its_ints() {
    let mut db = db();
    run(&mut db, &format!("put t {{xs: {WIDE}}}"), "").unwrap();
    run(&mut db, "put t {xs: $1}", &format!("[{WIDE}]")).unwrap();
    run(&mut db, "put t $1", &format!(r#"[{{"xs": {WIDE}}}]"#)).unwrap();
    // A page of documents, the REST body's shape.
    run(
        &mut db,
        "put t $1",
        &format!(r#"[[{{"xs": [1]}}, {{"xs": {WIDE}}}]]"#),
    )
    .unwrap();
    let want = ints(&WIDE_INTS);
    assert_eq!(
        column(&mut db, "xs"),
        [want.clone(), want.clone(), want.clone(), ints(&[1]), want]
    );
}

#[test]
fn a_float_list_keeps_its_doubles() {
    let mut db = db();
    let fs = "[0.1, 19.99, 16777217, 9007199254740993, 1.00000001, -0.0]";
    run(&mut db, &format!("put t {{fs: {fs}}}"), "").unwrap();
    run(&mut db, "put t {fs: $1}", &format!("[{fs}]")).unwrap();
    run(&mut db, "put t $1", &format!(r#"[{{"fs": {fs}}}]"#)).unwrap();
    // An int a double does not hold is the double nearest it, as a `float`
    // field takes one.
    let want = floats(&[
        0.1,
        19.99,
        16_777_217.0,
        9_007_199_254_740_992.0,
        1.00000001,
        -0.0,
    ]);
    let got = column(&mut db, "fs");
    assert_eq!(got, [want.clone(), want.clone(), want]);
    let bits = |v: &Value| match v {
        Value::List(l) => l
            .iter()
            .map(|x| x.as_f64().unwrap().to_bits())
            .collect::<Vec<_>>(),
        _ => panic!(),
    };
    assert!(got.iter().all(|v| bits(v)[5] == (-0.0f64).to_bits()));
}

#[test]
fn a_timestamp_list_takes_its_milliseconds() {
    let mut db = db();
    run(&mut db, "put t {ts: [1700000000123, 1]}", "").unwrap();
    run(&mut db, "put t {ts: $1}", "[[1700000000123, 1]]").unwrap();
    let want = Value::List(vec![
        Value::Timestamp(1_700_000_000_123),
        Value::Timestamp(1),
    ]);
    assert_eq!(column(&mut db, "ts"), [want.clone(), want]);
}

#[test]
fn a_set_keeps_them() {
    let mut db = db();
    run(&mut db, "put t {k: 'a', xs: [1], fs: [1]}", "").unwrap();
    run(
        &mut db,
        &format!("set t {{xs: {WIDE}, fs: [0.1]}} where k = 'a'"),
        "",
    )
    .unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&WIDE_INTS)]);
    assert_eq!(column(&mut db, "fs"), [floats(&[0.1])]);
    run(
        &mut db,
        "set t {xs: $1, fs: $2} where k = 'a'",
        "[[16777217], [0.2]]",
    )
    .unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&[16_777_217])]);
    assert_eq!(column(&mut db, "fs"), [floats(&[0.2])]);
}

#[test]
fn an_upsert_keeps_them() {
    let mut db = db();
    let up = "put t {k: 'a', xs: [16777217]} if absent else set {xs: [16777219]}";
    run(&mut db, up, "").unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&[16_777_217])]);
    run(&mut db, up, "").unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&[16_777_219])]);
    let up = "put t {k: 'a', xs: $1} if absent else set {xs: $2}";
    run(&mut db, up, "[[1], [16777221]]").unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&[16_777_221])]);
    let up = "put t {k: 'a', xs: $1} if absent else set {xs: new.xs}";
    run(&mut db, up, "[[9007199254740993]]").unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&[9_007_199_254_740_993])]);
}

/// A list field compared with a list finds the rows holding it: read the
/// quick way, the list was a vector, which no list equals.
#[test]
fn a_comparison_finds_the_list_written() {
    let mut db = db();
    run(&mut db, "put t {xs: [16777217], fs: [0.1]}", "").unwrap();
    run(
        &mut db,
        "put t {xs: [16777216], fs: [0.10000000149011612]}",
        "",
    )
    .unwrap();
    run(&mut db, "put t {xs: [1, 2]}", "").unwrap();
    assert_eq!(ids(&mut db, "get t where xs = [16777217]", ""), [1]);
    assert_eq!(ids(&mut db, "get t where xs = $1", "[[16777217]]"), [1]);
    assert_eq!(ids(&mut db, "get t where fs = [0.1]", ""), [1]);
    assert_eq!(ids(&mut db, "get t where xs in [[16777217], [3]]", ""), [1]);
    assert_eq!(ids(&mut db, "get t where xs = [1, 2]", ""), [3]);
    assert_eq!(ids(&mut db, "get t where xs = $1", "[[1, 2]]"), [3]);
}

/// Each refused naming the field, the element (from 0), and why: a float
/// that is not a whole number, an int past the range -- never the nearest
/// `f32`, nor clamped to an end. An integer past 64 bits in FennecQL is
/// refused as it is read, as a scalar's is.
#[test]
fn an_element_no_int_is_is_refused_naming_it() {
    let mut db = db();
    run(&mut db, "put t {k: 'a', xs: [7]}", "").unwrap();
    let float = "expected int, found float";
    for (xs, at, why) in [
        ("[1, 16777217.5]", 1, float),
        ("[1.00000001]", 0, float),
        ("[0, 2.5]", 1, float),
        ("[1, 1e20]", 1, RANGE),
        ("[-1e300]", 0, RANGE),
        ("[1, 2, 9223372036854775808]", 2, RANGE),
        ("[-9223372036854775809]", 0, RANGE),
    ] {
        if !xs.contains("922337") {
            refused(
                run(&mut db, &format!("put t {{xs: {xs}}}"), ""),
                "xs",
                at,
                why,
            );
        }
        refused(
            run(&mut db, "put t {xs: $1}", &format!("[{xs}]")),
            "xs",
            at,
            why,
        );
        let doc = format!(r#"[{{"xs": {xs}}}]"#);
        refused(run(&mut db, "put t $1", &doc), "xs", at, why);
        refused(
            run(&mut db, "set t {xs: $1}", &format!("[{xs}]")),
            "xs",
            at,
            why,
        );
    }
    for xs in ["[1, 2, 9223372036854775808]", "[-9223372036854775809]"] {
        let e = run(&mut db, &format!("put t {{xs: {xs}}}"), "").unwrap_err();
        assert!(e.to_string().contains("invalid integer"), "{e}");
    }
    assert_eq!(column(&mut db, "xs"), [ints(&[7])]);
}

/// A list whose `f32`s would pass for the whole numbers written, one of
/// them another number, is read as written by the quick read too -- a
/// list, not a vector -- so a vector of whole `f32`s is always the
/// integers written. A list of whole numbers written as floats is a vector
/// still, its `f32`s the numbers.
#[test]
fn a_list_hiding_a_rounding_is_read_as_written() {
    let lit = |text: &str| match fennec_ql::parse_one(&format!("put t {{fs: {text}}}")).unwrap() {
        Statement::Put { docs, .. } => match &docs[0][0].1 {
            Expr::Lit(v) => v.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    let hidden = Value::List(vec![Value::Float(1.00000001), Value::Int(2)]);
    assert_eq!(lit("[1.00000001, 2]"), hidden);
    assert_eq!(json::parse_params("[[1.00000001, 2]]").unwrap(), [hidden]);
    assert_eq!(
        json::parse_params("[[1e-50]]").unwrap(),
        [Value::List(vec![Value::Float(1e-50)])]
    );
    for whole in ["[1.0, 2]", "[1e3, -0.0]", "[0.0e-400]"] {
        assert!(matches!(lit(whole), Value::Vector(_)), "{whole}");
        let p = json::parse_params(&format!("[{whole}]")).unwrap();
        assert!(matches!(p[0], Value::Vector(_)), "{whole}");
    }
    // A number past a float's range was 0 here, a whole `f32`; it is refused.
    let e = fennec_ql::parse_one("put t {fs: [1e-400]}").unwrap_err();
    assert!(e.to_string().contains("past a float's range"), "{e}");
    let e = json::parse_params("[[1e-400]]").unwrap_err();
    assert!(e.to_string().contains("past a float's range"), "{e}");
    let mut db = db();
    run(
        &mut db,
        "put t {fs: [1.00000001, 2], e: [1.00000001, 0, 2]}",
        "",
    )
    .unwrap();
    run(&mut db, "put t {fs: $1, e: $1}", "[[1.00000001, 0, 2]]").unwrap();
    assert_eq!(
        column(&mut db, "fs"),
        [floats(&[1.00000001, 2.0]), floats(&[1.00000001, 0.0, 2.0])]
    );
    assert_eq!(
        column(&mut db, "e"),
        [
            Value::Vector(vec![1.0, 0.0, 2.0]),
            Value::Vector(vec![1.0, 0.0, 2.0])
        ]
    );
}

/// What is read again: a literal or a parameter a list of numbers is given
/// only where its vector is no whole `f32`s short of 2^24 -- an `[int]`
/// beside an embedding reads nothing again.
#[test]
fn a_vector_of_whole_numbers_is_not_read_again() {
    let db = db();
    let need = |text: &str| db.exactly(&fennec_ql::parse_one(text).unwrap());
    assert!(!need("put t {xs: [1, 2, -16777215]}").is_needed());
    assert!(!need("put t {e: [0.1, 0.2, 0.3], xs: [1, 2]}").is_needed());
    assert!(!need("set t {fs: [1, 2]} where k = 'a'").is_needed());
    assert!(need("put t {xs: [16777216]}").text);
    assert!(need("put t {fs: [0.5]}").text);
    // Compared, a list is read as written whatever it holds: a vector
    // equals no list.
    assert!(need("get t where xs = [1]").text);
    let put = fennec_ql::parse_one("put t {e: $1, xs: $2, fs: $3}").unwrap();
    let mut n = db.exactly(&put);
    assert_eq!(
        (n.params.to_vec(), n.lists.to_vec()),
        (vec![1, 2], vec![1, 2])
    );
    let quick = json::parse_params("[[0.1, 0.2, 0.3], [1, 2], [0.5]]").unwrap();
    n.settle(&quick);
    assert_eq!(*n.params, [2]);
    let mut n = db.exactly(&put);
    n.settle(&json::parse_params("[[0.1, 0.2, 0.3], [16777217], [1]]").unwrap());
    assert_eq!(*n.params, [1]);
}

/// Read with no database at hand, a list a list of numbers cannot take as
/// written is refused as it runs, as a json field's is; one of whole
/// numbers is taken.
#[test]
fn a_quick_read_is_refused_where_it_rounded() {
    let mut db = db();
    for text in ["put t {xs: [16777217]}", "put t {fs: [0.1]}"] {
        let e = db
            .execute(&fennec_ql::parse_one(text).unwrap())
            .unwrap_err();
        assert!(e.to_string().contains("as written"), "{e}");
    }
    let put = fennec_ql::parse_one("put t {xs: $1}").unwrap();
    let quick = json::parse_params("[[16777217]]").unwrap();
    assert!(db.execute_with(&put, &quick).is_err());
    db.execute(&fennec_ql::parse_one("put t {xs: [1, -2]}").unwrap())
        .unwrap();
    db.execute_with(&put, &json::parse_params("[[3]]").unwrap())
        .unwrap();
    assert_eq!(column(&mut db, "xs"), [ints(&[1, -2]), ints(&[3])]);
}

/// A vector made a list is its `f32`s: a `[float]` takes them, an `[int]`
/// the whole ones, and 2^63 -- where `i64::MAX` would round to as an `f32`
/// -- is past every int, where it was taken as `i64::MAX`.
#[test]
fn a_vector_made_a_list_is_its_f32s() {
    let list = |t: DataType| DataType::List(Box::new(t));
    assert_eq!(
        Value::Vector(vec![1.0, 0.1])
            .coerce(&list(DataType::Float))
            .unwrap(),
        floats(&[1.0, 0.1f32 as f64])
    );
    for (v, at, why) in [
        (vec![1.0, 2.5], 1, "expected int, found float"),
        (vec![9.223372e18], 0, RANGE),
        (vec![-9.223372e18], 0, RANGE),
    ] {
        match Value::Vector(v).coerce(&list(DataType::Int)) {
            Err(Error::Type(m)) if m == format!("element {at}: {why}") => {}
            other => panic!("{other:?}"),
        }
    }
}

/// Whether a field keeps a list of numbers is its collection's, kept with
/// its schema (`Database::exactly_needs`): a collection gaining an `[int]`
/// by `alter collection` after statements ran asks of a text parsed
/// before, as of one parsed after, and refuses its quick read; one losing
/// it, or a block putting the field back out, asks nothing again.
#[test]
fn a_list_of_numbers_added_or_dropped_is_asked_of_at_once() {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, "create collection p (n int, e vector<3>)", "").unwrap();
    let vector = fennec_ql::parse_one("put p {n: 1, e: $1}").unwrap();
    let list = fennec_ql::parse_one("put p {xs: $1}").unwrap();
    let quick = json::parse_params("[[16777217]]").unwrap();
    let asked = |db: &Database, stmt: &Statement| db.exactly_needs(stmt).map(|n| n.lists.to_vec());
    let embedding = json::parse_params("[[0.1, 0.2, 0.3]]").unwrap();
    db.execute_with(&vector, &embedding).unwrap();
    assert_eq!(asked(&db, &vector), None);
    assert_eq!(asked(&db, &list), None);
    // Gained, in a block put back: asked of in it, and not after.
    db.begin().unwrap();
    run(&mut db, "alter collection p add field xs [int]", "").unwrap();
    assert_eq!(asked(&db, &list), Some(vec![0]));
    db.rollback();
    assert_eq!(asked(&db, &list), None);
    // Gained for good: the text parsed before asks, its quick read refused.
    run(&mut db, "alter collection p add field xs [int]", "").unwrap();
    assert_eq!(asked(&db, &vector), None);
    assert_eq!(asked(&db, &list), Some(vec![0]));
    let e = db.execute_with(&list, &quick).unwrap_err();
    assert!(e.to_string().contains("as written"), "{e}");
    db.execute_with(&list, &[ints(&[16_777_217])]).unwrap();
    db.execute_with(&vector, &embedding).unwrap();
    let xs: Vec<Value> = match run(&mut db, "get p select xs order id", "").unwrap() {
        Response::Rows(rs) => rs.rows.iter().map(|r| r.values[0].clone()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(xs, [Value::Null, ints(&[16_777_217]), Value::Null]);
    // Lost: nothing asked of either again.
    run(&mut db, "alter collection p drop field xs", "").unwrap();
    assert_eq!(asked(&db, &list), None);
    assert_eq!(asked(&db, &vector), None);
    db.execute_with(&vector, &embedding).unwrap();
}
