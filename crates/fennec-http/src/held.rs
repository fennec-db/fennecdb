//! The database's lock, taken whole: a poisoned lock -- a thread that
//! panicked holding it -- is taken as it stands rather than turned into a
//! panic of every request after it. A write leaves the database as a block
//! landed or put back, never half of one, its hooks in place -- a panic's
//! too, the engine's put back by the engine (`Database::execute_with`),
//! one between statements by [`Writing`] -- so what it guarded is whole.

use fennec_core::engine::Prepared;
use fennec_core::prelude::*;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// The database to read: whatever a read lock finds has landed, since a
/// block of writes is open only under the write lock of whoever opened it.
///
/// The wait for it is a span of the request this thread serves, when it is
/// traced: a read waits out a write's block, and a write every read.
pub fn read(db: &RwLock<Database>) -> RwLockReadGuard<'_, Database> {
    let waiting = crate::trace::span("lock.wait");
    waiting.attr("fennec.lock", "read");
    db.read().unwrap_or_else(|e| e.into_inner())
}

/// The database to write into.
pub fn write(db: &RwLock<Database>) -> Writing<'_> {
    let waiting = crate::trace::span("lock.wait");
    waiting.attr("fennec.lock", "write");
    Writing(db.write().unwrap_or_else(|e| e.into_inner()))
}

/// The write lock, held: a panic unwinding while it is held puts back the
/// block it leaves open before the lock is let go of. The engine puts back
/// a block a panic unwinds out of one of its writes through
/// (`Database::execute_with`); this is the block a keyed write, a REST
/// write or an MCP call opened and left between statements, the panic the
/// server's own -- rendering an answer, keeping a key. Left open, the next
/// request read its writes as landed and ran its own in it, never landing.
/// A panic's count read as the guard drops: nothing a write pays for.
pub struct Writing<'a>(RwLockWriteGuard<'a, Database>);

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

/// The documents the `put`s among `stmts` write, built and encoded under
/// the read lock, beside the readers, for the write lock to take
/// ([`Database::prepare`]): a `/batch` of 1 000 single puts held the write
/// lock for all of their building. Nothing, and no lock taken, for a
/// statement of a few documents ([`Prepared::worth`]).
pub fn prepared(db: &RwLock<Database>, stmts: &[(&Statement, &[Value])]) -> Prepared {
    match Prepared::worth(stmts) {
        true => read(db).prepare(stmts),
        false => Prepared::default(),
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

    /// A panic of the server's own, between a block's statements, with the
    /// write lock held: the next to take the lock finds the block put back,
    /// a reader and a writer alike.
    #[test]
    fn a_block_a_panic_leaves_open_is_put_back_before_the_lock_is_let_go() {
        let lock = RwLock::new(Database::new());
        run(&mut write(&lock), "create collection t (n int)");
        let r = std::panic::catch_unwind(|| {
            let mut g = write(&lock);
            g.begin().unwrap();
            run(&mut g, "put t {n: 1}");
            panic!("rendering an answer");
        });
        assert!(r.is_err() && lock.is_poisoned());
        assert!(!read(&lock).in_block());
        assert_eq!(count(&read(&lock)), Value::Int(0));
        let mut g = write(&lock);
        run(&mut g, "put t {n: 2}");
        assert!(!g.in_block(), "a lone write runs in no block left open");
        assert_eq!(count(&g), Value::Int(1));
    }
}
