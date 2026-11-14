//! A vector's component is a number an `f32` holds. One within an `f64`'s
//! range but past an `f32`'s -- `1e39`, `-1e39`, `3.5e38` -- read as the
//! `f32`'s infinity, which JSON writes as `null`, and a `NaN` or an
//! infinity handed over as bits, are refused naming the field, the element
//! and the range wherever a vector field or `near` takes one: a statement's
//! literal, a parameter read from JSON or handed over as `f32`s, a
//! document's object, pgvector's text, a `set`, an upsert, a named
//! statement's argument, a sparse vector's weight; a field stored as `f16`
//! holds 65 504 at most. One rounding below the smallest subnormal is the
//! nearest `f32`, 0, as pgvector's `strtof` reads it. A file an earlier
//! build wrote one into opens, reads and ranks it as it did.

use fennec_core::prelude::*;
use std::sync::Arc;

const RANGE: &str = "a vector's component is a number at most 3.4028235e38 either side of 0";
const HALF: &str = "an f16 vector's component is a number at most 65504 either side of 0";

fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text)? {
        last = db.execute(&s)?;
    }
    Ok(last)
}

/// `text` with its parameters read from JSON, as a server and the browser
/// module read them.
fn with_json(db: &mut Database, text: &str, params: &str) -> Result<Response> {
    let s = fennec_ql::parse_one(text)?;
    db.execute_with(&s, &fennec_core::json::parse_params(params)?)
}

/// `text` with `params` as they are: a vector handed over as `f32`s, as the
/// native library and the browser module take one (`fennec_abi::with_vectors`).
fn with(db: &mut Database, text: &str, params: &[Value]) -> Result<Response> {
    let s = fennec_ql::parse_one(text)?;
    db.execute_with(&s, params)
}

fn read(db: &Database, text: &str, params: &[Value]) -> Result<ResultSet> {
    let s = fennec_ql::parse_one(text)?;
    Ok(db.query(&s, params)?.rows().unwrap().clone())
}

/// Each row's values as JSON writes them, a row a line.
fn json(db: &Database, text: &str) -> String {
    let rs = read(db, text, &[]).unwrap();
    let rows: Vec<String> = rs
        .rows
        .iter()
        .map(|r| {
            let values: Vec<String> = r.values.iter().map(fennec_core::json::to_string).collect();
            values.join(" ")
        })
        .collect();
    rows.join("\n")
}

/// The largest `f32` and the smallest subnormal as JSON writes them, every
/// digit and no exponent.
const MAX: &str = "340282350000000000000000000000000000000";
const TINY: &str = "0.000000000000000000000000000000000000000000001";

fn db() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection t (k text, n int, v vector<3>, h vector<3, f16>, fs [float], e vector<3> @hnsw(l2));
         create collection s (w sparse<4> @inverted)",
    )
    .unwrap();
    db
}

fn count(db: &Database, collection: &str) -> usize {
    read(db, &format!("get {collection} select count(*) as n"), &[])
        .unwrap()
        .rows[0]
        .values[0]
        .as_f64()
        .unwrap() as usize
}

fn refused(r: Result<Response>, says: &str, what: &str) {
    match r {
        Ok(r) => panic!("{what}: taken, {r:?}"),
        Err(e) => {
            let e = e.to_string();
            assert!(e.contains(says), "{what}: {e}\n  wanted: {says}");
        }
    }
}

/// Numbers within an `f64`'s range past an `f32`'s, at each place: `1e39`
/// either side, `3.5e38`, the next decimal past the largest `f32`'s text,
/// and 2^128 - 2^103, the tie between the largest `f32` and the next
/// power of two, which goes to the even one, the infinity.
const PAST: [(&str, usize); 5] = [
    ("[1e39, 0, 0]", 0),
    ("[0, -1e39, 0]", 1),
    ("[0, 0, 3.5e38]", 2),
    ("[3.4028236e38, 0, 0]", 0),
    ("[0, -3.4028235677973366e38, 0]", 1),
];

#[test]
fn a_component_past_an_f32_is_refused_naming_the_field_and_the_element() {
    let mut db = db();
    run(&mut db, "put t {id: 1, k: 'kept', v: [1, 2, 3]}").unwrap();
    let upsert = "put t {id: 1, v: [0, 0, 0]} if absent else set {v: $1}";
    for (list, at) in PAST {
        let (doc, param) = (format!("{{\"v\": {list}}}"), format!("[{list}]"));
        // A literal, an insert, a parameter, a document and a page of them
        // as a parameter, a `set` both ways, an upsert's `else set`.
        let tries = [
            run(&mut db, &format!("put t {{v: {list}}}")),
            run(&mut db, &format!("insert t {{v: {list}}}")),
            with_json(&mut db, "put t {v: $1}", &param),
            with_json(&mut db, "put t $1", &format!("[{doc}]")),
            with_json(&mut db, "put t $1", &format!("[[{doc}]]")),
            run(&mut db, &format!("set t {{v: {list}}} where id = 1")),
            with_json(&mut db, "set t {v: $1} where id = 1", &param),
            with_json(&mut db, upsert, &param),
        ];
        let says = format!("field `v`, element {at}: {RANGE}");
        for (n, r) in tries.into_iter().enumerate() {
            refused(r, &says, &format!("{list}, way {n}"));
        }
        // An indexed field too, before its graph meets it.
        let says = format!("field `e`, element {at}: {RANGE}");
        refused(run(&mut db, &format!("put t {{e: {list}}}")), &says, list);
    }
    // NaN and the infinities handed over as bits, as the native library and
    // the browser module take a vector.
    for (bits, at) in [
        ([f32::NAN, 0.0, 0.0], 0),
        ([0.0, f32::from_bits(0xffc0_0001), 0.0], 1),
        ([0.0, 0.0, f32::INFINITY], 2),
        ([f32::NEG_INFINITY, f32::NAN, 0.0], 0),
    ] {
        let v = [Value::Vector(bits.to_vec())];
        let says = format!("field `v`, element {at}: {RANGE}");
        refused(with(&mut db, "put t {v: $1}", &v), &says, "bits");
        refused(
            with(&mut db, "set t {v: $1} where id = 1", &v),
            &says,
            "bits",
        );
        let says = format!("field `h`, element {at}: {HALF}");
        refused(with(&mut db, "put t {h: $1}", &v), &says, "bits");
    }
    // Nothing went in, and the row the `set`s named is as it was.
    assert_eq!(count(&db, "t"), 1);
    assert_eq!(json(&db, "get t select v"), "[1,2,3]");
}

/// The ends: the largest `f32` either side, read from its shortest text and
/// from the largest `f64` under the tie, and the smallest subnormal. A
/// number below half the smallest subnormal is the nearest `f32`, 0, its
/// sign kept, and half of it a tie to the even 0: pgvector's `strtof`
/// reads each so and refuses only an infinity.
#[test]
fn the_ends_of_an_f32_are_taken_and_below_them_is_zero() {
    let mut db = db();
    let taken = [
        (
            "[3.4028235e38, -3.4028235e38, 1e-45]",
            format!("[{MAX},-{MAX},{TINY}]"),
        ),
        (
            "[3.4028235677973362e38, 0, -1.401298464324817e-45]",
            format!("[{MAX},0,-{TINY}]"),
        ),
        (
            "[1e-46, -1e-46, 7.006492321624085e-46]",
            "[0,-0,0]".to_string(),
        ),
        (
            "[340282346638528859811704183484516925440.0, 1e-300, 0]",
            format!("[{MAX},0,0]"),
        ),
    ];
    for (n, (list, out)) in taken.into_iter().enumerate() {
        let id = n + 1;
        run(
            &mut db,
            &format!("put t {{id: {id}, v: {list}, e: {list}}}"),
        )
        .unwrap();
        let got = json(&db, &format!("get t select v, e where id = {id}"));
        assert_eq!(got, format!("{out} {out}"), "{list}");
        let put = "put t {id: $1, v: $2, e: $2}";
        with_json(&mut db, put, &format!("[{id}, {list}]")).unwrap();
        let got = json(&db, &format!("get t select v, e where id = {id}"));
        assert_eq!(got, format!("{out} {out}"), "{list} as a parameter");
    }
    // The smallest subnormal and the largest `f32` handed over as bits.
    let ends = Value::Vector(vec![f32::from_bits(1), f32::MAX, -f32::MAX]);
    with(&mut db, "put t {id: 9, v: $1}", &[ends]).unwrap();
    let got = json(&db, "get t select v where id = 9");
    assert_eq!(got, format!("[{TINY},{MAX},-{MAX}]"));
    // `near` finds them by them.
    let hits = read(
        &db,
        "get t select id near e [3.4028235e38, -3.4028235e38, 1e-45] limit 1",
        &[],
    )
    .unwrap();
    assert_eq!(hits.rows[0].id, 1);
}

/// A `vector<N, f16>` stores each component as the nearest `f16`, and from
/// 65 520 that is the `f16`'s infinity: refused, where it was stored as
/// one and read back as `null`. 65 504 is its largest, and anything under
/// 65 520 rounds to it; a component too small for one is 0.
#[test]
fn an_f16_field_holds_65504_at_most() {
    let mut db = db();
    run(&mut db, "put t {id: 1, h: [65504, -65519.99, 1e-8]}").unwrap();
    assert_eq!(json(&db, "get t select h"), "[65504,-65504,0]");
    for (list, at) in [
        ("[65520, 0, 0]", 0),
        ("[0, -1e5, 0]", 1),
        ("[0, 0, 1e39]", 2),
    ] {
        let says = format!("field `h`, element {at}: {HALF}");
        refused(run(&mut db, &format!("put t {{h: {list}}}")), &says, list);
        refused(
            with_json(&mut db, "put t {h: $1}", &format!("[{list}]")),
            &says,
            list,
        );
    }
    // A `vector<N>` takes what an `f16` does not.
    run(&mut db, "put t {id: 2, v: [65520, -1e5, 1e38]}").unwrap();
    assert_eq!(count(&db, "t"), 2);
}

/// A vector past sixteen components, as an embedding is: a `put` copies
/// one it is given into the document with its test in the copy -- sixteen
/// components at a time on aarch64 -- a literal's, a parameter's handed
/// over as `f32`s or read from JSON, a document's in a page of them, and
/// `set` and an upsert test one as they test every value. Each refuses a
/// component no field holds at a block's ends and past the last block,
/// naming it, and takes one whose top byte reaches the bound's though it is
/// held -- 2^127 and past under `f32`, 2^15 and past under `f16` -- reading
/// it back as it went in.
#[test]
fn a_wide_vector_is_held_wherever_a_write_takes_it() {
    let mut db = db();
    // 70: a turn of four blocks and six past them; 40: two blocks and eight.
    run(
        &mut db,
        "create collection w (v vector<70>, h vector<40, f16>)",
    )
    .unwrap();
    // Numbers an `f16` holds too, so either field reads them back as given.
    let base = |n: usize| -> Vec<f32> { (0..n).map(|i| (i % 7) as f32 * 0.25 - 0.75).collect() };
    // In, each number with its exponent -- FennecQL reads no integer past an
    // `i64`, which JSON writes 2^127 as -- the one at `at` as `x` says. Out,
    // as JSON writes them.
    let digits_of = |v: &[f32], at: usize, x: &str| {
        let mut d: Vec<String> = v.iter().map(|x| format!("{x:e}")).collect();
        d[at] = x.to_string();
        format!("[{}]", d.join(","))
    };
    let text = |v: &[f32]| fennec_core::json::to_string(&Value::Vector(v.to_vec()));
    let writes = |db: &mut Database, f: &str, v: &[f32], digits: &str| {
        let doc = format!("{{\"{f}\": {digits}}}");
        [
            run(db, &format!("put w {{{f}: {digits}}}")),
            run(db, &format!("insert w {{{f}: {digits}}}")),
            with(
                db,
                &format!("put w {{{f}: $1}}"),
                &[Value::Vector(v.to_vec())],
            ),
            with_json(db, &format!("put w {{{f}: $1}}"), &format!("[{digits}]")),
            with_json(db, "put w $1", &format!("[[{doc}]]")),
            with(
                db,
                &format!("set w {{{f}: $1}} where id = 1"),
                &[Value::Vector(v.to_vec())],
            ),
            with_json(
                db,
                &format!("put w {{id: 1}} if absent else set {{{f}: $1}}"),
                &format!("[{digits}]"),
            ),
        ]
    };
    run(&mut db, "put w {id: 1}").unwrap();
    for (f, n, range, past) in [("v", 70, RANGE, "1e39"), ("h", 40, HALF, "65520")] {
        for at in [0, 15, 16, 31, 32, 63, 64, n - 1]
            .into_iter()
            .filter(|at| *at < n)
        {
            // Past what the field holds, as digits and as bits.
            let mut v = base(n);
            let digits = digits_of(&v, at, &format!("-{past}"));
            v[at] = f32::NAN;
            let says = format!("field `{f}`, element {at}: {range}");
            for (way, r) in writes(&mut db, f, &v, &digits).into_iter().enumerate() {
                refused(r, &says, &format!("{f}, {at}, way {way}"));
            }
            v[at] = f32::NEG_INFINITY;
            let r = with(
                &mut db,
                &format!("put w {{{f}: $1}}"),
                &[Value::Vector(v.clone())],
            );
            refused(r, &says, &format!("{f}, {at}, -inf"));
            // Held, though its top byte reaches the bound's.
            let big = match f {
                "v" => [f32::from_bits(0x7f00_0000), -3e38],
                _ => [32_768.0, -65_504.0],
            };
            for x in big {
                let mut v = base(n);
                v[at] = x;
                let ways = writes(&mut db, f, &v, &digits_of(&v, at, &format!("{x:e}")));
                assert!(ways.iter().all(|r| r.is_ok()), "{f}, {x} at {at}: {ways:?}");
                let got = json(&db, &format!("get w select {f} where id = 1"));
                assert_eq!(got, text(&v), "{f}, {x} at {at}");
                let got = json(&db, &format!("get w select {f} order id desc limit 1"));
                assert_eq!(got, text(&v), "{f}, {x} at {at}: the page's row");
            }
        }
        // One of another length, which the copy leaves to `coerce`, is told
        // its dimension as ever.
        let short = base(n - 1);
        let says = format!("vector dimension must be {n}, got {}", n - 1);
        refused(
            with(
                &mut db,
                &format!("put w {{{f}: $1}}"),
                &[Value::Vector(short)],
            ),
            &says,
            "short",
        );
    }
    // Each way that took one wrote a row, `set` and the upsert row 1.
    assert_eq!(count(&db, "w"), 1 + 2 * 8 * 5 + 2 * 6 * 5);
}

/// `near`'s and `rerank`'s query vector too: every distance from an
/// infinity is an infinity or a `NaN`, which ranked the rows as their bits
/// fell. Literal, parameter, JSON, pgvector's text, bits; with a filter,
/// `exact` or not.
#[test]
fn near_refuses_a_query_vector_past_an_f32() {
    let mut db = db();
    run(
        &mut db,
        "put t [{k: 'a', e: [1, 0, 0]}, {k: 'b', e: [0, 1, 0]}, {k: 'c', e: [0, 0, 1]}]",
    )
    .unwrap();
    for (list, at) in PAST {
        let says = format!("the query vector for `e`, element {at}: {RANGE}");
        for text in [
            format!("get t near e {list} limit 2"),
            format!("get t near e {list} exact limit 2"),
            format!("get t where k != 'a' near e {list} limit 2"),
            format!("get t near e '{list}' limit 2"),
        ] {
            refused(read(&db, &text, &[]).map(Response::Rows), &says, &text);
        }
        let params = fennec_core::json::parse_params(&format!("[{list}]")).unwrap();
        refused(
            read(&db, "get t near e $1 limit 2", &params).map(Response::Rows),
            &says,
            list,
        );
        let text = Value::Text(list.to_string());
        refused(
            read(&db, "get t near e $1 limit 2", &[text]).map(Response::Rows),
            &says,
            list,
        );
    }
    let bits = Value::Vector(vec![0.0, f32::NAN, 0.0]);
    let says = format!("the query vector for `e`, element 1: {RANGE}");
    refused(
        read(&db, "get t near e $1 limit 2", &[bits]).map(Response::Rows),
        &says,
        "bits",
    );
    // The largest `f32` is a query like any other.
    let r = read(&db, "get t near e [3.4028235e38, 0, 0] limit 1", &[]).unwrap();
    assert_eq!(r.rows.len(), 1);
}

#[cfg(all(feature = "text", feature = "vector"))]
#[test]
fn rerank_refuses_a_query_vector_past_an_f32() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection d (body text @text, e vector<3> @hnsw(cosine));
         put d [{body: 'red fox', e: [1, 0, 0]}, {body: 'red hen', e: [0, 1, 0]}]",
    )
    .unwrap();
    let text = "get d match body 'red' rerank e [0, 0, 1e39] limit 2";
    let says = format!("the query vector for `e`, element 2: {RANGE}");
    refused(read(&db, text, &[]).map(Response::Rows), &says, text);
    read(
        &db,
        "get d match body 'red' rerank e [0, 0, 1] limit 2",
        &[],
    )
    .unwrap();
}

/// A sparse vector's weight is an `f32` too, and every way in goes through
/// `sparse::normalise`, which names the index as the text writes it.
#[test]
fn a_sparse_weight_past_an_f32_is_refused_naming_its_index() {
    let mut db = db();
    let says =
        "index 3: a sparse vector's weight is a number at most 3.4028235e38 either side of 0";
    for text in ["{1:0.5,3:1e39}/4", "{3:-1e39,1:0.5}/4"] {
        refused(run(&mut db, &format!("put s {{w: '{text}'}}")), says, text);
        refused(
            read(&db, &format!("get s near w '{text}' limit 1"), &[]).map(Response::Rows),
            says,
            text,
        );
    }
    let bits = Value::Sparse(4, vec![(0, 0.5), (2, f32::INFINITY)]);
    refused(with(&mut db, "put s {w: $1}", &[bits]), says, "bits");
    // The ends are taken; a weight too small for an `f32` is its 0, and
    // dropped as a zero is.
    run(
        &mut db,
        "put s {id: 1, w: '{1:3.4028235e38,2:-3.4028235e38,4:1e-46}/4'}",
    )
    .unwrap();
    assert_eq!(
        json(&db, "get s select w"),
        format!("\"{{1:{MAX},2:-{MAX}}}/4\"")
    );
}

#[cfg(feature = "statements")]
#[test]
fn a_named_statement_s_vector_argument_is_held_too() {
    let mut db = db();
    run(
        &mut db,
        "create statement keep($v vector<3>) public as insert t {v: $v}",
    )
    .unwrap();
    let says = format!("the argument `v`, vector<3>: element 1: {RANGE}");
    refused(run(&mut db, "call keep {v: [0, 1e39, 0]}"), &says, "call");
    run(&mut db, "call keep {v: [0, 3.4028235e38, 0]}").unwrap();
    assert_eq!(count(&db, "t"), 1);
}

/// Puts into `v` and `e` the vector a document's `n` names, as an earlier
/// build stored `1e39`'s infinity or a binding's `NaN`: a hook's changes
/// are not read again, and no write takes one now.
struct Plant;
impl Hook for Plant {
    fn name(&self) -> &str {
        "plant"
    }
    fn before_write(&self, _: &Schema, _: WriteOp, doc: &mut Document) -> Result<()> {
        let planted = match doc.get("n") {
            Some(Value::Int(-1)) => [f32::INFINITY, 0.0, 0.0],
            Some(Value::Int(-2)) => [f32::NAN, 0.0, 0.0],
            Some(Value::Int(-3)) => [0.0, f32::NEG_INFINITY, f32::from_bits(0xffc0_0000)],
            _ => return Ok(()),
        };
        doc.set("v", Value::Vector(planted.to_vec()));
        doc.set("e", Value::Vector(planted.to_vec()));
        Ok(())
    }
}
impl Plugin for Plant {
    fn name(&self) -> &str {
        "plant"
    }
    fn init(&self, reg: &mut Registry) -> Result<()> {
        reg.register_hook(Arc::new(Plant));
        Ok(())
    }
}

/// A file an earlier build wrote an infinity or a `NaN` into -- `1e39` read
/// as an `f32`, a binding's bits -- opens and reads as it did: JSON writes
/// the component as `null`, another field of the row is written as ever,
/// `=` finds it by a key holding it, and its vector written again puts it
/// right.
#[test]
fn a_file_holding_a_component_no_f32_holds_opens_and_reads() {
    let mut planted = db();
    planted.install_plugin(&Plant).unwrap();
    run(
        &mut planted,
        "put t [{id: 1, n: -1}, {id: 2, n: -2}, {id: 3, n: -3}]",
    )
    .unwrap();
    for i in 4..40 {
        let e = format!("[{}, {}, 1]", i as f32 / 7.0, (40 - i) as f32 / 5.0);
        run(
            &mut planted,
            &format!("put t {{id: {i}, n: {i}, v: {e}, e: {e}}}"),
        )
        .unwrap();
    }
    let mut db = Database::new();
    db.load(&planted.snapshot()).expect("it opens");
    assert_eq!(count(&db, "t"), 39);
    assert_eq!(
        json(&db, "get t select v where id <= 3 order id"),
        "[null,0,0]\n[null,0,0]\n[0,null,null]"
    );
    // Another field of the row is written as ever, its vector kept.
    run(&mut db, "set t {k: 'seen'} where id = 1").unwrap();
    let r = read(&db, "get t select v where id = 1", &[]).unwrap();
    assert_eq!(
        r.rows[0].values[0],
        Value::Vector(vec![f32::INFINITY, 0.0, 0.0])
    );
    // `=` finds it by a key holding it, as the scan does.
    let key = Value::Vector(vec![f32::INFINITY, 0.0, 0.0]);
    let r = read(&db, "get t select id where v = $1", &[key]).unwrap();
    assert_eq!(r.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1]);
    // Read back as JSON it is `null` there, which no vector takes.
    refused(
        with_json(&mut db, "put t {id: 1, v: $1}", "[[null, 0, 0]]"),
        "vector element must be numeric, found null",
        "null",
    );
    // Written again, it is right.
    run(&mut db, "put t {id: 1, v: [1, 0, 0], e: [1, 0, 0]}").unwrap();
    assert_eq!(json(&db, "get t select v where id = 1"), "[1,0,0]");
}

/// `near` measures such a vector as it is, which no other distance pays
/// for: its distance is what IEEE arithmetic makes of the infinity or the
/// `NaN`, ordered by `total_cmp`, so the walk, the scan and a filtered
/// search answer, the finite rows come in the order they would without it,
/// and the row itself comes after them -- `+inf`, a positive `NaN` -- or
/// before them -- `-inf`, a negative `NaN` -- never among them, its
/// `_score` no number. Which way a `NaN` the arithmetic makes falls is the
/// platform's (x86-64's is negative); one a vector holds keeps its sign
/// through a sum and a product, so row 3's negative one leads an L2 page
/// and row 2's positive one a dot product's, which negates it.
#[test]
fn near_ranks_a_file_s_vector_holding_no_number_as_ieee_orders_it() {
    for metric in ["l2", "cosine", "dot"] {
        fennec_ql::install();
        let made = |planted: bool| {
            let mut db = Database::new();
            let create = format!("create collection t (n int, e vector<3> @hnsw({metric}))");
            run(&mut db, &create).unwrap();
            if planted {
                db.install_plugin(&Plant).unwrap();
                run(
                    &mut db,
                    "put t [{id: 1, n: -1}, {id: 2, n: -2}, {id: 3, n: -3}]",
                )
                .unwrap();
            }
            for i in 4..40 {
                let e = format!("[{}, {}, 1]", i as f32 / 7.0, (40 - i) as f32 / 5.0);
                run(&mut db, &format!("put t {{id: {i}, n: {i}, e: {e}}}")).unwrap();
            }
            db
        };
        let (db, twin) = (made(true), made(false));
        let q = format!("[{}, 6, 1]", 10f32 / 7.0);
        let exact = format!("get t near e {q} exact limit 39");
        let (page, alone) = (
            read(&db, &exact, &[]).unwrap(),
            read(&twin, &exact, &[]).unwrap(),
        );
        let finite = |rs: &ResultSet| -> Vec<u64> {
            let numbers = rs
                .rows
                .iter()
                .filter(|r| r.score.is_some_and(f32::is_finite));
            numbers.map(|r| r.id).collect()
        };
        assert_eq!(finite(&page), finite(&alone), "{metric}");
        // Before every number or after it, never among them.
        let at: Vec<usize> = (0..39).filter(|&i| page.rows[i].id > 3).collect();
        assert_eq!(at.len(), 36, "{metric}");
        assert_eq!(at[35] - at[0], 35, "{metric}: {:?}", page.rows);
        for row in page.rows.iter().filter(|r| r.id <= 3) {
            assert!(!row.score.unwrap().is_finite(), "{metric}");
        }
        let first = match metric {
            "l2" => Some(3),
            "dot" => Some(2),
            _ => None,
        };
        if let Some(first) = first {
            assert_eq!(page.rows[0].id, first, "{metric}");
        }
        // The walk and a filtered search find the same finite rows first.
        for (text, left_out) in [
            (format!("get t near e {q} limit 8"), 0),
            (format!("get t where n != 11 near e {q} limit 8"), 11),
        ] {
            let got = finite(&read(&db, &text, &[]).unwrap());
            let want: Vec<u64> = finite(&alone)
                .into_iter()
                .filter(|id| *id != left_out)
                .collect();
            assert_eq!(got[..4], want[..4], "{metric}: {text}");
        }
    }
}
