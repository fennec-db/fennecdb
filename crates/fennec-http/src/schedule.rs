//! Runs the scheduled statements of a file's server, and of a tenant
//! node's tenants ([`Node`])
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
//! leave. A node's scheduler holds a panic in one tenant's look to that
//! tenant ([`Node`]): the one shutdown stopped every other with it.
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

pub mod hint;
mod node;

pub(crate) use node::closing;
pub use node::Node;

use crate::held;
use crate::{metrics, statements, trace};
use fennec_core::engine::{HeldSlot, SlotMark};
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
    /// When it began, by the boot clock: how late it ran behind the runs
    /// before it, read by a test or a bench.
    pub started: std::time::Instant,
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

/// The process's counts of runs, each run counted once by what came of it:
/// what `/_metrics` reads ([`counts`], [`metrics`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub ran: u64,
    pub refused: u64,
    pub broken: u64,
    pub held: u64,
    /// Runs that landed as their lease lapsed: whether the tenant's next
    /// primary holds them is unknown. Not among `ran`.
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

/// The upper bounds of a run's lateness buckets, in microseconds: a tenth
/// of a second, where a run behind none lands, to a day, where one after a
/// day down does -- a node's ten thousand tenants due at once run within
/// a minute (`make schedule-bench`).
const LATENESS: [u64; 16] = [
    100_000,
    500_000,
    1_000_000,
    5_000_000,
    10_000_000,
    30_000_000,
    60_000_000,
    120_000_000,
    300_000_000,
    600_000_000,
    900_000_000,
    1_800_000_000,
    3_600_000_000,
    7_200_000_000,
    21_600_000_000,
    86_400_000_000,
];

/// How late each run began, past its slot. One process, one scheduler
/// thread counting -- a file's or a node's -- so no shard a thread.
static LATE: crate::metrics::Timings = crate::metrics::Timings::new(&LATENESS);

/// A look was made: the process holds a schedule, and `/_metrics` says
/// what its runs came to ([`metrics`]). A server that never held one
/// scrapes as it did, and wakes for nothing.
static LOOKED: AtomicBool = AtomicBool::new(false);

/// The scheduler's families, once a look was made: its runs by outcome,
/// the slots passed over, the marks found ahead of the clock and how late
/// runs began. No label names a tenant, a statement or a collection:
/// `/_stats/statements` counts each statement's runs by name, for those
/// who may read every shape (`docs/design/named-statements.md`, *Watching
/// schedules*).
pub(crate) fn metrics(out: &mut crate::metrics::Text) {
    if !LOOKED.load(Ordering::Relaxed) {
        return;
    }
    let c = counts();
    let name = "fennec_scheduled_runs_total";
    out.family(
        name,
        "counter",
        "Runs of scheduled statements, by outcome: ran (its writes and its slot's mark landed, or the mark waits for the next write), refused (the statement refused it, its slot marked), broken (the statement could not run, its slot marked), held (the state refused it -- the quota, a lapsed lease, a storage error -- its slot left due and tried again), unknown (it landed as its lease lapsed: whether the next primary holds it is unknown).",
    );
    for (outcome, n) in [
        ("ran", c.ran),
        ("refused", c.refused),
        ("broken", c.broken),
        ("held", c.held),
        ("unknown", c.unknown),
    ] {
        out.sample(name, &[("outcome", outcome)], n);
    }
    out.family(
        "fennec_scheduled_skipped_total",
        "counter",
        "Slots of scheduled statements passed over: missed slots run once, the latest.",
    );
    out.sample("fennec_scheduled_skipped_total", &[], c.skipped);
    out.family(
        "fennec_scheduled_mark_ahead_total",
        "counter",
        "A scheduled statement's last mark, or its definition's time, found ahead of the clock -- a clock stepped back, or one that was ahead -- each also the audit's clock line.",
    );
    out.sample("fennec_scheduled_mark_ahead_total", &[], c.mark_ahead);
    out.family(
        "fennec_scheduled_lateness_seconds",
        "histogram",
        "How late a scheduled statement's run began, past its slot: behind the runs before it, after a restart, a held slot tried again.",
    );
    crate::metrics::histogram(out, "fennec_scheduled_lateness_seconds", &[], &[&LATE]);
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
pub(crate) struct State {
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
            if let (Some(1..), Some(w)) = (n, hook.upgrade()) {
                wake(&w);
            }
            Ok(())
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
/// -- `--http-read-only`, a file's server's or a node's -- `why` naming
/// why: at once, and
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
        if let Some(n) = n.filter(|&n| told.swap(n, Ordering::Relaxed) != n) {
            tell_unrun(&what, why, n);
        }
        Ok(())
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

/// What a look runs against: a file's database, or a tenant's on a node
/// ([`Node`]).
pub(crate) struct Ctx<'a> {
    /// What names it in the log.
    pub what: &'a str,
    /// The tenant, on a node: its runs are its shapes, its slow lines and
    /// its spans', never a metric's label.
    pub tenant: Option<&'a str>,
    /// `--sync always`: a run is fsynced, and waits for the streams keeping
    /// up, before the next.
    pub durable: bool,
    /// Made by the scheduler's thread, the clock read again as each run
    /// takes the write lock; a test's look runs at the time it names.
    pub live: bool,
}

/// One look, at `now`: `live` where the scheduler's thread makes it, the
/// clock read again as each run takes the write lock; what it did told to
/// `out` where a test asks ([`pass`]), and built for no one otherwise.
fn look(w: &Watched, db: &RwLock<Database>, now: i64, live: bool, out: Option<&mut Look>) {
    w.looks.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    if tests::PANIC.swap(false, Ordering::SeqCst) {
        panic!("a look's panic");
    }
    let mut st = lock(&w.state);
    let ctx = Ctx {
        what: &w.what,
        tenant: None,
        durable: w.durable,
        live,
    };
    let (next, _, _) = look_at(&ctx, db, now, &mut st, usize::MAX, out);
    w.next.store(next, Ordering::Release);
}

/// A look at `db` at `now`, running at most `most` slots: when to look
/// again -- open, and closed after the look -- and whether a due slot was
/// left for the next look: a tenant's look runs one, so ten thousand
/// tenants due at once each run once before any runs twice ([`Node`]). A
/// closed tenant's held slot waits its backoff out: nothing moves its
/// counter or its limit, and opened each minute to find so, a tenant at its
/// quota was ten thousand opens a minute on a node of them.
pub(crate) fn look_at(
    w: &Ctx<'_>,
    db: &RwLock<Database>,
    now: i64,
    st: &mut State,
    most: usize,
    mut out: Option<&mut Look>,
) -> (i64, i64, bool) {
    if !LOOKED.load(Ordering::Relaxed) {
        LOOKED.store(true, Ordering::Relaxed);
    }
    let (dues, writes, following, seq, limit, told) = {
        let g = held::read(db);
        // A following database's writes are its primary's, the runs among
        // them; a failed one takes none until it is opened again; past a
        // lapsed lease the slot is the tenant's next primary's. Each slot
        // stays due, to whoever writes the file next.
        let following = g.history().following;
        let writes = !following && g.failure().is_none() && g.fenced().is_ok();
        let told = g.held_slots().to_vec();
        (
            g.due(now),
            writes,
            following,
            g.change_seq(),
            limit_of(&g),
            told,
        )
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
    let (mut ran, mut more) = (0, false);
    if writes {
        for d in dues.iter().filter(|d| d.run.is_some()) {
            // A slot held back and not due a try takes no write lock.
            let waits = |(n, h): &(String, Held)| {
                *n == d.name && d.run.is_some_and(|r| r.0 == h.slot) && !h.due(now, seq, limit)
            };
            if st.held.iter().any(waits) {
                continue;
            }
            if ran == most {
                more = true;
                break;
            }
            run(w, db, &d.name, now, st, out.as_deref_mut());
            ran += 1;
        }
    }
    let mut next = dues.iter().filter_map(|d| d.next).min().unwrap_or(NONE);
    // A slot held back, or one due where this copy may not run it -- a
    // lease granted again, a storage failure's file opened again -- is
    // looked at again within a minute. A following copy waits for nothing:
    // its promotion tells the scheduler (`Database::fork`), and a standby
    // node's ten thousand copies looked at each minute were ten thousand
    // looks for nothing.
    let blocked = !writes && !following && dues.iter().any(|d| d.run.is_some());
    if blocked {
        next = next.min(now.saturating_add(MINUTE));
    }
    let mut closed = next;
    for (_, h) in &st.held {
        next = next.min(now.saturating_add(MINUTE));
        closed = closed.min(h.tried.saturating_add(h.backoff));
    }
    if following {
        (next, closed) = (NONE, NONE);
    }
    // What the listings' `held` shows, told where it changed: the write
    // lock taken for it only then.
    let shown = shown_held(st);
    if shown != told {
        held::write(db).set_held_slots(shown);
    }
    (next, closed, more)
}

/// The slots `st` holds back, as the database is told them for its
/// listings (`Database::set_held_slots`).
pub(crate) fn shown_held(st: &State) -> Vec<(String, HeldSlot)> {
    (st.held.iter())
        .map(|(n, h)| {
            let shown = HeldSlot {
                slot: h.slot,
                status: h.status,
                since: h.since,
            };
            (n.clone(), shown)
        })
        .collect()
}

/// `name`'s due slot run, with an id of its own for the lines it writes
/// and a trace of its own, what came of it counted ([`watched`]), and told
/// to `out` where a test asks.
fn run(
    w: &Ctx<'_>,
    db: &RwLock<Database>,
    name: &str,
    now: i64,
    st: &mut State,
    out: Option<&mut Look>,
) {
    crate::request_id::begin(None);
    trace::begin_run(name, w.tenant);
    let started = out.is_some().then(std::time::Instant::now);
    let r = run_slot(w, db, name, now, st);
    match &r {
        Some(made) => watched(w, name, made),
        None => trace::discard(),
    }
    if let (Some(made), Some(out), Some(started)) = (r, out, started) {
        out.runs.push(Run {
            name: name.to_string(),
            slot: made.slot,
            skipped: made.skipped,
            outcome: made.outcome,
            id: run_id(),
            started,
        });
    }
    crate::request_id::end();
}

/// What a run of one slot came to, for what watches it ([`watched`]).
struct Made {
    slot: i64,
    skipped: u64,
    outcome: Outcome,
    /// The definition that ran.
    version: (u64, u64),
    /// What its last statement answered, as a call's are counted.
    rows: u64,
    /// Its time under the write lock: never how late it ran.
    took: std::time::Duration,
    /// How late it began, past its slot, in milliseconds.
    late: i64,
    /// A refusal's status and the reason its `else` wrote -- never the
    /// engine's words, which may hold a row's value.
    refused: Option<(u16, Option<String>)>,
    /// It landed as its lease lapsed.
    unknown: bool,
}

/// What a run came to, told where every statement's is: counted by its
/// outcome and its lateness (`/_metrics`, no label naming a tenant or a
/// statement), as the shape `schedule <name>` (`/_stats/statements`, a
/// tenant's apart), in the slow line past `--slow-ms` and as its trace,
/// the bindings its writes woke beside it as a request's are.
fn watched(w: &Ctx<'_>, name: &str, m: &Made) {
    let (counter, outcome): (fn(&Counters) -> &AtomicU64, _) = match (m.outcome, m.unknown) {
        (Outcome::Ran, true) => (|c| &c.unknown, "unknown"),
        (Outcome::Ran, false) => (|c| &c.ran, "ran"),
        (Outcome::Refused(_), _) => (|c| &c.refused, "refused"),
        (Outcome::Broken, _) => (|c| &c.broken, "broken"),
        (Outcome::Held(_), _) => (|c| &c.held, "held"),
    };
    count(counter, 1);
    // A held slot's tries are counted held, each; what it passed over and
    // how late it ran are counted once, by the run that did not hold.
    if !matches!(m.outcome, Outcome::Held(_)) {
        count(|c| &c.skipped, m.skipped);
        LATE.add(std::time::Duration::from_millis(m.late.max(0) as u64));
    }
    let failed = !matches!(m.outcome, Outcome::Ran);
    // The statements its writes woke, noted by the engine on this thread
    // as a request's are: counted, shaped and traced as theirs.
    if Database::runs_noted() {
        if let Some(r) = Database::take_runs() {
            metrics::bound(&r, m.took);
            statements::bound(w.tenant, &r);
            trace::bound_runs(&r);
        }
    }
    statements::scheduled(w.tenant, name, m.rows, m.took, failed);
    metrics::scheduled(m.took, failed, w.tenant, name, m.version, m.slot, m.rows);
    let refused = (m.refused.as_ref()).map(|(status, reason)| (*status, reason.as_deref()));
    trace::end_run(outcome, refused);
}

fn limit_of(db: &Database) -> usize {
    db.limit()
        .map_or(0, |l| Arc::as_ptr(l) as *const () as usize)
}

/// The slot run, what it passed over and what came of it; `None` where
/// none ran.
fn run_slot(
    w: &Ctx<'_>,
    db: &RwLock<Database>,
    name: &str,
    now: i64,
    st: &mut State,
) -> Option<Made> {
    let iso = fennec_core::time::format_iso;
    let what = w.what;
    let mut g = held::write(db);
    let locked = std::time::Instant::now();
    // The run's start, read once the lock is held: a run behind fifteen
    // others, their fsyncs and their streams, reads the time it runs at --
    // its `now()` and `@ttl`'s `alive` -- never the look's.
    let now = match w.live {
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
    let late = now.saturating_sub(slot);
    if trace::recording() {
        trace::attr("fennec.slot", iso(slot));
        trace::attr("fennec.lateness_ms", late);
        trace::attr("fennec.slots_skipped", skipped as i64);
    }
    let mut rows = 0;
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        g.begin()?;
        let running = trace::span("execute");
        let r = (g.run_slot(name, slot, now)).and_then(|(_, answered)| {
            rows = answered;
            let mark = SlotMark {
                at: slot,
                version,
                refused: None,
                skipped,
            };
            g.mark_slot(name, mark)
        });
        drop(running);
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
    // What a trace may tell of a refusal: its status and the reason its
    // `else` wrote, never the engine's words.
    let told = (refusal.as_ref()).map(|(status, _, e)| (*status, e.reason().map(String::from)));
    let outcome = match refusal {
        None => {
            st.held.retain(|(n, _)| n != name);
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
                true => Outcome::Broken,
                false => Outcome::Refused(status),
            }
        }
    };
    // Made durable as any write is, once the lock is let go: only where a
    // block landed -- a run that wrote nothing costs no fsync.
    let landed = g.change_seq() != seq;
    let durable = match (landed, w.durable) {
        (false, _) => Ok(None),
        (true, true) => g.flush(),
        (true, false) => Ok(g.write_behind()),
    };
    let took = locked.elapsed();
    drop(g);
    let syncing = landed.then(|| trace::span("durability"));
    let synced = durable.and_then(|d| d.map_or(Ok(()), |d| d()));
    drop(syncing);
    let mut unknown = false;
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
        unknown = true;
    }
    Some(Made {
        slot,
        skipped,
        outcome,
        version,
        rows,
        took,
        late,
        refused: told,
        unknown,
    })
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

fn tell_ahead(w: &Ctx<'_>, name: &str, mark: i64, now: i64, waits: bool) {
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
    crate::audit::clock(w.what, name, mark, now, waits);
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
