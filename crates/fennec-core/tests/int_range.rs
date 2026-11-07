//! A number past an `int`'s range is refused naming its field, never
//! clamped to the nearest int: `as` saturates, and `{"n": 1e300}` was filed
//! as 9223372036854775807. Every way a number reaches an int: a document's
//! JSON, a statement's literal, a parameter, a `set`'s arithmetic, a list's
//! element, a named statement's argument and its default -- and a read
//! through an index answers what the scan does. An `[int]` or a `[float]`
//! reads its list as written, never as a vector's `f32`s, and a `lookup`
//! by the float -2^63 finds `i64::MIN`.

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

/// `text` read as a server reads it: a list a field keeps as written read
/// so (`fennec_ql::parse_for`).
fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse_for(db, text)? {
        last = db.execute(&s)?;
    }
    Ok(last)
}

/// `text` with its parameters read from JSON, as a server and the browser
/// module read them: again as written where `Database::exactly` names one.
fn with(db: &mut Database, text: &str, params: &str) -> Result<Response> {
    let s = fennec_ql::parse_one_for(db, text)?;
    let params = match db.exactly(&s).params.is_empty() {
        true => fennec_core::json::parse_params(params)?,
        false => fennec_core::json::parse_params_exact(params)?,
    };
    db.execute_with(&s, &params)
}

fn db() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection t (n int, xs [int], f float, fs [float])",
    )
    .unwrap();
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
    let far = ["[1, 1e20]", "[-1e20]"];
    for xs in far
        .into_iter()
        .chain(["[9223372036854775808]", "[1, -9223372036854775809]"])
    {
        past(
            with(&mut db, "put t $1", &format!(r#"[{{"xs": {xs}}}]"#)),
            "field `xs`",
        );
        past(
            with(&mut db, "put t {xs: $1}", &format!("[{xs}]")),
            "field `xs`",
        );
    }
    for xs in far {
        past(run(&mut db, &format!("put t {{xs: {xs}}}")), "field `xs`");
    }
    for xs in ["[9223372036854775808]", "[1, -9223372036854775809]"] {
        let e = run(&mut db, &format!("put t {{xs: {xs}}}")).unwrap_err();
        assert!(e.to_string().contains("invalid integer"), "{e}");
    }
    assert_eq!(column(&db, "xs"), []);
}

/// An `[int]` and a `[float]` keep a list's numbers as written: read into a
/// vector's `f32`s, as a reader with no schema reads a list of numbers
/// alone, -9223372036854775808 was -2^63 and refused, 16777217 was stored
/// as 16777216 and 0.1 as 0.10000000149011612. A literal, a parameter, a
/// document, a `set` and a comparison alike; a text read the quick way is
/// refused rather than stored in `f32`s.
#[test]
fn a_lists_numbers_are_read_as_written() {
    let mut db = db();
    let ints = "[-9223372036854775808, 9223372036854775807, 16777217, 9007199254740993]";
    let floats = "[0.1, 16777217.5]";
    let want_ints = Value::List(
        [i64::MIN, i64::MAX, 16_777_217, 9_007_199_254_740_993]
            .map(Value::Int)
            .to_vec(),
    );
    let want_floats = Value::List(vec![Value::Float(0.1), Value::Float(16_777_217.5)]);
    run(&mut db, &format!("put t {{xs: {ints}, fs: {floats}}}")).unwrap();
    let both = format!("[{ints}, {floats}]");
    with(&mut db, "put t {xs: $1, fs: $2}", &both).unwrap();
    let doc = format!(r#"[{{"xs": {ints}, "fs": {floats}}}]"#);
    with(&mut db, "put t $1", &doc).unwrap();
    run(&mut db, "put t {n: 1}").unwrap();
    with(&mut db, "set t {xs: $1, fs: $2} where n = 1", &both).unwrap();
    assert_eq!(column(&db, "xs"), vec![want_ints; 4]);
    assert_eq!(column(&db, "fs"), vec![want_floats; 4]);
    // A comparison reads its list so too: as `f32`s, [16777217] was
    // [16777216], and found the row holding that.
    run(
        &mut db,
        "put t [{n: 2, xs: [16777217]}, {n: 3, xs: [16777216]}]",
    )
    .unwrap();
    for (text, params) in [
        ("get t where xs = [16777217] select n", "[]"),
        ("get t where xs = $1 select n", "[[16777217]]"),
    ] {
        let r = with(&mut db, text, params).unwrap();
        let rows = &r.rows().unwrap().rows;
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0].values[0], Value::Int(2), "{text}");
    }
    let quick = fennec_ql::parse_one(&format!("put t {{xs: {ints}}}")).unwrap();
    let e = db.execute(&quick).unwrap_err();
    assert!(e.to_string().contains("an [int], a [float]"), "{e}");
    let s = fennec_ql::parse_one("put t {fs: $1}").unwrap();
    let params = fennec_core::json::parse_params("[[0.1]]").unwrap();
    let e = db.execute_with(&s, &params).unwrap_err();
    assert!(e.to_string().contains("an [int], a [float]"), "{e}");
}

/// A key looked up by is the int -2^63 is, where only a write refuses the
/// float: a parent's float key -9223372036854775808.0 finds the child
/// holding `i64::MIN` through its `@hash` -- `required` too -- and
/// through an index over several fields, as `=` finds it.
#[test]
fn a_lookup_by_the_float_minus_2_to_63_finds_i64_min() {
    let mut db = db();
    run(
        &mut db,
        "create collection par (k float, j int);
         create collection ch (pk int @hash);
         create collection cc (pk int, z int, @sorted(pk, z));
         put par {k: -9223372036854775808.0, j: 1};
         put ch {pk: -9223372036854775808};
         put cc {pk: -9223372036854775808, z: 1}",
    )
    .unwrap();
    let json = |db: &mut Database, text: &str| {
        fennec_core::json::response_to_string(&run(db, text).unwrap())
    };
    let child = r#"[{"id":1,"pk":-9223372036854775808"#;
    for text in [
        "get par lookup ch on pk = k",
        "get par lookup ch on pk = k required",
        "get par lookup cc on (pk, z) = (k, j)",
    ] {
        let r = json(&mut db, text);
        assert!(r.contains(child), "{text}: {r}");
    }
    let r = json(&mut db, "get ch where pk = -9223372036854775808.0 count");
    assert!(r.contains(r#""count":1"#), "{r}");
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
/// scan does not: the index changed the answer. A `@hash`, a `@unique`, a
/// `@sorted` and an index over several fields each answer as `m`, which
/// none holds, is answered: by the scan.
#[test]
fn an_index_answers_past_the_range_as_the_scan_does() {
    let mut db = db();
    run(
        &mut db,
        "create collection h (n int @hash, u int @unique, s int @sorted, a int, b int, m int,
                              @sorted(a, b))",
    )
    .unwrap();
    for end in ["9223372036854775807", "-9223372036854775808"] {
        let row = format!("put h {{n: {end}, u: {end}, s: {end}, a: {end}, b: 1, m: {end}}}");
        run(&mut db, &row).unwrap();
    }
    let count = |db: &mut Database, text: &str| {
        let r = run(db, text).unwrap();
        r.rows().unwrap().rows.len()
    };
    for v in [
        "1e300",
        "-1e300",
        "9223372036854774784.0",
        "-9223372036854775808.0",
        "9223372036854775807",
        "-9223372036854775808",
    ] {
        for op in ["=", "<=", ">="] {
            let want = count(&mut db, &format!("get h where m {op} {v}"));
            for field in ["n", "u", "s", "a"] {
                let text = format!("get h where {field} {op} {v}");
                assert_eq!(count(&mut db, &text), want, "{text}");
            }
        }
    }
    assert_eq!(count(&mut db, "get h where m = 1e300"), 0);
    assert_eq!(count(&mut db, "get h where m = -9223372036854775808.0"), 1);
}
