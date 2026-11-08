//! A tenant node's scheduler (`docs/design/named-statements.md`, *Where a
//! run happens, and once*): one thread runs every tenant's scheduled
//! statements, a tenant at a time.
//!
//! **A queue by name, never an `Arc`.** It keeps each scheduled tenant's
//! next slot by its name -- an `Arc` of its database would keep it open
//! past `--idle-close` and the registry from closing it on its last -- and
//! opens it through the registry as a request does when its slot comes
//! ([`Tenants::for_run`]), under its slot's lock and the memory ceiling.
//! The run is no use of it: `last_used` stays as it was, a tenant opened
//! for a run counting as never used, so a client's open evicts it first
//! and a run's open evicts no tenant a client touched; and the scheduler
//! closes a tenant it opened once its run is done, unless a request opened
//! it meanwhile ([`Tenants::close_after_run`]). A closed tenant its lease
//! does not let the node write is not opened, nor one whose slot the state
//! held back until its backoff ends: neither could run.
//!
//! **A run from each before a second from any.** A look at a tenant runs
//! one slot; a tenant with another due goes to the back of the tenants due
//! (`ready`), so ten thousand tenants due at 03:00 run in turn and no
//! tenant's sixteen statements put the others behind them.
//!
//! **Through the tenant's gate and its lease.** A look holds the tenant's
//! gate as a request does (`Tenant::enter`) and passes over a frozen one,
//! so no run lands on a move's source after the export its target was made
//! from; the engine's fence refuses a run past the lease, and one landed as
//! it lapsed is counted (`unknown`). A tenant's quota is asked of the run's
//! block as it lands; its rates charge a request's own thread, which the
//! scheduler's is not. An installed tenant -- a move's target -- is first
//! looked at [`ARRIVING`] after its install: a move undone takes the file
//! back inside the move's own call, before it runs the slot its source then
//! runs.
//!
//! **One tenant's panic is that tenant's.** A look that panics outside the
//! run's own catch holds its tenant back, the backoff a held slot waits
//! (a minute, doubling to [`super::RETRY_MAX`]), counted and logged by its
//! name; the node goes on with every other. A panic outside any tenant's
//! look -- the loop, the hint's write -- stops the node through the one
//! shutdown, as anything beside the database does.
//!
//! **The hint** ([`super::hint`]) names the tenants holding a schedule and
//! those alone, each with its next slot and a closed tenant's pending
//! marks: a tenant enters it, synced, before every scheduled definition
//! writes anything (`Database::on_schedules`), before an installed file is
//! put in place, as it opens holding one -- a restore -- and as one reaches
//! it -- a replica's apply, a promotion -- and leaves it as it closes
//! holding none, its file synced by the close. A node starting with no
//! hint opens each tenant once, on the scheduler's thread before its first
//! look, to write one.

use super::hint::{self, Kept, Stood};
use super::{lock, look_at, now_ms, panic_words, Ctx, Look, Run, State, MINUTE, NONE, RETRY_MAX};
use crate::tenants::{Refused, Tenant, Tenants};
use fennec_core::engine::Due;
use fennec_core::error::Error;
use fennec_core::prelude::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// How long the scheduler sleeps at most with a slot to wait for: a clock
/// stepped forward is found within it.
const SLEEP_MOST: i64 = MINUTE;

/// How long an installed tenant waits before its first look. A node never
/// learns of a move's flip without a lease, and a move undone -- its quota
/// or its placement refused -- takes the target's file back within the
/// move's own call: run at once, the target ran a slot its thawed source
/// then ran again. A slot due waits as long, once.
pub const ARRIVING: i64 = MINUTE;

/// A node's scheduler: what [`Tenants`] tells of its tenants' opens,
/// closes, installs and deletes, and the thread that runs their slots.
pub struct Node {
    tenants: Weak<Tenants>,
    dir: PathBuf,
    /// `--sync always`: a run is fsynced, and waits for the streams keeping
    /// up, before the next.
    durable: bool,
    queue: Mutex<Queue>,
    /// What the thread sleeps on: a slot that came, a tenant woken.
    woke: Condvar,
    /// The hint's file, written by one at a time.
    io: Mutex<()>,
    /// No hint was read at the start: until each tenant was opened once to
    /// learn which hold a schedule, the hint is held in memory alone and
    /// written whole once learned.
    learning: AtomicBool,
    /// The tenants a look or the learning opened: what a start that opens
    /// no tenant before its slot, and the tenants opened a second, are
    /// read by.
    opens: AtomicU64,
    /// The looks that panicked, each holding its tenant back.
    panics: AtomicU64,
    /// [`ARRIVING`], which a test shortens.
    arriving: AtomicI64,
    /// Asleep with no deadline: what a test waits for.
    #[cfg(test)]
    asleep: AtomicBool,
}

/// The scheduled tenants, by name, and when to look at each.
#[derive(Default)]
struct Queue {
    entries: HashMap<Arc<str>, Entry>,
    /// The tenants with a slot to wait for, by it. Never taken out of: an
    /// item whose tenant's `next` moved, or which left, is passed over as
    /// it comes -- an ordered set's removal was a name's copy and its
    /// B-tree's code.
    waiting: BinaryHeap<Reverse<(i64, Arc<str>)>>,
    /// The tenants due, in turn: one run each before a second from any. A
    /// tenant gone is passed over as it comes.
    ready: VecDeque<Arc<str>>,
    /// The hint differs from its file.
    dirty: bool,
    /// When it was last written whole.
    written: i64,
    /// The entries made so far: a whole write notes those made before it
    /// began.
    made: u64,
}

struct Entry {
    name: Arc<str>,
    /// When to look next, in epoch milliseconds: [`NONE`] with nothing to
    /// wait for -- a following copy, or one holding none until it closes.
    next: i64,
    /// Among the tenants due (`ready`).
    queued: bool,
    /// Told since its look began: a schedule defined, dropped or brought
    /// meanwhile, looked at again.
    woken: bool,
    /// In the hint's file, synced.
    noted: bool,
    /// Its open failed, told once until one opens.
    failed: bool,
    /// Looked at no sooner: an installed tenant's arrival ([`ARRIVING`]).
    not_before: i64,
    /// A look at it panicked: what it waits before the next, doubling.
    backoff: i64,
    /// Its marks of runs that wrote nothing as it closed.
    kept: Option<Kept>,
    /// Its slots held back and marks told ahead -- taken by its look, put
    /// back before the look closes the tenant: `None` while a look holds
    /// them.
    state: Option<State>,
    /// Opened while a look held its state, so told no `held`
    /// ([`Node::opened`]): the look tells it as it ends.
    reopened: bool,
    made: u64,
}

impl Queue {
    /// `name`'s entry, made where there is none.
    fn entry(&mut self, name: &str) -> &mut Entry {
        if !self.entries.contains_key(name) {
            self.dirty = true;
            self.made += 1;
            let name: Arc<str> = Arc::from(name);
            let e = Entry {
                name: Arc::clone(&name),
                next: NONE,
                queued: false,
                woken: false,
                noted: false,
                failed: false,
                not_before: i64::MIN,
                backoff: 0,
                kept: None,
                state: Some(State::default()),
                reopened: false,
                made: self.made,
            };
            self.entries.insert(name, e);
        }
        self.entries.get_mut(name).expect("made above")
    }

    /// Waits for `next` to look at `name`: [`NONE`] for nothing, and never
    /// before its arrival. A tenant due already waits as it is.
    fn set_next(&mut self, name: &str, next: i64) {
        let Some(e) = self.entries.get_mut(name) else {
            return;
        };
        let next = match next {
            NONE => NONE,
            t => t.max(e.not_before),
        };
        if e.queued || e.next == next {
            return;
        }
        e.next = next;
        if next != NONE {
            self.waiting.push(Reverse((next, Arc::clone(&e.name))));
        }
        self.dirty = true;
    }

    /// The tenants whose slot came by `now`, among the due, by slot and
    /// then name.
    fn come(&mut self, now: i64) {
        while self.waiting.peek().is_some_and(|w| w.0 .0 <= now) {
            let Reverse((t, name)) = self.waiting.pop().expect("looked at above");
            if let Some(e) = self.entries.get_mut(&name) {
                if e.next == t && !e.queued {
                    e.next = NONE;
                    e.queued = true;
                    self.ready.push_back(name);
                }
            }
        }
    }

    /// The next tenant due, taken out of the queue for its look, with its
    /// state.
    fn take(&mut self) -> Option<(Arc<str>, State)> {
        while let Some(name) = self.ready.pop_front() {
            if let Some(e) = self.entries.get_mut(&name).filter(|e| e.queued) {
                e.queued = false;
                e.woken = false;
                return Some((name, e.state.take().unwrap_or_default()));
            }
        }
        None
    }

    /// `name` among the due again, at the back -- or, not arrived yet,
    /// waiting for its arrival.
    fn again(&mut self, name: &str, now: i64) {
        let Some(e) = self.entries.get_mut(name) else {
            return;
        };
        if e.queued {
            return;
        }
        if e.not_before > now {
            let at = e.not_before;
            return self.set_next(name, at);
        }
        e.next = NONE;
        e.queued = true;
        self.ready.push_back(Arc::clone(&e.name));
    }

    /// `name` out of the queue and the hint: whether it was in them.
    fn remove(&mut self, name: &str) -> bool {
        let gone = self.entries.remove(name).is_some();
        self.dirty |= gone;
        gone
    }
}

/// When to look at a database whose slots are `dues` at `now`: at once
/// where one is due, else at the first next slot.
fn next_of(dues: &[Due], now: i64) -> i64 {
    match dues.iter().any(|d| d.run.is_some()) {
        true => now,
        false => dues.iter().filter_map(|d| d.next).min().unwrap_or(NONE),
    }
}

impl Node {
    /// The scheduler of `tenants`' schedules, `durable` under `--sync
    /// always`: set on the registry before any tenant opens, so each that
    /// opens takes a scheduled definition and tells it of its slots
    /// ([`Self::opened`]). Reads the hint; where there is none, or one that
    /// does not read, the node learns one as it starts ([`Self::start`]).
    /// No thread runs yet: a test drives it ([`Self::pass`]).
    pub fn attach(tenants: &Arc<Tenants>, durable: bool) -> Arc<Node> {
        let dir = tenants.dir().to_path_buf();
        let mut queue = Queue::default();
        let read = hint::read(&dir, &mut |name, line| {
            let e = queue.entry(name);
            e.noted = true;
            e.kept = line.kept;
            queue.set_next(name, line.next.unwrap_or(i64::MIN));
        });
        let learning = match read {
            // A last line cut short: written whole at the first chance.
            Ok(Some(torn)) => {
                queue.dirty = torn;
                false
            }
            Ok(None) => true,
            Err(e) => {
                crate::log!(
                    "{}: the schedule hint does not read ({e}): learned again",
                    dir.join(hint::FILE).display()
                );
                queue = Queue::default();
                true
            }
        };
        let node = Arc::new(Node {
            tenants: Arc::downgrade(tenants),
            dir,
            durable,
            queue: Mutex::new(queue),
            woke: Condvar::new(),
            io: Mutex::new(()),
            learning: AtomicBool::new(learning),
            opens: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            arriving: AtomicI64::new(ARRIVING),
            #[cfg(test)]
            asleep: AtomicBool::new(false),
        });
        tenants.set_scheduler(Arc::clone(&node));
        node
    }

    /// Starts the thread that runs the node's tenants' slots, which first
    /// learns the hint where none was read: an `Err` where it cannot be
    /// made, for the server to stop on (`stop_with`), never to serve with
    /// its schedules unrun. `stop` is the one shutdown, which the thread
    /// asks for should it end. Called after the listener is bound, since a
    /// first look may run a missed slot.
    pub fn start(self: &Arc<Self>, stop: fn(i32)) -> std::result::Result<(), String> {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("fennec-schedule".into())
            .spawn(move || {
                let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| me.running()));
                let words = match &ended {
                    Ok(()) => "its loop ended".to_string(),
                    Err(p) => panic_words(p.as_ref()),
                };
                crate::log!("the scheduler stopped ({words}): the node stops with it");
                stop(1);
            })
            .map(|_| ())
            .map_err(|e| format!("could not start the scheduler: {e}"))
    }

    /// The thread's loop: the hint learned where none was read, then the
    /// next tenant due looked at, at the clock, and the hint written where
    /// it changed, at most once a minute.
    fn running(&self) {
        if self.learning() {
            self.learn();
        }
        loop {
            let due = self.next_due();
            // Taken after the sleep: a registry let go is not held by a
            // thread asleep.
            let Some(tenants) = self.tenants.upgrade() else {
                return;
            };
            match due {
                Some((name, st)) => self.look_caught(&tenants, &name, st, now_ms(), true, None),
                None => {
                    if let Err(e) = self.write_hint() {
                        crate::log!("could not write the schedule hint: {e}");
                    }
                }
            }
        }
    }

    /// The next tenant due -- or `None` for the hint to be written --
    /// sleeping meanwhile: until the first slot comes, a minute at most,
    /// or a tenant is woken.
    fn next_due(&self) -> Option<(Arc<str>, State)> {
        let mut q = lock(&self.queue);
        loop {
            let now = now_ms();
            q.come(now);
            if let Some(due) = q.take() {
                return Some(due);
            }
            let write_at = match q.dirty && !self.learning.load(Ordering::Acquire) {
                true => q.written.saturating_add(MINUTE),
                false => NONE,
            };
            if write_at <= now {
                return None;
            }
            let first = q.waiting.peek().map_or(NONE, |w| w.0 .0);
            // With nothing to wait for -- a node holding no schedule -- no
            // deadline: it wakes for nothing, as a file's server holding
            // none does.
            q = match first.min(write_at) {
                NONE => {
                    #[cfg(test)]
                    self.asleep.store(true, Ordering::SeqCst);
                    let q = self.woke.wait(q).unwrap_or_else(|e| e.into_inner());
                    #[cfg(test)]
                    self.asleep.store(false, Ordering::SeqCst);
                    q
                }
                until => {
                    let ms = until.saturating_sub(now).clamp(1, SLEEP_MOST) as u64;
                    (self.woke.wait_timeout(q, Duration::from_millis(ms)))
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
    }

    /// A look at every tenant due at `now`, as the thread makes them, each
    /// running one slot in turn until none is left: what ran, by tenant. A
    /// test drives the node's slots with the time it names.
    pub fn pass(&self, now: i64) -> Vec<(String, Run)> {
        let mut out = Vec::new();
        let Some(tenants) = self.tenants.upgrade() else {
            return out;
        };
        loop {
            let due = {
                let mut q = lock(&self.queue);
                q.come(now);
                q.take()
            };
            let Some((name, st)) = due else { break };
            self.look_caught(&tenants, &name, st, now, false, Some(&mut out));
        }
        if let Err(e) = self.write_hint() {
            crate::log!("could not write the schedule hint: {e}");
        }
        out
    }

    /// The tenants the hint names now, with their next slot (`None`: at
    /// once): what a test reads of it.
    pub fn named(&self) -> Vec<(String, Option<i64>)> {
        let q = lock(&self.queue);
        let mut out: Vec<_> = (q.entries.values())
            .map(|e| {
                let at = e.next != NONE && e.next != i64::MIN && !e.queued;
                (e.name.to_string(), at.then_some(e.next))
            })
            .collect();
        out.sort();
        out
    }

    /// Whether the node is still learning which of its tenants hold a
    /// schedule.
    pub fn learning(&self) -> bool {
        self.learning.load(Ordering::Acquire)
    }

    /// How many tenants its looks and its learning opened.
    pub fn opens(&self) -> u64 {
        self.opens.load(Ordering::Relaxed)
    }

    /// How many looks panicked, each holding its tenant back.
    pub fn panics(&self) -> u64 {
        self.panics.load(Ordering::Relaxed)
    }

    /// How long an installed tenant waits before its first look, in
    /// milliseconds: [`ARRIVING`], which a test shortens.
    pub fn set_arriving(&self, ms: i64) {
        self.arriving.store(ms, Ordering::Relaxed);
    }

    /// [`Tenants::for_run`], counted where it opened the tenant.
    fn open_for(
        &self,
        tenants: &Tenants,
        name: &str,
        run: bool,
    ) -> std::result::Result<Option<(Arc<Tenant>, bool)>, Refused> {
        let r = tenants.for_run(name, run);
        if matches!(r, Ok(Some((_, true)))) {
            self.opens.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    /// Whether `name` was opened while its look held its state, which
    /// the look then tells it.
    fn reopened(&self, name: &str) -> bool {
        let mut q = lock(&self.queue);
        (q.entries.get_mut(name)).is_some_and(|e| std::mem::take(&mut e.reopened))
    }

    /// [`Self::look`], a panic in it caught: one tenant's look panicking
    /// holds that tenant back, the backoff a held slot waits, and the node
    /// goes on with the others. What panics outside a look -- the loop, the
    /// hint's write -- stops the node ([`Self::start`]).
    fn look_caught(
        &self,
        tenants: &Tenants,
        name: &str,
        st: State,
        now: i64,
        live: bool,
        out: Option<&mut Vec<(String, Run)>>,
    ) {
        let looked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.look(tenants, name, st, now, live, out)
        }));
        let Err(p) = looked else { return };
        // A run's id the panic left set is no later line's.
        crate::request_id::end();
        self.panics.fetch_add(1, Ordering::Relaxed);
        let back = {
            let mut q = lock(&self.queue);
            let Some(e) = q.entries.get_mut(name) else {
                return;
            };
            e.backoff = (e.backoff * 2).clamp(MINUTE, RETRY_MAX);
            let back = e.backoff;
            q.set_next(name, now.saturating_add(back));
            back
        };
        crate::log!(
            "tenant `{name}`: a look at its scheduled statements panicked ({}): it is held back \
             {} minute(s), every other tenant's going on",
            panic_words(p.as_ref()),
            back / MINUTE
        );
    }

    /// One look at `name` at `now`, `st` its state: opened where it is
    /// closed, through its gate, one slot run, closed again where the look
    /// opened it and no request used it meanwhile.
    fn look(
        &self,
        tenants: &Tenants,
        name: &str,
        mut st: State,
        now: i64,
        live: bool,
        out: Option<&mut Vec<(String, Run)>>,
    ) {
        #[cfg(test)]
        tests::panic_at(name);
        let (mut refused, mut handed) = (None, false);
        let (next, more) = match self.open_for(tenants, name, true) {
            Ok(Some((t, opened))) => {
                // Opened since the look took its state -- by the look or a
                // request -- the database was told no `held`: told as the
                // look ends.
                st.changed |= self.reopened(name);
                let (open, closed, more) = self.look_open(&t, name, now, live, &mut st, out);
                drop(t);
                // Its state back before the close: a request opening the
                // tenant again before the look ends reads `held` from it
                // (slice 5's review).
                if let Some(e) = lock(&self.queue).entries.get_mut(name) {
                    e.state = Some(std::mem::take(&mut st));
                    handed = true;
                }
                // Closed again, its held slots wait their backoff out.
                match opened && tenants.close_after_run(name) {
                    true => (closed, more),
                    false => (open, more),
                }
            }
            // Closed, and its lease lets this node write it no more: not
            // opened, looked at again within a minute.
            Ok(None) => (now.saturating_add(MINUTE), false),
            // Gone: deleted, or a file the hint named that is not there --
            // unless entered again meanwhile, an install racing the look.
            Err(Refused(404, _)) => {
                let mut q = lock(&self.queue);
                match q.entries.get(name).map(|e| e.woken) {
                    Some(true) => q.again(name, now),
                    Some(false) => {
                        q.remove(name);
                    }
                    None => {}
                }
                return;
            }
            Err(Refused(status, why)) => {
                refused = Some((status, why));
                (now.saturating_add(MINUTE), false)
            }
        };
        let mut q = lock(&self.queue);
        // Gone from the hint meanwhile -- closed holding none, deleted:
        // what was held of it goes with the look.
        let Some(e) = q.entries.get_mut(name) else {
            return;
        };
        e.backoff = 0;
        if !handed {
            e.state = Some(st);
        }
        let failed = std::mem::replace(&mut e.failed, refused.is_some());
        let refused = refused.filter(|r| !failed && r.0 != 503);
        match more || e.woken {
            true => q.again(name, now),
            false => q.set_next(name, next),
        }
        drop(q);
        if let Some((status, why)) = refused {
            crate::log!(
                "tenant `{name}` did not open for its scheduled statements ({status}): {why}; \
                 tried again each minute"
            );
        }
    }

    /// [`Self::look`] at a tenant open for it: when to look again, open
    /// and closed after it ([`look_at`]), and whether a slot is left due.
    fn look_open(
        &self,
        t: &Tenant,
        name: &str,
        now: i64,
        live: bool,
        st: &mut State,
        out: Option<&mut Vec<(String, Run)>>,
    ) -> (i64, i64, bool) {
        // Held as a request holds it: a freeze waits for the run to land,
        // and a run asked after the freeze passes over the tenant, so none
        // lands on a move's source after the export. A thaw wakes it.
        let _gate = t.enter();
        if t.is_frozen() {
            let next = now.saturating_add(MINUTE);
            return (next, next, false);
        }
        let what = format!("tenant `{name}`");
        let ctx = Ctx {
            what: &what,
            tenant: Some(name),
            durable: self.durable,
            live,
        };
        match out {
            None => look_at(&ctx, &t.db, now, st, 1, None),
            Some(out) => {
                let mut look = Look::default();
                let r = look_at(&ctx, &t.db, now, st, 1, Some(&mut look));
                out.extend(look.runs.into_iter().map(|r| (name.to_string(), r)));
                r
            }
        }
    }

    /// Tells a tenant's database, as it opens, that this node runs its
    /// schedules: it takes a scheduled definition, entering the hint before
    /// it writes anything, tells of its table's schedules as they change,
    /// and is handed back the marks it closed with where its file stands
    /// where it did. A tenant opening holding one -- an install, a restore,
    /// a move's target, a file put in its place -- enters the hint, its
    /// slots learned here. Under the tenant's slot.
    pub(crate) fn opened(self: &Arc<Self>, name: &str, db: &mut Database) {
        db.allow_schedules();
        // One hook for both, holding the node and the name: two closures
        // and two copies of the name were what every open made.
        let (me, who) = (Arc::downgrade(self), Box::<str>::from(name));
        db.on_schedules(Arc::new(move |n| {
            let Some(node) = me.upgrade() else {
                return Ok(());
            };
            match n {
                None => node.note(&who, true).map_err(|e| {
                    Error::Io(format!(
                        "the node could not note tenant `{who}` in its schedule hint, and \
                         defines no schedule a start would not find: {e}"
                    ))
                }),
                Some(n) => {
                    node.woken(&who, n);
                    Ok(())
                }
            }
        }));
        let kept = {
            let mut q = lock(&self.queue);
            let kept = q.entries.get_mut(name).and_then(|e| e.kept.take());
            q.dirty |= kept.is_some();
            kept
        };
        // Each counts only while the file stands where it stood as they were
        // taken: a write since, or another file in its place, and the slot a
        // pending mark stood for runs again over what is there now.
        if let Some(k) = kept.filter(|k| k.stood == Stood::of(db)) {
            db.keep_pending(k.marks);
        }
        if !db.holds_schedules() {
            return;
        }
        let now = now_ms();
        let next = next_of(&db.due(now), now);
        let (noted, held) = {
            let mut q = lock(&self.queue);
            let e = q.entry(name);
            // What its scheduler holds back, for its listings: kept by the
            // entry across a close, unless a look holds it now -- which
            // tells the database as it ends.
            let held = match &e.state {
                Some(st) => Some(super::shown_held(st)),
                None => {
                    e.reopened = true;
                    None
                }
            };
            let noted = e.noted;
            if next < NONE {
                q.set_next(name, next);
            }
            (noted, held)
        };
        if let Some(held) = held {
            db.set_held_slots(held);
        }
        if !noted {
            if let Err(e) = self.note(name, false) {
                crate::log!(
                    "tenant `{name}` holds a scheduled statement, and the node could not note \
                     it in its schedule hint: {e}; a restart opens it only once something does"
                );
            }
        }
        self.woke.notify_all();
    }

    /// The engine's word that `name`'s table holds `n` schedules -- a
    /// definition, a drop, a replica's apply, an image adopted, a
    /// promotion -- under its write lock, or a thaw's: the tenant looked at
    /// again, and entered where it came to hold one.
    pub(crate) fn woken(&self, name: &str, n: usize) {
        let noted = {
            let mut q = lock(&self.queue);
            if n == 0 && !q.entries.contains_key(name) {
                return;
            }
            let e = q.entry(name);
            e.woken = true;
            let noted = n == 0 || e.noted;
            q.set_next(name, i64::MIN);
            noted
        };
        if !noted {
            if let Err(e) = self.note(name, false) {
                crate::log!(
                    "tenant `{name}` came to hold a scheduled statement, and the node could not \
                     note it in its schedule hint: {e}"
                );
            }
        }
        self.woke.notify_all();
    }

    /// Names `name` in the hint, synced: a line appended -- where it is not
    /// noted yet, or `again` -- and no look queued. Before an installed
    /// file is put in place, once; before every scheduled definition writes
    /// anything, again, so a crash past it finds the tenant to look at at
    /// once, never at a later slot the hint held. While the node learns,
    /// the hint is memory's until it is written whole.
    pub(crate) fn note(&self, name: &str, again: bool) -> std::io::Result<()> {
        if lock(&self.queue).entry(name).noted && !again {
            return Ok(());
        }
        if self.learning.load(Ordering::Acquire) {
            return Ok(());
        }
        let _io = lock(&self.io);
        match hint::append(&self.dir, name) {
            Ok(()) => {
                if let Some(e) = lock(&self.queue).entries.get_mut(name) {
                    e.noted = true;
                }
                Ok(())
            }
            // No hint to append to -- one an operator removed: written whole.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.write_held(),
            Err(e) => Err(e),
        }
    }

    /// A file installed as `name`, in place now: its first look queued once
    /// it arrived ([`ARRIVING`]) and the thread woken, which may sleep with
    /// no deadline. Under the tenant's slot, which a look's open waits on,
    /// so a look never finds the file not there yet.
    pub(crate) fn installed(&self, name: &str) {
        let noted = {
            let mut q = lock(&self.queue);
            let at = now_ms().saturating_add(self.arriving.load(Ordering::Relaxed));
            let e = q.entry(name);
            e.woken = true;
            e.not_before = at;
            let noted = e.noted;
            q.set_next(name, at);
            noted
        };
        if !noted {
            if let Err(e) = self.note(name, false) {
                crate::log!(
                    "tenant `{name}` was installed holding a scheduled statement, and the node \
                     could not note it in its schedule hint: {e}"
                );
            }
        }
        self.woke.notify_all();
    }

    /// A tenant closed: the marks of its runs that wrote nothing kept by
    /// name with where its file stood, for its next open, and its next look
    /// when its slot comes. One holding no schedule leaves the hint once its
    /// close's sync was durable.
    pub(crate) fn closed(&self, name: &str, c: Closing) {
        let mut q = lock(&self.queue);
        let Some(next) = c.next else {
            // The common close, a tenant holding none: one look at the map.
            // Its drop not durable, it stays named: looked at once.
            let gone = match c.synced {
                true => q.remove(name),
                false => {
                    q.set_next(name, NONE);
                    false
                }
            };
            drop(q);
            if gone {
                self.woke.notify_all();
            }
            return;
        };
        let e = q.entry(name);
        e.kept = (!c.kept.marks.is_empty()).then_some(c.kept);
        // Its slots as its file has them, what woke it since its last look
        // among them: a start, or a look, opens it no sooner. No look holds
        // it now -- one would hold its `Arc`, and it would not close.
        e.woken = false;
        q.dirty = true;
        q.set_next(name, next);
        drop(q);
        // The hint changed: written within a minute.
        self.woke.notify_all();
    }

    /// A tenant deleted: out of the hint.
    pub(crate) fn forget(&self, name: &str) {
        if lock(&self.queue).remove(name) {
            self.woke.notify_all();
        }
    }

    /// A move's source the router deleted after its flip, the delete
    /// refused or failed: out of the hint, written whole at once, so a
    /// restart -- its freeze gone with the process -- runs no slot into a
    /// file no router sends to. A thaw enters it again ([`Self::woken`]).
    pub(crate) fn left(&self, name: &str) {
        if !lock(&self.queue).remove(name) {
            return;
        }
        if let Err(e) = self.write_hint() {
            crate::log!(
                "tenant `{name}` left the node, and the schedule hint naming it was not \
                 written again: {e}"
            );
        }
        self.woke.notify_all();
    }

    /// The node stops: the open tenants' pending marks kept in the hint,
    /// written whole and synced, as their closes would have kept them.
    pub(crate) fn shutdown(&self, open: Vec<(String, Closing)>) {
        {
            let mut q = lock(&self.queue);
            for (name, c) in open {
                if c.next.is_some() && !c.kept.marks.is_empty() {
                    q.entry(&name).kept = Some(c.kept);
                    q.dirty = true;
                }
            }
        }
        if let Err(e) = self.write_hint() {
            crate::log!("could not write the schedule hint as the node stops: {e}");
        }
    }

    /// Writes the hint whole where it changed -- never while the node
    /// learns, whose hint is written once learned.
    pub fn write_hint(&self) -> std::io::Result<()> {
        if self.learning.load(Ordering::Acquire) || !lock(&self.queue).dirty {
            return Ok(());
        }
        let _io = lock(&self.io);
        self.write_held()
    }

    /// [`Self::write_hint`], whatever it holds, the file's lock held. Its
    /// text is written under the queue's lock, which every open and close
    /// takes: no line copied, the file's write and syncs after it.
    fn write_held(&self) -> std::io::Result<()> {
        #[cfg(test)]
        tests::panic_at(&self.dir.to_string_lossy());
        let (text, made) = {
            let mut q = lock(&self.queue);
            let mut text = hint::begun();
            for e in q.entries.values() {
                let next = (e.next != NONE && e.next != i64::MIN && !e.queued).then_some(e.next);
                hint::line_into(&mut text, &e.name, next, e.kept.as_ref());
            }
            q.dirty = false;
            q.written = now_ms();
            (text, q.made)
        };
        let written = hint::write(&self.dir, &text);
        let mut q = lock(&self.queue);
        match &written {
            Ok(()) => {
                for e in q.entries.values_mut().filter(|e| e.made <= made) {
                    e.noted = true;
                }
            }
            Err(_) => q.dirty = true,
        }
        written
    }

    /// Opens each tenant once, one at a time, to learn which hold a
    /// schedule -- each that does entering the hint as it opens -- then
    /// writes the hint whole: what a node starting with none does, on the
    /// scheduler's thread before its first look, saying so as it begins. A
    /// tenant opened for it is closed again, unless a request opened it
    /// meanwhile; one whose open panicked is learned as it next opens.
    pub fn learn(&self) {
        let Some(tenants) = self.tenants.upgrade() else {
            return;
        };
        let names = tenants.names();
        crate::log!(
            "{}: no schedule hint: opening each of its {} tenant(s) once to learn which hold a \
             scheduled statement",
            self.dir.display(),
            names.len()
        );
        let mut failed = 0;
        for name in &names {
            let learned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match self.open_for(&tenants, name, false) {
                    Ok(Some((t, opened))) => {
                        drop(t);
                        if opened {
                            tenants.close_after_run(name);
                        }
                        Ok(())
                    }
                    Ok(None) | Err(Refused(404, _)) => Ok(()),
                    Err(Refused(status, why)) => Err(format!("({status}): {why}")),
                }
            }));
            let why = match learned {
                Ok(Ok(())) => continue,
                Ok(Err(why)) => why,
                Err(p) => {
                    self.panics.fetch_add(1, Ordering::Relaxed);
                    format!("(a panic): {}", panic_words(p.as_ref()))
                }
            };
            failed += 1;
            crate::log!(
                "tenant `{name}` did not open to learn its schedules {why}; learned as it next \
                 opens"
            );
        }
        self.learning.store(false, Ordering::Release);
        let held = {
            let mut q = lock(&self.queue);
            q.dirty = true;
            q.entries.len()
        };
        match self.write_hint() {
            Ok(()) => crate::log!(
                "{}: {held} of {} tenant(s) hold a scheduled statement{}",
                self.dir.display(),
                names.len(),
                match failed {
                    0 => String::new(),
                    n => format!(", {n} not opened"),
                }
            ),
            Err(e) => crate::log!("could not write the schedule hint once learned: {e}"),
        }
        self.woke.notify_all();
    }
}

/// What the scheduler keeps of a tenant as it closes ([`Node::closed`]).
pub(crate) struct Closing {
    /// Where its file stands, and its pending marks.
    kept: Kept,
    /// When to look at it next -- [`NONE`] with nothing to wait for -- or
    /// `None` where it holds no schedule.
    next: Option<i64>,
    /// Whether the close's sync was durable: a tenant whose last drop is
    /// not leaves the hint no sooner.
    synced: bool,
}

/// What a closing tenant's database says the scheduler keeps of it,
/// `synced` where whatever it held reached the disk.
pub(crate) fn closing(db: &Database, synced: bool) -> Closing {
    let kept = Kept {
        stood: Stood::of(db),
        marks: db.pending().to_vec(),
    };
    let next = db.holds_schedules().then(|| {
        let now = now_ms();
        next_of(&db.due(now), now)
    });
    Closing { kept, next, synced }
}

#[cfg(test)]
mod tests {
    //! A look's panic, held to its tenant; one outside any look, the node's.
    use super::*;
    use std::sync::atomic::AtomicI32;

    /// The tenants whose look panics, and the directories whose hint's
    /// write does.
    static PANIC_AT: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// What the node's one shutdown was asked with.
    static STOPPED: AtomicI32 = AtomicI32::new(0);

    pub(super) fn panic_at(what: &str) {
        let hit = lock(&PANIC_AT).iter().any(|p| p == what);
        if hit {
            panic!("a panic at `{what}`");
        }
    }

    fn stop(code: i32) {
        STOPPED.store(code, Ordering::SeqCst);
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fennec-node-unit-{tag}-{}-{}",
            std::process::id(),
            std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tenant whose `tick` runs each hour from two hours ago: due now.
    fn due_now(tenants: &Tenants, name: &str) {
        let t = tenants.create(name).unwrap();
        let mut g = t.db.write().unwrap();
        g.set_clock(Some(now_ms() - 2 * 3_600_000));
        for q in [
            "create collection ticks (slot timestamp)",
            "create statement tick every 1h as insert ticks {slot: slot()}",
        ] {
            for s in fennec_ql::parse(q).unwrap() {
                g.execute(&s).unwrap();
            }
        }
        g.set_clock(None);
    }

    fn ticks(tenants: &Tenants, name: &str) -> usize {
        let t = tenants.get(name).unwrap();
        let g = t.db.read().unwrap();
        let q = fennec_ql::parse_one("get ticks").unwrap();
        g.query(&q, &[]).unwrap().rows().unwrap().rows.len()
    }

    fn ran(runs: &[(String, Run)], name: &str) -> bool {
        runs.iter().any(|r| r.0 == name)
    }

    fn until(what: &str, f: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !f() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A look panicking holds its tenant back a minute, then two -- each
    /// counted, the other tenant run -- and a look after its backoff runs
    /// it. On the real thread the node goes on past a look's panic, and a
    /// panic in the hint's write, outside any look, stops it through its
    /// one shutdown.
    #[test]
    fn a_look_s_panic_holds_its_tenant_back_and_one_outside_a_look_stops_the_node() {
        fennec_ql::install();
        let dir = scratch("panic");
        let tenants = Arc::new(Tenants::new(&dir).unwrap());
        let node = Node::attach(&tenants, false);
        node.learn();
        due_now(&tenants, "pa");
        due_now(&tenants, "pb");
        tenants.close_idle(Duration::ZERO);
        lock(&PANIC_AT).push("pa".into());
        let now = now_ms();
        let runs = node.pass(now);
        assert!(ran(&runs, "pb") && !ran(&runs, "pa"), "{runs:?}");
        assert_eq!(node.panics(), 1);
        let at = |n: &str| node.named().into_iter().find(|e| e.0 == n).unwrap().1;
        assert_eq!(at("pa"), Some(now + MINUTE), "held back a minute");
        assert!(!ran(&node.pass(now + MINUTE - 1), "pa"));
        assert!(!ran(&node.pass(now + MINUTE), "pa"));
        assert_eq!(node.panics(), 2);
        assert_eq!(at("pa"), Some(now + 3 * MINUTE), "then two");
        lock(&PANIC_AT).clear();
        assert!(ran(&node.pass(now + 3 * MINUTE), "pa"));
        assert_eq!(ticks(&tenants, "pa"), 1);
        drop((node, tenants));
        let _ = std::fs::remove_dir_all(&dir);

        // The real thread, started over a hint naming both due: `qa`'s look
        // panics, `qb` runs after it, and the hint's write then panics.
        let dir = scratch("live");
        {
            let tenants = Arc::new(Tenants::new(&dir).unwrap());
            let node = Node::attach(&tenants, false);
            node.learn();
            due_now(&tenants, "qa");
            due_now(&tenants, "qb");
            tenants.close_idle(Duration::ZERO);
            node.write_hint().unwrap();
        }
        let tenants = Arc::new(Tenants::new(&dir).unwrap());
        let node = Node::attach(&tenants, false);
        assert!(!node.learning());
        lock(&PANIC_AT).extend(["qa".into(), dir.to_string_lossy().into_owned()]);
        node.start(stop).unwrap();
        until("the node stopped", || STOPPED.load(Ordering::SeqCst) == 1);
        lock(&PANIC_AT).clear();
        assert_eq!(node.panics(), 1, "qa's look");
        assert_eq!(ticks(&tenants, "qb"), 1, "qb ran past qa's panic");
        assert_eq!(ticks(&tenants, "qa"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The node's one shutdown, for a test whose thread outlives it.
    fn ignored(_: i32) {}

    /// An image holding `tick`, due now.
    fn image() -> Vec<u8> {
        let mut db = Database::new();
        db.allow_schedules();
        db.set_clock(Some(now_ms() - 2 * 3_600_000));
        for q in [
            "create collection ticks (slot timestamp)",
            "create statement tick every 1h as insert ticks {slot: slot()}",
        ] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        db.snapshot()
    }

    /// A scheduler asleep with no deadline -- its node holding no schedule
    /// -- is woken by an install, and by a move's install, each slot due
    /// then run: the install's look queued with no word to the thread
    /// waited for a request or a restart, and a move of an idle tenant onto
    /// the node never ran.
    #[test]
    fn an_install_or_a_move_in_wakes_a_sleeping_scheduler() {
        fennec_ql::install();
        for (tag, proposed) in [("wake-install", None), ("wake-move", Some(0x5eed))] {
            let dir = scratch(tag);
            let tenants = Arc::new(Tenants::new(&dir).unwrap());
            let node = Node::attach(&tenants, false);
            node.set_arriving(0);
            node.start(ignored).unwrap();
            until("asleep", || node.asleep.load(Ordering::SeqCst));
            tenants.import_as("acme", &image(), proposed).unwrap();
            // Its look first, asked of the node: a client's open wakes the
            // thread itself.
            until("looked at", || node.opens() == 1);
            until("its slot ran", || ticks(&tenants, "acme") == 1);
            drop((node, tenants));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
