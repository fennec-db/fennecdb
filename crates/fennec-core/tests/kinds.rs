//! Values of two kinds order by kind, but for the numbers -- an int, a
//! float and a time, by their true values -- and a text and bytes, by their
//! bytes (`Value::cmp_value`). A time equalled a text that read as it
//! before, while the int equal to the time ranked below that text: no
//! order, and `order coalesce(t, j)` over a time field and a json one
//! holding ints and ISO texts panicked in the standard library's sort. A
//! text written beside a time -- a literal, a parameter -- is read as the
//! time it names before a row is compared, as PostgreSQL reads a literal of
//! no type, so `where t >= "2026-01-01"` still compares times. And a group,
//! a distinct count, a facet and a sketch tell apart only what `=` does:
//! a json field's `3` and `3.0` were two groups.

use fennec_core::prelude::*;
use fennec_core::time::format_iso;
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
    let mut out: Vec<u64> = answer(db, text, params).rows.iter().map(|r| r.id).collect();
    out.sort_unstable();
    out
}

fn count(db: &Database, text: &str) -> Value {
    answer(db, text, &[]).rows[0].values[0].clone()
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

/// 2026-01-01T00:00:00Z.
const MS: i64 = 1_767_225_600_000;

/// Where a value of the order below goes, worked out apart from
/// `cmp_value`: `null` first, then the numbers by value -- an int and a
/// time as their milliseconds, a half as itself, twice over so all are
/// ints -- then the texts by their bytes.
fn place(v: &Value) -> (u8, i128, Vec<u8>) {
    match v {
        Value::Null => (0, 0, vec![]),
        Value::Int(i) | Value::Timestamp(i) => (2, 2 * *i as i128, vec![]),
        Value::Float(f) => {
            assert_eq!(f.fract(), 0.5, "{f}");
            (2, (2.0 * f) as i128, vec![])
        }
        Value::Text(t) => (3, 0, t.as_bytes().to_vec()),
        v => panic!("{v:?}"),
    }
}

/// `order coalesce(t, j)` over a time field and a json one holding ints,
/// halves and the ISO texts of the same instants: the panic's rows, of
/// 1 191 to 2 530 rows a run. Every run answers, in the order worked out
/// apart -- every number below every text, ties in id order -- both ways.
#[test]
fn an_order_over_a_time_and_json_texts_is_one_order() {
    fennec_ql::install();
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    for run_no in 0..60 {
        let mut db = Database::new();
        run(&mut db, "create collection e (t timestamp, j json)");
        let n = 1_191 + r.below(1_340);
        let mut docs = Vec::new();
        for _ in 0..n {
            let at = MS + r.below(20) as i64 - 10;
            let t = match r.below(3) {
                0 => Value::Timestamp(at),
                _ => Value::Null,
            };
            let j = match r.below(5) {
                0 => Value::Int(at),
                1 => Value::Float(at as f64 + 0.5),
                2 => Value::Text(format_iso(at)),
                3 => Value::Text(fennec_core::time::format_pg(at)),
                _ => Value::Null,
            };
            docs.push(Value::Object(vec![("j".into(), j), ("t".into(), t)]));
        }
        with(&mut db, "put e $1", &[Value::List(docs)]).unwrap();
        for (dir, flip) in [("", false), (" desc", true)] {
            let text = format!("get e select t, j order coalesce(t, j){dir}");
            let rows = answer(&db, &text, &[]).rows;
            assert_eq!(rows.len() as u64, n, "{text}");
            let key = |row: &Row| match &row.values[0] {
                Value::Null => row.values[1].clone(),
                t => t.clone(),
            };
            let mut want: Vec<(Value, u64)> = rows.iter().map(|r| (key(r), r.id)).collect();
            want.sort_by(|a, b| {
                let o = place(&a.0).cmp(&place(&b.0));
                let o = if flip { o.reverse() } else { o };
                o.then(a.1.cmp(&b.1))
            });
            let got: Vec<u64> = rows.iter().map(|r| r.id).collect();
            let want: Vec<u64> = want.iter().map(|w| w.1).collect();
            assert_eq!(got, want, "run {run_no}: {text}");
        }
    }
}

/// Whether an operator holds of an order.
type Holds = fn(Ordering) -> bool;

const OPS: [(&str, Holds); 6] = [
    ("=", |o| o == Ordering::Equal),
    ("!=", |o| o != Ordering::Equal),
    ("<", |o| o == Ordering::Less),
    ("<=", |o| o != Ordering::Greater),
    (">", |o| o == Ordering::Greater),
    (">=", |o| o != Ordering::Less),
];

/// A text written beside a time field -- a literal, a parameter, an item
/// of `in`, `has`'s item over a list of times -- is read as the time it
/// names, through the scan, a `@hash`, a `@sorted`, an index over several
/// fields and an expression alike; a text naming no time is above every
/// time. A text read off a row is no literal: a json field's ISO text is a
/// text, and equals no time.
#[test]
fn a_text_written_beside_a_time_is_read_as_the_time() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection w (t timestamp, th timestamp @hash, ts timestamp @sorted, \
         tc timestamp, z int, tl [timestamp], j json, @sorted(tc, z))",
    );
    let at = |i: i64| MS + i * 500;
    for i in 0..12 {
        let t = match i {
            11 => Value::Null,
            i => Value::Timestamp(at(i)),
        };
        let doc = "put w {t: $1, th: $1, ts: $1, tc: $1, z: 1, tl: $2, j: $3}";
        let list = Value::List(vec![t.clone(), Value::Timestamp(at(20))]);
        let j = Value::Text(format_iso(at(i)));
        with(&mut db, doc, &[t, list, j]).unwrap();
    }
    let fields = ["t", "th", "ts", "tc", "t + 0"];
    // The texts a time is written as, each the time `at(4)` but the last
    // two, which read as no time.
    let iso = format_iso(at(4));
    let texts = [
        iso.clone(),
        iso.replace('T', " "),
        iso.replace('Z', "+00:00"),
        fennec_core::time::format_pg(at(4)),
        "abc".to_string(),
        String::new(),
    ];
    for written in &texts {
        let time = fennec_core::time::parse(written).ok();
        for (op, holds) in OPS {
            // A time below every text naming none; row 12 holds no time,
            // a `null`, which is only ever unequal.
            let want: Vec<u64> = (1..=12u64)
                .filter(|&id| match (id, time) {
                    (12, _) => op == "!=",
                    (_, Some(ms)) => holds(at(id as i64 - 1).cmp(&ms)),
                    (_, None) => holds(Ordering::Less),
                })
                .collect();
            for f in fields {
                let lit = format!("get w where {f} {op} {}", quoted(written));
                assert_eq!(ids(&db, &lit, &[]), want, "{lit}");
                let param = format!("get w where {f} {op} $1");
                let p = [Value::Text(written.clone())];
                assert_eq!(ids(&db, &param, &p), want, "{param} {written}");
                let turned = format!("get w where $1 {} {f}", turn(op));
                assert_eq!(ids(&db, &turned, &p), want, "{turned} {written}");
            }
        }
        let want = match time {
            Some(_) => vec![5u64],
            None => vec![],
        };
        for f in fields {
            let text = format!("get w where {f} in [{}, \"zzz\"]", quoted(written));
            assert_eq!(ids(&db, &text, &[]), want, "{text}");
            let text = format!("get w where {f} in $1");
            let list = Value::List(vec![Value::Text(written.clone())]);
            assert_eq!(ids(&db, &text, &[list]), want, "{text} {written}");
        }
        let text = format!("get w where tl has {}", quoted(written));
        assert_eq!(ids(&db, &text, &[]), want, "{text}");
        let text = format!("get w where coalesce(tl, []) has {}", quoted(written));
        assert_eq!(ids(&db, &text, &[]), want, "{text}");
    }
    // A long list, which a filter looks a row up in.
    let long: Vec<Value> = (0..20).map(|i| Value::Text(format_iso(at(i)))).collect();
    for f in ["t", "th", "ts", "tc"] {
        let text = format!("get w where {f} in $1");
        let got = ids(&db, &text, &[Value::List(long.clone())]);
        assert_eq!(got, (1..=11).collect::<Vec<u64>>(), "{text}");
    }
    // A json field's ISO text is a text: no time equals it, nor does a
    // time order beside it as the time it reads as.
    assert_eq!(ids(&db, "get w where t = j", &[]), Vec::<u64>::new());
    assert_eq!(
        ids(&db, "get w where t < j", &[]),
        (1..=11).collect::<Vec<_>>()
    );
    let now = [Value::Timestamp(at(4))];
    assert_eq!(ids(&db, "get w where j = $1", &now), Vec::<u64>::new());
    // `case` and `having` read a written text as a time too.
    let text = format!(
        "get w where t != null select case when t >= {} then 1 else 0 end as late",
        quoted(&iso)
    );
    let late: i64 = answer(&db, &text, &[])
        .rows
        .iter()
        .map(|r| match r.values[0] {
            Value::Int(n) => n,
            ref v => panic!("{v:?}"),
        })
        .sum();
    assert_eq!(late, 7, "{text}");
    let text = format!(
        "get w group z select z, max(t) as m having max(t) > {}",
        quoted(&iso)
    );
    assert_eq!(answer(&db, &text, &[]).rows.len(), 1, "{text}");
}

fn quoted(t: &str) -> String {
    format!("\"{t}\"")
}

/// `a op b` as `b turned a`.
fn turn(op: &str) -> &str {
    match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        o => o,
    }
}

/// What `=` finds equal is one group, one distinct value, one sketched,
/// one facet value and one `@unique` value: a json field's `3` and `3.0`,
/// `[3]` and `[3.0]`, a time and the int of its milliseconds, bytes and the
/// text of them. Through a `@hash` too, and a json `@hash` finds a time's
/// numbers, as the scan does, and never a text reading as it.
#[test]
fn what_equals_is_one_group() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection g (j json, jh json @hash, k text);
         create collection u (j json @unique);
         create collection cu (j json, z int, @unique(j, z));
         create collection x (t timestamp, b bytes, s text)",
    );
    let values = [
        Value::Int(3),
        Value::Float(3.0),
        Value::Float(3.5),
        Value::List(vec![Value::Int(3)]),
        Value::List(vec![Value::Float(3.0)]),
        Value::Text("3".into()),
        Value::Int(MS),
        Value::Float(MS as f64),
        Value::Text(format_iso(MS)),
        Value::Null,
    ];
    for v in &values {
        with(
            &mut db,
            "put g {j: $1, jh: $1, k: \"a\"}",
            std::slice::from_ref(v),
        )
        .unwrap();
    }
    for f in ["j", "jh"] {
        let text = format!("get g where {f} != null select {f}, count(*) as n group {f}");
        let mut got: Vec<Value> = answer(&db, &text, &[])
            .rows
            .iter()
            .map(|r| r.values[1].clone())
            .collect();
        got.sort_by(|a, b| a.cmp_value(b));
        let two = Value::Int(2);
        let one = Value::Int(1);
        assert_eq!(
            got,
            [one.clone(), one.clone(), one, two.clone(), two.clone(), two],
            "{text}"
        );
        for text in [
            format!("get g select count(distinct {f}) as n"),
            format!("get g select approx_count_distinct({f}) as n"),
            format!("get g select hll_estimate(hll_accumulate({f})) as n"),
        ] {
            assert_eq!(count(&db, &text), Value::Int(6), "{text}");
        }
        let text = format!("get g facet {f} limit 0");
        let facet = &answer(&db, &text, &[]).facets[0].values;
        let three = facet
            .iter()
            .find(|v| v.0.cmp_value(&Value::Int(3)) == Ordering::Equal)
            .unwrap_or_else(|| panic!("{text}: {facet:?}"));
        assert_eq!(three.1, 4, "{text}: {facet:?}");
        let held = facet.iter().filter(|v| !v.0.is_null()).count();
        assert_eq!(held, 5, "{text}: {facet:?}");
        // A time finds the numbers of its milliseconds, through the
        // bucket as through the scan, and not the text reading as it.
        let text = format!("get g where {f} = $1");
        assert_eq!(ids(&db, &text, &[Value::Timestamp(MS)]), [7, 8], "{text}");
    }
    let r = answer(&db, "get g group k select k, count(distinct j) as n", &[]);
    assert_eq!(r.rows[0].values[1], Value::Int(6));
    // `@unique` holds one of each, alone and beside another field.
    for (a, b) in [
        (Value::Int(3), Value::Float(3.0)),
        (
            Value::List(vec![Value::Int(3)]),
            Value::List(vec![Value::Float(3.0)]),
        ),
    ] {
        run(&mut db, "del u; del cu");
        with(&mut db, "put u {j: $1}", std::slice::from_ref(&a)).unwrap();
        let e = with(&mut db, "put u {j: $1}", std::slice::from_ref(&b)).unwrap_err();
        assert!(e.to_string().contains("unique"), "{e}");
        with(&mut db, "put cu {j: $1, z: 1}", std::slice::from_ref(&a)).unwrap();
        let e = with(&mut db, "put cu {j: $1, z: 1}", std::slice::from_ref(&b)).unwrap_err();
        assert!(e.to_string().contains("unique"), "{e}");
    }
    // A time and the int of its milliseconds, bytes and the text of them:
    // equal, and one value however they meet.
    with(
        &mut db,
        "put x {t: $1, b: $2}",
        &[Value::Timestamp(0), Value::Bytes(b"a".to_vec())],
    )
    .unwrap();
    with(&mut db, "put x {s: \"a\"}", &[]).unwrap();
    for text in [
        "get x select count(distinct coalesce(t, 0)) as n",
        "get x select count(distinct coalesce(b, s)) as n",
        "get x select approx_count_distinct(coalesce(t, 0)) as n",
    ] {
        assert_eq!(count(&db, text), Value::Int(1), "{text}");
    }
    let text = "get x select coalesce(t, 0) as k, count(*) as n group k";
    assert_eq!(answer(&db, text, &[]).rows.len(), 1, "{text}");
}
