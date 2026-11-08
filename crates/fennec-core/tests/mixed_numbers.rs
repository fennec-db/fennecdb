//! An int and a float compare by their true values, everywhere a value is
//! compared: the scan, `@hash`, `@unique`, `@sorted`, an index over several
//! fields, `lookup`, `in`, `order`, `having` and `min`/`max`. Met as two
//! f64s, the ints 2^60 and 2^60 + 1 each equalled the float 2^60 and not
//! each other: `n = 1152921504606846976.0` found one row through a `@hash`,
//! which keys the int exactly, and four through the scan.

use fennec_core::prelude::*;

const P53: i64 = 1 << 53;
const P60: i64 = 1 << 60;
/// 2^63, which no int is.
const END: f64 = 9_223_372_036_854_775_808.0;

fn run(db: &mut Database, text: &str) -> Response {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text).unwrap_or_else(|e| panic!("{text}: {e}")) {
        last = db.execute(&s).unwrap_or_else(|e| panic!("{text}: {e}"));
    }
    last
}

fn with(db: &mut Database, text: &str, params: &[Value]) -> Result<Response> {
    db.execute_with(&fennec_ql::parse_one(text).unwrap(), params)
}

fn answer(db: &Database, text: &str, params: &[Value]) -> ResultSet {
    let s = fennec_ql::parse_one(text).unwrap_or_else(|e| panic!("{text}: {e}"));
    let r = db
        .query(&s, params)
        .unwrap_or_else(|e| panic!("{text}: {e}"));
    r.rows().unwrap().clone()
}

/// A json object of `members`, their keys in order.
fn obj(members: &[(&str, &Value)]) -> Value {
    Value::Object(
        members
            .iter()
            .map(|(k, v)| (k.to_string(), (*v).clone()))
            .collect(),
    )
}

/// The json `m` the int collections hold: `h` and `s` indexed, `v` not.
fn three(v: &Value) -> Value {
    obj(&[("h", v), ("s", v), ("v", v)])
}

/// One parameter.
fn one(v: &Value) -> &[Value] {
    std::slice::from_ref(v)
}

fn ids(db: &Database, text: &str, params: &[Value]) -> Vec<u64> {
    answer(db, text, params).rows.iter().map(|r| r.id).collect()
}

/// The ints near 2^53, 2^60 and the ends that a float holds, a float
/// rounds to, or lies halfway between: 2^60 + 128 rounds to 2^60, 2^60 +
/// 129 to 2^60 + 256.
const INTS: [i64; 20] = [
    0,
    3,
    P53 - 1,
    P53,
    P53 + 1,
    P53 + 2,
    P60 - 1,
    P60,
    P60 + 1,
    P60 + 128,
    P60 + 129,
    P60 + 256,
    -P60,
    -P60 - 1,
    9_223_372_036_854_774_784,
    i64::MAX - 1,
    i64::MAX,
    i64::MIN,
    i64::MIN + 1,
    -P53 - 1,
];

/// Floats at and between those ints, the ends and past them.
fn floats() -> Vec<f64> {
    let mut out = vec![
        0.0,
        -0.0,
        0.5,
        3.0,
        END,
        -END,
        1e300,
        -1e300,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    for i in INTS {
        let f = i as f64;
        out.extend([
            f,
            f64::from_bits(f.to_bits() + 1),
            f64::from_bits(f.to_bits().max(1) - 1),
        ]);
    }
    out
}

const OPS: [&str; 6] = ["=", "!=", "<", "<=", ">", ">="];

/// A collection whose `n` is the scan and every other field the same value
/// through an index: `h` a `@hash`, `u` a `@unique`, `s` a `@sorted`, `a`
/// the first part of an index over two, and `m` a json field whose `h` and
/// `s` paths are indexed and `v` is not.
fn ints() -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection t (n int, h int @hash, u int @unique, s int @sorted, a int, z int,
                              m json, @sorted(a, z));
         create index on t (m.h) @hash;
         create index on t (m.s) @sorted",
    );
    for i in INTS {
        let text = "put t {n: $1, h: $1, u: $1, s: $1, a: $1, z: 0, m: $2}";
        with(&mut db, text, &[Value::Int(i), three(&Value::Int(i))]).unwrap();
    }
    db
}

#[test]
fn the_float_2_to_60_is_the_one_int_it_is_through_every_index() {
    let db = ints();
    let p60 = Value::Float(P60 as f64);
    let at = |i: i64| INTS.iter().position(|&x| x == i).unwrap() as u64 + 1;
    for field in ["n", "h", "u", "s", "a", "m.h", "m.s", "m.v"] {
        let text = format!("get t where {field} = $1");
        assert_eq!(ids(&db, &text, one(&p60)), [at(P60)], "{text}");
        let text = format!("get t where {field} = 1152921504606846976.0");
        assert_eq!(ids(&db, &text, &[]), [at(P60)], "{text}");
        // 2^63 is past every int, and -2^63 is `i64::MIN`.
        let text = format!("get t where {field} = $1");
        assert_eq!(ids(&db, &text, &[Value::Float(END)]), [0u64; 0], "{text}");
        assert_eq!(
            ids(&db, &text, &[Value::Float(-END)]),
            [at(i64::MIN)],
            "{text}"
        );
        let text = format!("get t where {field} >= $1");
        assert_eq!(ids(&db, &text, &[Value::Float(END)]), [0u64; 0], "{text}");
        let text = format!("get t where {field} < $1");
        assert_eq!(
            ids(&db, &text, &[Value::Float(END)]).len(),
            INTS.len(),
            "{text}"
        );
    }
    // Each op, each number near the edges, each field against the scan.
    let mut literals: Vec<Value> = floats().into_iter().map(Value::Float).collect();
    literals.extend(INTS.map(Value::Int));
    literals.push(Value::Float(f64::NAN));
    for v in &literals {
        for op in OPS {
            let want = ids(&db, &format!("get t where n {op} $1"), one(v));
            for field in ["h", "u", "s", "a", "m.h", "m.s", "m.v"] {
                let text = format!("get t where {field} {op} $1");
                assert_eq!(ids(&db, &text, one(v)), want, "{text} with {v:?}");
            }
        }
    }
}

#[test]
fn an_in_list_finds_what_its_equalities_find() {
    let db = ints();
    let short = Value::List(vec![Value::Float(P60 as f64), Value::Float(END)]);
    // Long enough for a list looked up rather than compared a value at a
    // time, a float among its ints.
    let mut long: Vec<Value> = INTS[..10].iter().map(|&i| Value::Int(i + 1)).collect();
    long.push(Value::Float(P60 as f64));
    let long = Value::List(long);
    for list in [short, long] {
        let want = ids(&db, "get t where n in $1", one(&list));
        assert!(want.contains(&8), "{want:?}");
        assert!(!want.contains(&7) && !want.contains(&10), "{want:?}");
        for field in ["h", "u", "s", "a", "m.h", "m.s", "m.v"] {
            let text = format!("get t where {field} in $1");
            assert_eq!(ids(&db, &text, one(&list)), want, "{text}");
        }
    }
    let text = "get t where h in [1152921504606846976.0, 9223372036854775808.0]";
    assert_eq!(ids(&db, text, &[]), [8]);
}

/// A float field meets an int as the int it is: 2^53 + 1 is no float, so
/// `=` finds none and the index -- which `as` made it 2^53 for -- none.
#[test]
fn a_float_field_meets_an_int_no_float_holds_as_none() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection f (x float, xh float @hash, xu float @unique, xs float @sorted,
                              a float, z int, @sorted(a, z))",
    );
    let fs: Vec<f64> = floats().into_iter().filter(|f| f.is_finite()).collect();
    let mut kept = Vec::new();
    for f in fs {
        // `@unique` holds -0.0 and 0.0 one value.
        if kept.contains(&f) {
            continue;
        }
        kept.push(f);
        let text = "put f {x: $1, xh: $1, xu: $1, xs: $1, a: $1, z: 0}";
        with(&mut db, text, &[Value::Float(f)]).unwrap();
    }
    for field in ["x", "xh", "xu", "xs", "a"] {
        let text = format!("get f where {field} = 9007199254740993");
        assert_eq!(ids(&db, &text, &[]), [0u64; 0], "{text}");
        let text = format!("get f where {field} = 9223372036854775807");
        assert_eq!(ids(&db, &text, &[]), [0u64; 0], "{text}");
    }
    let mut literals: Vec<Value> = INTS.map(Value::Int).to_vec();
    literals.extend(kept.iter().map(|&f| Value::Float(f)));
    for v in &literals {
        for op in OPS {
            let want = ids(&db, &format!("get f where x {op} $1"), one(v));
            for field in ["xh", "xu", "xs", "a"] {
                let text = format!("get f where {field} {op} $1");
                assert_eq!(ids(&db, &text, one(v)), want, "{text} with {v:?}");
            }
        }
    }
}

/// A `lookup` finds the children `=` finds: a parent's float 2^60 the child
/// holding 2^60 and not its neighbours, 2^63 none, -2^63 `i64::MIN`; an
/// int parent no float holds no `float` child.
#[test]
fn a_lookup_finds_what_equality_finds() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection par (k float, i int, j int);
         create collection ch (pk int @hash);
         create collection cc (pk int, z int, @sorted(pk, z));
         create collection cf (pf float @hash)",
    );
    for i in [P60 - 1, P60, P60 + 1, P60 + 128, i64::MAX, i64::MIN] {
        with(&mut db, "put ch {pk: $1}", &[Value::Int(i)]).unwrap();
        with(&mut db, "put cc {pk: $1, z: 1}", &[Value::Int(i)]).unwrap();
    }
    for f in [P53 as f64, (P53 + 2) as f64, P60 as f64] {
        with(&mut db, "put cf {pf: $1}", &[Value::Float(f)]).unwrap();
    }
    let count = |db: &Database, text: &str, v: &Value| answer(db, text, one(v)).rows.len();
    for k in [P60 as f64, END, -END, (P60 + 256) as f64] {
        run(&mut db, "del par");
        with(&mut db, "put par {k: $1, i: 1, j: 1}", &[Value::Float(k)]).unwrap();
        let want = count(&db, "get ch where pk = $1", &Value::Float(k));
        for text in [
            "get par lookup ch on pk = k",
            "get par lookup ch on pk = k required",
            "get par lookup cc on (pk, z) = (k, j)",
        ] {
            let r = answer(&db, text, &[]);
            let got = r.nested.as_ref().map_or(0, |n| n.rows().count());
            assert_eq!(got, want, "{text} with {k}");
        }
        let want = if k == P60 as f64 || k == -END { 1 } else { 0 };
        assert_eq!(
            count(&db, "get ch where pk = $1", &Value::Float(k)),
            want,
            "{k}"
        );
    }
    for i in [P53 + 1, P53 + 2, P60, P60 + 1] {
        run(&mut db, "del par");
        with(&mut db, "put par {k: 0.0, i: $1, j: 1}", &[Value::Int(i)]).unwrap();
        let want = count(&db, "get cf where pf = $1", &Value::Int(i));
        assert_eq!(want, usize::from(i != P53 + 1 && i != P60 + 1), "{i}");
        let r = answer(&db, "get par lookup cf on pf = i", &[]);
        assert_eq!(r.nested.unwrap().rows().count(), want, "{i}");
    }
}

/// A json field holding ints and floats is put in their true order, two
/// equal ones in id order, with an ordered index on it or without.
#[test]
fn an_order_over_ints_and_floats_is_their_true_order() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection o (m json, g int);
         create index on o (m.s) @sorted",
    );
    let vals = [
        Value::Int(P60 + 1),
        Value::Float(P60 as f64),
        Value::Int(P60),
        Value::Int(P60 - 1),
        Value::Float((P60 + 256) as f64),
        Value::Int(P60 + 255),
        Value::Float(P60 as f64 - 128.0),
    ];
    for v in &vals {
        let m = obj(&[("s", v), ("v", v)]);
        with(&mut db, "put o {m: $1, g: 1}", &[m]).unwrap();
    }
    let want = [7, 4, 2, 3, 1, 6, 5];
    assert_eq!(ids(&db, "get o order m.v", &[]), want);
    assert_eq!(ids(&db, "get o order m.s", &[]), want);
    let back: Vec<u64> = [5, 6, 1, 2, 3, 4, 7].to_vec();
    assert_eq!(ids(&db, "get o order m.v desc", &[]), back);
    assert_eq!(ids(&db, "get o order m.s desc", &[]), back);
    // Without 2^60 - 1, 2^60 + 1 and 2^60 + 255, which no float holds and
    // the ordered index holds apart, it answers: the int 2^60 is keyed as
    // the float it is, beside it in id order.
    run(&mut db, "del o where id = 1 or id = 4 or id = 6");
    assert_eq!(ids(&db, "get o order m.v", &[]), [7, 2, 3, 5]);
    assert_eq!(ids(&db, "get o order m.s", &[]), [7, 2, 3, 5]);
    for k in 0..300 {
        let m = obj(&[("s", &Value::Int(k)), ("v", &Value::Int(k))]);
        with(&mut db, "put o {m: $1, g: 2}", &[m]).unwrap();
    }
    let text = "get o order m.s desc limit 4";
    assert_eq!(ids(&db, text, &[]), [5, 2, 3, 7]);
    assert_eq!(ids(&db, "get o order m.v desc limit 4", &[]), [5, 2, 3, 7]);
    let p = answer(&db, &format!("explain {text}"), &[]);
    assert!(format!("{:?}", p.rows).contains("walked"), "{p:?}");
    let p = answer(
        &db,
        "explain get o where m.s >= 1152921504606846976 count",
        &[],
    );
    assert!(format!("{:?}", p.rows).contains("ordered index"), "{p:?}");
    for v in [
        Value::Int(P60),
        Value::Int(P60 + 1),
        Value::Int(P60 - 1),
        Value::Float(P60 as f64),
        Value::Float(END),
    ] {
        for op in OPS {
            let want = ids(&db, &format!("get o where m.v {op} $1"), one(&v));
            let text = format!("get o where m.s {op} $1");
            assert_eq!(ids(&db, &text, one(&v)), want, "{text} with {v:?}");
        }
    }
}

/// `min`, `max` and `having` compare as `=` and `order` do.
#[test]
fn min_max_and_having_take_the_true_values() {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, "create collection g (k int, v json)");
    for (k, v) in [
        (1, Value::Float(P60 as f64)),
        (1, Value::Int(P60 + 1)),
        (2, Value::Float(P60 as f64)),
        (2, Value::Int(P60 - 1)),
        (3, Value::Int(P60)),
    ] {
        with(&mut db, "put g {k: $1, v: $2}", &[Value::Int(k), v]).unwrap();
    }
    let r = answer(
        &db,
        "get g group k select k, min(v) as lo, max(v) as hi order k",
        &[],
    );
    let got: Vec<Vec<Value>> = r.rows.iter().map(|r| r.values.clone()).collect();
    assert_eq!(
        got,
        [
            vec![Value::Int(1), Value::Float(P60 as f64), Value::Int(P60 + 1)],
            vec![Value::Int(2), Value::Int(P60 - 1), Value::Float(P60 as f64)],
            vec![Value::Int(3), Value::Int(P60), Value::Int(P60)],
        ]
    );
    let r = answer(
        &db,
        "get g group k select k having max(v) > 1152921504606846976.0",
        &[],
    );
    let got: Vec<Value> = r.rows.iter().map(|r| r.values[0].clone()).collect();
    assert_eq!(got, [Value::Int(1)]);
}

/// `@unique` on a path, and on an index over several fields with a json
/// part, refuses a float equal to an int held, and takes one equal to none.
#[test]
fn a_unique_json_number_is_its_true_value() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection u (m json, a int, @unique(a, m.c));
         create index on u (m.k) @unique",
    );
    let put = |db: &mut Database, k: Value, c: Value| {
        with(db, "put u {a: 1, m: $1}", &[obj(&[("c", &c), ("k", &k)])])
    };
    put(&mut db, Value::Int(P60), Value::Int(1)).unwrap();
    let e = put(&mut db, Value::Float(P60 as f64), Value::Int(2)).unwrap_err();
    assert!(e.to_string().contains("unique"), "{e}");
    put(&mut db, Value::Int(P60 + 1), Value::Int(3)).unwrap();
    put(&mut db, Value::Float(END), Value::Int(4)).unwrap();
    put(&mut db, Value::Int(i64::MAX), Value::Int(5)).unwrap();
    put(&mut db, Value::Int(6), Value::Int(P60)).unwrap();
    let e = put(&mut db, Value::Int(7), Value::Float(P60 as f64)).unwrap_err();
    assert!(e.to_string().contains("unique"), "{e}");
    put(&mut db, Value::Int(8), Value::Int(P60 + 1)).unwrap();
    put(&mut db, Value::Int(9), Value::Float((P60 + 256) as f64)).unwrap();
    let e = put(&mut db, Value::Int(10), Value::Int(P60 + 256)).unwrap_err();
    assert!(e.to_string().contains("unique"), "{e}");
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// An int near a power of two from 2^53 to 2^63, either sign: a few ints
/// off it, or a multiple of the float spacing there off it, give or take
/// one.
fn near(r: &mut Rng) -> i64 {
    let e = 53 + r.below(11) as u32;
    let base: i128 = 1i128 << e;
    let spacing: i128 = 1i128 << (e - 52);
    let off: i128 = match r.below(3) {
        0 => r.below(7) as i128 - 3,
        1 => spacing * (r.below(9) as i128 - 4) + r.below(3) as i128 - 1,
        _ => spacing / 2 * (r.below(5) as i128 - 2),
    };
    let x = if r.below(2) == 0 {
        base + off
    } else {
        -base - off
    };
    x.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// A literal near `i`: the int, a neighbour, the nearest float, the floats
/// either side of it, or one of the edges.
fn literal(r: &mut Rng, i: i64) -> Value {
    let f = i as f64;
    match r.below(8) {
        0 => Value::Int(i),
        1 => Value::Int(i.saturating_add(r.below(3) as i64 - 1)),
        2 | 3 => Value::Float(f),
        4 => Value::Float(f64::from_bits(f.to_bits() + 1)),
        5 => Value::Float(f64::from_bits(f.to_bits() - 1)),
        6 => Value::Float(f + 0.5),
        _ => [
            Value::Float(END),
            Value::Float(-END),
            Value::Float(f64::NAN),
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
        ][r.below(5) as usize]
            .clone(),
    }
}

/// What a random run's json path holds: the int or the float nearest it
/// (`Mixed`), ints no float holds among them, which its ordered index and
/// the index over several fields hold apart and answer nothing while they
/// do; or numbers a float holds alone (`Exact`) -- the int the nearest
/// float is, or that float -- so they answer, and are held to the scan.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Json {
    Mixed,
    Exact,
}

/// The plan `explain` gives for `text`.
fn plan(db: &Database, text: &str, params: &[Value]) -> String {
    format!("{:?}", answer(db, &format!("explain {text}"), params).rows)
}

/// Random ints near the powers of two and random literals near them: every
/// index -- over ints, timestamps, floats (one descending part among them),
/// json paths, an index over several fields with a json part -- answers
/// every comparison and `in` list as the scan does, `NaN` rows among the
/// floats, and a json path holding ints and floats is ordered as the
/// unindexed one is.
fn random_edges(seed: u64, json: Json) {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection t (n int, h int @hash, u int @unique, s int @sorted, a int, z int,
                              tn timestamp, ts timestamp @sorted, th timestamp @hash,
                              m json, @sorted(a, z), @sorted(m.c, z));
         create index on t (m.h) @hash;
         create index on t (m.s) @sorted;
         create collection f (x float, xh float @hash, xs float @sorted, a float, b float,
                              y float, z int, @sorted(a, z), @sorted(b desc, z))",
    );
    let mut r = Rng(seed);
    let mut held: Vec<i64> = Vec::new();
    while held.len() < 160 {
        let i = near(&mut r);
        if held.contains(&i) {
            continue;
        }
        held.push(i);
        let f = i as f64;
        let j = match (json, r.below(2)) {
            (Json::Mixed, 0) => Value::Int(i),
            (Json::Exact, 0) if f < END => Value::Int(f as i64),
            _ => Value::Float(f),
        };
        let m = obj(&[("c", &j), ("h", &j), ("s", &j), ("v", &j)]);
        let text = "put t {n: $1, h: $1, u: $1, s: $1, a: $1, z: 0, tn: $1, ts: $1, th: $1, m: $2}";
        with(&mut db, text, &[Value::Int(i), m]).unwrap();
        let text = "put f {x: $1, xh: $1, xs: $1, a: $1, b: $1, y: $1, z: 0}";
        with(&mut db, text, &[Value::Float(f)]).unwrap();
    }
    // `NaN`, above every number: through the ordered indexes and the
    // `@hash`, whose buckets file every `NaN` under one key, as through
    // the scan.
    for _ in 0..3 {
        let text = "put f {x: $1, xh: $1, xs: $1, a: $1, b: $1, y: $1, z: 0}";
        with(&mut db, text, &[Value::Float(f64::NAN)]).unwrap();
    }
    if json == Json::Exact {
        let some = Value::Int(held[0]);
        let text = "get t where m.s > $1";
        assert!(
            plan(&db, text, one(&some)).contains("ordered index on m.s"),
            "{text}"
        );
        let text = "get t where m.c > $1";
        assert!(
            plan(&db, text, one(&some)).contains("a range on m.c"),
            "{text}"
        );
    }
    let fields = [
        ("t", "n", &["h", "u", "s", "a"][..]),
        ("t", "tn", &["ts", "th"][..]),
        ("t", "m.v", &["m.h", "m.s", "m.c"][..]),
        ("f", "x", &["xs", "a", "b"][..]),
        ("f", "y", &["xh"][..]),
    ];
    for _ in 0..400 {
        let i = held[r.below(held.len() as u64) as usize];
        let v = literal(&mut r, i);
        for op in OPS {
            for (c, scan, indexed) in fields {
                let want = ids(&db, &format!("get {c} where {scan} {op} $1"), one(&v));
                for field in indexed {
                    let text = format!("get {c} where {field} {op} $1");
                    assert_eq!(
                        ids(&db, &text, one(&v)),
                        want,
                        "{json:?}: {text} with {v:?}"
                    );
                }
            }
        }
        let list = Value::List(
            (0..1 + r.below(10))
                .map(|_| {
                    let i = held[r.below(held.len() as u64) as usize];
                    literal(&mut r, i)
                })
                .collect(),
        );
        for (c, scan, indexed) in fields {
            let want = ids(&db, &format!("get {c} where {scan} in $1"), one(&list));
            for field in indexed {
                let text = format!("get {c} where {field} in $1");
                assert_eq!(
                    ids(&db, &text, one(&list)),
                    want,
                    "{json:?}: {text} with {list:?}"
                );
            }
        }
    }
    // The json path in order, its ints and floats together, and every row
    // against every other by the true values.
    let order = ids(&db, "get t order m.v", &[]);
    assert_eq!(ids(&db, "get t order m.s", &[]), order);
    assert_eq!(ids(&db, "get t order m.c, z", &[]), order);
    let r = answer(&db, "get t select m.v order m.v", &[]);
    for w in r.rows.windows(2) {
        let (a, b) = (&w[0].values[0], &w[1].values[0]);
        assert!(a.cmp_value(b).is_le(), "{a:?} before {b:?}");
        if a.cmp_value(b).is_eq() {
            assert!(w[0].id < w[1].id, "{a:?} and {b:?} tie in id order");
        }
    }
    let order = ids(&db, "get t order tn", &[]);
    assert_eq!(ids(&db, "get t order ts", &[]), order);
    if json == Json::Exact {
        let text = "get t order m.s limit 5";
        assert!(plan(&db, text, &[]).contains("walked"), "{text}");
    }
}

#[test]
fn random_numbers_near_the_edges_get_the_scans_answer_through_every_index() {
    random_edges(0x9E37_79B9_7F4A_7C15, Json::Mixed);
}

#[test]
fn random_numbers_a_float_holds_get_the_scans_answer_through_an_answering_json_index() {
    random_edges(0xD1B5_4A32_D192_ED03, Json::Exact);
}

/// A float field's ordered index, and an index over several fields with a
/// float part, answer a comparison with an int no float holds: it lies
/// between two floats with none between them, which bound it -- tested row
/// by row, `xs > 9007199254740993` read every row.
#[test]
fn an_int_no_float_holds_is_bounded_by_the_floats_beside_it() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection f (x float, xs float @sorted, a float, b float, z int,
                              @sorted(a, z), @sorted(b desc, z), m json);
         create index on f (m.s) @sorted",
    );
    let mut fs = Vec::new();
    for e in [53, 54, 60, 62] {
        for sign in [1.0, -1.0] {
            let p = sign * 2f64.powi(e);
            for d in 0..4u64 {
                fs.push(f64::from_bits(p.to_bits() + d));
                fs.push(f64::from_bits(p.to_bits() - d));
            }
        }
    }
    fs.extend([END, -END, 0.0]);
    fs.sort_by(f64::total_cmp);
    fs.dedup();
    for f in &fs {
        let m = obj(&[("s", &Value::Float(*f)), ("v", &Value::Float(*f))]);
        let text = "put f {x: $1, xs: $1, a: $1, b: $1, z: 0, m: $2}";
        with(&mut db, text, &[Value::Float(*f), m]).unwrap();
    }
    let nan = "put f {x: $1, xs: $1, a: $1, b: $1, z: 0}";
    with(&mut db, nan, &[Value::Float(f64::NAN)]).unwrap();
    let mut literals = Vec::new();
    for e in [53, 54, 60, 62] {
        for d in [1i64, 3, 5, 7, -1, -3] {
            for sign in [1i64, -1] {
                // Short of 2^53 every int is a float.
                let i = sign * ((1i64 << e) + d);
                if (i as f64) as i64 != i {
                    literals.push(Value::Int(i));
                }
            }
        }
    }
    assert_eq!(literals.len(), 44);
    literals.extend([Value::Int(i64::MAX), Value::Int(i64::MIN + 1)]);
    for v in &literals {
        for op in OPS {
            let want = ids(&db, &format!("get f where x {op} $1"), one(v));
            for field in ["xs", "a", "b"] {
                let text = format!("get f where {field} {op} $1");
                assert_eq!(ids(&db, &text, one(v)), want, "{text} with {v:?}");
            }
            // The json path holds no `NaN`: its index answers alone.
            let want = ids(&db, &format!("get f where m.v {op} $1"), one(v));
            let text = format!("get f where m.s {op} $1");
            assert_eq!(ids(&db, &text, one(v)), want, "{text} with {v:?}");
        }
    }
    let p = plan(&db, "get f where xs > 9007199254740993", &[]);
    assert!(
        p.contains("ordered index on xs") && p.contains("which is the answer"),
        "{p}"
    );
    let p = plan(&db, "get f where m.s <= -1152921504606846977", &[]);
    assert!(
        p.contains("ordered index on m.s") && p.contains("which is the answer"),
        "{p}"
    );
    for field in ["a", "b"] {
        let p = plan(
            &db,
            &format!("get f where {field} >= 9007199254740993"),
            &[],
        );
        assert!(p.contains(&format!("a range on {field}")), "{p}");
    }
    // `=` is a range of no key: no row, the `NaN` one neither, as the scan
    // has it.
    let text = "get f where xs = 9007199254740993";
    assert_eq!(
        ids(&db, text, &[]),
        ids(&db, "get f where x = 9007199254740993", &[])
    );
    assert_eq!(ids(&db, text, &[]).len(), 0);
    // A facet's ranges by such ints count what the scan counts.
    let text = "get f facet xs ranges [9007199254740993, 1152921504606846977] limit 0";
    let r = answer(&db, text, &[]);
    let scan = answer(
        &db,
        "get f facet x ranges [9007199254740993, 1152921504606846977] limit 0",
        &[],
    );
    assert_eq!(r.facets[0].values, scan.facets[0].values);
    assert!(r.facets[0].values[0].1 > 0, "{:?}", r.facets);
}

/// A number read back through JSON -- a row as a server writes it, sent
/// back as a parameter, or its digits written into a statement -- finds
/// the row it was read from, through the scan and every index, in every
/// kind of field a number goes in. The float 2^60 went out as its shortest
/// digits, `1152921504606847000`, which read back as another int and found
/// none of its rows.
#[test]
fn a_number_read_back_through_json_finds_its_row() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection f (x float, xh float @hash, xs float @sorted, a float,
                              n int, nh int @hash, ns int @sorted,
                              t timestamp, th timestamp @hash, ts timestamp @sorted,
                              m json, z int, @sorted(a, z));
         create index on f (m.h) @hash;
         create index on f (m.s) @sorted",
    );
    let floats = [
        P60 as f64,
        -(P60 as f64),
        (P53 + 2) as f64,
        (P60 + 256) as f64,
        END - 1024.0,
        END,
        -END,
        1e300,
        1e15 + 1.0,
        3.0,
        0.5,
        -0.0,
    ];
    for f in floats {
        let v = Value::Float(f);
        let m = obj(&[("h", &v), ("s", &v), ("v", &v)]);
        for _ in 0..2 {
            let text = "put f {x: $1, xh: $1, xs: $1, a: $1, z: 0, m: $2}";
            with(&mut db, text, &[v.clone(), m.clone()]).unwrap();
        }
    }
    for i in [P60, P60 + 1, P53 + 1, i64::MAX, i64::MIN, -P60 - 1, 3] {
        let v = Value::Int(i);
        let m = obj(&[("h", &v), ("s", &v), ("v", &v)]);
        for _ in 0..2 {
            let text = "put f {n: $1, nh: $1, ns: $1, z: 0, m: $2}";
            with(&mut db, text, &[v.clone(), m.clone()]).unwrap();
        }
    }
    // A time goes out as its ISO text, which reads back as it from year
    // -9999 to 9999.
    for ms in [0, -1, 1_700_000_000_123, 253_402_300_799_999] {
        for _ in 0..2 {
            let text = "put f {t: $1, th: $1, ts: $1, z: 0}";
            with(&mut db, text, &[Value::Int(ms)]).unwrap();
        }
    }
    // The rows as `get` answers them over HTTP, read off the stored
    // documents, and as `query` answers them: the same text.
    let get = fennec_ql::parse_one("get f").unwrap();
    let mut text = String::new();
    assert!(db.query_json(&get, &[], &mut text).unwrap().is_some());
    let mut answered = String::new();
    fennec_core::json::rows_array_into(&mut answered, &answer(&db, "get f", &[]));
    assert_eq!(text, answered);
    let docs = fennec_core::json::parse_documents(&text).unwrap();
    let mut checked = 0;
    for doc in &docs {
        let id = match doc.iter().find(|(k, _)| k == "id") {
            Some((_, Value::Int(id))) => *id as u64,
            other => panic!("{other:?}"),
        };
        let mut found = Vec::new();
        for (k, v) in doc {
            match (k.as_str(), v) {
                ("id" | "z", _) | (_, Value::Null) => {}
                ("m", Value::Object(members)) => {
                    found.extend(members.iter().map(|(p, v)| (format!("m.{p}"), v.clone())))
                }
                (k, v) => found.push((k.to_string(), v.clone())),
            }
        }
        for (field, v) in found {
            let digits = fennec_core::json::to_string(&v);
            // Sent back as a client sends a parameter, read as a server
            // reads one, and the value read back itself.
            let sent = fennec_core::json::parse_params_exact(&format!("[{digits}]")).unwrap();
            for p in [v.clone(), sent[0].clone()] {
                let q = format!("get f where {field} = $1");
                let got = ids(&db, &q, one(&p));
                assert!(
                    got.contains(&id) && got.len() >= 2,
                    "{q} with {digits}: {got:?}"
                );
            }
            // Written into a statement: an int past an `i64` is refused
            // there, never read as another number.
            let q = format!("get f where {field} = {digits}");
            if let Ok(s) = fennec_ql::parse_one(&q) {
                let got: Vec<u64> = db
                    .query(&s, &[])
                    .unwrap()
                    .rows()
                    .unwrap()
                    .rows
                    .iter()
                    .map(|r| r.id)
                    .collect();
                assert!(got.contains(&id) && got.len() >= 2, "{q}: {got:?}");
            }
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}
