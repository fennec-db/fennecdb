//! The write lock over a database, taken whole by both that hold one: a
//! `fennec-server` connection's thread (`fennec_http::held`) and a native
//! library's call (`fennec-ffi`). Each takes a lock a panic poisoned as it
//! stands -- the panic unwinds one connection or is caught at the C
//! boundary -- so whatever the panic left is what every later request or
//! call meets.

use fennec_core::prelude::*;
use std::sync::{RwLock, RwLockWriteGuard};

/// The write lock, held: a panic unwinding while it is held puts back the
/// block it leaves open before the lock is let go of. The engine puts back
/// a block a panic unwinds out of one of its writes through
/// (`Database::execute_with`); this is the block its holder opened and
/// left between statements, the panic its holder's own -- a server
/// rendering an answer or keeping a key, the library's `run_text` or a
/// sync's `remake` between the statements of a block. Left open, the next
/// to take the lock read its writes as landed and ran its own in it, never
/// landing. A panic's count read as the guard drops: nothing a write pays
/// for.
pub struct Writing<'a>(RwLockWriteGuard<'a, Database>);

impl<'a> Writing<'a> {
    /// The write lock over `db`, taken as it stands where a panic
    /// poisoned it.
    pub fn take(db: &'a RwLock<Database>) -> Writing<'a> {
        Writing(db.write().unwrap_or_else(|e| e.into_inner()))
    }
}

impl std::ops::Deref for Writing<'_> {
    type Target = Database;
    fn deref(&self) -> &Database {
        &self.0
    }
}

impl std::ops::DerefMut for Writing<'_> {
    fn deref_mut(&mut self) -> &mut Database {
        &mut self.0
    }
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() && self.0.in_block() {
            self.0.rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(db: &mut Database, text: &str) {
        db.execute(&fennec_ql::parse_one(text).unwrap()).unwrap();
    }

    fn count(db: &Database) -> Value {
        let stmt = fennec_ql::parse_one("get t count").unwrap();
        db.query(&stmt, &[]).unwrap().rows().unwrap().rows[0].values[0].clone()
    }

    /// A panic of the holder's own between a block's statements -- as
    /// `run_text` or a sync's `remake` would, caught at the C boundary --
    /// with the write lock held: the next call finds the block put back.
    #[test]
    fn a_block_a_panic_leaves_open_is_put_back_before_the_lock_is_let_go() {
        let lock = RwLock::new(Database::new());
        run(&mut Writing::take(&lock), "create collection t (n int)");
        let r = std::panic::catch_unwind(|| {
            let mut db = Writing::take(&lock);
            db.begin().unwrap();
            run(&mut db, "put t {n: 1}");
            panic!("between a block's statements");
        });
        assert!(r.is_err() && lock.is_poisoned());
        let mut db = Writing::take(&lock);
        assert!(!db.in_block(), "the block is put back");
        assert_eq!(count(&db), Value::Int(0));
        run(&mut db, "put t {n: 2}");
        assert!(!db.in_block(), "a lone write runs in no block left open");
        assert_eq!(count(&db), Value::Int(1));
    }
}
