//! `match` beside `order`, `count` or an aggregate: a filter, the rows its
//! ranking would score above zero, the statement's own clause deciding the
//! rest as it does for a `where` (`Select::match_ranks`).
//!
//! Every form is held to what a caller wrote before it was there, row for
//! row: `id in (get <c> match ... select id)` beside the filter -- that
//! list the ranking's rows, so the two can only agree where the set is
//! exactly the ranking's -- and the ranking itself to the set, word for
//! word, loose or not, filtered or not, scored over the filter's rows or
//! the collection's. Then what the list could not do: a count past its
//! 10 000 rows, and a row past its time left out as every read leaves it.

use fennec_core::prelude::*;

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

const WORDS: [&str; 16] = [
    "rust",
    "wasm",
    "database",
    "databases",
    "index",
    "query",
    "search",
    "page",
    "tent",
    "garden",
    "river",
    "stone",
    "budget",
    "report",
    "rusty",
    "searches",
];

fn exec(db: &mut Database, sql: &str, params: &[Value]) {
    for s in fennec_ql::parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}")) {
        db.execute_with(&s, params)
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

fn answer(db: &Database, sql: &str, params: &[Value]) -> ResultSet {
    let stmt = fennec_ql::parse_one(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    match db.query(&stmt, params) {
        Ok(Response::Rows(rs)) => rs,
        Ok(other) => panic!("{sql}: {other:?}"),
        Err(e) => panic!("{sql}: {e}"),
    }
}

/// The same `get`, its `match` scored over the filter's rows
/// (`Match::within`), as a scoped token's is.
fn answer_within(db: &Database, sql: &str, params: &[Value]) -> ResultSet {
    let Statement::Select(mut sel) = fennec_ql::parse_one(sql).unwrap() else {
        panic!("{sql}");
    };
    if let Some(m) = sel.matcher.as_mut() {
        m.within = true;
    }
    match db.query(&Statement::Select(sel), params) {
        Ok(Response::Rows(rs)) => rs,
        other => panic!("{sql}: {other:?}"),
    }
}

/// The refusal, the parser's -- which checks a `get` as the engine does --
/// or the engine's.
fn error(db: &Database, sql: &str, params: &[Value]) -> String {
    match fennec_ql::parse_one(sql) {
        Err(e) => e.to_string(),
        Ok(stmt) => db.query(&stmt, params).expect_err(sql).to_string(),
    }
}

fn ids(rs: &ResultSet) -> Vec<DocId> {
    rs.rows.iter().map(|r| r.id).collect()
}

fn values(rs: &ResultSet) -> Vec<Vec<Value>> {
    rs.rows.iter().map(|r| r.values.clone()).collect()
}

fn counted(rs: &ResultSet) -> i64 {
    match rs.rows[0].values[..] {
        [Value::Int(n)] => n,
        ref v => panic!("{v:?}"),
    }
}

/// 3 000 rows of a few words each: `at` repeats, so an order by it alone
/// meets ties, which go in id order; `kind` is hashed, `n` is not indexed.
fn setup() -> Database {
    let mut db = Database::new();
    exec(
        &mut db,
        "create collection r (title text @text, at int @sorted, kind text @hash, n int)",
        &[],
    );
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut docs = Vec::new();
    for i in 0..3_000 {
        let len = 1 + rng.below(6);
        let title: Vec<&str> = (0..len)
            .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
            .collect();
        let kind = ["a", "b", "c"][rng.below(3) as usize];
        docs.push(format!(
            r#"{{title: "{}", at: {}, kind: "{kind}", n: {}}}"#,
            title.join(" "),
            rng.below(400),
            i % 97
        ));
    }
    exec(&mut db, &format!("put r [{}]", docs.join(", ")), &[]);
    db
}

/// The queries each form is asked with: a word, two, a word every few rows
/// hold, a word no row holds, and loose ones.
const QUERIES: [(&str, &str); 8] = [
    ("rust", ""),
    ("database search", ""),
    ("tent garden river", ""),
    ("kotlin", ""),
    ("databse", " fuzzy"),
    ("datab", " prefix"),
    ("serch rus", " fuzzy prefix"),
    ("rustt", " fuzzy 1"),
];

/// The filters each form is asked with: none, an equality its hash index
/// answers whole, a range the scan tests, and both.
const FILTERS: [&str; 4] = [
    "",
    r#" where kind = "a""#,
    " where n > 40",
    r#" where kind = "b" and n < 30"#,
];

/// The workaround's filter: the ranking's rows as a list, and the filter.
fn listed(filter: &str, how: &str) -> String {
    let list = format!("id in (get r select id match title $1{how})");
    match filter.strip_prefix(" where ") {
        None => format!(" where {list}"),
        Some(f) => format!(" where {list} and {f}"),
    }
}

#[test]
fn the_set_is_the_rankings_rows() {
    let db = setup();
    for (q, how) in QUERIES {
        let p = [Value::Text(q.into())];
        for filter in FILTERS {
            // Every row the ranking scores, no limit: its ceiling is past
            // what these rows hold.
            let ranked = answer(&db, &format!("get r{filter} match title $1{how}"), &p);
            let mut ranked = ids(&ranked);
            ranked.sort_unstable();
            let filtered = answer(
                &db,
                &format!("get r{filter} match title $1{how} order id"),
                &p,
            );
            assert_eq!(ids(&filtered), ranked, "{q}{how}{filter}");
            assert!(filtered.rows.iter().all(|r| r.score.is_none()));
            // Scored over the filter's rows, as a scoped token's is: the
            // set the same.
            let within = answer_within(
                &db,
                &format!("get r{filter} match title $1{how} order id"),
                &p,
            );
            assert_eq!(ids(&within), ranked, "{q}{how}{filter} within");
            let mut ranked_within = ids(&answer_within(
                &db,
                &format!("get r{filter} match title $1{how}"),
                &p,
            ));
            ranked_within.sort_unstable();
            assert_eq!(ranked_within, ranked, "{q}{how}{filter} ranked within");
        }
    }
}

#[test]
fn an_ordered_page_is_the_lists() {
    let db = setup();
    for (q, how) in QUERIES {
        let p = [Value::Text(q.into())];
        for filter in FILTERS {
            for page in [
                " order at desc limit 20",
                " order at desc, id desc limit 20",
                " order at limit 7 offset 5",
                " order kind, n desc, at",
                " order n * 2 - at desc limit 9",
            ] {
                let new = format!("get r{filter} match title $1{how}{page}");
                let old = format!("get r{}{page}", listed(filter, how));
                let (a, b) = (answer(&db, &new, &p), answer(&db, &old, &p));
                assert_eq!(ids(&a), ids(&b), "{new}");
                assert_eq!(values(&a), values(&b), "{new}");
            }
        }
    }
}

#[test]
fn a_count_and_an_aggregate_are_the_lists() {
    let db = setup();
    for (q, how) in QUERIES {
        let p = [Value::Text(q.into())];
        for filter in FILTERS {
            for tail in [
                " count",
                " select count(*), sum(n), min(at), max(at), avg(n)",
                " select kind, count(*), sum(n) group kind order kind",
                " select kind, count(*) group kind having count(*) > 3 order count desc, kind",
                " select count(*), sum(n) order at desc limit 50",
                " select sum(at) order at, id limit 30 offset 10",
            ] {
                let new = format!("get r{filter} match title $1{how}{tail}");
                let old = format!("get r{}{tail}", listed(filter, how));
                let (a, b) = (answer(&db, &new, &p), answer(&db, &old, &p));
                assert_eq!(values(&a), values(&b), "{new}");
                let within = answer_within(&db, &new, &p);
                assert_eq!(values(&within), values(&b), "{new} within");
            }
        }
    }
}

/// `highlight()` marks a row's words whatever orders it: under `order`
/// each row's marks are the ones its ranking gives it.
#[test]
fn marks_answer_under_an_order() {
    let db = setup();
    for (q, how) in [("rust database", ""), ("serch", " fuzzy")] {
        let p = [Value::Text(q.into())];
        let ordered = answer(
            &db,
            &format!(
                "get r select id, highlight(title) match title $1{how} order at desc limit 40"
            ),
            &p,
        );
        let ranked = answer(
            &db,
            &format!("get r select id, highlight(title) match title $1{how}"),
            &p,
        );
        assert_eq!(ordered.rows.len(), 40);
        for row in &ordered.rows {
            let same = ranked.rows.iter().find(|r| r.id == row.id).expect("ranked");
            assert_eq!(row.values, same.values, "{q}");
        }
    }
}

/// Without `order`, `count` or an aggregate the ranking is as it was: best
/// first, each row its `_score`, ties in id order, the page cut from it.
#[test]
fn a_match_alone_still_ranks() {
    let db = setup();
    let p = [Value::Text("rust database".into())];
    let all = answer(&db, "get r match title $1", &p);
    let scores: Vec<f32> = all.rows.iter().map(|r| r.score.unwrap()).collect();
    for (w, ids) in scores.windows(2).zip(all.rows.windows(2)) {
        assert!(
            w[0] > w[1] || (w[0] == w[1] && ids[0].id < ids[1].id),
            "{w:?}"
        );
    }
    let page = answer(&db, "get r match title $1 limit 10 offset 5", &p);
    assert_eq!(ids(&page), ids(&all)[5..15]);
    assert!(error(&db, "get r match title $1 limit 10001", &p).contains("at most 10000 rows"));
}

/// The refusals left: what ranks the rows beside what is a filter, a
/// row's marks beside a number, and a page picked by id that reads as the
/// best rows.
#[test]
fn what_cannot_be_answered_is_refused() {
    let mut db = setup();
    exec(
        &mut db,
        "create collection v (title text @text, e vector<2>)",
        &[],
    );
    exec(&mut db, r#"put v {title: "rust", e: [1, 0]}"#, &[]);
    let p = [Value::Text("rust".into())];
    for (sql, want) in [
        (
            "get v match title $1 rerank e [1, 0] order id",
            "ranks nothing: leave out `rerank`",
        ),
        (
            "get v match title $1 rerank e [1, 0] count",
            "ranks nothing: leave out `rerank`",
        ),
        (
            "get r select highlight(title) match title $1 count",
            "`count` answers no row",
        ),
        (
            "get r select count(*) match title $1 limit 50",
            "`limit` and `offset` need `order`",
        ),
        (
            "get r select sum(n) match title $1 offset 5",
            "`limit` and `offset` need `order`",
        ),
        (
            "get r match title $1 count limit 5",
            "`count` cannot be used together with `limit`",
        ),
        // No score to order by: a ranking's alone, and no field.
        (
            "get r match title $1 order _score desc",
            "`_score` is a ranking's: leave out `order`",
        ),
    ] {
        let e = error(&db, sql, &p);
        assert!(e.contains(want), "{sql}: {e}");
    }
}

/// A count and an aggregate answer no rows, so the 10 000 rows a ranking
/// stops at bound neither, and an ordered page reads every match as an
/// ordered `get` reads every row its filter passes; the ranking keeps its
/// ceiling, and the list its inner `get` makes stops there, a page short.
#[test]
fn a_count_past_the_rankings_ceiling_is_answered() {
    let mut db = Database::new();
    exec(
        &mut db,
        "create collection big (title text @text, at int @sorted, n int)",
        &[],
    );
    let docs: Vec<String> = (0..12_500)
        .map(|i| {
            let extra = if i % 5 == 0 { " rare" } else { "" };
            format!(
                r#"{{title: "common word{extra}", at: {}, n: {i}}}"#,
                i % 1000
            )
        })
        .collect();
    exec(&mut db, &format!("put big [{}]", docs.join(", ")), &[]);
    let p = [Value::Text("common".into())];
    assert_eq!(
        counted(&answer(&db, "get big match title $1 count", &p)),
        12_500
    );
    assert_eq!(
        counted(&answer(
            &db,
            "get big where n >= 500 match title $1 count",
            &p
        )),
        12_000
    );
    let folded = answer(&db, "get big select count(*), max(n) match title $1", &p);
    assert_eq!(
        folded.rows[0].values,
        [Value::Int(12_500), Value::Int(12_499)]
    );
    let grouped = answer(
        &db,
        "get big select at, count(*) match title $1 group at order at limit 2",
        &p,
    );
    assert_eq!(
        values(&grouped),
        [
            vec![Value::Int(0), Value::Int(13)],
            vec![Value::Int(1), Value::Int(13)],
        ]
    );
    // The newest first, ties in id order: 999 is every thousandth row's.
    let page = answer(
        &db,
        "get big select n match title $1 order at desc limit 3",
        &p,
    );
    assert_eq!(
        values(&page),
        [
            vec![Value::Int(999)],
            vec![Value::Int(1999)],
            vec![Value::Int(2999)],
        ]
    );
    let all = answer(&db, "get big select n match title $1 order n desc", &p);
    assert_eq!(all.rows.len(), 12_500);
    // A two-word query's set is every row holding either.
    let two = [Value::Text("rare common".into())];
    assert_eq!(
        counted(&answer(&db, "get big match title $1 count", &two)),
        12_500
    );
    let rare = [Value::Text("rare".into())];
    assert_eq!(
        counted(&answer(&db, "get big match title $1 count", &rare)),
        2_500
    );
    // The ranking is held to its ceiling, a query error past it.
    let e = error(&db, "get big match title $1 limit 12000", &p);
    assert!(e.contains("at most 10000 rows"), "{e}");
}

/// A row past its `@ttl` is left out of a `match` filter at once, as every
/// read leaves it out: the twin holds the same rows under a plain
/// `@sorted`, the expiry written out as a filter.
#[test]
fn a_row_past_its_time_is_not_matched() {
    const NOW: i64 = 1_800_000_000_000;
    const TTL: i64 = 30 * 60 * 1000;
    let mut db = Database::new();
    db.set_clock(Some(NOW));
    exec(
        &mut db,
        "create collection s (title text @text, seen timestamp @ttl(30m), n int)",
        &[],
    );
    exec(
        &mut db,
        "create collection t (title text @text, seen timestamp @sorted, n int)",
        &[],
    );
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..600 {
        let len = 1 + rng.below(4);
        let title: Vec<&str> = (0..len)
            .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
            .collect();
        // A third of them past their time.
        let seen = NOW - (rng.below(3 * TTL as u64 / 2) as i64);
        let doc = format!(r#"{{title: "{}", seen: $1, n: {i}}}"#, title.join(" "));
        let at = [Value::Timestamp(seen)];
        exec(&mut db, &format!("put s {doc}"), &at);
        exec(&mut db, &format!("put t {doc}"), &at);
    }
    let alive = Value::Timestamp(NOW - TTL);
    let mut expired = 0;
    for (q, how) in QUERIES {
        let p = [Value::Text(q.into()), alive.clone()];
        for tail in [
            " count",
            " select n order seen desc, n limit 25",
            " select count(*), sum(n)",
        ] {
            let a = answer(&db, &format!("get s match title $1{how}{tail}"), &p);
            let b = answer(
                &db,
                &format!("get t where seen > $2 match title $1{how}{tail}"),
                &p,
            );
            assert_eq!(values(&a), values(&b), "{q}{how}{tail}");
        }
        let every = answer(&db, &format!("get t match title $1{how} count"), &p);
        let kept = answer(&db, &format!("get s match title $1{how} count"), &p);
        expired += counted(&every) - counted(&kept);
    }
    assert!(expired > 0, "some matched row was past its time");
}

/// `facet` beside a filtering `match` counts the rows it selects, as the
/// rows and the count it goes beside are; beside a ranking it counts as
/// it did.
#[test]
fn facets_beside_a_filter_count_its_rows() {
    let db = setup();
    let p = [Value::Text("rust tent".into())];
    let counted_beside = answer(&db, "get r match title $1 count facet kind", &p);
    let ranked_beside = answer(&db, "get r match title $1 limit 5 facet kind", &p);
    let ordered_beside = answer(&db, "get r match title $1 order at limit 5 facet kind", &p);
    assert_eq!(counted_beside.facets, ranked_beside.facets);
    assert_eq!(ordered_beside.facets, ranked_beside.facets);
    let n: u64 = counted_beside.facets[0].values.iter().map(|v| v.1).sum();
    assert_eq!(n as i64, counted(&counted_beside));
}

/// `explain` names the filter it was: the rows the words scored above
/// zero, those the filter kept, then the order put on them.
#[test]
fn explain_names_the_filter() {
    let db = setup();
    let p = [Value::Text("rust".into())];
    let steps = |sql: &str| -> Vec<String> {
        (answer(&db, sql, &p).rows.iter())
            .map(|r| match &r.values[0] {
                Value::Text(t) => t.clone(),
                v => panic!("{v:?}"),
            })
            .collect()
    };
    let filtered = steps(r#"explain get r where kind = "a" match title $1 order at desc limit 5"#);
    // The hash index names the filter's rows, then the words' are kept
    // among them.
    assert!(
        filtered[0].starts_with("filter: the hash index on kind"),
        "{filtered:?}"
    );
    assert!(
        filtered[1].starts_with("match: the text index on title, a filter: ")
            && filtered[1].contains("rows scored")
            && filtered[1].contains("kept"),
        "{filtered:?}"
    );
    assert!(
        filtered.iter().any(|s| s.starts_with("order: at desc")),
        "{filtered:?}"
    );
    assert_eq!(filtered.last().map(String::as_str), Some("rows: 5"));
    // On its own `match` still ranks, and says so.
    let ranked = steps("explain get r match title $1 limit 5");
    assert_eq!(ranked[0], "match: the text index on title, 5 ranked");
    // A facet beside the ranking counts the filter's set, and says nothing
    // more of it.
    let faceted = steps("explain get r match title $1 limit 5 facet kind");
    let told: Vec<&String> = faceted.iter().filter(|s| s.starts_with("match:")).collect();
    assert_eq!(
        told,
        ["match: the text index on title, 5 ranked"],
        "{faceted:?}"
    );
}
