//! A number past an `int`'s range is refused naming its field, never
//! clamped to the nearest int: `as` saturates, and `{"n": 1e300}` was filed
//! as 9223372036854775807. Every way a number reaches an int: a document's
//! JSON, a statement's literal, a parameter, a `set`'s arithmetic, an
//! upsert, a list's element, a named statement's argument and its default
//! -- and a read through an index or a `lookup` answers what the scan and
//! `=` do, a `lookup` by the float -2^63 finding `i64::MIN`.

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
    run(
        &mut db,
        "create collection t (k text @unique, n int, xs [int], f float)",
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

/// A float that is not whole was refused before the range was, and is.
#[test]
fn a_float_that_is_not_whole_stays_refused() {
    let mut db = db();
    #[track_caller]
    fn not_whole(r: Result<Response>) {
        match r {
            Err(Error::Type(m)) if m == "field `n`: expected int, found float" => {}
            other => panic!("{other:?}"),
        }
    }
    not_whole(run(&mut db, "put t {n: 2.5}"));
    not_whole(with(&mut db, "put t {n: $1}", "[2.5]"));
    not_whole(with(&mut db, "put t $1", r#"[{"n": 2.5}]"#));
    run(&mut db, "put t {n: 1}").unwrap();
    not_whole(run(&mut db, "set t {n: n + 0.5}"));
    assert_eq!(column(&db, "n"), [Value::Int(1)]);
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

/// `if absent else set`: the document it would insert and the `set` it
/// runs instead are held to the range alike, the row kept as it was.
#[test]
fn an_upsert_past_it_is_refused() {
    let mut db = db();
    for n in PAST {
        let text = "put t {k: 'a', n: $1} if absent else set {n: new.n}";
        past(with(&mut db, text, &format!("[{n}]")), "field `n`");
    }
    assert_eq!(column(&db, "n"), []);
    run(&mut db, "put t {k: 'a', n: 1}").unwrap();
    for n in PAST {
        let text = "put t {k: 'a', n: 2} if absent else set {n: $1}";
        past(with(&mut db, text, &format!("[{n}]")), "field `n`");
        let text = "put t {k: 'a', n: $1} if absent else set {n: new.n}";
        past(with(&mut db, text, &format!("[{n}]")), "field `n`");
    }
    past(
        run(
            &mut db,
            "put t {k: 'a', n: 2} if absent else set {n: n * 1e300}",
        ),
        "field `n`",
    );
    assert_eq!(column(&db, "n"), [Value::Int(1)]);
    for (n, want) in EDGES {
        let text = "put t {k: 'a', n: 2} if absent else set {n: $1}";
        with(&mut db, text, &format!("[{n}]")).unwrap();
        assert_eq!(column(&db, "n"), [Value::Int(want)], "{n}");
    }
}

/// An `[int]` is read as a vector's `f32`s, as on main: 2^63 is where an
/// `f32` puts 9223372036854775807 and everything near it, so the ends are
/// stored as they were, and only an `f32` past 2^63 -- `1e20` -- is
/// refused, never clamped.
#[test]
fn a_lists_element_past_it_is_refused_and_its_ends_kept() {
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
    let ends = "[-9223372036854775808, 9223372036854775807]";
    run(&mut db, &format!("put t {{xs: {ends}}}")).unwrap();
    with(&mut db, "put t $1", &format!(r#"[{{"xs": {ends}}}]"#)).unwrap();
    with(&mut db, "put t {xs: $1}", &format!("[{ends}]")).unwrap();
    for xs in ["[-9223372036854775808]", "[9223372036854775807]"] {
        with(&mut db, "put t {xs: $1}", &format!("[{xs}]")).unwrap();
    }
    let both = Value::List(vec![Value::Int(i64::MIN), Value::Int(i64::MAX)]);
    let one = |i| Value::List(vec![Value::Int(i)]);
    assert_eq!(
        column(&db, "xs"),
        [
            both.clone(),
            both.clone(),
            both,
            one(i64::MIN),
            one(i64::MAX)
        ]
    );
}

/// A key looked up by is the int the float is, where only a write refuses
/// the float -2^63: a parent's float key -9223372036854775808.0 finds the
/// child holding `i64::MIN` through its `@hash` -- `required` too -- and
/// through an index over several fields, as `=` finds it, and
/// 9223372036854775807.0 (2^63) none, as `=` finds none: it is past every
/// int, `i64::MAX` too. A parent's `1e300` finds no child, where it found
/// the one holding `i64::MAX` it was clamped to.
#[test]
fn a_lookup_by_the_float_2_to_63_finds_the_int_it_is() {
    let mut db = db();
    run(
        &mut db,
        "create collection par (k float, j int);
         create collection ch (pk int @hash);
         create collection cc (pk int, z int, @sorted(pk, z));
         put ch [{pk: -9223372036854775808}, {pk: 9223372036854775807}];
         put cc [{pk: -9223372036854775808, z: 1}, {pk: 9223372036854775807, z: 1}]",
    )
    .unwrap();
    let json = |db: &mut Database, text: &str| {
        fennec_core::json::response_to_string(&run(db, text).unwrap())
    };
    for (k, end) in [
        ("-9223372036854775808.0", Some("-9223372036854775808")),
        ("9223372036854775807.0", None),
    ] {
        run(&mut db, &format!("del par; put par {{k: {k}, j: 1}}")).unwrap();
        let found = usize::from(end.is_some());
        for text in [
            "get par lookup ch on pk = k",
            "get par lookup ch on pk = k required",
            "get par lookup cc on (pk, z) = (k, j)",
        ] {
            let r = json(&mut db, text);
            if let Some(end) = end {
                let child = format!(r#""pk":{end}"#);
                assert_eq!(r.matches(&child).count(), 1, "{text}: {r}");
            }
            assert_eq!(r.matches(r#""pk":"#).count(), found, "{text}: {r}");
        }
        let r = json(&mut db, &format!("get ch where pk = {k} count"));
        assert!(r.contains(&format!(r#""count":{found}"#)), "{k}: {r}");
    }
    run(&mut db, "set par {k: 1e300}").unwrap();
    for text in [
        "get par lookup ch on pk = k required",
        "get par lookup cc on (pk, z) = (k, j) required",
    ] {
        let r = run(&mut db, text).unwrap();
        assert_eq!(r.rows().unwrap().rows.len(), 0, "{text}");
    }
    let r = json(&mut db, "get ch where pk = 1e300 count");
    assert!(r.contains(r#""count":0"#), "{r}");
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
        "9223372036854775807.0",
        "-9223372036854775808.0",
        "9223372036854774784.0",
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
