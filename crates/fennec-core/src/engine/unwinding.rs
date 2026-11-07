//! A panic as a block lands, at each point a test can make one: before the
//! sink call the block is put back and the database goes on; from the sink
//! call through the block's notes on the feed to its end, the database is
//! failed, so that no later block takes the change numbers a record in the
//! file holds already (`docs/design/engine.md`, *Blocks*). And a panic in
//! the middle of a write -- as each index takes a document in or out, part
//! way into one, in the graph's linking -- is put back whole with the block
//! it is in: every write records what undoes it before it changes anything.

use std::cell::Cell;

thread_local! {
    /// The point the next landing or write on this thread panics at, once:
    /// a name, or `+` or `-` and an index's name for that index taking a
    /// document in or out.
    static PANIC_AT: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Whether a test asked for a panic at `at`, which it is then told it
/// has: for a site that leaves something behind before it panics.
pub(crate) fn armed(at: &str) -> bool {
    let hit = PANIC_AT.with(|p| p.get()) == Some(at);
    if hit {
        PANIC_AT.with(|p| p.set(None));
    }
    hit
}

/// A panic where a test asked for one at `at`: a site in the landing or
/// in a write's upkeep of the indexes, compiled into the tests alone.
pub(crate) fn panic_at(at: &str) {
    if armed(at) {
        panic!("a test's panic at {at}");
    }
}

/// [`panic_at`] as `index` takes a document in (`dir` `+`) or out (`-`),
/// asked by the name the index goes by with no string made for it.
pub(crate) fn panic_in(dir: char, index: &str) {
    let at = PANIC_AT.with(|p| p.get());
    if at.is_some_and(|at| at.strip_prefix(dir) == Some(index)) {
        PANIC_AT.with(|p| p.set(None));
        panic!("a test's panic at {dir}{index}");
    }
}

#[cfg(feature = "std-fs")]
mod on_file {
    use super::super::*;
    use super::PANIC_AT;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn put(db: &mut Database, n: i64) -> Result<Response> {
        db.execute(&Statement::Put {
            collection: "t".into(),
            docs: vec![vec![("n".into(), Expr::Lit(Value::Int(n)))]],
            docs_param: None,
            insert: false,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        })
    }

    fn rows(db: &Database) -> usize {
        db.collection("t").expect("t").stats().documents
    }

    /// Each point, on a file of its own: whether the database is failed,
    /// and what the file holds once opened again -- the counter counting
    /// the records in it, the next block numbered past every one.
    #[test]
    fn a_panic_from_the_sink_call_on_fails_the_database() {
        let dir = std::env::temp_dir().join(format!("fennecdb-landing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        // The point, whether it fails the database, and whether the
        // panicking block's record is in the file.
        for (at, fails, in_file) in [
            ("before", false, false),
            ("sink", true, false),
            ("noted", true, true),
            ("notes", true, true),
            ("end", true, true),
        ] {
            let path = dir.join(format!("{at}.fennec"));
            let _ = std::fs::remove_file(&path);
            let mut db = crate::fs::open(&path).expect("open");
            let schema = Schema::new("t", vec![crate::schema::Field::new("n", DataType::Int)])
                .expect("schema");
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .expect("create");
            put(&mut db, 1).expect("put");
            let seq = db.change_seq();

            PANIC_AT.with(|p| p.set(Some(at)));
            let r = catch_unwind(AssertUnwindSafe(|| put(&mut db, 2)));
            assert!(r.is_err(), "{at}: no panic");
            assert!(!db.in_block(), "{at}: a block left open");
            assert!(!db.landing, "{at}: the landing left marked");
            assert_eq!(db.failure().is_some(), fails, "{at}");
            let next = put(&mut db, 3);
            if fails {
                // The record took `seq + 1`, or may have: a block landing
                // now would be numbered with it.
                assert!(matches!(next, Err(Error::Io(_))), "{at}: {next:?}");
            } else {
                // Put back: the next block takes the number it would have.
                next.expect("the write after a put-back block");
                assert_eq!(db.change_seq(), seq + 1, "{at}");
            }
            if fails {
                db.sync().expect_err("a failed database syncs nothing");
            } else {
                db.sync().expect("sync");
            }
            drop(db);

            let mut db = crate::fs::open(&path).expect("reopen");
            let landed = u64::from(in_file || !fails);
            assert_eq!(db.change_seq(), seq + landed, "{at}: the counter");
            assert_eq!(rows(&db), 1 + landed as usize, "{at}: the rows");
            put(&mut db, 4).expect("a write after the reopen");
            assert_eq!(db.change_seq(), seq + landed + 1, "{at}");
            db.sync().expect("sync");
            drop(db);
            let db = crate::fs::open(&path).expect("reopen");
            assert_eq!(db.change_seq(), seq + landed + 1, "{at}: no number twice");
            drop(db);
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(crate::synclog::path_for(&path));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A panic in the middle of a write, at each place a write changes
/// something: as each index takes the document in or out, part way into
/// the text index's terms, its sorted words and the sparse index's
/// dimensions, and as the graph places a vector, leaves one waiting and
/// links the waiting. Each write -- an insert, a `put` over a row, a
/// `set`, a `del` and an upsert -- runs alone and as the second of a block
/// after one that went well; once the panic has put the block back, the
/// rows, every index's entries and answers, the ids handed out, the change
/// counter, the feed and the file opened again are what they were before
/// the block, and the write made again is taken.
#[cfg(all(feature = "std-fs", feature = "indexes", feature = "geo"))]
mod torn {
    use super::super::*;
    use super::PANIC_AT;
    use crate::schema::{Field, TextIndexSpec, VectorIndexSpec};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::Path;

    /// A hundred years: no row expires while a test runs.
    const TTL: u64 = 100 * 365 * 86_400_000;

    /// The rows the block finds, those its writes make, and the one the
    /// block's first write makes.
    const KS: [i64; 10] = [1, 2, 3, 4, 101, 102, 103, 104, 105, 200];

    const BODIES: [&str; 4] = [
        "harbour lantern meadow",
        "lantern orchard",
        "meadow quarrel sapphire",
        "orchard thunder harbour",
    ];

    /// Every place a test panics at, each index's by its name: `+` as it
    /// takes a document in, `-` as it takes one out.
    const POINTS: [&str; 28] = [
        "+email",
        "+email@text",
        "+tag",
        "+n",
        "+at",
        "+body@text",
        "+v",
        "+s",
        "+p",
        "+tag+n",
        "-email",
        "-email@text",
        "-tag",
        "-n",
        "-at",
        "-body@text",
        "-v",
        "-s",
        "-p",
        "-tag+n",
        "+text-term",
        "-text-term",
        "words",
        "+sparse-dim",
        "-sparse-dim",
        "graph-link",
        "graph-defer",
        "graph-pending",
    ];

    fn text(s: &str) -> Value {
        Value::Text(s.into())
    }

    /// A row's fields, each value made from `k`: the rows the block finds
    /// are 1 to 4, and a write's values are of a `k` no row holds -- a
    /// tag, an email, words, dimensions and a point of its own.
    fn row(k: i64) -> Vec<(String, Expr)> {
        let body = match k {
            1..=4 => BODIES[k as usize - 1].to_string(),
            _ => format!("willow{k} zephyr harbour"),
        };
        let tag = match k {
            1..=4 => ["red", "blue"][k as usize % 2],
            _ => "green",
        };
        let dim = (k % 8) as u32;
        [
            ("email", text(&format!("u{k}@x"))),
            ("tag", text(tag)),
            ("n", Value::Int(k * 10)),
            ("at", Value::Timestamp(1_000_000 + k)),
            ("body", text(&body)),
            ("v", Value::Vector(vec![k as f32, 1.0, 0.5, 0.0])),
            ("s", Value::Sparse(16, vec![(dim, 1.0), (dim + 8, 0.5)])),
            (
                "p",
                Value::List(vec![
                    Value::Float(k as f64 / 10.0),
                    Value::Float(k as f64 / 20.0),
                ]),
            ),
        ]
        .into_iter()
        .map(|(f, v)| (f.to_string(), Expr::Lit(v)))
        .collect()
    }

    fn with_id(id: i64, mut fields: Vec<(String, Expr)>) -> Vec<(String, Expr)> {
        fields.insert(0, ("id".into(), Expr::Lit(Value::Int(id))));
        fields
    }

    fn put(doc: Vec<(String, Expr)>, insert: bool) -> Statement {
        Statement::Put {
            collection: "t".into(),
            docs: vec![doc],
            docs_param: None,
            insert,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        }
    }

    fn eq(field: &str, v: Value) -> Expr {
        Expr::Cmp(
            CmpOp::Eq,
            Box::new(Expr::Field(field.into())),
            Box::new(Expr::Lit(v)),
        )
    }

    #[derive(Clone, Copy, Debug)]
    enum Write {
        Insert,
        Over,
        Set,
        Del,
        Upsert,
    }

    fn write(w: Write) -> Statement {
        match w {
            Write::Insert => put(row(105), true),
            Write::Over => put(with_id(2, row(102)), false),
            Write::Set => Statement::Update {
                collection: "t".into(),
                set: row(103),
                filter: Some(eq("id", Value::Int(3))),
                require: None,
                pick: None,
            },
            Write::Del => Statement::Delete {
                collection: "t".into(),
                filter: Some(eq("id", Value::Int(4))),
                require: None,
                pick: None,
            },
            // Row 1 holds the email: its every other field is set.
            Write::Upsert => {
                let mut doc = row(101);
                doc[0].1 = Expr::Lit(text("u1@x"));
                Statement::Put {
                    collection: "t".into(),
                    docs: vec![doc],
                    docs_param: None,
                    insert: true,
                    if_absent: true,
                    else_set: Some(row(101).into_iter().skip(1).collect()),
                    require: None,
                    pick: None,
                }
            }
        }
    }

    /// A file holding rows 1 to 4 under every kind of index: `@unique`
    /// with `@text` beside it, `@hash`, `@sorted`, `@ttl`, `@text`, the
    /// graph, `@inverted`, `@geo` and an index over two fields.
    fn made(path: &Path) -> Database {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(crate::synclog::path_for(path));
        let mut db = crate::fs::open(path).expect("open");
        let mut email = Field::new("email", DataType::Text).indexed(IndexKind::UNIQUE);
        email
            .add_index(IndexKind::Text(TextIndexSpec::default()))
            .expect("beside");
        let vector = VectorIndexSpec::default().resolved();
        let schema = Schema::new(
            "t",
            vec![
                email,
                Field::new("tag", DataType::Text).indexed(IndexKind::HASH),
                Field::new("n", DataType::Int).indexed(IndexKind::SORTED),
                Field::new("at", DataType::Timestamp).indexed(IndexKind::Sorted { ttl: Some(TTL) }),
                Field::new("body", DataType::Text)
                    .indexed(IndexKind::Text(TextIndexSpec::default())),
                Field::new("v", DataType::Vector(4, VecPrec::F32))
                    .indexed(IndexKind::Vector(vector)),
                Field::new("s", DataType::Sparse(16)).indexed(IndexKind::Inverted),
                Field::new("p", DataType::Geo).indexed(IndexKind::Geo),
            ],
        )
        .expect("schema");
        db.execute(&Statement::CreateCollection {
            schema,
            if_not_exists: false,
        })
        .expect("create");
        db.execute(&Statement::CreateComposite {
            collection: "t".into(),
            index: Composite {
                keys: vec![("tag".into(), false), ("n".into(), true)],
                unique: false,
            },
            if_not_exists: false,
        })
        .expect("composite");
        for k in 1..=4 {
            db.execute(&put(with_id(k, row(k)), false)).expect("a row");
        }
        db.sync().expect("sync");
        db
    }

    /// What every index answers: a tag's rows (`@hash`), by `n` descending
    /// (the index over both), an email's (`@unique`), `n`'s range in order
    /// (`@sorted`), each word ranked and two misspelt (`@text`, the sorted
    /// words), the nearest of each vector (the graph), a sparse vector's
    /// (`@inverted`), the points near one (`@geo`), and the rows past
    /// their time a century on (`@ttl`).
    fn answers(db: &Database) -> Vec<String> {
        let ask = |sel: Select| {
            let sel = Select {
                collection: "t".into(),
                ..sel
            };
            format!("{:?}", db.query(&Statement::Select(sel), &[]))
        };
        let matched = |w: &str, fuzzy: u8| Select {
            matcher: Some(Match {
                field: "body".into(),
                query: Expr::Lit(text(w)),
                within: false,
                fuzzy,
                prefix: false,
            }),
            limit: Some(Count::N(10)),
            ..Select::default()
        };
        let mut out = Vec::new();
        for tag in ["red", "blue", "green"] {
            let filter = Some(eq("tag", text(tag)));
            out.push(ask(Select {
                filter: filter.clone(),
                ..Select::default()
            }));
            out.push(ask(Select {
                filter,
                order: vec![Sort::new("n", false)],
                ..Select::default()
            }));
        }
        for k in KS {
            out.push(ask(Select {
                filter: Some(eq("email", text(&format!("u{k}@x")))),
                ..Select::default()
            }));
        }
        let from = Expr::Cmp(
            CmpOp::Ge,
            Box::new(Expr::Field("n".into())),
            Box::new(Expr::Lit(Value::Int(-1))),
        );
        out.push(ask(Select {
            filter: Some(from),
            order: vec![Sort::new("n", true)],
            ..Select::default()
        }));
        let words = [
            "harbour", "lantern", "meadow", "orchard", "quarrel", "sapphire", "thunder",
        ];
        for w in words
            .iter()
            .copied()
            .chain(["zephyr", "willow101", "willow105", "willow200"])
        {
            out.push(ask(matched(w, 0)));
        }
        out.push(ask(matched("harbor", 1)));
        out.push(ask(matched("zephir", 1)));
        for k in KS {
            out.push(ask(Select {
                near: Some(Near {
                    field: "v".into(),
                    vector: Expr::Lit(Value::Vector(vec![k as f32, 1.0, 0.5, 0.0])),
                    ef: None,
                    exact: false,
                    metric: None,
                }),
                limit: Some(Count::N(3)),
                ..Select::default()
            }));
        }
        out.push(ask(Select {
            near: Some(Near {
                field: "s".into(),
                vector: Expr::Lit(Value::Sparse(16, vec![(1, 1.0), (5, 1.0), (10, 1.0)])),
                ef: None,
                exact: false,
                metric: None,
            }),
            limit: Some(Count::N(10)),
            ..Select::default()
        }));
        let distance = Expr::Call(
            "distance".into(),
            vec![
                Expr::Field("p".into()),
                Expr::Lit(Value::List(vec![Value::Float(0.0), Value::Float(0.0)])),
            ],
        );
        out.push(ask(Select {
            filter: Some(Expr::Cmp(
                CmpOp::Le,
                Box::new(distance),
                Box::new(Expr::Lit(Value::Float(2_000_000.0))),
            )),
            ..Select::default()
        }));
        out.push(format!("{:?}", db.expired("t", i64::MAX / 4, 100)));
        out
    }

    /// The database as a test holds it to another: the change counter, the
    /// next id handed out, each row, what each index answers and holds.
    #[derive(Debug, PartialEq)]
    struct State {
        seq: u64,
        next_id: DocId,
        rows: Vec<String>,
        answers: Vec<String>,
        indexes: Vec<(String, String)>,
    }

    fn state(db: &Database) -> State {
        let c = db.collection("t").expect("t");
        let mut rows: Vec<_> = (c.store.iter_ids())
            .map(|id| format!("{id} {:?}", c.store.read(&c.schema, id)))
            .collect();
        rows.sort();
        // Answered first, so that the sorted words are made for the dump
        // to hold to the terms.
        let answers = answers(db);
        State {
            seq: db.change_seq(),
            next_id: c.store.next_id(),
            rows,
            answers,
            indexes: c.dump_indexes(),
        }
    }

    /// Write `w` with a panic at `at`, alone or after a write that went
    /// well in a block of several; whether it reached `at`.
    fn case(dir: &Path, w: Write, at: &'static str, alone: bool) -> bool {
        let name = format!("{w:?}{at}{alone}").replace(['+', '@'], "_");
        let path = dir.join(format!("{name}.fennec"));
        let mut db = made(&path);
        let before = state(&db);
        let stmt = write(w);
        let r = catch_unwind(AssertUnwindSafe(|| {
            if alone {
                PANIC_AT.with(|p| p.set(Some(at)));
                return db.execute(&stmt).map(drop);
            }
            db.begin()?;
            db.execute(&put(row(200), true))?;
            PANIC_AT.with(|p| p.set(Some(at)));
            db.execute(&stmt)?;
            db.commit()
        }));
        let reached = PANIC_AT.with(|p| p.take()).is_none();
        let tag = format!("{w:?} at {at}{}", if alone { ", alone" } else { "" });
        if let Ok(r) = r {
            assert!(!reached, "{tag}: a panic that did not unwind");
            r.expect("the write");
            drop(db);
            let _ = std::fs::remove_file(&path);
            return false;
        }
        assert!(reached, "{tag}: a panic not the test's");
        assert!(!db.in_block(), "{tag}: a block left open");
        assert!(db.failure().is_none(), "{tag}: the database failed");
        assert_eq!(state(&db), before, "{tag}: put back");
        match db
            .changes_since("t", before.seq, None, None, &[])
            .expect("changes")
        {
            Changes::Batch(b) => {
                assert!(
                    b.puts.rows.is_empty() && b.dels.is_empty(),
                    "{tag}: the feed"
                )
            }
            Changes::Reseed => panic!("{tag}: the feed reseeds"),
        }
        assert_eq!(
            db.changed_collections_since(before.seq),
            Some(vec![]),
            "{tag}"
        );
        // Taken again -- a `@unique` value or an id left behind would refuse
        // it -- and put back as a refused block is.
        db.begin().expect("begin");
        db.execute(&stmt).expect("the write again");
        db.rollback();
        assert_eq!(state(&db), before, "{tag}: put back again");
        drop(db);
        let db = crate::fs::open(&path).expect("reopen");
        assert_eq!(state(&db), before, "{tag}: the file");
        drop(db);
        let _ = std::fs::remove_file(&path);
        true
    }

    #[test]
    fn a_panic_anywhere_in_a_write_puts_its_block_back_whole() {
        let dir = std::env::temp_dir().join(format!("fennecdb-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let writes = [
            Write::Insert,
            Write::Over,
            Write::Set,
            Write::Del,
            Write::Upsert,
        ];
        let (mut reached, mut failed) = (Vec::new(), Vec::new());
        for w in writes {
            for at in POINTS {
                for alone in [false, true] {
                    // Every case run, and those that failed named together.
                    match catch_unwind(AssertUnwindSafe(|| case(&dir, w, at, alone))) {
                        Ok(true) => reached.push(format!("{w:?} {at}")),
                        Ok(false) => {}
                        Err(_) => failed.push(format!("{w:?} {at} alone={alone}")),
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failed.is_empty(), "{} failed: {failed:#?}", failed.len());
        // Each write met a panic wherever it changes something: every index
        // it takes the row in or out of, part way into each, and the graph.
        let mid = ["+text-term", "words", "+sparse-dim", "graph-link"];
        let out_mid = ["-text-term", "-sparse-dim"];
        let all = [
            "email",
            "email@text",
            "tag",
            "n",
            "at",
            "body@text",
            "v",
            "s",
            "p",
            "tag+n",
        ];
        let mut want = Vec::new();
        let mut each = |w: Write, dirs: &str, skip: &[&str], also: &[&str]| {
            for d in dirs.chars() {
                for f in all
                    .iter()
                    .filter(|f| !skip.contains(&format!("{d}{f}").as_str()))
                {
                    want.push(format!("{w:?} {d}{f}"));
                }
            }
            want.extend(also.iter().map(|a| format!("{w:?} {a}")));
        };
        let both: Vec<&str> = mid.iter().chain(&out_mid).copied().collect();
        each(
            Write::Insert,
            "+",
            &[],
            &[&mid[..], &["graph-defer", "graph-pending"]].concat(),
        );
        each(
            Write::Over,
            "+-",
            &["-v"],
            &[&both[..], &["graph-defer", "graph-pending"]].concat(),
        );
        each(Write::Set, "+-", &["-v"], &both);
        each(Write::Del, "-", &[], &out_mid);
        let upsert_skip = ["+email", "+email@text", "-email", "-email@text", "-v"];
        each(Write::Upsert, "+-", &upsert_skip, &both);
        want.sort();
        want.dedup();
        reached.sort();
        reached.dedup();
        let missed: Vec<_> = want.iter().filter(|w| !reached.contains(w)).collect();
        assert!(missed.is_empty(), "never reached: {missed:?}");
    }
}
