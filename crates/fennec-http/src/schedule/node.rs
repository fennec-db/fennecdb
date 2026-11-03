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
//! it meanwhile ([`Tenants::close_after_run`]).
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
//! scheduler's is not.
//!
//! **The hint** ([`super::hint`]) names the tenants holding a schedule and
//! those alone, each with its next slot and a closed tenant's pending
//! marks: a tenant enters it, synced, before the block bringing its first
//! schedule lands (`Database::before_schedules`), as it opens holding one
//! -- an install, a restore, a move's target -- and as one reaches it -- a
//! replica's apply, a promotion -- and leaves it as it closes holding none,
//! its file synced by the close. A node starting with no hint opens each
//! tenant once in the background, a few at a time, to write one.

use super::hint::{self, Kept, Line, Stood};
use super::{lock, look_at, now_ms, panic_words, Ctx, Look, Run, State, MINUTE, NONE};
use crate::tenants::{Refused, Tenant, Tenants};
use fennec_core::error::Error;
use fennec_core::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// The tenants a node with no hint opens at once to learn which hold a
/// schedule: a few, so a start's learning is not every tenant's open at
/// once beside the first requests.
const LEARNING: usize = 4;

/// How long the scheduler sleeps at most with a slot to wait for: a clock
/// stepped forward is found within it.
const SLEEP_MOST: i64 = MINUTE;

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
    /// Each tenant's slots held back and marks told ahead, by name: the
    /// looking thread's alone.
    looks: Mutex<HashMap<String, State>>,
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
}

#[derive(Default)]
struct Queue {
    entries: HashMap<String, Entry>,
    /// The tenants with a slot to wait for, by it.
    waiting: BTreeSet<(i64, String)>,
    /// The tenants due, in turn: one run each before a second from any.
    ready: VecDeque<String>,
    /// The hint differs from its file.
    dirty: bool,
    /// When it was last written whole.
    written: i64,
    /// The tenants whose open failed, told once until one opens.
    failed: HashSet<String>,
}

#[derive(Default)]
struct Entry {
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
    /// Its marks of runs that wrote nothing as it closed.
    kept: Option<Kept>,
}

impl Queue {
    /// `name`'s entry, made where there is none.
    fn entry(&mut self, name: &str) -> &mut Entry {
        if !self.entries.contains_key(name) {
            self.dirty = true;
            let e = Entry {
                next: NONE,
                ..Entry::default()
            };
            self.entries.insert(name.to_string(), e);
        }
        self.entries.get_mut(name).expect("made above")
    }

    /// Waits for `next` to look at `name`: [`NONE`] for nothing. A tenant
    /// due already waits as it is.
    fn set_next(&mut self, name: &str, next: i64) {
        let Some(e) = self.entries.get_mut(name) else {
            return;
        };
        if e.queued {
            return;
        }
        if e.next == next {
            return;
        }
        if e.next != NONE {
            self.waiting.remove(&(e.next, name.to_string()));
        }
        e.next = next;
        if next != NONE {
            self.waiting.insert((next, name.to_string()));
        }
        self.dirty = true;
    }

    /// The tenants whose slot came by `now`, among the due, by slot and
    /// then name.
    fn come(&mut self, now: i64) {
        while let Some((t, _)) = self.waiting.first() {
            if *t > now {
                break;
            }
            let (_, name) = self.waiting.pop_first().expect("looked at above");
            if let Some(e) = self.entries.get_mut(&name) {
                e.next = NONE;
                e.queued = true;
                self.ready.push_back(name);
            }
        }
    }

    /// The next tenant due, taken out of the queue for its look.
    fn take(&mut self) -> Option<String> {
        let name = self.ready.pop_front()?;
        if let Some(e) = self.entries.get_mut(&name) {
            e.queued = false;
            e.woken = false;
        }
        Some(name)
    }

    /// `name` among the due again, at the back.
    fn again(&mut self, name: &str) {
        if let Some(e) = self.entries.get_mut(name) {
            if !e.queued {
                if e.next != NONE {
                    self.waiting.remove(&(e.next, name.to_string()));
                }
                e.next = NONE;
                e.queued = true;
                self.ready.push_back(name.to_string());
            }
        }
    }

    /// `name` out of the queue and the hint: whether it was in them.
    fn remove(&mut self, name: &str) -> bool {
        let Some(e) = self.entries.remove(name) else {
            return false;
        };
        if e.queued {
            self.ready.retain(|n| n != name);
        } else if e.next != NONE {
            self.waiting.remove(&(e.next, name.to_string()));
        }
        self.dirty = true;
        true
    }
}

/// When to look at a database whose slots are `dues` at `now`: at once
/// where one is due, else at the first next slot.
fn next_of(dues: &[fennec_core::engine::Due], now: i64) -> i64 {
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
        let read = hint::read(&dir);
        let mut queue = Queue::default();
        let learning = match read {
            Ok(Some(lines)) => {
                for (name, line) in lines {
                    queue.entries.insert(
                        name.clone(),
                        Entry {
                            next: NONE,
                            noted: true,
                            kept: line.kept,
                            ..Entry::default()
                        },
                    );
                    queue.set_next(&name, line.next.unwrap_or(i64::MIN));
                }
                queue.dirty = false;
                false
            }
            Ok(None) => true,
            Err(e) => {
                crate::log!(
                    "{}: the schedule hint does not read ({e}): learned again",
                    dir.join(hint::FILE).display()
                );
                true
            }
        };
        let node = Arc::new(Node {
            tenants: Arc::downgrade(tenants),
            dir,
            durable,
            queue: Mutex::new(queue),
            woke: Condvar::new(),
            looks: Mutex::new(HashMap::new()),
            io: Mutex::new(()),
            learning: AtomicBool::new(learning),
            opens: AtomicU64::new(0),
        });
        tenants.set_scheduler(Arc::clone(&node));
        node
    }

    /// Starts the thread that runs the node's tenants' slots, and where no
    /// hint was read the background opens that learn one: an `Err` where
    /// a thread cannot be made, for the server to stop on (`stop_with`),
    /// never to serve with its schedules unrun. `stop` is the one shutdown,
    /// which the thread asks for should it end. Called after the listener
    /// is bound, since a first look may run a missed slot.
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
            .map_err(|e| format!("could not start the scheduler: {e}"))?;
        if self.learning.load(Ordering::Acquire) {
            let me = Arc::clone(self);
            std::thread::Builder::new()
                .name("fennec-schedule-learn".into())
                .spawn(move || {
                    let ended =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| me.learn()));
                    if let Err(p) = ended {
                        crate::log!(
                            "learning which tenants hold a schedule stopped ({}): the node \
                             stops with it",
                            panic_words(p.as_ref())
                        );
                        stop(1);
                    }
                })
                .map_err(|e| format!("could not start learning the schedules: {e}"))?;
        }
        Ok(())
    }

    /// The thread's loop: the next tenant due looked at, at the clock, and
    /// the hint written where it changed, at most once a minute.
    fn running(&self) {
        loop {
            let Some(tenants) = self.tenants.upgrade() else {
                return;
            };
            match self.next_due() {
                Some(name) => {
                    self.look(&tenants, &name, now_ms(), true, None);
                }
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
    fn next_due(&self) -> Option<String> {
        let mut q = lock(&self.queue);
        loop {
            let now = now_ms();
            q.come(now);
            if let Some(name) = q.take() {
                return Some(name);
            }
            let write_at = match q.dirty && !self.learning.load(Ordering::Acquire) {
                true => q.written.saturating_add(MINUTE),
                false => NONE,
            };
            if write_at <= now {
                return None;
            }
            let first = q.waiting.first().map_or(NONE, |(t, _)| *t);
            // With nothing to wait for -- a node holding no schedule -- no
            // deadline: it wakes for nothing, as a file's server holding
            // none does.
            q = match first.min(write_at) {
                NONE => self.woke.wait(q).unwrap_or_else(|e| e.into_inner()),
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
    /// test drives the node's slots with the time it names; no thread may
    /// run beside it ([`Self::start`]).
    pub fn pass(&self, now: i64) -> Vec<(String, Run)> {
        let mut out = Vec::new();
        let Some(tenants) = self.tenants.upgrade() else {
            return out;
        };
        loop {
            let name = {
                let mut q = lock(&self.queue);
                q.come(now);
                q.take()
            };
            let Some(name) = name else { break };
            self.look(&tenants, &name, now, false, Some(&mut out));
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
        let mut out: Vec<_> = (q.entries.iter())
            .map(|(n, e)| {
                let at = e.next != NONE && e.next != i64::MIN && !e.queued;
                (n.clone(), at.then_some(e.next))
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

    /// [`Tenants::for_run`], counted where it opened the tenant.
    fn open_for(
        &self,
        tenants: &Tenants,
        name: &str,
    ) -> std::result::Result<(Arc<Tenant>, bool), Refused> {
        let r = tenants.for_run(name);
        if matches!(r, Ok((_, true))) {
            self.opens.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    /// One look at `name` at `now`: opened where it is closed, through its
    /// gate, one slot run, closed again where the look opened it and no
    /// request used it meanwhile.
    fn look(
        &self,
        tenants: &Tenants,
        name: &str,
        now: i64,
        live: bool,
        out: Option<&mut Vec<(String, Run)>>,
    ) {
        let (next, more) = match self.open_for(tenants, name) {
            Ok((t, opened)) => {
                lock(&self.queue).failed.remove(name);
                let looked = self.look_open(&t, name, now, live, out);
                drop(t);
                if opened {
                    tenants.close_after_run(name);
                }
                looked
            }
            // Gone: deleted, or a file the hint named that is not there.
            Err(Refused(404, _)) => {
                self.forget(name);
                lock(&self.looks).remove(name);
                return;
            }
            Err(Refused(status, why)) => {
                if lock(&self.queue).failed.insert(name.to_string()) && status != 503 {
                    crate::log!(
                        "tenant `{name}` did not open for its scheduled statements \
                         ({status}): {why}; tried again each minute"
                    );
                }
                (now.saturating_add(MINUTE), false)
            }
        };
        let mut q = lock(&self.queue);
        let Some(woken) = q.entries.get(name).map(|e| e.woken) else {
            // Gone from the hint meanwhile -- closed holding none, deleted:
            // what was held of it goes, on the one thread that looks.
            drop(q);
            lock(&self.looks).remove(name);
            return;
        };
        match more || woken {
            true => q.again(name),
            false => q.set_next(name, next),
        }
    }

    /// [`Self::look`] at a tenant open for it: when to look again, and
    /// whether a slot is left due.
    fn look_open(
        &self,
        t: &Tenant,
        name: &str,
        now: i64,
        live: bool,
        out: Option<&mut Vec<(String, Run)>>,
    ) -> (i64, bool) {
        // Held as a request holds it: a freeze waits for the run to land,
        // and a run asked after the freeze passes over the tenant, so none
        // lands on a move's source after the export. A thaw wakes it.
        let _gate = t.enter();
        if t.is_frozen() {
            return (now.saturating_add(MINUTE), false);
        }
        let what = format!("tenant `{name}`");
        let ctx = Ctx {
            what: &what,
            durable: self.durable,
            live,
        };
        let mut looks = lock(&self.looks);
        let st = looks.entry(name.to_string()).or_default();
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
    /// it lands, tells of its table's schedules as they change, and is
    /// handed back the marks it closed with where its file stands where it
    /// did. A tenant opening holding one -- an install, a restore, a move's
    /// target, a file put in its place -- enters the hint, its slots learned
    /// here. Under the tenant's slot.
    pub(crate) fn opened(self: &Arc<Self>, name: &str, db: &mut Database) {
        db.allow_schedules();
        let (me, who) = (Arc::downgrade(self), name.to_string());
        db.before_schedules(Arc::new(move || match me.upgrade() {
            Some(node) => node.enter(&who).map_err(|e| {
                Error::Io(format!(
                    "the node could not note tenant `{who}` in its schedule hint, and \
                     defines no schedule a start would not find: {e}"
                ))
            }),
            None => Ok(()),
        }));
        let (me, who) = (Arc::downgrade(self), name.to_string());
        db.on_schedules(Arc::new(move |n| {
            if let Some(node) = me.upgrade() {
                node.woken(&who, n);
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
        let enter = {
            let mut q = lock(&self.queue);
            let e = q.entry(name);
            let enter = !e.noted;
            if next < NONE {
                q.set_next(name, next);
            }
            enter
        };
        if enter {
            if let Err(e) = self.enter(name) {
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
        let enter = {
            let mut q = lock(&self.queue);
            if n == 0 && !q.entries.contains_key(name) {
                return;
            }
            let e = q.entry(name);
            e.woken = true;
            let enter = n > 0 && !e.noted;
            q.set_next(name, i64::MIN);
            enter
        };
        if enter {
            if let Err(e) = self.enter(name) {
                crate::log!(
                    "tenant `{name}` came to hold a scheduled statement, and the node could not \
                     note it in its schedule hint: {e}"
                );
            }
        }
        self.woke.notify_all();
    }

    /// Names `name` in the hint, synced, before anything of its first
    /// schedule is written -- a definition, an install -- once: a line
    /// appended. While the node learns, the hint is memory's until it is
    /// written whole.
    pub(crate) fn enter(&self, name: &str) -> std::io::Result<()> {
        {
            let mut q = lock(&self.queue);
            let e = q.entry(name);
            if e.noted {
                return Ok(());
            }
            q.set_next(name, i64::MIN);
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

    /// A tenant closed, its file synced: the marks of its runs that wrote
    /// nothing kept by name with where its file stood, for its next open;
    /// one holding no schedule leaves the hint, its last drop durable.
    pub(crate) fn closed(&self, name: &str, c: Closing) {
        let mut q = lock(&self.queue);
        let Some(next) = c.next else {
            // The common close, a tenant holding none: one look at the map.
            if q.remove(name) {
                drop(q);
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

    /// A tenant deleted: out of the hint. Its slots held and marks told
    /// go at its next look, on the looking thread: taken here, under the
    /// slot's lock, a close waited out another tenant's run.
    pub(crate) fn forget(&self, name: &str) {
        if lock(&self.queue).remove(name) {
            self.woke.notify_all();
        }
    }

    /// A move's source the router deleted after its flip, the delete
    /// refused while a request holds it: out of the hint, written whole at
    /// once, so a restart -- its freeze gone with the process -- runs no
    /// slot into a file no router sends to. A thaw enters it again
    /// ([`Self::woken`]).
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

    /// [`Self::write_hint`], whatever it holds, the file's lock held.
    fn write_held(&self) -> std::io::Result<()> {
        let mut q = lock(&self.queue);
        let lines: Vec<(String, Line)> = (q.entries.iter())
            .map(|(n, e)| {
                let next = (e.next != NONE && e.next != i64::MIN && !e.queued).then_some(e.next);
                let line = Line {
                    next,
                    kept: e.kept.clone(),
                };
                (n.clone(), line)
            })
            .collect();
        q.dirty = false;
        q.written = now_ms();
        drop(q);
        let written = hint::write(
            &self.dir,
            lines.iter().map(|(n, l)| (n.as_str(), l.clone())),
        );
        let mut q = lock(&self.queue);
        match &written {
            Ok(()) => {
                for (n, _) in &lines {
                    if let Some(e) = q.entries.get_mut(n) {
                        e.noted = true;
                    }
                }
            }
            Err(_) => q.dirty = true,
        }
        written
    }

    /// Opens each tenant once, [`LEARNING`] at a time, to learn which hold
    /// a schedule -- each that does entering the hint as it opens -- then
    /// writes the hint whole: what a node starting with none does, on
    /// threads of its own, saying so as it begins. A tenant opened for it
    /// is closed again, unless a request opened it meanwhile.
    pub fn learn(&self) {
        let Some(tenants) = self.tenants.upgrade() else {
            return;
        };
        let names = tenants.names();
        crate::log!(
            "{}: no schedule hint: opening each of its {} tenant(s) once, {LEARNING} at a time, \
             to learn which hold a scheduled statement",
            self.dir.display(),
            names.len()
        );
        let at = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..LEARNING.min(names.len()) {
                s.spawn(|| loop {
                    let i = at.fetch_add(1, Ordering::Relaxed);
                    let Some(name) = names.get(i) else { break };
                    match self.open_for(&tenants, name) {
                        Ok((t, opened)) => {
                            drop(t);
                            if opened {
                                tenants.close_after_run(name);
                            }
                        }
                        Err(Refused(404, _)) => {}
                        Err(Refused(status, why)) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                            crate::log!(
                                "tenant `{name}` did not open to learn its schedules \
                                 ({status}): {why}; learned as it next opens"
                            );
                        }
                    }
                });
            }
        });
        self.learning.store(false, Ordering::Release);
        lock(&self.queue).dirty = true;
        let held = lock(&self.queue).entries.len();
        match self.write_hint() {
            Ok(()) => crate::log!(
                "{}: {held} of {} tenant(s) hold a scheduled statement{}",
                self.dir.display(),
                names.len(),
                match failed.load(Ordering::Relaxed) {
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
}

/// What a closing tenant's database says the scheduler keeps of it.
pub(crate) fn closing(db: &Database) -> Closing {
    let kept = Kept {
        stood: Stood::of(db),
        marks: db.pending().to_vec(),
    };
    let next = db.holds_schedules().then(|| {
        let now = now_ms();
        next_of(&db.due(now), now)
    });
    Closing { kept, next }
}
