//! A number past an `int`'s range is refused naming its field, never
//! clamped to the nearest int: `as` saturates, and `{"n": 1e300}` was filed
//! as 9223372036854775807. Every way a number reaches an int: a document's
//! JSON, a statement's literal, a parameter, a `set`'s arithmetic, a list's
//! element, a named statement's argument and its default -- and a read
//! through a hash index answers what the scan does.

use fennec_core::prelude::*;

/// Past either end: as floats, and as JSON integers past 64 bits, which
/// read as the floats they round to -- `-9223372036854775809` is -2^63.
const PAST: [&str; 4] = [
    "1e300",
    "-1e300",
    "9223372036854775808",
    "-9223372036854775809",
];

/// The ends, written as integers, and the whole floats nearest them.
const EDGES: [(&str, i64); 4] = [
    ("9223372036854775807", i64::MAX),
    ("-9223372036854775808", i64::MIN),
    ("9223372036854774784.0", 9_223_372_036_854_774_784),
    ("-9.223372036854774784e18", -9_223_372_036_854_774_784),
];

const RANGE: &str = "an int is from -9223372036854775808 to 9223372036854775807";

fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text)? {
        last = db.execute(&s)?;
    }
    Ok(last)
}

/// `text` with its parameters read from JSON, as a server and the browser
/// module read them.
fn with(db: &mut Database, text: &str, params: &str) -> Result<Response> {
    let s = fennec_ql::parse_one(text)?;
    db.execute_with(&s, &fennec_core::json::parse_params(params)?)
}

fn db() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, "create collection t (n int, xs [int], f float)").unwrap();
    db
}

fn column(db: &Database, field: &str) -> Vec<Value> {
    let s = fennec_ql::parse_one(&format!("get t select {field} order id")).unwrap();
    let r = db.query(&s, &[]).unwrap();
    r.rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.values[0].clone())
        .collect()
}

/// The refusal names the field, or the argument, and the range.
#[track_caller]
fn past(r: Result<Response>, name: &str) {
    match r {
        Err(Error::Type(m) | Error::Query(m)) if m.contains(name) && m.contains(RANGE) => {}
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn a_document_past_an_ints_range_is_refused_naming_the_field() {
    let mut db = db();
    for n in PAST {
        past(
            with(&mut db, "put t $1", &format!(r#"[{{"n": {n}}}]"#)),
            "field `n`",
        );
        // A page of documents, the REST body's shape.
        let page = format!(r#"[[{{"n": 1}}, {{"n": {n}}}]]"#);
        past(with(&mut db, "put t $1", &page), "field `n`");
    }
    assert_eq!(column(&db, "n"), []);
    for (n, want) in EDGES {
        with(&mut db, "put t $1", &format!(r#"[{{"n": {n}}}]"#)).unwrap();
        assert_eq!(column(&db, "n").last(), Some(&Value::Int(want)), "{n}");
    }
}

#[test]
fn a_parameter_or_a_literal_past_it_is_refused() {
    let mut db = db();
    for n in PAST {
        past(
            with(&mut db, "put t {n: $1}", &format!("[{n}]")),
            "field `n`",
        );
    }
    // FennecQL refuses an integer literal past 64 bits as it reads it.
    for n in ["1e300", "-1e300"] {
        past(run(&mut db, &format!("put t {{n: {n}}}")), "field `n`");
    }
    for n in ["9223372036854775808", "-9223372036854775809"] {
        let e = run(&mut db, &format!("put t {{n: {n}}}")).unwrap_err();
        assert!(e.to_string().contains("invalid integer"), "{e}");
    }
    assert_eq!(column(&db, "n"), []);
    run(&mut db, "put t {n: 1}").unwrap();
    for n in PAST {
        past(
            with(&mut db, "set t {n: $1} where id = 1", &format!("[{n}]")),
            "field `n`",
        );
    }
    assert_eq!(column(&db, "n"), [Value::Int(1)]);
    for (n, want) in EDGES {
        with(&mut db, "set t {n: $1} where id = 1", &format!("[{n}]")).unwrap();
        assert_eq!(column(&db, "n"), [Value::Int(want)], "{n}");
        run(&mut db, &format!("set t {{n: {n}}} where id = 1")).unwrap();
        assert_eq!(column(&db, "n"), [Value::Int(want)], "{n}");
    }
}

/// An int and a float make a float, which an int field takes when it is
/// whole -- and inside the range.
#[test]
fn arithmetic_past_it_is_refused_and_the_row_kept() {
    let mut db = db();
    run(&mut db, "put t {n: 4611686018427387904}").unwrap();
    for set in ["n * 2.0", "n * 4.0", "0 - n * 2.0", "n - 1e300"] {
        past(run(&mut db, &format!("set t {{n: {set}}}")), "field `n`");
    }
    assert_eq!(column(&db, "n"), [Value::Int(1 << 62)]);
    run(&mut db, "set t {n: n * 1.0 + 4611686018427387904.0 - 1024}").unwrap();
    assert_eq!(column(&db, "n"), [Value::Int(9_223_372_036_854_774_784)]);
}

#[test]
fn a_lists_element_past_it_is_refused() {
    let mut db = db();
    for xs in ["[1, 1e20]", "[-1e20]"] {
        past(run(&mut db, &format!("put t {{xs: {xs}}}")), "field `xs`");
        past(
            with(&mut db, "put t $1", &format!(r#"[{{"xs": {xs}}}]"#)),
            "field `xs`",
        );
        past(
            with(&mut db, "put t {xs: $1}", &format!("[{xs}]")),
            "field `xs`",
        );
    }
    assert_eq!(column(&db, "xs"), []);
}

#[test]
fn a_calls_argument_and_a_default_past_it_are_refused() {
    let mut db = db();
    run(
        &mut db,
        "create statement put_n($n int) public as insert t {n: $n};
         create statement put_far($n int = 1e300) public as insert t {n: $n}",
    )
    .unwrap();
    for n in ["1e300", "-1e300"] {
        past(
            run(&mut db, &format!("call put_n {{n: {n}}}")),
            "the argument `n`",
        );
        past(
            run(&mut db, &format!("call put_n({n})")),
            "the argument `n`",
        );
    }
    for n in PAST {
        let args = format!(r#"[{{"n": {n}}}]"#);
        past(with(&mut db, "call put_n $1", &args), "the argument `n`");
    }
    past(run(&mut db, "call put_far"), "the argument `n`");
    assert_eq!(column(&db, "n"), []);
    for (n, want) in EDGES {
        with(&mut db, "call put_n $1", &format!(r#"[{{"n": {n}}}]"#)).unwrap();
        assert_eq!(column(&db, "n").last(), Some(&Value::Int(want)), "{n}");
    }
}

/// `n = 1e300` over a hash index looked in the bucket of the int it was
/// clamped to, and found the row holding 9223372036854775807, which the
/// scan does not: the index changed the answer.
#[test]
fn an_index_answers_past_the_range_as_the_scan_does() {
    let mut db = db();
    run(&mut db, "create collection h (n int @hash, m int)").unwrap();
    run(
        &mut db,
        "put h [{n: 9223372036854775807, m: 9223372036854775807},
                {n: -9223372036854775808, m: -9223372036854775808}]",
    )
    .unwrap();
    for (n, rows) in [("1e300", 0), ("-1e300", 0), ("9223372036854774784.0", 0)] {
        for field in ["n", "m"] {
            let text = format!("get h where {field} = {n}");
            let r = run(&mut db, &text).unwrap();
            assert_eq!(r.rows().unwrap().rows.len(), rows, "{text}");
        }
    }
}
