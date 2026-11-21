//! A node's tenants synced under `--sync <ms>`, each on a thread of its own
//! (docs/design/durability.md#a-nodes-syncs).
//!
//! A file's syncer (`fennec-server`'s `run_syncer`) sleeps the interval,
//! flushes under the write lock and fsyncs after it, so a write answered
//! just after a sync took its writes is on disk within the interval and two
//! syncs' own time. A node's pass synced its dirty tenants one after
//! another on the main loop's thread and slept the interval after the last:
//! a tenant's window took in every sync before it in the pass, and the next
//! pass waited for the slowest, so it grew with the tenants a pass synced
//! and with any one's slow fsync -- or with a writer holding one's lock
//! through a spill, which the pass's look at it waited out. Two tenants on
//! the power-cut rig handed a write over 772 and 734 ms after its answer
//! (runs 43696612848 and 43697476087), past the 650 ms a file's window
//! holds, and on a laptop a node of 32 tenants' pages came 415-419 ms apart
//! at the median where a file's come 256 (`make node-sync-bench`).
//!
//! Here a clock of its own ([`Tenants::sync_every`]) hands each dirty
//! tenant to a thread every interval ([`Tenants::sync_each`]) and goes on:
//! each sync is the pass's own -- the flush under the tenant's write lock,
//! the fsync after it, no wait for the streams (`sync_now`) -- and a tenant
//! still syncing is passed over, its next sync a tick after. A tenant's
//! window is the interval and its own syncs, a file's; a slow one holds a
//! thread, never the others, nor the clock -- a look that finds its lock
//! taken hands it over unlooked at, its thread waiting for the lock.

use super::{sync_now, Tenant, Tenants};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::Duration;

/// How many tenants a node syncs at once. A sync waits on the disk, not
/// the processor, and a thread is made only where more tenants wait than
/// threads are free, so a node of a few tenants holds a few. Past this
/// many slow at once, the rest wait their turn.
pub(crate) const THREADS: usize = 16;

/// The node's sync threads and the tenants waiting for one.
#[derive(Default)]
pub(crate) struct Syncs {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    queue: Mutex<Queue>,
    ready: Condvar,
    /// Told as the last sync under way ends, for [`Syncs::close`].
    idle: Condvar,
    /// The one shutdown, which a sync's panic asks for: the node never
    /// serves on with a tenant whose writes nothing syncs. Unset where no
    /// clock runs (a test's own calls), where the panic is logged alone.
    stop: OnceLock<fn()>,
    /// Whether a thread that could not be made was said.
    said: AtomicBool,
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<Arc<Tenant>>,
    threads: usize,
    busy: usize,
    /// The registry is gone or shutting down ([`Syncs::close`]): nothing
    /// more is handed over, and each thread ends once nothing waits.
    closed: bool,
}

impl Drop for Syncs {
    fn drop(&mut self) {
        lock(&self.inner.queue).closed = true;
        self.inner.ready.notify_all();
    }
}

fn lock(m: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Syncs {
    /// Queues `t`'s sync for a free thread, making one where every thread
    /// is taken and fewer than [`THREADS`] run. Where none can be made and
    /// none runs, the sync runs here, as the pass ran it.
    pub(super) fn hand(&self, t: Arc<Tenant>) {
        let mut q = lock(&self.inner.queue);
        // The shutdown's own sync takes it in, under the lock it keeps.
        if q.closed {
            return;
        }
        q.waiting.push_back(t);
        // A sync run here, where no thread could be made, counts as busy
        // with no thread of its own: `busy` may pass `threads`.
        let free = q.threads.saturating_sub(q.busy);
        if q.waiting.len() <= free || q.threads >= THREADS {
            drop(q);
            self.inner.ready.notify_one();
            return;
        }
        q.threads += 1;
        drop(q);
        let inner = Arc::clone(&self.inner);
        let made = std::thread::Builder::new()
            .name("fennec-sync".into())
            .spawn(move || inner.work());
        if let Err(e) = made {
            if !self.inner.said.swap(true, Ordering::Relaxed) {
                crate::log!(
                    "a tenant's sync thread could not be made ({e}): synced by those running"
                );
            }
            let mut q = lock(&self.inner.queue);
            q.threads -= 1;
            if q.threads > 0 || q.closed {
                return;
            }
            let here: Vec<_> = q.waiting.drain(..).collect();
            q.busy += 1;
            drop(q);
            let _busy = Busy(&self.inner);
            for t in here {
                run(&t);
            }
        }
    }

    /// Ends the node's syncs for its shutdown, before it keeps a tenant's
    /// lock ([`Tenants::shutdown_with`]): nothing more is handed over, what
    /// waits is let go -- the shutdown's own sync takes it in -- and every
    /// sync under way is waited for. One left running reached for a lock
    /// the shutdown keeps to the exit, or for the write lock again to note
    /// a failed fsync (`Database::fail`), and waited for good, holding its
    /// tenant.
    pub(crate) fn close(&self) {
        let mut q = lock(&self.inner.queue);
        q.closed = true;
        let left = std::mem::take(&mut q.waiting);
        while q.busy > 0 {
            q = self.inner.idle.wait(q).unwrap_or_else(|e| e.into_inner());
        }
        drop(q);
        self.inner.ready.notify_all();
        drop(left);
    }
}

/// A sync under way, counted until it ends -- a panic's unwinding too, or
/// [`Syncs::close`] would wait for it for good.
struct Busy<'a>(&'a Inner);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        let mut q = lock(&self.0.queue);
        q.busy -= 1;
        if q.busy == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl Inner {
    /// A sync thread: the tenants handed over, one at a time, until the
    /// registry is gone. A panic is caught where it runs and stops the node
    /// through its one shutdown, whose final sync takes in every tenant.
    fn work(self: Arc<Self>) {
        loop {
            let t = {
                let mut q = lock(&self.queue);
                loop {
                    if let Some(t) = q.waiting.pop_front() {
                        q.busy += 1;
                        break t;
                    }
                    if q.closed {
                        return;
                    }
                    q = self.ready.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            let busy = Busy(&self);
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&t)));
            // Counted busy until the panic has asked for the stop: let go
            // before, a shutdown waiting in `close` went on and could exit
            // with code 0 before `stop` stored the failure.
            if let Err(p) = ran {
                let words = crate::schedule::panic_words(p.as_ref());
                match self.stop.get() {
                    Some(stop) => {
                        crate::log!(
                            "tenant `{}`'s sync panicked ({words}): the node stops",
                            t.name
                        );
                        stop();
                    }
                    None => crate::log!("tenant `{}`'s sync panicked ({words})", t.name),
                }
            }
            drop(busy);
        }
    }
}

/// One tenant's sync, as the pass made it; its mark let go however it ends,
/// a panic's unwinding too, or the tenant would never be handed over again.
fn run(t: &Tenant) {
    struct Done<'a>(&'a AtomicBool);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _done = Done(&t.syncing);
    // After a storage error nothing is clean again, and every sync would
    // log it; the writes that follow are refused and say so.
    let pending = {
        let g = t.read();
        g.is_dirty() && g.failure().is_none()
    };
    if pending {
        sync_now(t);
    }
}

impl Tenants {
    /// Hands every dirty open tenant to the node's sync threads and returns
    /// without waiting for any. A tenant still queued or syncing is passed
    /// over: its sync is under way. One whose lock a writer holds -- a
    /// spill, a long block -- is handed over unlooked at, its thread
    /// waiting for the lock rather than the clock.
    pub fn sync_each(&self) {
        for t in self.open_tenants() {
            let pending = match t.db.try_read() {
                Ok(g) => g.is_dirty() && g.failure().is_none(),
                Err(TryLockError::Poisoned(p)) => {
                    let g = p.into_inner();
                    g.is_dirty() && g.failure().is_none()
                }
                Err(TryLockError::WouldBlock) => true,
            };
            if pending && !t.syncing.swap(true, Ordering::AcqRel) {
                self.syncs.hand(t);
            }
        }
    }

    /// `--sync <ms>` on a node: a thread that hands the dirty tenants over
    /// every `every` ([`Self::sync_each`]), counted from its last look,
    /// until the registry is gone or shutting down -- the shutdown's sync,
    /// under the locks it keeps to the exit, takes in what is left. `stop`
    /// is the one shutdown for a failure, asked for should the thread or a
    /// sync panic.
    /// An `Err` where the thread cannot be made, for the server to stop on.
    pub fn sync_every(
        self: &Arc<Self>,
        every: Duration,
        stop: fn(),
    ) -> std::result::Result<(), String> {
        let _ = self.syncs.inner.stop.set(stop);
        let me = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("fennec-syncer".into())
            .spawn(move || {
                let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                    std::thread::sleep(every);
                    let Some(tenants) = me.upgrade() else { return };
                    if tenants.shutting_down.load(Ordering::SeqCst) {
                        return;
                    }
                    tenants.sync_each();
                }));
                if let Err(p) = ended {
                    let words = crate::schedule::panic_words(p.as_ref());
                    crate::log!("the tenants' syncer stopped ({words}): the node stops with it");
                    stop();
                }
            })
            .map(|_| ())
            .map_err(|e| format!("could not start the tenants' syncer: {e}"))
    }
}

/// A test's hold on a tenant's sync, by name: [`pass`], called between the
/// flush and the fsync, waits while the name is held -- a disk slow for one
/// tenant alone, which no disk here is on demand.
#[cfg(test)]
pub(super) mod held {
    use std::collections::HashMap;
    use std::sync::{Condvar, Mutex};

    /// Each name held, and how many of its syncs wait at the hold now.
    static HELD: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
    static MOVED: Condvar = Condvar::new();

    pub fn hold(name: &str) {
        let mut g = HELD.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(HashMap::new).insert(name.into(), 0);
    }

    pub fn release(name: &str) {
        let mut g = HELD.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(HashMap::new).remove(name);
        MOVED.notify_all();
    }

    /// Whether a sync of `name` waits at the hold, waiting up to `within`
    /// for one to.
    pub fn waiting(name: &str, within: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + within;
        let mut g = HELD.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if g.as_ref().and_then(|m| m.get(name)).is_some_and(|n| *n > 0) {
                return true;
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            g = MOVED
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub(in crate::tenants) fn pass(name: &str) {
        let mut g = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if !g.as_ref().is_some_and(|m| m.contains_key(name)) {
            return;
        }
        *g.get_or_insert_with(HashMap::new).get_mut(name).unwrap() += 1;
        MOVED.notify_all();
        while g.as_ref().is_some_and(|m| m.contains_key(name)) {
            g = MOVED.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }
}
