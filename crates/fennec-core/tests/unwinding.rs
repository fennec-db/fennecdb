//! A panic unwinding out of a write -- a hook's, a fence's, the engine's --
//! leaves the database as a failed write does: its block put back, whoever
//! opened it, and the hooks and functions it had. A server takes a lock a
//! panic poisoned as it stands (`fennec_http::held`), so whatever a panic
//! left is what every later request meets: a registry taken out for the
//! statement and never put back let every later write past a scoped
//! token's `Check`.

use fennec_core::engine::Fence;
use fennec_core::plugin::{Hook, WriteOp};
use fennec_core::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// Panics on a document holding `boom: true`, as a hook with a bug would.
struct Boom;

impl Hook for Boom {
    fn name(&self) -> &str {
        "boom"
    }
    fn before_write(&self, _: &Schema, _: WriteOp, doc: &mut Document) -> Result<()> {
        if doc.get("boom") == Some(&Value::Bool(true)) {
            panic!("a hook's bug");
        }
        Ok(())
    }
}

/// Refuses a document holding `secret: true`: a scoped token's `Check`.
struct Refuse;

impl Hook for Refuse {
    fn name(&self) -> &str {
        "refuse"
    }
    fn before_write(&self, _: &Schema, _: WriteOp, doc: &mut Document) -> Result<()> {
        match doc.get("secret") {
            Some(Value::Bool(true)) => Err(Error::Denied("secret".into())),
            _ => Ok(()),
        }
    }
}

fn run(db: &mut Database, text: &str) -> Result<Response> {
    db.execute(&fennec_ql::parse_one(text).expect("parse"))
}

/// The statement, which must panic, run as a server's connection runs it:
/// the panic caught where the connection's thread would end.
fn panics(db: &mut Database, text: &str) {
    let stmt = fennec_ql::parse_one(text).expect("parse");
    let r = catch_unwind(AssertUnwindSafe(|| db.execute(&stmt)));
    assert!(r.is_err(), "{text} did not panic");
    assert!(!db.in_block(), "{text} left its block open");
}

fn ns(db: &Database) -> Vec<i64> {
    let rs = db
        .query(
            &fennec_ql::parse_one("select n from t order by n").unwrap(),
            &[],
        )
        .unwrap()
        .rows()
        .unwrap()
        .clone();
    rs.rows
        .iter()
        .map(|r| match r.values[0] {
            Value::Int(n) => n,
            ref v => panic!("{v:?}"),
        })
        .collect()
}

fn tmp() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("fennecdb-unwinding-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("data.fennec");
    let _ = std::fs::remove_file(&path);
    (dir, path)
}

#[cfg(feature = "std-fs")]
#[test]
fn a_panicking_write_keeps_the_hooks_and_puts_its_block_back() {
    let (dir, path) = tmp();
    let mut db = fennec_core::fs::open(&path).expect("open");
    run(
        &mut db,
        "create collection t (n int, boom bool, secret bool, name text)",
    )
    .unwrap();
    run(&mut db, "put t {n: 1}").unwrap();
    db.registry_mut().register_hook(Arc::new(Refuse));
    db.registry_mut().register_hook(Arc::new(Boom));

    // A `put` of two, the second panicking in a hook: the first, written
    // in memory before it, goes with the block.
    panics(&mut db, "put t [{n: 2}, {n: 3, boom: true}]");
    assert_eq!(ns(&db), [1]);
    assert_eq!(db.registry().hooks().len(), 2, "the hooks are kept");
    // The refusing hook meets the next write, and the functions are there.
    let e = run(&mut db, "put t {n: 4, secret: true}").unwrap_err();
    assert!(matches!(e, Error::Denied(_)), "{e}");
    run(&mut db, "put t {n: 5, name: lower(\"A\")}").unwrap();

    // A `set`, the update path, panicking in its hook.
    panics(&mut db, "set t {boom: true, n: 50} where n = 5");
    assert_eq!(ns(&db), [1, 5]);
    let e = run(&mut db, "set t {secret: true} where n = 5").unwrap_err();
    assert!(matches!(e, Error::Denied(_)), "{e}");

    // A block the caller opened -- a keyed write, a `/batch` -- put back
    // with what it held before the statement that panicked.
    db.begin().unwrap();
    run(&mut db, "put t {n: 6}").unwrap();
    panics(&mut db, "put t {n: 7, boom: true}");
    assert_eq!(ns(&db), [1, 5]);

    // A block of statements, as a `/batch` runs one.
    let (a, b) = (
        fennec_ql::parse_one("put t {n: 8}").unwrap(),
        fennec_ql::parse_one("put t {n: 9, boom: true}").unwrap(),
    );
    let r = catch_unwind(AssertUnwindSafe(|| {
        db.execute_block(&[(&a, &[]), (&b, &[])])
    }));
    assert!(r.is_err() && !db.in_block());
    assert_eq!(ns(&db), [1, 5]);

    // A fence -- server code, asked as the block lands -- panicking: the
    // block was out of the database by then.
    let fence: Fence = Arc::new(|| panic!("a fence's bug"));
    db.set_fence(Some(fence));
    panics(&mut db, "put t {n: 10}");
    assert_eq!(ns(&db), [1, 5]);
    assert_eq!(db.failure(), None, "nothing reached the sink");
    db.set_fence(None);

    // The writes after it land, and only they: nothing a panic left.
    run(&mut db, "put t {n: 11}").unwrap();
    assert_eq!(ns(&db), [1, 5, 11]);
    db.sync().unwrap();
    drop(db);
    let db = fennec_core::fs::open(&path).expect("reopen");
    assert_eq!(ns(&db), [1, 5, 11]);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
