//! Values of two kinds order by kind, but for the numbers -- an int, a
//! float and a time, by their true values -- and a text and bytes, by their
//! bytes (`Value::cmp_value`). A time equalled a text that read as it
//! before, while the int equal to the time ranked below that text: no
//! order, and `order coalesce(t, j)` over a time field and a json one
//! holding ints and ISO texts panicked in the standard library's sort. A
//! comparison still reads a text beside a time as the time it names
//! (`Value::cmp_read`), whichever side either came from -- a literal, a
//! parameter, a json field's ISO text, a text field's -- through the scan
//! and every index alike. And a group, a distinct count, a facet and a
//! sketch tell apart only what `cmp_value` does: a json field's `3` and
//! `3.0` were two groups.

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
/// time. A text read off a row is read so too: a json field's ISO text
/// equals its time.
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
    // A json field's ISO text is the time it names beside a time, either
    // side.
    let every: Vec<u64> = (1..=11).collect();
    for text in [
        "get w where t = j",
        "get w where j = t",
        "get w where j <= t",
    ] {
        assert_eq!(ids(&db, text, &[]), every, "{text}");
    }
    assert_eq!(ids(&db, "get w where t < j", &[]), Vec::<u64>::new());
    let now = [Value::Timestamp(at(4))];
    assert_eq!(ids(&db, "get w where j = $1", &now), [5]);
    assert_eq!(
        ids(&db, "get w where $1 < j", &now),
        (6..=12).collect::<Vec<_>>()
    );
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

/// What `cmp_value` finds equal is one group, one distinct value, one
/// sketched, one facet value and one `@unique` value: a json field's `3`
/// and `3.0`, `[3]` and `[3.0]`, a time and the int of its milliseconds,
/// bytes and the text of them. Through a `@hash` too.
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
        // A time finds the numbers of its milliseconds and the text naming
        // it, a json `@hash` stepping aside for the scan: one value to `=`,
        // three to a group, as the order has them.
        let text = format!("get g where {f} = $1");
        assert_eq!(
            ids(&db, &text, &[Value::Timestamp(MS)]),
            [7, 8, 9],
            "{text}"
        );
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

/// A json path's ISO text beside a time -- `now()` less a span, which the
/// parser folds to a time, a parameter, a time field -- is the time it
/// names, through the scan, a `@sorted` and a `@hash` on the path alike, an
/// ISO text a time was written into the field as among them. Ordered by
/// kind alone, every text was above every time: `j.at > now() - 1d` found
/// the rows of a week ago, and `t = j.at` none.
#[test]
fn a_json_time_compares_as_the_time_through_every_index() {
    fennec_ql::install();
    let hour = 3_600_000;
    let now = MS + 30 * 24 * hour;
    let iso = |ms: i64| Value::Text(format_iso(ms));
    let at = |v: Value| Value::Object(vec![("at".into(), v)]);
    for index in ["", "@sorted", "@hash"] {
        let mut db = Database::new();
        db.set_clock(Some(now));
        run(&mut db, "create collection c (t timestamp, j json)");
        if !index.is_empty() {
            run(&mut db, &format!("create index on c (j.at) {index}"));
        }
        let rows = [
            (Value::Timestamp(now - hour), at(iso(now - hour))),
            (
                Value::Timestamp(now - 168 * hour),
                at(iso(now - 168 * hour)),
            ),
            // A time written into a json field, which keeps its ISO text.
            (Value::Null, at(Value::Timestamp(now - hour / 2))),
            (Value::Null, at(iso(now - 168 * hour))),
        ];
        for (t, j) in rows {
            with(&mut db, "put c {t: $1, j: $2}", &[t, j]).unwrap();
        }
        let hour_ago = [Value::Timestamp(now - hour)];
        let cases: [(&str, &[Value], &[u64]); 11] = [
            ("get c where j.at > now() - 86400000", &[], &[1, 3]),
            ("get c where now() - 86400000 < j.at", &[], &[1, 3]),
            ("get c where j.at < now() - 86400000", &[], &[2, 4]),
            ("get c where j.at = $1", &hour_ago, &[1]),
            ("get c where $1 = j.at", &hour_ago, &[1]),
            ("get c where j.at in [$1]", &hour_ago, &[1]),
            ("get c where j.at >= $1", &hour_ago, &[1, 3]),
            ("get c where t = j.at", &[], &[1, 2]),
            ("get c where j.at = t", &[], &[1, 2]),
            (
                "get c where coalesce(t, j.at) > now() - 9000000",
                &[],
                &[1, 3],
            ),
            (
                "get c where coalesce(t, j.at) < now() - 9000000",
                &[],
                &[2, 4],
            ),
        ];
        for (text, params, want) in cases {
            assert_eq!(ids(&db, text, params), want, "{index}: {text}");
        }
    }
}

/// A comparison answers alike wherever it stands and whichever side each
/// value is on: a filter, a select list's item, an aggregate's `filter`, a
/// `having` -- whose fields reach the comparison as values lifted off the
/// group's rows, which once read as a statement's own.
#[test]
fn a_comparison_answers_alike_wherever_it_stands() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection s (t timestamp, j json, x text, k int)",
    );
    let iso = |ms: i64| Value::Text(format_iso(ms));
    let text = |t: &str| Value::Text(t.into());
    let pg = Value::Text(fennec_core::time::format_pg(MS));
    let rows = [
        (Value::Timestamp(MS), iso(MS), iso(MS)),
        (Value::Timestamp(MS), iso(MS + 1000), pg),
        (Value::Timestamp(MS + 5000), iso(MS), text("abc")),
        (
            Value::Timestamp(MS),
            text("abc"),
            text(&format_iso(MS).replace('T', " ")),
        ),
        (Value::Timestamp(MS), Value::Int(MS), text("2026-01-01")),
        (Value::Null, iso(MS), Value::Null),
        (Value::Timestamp(MS - 1), Value::Null, iso(MS + 1)),
        (
            Value::Timestamp(MS + 1),
            Value::Float(MS as f64 + 0.5),
            text(""),
        ),
    ];
    for (k, (t, j, x)) in rows.into_iter().enumerate() {
        let doc = "put s {t: $1, j: $2, x: $3, k: $4}";
        with(&mut db, doc, &[t, j, x, Value::Int(k as i64 + 1)]).unwrap();
    }
    assert_eq!(ids(&db, "get s where t = j", &[]), [1, 5]);
    // Row 6's two nulls are equal, as a filter has them.
    assert_eq!(ids(&db, "get s where t = x", &[]), [1, 2, 4, 5, 6]);
    let selected = |text: &str, params: &[Value]| -> Vec<u64> {
        let mut out: Vec<u64> = answer(&db, text, params)
            .rows
            .iter()
            .filter(|r| r.values[0] == Value::Bool(true))
            .map(|r| r.id)
            .collect();
        out.sort_unstable();
        out
    };
    let counted = |text: &str, params: &[Value]| -> usize {
        match answer(&db, text, params).rows[0].values[0] {
            Value::Int(n) => n as usize,
            ref v => panic!("{text}: {v:?}"),
        }
    };
    let having = |text: &str, params: &[Value]| -> Vec<u64> {
        let mut out: Vec<u64> = answer(&db, text, params)
            .rows
            .iter()
            .map(|r| match r.values[0] {
                Value::Int(k) => k as u64,
                ref v => panic!("{text}: {v:?}"),
            })
            .collect();
        out.sort_unstable();
        out
    };
    let time = [Value::Timestamp(MS)];
    let pairs: [(&str, &str, &[Value]); 5] = [
        ("t", "j", &[]),
        ("t", "x", &[]),
        ("j", "x", &[]),
        ("j", "$1", &time),
        ("x", "$1", &time),
    ];
    for (a, b, params) in pairs {
        let agg = |e: &str| match e.starts_with('$') {
            true => e.to_string(),
            false => format!("max({e})"),
        };
        for (op, _) in OPS {
            let back = turn(op);
            let want = ids(&db, &format!("get s where {a} {op} {b}"), params);
            let forms = [
                (
                    "where, turned",
                    ids(&db, &format!("get s where {b} {back} {a}"), params),
                ),
                (
                    "select",
                    selected(&format!("get s select {a} {op} {b} as v"), params),
                ),
                (
                    "select, turned",
                    selected(&format!("get s select {b} {back} {a} as v"), params),
                ),
                (
                    "having",
                    having(
                        &format!("get s group k select k having {} {op} {}", agg(a), agg(b)),
                        params,
                    ),
                ),
                (
                    "having, turned",
                    having(
                        &format!("get s group k select k having {} {back} {}", agg(b), agg(a)),
                        params,
                    ),
                ),
            ];
            for (form, got) in forms {
                assert_eq!(got, want, "{a} {op} {b}, {form}");
            }
            for (x, o, y) in [(a, op, b), (b, back, a)] {
                let text = format!("get s select count(*) filter (where {x} {o} {y}) as n");
                assert_eq!(counted(&text, params), want.len(), "{text}");
            }
        }
    }
}

/// A text field beside a time reads its every spelling of it -- ISO, a
/// space for the `T`, an offset, PostgreSQL's, a date -- through the scan,
/// a `@hash`, a `@sorted` and an index over several fields alike: the
/// `@hash` filed the ISO text alone, and found that one where the scan
/// found them all.
#[test]
fn a_text_field_meets_a_time_alike_through_every_index() {
    fennec_ql::install();
    let mut db = Database::new();
    run(
        &mut db,
        "create collection x (s text, sh text @hash, ss text @sorted, sc text, z int, \
         @sorted(sc, z))",
    );
    let iso = format_iso(MS);
    let spellings = [
        iso.clone(),
        iso.replace('T', " "),
        iso.replace('Z', "+00:00"),
        fennec_core::time::format_pg(MS),
        "2026-01-01".to_string(),
        format_iso(MS + 1000),
        format_iso(MS - 1000),
        "abc".to_string(),
        String::new(),
    ];
    for s in &spellings {
        let doc = "put x {s: $1, sh: $1, ss: $1, sc: $1, z: 1}";
        with(&mut db, doc, &[Value::Text(s.clone())]).unwrap();
    }
    let time = [Value::Timestamp(MS)];
    for (op, holds) in OPS {
        let want: Vec<u64> = spellings
            .iter()
            .zip(1u64..)
            .filter(|(s, _)| match fennec_core::time::parse(s) {
                Ok(ms) => holds(ms.cmp(&MS)),
                // Above every time.
                Err(_) => holds(Ordering::Greater),
            })
            .map(|(_, id)| id)
            .collect();
        for f in ["s", "sh", "ss", "sc"] {
            let text = format!("get x where {f} {op} $1");
            assert_eq!(ids(&db, &text, &time), want, "{text}");
            let text = format!("get x where $1 {} {f}", turn(op));
            assert_eq!(ids(&db, &text, &time), want, "{text}");
        }
    }
    // `in`, short and long: a set keyed by the field's texts holds no time.
    let mut list = vec![Value::Timestamp(MS)];
    list.extend((1..10).map(|i| Value::Timestamp(MS + i * 7_000)));
    for f in ["s", "sh", "ss", "sc"] {
        for n in [1, list.len()] {
            let text = format!("get x where {f} in $1");
            let got = ids(&db, &text, &[Value::List(list[..n].to_vec())]);
            assert_eq!(got, [1, 2, 3, 4, 5], "{text} ({n})");
        }
    }
}
