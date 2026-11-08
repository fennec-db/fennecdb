//! `NaN` has one place in every order, PostgreSQL's: it equals `NaN`,
//! whatever its bits, and is greater than every other number, `+inf`
//! included. It equalled every value before, so `NaN = 3`, `NaN = 5` and
//! `3 < 5` held at once: an `order` over a float field holding one could
//! panic in the standard library's sort, a float field's `@hash` answered
//! `x = 5.0` without the `NaN` rows the scan took, and a `lookup` by a
//! `NaN` key found no child.

use fennec_core::prelude::*;
use std::cmp::Ordering;

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

fn ids(db: &Database, text: &str, params: &[Value]) -> Vec<u64> {
    answer(db, text, params).rows.iter().map(|r| r.id).collect()
}

fn one(v: &Value) -> &[Value] {
    std::slice::from_ref(v)
}

/// The plan `explain` gives for `text`.
fn plan(db: &Database, text: &str, params: &[Value]) -> String {
    format!("{:?}", answer(db, &format!("explain {text}"), params).rows)
}

/// `NaN`s of other bits: the one Rust makes, its negative (what `0.0 /
/// 0.0` gives on x86), one with a payload and a signalling one.
const NANS: [u64; 4] = [
    0x7FF8_0000_0000_0000,
    0xFFF8_0000_0000_0000,
    0x7FF8_0000_0000_0001,
    0xFFF0_0000_0000_0001,
];

fn nan(i: usize) -> f64 {
    f64::from_bits(NANS[i % NANS.len()])
}

/// 2^63, which no int is.
const END: f64 = 9_223_372_036_854_775_808.0;

/// PostgreSQL's order of two floats, worked out apart from `cmp_value`:
/// `NaN` equal to `NaN` and above the rest, `-0.0` equal to `0.0`.
fn pg(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => a.partial_cmp(&b).unwrap(),
    }
}

/// A float field's value or `null`, as an order meets it: `null` first.
fn pg_value(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Float(x), Value::Float(y)) => pg(*x, *y),
        _ => panic!("{a:?} {b:?}"),
    }
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

/// A float at an edge: a `NaN` of any bits, an infinity, a zero of either
/// sign, the ends of the finite floats, a power of two from 2^53 to 2^63
/// or a float beside one, or a small number.
fn edge(r: &mut Rng) -> f64 {
    let sign = if r.below(2) == 0 { 1.0 } else { -1.0 };
    match r.below(8) {
        0 | 1 => nan(r.below(4) as usize),
        2 => sign * f64::INFINITY,
        3 => sign * 0.0,
        4 => [f64::MAX, f64::MIN_POSITIVE, END, 1e300][r.below(4) as usize] * sign,
        5 | 6 => {
            let p = sign * 2f64.powi(53 + r.below(11) as i32);
            f64::from_bits(p.to_bits() + r.below(3) - 1)
        }
        _ => sign * (r.below(7) as f64) / 2.0,
    }
}

/// A literal near the edges: a float as [`edge`] makes them, an int near
/// 2^53 to 2^63, one at the ends or a small one.
fn literal(r: &mut Rng) -> Value {
    match r.below(5) {
        0..=2 => Value::Float(edge(r)),
        3 => {
            let sign = if r.below(2) == 0 { 1 } else { -1 };
            let p = 1i64 << (53 + r.below(10));
            Value::Int(sign * (p + r.below(5) as i64 - 2))
        }
        _ => [
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
            Value::Int(3),
            Value::Int(0),
        ][r.below(4) as usize]
            .clone(),
    }
}

const OPS: [&str; 6] = ["=", "!=", "<", "<=", ">", ">="];

/// A float field holding `NaN`s of every bit pattern beside the other
/// edges, through the scan (`x`), a `@hash` (`xh`), an ordered index
/// (`xs`) and an index over several fields ascending (`a`) and descending
/// (`b`); an int field and a timestamp one, which a `NaN` literal meets;
/// and a json field, whose numbers are finite.
fn edges_db(seed: u64, rows: usize) -> (Database, Vec<f64>) {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection f (x float, xh float @hash, xs float @sorted, a float, b float,
                              z int, @sorted(a, z), @sorted(b desc, z));
         create collection t (n int, nh int @hash, ns int @sorted, c int, z int,
                              tn timestamp, ts timestamp @sorted, th timestamp @hash,
                              m json, @sorted(c, z), @sorted(m.c, z));
         create index on t (m.h) @hash;
         create index on t (m.s) @sorted",
    );
    let mut r = Rng(seed);
    let mut held = Vec::new();
    for i in 0..rows {
        // A row in twenty holds no value: `null`, below every value.
        if i % 20 == 7 {
            run(&mut db, "put f {z: 0}");
            continue;
        }
        let x = edge(&mut r);
        held.push(x);
        let text = "put f {x: $1, xh: $1, xs: $1, a: $1, b: $1, z: 0}";
        with(&mut db, text, &[Value::Float(x)]).unwrap();
    }
    for _ in 0..rows / 4 {
        let n = match literal(&mut r) {
            Value::Int(n) => n,
            Value::Float(_) => r.next() as i64 >> r.below(64),
            v => panic!("{v:?}"),
        };
        // A json number is finite: the float nearest the int, or the int.
        let j = match r.below(2) {
            0 => Value::Int(n),
            _ => Value::Float(n as f64),
        };
        let m = Value::Object(
            ["c", "h", "s", "v"]
                .iter()
                .map(|k| (k.to_string(), j.clone()))
                .collect(),
        );
        let text = "put t {n: $1, nh: $1, ns: $1, c: $1, z: 0, tn: $1, ts: $1, th: $1, m: $2}";
        with(&mut db, text, &[Value::Int(n), m]).unwrap();
    }
    (db, held)
}

/// Each comparison, `between` and `in` list against random literals near
/// the edges, `NaN`s of every bits among them: every index answers as the
/// scan does, row for row, and the scan over the float field as
/// PostgreSQL's order says.
fn every_index_as_the_scan(seed: u64) {
    let (db, _) = edges_db(seed, 400);
    let fields = [
        ("f", "x", &["xh", "xs", "a", "b"][..]),
        ("t", "n", &["nh", "ns", "c"][..]),
        ("t", "tn", &["ts", "th"][..]),
        ("t", "m.v", &["m.h", "m.s", "m.c"][..]),
    ];
    let all = answer(&db, "get f select x", &[]);
    let mut r = Rng(seed ^ 0x5555);
    for _ in 0..150 {
        let v = literal(&mut r);
        for op in OPS {
            for (c, scan, indexed) in fields {
                let want = ids(&db, &format!("get {c} where {scan} {op} $1"), one(&v));
                for field in indexed {
                    let text = format!("get {c} where {field} {op} $1");
                    assert_eq!(ids(&db, &text, one(&v)), want, "{text} with {v:?}");
                }
            }
            // The scan over floats, by PostgreSQL's order.
            if let Value::Float(l) = v {
                let want: Vec<u64> = all
                    .rows
                    .iter()
                    .filter(|row| match &row.values[0] {
                        Value::Float(x) => {
                            let o = pg(*x, l);
                            match op {
                                "=" => o.is_eq(),
                                "!=" => o.is_ne(),
                                "<" => o.is_lt(),
                                "<=" => o.is_le(),
                                ">" => o.is_gt(),
                                _ => o.is_ge(),
                            }
                        }
                        // A `null` is unequal to every value, and in no
                        // order.
                        _ => op == "!=",
                    })
                    .map(|row| row.id)
                    .collect();
                let text = format!("get f where x {op} $1");
                assert_eq!(ids(&db, &text, one(&v)), want, "{text} with {v:?}");
            }
        }
        let w = literal(&mut r);
        let both = [v.clone(), w.clone()];
        for (c, scan, indexed) in fields {
            let want = ids(
                &db,
                &format!("get {c} where {scan} between $1 and $2"),
                &both,
            );
            for field in indexed {
                let text = format!("get {c} where {field} between $1 and $2");
                assert_eq!(ids(&db, &text, &both), want, "{text} with {both:?}");
            }
        }
        let list = Value::List((0..1 + r.below(6)).map(|_| literal(&mut r)).collect());
        for (c, scan, indexed) in fields {
            let want = ids(&db, &format!("get {c} where {scan} in $1"), one(&list));
            for field in indexed {
                let text = format!("get {c} where {field} in $1");
                assert_eq!(ids(&db, &text, one(&list)), want, "{text} with {list:?}");
            }
        }
    }
}

#[test]
fn every_index_answers_a_nan_as_the_scan_does() {
    every_index_as_the_scan(0x9E37_79B9_7F4A_7C15);
}

#[test]
fn every_index_answers_a_nan_as_the_scan_does_another_draw() {
    every_index_as_the_scan(0xD1B5_4A32_D192_ED03);
}

/// An order over a float field holding `NaN`s -- by the scan's sort, the
/// ordered index's walk and an index over several fields either way --
/// never panics and is PostgreSQL's: `null` first, then the numbers,
/// then every `NaN`, ties in id order, ascending and descending, with a
/// `limit` and an `offset`. The standard library's sort panicked over
/// one: "user-provided comparison function does not correctly implement
/// a total order".
#[test]
fn an_order_over_nan_is_a_total_order() {
    for seed in [0xA5A5_1234_5678_9ABC, 0x0123_4567_89AB_CDEF, 7] {
        let (db, _) = edges_db(seed, 2_000);
        let all = answer(&db, "get f select x", &[]);
        let mut rows: Vec<(Value, u64)> = all
            .rows
            .iter()
            .map(|r| (r.values[0].clone(), r.id))
            .collect();
        rows.sort_by(|a, b| pg_value(&a.0, &b.0).then(a.1.cmp(&b.1)));
        let asc: Vec<u64> = rows.iter().map(|r| r.1).collect();
        rows.sort_by(|a, b| pg_value(&b.0, &a.0).then(a.1.cmp(&b.1)));
        let desc: Vec<u64> = rows.iter().map(|r| r.1).collect();
        for (field, rest) in [("x", ""), ("xs", ""), ("a", ", z"), ("xh", "")] {
            let text = format!("get f order {field}{rest}");
            assert_eq!(ids(&db, &text, &[]), asc, "{text}");
            let text = format!("get f order {field} desc{rest}");
            assert_eq!(ids(&db, &text, &[]), desc, "{text}");
            for (limit, offset) in [(1, 0), (7, 0), (50, 3), (40, asc.len() - 30)] {
                let text = format!("get f order {field}{rest} limit {limit} offset {offset}");
                let want = &asc[offset.min(asc.len())..(offset + limit).min(asc.len())];
                assert_eq!(ids(&db, &text, &[]), want, "{text}");
                let text = format!("get f order {field} desc{rest} limit {limit} offset {offset}");
                let want = &desc[offset.min(desc.len())..(offset + limit).min(desc.len())];
                assert_eq!(ids(&db, &text, &[]), want, "{text}");
            }
        }
        let text = "get f order b desc, z";
        assert_eq!(ids(&db, text, &[]), desc, "{text}");
        // The ordered index holds a `NaN` in its place and is walked.
        for text in ["get f order xs limit 5", "get f order xs desc limit 5"] {
            assert!(plan(&db, text, &[]).contains("walked"), "{text}");
        }
    }
}

/// `NaN` is one value wherever values are told apart or found by their
/// key, whatever its bits: one bucket of a `@hash`, one value to a
/// `@unique` and to an index over several fields, one group, one value
/// counted and sketched, one facet value -- through the buckets and the
/// scan -- the `max`, and the key a `lookup` finds its children by.
#[test]
fn nan_is_one_value_whatever_its_bits() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection f (x float, xh float @hash, k text);
         create collection u (x float @unique);
         create collection w (x float, z int, @unique(x, z));
         create collection par (k float, j int);
         create collection ch (pf float @hash);
         create collection cs (pf float @sorted);
         create collection cc (pf float, z int, @sorted(pf, z))",
    );
    for i in 0..8 {
        let x = Value::Float(nan(i));
        let k = Value::Text(["a", "b"][i % 2].into());
        with(&mut db, "put f {x: $1, xh: $1, k: $2}", &[x.clone(), k]).unwrap();
        for c in ["ch", "cs"] {
            with(&mut db, &format!("put {c} {{pf: $1}}"), one(&x)).unwrap();
        }
        with(&mut db, "put cc {pf: $1, z: 1}", one(&x)).unwrap();
    }
    for x in [1.5, f64::INFINITY, -0.0, 0.0] {
        let x = Value::Float(x);
        let k = Value::Text("a".into());
        with(&mut db, "put f {x: $1, xh: $1, k: $2}", &[x.clone(), k]).unwrap();
        for c in ["ch", "cs"] {
            with(&mut db, &format!("put {c} {{pf: $1}}"), one(&x)).unwrap();
        }
        with(&mut db, "put cc {pf: $1, z: 1}", one(&x)).unwrap();
    }
    // Every `NaN` finds the eight, through the bucket as through the scan.
    for i in 0..NANS.len() {
        let v = Value::Float(nan(i));
        for field in ["x", "xh"] {
            let text = format!("get f where {field} = $1");
            assert_eq!(
                ids(&db, &text, one(&v)),
                (1..=8).collect::<Vec<_>>(),
                "{text}"
            );
            let text = format!("get f where {field} in $1");
            let list = Value::List(vec![Value::Float(2.0), v.clone()]);
            assert_eq!(ids(&db, &text, one(&list)).len(), 8, "{text}");
        }
        let text = "get f where xh = $1";
        assert!(plan(&db, text, one(&v)).contains("hash index"), "{text}");
    }
    // `-0.0` stays `0.0`: one value, two rows.
    let zero = Value::Float(0.0);
    assert_eq!(ids(&db, "get f where xh = $1", one(&zero)).len(), 2);
    assert_eq!(ids(&db, "get f where x = $1", one(&zero)).len(), 2);
    // One group, one distinct value, one sketched, one facet value.
    let groups = answer(&db, "get f select x, count(*) as n group x", &[]);
    let got: Vec<(Value, Value)> = groups
        .rows
        .iter()
        .map(|r| (r.values[0].clone(), r.values[1].clone()))
        .collect();
    assert_eq!(got.len(), 4, "{got:?}");
    let nans: Vec<_> = got
        .iter()
        .filter(|g| matches!(g.0, Value::Float(x) if x.is_nan()))
        .collect();
    assert_eq!(nans.len(), 1, "{got:?}");
    assert_eq!(nans[0].1, Value::Int(8), "{got:?}");
    let two = answer(&db, "get f select k, x, count(*) as n group k, x", &[]);
    assert_eq!(two.rows.len(), 5, "{:?}", two.rows);
    for text in [
        "get f select count(distinct x) as n",
        "get f select approx_count_distinct(x) as n",
        "get f select hll_estimate(hll_accumulate(x)) as n",
    ] {
        let r = answer(&db, text, &[]);
        assert_eq!(r.rows[0].values, [Value::Int(4)], "{text}");
    }
    for field in ["x", "xh"] {
        let r = answer(&db, &format!("get f facet {field} limit 0"), &[]);
        let values = &r.facets[0].values;
        assert_eq!(values.len(), 4, "{field}: {values:?}");
        assert!(
            matches!(values[0], (Value::Float(x), 8) if x.is_nan()),
            "{values:?}"
        );
    }
    // The largest is a `NaN`, the least the least number, as in
    // PostgreSQL; `having` keeps the groups holding one.
    let r = answer(&db, "get f select min(x) as lo, max(x) as hi", &[]);
    assert_eq!(r.rows[0].values[0], Value::Float(-0.0));
    assert!(matches!(r.rows[0].values[1], Value::Float(x) if x.is_nan()));
    let r = answer(
        &db,
        "get f group k select k having max(x) > 1e308 order k",
        &[],
    );
    assert_eq!(r.rows.len(), 2, "{:?}", r.rows);
    // `greatest` and `least` too.
    let r = answer(
        &db,
        "get f where x = 1.5 select greatest(x, $1) as g, least(x, $1) as l",
        one(&Value::Float(nan(1))),
    );
    assert!(matches!(r.rows[0].values[0], Value::Float(x) if x.is_nan()));
    assert_eq!(r.rows[0].values[1], Value::Float(1.5));
    // `@unique` and an index over several fields refuse a second `NaN`,
    // of other bits.
    with(&mut db, "put u {x: $1}", &[Value::Float(nan(0))]).unwrap();
    let e = with(&mut db, "put u {x: $1}", &[Value::Float(nan(1))]).unwrap_err();
    assert!(e.to_string().contains("unique"), "{e}");
    with(&mut db, "put w {x: $1, z: 1}", &[Value::Float(nan(2))]).unwrap();
    let e = with(&mut db, "put w {x: $1, z: 1}", &[Value::Float(nan(3))]).unwrap_err();
    assert!(e.to_string().contains("unique"), "{e}");
    with(&mut db, "put w {x: $1, z: 2}", &[Value::Float(nan(3))]).unwrap();
    // A parent's `NaN` key finds every `NaN` child, whatever its bits,
    // through a `@hash`, an ordered index and an index over two fields.
    for bits in NANS {
        let k = Value::Float(f64::from_bits(bits));
        run(&mut db, "del par");
        with(&mut db, "put par {k: $1, j: 1}", one(&k)).unwrap();
        for (text, want) in [
            ("get par lookup ch on pf = k", 8),
            ("get par lookup ch on pf = k required", 8),
            ("get par lookup cc on (pf, z) = (k, j)", 8),
        ] {
            let r = answer(&db, text, &[]);
            let got = r.nested.as_ref().map_or(0, |n| n.rows().count());
            assert_eq!(got, want, "{text} with {bits:x}");
        }
        let want = ids(&db, "get ch where pf = $1", one(&k));
        assert_eq!(want.len(), 8);
        let got = ids(&db, "get cs where pf = $1", one(&k));
        assert_eq!(got, want);
    }
}

/// A float field's ordered index is a range above every number for
/// `NaN`: `> 5` takes the `NaN` rows, as PostgreSQL's `x > 5` does, and
/// `< 'infinity'` leaves them out -- through the index, its range said as
/// keys.
#[test]
fn a_range_above_every_number_holds_the_nans() {
    fennec_ql::install();
    let mut db = Database::new();
    run(&mut db, "create collection f (x float, xs float @sorted)");
    for x in [nan(0), 3.0, 7.0, f64::INFINITY, nan(1), f64::NEG_INFINITY] {
        with(&mut db, "put f {x: $1, xs: $1}", &[Value::Float(x)]).unwrap();
    }
    // Rows below the ranges, so that the index names fewer than the scan.
    for i in 0..30 {
        let x = Value::Float(-1.0 - i as f64);
        with(&mut db, "put f {x: $1, xs: $1}", one(&x)).unwrap();
    }
    let below: Vec<u64> = (7..=36).collect();
    for field in ["x", "xs"] {
        let text = format!("get f where {field} > 5");
        assert_eq!(ids(&db, &text, &[]), [1, 3, 4, 5], "{text}");
        let text = format!("get f where {field} < $1 and {field} > 2");
        let inf = Value::Float(f64::INFINITY);
        assert_eq!(ids(&db, &text, one(&inf)), [2, 3], "{text}");
        let text = format!("get f where {field} >= $1");
        let v = Value::Float(nan(2));
        assert_eq!(ids(&db, &text, one(&v)), [1, 5], "{text}");
        let text = format!("get f where {field} > $1");
        assert_eq!(ids(&db, &text, one(&v)), [0u64; 0], "{text}");
        let text = format!("get f where {field} != $1");
        let want: Vec<u64> = [2, 3, 4, 6]
            .into_iter()
            .chain(below.iter().copied())
            .collect();
        assert_eq!(ids(&db, &text, one(&v)), want, "{text}");
    }
    let text = "get f where xs > 5";
    let p = plan(&db, text, &[]);
    assert!(
        p.contains("ordered index on xs") && p.contains("which is the answer"),
        "{p}"
    );
    // `count` over the range, from the index alone.
    let r = answer(&db, "get f where xs >= 7 count", &[]);
    assert_eq!(r.rows[0].values, [Value::Int(4)]);
}
