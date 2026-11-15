//! A `@hash` bucket holds its ids ascending, whatever order they came in,
//! and gives the scan's answer row for row.
//!
//! A bucket was a list in the order its ids were added, and a row left it
//! by a walk of the whole list: a row written out of a bucket and back in
//! went last, so a page through the index was not the scan's page, and with
//! a field of few values every delete walked a fifth of the collection
//! (the `@ttl` sweep held the write lock 370 to 500 ms a thousand rows).

use fennec_core::prelude::*;

fn run(db: &mut Database, sql: &str) -> Response {
    db.execute(&fennec_ql::parse_one(sql).expect("parse"))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn ids(db: &mut Database, sql: &str) -> Vec<u64> {
    run(db, sql)
        .rows()
        .expect("rows")
        .rows
        .iter()
        .map(|r| r.id)
        .collect()
}

#[test]
fn a_row_written_back_into_a_bucket_keeps_its_place_in_a_page() {
    let mut db = Database::new();
    run(&mut db, "create collection t (k text @hash, n int)");
    run(&mut db, "create collection plain (k text, n int)");
    for c in ["t", "plain"] {
        for i in 1..=4 {
            run(&mut db, &format!("put {c} {{id: {i}, k: \"a\", n: {i}}}"));
        }
        // Out of the bucket and back: it went to the end of the list.
        run(&mut db, &format!("set {c} {{k: \"b\"}} where id = 2"));
        run(&mut db, &format!("set {c} {{k: \"a\"}} where id = 2"));
    }
    let scan = ids(&mut db, "get plain where k = \"a\" limit 2");
    assert_eq!(scan, vec![1, 2]);
    assert_eq!(ids(&mut db, "get t where k = \"a\" limit 2"), scan);
    assert_eq!(
        ids(&mut db, "get t where k = \"a\""),
        ids(&mut db, "get plain where k = \"a\"")
    );
}

/// Rows of few values written, rewritten and deleted -- the oldest first, as
/// the sweep deletes, and here and there -- through buckets past a run's
/// size, each answer held to the same collection without the index.
#[test]
fn buckets_of_many_rows_answer_as_the_scan_through_deletes_and_rewrites() {
    let mut db = Database::new();
    run(&mut db, "create collection t (k text @hash, n int)");
    run(&mut db, "create collection plain (k text, n int)");
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let docs: Vec<String> = (0..6_000)
        .map(|i| format!("{{k: \"v{}\", n: {i}}}", next() % 5))
        .collect();
    for c in ["t", "plain"] {
        run(&mut db, &format!("put {c} [{}]", docs.join(", ")));
    }
    let check = |db: &mut Database| {
        for v in 0..5 {
            for tail in ["", " limit 7", " limit 3 offset 600"] {
                let a = ids(db, &format!("get t where k = \"v{v}\"{tail}"));
                let b = ids(db, &format!("get plain where k = \"v{v}\"{tail}"));
                assert_eq!(a, b, "k = v{v}{tail}");
            }
            let a = ids(
                db,
                &format!("get t where k in [\"v{v}\", \"v{}\"]", (v + 2) % 5),
            );
            let b = ids(
                db,
                &format!("get plain where k in [\"v{v}\", \"v{}\"]", (v + 2) % 5),
            );
            assert_eq!(a, b);
            // Each bucket a run of the union, merged where it ends: three
            // and five of them, a value twice, and one with no rows.
            for list in [
                format!("\"v{v}\", \"v{}\", \"v{}\"", (v + 1) % 5, (v + 3) % 5),
                "\"v4\", \"v0\", \"v3\", \"v1\", \"v2\"".to_string(),
                format!("\"v{v}\", \"none\", \"v{v}\", \"v{}\"", (v + 4) % 5),
            ] {
                let a = ids(db, &format!("get t where k in [{list}]"));
                let b = ids(db, &format!("get plain where k in [{list}]"));
                assert_eq!(a, b, "in [{list}]");
            }
        }
    };
    check(&mut db);
    for round in 0..6 {
        let lo = round * 400;
        let r = next() % 5;
        let s = next() % 6_000;
        for c in ["t", "plain"] {
            run(
                &mut db,
                &format!("del {c} where n >= {lo} and n < {}", lo + 400),
            );
            run(
                &mut db,
                &format!("set {c} {{k: \"v{r}\"}} where n > {s} and n < {}", s + 300),
            );
            run(
                &mut db,
                &format!("del {c} where n > {} and n < {}", s / 2, s / 2 + 50),
            );
        }
        check(&mut db);
    }
}

/// Deleting the oldest rows of a collection whose `@hash` fields hold a
/// few values each costs about what it costs without the indexes: a row
/// leaves a bucket by a binary search and a run of at most 512 ids moved.
/// It walked the whole bucket -- 15 000 to 30 000 ids a field here -- and
/// 100 deletes took 91 ms against 0.61 in a debug build. Each side's best
/// of several rounds, the two taken in turns, so a busy machine slows both
/// rather than one.
#[test]
fn a_delete_out_of_a_large_bucket_costs_what_a_delete_without_one_does() {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection hashed (country text @hash, device text @hash, \
         browser text @hash, name text @hash, n int)",
    );
    run(
        &mut db,
        "create collection plain (country text, device text, browser text, name text, n int)",
    );
    let docs: Vec<String> = (0..60_000)
        .map(|i| {
            format!(
                "{{country: \"c{}\", device: \"d{}\", browser: \"b{}\", name: \"e{}\", n: {i}}}",
                i % 4,
                i % 3,
                i % 2,
                i % 4
            )
        })
        .collect();
    for c in ["hashed", "plain"] {
        for chunk in docs.chunks(5_000) {
            run(&mut db, &format!("put {c} [{}]", chunk.join(", ")));
        }
        // The indexes built, as the first read builds them.
        run(&mut db, &format!("get {c} where country = \"c0\" limit 1"));
    }
    let (mut hashed, mut plain) = (f64::MAX, f64::MAX);
    for round in 0..8 {
        for (c, best) in [("hashed", &mut hashed), ("plain", &mut plain)] {
            let t = std::time::Instant::now();
            for i in 0..100 {
                let n = round * 100 + i;
                run(&mut db, &format!("del {c} where id = {}", n + 1));
            }
            *best = best.min(t.elapsed().as_secs_f64());
        }
    }
    assert!(
        hashed < plain * 4.0,
        "100 deletes took {:.2} ms with four @hash fields, {:.2} ms without",
        hashed * 1e3,
        plain * 1e3
    );
}

/// The rows an equality's bucket gives are tested without that equality,
/// every one holding it (`Filter::without`): they answer as the scan does,
/// row for row and failure for failure, wherever the term stands in the
/// `and` chain -- a value read through the field's type, a `nocase` fold,
/// the id, a parameter, the term written twice, bytes compared with the
/// list of numbers JSON writes them as -- and a term after it that fails
/// still fails over the rows it reaches.
#[test]
fn rows_a_bucket_gave_answer_as_the_scan_without_its_equality() {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection hashed (k text @hash, f float @hash, h text nocase @hash, n int, \
         b bytes @hash)",
    );
    run(
        &mut db,
        "create collection plain (k text, f float, h text nocase, n int, b bytes)",
    );
    let cursor = db.change_seq();
    for c in ["hashed", "plain"] {
        for i in 0..40 {
            run(
                &mut db,
                &format!(
                    "put {c} {{k: \"k{}\", f: {}, h: \"{}\", n: {}, b: {}}}",
                    i % 3,
                    i % 4,
                    ["Ab", "aB", "cd"][i % 3],
                    i % 7,
                    ["[104, 105]", "[1, 2]", "\"hi\""][i % 3],
                ),
            );
        }
    }
    let hi = Value::List(vec![Value::Int(104), Value::Int(105)]);
    let params = [Value::Text("k1".into()), Value::Int(3), hi];
    for filter in [
        "k = \"k1\" and n > 2",
        "n > 2 and k = \"k1\"",
        "k = \"k1\" and n > 2 and k = \"k1\"",
        "f = 2 and n < 5",
        "h = \"AB\" and n >= 3",
        "id = 7 and n > 1",
        "n > 1 and id = 7",
        "k = $1 and n >= $2",
        "k = \"k1\" and (n = 1 or n = 4)",
        "k = \"k1\" and k = \"k2\"",
        "k = \"k1\" and 1 / (n - 3) > 0",
        "1 / (n - 3) > 0 and k = \"k1\"",
        "k = \"k2\" and 1 / (n - 6) > 0",
        // Bytes go out as JSON as a list of numbers, and come back so: the
        // bucket found the rows by the list read as bytes, the scan
        // compared bytes with a list by kind and found none.
        "b = [104, 105] and n >= 0",
        "n >= 0 and b = [104, 105]",
        "b = [104, 105]",
        "b = $3 and n > 1",
        "b = $3",
        "b = \"hi\" and n > 1",
        "b in [[104, 105], [9]] and n > 1",
        "b in [[104, 105], [9]]",
        "b != [104, 105] and n > 4",
        "b = [104, 105.5] and n >= 0",
        "b = [104, 256] and n >= 0",
    ] {
        let answer = |c: &str| {
            let stmt = fennec_ql::parse_one(&format!("get {c} where {filter} order id")).unwrap();
            db.query(&stmt, &params)
                .map(|r| {
                    r.rows()
                        .unwrap()
                        .rows
                        .iter()
                        .map(|r| r.id)
                        .collect::<Vec<_>>()
                })
                .map_err(|e| e.to_string())
        };
        assert_eq!(answer("hashed"), answer("plain"), "{filter}");
        // The bytes `[104, 105]` and the text `"hi"` are one value, the
        // rows `i % 3` of 0 and 2.
        if filter.starts_with("b = [104, 105]") || filter.starts_with("b = $3") {
            let found = answer("plain").unwrap();
            assert!(!found.is_empty(), "{filter}");
            assert!(
                found.iter().all(|id| (id - 1) % 3 != 1),
                "{filter}: {found:?}"
            );
        }
        let count = |c: &str| {
            let stmt = fennec_ql::parse_one(&format!("get {c} where {filter} count")).unwrap();
            db.query(&stmt, &params)
                .map(|r| format!("{:?}", r.rows().unwrap().rows))
                .map_err(|e| e.to_string())
        };
        assert_eq!(count("hashed"), count("plain"), "{filter}");
        if !filter.starts_with("b ") && !filter.contains(" b ") {
            continue;
        }
        // A stream's test of a changed row (`changes_since`, `eval`'s) and
        // a select list's item read the list beside bytes as the query's
        // filter does: compared by kind, a stream seeded the row and then
        // told its subscriber it was deleted.
        for c in ["hashed", "plain"] {
            let rows = answer(c).unwrap();
            let shape = match fennec_ql::parse_one(&format!("get {c} where {filter}")).unwrap() {
                Statement::Select(s) => s.filter.unwrap(),
                _ => unreachable!(),
            };
            let streamed = |b: Changes| match b {
                Changes::Batch(b) => {
                    assert_eq!(b.puts.rows.len() + b.dels.len(), 40, "{c} {filter}");
                    b.puts.rows.iter().map(|r| r.id).collect::<Vec<_>>()
                }
                Changes::Reseed => panic!("{c} {filter}: a reseed"),
            };
            let since = db.changes_since(c, cursor, Some(&shape), None, &params);
            assert_eq!(
                streamed(since.unwrap()),
                rows,
                "{c} {filter}: changes_since"
            );
            if !filter.contains('$') {
                let written = db.written_after(c, cursor).unwrap().unwrap();
                let held = db.changes_held(c, written, Some(&shape), None, None);
                assert_eq!(streamed(held.unwrap()), rows, "{c} {filter}: changes_held");
            }
            let sql = format!("get {c} select id, {filter} as hit order id");
            let listed = db
                .query(&fennec_ql::parse_one(&sql).unwrap(), &params)
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
            let hits: Vec<u64> = listed
                .rows()
                .unwrap()
                .rows
                .iter()
                .filter(|r| r.values[1] == Value::Bool(true))
                .map(|r| r.id)
                .collect();
            assert_eq!(hits, rows, "{sql}");
        }
    }
}

/// The rows an index took over from an equality's bucket -- an `in` by id
/// or over a `@hash`, an index over several fields, a range of a
/// `@sorted` field, a box of a `@geo` one, each narrower than the bucket --
/// are tested with that equality: they need not hold it, and only the
/// bucket's own rows are tested without it (`Database::candidates_held`
/// lets the equality go wherever another set takes its place). Each case's
/// plan names the set that answered, so a case whose set stops taking
/// over -- a box past its cap, a range declined -- fails rather than
/// passing as the bucket's.
#[test]
fn rows_another_index_took_over_are_tested_with_the_equality() {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection hashed (k text @hash, n int @sorted, g int @hash, a int, b int, \
         loc geo @geo, @sorted(a, b))",
    );
    run(
        &mut db,
        "create collection plain (k text, n int, g int, a int, b int, loc geo)",
    );
    for c in ["hashed", "plain"] {
        for i in 0..60 {
            let put = format!(
                "put {c} {{k: \"k{}\", n: {i}, g: {i}, a: {}, b: {}, loc: $1}}",
                i % 3,
                i % 20,
                i % 7,
            );
            let loc = Value::List(vec![Value::Float(i as f64 / 10.0), Value::Float(0.5)]);
            db.execute_with(&fennec_ql::parse_one(&put).unwrap(), &[loc])
                .unwrap_or_else(|e| panic!("{put}: {e}"));
        }
    }
    let boxed = Value::List([1.05, 0.0, 1.45, 1.0].map(Value::Float).to_vec());
    let mut wrong = Vec::new();
    // Each filter, and the start of its plan's line for the set that gave
    // the rows: the bucket of `k = "k1"`, 20 rows, is chosen first and the
    // set named here takes its place, but in the last case.
    for (filter, answered) in [
        (
            "k = \"k1\" and id in [2, 3, 4]",
            "filter: the id index, `in`, 3 rows",
        ),
        (
            "k = \"k1\" and g in [2, 3, 4]",
            "filter: the hash index, `in`, on g, 3 rows",
        ),
        (
            "k = \"k1\" and a = 5",
            "filter: the index on (a, b), a fixed, 3 rows",
        ),
        (
            "k = \"k1\" and a = 5 and b >= 0",
            "filter: the index on (a, b), a fixed, a range on b, 3 rows",
        ),
        (
            "k = \"k1\" and a in [5, 6, 7]",
            "filter: the index on (a, b), a fixed, 3 spans, 9 rows",
        ),
        // The bucket, then the index over (a, b) above, then a range
        // narrower than both: the rows whose `n` is 26 and 27 hold every
        // term but the equality, whose bucket they are not in.
        (
            "k = \"k1\" and a in [5, 6, 7] and n >= 25 and n <= 27",
            "filter: the ordered index on n, 3 rows",
        ),
        (
            "k = \"k1\" and n > 55",
            "filter: the ordered index on n, 4 rows",
        ),
        (
            "n > 55 and k = \"k1\"",
            "filter: the ordered index on n, 4 rows",
        ),
        (
            "k = \"k1\" and within(loc, $1)",
            "filter: the point index on loc, ",
        ),
        // The bucket narrower than the range: its rows tested without it.
        (
            "k = \"k1\" and n > 2",
            "filter: the hash index on k, 20 rows",
        ),
    ] {
        let answer = |c: &str, tail: &str| {
            let stmt = fennec_ql::parse_one(&format!("get {c} where {filter} {tail}")).unwrap();
            db.query(&stmt, std::slice::from_ref(&boxed))
                .map(|r| format!("{:?}", r.rows().unwrap().rows))
                .map_err(|e| e.to_string())
        };
        for tail in ["order id", "count", "limit 2"] {
            let (hashed, plain) = (answer("hashed", tail), answer("plain", tail));
            if hashed != plain {
                wrong.push(format!(
                    "{filter} {tail}: {hashed:?} where the scan said {plain:?}"
                ));
            }
            let sql = format!("explain get hashed where {filter} {tail}");
            let plan: Vec<String> = db
                .query(
                    &fennec_ql::parse_one(&sql).unwrap(),
                    std::slice::from_ref(&boxed),
                )
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                .rows()
                .unwrap()
                .rows
                .iter()
                .map(|r| match &r.values[..] {
                    [Value::Text(s)] => s.clone(),
                    other => panic!("{sql}: a plan row holds {other:?}"),
                })
                .collect();
            assert!(
                plan.iter().any(|l| l.starts_with(answered)),
                "{sql}: no line {answered:?} in {plan:#?}"
            );
            // The index over (a, b) took the bucket's place before the
            // range took its own: declined, the range would have taken the
            // bucket's place alone.
            if filter.contains("a in") {
                assert!(
                    !plan.iter().any(|l| l.contains("(a, b) not used")),
                    "{sql}: {plan:#?}"
                );
            }
        }
        // Each filter answers some rows and leaves some out.
        let rows = answer("plain", "count").unwrap();
        assert!(
            !rows.contains("Int(0)") && !rows.contains("Int(60)"),
            "{filter}: {rows}"
        );
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
