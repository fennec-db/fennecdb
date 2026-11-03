//! `match` scored over the rows its filter selects (`Match::within`), as a
//! scoped token's is: every score is the one a collection holding those
//! rows alone gives, so nothing of the other rows reaches it. Scored over
//! the collection, a user's own memo went 9.87 -> 4.79 once another user
//! wrote 200 private ones holding the same word.

use fennec_core::prelude::*;

const WORDS: [&str; 12] = [
    "initech", "revenue", "memo", "budget", "sand", "dune", "oasis", "night", "report", "plan",
    "rust", "wasm",
];

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

fn run(db: &mut Database, sql: &str) {
    for s in fennec_ql::parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}")) {
        db.execute(&s).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

/// `(n, score)` of each row, in the order answered.
fn scores(db: &Database, sql: &str, within: bool) -> Vec<(i64, f32)> {
    let Statement::Select(mut sel) = fennec_ql::parse_one(sql).unwrap() else {
        panic!("{sql}");
    };
    sel.matcher.as_mut().unwrap().within = within;
    let Response::Rows(rs) = db.query(&Statement::Select(sel), &[]).unwrap() else {
        panic!("{sql}");
    };
    rs.rows
        .iter()
        .map(|r| match r.values[0] {
            Value::Int(n) => (n, r.score.unwrap()),
            ref v => panic!("{v:?}"),
        })
        .collect()
}

#[test]
fn a_filtered_match_within_scores_as_its_rows_alone_would() {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection memos (n int, owner text @hash, body text @text)",
    );
    for u in 0..4 {
        run(
            &mut db,
            &format!("create collection only{u} (n int, owner text @hash, body text @text)"),
        );
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for n in 0..2_000 {
        // User 3 writes most, and most of it about initech.
        let u = if rng.below(4) == 0 { rng.below(3) } else { 3 };
        let len = 1 + rng.below(12);
        let body: Vec<&str> = (0..len)
            .map(|_| {
                if u == 3 && rng.below(2) == 0 {
                    "initech"
                } else {
                    WORDS[rng.below(WORDS.len() as u64) as usize]
                }
            })
            .collect();
        let doc = format!("{{n: {n}, owner: \"u{u}\", body: \"{}\"}}", body.join(" "));
        run(&mut db, &format!("put memos {doc}"));
        run(&mut db, &format!("put only{u} {doc}"));
    }
    for q in [
        "initech",
        "initech memo",
        "revenue budget plan",
        "sand dune oasis night",
        "rust",
        "nothing",
    ] {
        for u in 0..4 {
            let shared =
                format!("get memos select n where owner = \"u{u}\" match body \"{q}\" limit 50");
            let alone = format!("get only{u} select n match body \"{q}\" limit 50");
            let within = scores(&db, &shared, true);
            assert_eq!(within, scores(&db, &alone, false), "{q} as u{u}");
            if u < 3 && q.starts_with("initech") {
                // Over the collection, user 3's rows weigh on the others'.
                assert_ne!(within, scores(&db, &shared, false), "{q} as u{u}");
            }
        }
    }
    // With no filter the rows are the collection's, and so are the scores.
    let all = "get memos select n match body \"initech memo\" limit 20";
    assert_eq!(scores(&db, all, true), scores(&db, all, false));
}

/// Under `@text(accents)` whether a word typed with accents reads its
/// spelling is decided by the rows the filter selects, as BM25's counts
/// are: alice's `match`, held to her rows, answers the same rows, scores,
/// marks and facets whether or not bob wrote rows of the spelling she
/// cannot read -- her `sutlac tarifi` found by `sütlaç` before and after.
/// Over the collection his rows are the spelling it reads, and hers is
/// found no more.
#[test]
fn a_row_the_filter_leaves_out_decides_no_spelling() {
    let answer = |bob: bool, within: bool, q: &str| {
        let mut db = Database::new();
        run(
            &mut db,
            "create collection notes (n int, owner text @hash, kind text, body text @text(accents))",
        );
        run(
            &mut db,
            "put notes {n: 1, owner: \"alice\", kind: \"tatli\", body: \"sutlac tarifi\"}, \
             {n: 2, owner: \"alice\", kind: \"yemek\", body: \"sutlac ve kofte\"}",
        );
        let bobs = if bob { "sütlaç" } else { "kek" };
        run(
            &mut db,
            &format!("put notes {{n: 3, owner: \"bob\", kind: \"gizli\", body: \"{bobs}\"}}"),
        );
        let Statement::Select(mut sel) = fennec_ql::parse_one(&format!(
            "get notes select n, highlight(body) where owner = \"alice\" match body \"{q}\" \
             limit 10 facet kind"
        ))
        .unwrap() else {
            panic!("{q}");
        };
        sel.matcher.as_mut().unwrap().within = within;
        let Response::Rows(rs) = db.query(&Statement::Select(sel), &[]).unwrap() else {
            panic!("{q}");
        };
        rs
    };
    for q in ["sütlaç", "SÜTLAÇ tarifi", "sütlac"] {
        let alone = answer(false, true, q);
        assert_eq!(alone, answer(true, true, q), "{q}");
        assert_eq!(alone.rows.len(), 2, "{q}");
        assert_eq!(alone.facets[0].values.len(), 2, "{q}");
        // Her `sutlac` is marked: the word as it read it.
        let first = alone
            .rows
            .iter()
            .find(|r| r.values[0] == Value::Int(1))
            .unwrap();
        let span = Value::List(vec![Value::Int(0), Value::Int(6)]);
        let Value::List(spans) = &first.values[1] else {
            panic!("{q}: {:?}", first.values[1]);
        };
        assert_eq!(spans.first(), Some(&span), "{q}");
    }
    // Over the collection bob's rows decide it: `sütlaç` reads his
    // spelling, which none of alice's rows writes.
    let over_all = answer(true, false, "sütlaç");
    assert!(over_all.rows.is_empty(), "{:?}", over_all.rows);
}
