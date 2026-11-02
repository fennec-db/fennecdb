//! A panic as a block lands, at each point a test can make one: before the
//! sink call the block is put back and the database goes on; from the sink
//! call through the block's notes on the feed to its end, the database is
//! failed, so that no later block takes the change numbers a record in the
//! file holds already (`docs/design/engine.md`, *Blocks*).

use std::cell::Cell;

thread_local! {
    /// The point the next landing on this thread panics at, once.
    static PANIC_AT: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// A panic where a test asked for one at `at`: a site in the landing
/// compiled into the tests alone.
pub(super) fn panic_at(at: &'static str) {
    if PANIC_AT.with(|p| p.get()) == Some(at) {
        PANIC_AT.with(|p| p.set(None));
        panic!("a test's panic at {at}");
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
