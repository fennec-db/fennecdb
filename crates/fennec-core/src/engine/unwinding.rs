//! A panic as a block lands, at each point a test can make one: before the
//! sink call the block is put back and the database goes on; from the sink
//! call through the block's notes on the feed to its end, the database is
//! failed, so that no later block takes the change numbers a record in the
//! file holds already (`docs/design/engine.md`, *Blocks*). And a panic in
//! the middle of a write -- as each index takes a document in or out, part
//! way into one, in the graph's linking -- is put back whole with the block
//! it is in: every write records what undoes it before it changes anything,
//! a replica's applying its primary's records too.

use std::cell::Cell;

thread_local! {
    /// The point the next landing or write on this thread panics at, once:
    /// a name, or `+` or `-` and an index's name for that index taking a
    /// document in or out.
    static PANIC_AT: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// How many times [`armed`] passes that point by before it is armed: a
    /// replica's records land one after another, each through the same
    /// points, and the graph links a batch's vectors one after another.
    static PANIC_SKIP: Cell<usize> = const { Cell::new(0) };
}

/// Whether a test asked for a panic at `at`, which it is then told it
/// has: for a site that leaves something behind before it panics.
pub(crate) fn armed(at: &str) -> bool {
    let hit = PANIC_AT.with(|p| p.get()) == Some(at);
    if hit {
        if PANIC_SKIP.with(|s| s.replace(s.get().saturating_sub(1))) > 0 {
            return false;
        }
        PANIC_AT.with(|p| p.set(None));
    }
    hit
}

/// A panic at `at` the `skip + 1`th time a write passes it, for the next
/// write on this thread.
#[cfg(all(feature = "std-fs", feature = "indexes", feature = "geo"))]
fn arm(at: &'static str, skip: usize) {
    PANIC_SKIP.with(|s| s.set(skip));
    PANIC_AT.with(|p| p.set(Some(at)));
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

/// A test's directory of its own, `<temp>/<name>/<pid>`, removed as the
/// test ends however it ends but by an abort -- a second panic while one
/// unwinds runs no drop, and left `fennecdb-torn-<pid>` directories behind
/// -- and what an aborted run left removed by the next run's start: every
/// directory beside its own that no run has written in for an hour. By
/// its age, not its name alone: a run beside this one, another checkout's,
/// is writing in its own, a file made and removed a case.
#[cfg(feature = "std-fs")]
struct Scratch(std::path::PathBuf);

#[cfg(feature = "std-fs")]
impl Scratch {
    fn new(name: &str) -> Scratch {
        let parent = std::env::temp_dir().join(name);
        let hour = std::time::Duration::from_secs(3600);
        for e in std::fs::read_dir(&parent).into_iter().flatten().flatten() {
            let age = e.metadata().and_then(|m| m.modified()).map(|t| t.elapsed());
            if age.is_ok_and(|a| a.is_ok_and(|a| a > hour)) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        let dir = parent.join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        // A run ending beside this one removes the parent once it is empty,
        // which may fall between the parent's making and its child's.
        let made = (0..3).any(|_| std::fs::create_dir_all(&dir).is_ok());
        assert!(made, "{} not made", dir.display());
        Scratch(dir)
    }
}

#[cfg(feature = "std-fs")]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        // The parent too, once no run holds a directory in it.
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

#[cfg(feature = "std-fs")]
mod on_file {
    use super::super::*;
    use super::{Scratch, PANIC_AT};
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
        let scratch = Scratch::new("fennecdb-landing");
        let dir = &scratch.0;
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
/// the block, and the write made again is taken. And a replica applying
/// the records of each, as its primary landed them: put back whole too.
#[cfg(all(feature = "std-fs", feature = "indexes", feature = "geo"))]
mod torn {
    use super::super::*;
    use super::{arm, Scratch, PANIC_AT};
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

    /// A row of `u`, the second collection: a key, a vector in a graph of
    /// its own and a note no index reads.
    fn u_row(k: i64) -> Vec<(String, Expr)> {
        [
            ("k", Value::Int(k)),
            ("w", Value::Vector(vec![k as f32, 0.5, 1.0, 0.0])),
            ("note", text(&format!("alpha{k} beta"))),
        ]
        .into_iter()
        .map(|(f, v)| (f.to_string(), Expr::Lit(v)))
        .collect()
    }

    fn put_u(k: i64) -> Statement {
        Statement::Put {
            collection: "u".into(),
            docs: vec![u_row(k)],
            docs_param: None,
            insert: true,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        }
    }

    /// A file holding rows 1 to 4 under every kind of index: `@unique`
    /// with `@text` beside it, `@hash`, `@sorted`, `@ttl`, `@text`, the
    /// graph, `@inverted`, `@geo` and an index over two fields; and `u`'s
    /// rows 1 and 2.
    fn made(path: &Path) -> Database {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(crate::synclog::path_for(path));
        let mut db = made_on(crate::fs::open(path).expect("open"));
        db.sync().expect("sync");
        db
    }

    /// [`made`]'s collections and rows, written into `db`.
    fn made_on(mut db: Database) -> Database {
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
        let u = Schema::new(
            "u",
            vec![
                Field::new("k", DataType::Int).indexed(IndexKind::HASH),
                Field::new("w", DataType::Vector(4, VecPrec::F32))
                    .indexed(IndexKind::Vector(vector)),
                Field::new("note", DataType::Text),
            ],
        )
        .expect("u");
        db.execute(&Statement::CreateCollection {
            schema: u,
            if_not_exists: false,
        })
        .expect("create u");
        for k in 1..=2 {
            db.execute(&put_u(k)).expect("a row of u");
        }
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
    /// next id handed out, each row, what each index answers and holds --
    /// and the collections' ids, `t`'s schema, and every other collection,
    /// its schema, rows and indexes.
    #[derive(Clone, Debug, PartialEq)]
    struct State {
        seq: u64,
        next_id: DocId,
        rows: Vec<String>,
        answers: Vec<String>,
        indexes: Vec<(String, String)>,
        next_coll_id: u32,
        schema: String,
        others: Vec<String>,
    }

    fn rows_of(c: &Collection) -> Vec<String> {
        let mut rows: Vec<_> = (c.store.iter_ids())
            .map(|id| format!("{id} {:?}", c.store.read(&c.schema, id)))
            .collect();
        rows.sort();
        rows
    }

    fn state(db: &Database) -> State {
        let c = db.collection("t").expect("t");
        // Answered first, so that the sorted words are made for the dump
        // to hold to the terms.
        let answers = answers(db);
        let others = (db.order.iter().filter(|n| *n != "t"))
            .map(|n| {
                let o = &db.collections[n];
                let s = (o.id, &*o.schema, rows_of(o), o.store.next_id());
                format!("{n} {s:?} {:?}", o.dump_indexes())
            })
            .collect();
        State {
            seq: db.change_seq(),
            next_id: c.store.next_id(),
            rows: rows_of(c),
            answers,
            indexes: c.dump_indexes(),
            next_coll_id: db.next_coll_id,
            schema: format!("{:?}", *c.schema),
            others,
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
        let scratch = Scratch::new("fennecdb-torn");
        let dir = &scratch.0;
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
                    match catch_unwind(AssertUnwindSafe(|| case(dir, w, at, alone))) {
                        Ok(true) => reached.push(format!("{w:?} {at}")),
                        Ok(false) => {}
                        Err(_) => failed.push(format!("{w:?} {at} alone={alone}")),
                    }
                }
            }
        }
        drop(scratch);
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

    /// A primary's sink in memory, keeping the records it is handed as
    /// writes: what its feed sends a replica.
    #[derive(Clone, Default)]
    struct Tap(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

    impl Sink for Tap {
        fn append(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn record(&mut self, _: u64, bytes: &[u8]) -> Result<()> {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
        fn rewrite(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }

    /// The records a primary holding [`made`]'s rows lands for `writes`,
    /// one after another, and the state it is in after them.
    fn landed(writes: impl FnOnce(&mut Database) -> Result<()>) -> (Vec<u8>, State) {
        let tap = Tap::default();
        let mut p = made_on(Database::with_sink(Box::new(tap.clone())));
        let from = tap.0.lock().unwrap().len();
        writes(&mut p).expect("the primary's writes");
        let records = tap.0.lock().unwrap()[from..].concat();
        (records, state(&p))
    }

    /// How a write reaches a replica: its record alone; after the record
    /// of a `put` to `u`, in one `apply`, which waits for the graph to take
    /// its vector with the write; in one block record with that `put`; or
    /// after the records of three, whose vectors wait together and go into
    /// the graph one after another as the write's record comes.
    #[derive(Clone, Copy, Debug)]
    enum Shape {
        Alone,
        After,
        InBlock,
        Three,
    }

    const SHAPES: [Shape; 4] = [Shape::Alone, Shape::After, Shape::InBlock, Shape::Three];

    /// The records of `w` in `shape`, and the primary after them.
    fn shaped(w: Write, shape: Shape) -> (Vec<u8>, State) {
        let stmt = write(w);
        landed(|p| match shape {
            Shape::Alone => p.execute(&stmt).map(drop),
            Shape::After => {
                p.execute(&put_u(3))?;
                p.execute(&stmt).map(drop)
            }
            Shape::InBlock => {
                p.begin()?;
                p.execute(&put_u(3))?;
                p.execute(&stmt)?;
                p.commit()
            }
            Shape::Three => {
                for k in 3..=5 {
                    p.execute(&put_u(k))?;
                }
                p.execute(&stmt).map(drop)
            }
        })
    }

    /// Each of `records`, as the primary landed it.
    fn each_record(records: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < records.len() {
            let r = record_at(records, &mut pos).expect("a record");
            out.push(&records[r.at..pos]);
        }
        out
    }

    /// A replica's state after none of `each`, after the first, the first
    /// two, and so on: what it holds once those landed.
    fn steps(each: &[&[u8]]) -> Vec<State> {
        (0..=each.len())
            .map(|n| {
                let mut db = made_on(Database::new());
                db.apply(&each[..n].concat()).expect("the records");
                state(&db)
            })
            .collect()
    }

    /// A replica over a file holding [`made`]'s rows, following.
    fn replica(path: &Path) -> Database {
        let mut db = made(path);
        db.follow(vec![(7, 0)]).expect("follow");
        db
    }

    /// Nothing on the feed after `seq`, of either collection.
    fn unfed(db: &Database, seq: u64, tag: &str) {
        for c in ["t", "u"] {
            match db.changes_since(c, seq, None, None, &[]).expect("changes") {
                Changes::Batch(b) => assert!(
                    b.puts.rows.is_empty() && b.dels.is_empty(),
                    "{tag}: {c}'s feed"
                ),
                Changes::Reseed => panic!("{tag}: {c}'s feed reseeds"),
            }
        }
        assert_eq!(db.changed_collections_since(seq), Some(vec![]), "{tag}");
    }

    /// `records` applied by a replica with a panic at `at`; whether it
    /// reached `at`. Once the panic has put them back the replica is as it
    /// was -- its rows, every index, the ids, the counter, the feed, the
    /// schemas -- and takes them again, as a follower connecting again
    /// does: a `@unique` value, an id or a node left behind would refuse
    /// them or tell the replica from `after`, as the file opened again
    /// would.
    fn applied(dir: &Path, name: &str, at: &'static str, records: &[u8], after: &State) -> bool {
        let path = dir.join(format!("{name}{at}.fennec").replace(['+', '@', '*'], "_"));
        let mut db = replica(&path);
        let before = state(&db);
        // `graph-link*1`: the second time the graph places a vector, the
        // first linked -- a batch linked half way.
        match at.split_once('*') {
            Some((point, skip)) => arm(point, skip.parse().expect("a skip")),
            None => arm(at, 0),
        }
        let r = catch_unwind(AssertUnwindSafe(|| db.apply(records)));
        let reached = PANIC_AT.with(|p| p.take()).is_none();
        let tag = format!("{name} at {at}");
        match r {
            Ok(r) => {
                assert!(!reached, "{tag}: a panic that did not unwind");
                r.expect("the records");
            }
            Err(_) => {
                assert!(reached, "{tag}: a panic not the test's");
                assert!(!db.in_block(), "{tag}: a block left open");
                assert!(db.failure().is_none(), "{tag}: the replica failed");
                assert_eq!(state(&db), before, "{tag}: put back");
                unfed(&db, before.seq, &tag);
                db.apply(records).expect("the records again");
            }
        }
        assert_eq!(state(&db), *after, "{tag}: applied");
        drop(db);
        let db = crate::fs::open(&path).expect("reopen");
        assert_eq!(state(&db), *after, "{tag}: the file");
        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::synclog::path_for(&path));
        reached
    }

    /// A replica's `apply` writes no block of its own making, and a panic
    /// in a record's upkeep of the indexes -- #501's, in the sorted words
    /// -- left part of it in memory: the row in the store and some of its
    /// indexes, in no record, the counter not moved, served to every read
    /// until the next open. Each write, in each [`Shape`], with a panic at
    /// each place it changes something, as the record is whole and as it
    /// is handed to the sink: put back whole, and taken again.
    #[test]
    fn a_panic_anywhere_in_applying_a_record_puts_it_back_whole() {
        let scratch = Scratch::new("fennecdb-applied");
        let dir = &scratch.0;
        let writes = [
            Write::Insert,
            Write::Over,
            Write::Set,
            Write::Del,
            Write::Upsert,
        ];
        // The graph's own points but its placing a vector, the first and
        // the second of a batch: a replica links a batch, never leaves one
        // waiting.
        let points: Vec<&'static str> = (POINTS.iter().copied())
            .filter(|p| !p.starts_with("graph-"))
            .chain(["graph-link", "graph-link*1", "applied", "before"])
            .collect();
        let (mut reached, mut failed) = (Vec::new(), Vec::new());
        for w in writes {
            for shape in SHAPES {
                let (records, after) = shaped(w, shape);
                let name = format!("{w:?}{shape:?}");
                for &at in &points {
                    match catch_unwind(AssertUnwindSafe(|| {
                        applied(dir, &name, at, &records, &after)
                    })) {
                        Ok(true) => reached.push(format!("{w:?} {at}")),
                        Ok(false) => {}
                        Err(_) => failed.push(format!("{name} {at}")),
                    }
                }
            }
        }
        drop(scratch);
        assert!(failed.is_empty(), "{} failed: {failed:#?}", failed.len());
        let mid = ["+text-term", "words", "+sparse-dim"];
        let out_mid = ["-text-term", "-sparse-dim"];
        let both: Vec<&str> = mid.iter().chain(&out_mid).copied().collect();
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
            // The graph's in every write's shapes: a batch of `u`'s, three
            // vectors in one.
            let landing = ["graph-link", "graph-link*1", "applied", "before"];
            want.extend(also.iter().chain(&landing).map(|a| format!("{w:?} {a}")));
        };
        each(Write::Insert, "+", &[], &mid);
        each(Write::Over, "+-", &["-v"], &both);
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

    /// From the sink call on, a panic fails the replica, as it fails a
    /// primary landing a block: the record in the sink's hands stays whole
    /// in memory, its writes on no feed and the counter where it was -- no
    /// record takes the numbers the file may hold -- the records landed
    /// before it stay, and those after it, applied with it as their
    /// vectors waited together, are put back: left in, they were served
    /// by the failed replica, in no record. A panic before the sink call
    /// of a record after the first puts back that one and those after it,
    /// and the replica goes on. The file opened again holds the records
    /// the sink was handed, and takes the rest.
    #[test]
    fn a_panic_landing_an_applied_record_fails_the_replica() {
        let scratch = Scratch::new("fennecdb-applied-landing");
        let (mut cases, mut failed) = (0, Vec::new());
        for shape in SHAPES {
            let (records, _) = shaped(Write::Insert, shape);
            let each = each_record(&records);
            let steps = steps(&each);
            for k in 0..each.len() {
                // The notes of the records before it, passed by.
                let noted = (steps[k].seq - steps[0].seq) as usize;
                for (at, skip, in_file) in [
                    ("before", k, false),
                    ("sink", k, false),
                    ("noted", k, true),
                    ("notes", noted, true),
                ] {
                    let tag = format!("{shape:?} at {at} of record {k}");
                    let path = scratch.0.join(format!("{shape:?}{at}{k}.fennec"));
                    cases += 1;
                    let case = || {
                        let mut db = replica(&path);
                        arm(at, skip);
                        let r = catch_unwind(AssertUnwindSafe(|| db.apply(&records)));
                        assert!(r.is_err(), "{tag}: no panic");
                        assert!(!db.in_block(), "{tag}: a block left open");
                        assert!(!db.landing, "{tag}: the landing left marked");
                        let fails = at != "before";
                        assert_eq!(db.failure().is_some(), fails, "{tag}: failed");
                        let held = match fails {
                            true => State {
                                seq: steps[k].seq,
                                ..steps[k + 1].clone()
                            },
                            false => steps[k].clone(),
                        };
                        assert_eq!(state(&db), held, "{tag}: in memory");
                        unfed(&db, steps[k].seq, &tag);
                        let rest = each[k..].concat();
                        if fails {
                            let again = db.apply(&rest);
                            assert!(matches!(again, Err(Error::Io(_))), "{tag}: {again:?}");
                        } else {
                            db.apply(&rest).expect("the records after the put-back");
                            assert_eq!(state(&db), steps[each.len()], "{tag}: taken again");
                        }
                        drop(db);
                        let mut db = crate::fs::open(&path).expect("reopen");
                        if fails {
                            let landed = k + usize::from(in_file);
                            assert_eq!(state(&db), steps[landed], "{tag}: the file");
                            db.apply(&each[landed..].concat())
                                .expect("the rest after the reopen");
                        }
                        assert_eq!(state(&db), steps[each.len()], "{tag}: the file");
                    };
                    if catch_unwind(AssertUnwindSafe(case)).is_err() {
                        failed.push(tag);
                    }
                    PANIC_AT.with(|p| p.set(None));
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(crate::synclog::path_for(&path));
                }
            }
        }
        drop(scratch);
        assert!(
            failed.is_empty(),
            "{} of {cases} failed: {failed:#?}",
            failed.len()
        );
        // Alone, after one, in a block, after three: records of one, two,
        // one and four, at four points each.
        assert_eq!(cases, 4 * (1 + 2 + 1 + 4));
    }

    /// A sink refusing every record from the `n`th it is handed on, as a
    /// full disk does; `None` takes them all.
    #[derive(Clone, Default)]
    struct Refusing(std::sync::Arc<std::sync::Mutex<Option<usize>>>);

    impl Sink for Refusing {
        fn append(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn record(&mut self, _: u64, _: &[u8]) -> Result<()> {
            let mut n = self.0.lock().unwrap();
            match *n {
                Some(0) => Err(Error::Io("a test's refusal".into())),
                Some(k) => {
                    *n = Some(k - 1);
                    Ok(())
                }
                None => Ok(()),
            }
        }
        fn rewrite(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }

    /// The sink's refusal of a record puts back it and every record after
    /// it, applied with it as their vectors waited together, and stops
    /// writes, as a primary's refused block is put back: the records
    /// before it landed stay. Left in, the records after it were served by
    /// the failed replica, in no record and on no feed -- the three rows of
    /// `u` of [`Shape::Three`] refused at its first.
    #[test]
    fn a_refused_record_is_put_back_with_every_record_after_it() {
        let (mut cases, mut failed) = (0, Vec::new());
        for shape in SHAPES {
            let (records, _) = shaped(Write::Insert, shape);
            let each = each_record(&records);
            let steps = steps(&each);
            for k in 0..each.len() {
                let tag = format!("{shape:?}, record {k} refused");
                cases += 1;
                let case = || {
                    let sink = Refusing::default();
                    let mut db = made_on(Database::with_sink(Box::new(sink.clone())));
                    db.follow(vec![(7, 0)]).expect("follow");
                    assert_eq!(state(&db), steps[0], "{tag}: made");
                    *sink.0.lock().unwrap() = Some(k);
                    let r = db.apply(&records);
                    assert!(matches!(r, Err(Error::Io(_))), "{tag}: {r:?}");
                    assert!(db.failure().is_some(), "{tag}: writes go on");
                    assert!(!db.landing, "{tag}: the landing left marked");
                    assert_eq!(state(&db), steps[k], "{tag}: put back");
                    unfed(&db, steps[k].seq, &tag);
                };
                if catch_unwind(AssertUnwindSafe(case)).is_err() {
                    failed.push(tag);
                }
            }
        }
        assert!(
            failed.is_empty(),
            "{} of {cases} failed: {failed:#?}",
            failed.len()
        );
    }

    /// A watcher keeping each change it is told the counter reached: what
    /// wakes a server's readers waiting for a write (`Hub`).
    #[derive(Clone, Default)]
    struct Heard(std::sync::Arc<std::sync::Mutex<Vec<u64>>>);

    impl Watcher for Heard {
        fn notify(&self, seq: u64) {
            self.0.lock().unwrap().push(seq);
        }
    }

    /// A replica in memory over [`made_on`]'s rows, following, its sink
    /// refusing what the test says and its watcher heard.
    fn heard_replica() -> (Database, Refusing, Heard) {
        let sink = Refusing::default();
        let mut db = made_on(Database::with_sink(Box::new(sink.clone())));
        db.follow(vec![(7, 0)]).expect("follow");
        let heard = Heard::default();
        db.set_watcher(std::sync::Arc::new(heard.clone()));
        (db, sink, heard)
    }

    /// A panic before a record's sink call puts back that one and those
    /// after it, and keeps those landed before it in the same `apply`,
    /// noted and counted: whoever waits for one of them is woken. Told
    /// only as `apply` returned, the watcher heard nothing, and a reader
    /// waiting on the replica for the first record's change waited on --
    /// the next `apply` began past it.
    #[test]
    fn a_panic_after_a_record_landed_wakes_whoever_waits_for_it() {
        let (records, _) = shaped(Write::Insert, Shape::After);
        let each = each_record(&records);
        let steps = steps(&each);
        let (mut db, _, heard) = heard_replica();
        arm("before", 1);
        let r = catch_unwind(AssertUnwindSafe(|| db.apply(&records)));
        assert!(r.is_err(), "no panic");
        assert!(db.failure().is_none(), "the replica failed");
        assert_eq!(
            state(&db),
            steps[1],
            "the first landed, the second put back"
        );
        assert_eq!(
            *heard.0.lock().unwrap(),
            [steps[1].seq],
            "the first's waiters"
        );
    }

    /// `x`: a vector in a graph of its own and words under `@text`, made
    /// and dropped among a replica's records.
    fn x_schema() -> Schema {
        let vector = VectorIndexSpec::default().resolved();
        Schema::new(
            "x",
            vec![
                Field::new("w", DataType::Vector(4, VecPrec::F32))
                    .indexed(IndexKind::Vector(vector)),
                Field::new("words", DataType::Text)
                    .indexed(IndexKind::Text(TextIndexSpec::default())),
            ],
        )
        .expect("x")
    }

    fn put_x(k: i64) -> Statement {
        let doc = [
            ("w", Value::Vector(vec![k as f32, 0.25, 1.0, 0.5])),
            ("words", text(&format!("ember{k} quartz"))),
        ];
        Statement::Put {
            collection: "x".into(),
            docs: vec![(doc.into_iter())
                .map(|(f, v)| (f.to_string(), Expr::Lit(v)))
                .collect()],
            docs_param: None,
            insert: true,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        }
    }

    /// Eight records in one `apply`, schema changes among them and writes
    /// to three collections, landing in three groups as their vectors wait
    /// together: a `put` to `u`, one to `t`, `x` made (the first group,
    /// linked as `x` is made), a block of a `put` to `x`, one to `u` and
    /// another to `x`, a `put` to `u`, one to `x`, `x` dropped (the second,
    /// linked as `x` goes), and a `put` to `u`. Each record refused, a
    /// panic before its sink call and one in it: the records before it
    /// landed and kept, it whole in memory where the sink had it, those
    /// after it put back -- a collection made taken off, one dropped back
    /// with its rows and indexes -- the replica failed but before the sink
    /// call, nothing of the log left behind, and the watcher told of the
    /// records landed. Put back before the sink call, the rest applied
    /// again give the primary's state.
    #[test]
    fn records_making_and_dropping_a_collection_are_put_back_from_any_one_on() {
        let (records, primary) = landed(|p| {
            p.execute(&put_u(3))?;
            p.execute(&write(Write::Insert))?;
            p.execute(&Statement::CreateCollection {
                schema: x_schema(),
                if_not_exists: false,
            })?;
            p.begin()?;
            p.execute(&put_x(1))?;
            p.execute(&put_u(4))?;
            p.execute(&put_x(2))?;
            p.commit()?;
            p.execute(&put_u(5))?;
            p.execute(&put_x(3))?;
            p.execute(&Statement::DropCollection {
                name: "x".into(),
                if_exists: false,
            })?;
            p.execute(&put_u(6)).map(drop)
        });
        let each = each_record(&records);
        assert_eq!(each.len(), 8, "the records");
        let steps = steps(&each);
        assert_eq!(steps[8], primary, "the replica's state is the primary's");
        let (mut cases, mut failed) = (0, Vec::new());
        for k in 0..each.len() {
            for how in ["refused", "before", "sink"] {
                let tag = format!("record {k}, {how}");
                cases += 1;
                let case = || {
                    let (mut db, sink, heard) = heard_replica();
                    assert_eq!(state(&db), steps[0], "{tag}: made");
                    match how {
                        "refused" => *sink.0.lock().unwrap() = Some(k),
                        at => arm(at, k),
                    }
                    let r = catch_unwind(AssertUnwindSafe(|| db.apply(&records)));
                    match r {
                        Ok(r) if how == "refused" => {
                            assert!(matches!(r, Err(Error::Io(_))), "{tag}: {r:?}")
                        }
                        Ok(r) => panic!("{tag}: no panic but {r:?}"),
                        Err(_) => assert!(how != "refused", "{tag}: a panic"),
                    }
                    let fails = how != "before";
                    assert_eq!(db.failure().is_some(), fails, "{tag}: failed");
                    assert!(!db.in_block(), "{tag}: a block left open");
                    assert!(!db.landing, "{tag}: the landing left marked");
                    assert!(db.spare.was.is_empty(), "{tag}: entries left in the log");
                    assert!(db.spare.marks.is_empty(), "{tag}: marks left in the log");
                    let held = match how {
                        "sink" => State {
                            seq: steps[k].seq,
                            ..steps[k + 1].clone()
                        },
                        _ => steps[k].clone(),
                    };
                    assert_eq!(state(&db), held, "{tag}: in memory");
                    unfed(&db, steps[k].seq, &tag);
                    let woken = (k > 0).then_some(steps[k].seq);
                    assert_eq!(
                        *heard.0.lock().unwrap(),
                        Vec::from_iter(woken),
                        "{tag}: woken"
                    );
                    if !fails {
                        db.apply(&each[k..].concat()).expect("the rest");
                        assert_eq!(state(&db), primary, "{tag}: the rest applied");
                    }
                };
                if catch_unwind(AssertUnwindSafe(case)).is_err() {
                    failed.push(tag);
                }
                PANIC_AT.with(|p| p.set(None));
            }
        }
        assert!(
            failed.is_empty(),
            "{} of {cases} failed: {failed:#?}",
            failed.len()
        );
        assert_eq!(cases, 24);
    }

    /// A frame that does not read -- a payload no document, an op neither
    /// a put nor a del -- is refused as corrupt before it changes anything,
    /// alone and after a frame that read, which is put back. Read from the
    /// store after the frame went in, a payload that did not read was put
    /// back by difference into indexes it had never left, `t`'s row 1 in
    /// its text indexes twice; and an op of neither was taken, the row out
    /// of its indexes and the store still holding it. Alone, nothing of it
    /// is touched: `t`'s graph counts no change. Taken into the store and
    /// refused after, a payload's frame was put back as a torn write --
    /// what an error's put-back asks, which held the rows and indexes as
    /// before -- its row taken out of the graph and placed in it again.
    #[test]
    fn a_frame_that_does_not_read_changes_nothing() {
        let (records, after) = landed(|p| p.execute(&write(Write::Over)).map(drop));
        let one = record_at(&records, &mut 0).expect("a record");
        assert_eq!(one.kind, REC_DATA);
        let scratch = Scratch::new("fennecdb-applied-unread");
        let path = scratch.0.join("unread.fennec");
        let mut db = replica(&path);
        let before = state(&db);
        let graphs = |db: &Database| -> Vec<u64> {
            (db.collections["t"].vectors.iter())
                .map(|(_, g)| g.changes())
                .collect()
        };
        let bad = [
            ("a payload no document", Store::frame(OP_PUT, 1, &[0xfe; 4])),
            ("an op of neither", Store::frame(7, 1, &[])),
        ];
        for (what, frame) in &bad {
            for first in [false, true] {
                let tag = format!("{what}{}", if first { ", after one" } else { "" });
                let mut body = match first {
                    true => one.body.to_vec(),
                    false => Vec::new(),
                };
                body.extend_from_slice(frame);
                let graphed = graphs(&db);
                let r = db.apply(&framed(REC_DATA, one.cid, &body));
                assert!(matches!(r, Err(Error::Corrupt(_))), "{tag}: {r:?}");
                assert!(db.failure().is_none(), "{tag}: the replica failed");
                if !first {
                    assert_eq!(graphs(&db), graphed, "{tag}: the graph touched");
                }
                assert_eq!(state(&db), before, "{tag}: put back");
                unfed(&db, before.seq, &tag);
            }
        }
        db.apply(&records).expect("the record");
        assert_eq!(state(&db), after, "taken");
        drop(db);
        let db = crate::fs::open(&path).expect("reopen");
        assert_eq!(state(&db), after, "the file");
    }

    /// A schema change applied, with a panic at each point it reaches:
    /// put back whole -- the schema, every index it built or let go of,
    /// a collection made or dropped -- and taken again. Alone and after a
    /// record whose vector waits on the graph's batch, which the change
    /// takes in first.
    #[test]
    fn a_panic_applying_a_schema_change_puts_it_back_whole() {
        let scratch = Scratch::new("fennecdb-applied-schema");
        let dir = &scratch.0;
        let u = |change: Alter| Statement::AlterCollection {
            collection: "u".into(),
            change,
        };
        let z = Schema::new(
            "z",
            vec![Field::new("q", DataType::Int).indexed(IndexKind::HASH)],
        )
        .expect("z");
        let changes: Vec<(&str, Option<Statement>, &[&str])> = vec![
            (
                "index",
                Some(Statement::CreateIndex {
                    collection: "u".into(),
                    field: "note".into(),
                    kind: IndexKind::Text(TextIndexSpec::default()),
                    if_not_exists: false,
                }),
                &["+text-term"],
            ),
            (
                "composite",
                Some(Statement::CreateComposite {
                    collection: "u".into(),
                    index: Composite {
                        keys: vec![("k".into(), false), ("note".into(), false)],
                        unique: false,
                    },
                    if_not_exists: false,
                }),
                &[],
            ),
            (
                "uncomposite",
                Some(Statement::DropComposite {
                    collection: "t".into(),
                    keys: vec!["tag".into(), "n".into()],
                    if_exists: false,
                }),
                &[],
            ),
            (
                "add",
                Some(u(Alter::AddField(
                    Field::new("rank", DataType::Int).indexed(IndexKind::SORTED),
                ))),
                &[],
            ),
            ("dropfield", Some(u(Alter::DropField("w".into()))), &[]),
            (
                "rename",
                Some(u(Alter::RenameField("k".into(), "key".into()))),
                &[],
            ),
            (
                "drop",
                Some(Statement::DropCollection {
                    name: "u".into(),
                    if_exists: false,
                }),
                &[],
            ),
            (
                "create",
                Some(Statement::CreateCollection {
                    schema: z,
                    if_not_exists: false,
                }),
                &[],
            ),
            // An index record no statement writes now, which rebuilds them
            // all: `k`'s index another kind, and `note` given one.
            ("rebuild", None, &["+text-term"]),
        ];
        let (mut want, mut reached, mut failed) = (Vec::new(), Vec::new(), Vec::new());
        for (name, change, points) in &changes {
            for after in [false, true] {
                let (mut records, _) = landed(|p| {
                    if after {
                        p.execute(&put_u(3))?;
                    }
                    if let Some(c) = change {
                        p.execute(c)?;
                    }
                    Ok(())
                });
                if change.is_none() {
                    let p = made_on(Database::new());
                    let c = &p.collections["u"];
                    let mut s = Schema::clone(&c.schema);
                    s.fields[0].index = IndexKind::SORTED;
                    s.fields[2].index = IndexKind::Text(TextIndexSpec::default());
                    records.extend_from_slice(&framed(REC_ALTER, c.id, &s.encode()));
                }
                // What a replica holds once it took them.
                let mut taken = made_on(Database::new());
                taken.apply(&records).expect("the records");
                let taken = state(&taken);
                let tag = format!("{name}{}", if after { "After" } else { "" });
                for &at in ["applied", "before"].iter().chain(points.iter()) {
                    want.push(format!("{name} {at}"));
                    match catch_unwind(AssertUnwindSafe(|| {
                        applied(dir, &tag, at, &records, &taken)
                    })) {
                        Ok(true) => reached.push(format!("{name} {at}")),
                        Ok(false) => {}
                        Err(_) => failed.push(format!("{tag} {at}")),
                    }
                }
            }
        }
        drop(scratch);
        assert!(failed.is_empty(), "{} failed: {failed:#?}", failed.len());
        want.sort();
        want.dedup();
        reached.sort();
        reached.dedup();
        let missed: Vec<_> = want.iter().filter(|w| !reached.contains(w)).collect();
        assert!(missed.is_empty(), "never reached: {missed:?}");
    }

    /// A record refused part way -- a block's second write, to a collection
    /// the replica does not hold -- is put back whole, and with it the
    /// record before it, waiting on the graph's batch: its first write
    /// stayed, in no record and on no feed.
    #[test]
    fn a_record_refused_part_way_is_put_back_whole() {
        let (records, _) = landed(|p| p.execute(&put_u(3)).map(drop));
        let r = record_at(&records, &mut 0).expect("a record");
        let mut inner = Vec::new();
        frame_into(&mut inner, REC_DATA, r.cid, r.body);
        frame_into(&mut inner, REC_DATA, 99, r.body);
        let refused = framed(REC_BLOCK, 0, &inner);
        let scratch = Scratch::new("fennecdb-applied-refused");
        let path = scratch.0.join("refused.fennec");
        let mut db = replica(&path);
        let before = state(&db);
        let mut two = records.clone();
        two.extend_from_slice(&refused);
        for (what, bytes) in [("alone", &refused), ("after one", &two)] {
            let r = db.apply(bytes);
            assert!(matches!(r, Err(Error::Corrupt(_))), "{what}: {r:?}");
            assert!(!db.in_block(), "{what}: a block left open");
            assert_eq!(state(&db), before, "{what}: put back");
            unfed(&db, before.seq, what);
        }
        db.apply(&records).expect("the record alone");
        assert_eq!(db.change_seq(), before.seq + 1);
        drop(db);
        let db = crate::fs::open(&path).expect("reopen");
        assert_eq!(db.change_seq(), before.seq + 1);
    }
}
