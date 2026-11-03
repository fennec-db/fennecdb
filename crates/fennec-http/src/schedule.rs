//! Runs the scheduled statements of a file's server
//! (`docs/design/named-statements.md`, *Scheduled statements (F3)*).
//!
//! **One thread a process, asleep with no schedule.** [`watch`] gives a
//! database to the scheduler -- `fennec-server`'s main, after both
//! listeners are bound, as the sweep's is -- and lets it take a scheduled
//! statement's definition (`Database::allow_schedules`), which a database
//! no scheduler watches refuses: never a schedule no one runs. The thread
//! waits on a condition with no deadline while no database it watches
//! holds a schedule, so a server holding none wakes no more than it did;
//! the engine wakes it where the table comes to hold one
//! (`Database::on_schedules`, asked where `rebind` is), never a timer. With
//! a schedule it sleeps until the earliest next slot, a minute at most, so
//! a clock stepped forward is noticed within one. A panic outside a run's
//! own catch -- a look, a log line, a sync -- stops the server through the
//! one shutdown, as anything beside the database does (`stop_with`): a
//! server answering while its schedules never run is the gap F3 must never
//! leave.
//!
//! **A look** ([`pass`], which a test drives with the time it names) asks
//! [`Database::due`] of each database it is due for, and runs each due
//! slot under the write lock -- the slot asked again under it, so no slot
//! runs twice in one history -- in a block with its mark: the run's
//! writes, waking the statements bound to them, and the mark land as one
//! record or neither does (`Database::run_slot`, `Database::mark_slot`).
//! A run that wrote nothing lands nothing: its mark waits in memory for
//! the next block that lands (`Database::pending`). The run is then made
//! durable as any write is, after the lock: its fsync and, under `--sync
//! always`, the streams keeping up.
//!
//! **Who refused decides whether the slot is marked.** A refusal the
//! statement makes -- an unmet guard, a broken statement, a clash, a
//! bound, a binding's or a hook's refusal, a panic -- marks the slot
//! refused, its status and reason in the mark, and the next slot runs as
//! due. One the state makes -- the quota, a lapsed lease's fence, a
//! storage error -- leaves the slot due, held here ([`Held`]) and tried
//! again as the change counter or the limit moves, at most once a minute,
//! or after a backoff doubling from a minute to [`RETRY_MAX`]; the next
//! slot supersedes it. Nothing runs on a following database, one whose
//! storage failed or one whose lease lapsed.
//!
//! **A run's authority is its text's**: it runs as the server's token's
//! call does, checked by no scope, with no claim and no rate; who may
//! define one is the server's token alone, and only that token calls one
//! by hand, which marks nothing (`Database::callee`). Each run's lines
//! carry an id of its own ([`run_id`]), as a request's carry its.

use crate::held;
use fennec_core::engine::SlotMark;
use fennec_core::error::Error;
use fennec_core::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
use std::time::Duration;

const MINUTE: i64 = 60_000;

/// The longest a slot the state holds back waits between tries, the
/// backoff doubling from a minute to it: a compaction making room moves
/// neither the change counter nor the limit, and is found within it.
pub const RETRY_MAX: i64 = 15 * MINUTE;

/// The longest the scheduler sleeps while a database it watches holds a
/// schedule: a clock stepped forward is found within it, as cron's minute.
/// A wake-up that finds nothing due looks at no database.
const SLEEP_MOST: i64 = MINUTE;

/// `next` of a database with no slot to wait for.
const NONE: i64 = i64::MAX;

/// A due slot the state holds back -- the quota, a lapsed lease, a storage
/// error -- kept in memory alone: a restart, a failover or a move finds it
/// due in the file and tries it at its first look.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    /// The slot held back.
    pub slot: i64,
    /// The status of the refusal that held it.
    pub status: u16,
    /// When it was first refused.
    pub since: i64,
    /// The change counter and the limit as it was last tried: moved, it is
    /// tried again, at most once a minute.
    seq: u64,
    limit: usize,
    tried: i64,
    backoff: i64,
}

impl Held {
    /// Whether to try it again at `now`, the database at change `seq` and
    /// held to the limit `limit`.
    fn due(&self, now: i64, seq: u64, limit: usize) -> bool {
        let since = now.saturating_sub(self.tried);
        let moved = seq != self.seq || limit != self.limit;
        (moved && since >= MINUTE) || since >= self.backoff
    }
}

/// What a run of one slot came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Its writes and its mark landed, or its mark waits for the next
    /// block, where it wrote nothing.
    Ran,
    /// The statement refused it, with this status: the slot is marked
    /// refused, and the next runs as due.
    Refused(u16),
    /// The statement is broken (409), its slot marked refused unrun.
    Broken,
    /// The state refused it, with this status: the slot is left due, held
    /// and tried again.
    Held(u16),
}

/// A run of one statement's slot by a look.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub name: String,
    /// The slot it ran for.
    pub slot: i64,
    /// The slots before it that it passed over: missed slots run once,
    /// the latest.
    pub skipped: u64,
    pub outcome: Outcome,
    /// The run's own id, at the end of every line it wrote ([`run_id`]).
    pub id: String,
}

/// A mark, or a `since`, a look found ahead of the clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Ahead {
    pub name: String,
    pub mark: i64,
    /// Whether the slots wait for the clock to pass it -- ahead by
    /// `named::AHEAD_MAX` at most -- or the schedule goes on from the
    /// clock.
    pub waits: bool,
}

/// What a look at a database did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    pub runs: Vec<Run>,
    /// The marks found ahead of the clock, each told once a statement and
    /// mark.
    pub ahead: Vec<Ahead>,
}

/// The process's counts of runs: what `/_metrics` will read (F3's slice
/// 5, [`counts`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub ran: u64,
    pub refused: u64,
    pub broken: u64,
    pub held: u64,
    /// Runs that landed as their lease lapsed: whether the tenant's next
    /// primary holds them is unknown.
    pub unknown: u64,
    pub skipped: u64,
    pub mark_ahead: u64,
}

struct Counters {
    ran: AtomicU64,
    refused: AtomicU64,
    broken: AtomicU64,
    held: AtomicU64,
    unknown: AtomicU64,
    skipped: AtomicU64,
    mark_ahead: AtomicU64,
}

static TOTAL: Counters = Counters {
    ran: AtomicU64::new(0),
    refused: AtomicU64::new(0),
    broken: AtomicU64::new(0),
    held: AtomicU64::new(0),
    unknown: AtomicU64::new(0),
    skipped: AtomicU64::new(0),
    mark_ahead: AtomicU64::new(0),
};

fn count(f: impl Fn(&Counters) -> &AtomicU64, n: u64) {
    f(&TOTAL).fetch_add(n, Ordering::Relaxed);
}

/// The runs the process counted.
pub fn counts() -> Counts {
    let r = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let t = &TOTAL;
    Counts {
        ran: r(&t.ran),
        refused: r(&t.refused),
        broken: r(&t.broken),
        held: r(&t.held),
        unknown: r(&t.unknown),
        skipped: r(&t.skipped),
        mark_ahead: r(&t.mark_ahead),
    }
}

/// A database the scheduler knows: one [`watch`] gave it, which its
/// thread looks at, or one a test's [`pass`] looks at alone.
struct Watched {
    what: String,
    db: Weak<RwLock<Database>>,
    /// `--sync always`: a run is fsynced, and waits for the streams
    /// keeping up, before the next.
    durable: bool,
    /// Whether the scheduler's thread looks at it.
    run: AtomicBool,
    /// The engine said its table changed and holds a schedule.
    woken: AtomicBool,
    /// When to look next, in epoch milliseconds: [`NONE`] with no slot to
    /// wait for.
    next: AtomicI64,
    looks: AtomicU64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The slots the state holds back, by statement.
    held: Vec<(String, Held)>,
    /// The marks ahead of the clock told, by statement: once each.
    told: Vec<(String, i64)>,
}

static WATCHED: Mutex<Vec<Arc<Watched>>> = Mutex::new(Vec::new());
/// What the scheduler's thread sleeps on: a database's table came to hold
/// a schedule, or one was given to it.
static WAKE: Mutex<()> = Mutex::new(());
static WOKEN: Condvar = Condvar::new();
static STARTED: Mutex<bool> = Mutex::new(false);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    fennec_core::time::now_ms().unwrap_or(0)
}

/// The entry of `db`, made where there is none.
fn entry(db: &Arc<RwLock<Database>>, what: &str, durable: bool) -> Arc<Watched> {
    let weak = Arc::downgrade(db);
    let mut list = lock(&WATCHED);
    list.retain(|w| w.db.strong_count() > 0);
    if let Some(w) = list.iter().find(|w| Weak::ptr_eq(&w.db, &weak)) {
        return Arc::clone(w);
    }
    let w = Arc::new(Watched {
        what: what.to_string(),
        db: weak,
        durable,
        run: AtomicBool::new(false),
        woken: AtomicBool::new(false),
        next: AtomicI64::new(NONE),
        looks: AtomicU64::new(0),
        state: Mutex::default(),
    });
    list.push(Arc::clone(&w));
    w
}

/// Runs `db`'s scheduled statements from now on, `what` naming it in the
/// log: lets it take a scheduled statement's definition, and starts the
/// process's scheduler where none runs yet -- an `Err` where its thread
/// cannot be made, for the server to stop on (`stop_with`), never to serve
/// with its schedules unrun. `durable` is `--sync always`; `stop` is the
/// server's one shutdown, which the scheduler's thread asks for should it
/// end (`stop_with`). Called after both listeners are bound, since a first
/// look may run a missed slot.
pub fn watch(
    what: &str,
    db: &Arc<RwLock<Database>>,
    durable: bool,
    stop: fn(i32),
) -> std::result::Result<(), String> {
    start(stop)?;
    let w = entry(db, what, durable);
    w.run.store(true, Ordering::Release);
    let holds = {
        let mut g = held::write(db);
        g.allow_schedules();
        let hook = Arc::downgrade(&w);
        g.on_schedules(Arc::new(move |n| {
            if let (true, Some(w)) = (n > 0, hook.upgrade()) {
                wake(&w);
            }
        }));
        g.holds_schedules()
    };
    // A file opened holding one: its missed slots run at the first look.
    if holds {
        wake(&w);
    }
    Ok(())
}

/// Wakes the scheduler for `w`: its table came to hold a schedule, or
/// one changed. Under the database's write lock, a flag and a notify.
fn wake(w: &Watched) {
    w.woken.store(true, Ordering::Release);
    let _g = lock(&WAKE);
    WOKEN.notify_all();
}

/// Says how many scheduled statements `db` holds that nothing here runs
/// -- `--http-read-only`, a tenant node -- `why` naming why: at once, and
/// again wherever the table comes to hold another (a replica's apply, an
/// image adopted), so a schedule is never held unrun without a word. Its
/// file keeps their slots due, to whoever runs them next.
pub fn unrun(what: &str, why: &'static str, db: &mut Database) {
    let held = db
        .statements()
        .iter()
        .filter(|s| s.schedule.is_some())
        .count();
    tell_unrun(what, why, held);
    let told = std::sync::atomic::AtomicUsize::new(held);
    let what = what.to_string();
    db.on_schedules(Arc::new(move |n| {
        if told.swap(n, Ordering::Relaxed) != n {
            tell_unrun(&what, why, n);
        }
    }));
}

fn tell_unrun(what: &str, why: &str, n: usize) {
    if n > 0 {
        crate::log!("{what}: {n} scheduled statement(s) held unrun: {why}, their slots left due");
    }
}

fn start(stop: fn(i32)) -> std::result::Result<(), String> {
    let mut started = lock(&STARTED);
    if *started {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("fennec-schedule".into())
        .spawn(move || scheduler(stop))
        .map_err(|e| format!("could not start the scheduler: {e}"))?;
    *started = true;
    Ok(())
}

/// The scheduler's thread. A run's panic is caught where it runs, its slot
/// marked; one anywhere else -- `Database::due`, a look, a log line, a
/// sync -- or the loop's end stops the server through its one shutdown
/// (`stop`), never a thread gone and the server serving with no schedule
/// run.
fn scheduler(stop: fn(i32)) {
    let ended = std::panic::catch_unwind(looking);
    let words = match &ended {
        Ok(()) => "its loop ended".to_string(),
        Err(p) => panic_words(p.as_ref()),
    };
    crate::log!("the scheduler stopped ({words}): the server stops with it");
    stop(1);
}

fn looking() {
    loop {
        for w in due() {
            if let Some(db) = w.db.upgrade() {
                w.woken.store(false, Ordering::Release);
                look(&w, &db, now_ms(), true, None);
            }
        }
    }
}

fn panic_words(p: &(dyn std::any::Any + Send)) -> String {
    (p.downcast_ref::<&str>().map(|s| s.to_string()))
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}

/// The databases a look is due for, once there are any: woken, or past
/// their next slot. Sleeps meanwhile -- with no deadline while none holds
/// a schedule, so a server holding none wakes for nothing.
fn due() -> Vec<Arc<Watched>> {
    let mut g = lock(&WAKE);
    loop {
        let now = now_ms();
        let mut due = Vec::new();
        let mut soonest = NONE;
        {
            let mut list = lock(&WATCHED);
            list.retain(|w| w.db.strong_count() > 0);
            for w in list.iter().filter(|w| w.run.load(Ordering::Acquire)) {
                let next = w.next.load(Ordering::Acquire);
                if w.woken.load(Ordering::Acquire) || next <= now {
                    due.push(Arc::clone(w));
                } else {
                    soonest = soonest.min(next);
                }
            }
        }
        if !due.is_empty() {
            return due;
        }
        g = match soonest {
            NONE => WOKEN.wait(g).unwrap_or_else(|e| e.into_inner()),
            t => {
                let ms = t.saturating_sub(now).clamp(1, SLEEP_MOST) as u64;
                let (g, _) = (WOKEN.wait_timeout(g, Duration::from_millis(ms)))
                    .unwrap_or_else(|e| e.into_inner());
                g
            }
        };
    }
}

/// One look at `db` at `now`, as the scheduler makes it: each due slot run
/// once, under the write lock, and what came of it. A test drives a slot
/// with the time it names; the scheduler's thread runs none on a database
/// it was not given ([`watch`]), so one a test looks at is the test's.
pub fn pass(db: &Arc<RwLock<Database>>, now: i64) -> Look {
    let w = entry(db, "the database", true);
    let mut out = Look::default();
    look(&w, db, now, false, Some(&mut out));
    out
}

/// The looks made at `db` so far: none, for a database holding no
/// schedule (`a_server_holding_no_schedule_makes_no_look`).
pub fn looks(db: &Arc<RwLock<Database>>) -> u64 {
    let weak = Arc::downgrade(db);
    let list = lock(&WATCHED);
    let w = list.iter().find(|w| Weak::ptr_eq(&w.db, &weak));
    w.map_or(0, |w| w.looks.load(Ordering::Relaxed))
}

/// The id of the run this thread is making, at the end of each line it
/// writes as a request's id is (`request_id`); empty between runs.
pub fn run_id() -> String {
    crate::request_id::current()
}

/// One look, at `now`: `live` where the scheduler's thread makes it, the
/// clock read again as each run takes the write lock; what it did told to
/// `out` where a test asks ([`pass`]), and built for no one otherwise.
fn look(w: &Watched, db: &RwLock<Database>, now: i64, live: bool, mut out: Option<&mut Look>) {
    w.looks.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    if tests::PANIC.swap(false, Ordering::SeqCst) {
        panic!("a look's panic");
    }
    let mut st = lock(&w.state);
    let (dues, writes, seq, limit) = {
        let g = held::read(db);
        // A following database's writes are its primary's, the runs among
        // them; a failed one takes none until it is opened again; past a
        // lapsed lease the slot is the tenant's next primary's. Each slot
        // stays due, to whoever writes the file next.
        let writes = !g.history().following && g.failure().is_none() && g.fenced().is_ok();
        (g.due(now), writes, g.change_seq(), limit_of(&g))
    };
    // A mark ahead of the clock: counted and told once a statement and
    // mark.
    st.told
        .retain(|(n, m)| dues.iter().any(|d| d.name == *n && d.ahead == Some(*m)));
    for d in &dues {
        let Some(mark) = d.ahead else {
            continue;
        };
        if st.told.iter().any(|(n, m)| *n == d.name && *m == mark) {
            continue;
        }
        let waits = mark <= now.saturating_add(fennec_core::named::AHEAD_MAX);
        tell_ahead(w, &d.name, mark, now, waits);
        st.told.push((d.name.clone(), mark));
        if let Some(out) = out.as_deref_mut() {
            out.ahead.push(Ahead {
                name: d.name.clone(),
                mark,
                waits,
            });
        }
    }
    // A slot held for a statement dropped, replaced past it or run since
    // is held no more.
    st.held.retain(|(n, h)| {
        dues.iter()
            .any(|d| d.name == *n && d.run.is_some_and(|r| r.0 >= h.slot))
    });
    if writes {
        for d in dues.iter().filter(|d| d.run.is_some()) {
            // A slot held back and not due a try takes no write lock.
            let waits = |(n, h): &(String, Held)| {
                *n == d.name && d.run.is_some_and(|r| r.0 == h.slot) && !h.due(now, seq, limit)
            };
            if st.held.iter().any(waits) {
                continue;
            }
            run(w, db, &d.name, now, live, &mut st, out.as_deref_mut());
        }
    }
    let mut next = dues.iter().filter_map(|d| d.next).min().unwrap_or(NONE);
    // A slot held back, or one due where this copy may not run it -- a
    // replica promoted, a lease granted again -- is looked at again within
    // a minute.
    if !st.held.is_empty() || (!writes && dues.iter().any(|d| d.run.is_some())) {
        next = next.min(now.saturating_add(MINUTE));
    }
    w.next.store(next, Ordering::Release);
}

/// `name`'s due slot run, with an id of its own for the lines it writes,
/// and told to `out` where a test asks.
fn run(
    w: &Watched,
    db: &RwLock<Database>,
    name: &str,
    now: i64,
    live: bool,
    st: &mut State,
    out: Option<&mut Look>,
) {
    crate::request_id::begin(None);
    let r = run_slot(w, db, name, now, live, st);
    if let (Some((slot, skipped, outcome)), Some(out)) = (r, out) {
        out.runs.push(Run {
            name: name.to_string(),
            slot,
            skipped,
            outcome,
            id: run_id(),
        });
    }
    crate::request_id::end();
}

fn limit_of(db: &Database) -> usize {
    db.limit()
        .map_or(0, |l| Arc::as_ptr(l) as *const () as usize)
}

/// The slot run, what it passed over and what came of it; `None` where
/// none ran.
fn run_slot(
    w: &Watched,
    db: &RwLock<Database>,
    name: &str,
    now: i64,
    live: bool,
    st: &mut State,
) -> Option<(i64, u64, Outcome)> {
    let iso = fennec_core::time::format_iso;
    let what = &w.what;
    let mut g = held::write(db);
    // The run's start, read once the lock is held: a run behind fifteen
    // others, their fsyncs and their streams, reads the time it runs at --
    // its `now()` and `@ttl`'s `alive` -- never the look's.
    let now = match live {
        true => now.max(now_ms()),
        false => now,
    };
    // Asked again under the lock the run holds to its landing: a slot is
    // run once in a history, the mark landing with the run.
    if g.history().following || g.failure().is_some() || g.fenced().is_err() {
        return None;
    }
    let d = g.due(now).into_iter().find(|d| d.name == name)?;
    let (slot, skipped) = d.run?;
    let (seq, limit) = (g.change_seq(), limit_of(&g));
    if let Some(i) = st.held.iter().position(|(n, _)| n == name) {
        let h = &st.held[i].1;
        if h.slot == slot {
            if !h.due(now, seq, limit) {
                return None;
            }
        } else {
            crate::log!(
                "{what}: scheduled statement `{name}`: slot {} held since {} ({}) is passed \
                 over, slot {} superseding it",
                iso(h.slot),
                iso(h.since),
                h.status,
                iso(slot)
            );
            st.held.remove(i);
        }
    }
    let s = g.statements().get(name)?;
    let version = s.version;
    let broken = s.broken.is_some() || s.tree.is_none();
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        g.begin()?;
        let r = (g.run_slot(name, slot, now)).and_then(|_| {
            let mark = SlotMark {
                at: slot,
                version,
                refused: None,
                skipped,
            };
            g.mark_slot(name, mark)
        });
        match r {
            Ok(()) => g.commit(),
            Err(e) => {
                g.rollback();
                Err(e)
            }
        }
    }));
    let refusal = match ran {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some((crate::api::status_of(&e), e.to_string(), e)),
        Err(p) => {
            // Put back whole by the engine, its block too.
            g.rollback();
            let e = Error::Plugin(format!("a panic in the run: {}", panic_words(p.as_ref())));
            Some((500, e.to_string(), e))
        }
    };
    let outcome = match refusal {
        None => {
            st.held.retain(|(n, _)| n != name);
            count(|c| &c.ran, 1);
            Outcome::Ran
        }
        // The state's: the slot is left due, and tried again.
        Some((status, words, e)) if state_refused(&g, &e) => {
            let (seq, limit) = (g.change_seq(), limit_of(&g));
            match st
                .held
                .iter_mut()
                .find(|(n, h)| n == name && h.slot == slot)
            {
                Some((_, h)) => {
                    h.tried = now;
                    h.backoff = (h.backoff * 2).min(RETRY_MAX);
                    (h.seq, h.limit, h.status) = (seq, limit, status);
                }
                None => {
                    crate::log!(
                        "{what}: scheduled statement `{name}`: slot {} held ({status}): \
                         {words}; tried again as the data or the limit changes, or in a minute",
                        iso(slot)
                    );
                    st.held.push((
                        name.to_string(),
                        Held {
                            slot,
                            status,
                            since: now,
                            seq,
                            limit,
                            tried: now,
                            backoff: MINUTE,
                        },
                    ));
                }
            }
            count(|c| &c.held, 1);
            Outcome::Held(status)
        }
        // The statement's: the slot is marked refused, a run that lands
        // nothing, and the next runs as due.
        Some((status, words, e)) => {
            st.held.retain(|(n, _)| n != name);
            let reason = e.reason().unwrap_or_default().to_string();
            let mark = SlotMark {
                at: slot,
                version,
                refused: Some((status, reason)),
                skipped,
            };
            if let Err(e) = g.mark_slot(name, mark) {
                crate::log!("{what}: scheduled statement `{name}`: its slot unmarked: {e}");
            }
            let kind = match broken {
                true => "broken",
                false => "refused",
            };
            crate::log!(
                "{what}: scheduled statement `{name}`: slot {} {kind} ({status}): {words}",
                iso(slot)
            );
            match broken {
                true => {
                    count(|c| &c.broken, 1);
                    Outcome::Broken
                }
                false => {
                    count(|c| &c.refused, 1);
                    Outcome::Refused(status)
                }
            }
        }
    };
    if !matches!(outcome, Outcome::Held(_)) {
        count(|c| &c.skipped, skipped);
    }
    // Made durable as any write is, once the lock is let go: only where a
    // block landed -- a run that wrote nothing costs no fsync.
    let landed = g.change_seq() != seq;
    let durable = match (landed, w.durable) {
        (false, _) => Ok(None),
        (true, true) => g.flush(),
        (true, false) => Ok(g.write_behind()),
    };
    drop(g);
    let synced = durable.and_then(|d| d.map_or(Ok(()), |d| d()));
    if let Err(e) = synced {
        crate::log!("{what}: sync error after scheduled statement `{name}`'s run: {e}");
        held::write(db).fail(&e);
    } else if landed && held::read(db).fenced().is_err() {
        // Landed, and its lease lapsed meanwhile: as `answered_under_lease`
        // tells a client, whether the tenant's next primary holds it is
        // unknown.
        crate::log!(
            "{what}: scheduled statement `{name}`: slot {} landed as its lease lapsed: whether \
             the next primary holds it is unknown",
            iso(slot)
        );
        count(|c| &c.unknown, 1);
    }
    Some((slot, skipped, outcome))
}

/// Whether `e`, a run's refusal, is the state's -- the quota, a lapsed
/// lease's fence, a storage error -- which the same text over the same
/// rows may not meet again: the slot is left due. The fence's refusal and
/// a hook's are both [`Error::ReadOnly`]; the fence asked again tells
/// them apart.
fn state_refused(db: &Database, e: &Error) -> bool {
    match e {
        Error::Quota(_) | Error::Io(_) => true,
        Error::ReadOnly(_) => db.fenced().is_err() || db.history().following,
        _ => false,
    }
}

fn tell_ahead(w: &Watched, name: &str, mark: i64, now: i64, waits: bool) {
    let iso = fennec_core::time::format_iso;
    count(|c| &c.mark_ahead, 1);
    let then = match waits {
        true => "its slots wait for the clock to pass it",
        false => "taken for a wrong clock's: the schedule goes on from the clock",
    };
    crate::log!(
        "{}: scheduled statement `{name}`: its last run's mark {} is ahead of the clock {}: {then}",
        w.what,
        iso(mark),
        iso(now)
    );
    crate::audit::clock(&w.what, name, mark, now, waits);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI32;

    /// Makes the next look panic, outside any run.
    pub(super) static PANIC: AtomicBool = AtomicBool::new(false);
    static STOPPED: AtomicI32 = AtomicI32::new(0);

    fn stop(code: i32) {
        STOPPED.store(code, Ordering::SeqCst);
    }

    /// A panic outside a run -- here a look's -- ends the scheduler's
    /// thread through the server's one shutdown (`stop`, `stop_with` in
    /// `fennec-server`), never silently: the server would go on answering
    /// with no schedule run.
    #[test]
    fn a_panic_outside_a_run_stops_the_server() {
        let mut db = Database::new();
        db.allow_schedules();
        for q in [
            "create collection holds (state text @hash)",
            "create statement tidy every 1m as del holds where state = 'x'",
        ] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        let db = Arc::new(RwLock::new(db));
        PANIC.store(true, Ordering::SeqCst);
        // Holding a schedule, the database is looked at once given.
        watch("a test's database", &db, false, stop).unwrap();
        let t = std::time::Instant::now();
        while STOPPED.load(Ordering::SeqCst) == 0 {
            assert!(t.elapsed() < Duration::from_secs(20), "never stopped");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(STOPPED.load(Ordering::SeqCst), 1);
        assert!(!PANIC.load(Ordering::SeqCst), "the look panicked");
    }
}
