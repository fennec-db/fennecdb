//! The database's lock, taken whole: a poisoned lock -- a thread that
//! panicked holding it -- is taken as it stands rather than turned into a
//! panic of every request after it. A write leaves the database as a block
//! landed or put back, never half of one, its hooks in place -- a panic's
//! too, the engine's put back by the engine (`Database::execute_with`),
//! one between statements by [`Writing`], the native library's guard too
//! (`fennec_abi::held`) -- so what it guarded is whole.

pub use fennec_abi::held::Writing;
use fennec_core::engine::Prepared;
use fennec_core::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, Weak};

/// The databases whose write lock a node's shutdown keeps to the process's
/// exit (`Tenants::shutdown`), never let go. Kept as `Weak`s, which keep
/// the allocation: no database made later at one's address is taken for
/// it.
static KEPT: Mutex<Vec<Weak<RwLock<Database>>>> = Mutex::new(Vec::new());
/// Whether [`KEPT`] was ever written: a process that never shut a node
/// down takes no lock to look.
static ANY_KEPT: AtomicBool = AtomicBool::new(false);

/// Notes that the shutdown keeps `db`'s write lock to the exit, before it
/// takes it.
pub(crate) fn kept_to_exit(db: &Arc<RwLock<Database>>) {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|w| w.strong_count() > 0);
    kept.push(Arc::downgrade(db));
    ANY_KEPT.store(true, Ordering::Release);
}

/// The databases their holder let go of -- a tenant released for a delete,
/// a follow or a take-back, a server shutting down -- whose rebuild beside
/// them, or compact in its graph phase, gives up at its next step
/// ([`stops`]) rather than hold them, and their files' locks, for the whole
/// build: a delete then waited `RELEASE_WAIT` for the lock and answered
/// 500, and a create of the name after it too. As [`KEPT`], `Weak`s.
static LET_GO: Mutex<Vec<Weak<RwLock<Database>>>> = Mutex::new(Vec::new());
/// Whether [`LET_GO`] holds any: a process that let none go takes no lock
/// to look.
static ANY_LET_GO: AtomicBool = AtomicBool::new(false);

/// Notes that `db`'s holder lets it go: a rebuild under way beside it, or
/// a compact's graph phase, gives up at its next step, and none begins.
pub fn let_go(db: &Arc<RwLock<Database>>) {
    let mut gone = LET_GO.lock().unwrap_or_else(|e| e.into_inner());
    gone.retain(|w| w.strong_count() > 0);
    gone.push(Arc::downgrade(db));
    ANY_LET_GO.store(true, Ordering::Release);
}

/// Takes [`let_go`]'s note back: a release refused, the database goes on.
/// A shutdown's note goes with it, which the shutdown's [`kept_to_exit`]
/// gives again before it takes the database's lock (`Tenants::shutdown_with`).
pub fn taken_back(db: &Arc<RwLock<Database>>) {
    let mut gone = LET_GO.lock().unwrap_or_else(|e| e.into_inner());
    gone.retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), Arc::as_ptr(db)));
}

/// Whether a rebuild beside `db`, or a compact's graph phase, gives up:
/// its holder let it go ([`let_go`]), or the shutdown keeps its lock to the
/// exit ([`kept_to_exit`]), where the rebuild would wait for that lock for
/// good.
pub fn stops(db: &Arc<RwLock<Database>>) -> bool {
    listed(&LET_GO, &ANY_LET_GO, db) || listed(&KEPT, &ANY_KEPT, db)
}

/// Whether `list`, written once `any` is, holds `db`.
fn listed(
    list: &Mutex<Vec<Weak<RwLock<Database>>>>,
    any: &AtomicBool,
    db: &Arc<RwLock<Database>>,
) -> bool {
    any.load(Ordering::Acquire)
        && list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(db)))
}

/// `db` for a thread working beside the queries -- the compacter, the graph
/// keeper, the sweeper, a linking or warming thread -- unless it is gone or
/// its lock is kept to the exit ([`kept_to_exit`]). That lock is never let
/// go: in a process going on past the shutdown -- a test's -- with the
/// database still held, by a request or a follower parked at it, the
/// process's one compacter and one sweeper each parked behind it for good.
/// A pass already holding the database as the shutdown took the lock waits
/// for it still, as until a server's exit: let go, the lock would let it
/// write after the final sync.
pub fn beside(db: &Weak<RwLock<Database>>) -> Option<Arc<RwLock<Database>>> {
    let db = db.upgrade()?;
    (!listed(&KEPT, &ANY_KEPT, &db)).then_some(db)
}

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

/// The database to write into: a block a keyed write, a REST write or an
/// MCP call opens and a panic of the server's own leaves open between its
/// statements is put back as the lock drops ([`Writing`]).
pub fn write(db: &RwLock<Database>) -> Writing<'_> {
    let waiting = crate::trace::span("lock.wait");
    waiting.attr("fennec.lock", "write");
    Writing::take(db)
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
