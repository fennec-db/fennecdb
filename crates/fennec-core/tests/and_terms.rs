//! An `and`'s terms are tested in an order of their own (`Filter::chain`):
//! of the terms before the first that may fail -- one the bound filter
//! evaluates, a division, a call, a field it does not know -- each single
//! test first and then each `or`; from that one on, each where it was
//! written. A term that may fail so runs over the rows the terms written
//! before it pass, and no others: fennec-http writes a scoped token's rules
//! first, and a term of the caller's failing by a field of a row the rules
//! hide told what that row held (access.md, *The rules are asked before
//! the caller's terms*). The answers, failures included, are the filter's
//! as written: the reference is the same filter under `not (not ...)`,
//! which is bound as it stands and read by the scan.

use fennec_core::prelude::*;

fn run(db: &mut Database, sql: &str) {
    db.execute(&fennec_ql::parse_one(sql).expect("parse"))
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// The ids `get <c> where <filter>` answers, or its error.
fn answer(db: &Database, c: &str, filter: &str, tail: &str, params: &[Value]) -> Result<String> {
    let sql = format!("get {c} where {filter} {tail}");
    let stmt = fennec_ql::parse_one(&sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    db.query(&stmt, params).map(|r| {
        let rs = r.rows().expect("rows");
        format!(
            "{:?} {:?}",
            rs.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            rs.rows.iter().map(|r| &r.values).collect::<Vec<_>>()
        )
    })
}

/// Posts nine in ten published, by three authors and three owners, `owner`
/// a `@hash` in `hashed` and not in `plain`.
fn posts() -> Database {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection hashed (published bool, author text, owner text @hash, n int, body text)",
    );
    run(
        &mut db,
        "create collection plain (published bool, author text, owner text, n int, body text)",
    );
    for c in ["hashed", "plain"] {
        for i in 0..90 {
            run(
                &mut db,
                &format!(
                    "put {c} {{published: {}, author: \"{}\", owner: \"{}\", n: {}, body: \"{}\"}}",
                    i % 10 != 0,
                    ["a", "b", "c"][i % 3],
                    ["a", "b", "c"][(i / 3) % 3],
                    i % 7,
                    ["xy", "m", "zz"][i % 3],
                ),
            );
        }
    }
    db
}

/// A term that fails by a field of a row runs over the rows the terms
/// written before it pass, whatever stands beside it: under a rule that is
/// an `or`, the caller's plain comparison is asked first and the failing
/// term still after the rule; under a rule over a `@hash`, whose bucket
/// gives the rows, too. Each answer is the written filter's, row for row.
#[test]
fn a_term_that_may_fail_runs_over_the_rows_written_before_it_pass() {
    let db = posts();
    // Fails over every row the rule before it hides.
    let open = "1 / (case when published = true or author = \"a\" then 1 else 0 end) = 1";
    let owned = "1 / (case when owner = \"a\" then 1 else 0 end) = 1";
    let rule = "(published = true or author = \"a\")";
    let params = [Value::Text("a".into()), Value::Int(3)];
    for filter in [
        format!("{rule} and n = 3 and {open}"),
        format!("{rule} and n >= 0 and {open}"),
        format!("{rule} and {open} and n = 3"),
        format!("{rule} and body ~ \"x\" and {open}"),
        format!("{rule} and (n = 1 or n = 2) and {open}"),
        format!("{rule} and n in [1, 2, 9] and body is not null and {open}"),
        format!("({rule} or n = $2) and author = $1 and {open} and n > 1"),
        format!("owner = \"a\" and n >= 0 and {owned}"),
        format!("owner = $1 and (n = 1 or published = false) and {owned}"),
        format!("owner = \"a\" and {rule} and n = 3 and {owned}"),
    ] {
        for c in ["hashed", "plain"] {
            for tail in ["order id", "count", "limit 5", "order n limit 3"] {
                let got = answer(&db, c, &filter, tail, &params);
                assert!(got.is_ok(), "{c} {filter} {tail}: {got:?}");
                let written = answer(&db, c, &format!("not (not ({filter}))"), tail, &params);
                assert_eq!(got, written, "{c} {filter} {tail}");
            }
        }
        assert_eq!(
            answer(&db, "hashed", &filter, "order id", &params),
            answer(&db, "plain", &filter, "order id", &params),
            "{filter}"
        );
    }
}

/// A term that cannot fail stays behind one that may: the one that may
/// can be a rule's -- a call over a field -- and a term of the caller's
/// moved ahead of it would choose the rows it runs over. Here `n != 3`
/// would keep every row `1 / (n - 3)` fails over from it; written after
/// it, the division still meets them and the query fails, as written.
#[test]
fn a_term_that_cannot_fail_stays_behind_one_that_may() {
    let db = posts();
    for filter in [
        "1 / (n - 3) > 0 and n != 3",
        "published = true and 1 / (n - 3) > 0 and n != 3",
        "(published = true or author = \"a\") and 1 / (n - 3) > -9 and n != 3 and body = \"m\"",
        "owner = \"b\" and 1 / (n - 3) > -9 and n != 3",
        "n = 3 and nosuch = 1 and published = false",
        "published = false and n = $5 and n = 3",
    ] {
        for c in ["hashed", "plain"] {
            let got = answer(&db, c, filter, "count", &[]);
            assert!(got.is_err(), "{c} {filter}: {got:?}");
            let written = answer(&db, c, &format!("not (not ({filter}))"), "count", &[]);
            assert_eq!(got, written, "{c} {filter}");
        }
    }
    // Ahead of every term that may fail, the terms that cannot are laid
    // out among themselves alone: the same rows, the same failure.
    for filter in [
        "n != 3 and 1 / (n - 3) > -9",
        "(published = true or author = \"a\") and n != 3 and 1 / (n - 3) > -9",
    ] {
        for c in ["hashed", "plain"] {
            let got = answer(&db, c, filter, "order id", &[]);
            assert!(got.is_ok(), "{c} {filter}: {got:?}");
            let written = answer(&db, c, &format!("not (not ({filter}))"), "order id", &[]);
            assert_eq!(got, written, "{c} {filter}");
        }
    }
}
