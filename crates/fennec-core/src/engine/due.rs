//! When a scheduled statement's slots come, and the marks of the runs that
//! wrote nothing (`docs/design/named-statements.md`, *Scheduled statements
//! (F3)*): [`Database::due`], a pure function of the table, the marks and
//! a time -- what a server's scheduler asks under the write lock it runs
//! under, and what an app's own timer would ask of the native library --
//! and [`Database::mark_slot`], the mark a run lands with its writes or
//! keeps pending until the next block that lands.
//!
//! A slot is an instant read off a zone's clock (`tz.rs`), which a summer
//! time's change makes skip an hour or show one twice:
//!
//! - `at '<cron>'`, and `every` a whole number of days, weeks or months,
//!   name times of the clock, each run at the first instant the clock reads
//!   it or later: a time it skips runs at the instant it jumps past it,
//!   every time a skipped hour holds being that one slot, and a time it
//!   shows twice runs once, at the first;
//! - `every` a shorter interval runs at the edges `bucket` cuts: each
//!   instant the clock reads an edge, the hour it shows twice holding two of
//!   each and one it skips none, and the instant it jumps past an edge.
//!
//! Each is a walk over the zone's stretches of one offset near a time, a
//! handful at most, and over the clock's times by arithmetic -- a cron
//! expression's a day at a time -- so a server down for a year finds its
//! missed slots counted without a walk over each.
//!
//! What runs one is a server's scheduler (`fennec_http::schedule`), which
//! asks [`Database::due`] under the write lock and runs a slot's body with
//! [`Database::run_slot`]; the server's token's `call` runs it by hand
//! ([`Database::slot_at`]). The browser module, which runs none, carries
//! none of it.

use super::stored::{marked, SlotMark, Stored, Times};
use super::Database;
use crate::error::{Error, Result};
use crate::named::AHEAD_MAX;
use crate::query::{Expr, Response, Statement};
use crate::time::{civil_from_days, days_from_civil};
use crate::tz::Zone;
use crate::value::Value;
use std::borrow::Cow;
use std::cell::Cell;

thread_local! {
    /// The slot `slot()` reads on this thread: the run's, or the latest at
    /// or before a call's time where the server's token calls a scheduled
    /// statement by hand -- `None` elsewhere, where `slot()` is no function
    /// (`plugin::Registry::call`). A thread's, as `call::LEFT` is: a body
    /// runs on the thread that runs it, under the lock it holds.
    static SLOT: Cell<Option<i64>> = const { Cell::new(None) };
}

/// The slot `slot()` reads on this thread, where a scheduled body runs.
pub(crate) fn slot_now() -> Option<i64> {
    SLOT.with(Cell::get)
}

/// `slot()` reading a slot on this thread for as long as it lives, put
/// back as it is dropped -- on a return and as a panic unwinds alike, with
/// no `catch_unwind` of its own: the scheduler's catches a run's panic.
struct SlotRead(Option<i64>);

impl SlotRead {
    fn set(slot: i64) -> SlotRead {
        SlotRead(SLOT.with(|s| s.replace(Some(slot))))
    }
}

impl Drop for SlotRead {
    fn drop(&mut self) {
        SLOT.with(|s| s.set(self.0));
    }
}

/// The database with its clock fixed to a run's start, given back as it is
/// dropped: on a return and as a panic unwinds alike.
struct Clocked<'a> {
    db: &'a mut Database,
    before: Option<i64>,
}

impl Drop for Clocked<'_> {
    fn drop(&mut self) {
        self.db.clock = self.before;
    }
}

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// Minutes a day.
const MINUTES: i64 = 1440;
/// The days a walk over a cron expression's looks at, at most: one fires
/// at least once in eight years -- 29 February where a century skips a
/// leap year -- since one that never fires is refused at its definition.
const CRON_DAYS: i64 = 8 * 366 + 31;
/// The stretches of one offset a walk passes at most: a handful near a
/// time, two a year over a span of years; bounded should a table be bad.
const STRETCHES: usize = 4096;

/// What a scheduled statement's clock says at a time ([`Database::due`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub name: String,
    /// The slot to run now -- the latest at or before the time, after the
    /// statement's `since` and its last mark -- and the slots before it it
    /// passes over (`skipped`): missed slots run once, the latest.
    pub run: Option<(i64, u64)>,
    /// The first slot after the time, or after a mark ahead of it: when to
    /// look again. `None` where this build cannot read the schedule's zone.
    pub next: Option<i64>,
    /// The last mark, or `since`, where it stands ahead of the time: a
    /// clock stepped back behind a run. Within [`AHEAD_MAX`] its slots wait
    /// for the clock to pass it; further ahead the mark is taken for the
    /// wrong reading and the schedule goes on from the clock. Counted and
    /// logged by whoever runs it.
    pub ahead: Option<i64>,
}

/// A due slot a server's scheduler holds back -- the quota, a lapsed
/// lease, a storage error -- as it tells the database it runs
/// ([`Database::set_held_slots`]): what the listings' `held` shows. The
/// scheduler's memory (`fennec_http::schedule::Held`), never the file's: a
/// restart, a failover or a move finds the slot due and holds it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSlot {
    /// The slot held back.
    pub slot: i64,
    /// The status of the refusal that held it, as its answer would be.
    pub status: u16,
    /// When it was first refused.
    pub since: i64,
}

impl Database {
    /// The slots the scheduler running this database's statements holds
    /// back, by statement, as it last told them.
    pub fn held_slots(&self) -> &[(String, HeldSlot)] {
        self.held_slots.as_deref().map_or(&[], |h| &h[..])
    }

    /// What the scheduler running this database's statements holds back,
    /// told after each look that changed it and as a tenant opens: the
    /// listings' `held`. Nothing else reads it; a slot run, marked or
    /// passed over since is shown none of, whatever was told.
    pub fn set_held_slots(&mut self, held: Vec<(String, HeldSlot)>) {
        self.held_slots = (!held.is_empty()).then(|| Box::new(held));
    }

    /// What the listings show of a scheduled statement beside its record:
    /// its schedule as its text writes it, the slot it runs next -- one due
    /// now, overdue where it is past, or the first after `now` -- its last
    /// mark, pending or landed, and the slot its scheduler holds back; four
    /// `null`s for a statement no clock runs (`statements`, `describe
    /// statement`, `GET /_schema`). `now` is `None` where no clock reads.
    pub fn schedule_shown(&self, s: &Stored, now: Option<i64>) -> [Value; 4] {
        match s.schedule {
            None => [Value::Null, Value::Null, Value::Null, Value::Null],
            Some(_) => self.scheduled_shown(s, now),
        }
    }

    /// [`Self::schedule_shown`] of a statement the clock runs: out of line,
    /// so a listing of statements none is runs four `null`s a row.
    #[cold]
    #[inline(never)]
    fn scheduled_shown(&self, s: &Stored, now: Option<i64>) -> [Value; 4] {
        let due = now.map(|now| self.due_of(s, now));
        let due_slot = due.as_ref().and_then(|d| d.run.map(|r| r.0));
        let next = due_slot
            .or(due.and_then(|d| d.next))
            .map_or(Value::Null, Value::Timestamp);
        let mark = self.pending_of(s).or(self.last_mark(&s.name));
        let last = mark.map_or(Value::Null, |m| {
            let (outcome, status, reason) = match &m.refused {
                None => ("ran", Value::Null, Value::Null),
                Some((status, reason)) => (
                    "refused",
                    Value::Int(*status as i64),
                    match reason.is_empty() {
                        true => Value::Null,
                        false => Value::Text(reason.clone()),
                    },
                ),
            };
            // Its members ascending by key, as an object holds them: a
            // helper taking keys and values apart was 0.1 to 0.6 KB more
            // of the server than these written out (slice 5's review).
            Value::Object(vec![
                ("outcome".into(), Value::Text(outcome.into())),
                ("reason".into(), reason),
                ("skipped".into(), Value::Int(m.skipped as i64)),
                ("slot".into(), Value::Timestamp(m.at)),
                ("status".into(), status),
            ])
        });
        // Held as long as the slot it holds is the one due: one run,
        // marked or passed over since is held no more.
        let held = (self.held_slots().iter())
            .find(|(n, h)| *n == s.name && Some(h.slot) == due_slot)
            .map_or(Value::Null, |(_, h)| {
                Value::Object(vec![
                    ("since".into(), Value::Timestamp(h.since)),
                    ("slot".into(), Value::Timestamp(h.slot)),
                    ("status".into(), Value::Int(h.status as i64)),
                ])
            });
        let schedule = crate::named::schedule_text(&s.text)
            .map_or(Value::Null, |t| Value::Text(t.to_string()));
        [schedule, next, last, held]
    }

    /// Lets a scheduled statement be defined, which every way in refuses
    /// where no scheduler runs the database's (`named::SCHEDULED_UNRUN`):
    /// what a server sets as its scheduler starts watching it -- a file's
    /// (`fennec_http::schedule::watch`) or a tenant's as it opens on a node
    /// (`fennec_http::schedule::Node`) -- never the shell or the native
    /// library, which run none. A test sets it to hold a schedule's record
    /// and drive its runs.
    pub fn allow_schedules(&mut self) {
        self.scheduling = true;
    }

    /// Hands back the marks of runs that wrote nothing which a tenant node
    /// kept as the file closed ([`Self::pending`]), read off it while it
    /// stood where it stands now -- its change and its history the same:
    /// each counts as it did, landing with the next block, and one of a
    /// definition replaced since counts for nothing. A following database
    /// runs and marks none, and takes none back; nor does one whose slot is
    /// not past its name's landed mark. A pending mark outranks the landed
    /// one by order ([`Self::due`]), so one handed back older than a mark
    /// landed since would run again the slots between them.
    pub fn keep_pending(&mut self, marks: Vec<(String, SlotMark)>) {
        if marks.is_empty() || self.history.following {
            return;
        }
        for (name, m) in marks {
            if self.last_mark(&name).is_some_and(|l| l.at >= m.at) {
                continue;
            }
            let pending = self.pending.get_or_insert_with(Default::default);
            if !pending.iter().any(|(n, _)| *n == name) {
                pending.push((name, m));
            }
        }
    }

    /// Calls `tell` with the number of scheduled statements the table
    /// holds wherever it may have changed -- a definition, a drop, an image
    /// adopted, a replica's apply, a copy promoted -- under the write lock:
    /// what wakes a scheduler that sleeps with no slot to wait for, never a
    /// timer, and what lets a server running none say it holds one unrun.
    /// Asked at a schema change and a promotion alone (`rebind`, `fork`),
    /// its answer unread. And with `None` before a scheduled definition
    /// writes anything, refused with what it answers: a tenant node names
    /// the tenant in its hint, synced, so the block bringing a schedule
    /// never lands where the node's next start would not look for it --
    /// asked under the write lock, as the definition is. One hook for both:
    /// a node's open makes one, not two.
    pub fn on_schedules(&mut self, tell: super::SchedulesHook) {
        self.on_schedules = Some(tell);
    }

    /// [`Self::on_schedules`]' hook asked with `None`, by a scheduled
    /// definition before its record is written.
    pub(super) fn entering(&self) -> Result<()> {
        match &self.on_schedules {
            Some(tell) => tell(None),
            None => Ok(()),
        }
    }

    /// Whether the table holds a scheduled statement.
    pub fn holds_schedules(&self) -> bool {
        self.statements.iter().any(|s| s.schedule.is_some())
    }

    /// Each scheduled statement's slot due at `now`, in epoch milliseconds:
    /// what to run, what it passes over and when to look again, from the
    /// table, the marks -- one pending past the landed one first -- and the
    /// time alone. Runs nothing and changes nothing; a broken statement's
    /// slots are due as any, for its runner to mark refused.
    pub fn due(&self, now: i64) -> Vec<Due> {
        self.statements
            .iter()
            .filter(|s| s.schedule.is_some())
            .map(|s| self.due_of(s, now))
            .collect()
    }

    fn due_of(&self, s: &Stored, now: i64) -> Due {
        let mut due = Due {
            name: s.name.clone(),
            run: None,
            next: None,
            ahead: None,
        };
        let Some(t) = &s.schedule else {
            return due;
        };
        // A zone this binary's table lacks: a later binary's, whose text
        // read no tree here -- marked broken, and no slot this build can
        // compute.
        let Ok(zone) = zone_named(&t.zone) else {
            return due;
        };
        let clock = Clock::of(t.times, &zone);
        let mark = self.pending_of(s).or(self.last_mark(&s.name)).map(|m| m.at);
        let anchor = mark.map_or(t.since, |m| m.max(t.since));
        if anchor > now {
            due.ahead = Some(anchor);
        }
        // Ahead by more than an hour, a reading is taken for the wrong one
        // -- a mark left by a clock that was ahead, or a `since` as far --
        // and anchors nothing. With neither anchor the latest slot runs
        // once, and its mark anchors the slots after it: a `since` taken
        // for the time itself was read again at every look, and the
        // schedule never ran.
        let wrong = |x: i64| x > now.saturating_add(AHEAD_MAX);
        let since = Some(t.since).filter(|&s| !wrong(s));
        let from = match (mark.filter(|&m| !wrong(m)), since) {
            (Some(m), Some(s)) => Some(m.max(s)),
            (m, s) => m.or(s),
        };
        let latest = clock.latest(now);
        due.run = match from {
            Some(from) => {
                (latest.filter(|&l| l > from)).map(|l| (l, clock.count(from, l).saturating_sub(1)))
            }
            None => latest.map(|l| (l, 0)),
        };
        due.next = clock.next(from.map_or(now, |f| f.max(now)));
        due
    }

    /// The latest slot of `name`'s schedule at or before `now`, its
    /// `since` and its marks aside: the `slot()` the server's token's call
    /// of it reads, the slot that has come -- the one the scheduler ran
    /// last or runs next -- under its label. `None` for a statement not
    /// scheduled, or a zone this build's table lacks.
    pub fn slot_at(&self, name: &str, now: i64) -> Option<i64> {
        let t = self.statements.get(name)?.schedule.as_ref()?;
        let zone = zone_named(&t.zone).ok()?;
        Clock::of(t.times, &zone).latest(now)
    }

    /// Runs `name`'s body for `slot`, in the block its caller opened -- a
    /// server's scheduler, which asked [`Self::due`] for the slot under the
    /// write lock it holds -- as the server's token's call runs a writing
    /// body ([`Self::run_callee`]): each statement through `run_one`,
    /// waking the bindings its writes reach, its rows bounded by a fresh
    /// `--call-max-rows`. `now()` reads `now` throughout, the run's start,
    /// as `@ttl`'s `alive` does, and `slot()` reads `slot`. The version
    /// that ran, for the mark the caller lands with the run's writes
    /// ([`Self::mark_slot`]), and the rows its last statement answered, as
    /// a call's are counted; a statement not scheduled is missing (404),
    /// and a broken one or one whose text this process holds unread is
    /// refused (409) with nothing of it run, as its call would be.
    pub fn run_slot(&mut self, name: &str, slot: i64, now: i64) -> Result<((u64, u64), u64)> {
        let s = (self.statements.get(name))
            .filter(|s| s.schedule.is_some())
            .ok_or_else(|| Error::NotFound(format!("scheduled statement `{name}`")))?;
        let version = s.version;
        if let Some(why) = &s.broken {
            return Err(Error::Exists(format!(
                "statement `{name}` is broken: {why}"
            )));
        }
        let Some(def) = s.tree.clone() else {
            return Err(Error::Exists(format!(
                "statement `{name}` is held unread: no FennecQL reader is installed"
            )));
        };
        // As a block of statements starts: what an earlier write's
        // bindings or preparation left is not this run's.
        #[cfg(not(target_arch = "wasm32"))]
        self.disarm();
        self.settle_woken();
        let callee = super::Callee {
            def,
            values: Vec::new(),
            slot: Some(slot),
        };
        let before = self.clock.replace(now);
        let c = Clocked { db: self, before };
        let rows = |r: Response| match r {
            Response::Affected(n) => n as u64,
            Response::Rows(rs) => rs.rows.len() as u64,
            _ => 0,
        };
        c.db.run_callee(&callee).map(|r| (version, rows(r)))
    }

    /// A scheduled body run for `slot` -- by the clock, or by the server's
    /// token's call -- out of line: `slot()` reads it on this thread while
    /// the body runs, and each statement's filter is handed it as written
    /// ([`slot_handed`]), so `where period = slot()` reaches an index as
    /// `where period = $1` does (`body_checked` holds it a hole, as a
    /// parameter is).
    #[cold]
    #[inline(never)]
    pub(super) fn run_slotted(
        &mut self,
        body: &[Statement],
        values: &[Value],
        slot: i64,
    ) -> Result<Response> {
        let _read = SlotRead::set(slot);
        self.run_body(body, values, Some(&Value::Timestamp(slot)))
    }

    /// Marks `name`'s slot run, or refused: where the open block holds
    /// writes -- the run's -- as the last of them, landing whole with them
    /// or put back with them; otherwise kept pending, to land as the last
    /// record of the next block that does, any writer's, or with an image.
    /// A run that wrote nothing costs no fsync, no record to an archive
    /// and no send to a standby of its own. Refused for a statement not
    /// scheduled, or for a definition other than the one the table holds.
    pub fn mark_slot(&mut self, name: &str, slot: SlotMark) -> Result<()> {
        let held = self.statements.get(name).filter(|s| s.schedule.is_some());
        match held {
            None => return Err(Error::NotFound(format!("scheduled statement `{name}`"))),
            Some(s) if s.version != slot.version => {
                return Err(Error::Query(format!(
                    "statement `{name}` was defined again since the run: its mark names the \
                     definition that ran"
                )))
            }
            Some(_) => {}
        }
        // Beside the table, never a new one: a mark changes its statement's
        // entry alone, and the block keeps the one before to put back. The
        // block's mark is the name's last, and a pending one of an earlier
        // run goes: carried after it, it would land as the last. A block put
        // back then leaves the slot it held due, as a pending mark lost to
        // a crash does, the latest due run once.
        if self.block.as_ref().is_some_and(|b| !b.heads.is_empty()) {
            self.wal(super::REC_STATEMENT, 0, &marked(name, &slot))?;
            self.note(0, crate::changes::SLOT_MARK);
            let was = self.set_mark(name.to_string(), slot);
            if let Some(p) = &mut self.pending {
                p.retain(|(n, _)| n != name);
            }
            if let Some(b) = &mut self.block {
                (b.was).push(super::Undo::Mark(Box::new((name.to_string(), was))));
            }
            return Ok(());
        }
        let pending = self.pending.get_or_insert_with(Default::default);
        pending.retain(|(n, _)| n != name);
        pending.push((name.to_string(), slot));
        Ok(())
    }

    /// The marks of runs that wrote nothing, by statement, not landed yet:
    /// a tenant node keeps a closing tenant's, and hands them back as it
    /// opens again ([`Self::keep_pending`]).
    pub fn pending(&self) -> &[(String, SlotMark)] {
        self.pending.as_deref().map_or(&[], |p| &p[..])
    }

    /// `s`'s pending mark, where one counts: of its definition as it
    /// stands -- one a replacement left counts for nothing. It is later
    /// than the mark landed by order, never by slot: a run's landed mark
    /// takes its name's pending one ([`Self::mark_slot`]), and a mark a
    /// wrong clock left ahead, landed, must not outrank the runs after it
    /// -- by slot, each run that found nothing ran its slot again.
    pub(super) fn pending_of(&self, s: &Stored) -> Option<&SlotMark> {
        let (_, p) = self.pending().iter().find(|(n, _)| *n == s.name)?;
        (s.schedule.is_some() && p.version == s.version).then_some(p)
    }

    /// Appends to `b`, a block about to land, the pending marks that
    /// count, as its last records: one write each, noted on no collection
    /// as a write that changes nothing read ([`crate::changes::SLOT_MARK`]).
    pub(super) fn carry_pending(&self, b: &mut super::Block) {
        for s in self.statements.iter() {
            if let Some(p) = self.pending_of(s) {
                b.frames.extend_from_slice(&marked(&s.name, p));
                b.heads.push((super::REC_STATEMENT, 0, b.frames.len()));
                b.notes.push((0, crate::changes::SLOT_MARK));
            }
        }
    }

    /// The pending marks, landed with the block that carried them: each
    /// that counted becomes its statement's last mark, and those that
    /// counted for nothing go.
    pub(super) fn landed_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        for (name, p) in *pending {
            if self.statements.marks(&name, p.version) {
                self.set_mark(name, p);
            }
        }
    }
}

/// `s` with `slot()` in its filter made `at`, the slot run for: a `set`
/// or a `del` made anew around it rather than the statement cloned whole --
/// `Statement`'s clone was its every variant's, 6 KB of the server (as
/// `Database::answered` makes one) -- a `get` cloned, any other as it is.
#[cold]
#[inline(never)]
pub(super) fn slot_handed<'s>(s: &'s Statement, at: &Value) -> Cow<'s, Statement> {
    let folded = |f: &Option<Expr>| {
        f.clone().map(|mut f| {
            slot_folded(&mut f, at);
            f
        })
    };
    Cow::Owned(match s {
        Statement::Update {
            collection,
            set,
            filter,
            require,
            pick,
        } => Statement::Update {
            collection: collection.clone(),
            set: set.clone(),
            filter: folded(filter),
            require: require.clone(),
            pick: pick.clone(),
        },
        Statement::Delete {
            collection,
            filter,
            require,
            pick,
        } => Statement::Delete {
            collection: collection.clone(),
            filter: folded(filter),
            require: require.clone(),
            pick: pick.clone(),
        },
        Statement::Select(sel) => {
            let mut sel = sel.clone();
            if let Some(f) = &mut sel.filter {
                slot_folded(f, at);
            }
            Statement::Select(sel)
        }
        _ => return Cow::Borrowed(s),
    })
}

/// Each `slot()` of `e` made the value `at`, outside an inner `get` -- whose
/// own reads it on the thread, as every other place does.
pub(super) fn slot_folded(e: &mut Expr, at: &Value) {
    if matches!(e, Expr::Call(n, a) if a.is_empty() && n.eq_ignore_ascii_case("slot")) {
        *e = Expr::Lit(at.clone());
    } else {
        e.each_child_mut(&mut |c| slot_folded(c, at));
    }
}

/// The zone a schedule's record names: UTC where it names none.
fn zone_named(name: &str) -> Result<Zone> {
    match name {
        "" => Ok(Zone::Fixed(0)),
        _ => crate::tz::zone_of(&crate::value::Value::Text(name.into())),
    }
}

/// A schedule's slots on its zone's clock.
struct Clock<'z> {
    times: Times,
    zone: &'z Zone,
    /// Whether each instant the clock reads one of its times is a slot --
    /// `every` a shorter interval than a day, `bucket`'s edges -- rather
    /// than the first alone.
    every: bool,
}

impl<'z> Clock<'z> {
    fn of(times: Times, zone: &'z Zone) -> Clock<'z> {
        let every = matches!(times, Times::Step(step, _) if step % DAY != 0);
        Clock { times, zone, every }
    }

    /// The latest slot at or before `t`.
    fn latest(&self, t: i64) -> Option<i64> {
        if !self.every {
            return Some(self.zone.first_reading(self.times.floor(self.high(t))?));
        }
        let mut x = t;
        for _ in 0..STRETCHES {
            let (start, _, off) = self.zone.stretch(x);
            let at = self.times.floor(x.saturating_add(off))? - off;
            if at >= start {
                return Some(at);
            }
            if self.jump_slot(start) {
                return Some(start);
            }
            x = start - 1;
        }
        None
    }

    /// The first slot after `t`.
    fn next(&self, t: i64) -> Option<i64> {
        if !self.every {
            return Some(self.zone.first_reading(self.times.above(self.high(t))?));
        }
        let mut x = t;
        for _ in 0..STRETCHES {
            let (_, end, off) = self.zone.stretch(x);
            let at = self.times.above(x.saturating_add(off))? - off;
            if at < end {
                return Some(at);
            }
            if self.jump_slot(end) {
                return Some(end);
            }
            x = end;
        }
        None
    }

    /// The slots in `(a, b]`.
    fn count(&self, a: i64, b: i64) -> u64 {
        if b <= a {
            return 0;
        }
        let mut n = 0u64;
        let mut x = a;
        if !self.every {
            // Each of the clock's times the clock reached in `(a, b]` is
            // a slot, but those a forward jump passed together: one.
            n = self.times.count(self.high(a), self.high(b));
            for _ in 0..STRETCHES {
                let (_, end, off) = self.zone.stretch(x);
                if end > b {
                    break;
                }
                let (_, _, after) = self.zone.stretch(end);
                if after > off {
                    let together = self
                        .times
                        .count(self.high(end - 1), end.saturating_add(after));
                    n = n.saturating_sub(together.saturating_sub(1));
                }
                x = end;
            }
            return n;
        }
        for _ in 0..STRETCHES {
            let (_, end, off) = self.zone.stretch(x);
            let hi = b.min(end.saturating_sub(1));
            n += self
                .times
                .count(x.saturating_add(off), hi.saturating_add(off));
            if end > b {
                break;
            }
            n += self.jump_slot(end) as u64;
            x = end;
        }
        n
    }

    /// Whether `j`, where a stretch of one offset begins, is a slot of an
    /// `every` shorter than a day: the clock reads one of its times there,
    /// or jumps forward past one.
    fn jump_slot(&self, j: i64) -> bool {
        if j == i64::MIN || j == i64::MAX {
            return false;
        }
        let (_, _, before) = self.zone.stretch(j - 1);
        let (_, _, after) = self.zone.stretch(j);
        let at = j + after;
        self.times.floor(at) == Some(at)
            || (after > before && self.times.count(j - 1 + before, at) > 0)
    }

    /// The furthest the clock read at or before `t`: past a change that set
    /// it back, the time it read before the change, until it reads it
    /// again.
    fn high(&self, t: i64) -> i64 {
        let (mut start, _, off) = self.zone.stretch(t);
        let mut high = t.saturating_add(off);
        // Every change sets a clock back by less than a day.
        for _ in 0..8 {
            if start == i64::MIN || start <= t.saturating_sub(2 * DAY) {
                break;
            }
            let (s, _, o) = self.zone.stretch(start - 1);
            high = high.max((start - 1).saturating_add(o));
            start = s;
        }
        high
    }
}

impl Times {
    /// The latest of its times of the clock at or before `l`, in local
    /// milliseconds.
    fn floor(&self, l: i64) -> Option<i64> {
        match *self {
            Times::Step(step, origin) => Some(origin + (l - origin).div_euclid(step) * step),
            Times::Months(n) => Some(month_start(month_of(l).div_euclid(n) * n)),
            Times::Cron(c) => cron_walk(&c, l.div_euclid(MINUTE), false),
        }
    }

    /// The first of its times after `l`.
    fn above(&self, l: i64) -> Option<i64> {
        match *self {
            Times::Step(step, origin) => Some(origin + ((l - origin).div_euclid(step) + 1) * step),
            Times::Months(n) => Some(month_start((month_of(l).div_euclid(n) + 1) * n)),
            Times::Cron(c) => cron_walk(&c, l.div_euclid(MINUTE) + 1, true),
        }
    }

    /// How many of its times are in `(a, b]`.
    fn count(&self, a: i64, b: i64) -> u64 {
        if b <= a {
            return 0;
        }
        let n = match *self {
            Times::Step(step, origin) => {
                (b - origin).div_euclid(step) - (a - origin).div_euclid(step)
            }
            Times::Months(n) => month_of(b).div_euclid(n) - month_of(a).div_euclid(n),
            Times::Cron(c) => {
                return cron_count(&c, a.div_euclid(MINUTE) + 1, b.div_euclid(MINUTE))
            }
        };
        n.max(0) as u64
    }
}

/// The month `l` falls in, counted from year 0's first, as `bucket` counts
/// months.
fn month_of(l: i64) -> i64 {
    let (y, m, _) = civil_from_days(l.div_euclid(DAY));
    y * 12 + m as i64 - 1
}

/// The first instant of month `m`, counted as [`month_of`] counts them.
fn month_start(m: i64) -> i64 {
    days_from_civil(m.div_euclid(12), m.rem_euclid(12) as u32 + 1, 1) * DAY
}

/// Whether the cron masks `c` hold day `day`, counted from the epoch: its
/// month, its day of the month and its day of the week each.
fn cron_day(c: &[u64; 5], day: i64) -> bool {
    let (_, m, d) = civil_from_days(day);
    // 1970-01-01 was a Thursday.
    let wd = (day + 4).rem_euclid(7);
    c[3] >> (m - 1) & 1 != 0 && c[2] >> (d - 1) & 1 != 0 && c[4] >> wd & 1 != 0
}

/// The minutes of an hour `c` holds from minute `lo` to `hi`, both within
/// it, as a mask.
fn cron_minutes(c: &[u64; 5], lo: i64, hi: i64) -> u64 {
    (c[0] >> lo << lo) & (u64::MAX >> (63 - hi))
}

/// The minute, counted from the epoch, of the first time `c` names at or
/// after minute `m` (`up`), or of the last at or before it, in local
/// milliseconds.
fn cron_walk(c: &[u64; 5], m: i64, up: bool) -> Option<i64> {
    let (mut day, mut at) = (m.div_euclid(MINUTES), m.rem_euclid(MINUTES));
    for _ in 0..CRON_DAYS {
        // Each hour of the day from `at`'s, up or down.
        let mut h = at / 60;
        while cron_day(c, day) && (0..24).contains(&h) {
            if c[1] >> h & 1 != 0 {
                let (lo, hi) = match (up, h == at / 60) {
                    (true, true) => (at % 60, 59),
                    (false, true) => (0, at % 60),
                    _ => (0, 59),
                };
                let mins = cron_minutes(c, lo, hi);
                if mins != 0 {
                    let minute = match up {
                        true => mins.trailing_zeros() as i64,
                        false => 63 - mins.leading_zeros() as i64,
                    };
                    return Some((day * MINUTES + h * 60 + minute) * MINUTE);
                }
            }
            h += if up { 1 } else { -1 };
        }
        (day, at) = match up {
            true => (day + 1, 0),
            false => (day - 1, MINUTES - 1),
        };
    }
    None
}

/// How many minutes from `lo` to `hi`, counted from the epoch, `c` names.
fn cron_count(c: &[u64; 5], lo: i64, hi: i64) -> u64 {
    let mut n = 0;
    let whole = (c[1].count_ones() * c[0].count_ones()) as u64;
    for day in lo.div_euclid(MINUTES)..=hi.div_euclid(MINUTES) {
        if !cron_day(c, day) {
            continue;
        }
        let from = (lo - day * MINUTES).max(0);
        let to = (hi - day * MINUTES).min(MINUTES - 1);
        if from == 0 && to == MINUTES - 1 {
            n += whole;
            continue;
        }
        for h in from / 60..=to / 60 {
            if c[1] >> h & 1 != 0 {
                let lo = if h == from / 60 { from % 60 } else { 0 };
                let hi = if h == to / 60 { to % 60 } else { 59 };
                n += cron_minutes(c, lo, hi).count_ones() as u64;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Interval;

    fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        (days_from_civil(y, mo, d) * MINUTES + h * 60 + mi) * MINUTE
    }

    fn cron(text: &str) -> Times {
        let c = crate::named::Cron::read(text).unwrap();
        Times::Cron([
            c.minutes,
            c.hours as u64,
            c.days as u64,
            c.months as u64,
            c.weekdays as u64,
        ])
    }

    fn zone(name: &str) -> Zone {
        zone_named(name).unwrap()
    }

    fn clock(times: Times, zone: &Zone) -> Clock<'_> {
        let every = matches!(times, Times::Step(step, _) if step % DAY != 0);
        Clock { times, zone, every }
    }

    /// The slots in `(a, b]` worked out the long way, a minute at a time:
    /// an `every`'s are the starts `bucket` gives each minute; a cron's are
    /// the first instant the clock reads each of its times, or later.
    fn brute(times: Times, z: &Zone, a: i64, b: i64) -> Vec<i64> {
        let mut out = Vec::new();
        let iv = match times {
            Times::Step(step, origin) => Some(Interval::Ms(step, origin)),
            Times::Months(n) => Some(Interval::Months(n)),
            Times::Cron(_) => None,
        };
        if let Some(iv) = iv {
            let mut t = a + MINUTE;
            while t <= b {
                out.push(z.bucket(iv, t).unwrap());
                t += MINUTE;
            }
        }
        if let Times::Cron(c) = times {
            for m in (a - DAY) / MINUTE..=(b + DAY) / MINUTE {
                let (day, at) = (m.div_euclid(MINUTES), m.rem_euclid(MINUTES));
                let named =
                    cron_day(&c, day) && c[1] >> (at / 60) & 1 != 0 && c[0] >> (at % 60) & 1 != 0;
                if named {
                    // The first minute whose clock reads it or later.
                    let mut t = m * MINUTE - 2 * DAY;
                    while t + z.offset_at(t) < m * MINUTE {
                        t += MINUTE;
                    }
                    out.push(t);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out.retain(|&s| a < s && s <= b);
        out
    }

    /// Every way the clock is asked agrees with the long way round, over
    /// a week across each change of each zone: the slots one by one
    /// (`next`), the latest at or before each (`latest`) and how many lie
    /// between two (`count`).
    #[test]
    fn the_slots_are_the_long_way_s_across_every_change() {
        let schedules = [
            Times::Step(15 * MINUTE, 0),
            Times::Step(60 * MINUTE, 0),
            Times::Step(120 * MINUTE, 0),
            Times::Step(90 * MINUTE, 0),
            Times::Step(DAY, 0),
            Times::Step(7 * DAY, 4 * DAY),
            Times::Months(1),
            cron("30 2 * * *"),
            cron("*/15 2 * * *"),
            cron("0 0 * * *"),
            cron("*/20 * * * mon-fri"),
            cron("15 1-3 1,15 * *"),
        ];
        let zones = [
            ("", [utc(2026, 3, 26, 0, 0), utc(2026, 10, 22, 0, 0)]),
            ("+05:45", [utc(2026, 3, 26, 0, 0), utc(2026, 10, 22, 0, 0)]),
            (
                "Europe/Berlin",
                [utc(2026, 3, 26, 0, 0), utc(2026, 10, 22, 0, 0)],
            ),
            // Half an hour back and forward.
            (
                "Australia/Lord_Howe",
                [utc(2026, 4, 1, 0, 0), utc(2026, 10, 1, 0, 0)],
            ),
            // At midnight: a day that starts twice, and one with none.
            (
                "America/Santiago",
                [utc(2026, 4, 2, 0, 0), utc(2026, 9, 3, 0, 0)],
            ),
        ];
        for (name, starts) in zones {
            let z = zone(name);
            for a in starts {
                let b = a + 7 * DAY;
                for times in schedules {
                    let c = clock(times, &z);
                    let want = brute(times, &z, a, b);
                    let mut got = Vec::new();
                    let mut t = a;
                    while let Some(s) = c.next(t).filter(|&s| s <= b) {
                        assert!(s > t, "{name} {times:?}: {s} after {t}");
                        got.push(s);
                        t = s;
                    }
                    assert_eq!(got, want, "{name} {times:?} from {a}");
                    assert_eq!(c.count(a, b), want.len() as u64, "{name} {times:?}");
                    for (i, &s) in want.iter().enumerate() {
                        assert_eq!(c.latest(s), Some(s), "{name} {times:?}");
                        if i > 0 {
                            assert_eq!(c.latest(s - 1), Some(want[i - 1]), "{name} {times:?}");
                            assert_eq!(c.count(want[0], s), i as u64, "{name} {times:?}");
                        }
                    }
                }
            }
        }
    }

    /// Berlin's clocks go forward at 02:00 on 29 March 2026 and back at
    /// 03:00 on 25 October: `at '30 2 * * *'` runs at the jump the first
    /// night and once the second, at the first 02:30; `*/15 2 * * *` once
    /// the first night, where it runs four times the night before; and
    /// `every 1h` twice in the hour shown twice and not in the one skipped.
    #[test]
    fn summer_time_runs_a_skipped_time_at_the_jump_and_a_repeated_one_once() {
        let z = zone("Europe/Berlin");
        let night = |d: u32, mo: u32| (utc(2026, mo, d - 1, 21, 0), utc(2026, mo, d, 4, 0));
        let at = clock(cron("30 2 * * *"), &z);
        let (a, b) = night(29, 3);
        assert_eq!(at.next(a), Some(utc(2026, 3, 29, 1, 0)), "at the jump");
        assert_eq!(at.count(a, b), 1);
        let (a, b) = night(25, 10);
        assert_eq!(
            at.next(a),
            Some(utc(2026, 10, 25, 0, 30)),
            "the first 02:30"
        );
        assert_eq!(at.count(a, b), 1);
        let quarters = clock(cron("*/15 2 * * *"), &z);
        let (a, b) = night(28, 3);
        assert_eq!(quarters.count(a, b), 4);
        let (a, b) = night(29, 3);
        assert_eq!(quarters.count(a, b), 1, "the skipped hour is one slot");
        let hourly = clock(Times::Step(60 * MINUTE, 0), &z);
        // 21:00 to 04:00 UTC: 23:00 to 05:00 local, 02:00 twice -- seven
        // starts, one more than the hours of the clock.
        let (a, b) = night(25, 10);
        assert_eq!(hourly.count(a, b), 7);
        // 22:00 to 06:00 local, 02:00 skipped: seven starts, the one at
        // the jump reading 03:00.
        let (a, b) = night(29, 3);
        assert_eq!(hourly.count(a, b), 7);
    }

    /// Each walk is bounded however far apart its slots: 29 February in
    /// a century that skips a leap year, eight years on.
    #[test]
    fn a_cron_walk_finds_a_slot_years_away() {
        let z = zone("");
        let leap = clock(cron("0 0 29 2 *"), &z);
        assert_eq!(
            leap.next(utc(2096, 3, 1, 0, 0)),
            Some(utc(2104, 2, 29, 0, 0))
        );
        assert_eq!(
            leap.latest(utc(2104, 2, 28, 0, 0)),
            Some(utc(2096, 2, 29, 0, 0))
        );
        assert_eq!(leap.count(utc(2000, 1, 1, 0, 0), utc(2104, 3, 1, 0, 0)), 26);
    }
}
