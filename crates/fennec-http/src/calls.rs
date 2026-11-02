//! What the server holds a named statement's call to beside its scope
//! (`docs/design/named-statements.md`, *Calls* and *Authority*):
//! `--call-rate`, the calls a second one caller makes and the runs of the
//! statements bound to its writes, and the log of a `call` or `wake` line
//! naming a statement its database does not hold as that line grants it.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use fennec_core::engine::Database;
use fennec_core::error::Error;
use fennec_core::query::Statement;

use crate::access::{Access, Who};
use crate::http::Response;

/// What `--call-rate` is unless told: the calls a second one caller -- a
/// scoped token's subject and issuer -- makes. A statement granted to every
/// user is a write path every user can lean on, which a tenant's quota
/// bounds in bytes and rows but not in time; a hundred a second is more
/// than a person's app makes, and a service calling more says so with
/// `--call-rate 0`.
pub const CALL_RATE: u32 = 100;

/// A caller's calls: a token bucket, refilled at the rate, holding a
/// second's worth, as MCP's tool calls are held (`mcp::admitted`).
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Who a call is counted for: the database it runs in -- a tenant's own on
/// a node, which holds one `Database` a tenant (`Tenants`) -- and the
/// caller ([`crate::access::Scope::caller`]). A `sub` names a person
/// within its tenant alone: counted by the caller only, tenant A's `sub`
/// "1" spent tenant B's.
type Key = (usize, String);

/// The most callers held at once, some 150 bytes each. A bucket refills
/// whole within a second ([`WINDOW`]), so a caller is held only while it
/// called within the last two: every caller beyond is forgotten as the
/// window turns, which hands back nothing, its bucket full again by then.
/// Past this many callers within those two, a new one waits for a turn
/// (429); none still counting is forgotten -- emptied at once past 4 096,
/// as it first was, the table gave every caller its second back, and a
/// caller holding enough tokens of other subjects reset its own limit by
/// flooding.
const CALLERS: usize = 16_384;

/// The time a bucket takes to refill from empty: it holds a second's worth
/// at the rate.
const WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// The callers in two generations: those who called since the window last
/// turned, and those who called in the window before. A turn forgets the
/// older generation, whose callers have not called for a whole window.
struct Callers {
    current: std::collections::HashMap<Key, Bucket>,
    previous: std::collections::HashMap<Key, Bucket>,
    turned: Instant,
}

impl Callers {
    fn new(now: Instant) -> Self {
        Callers {
            current: Default::default(),
            previous: Default::default(),
            turned: now,
        }
    }

    /// One call by `key` at `now`, at `rate` a second: `Err` with the
    /// milliseconds to wait, where its bucket holds no call or the table
    /// holds no room for a caller it does not hold.
    fn charge(&mut self, key: Key, rate: f64, now: Instant) -> std::result::Result<(), u64> {
        self.take(key, 1.0, rate, now)
    }

    /// `n` calls -- or a write's runs of the statements bound to it -- by
    /// `key` at once, as [`Self::charge`] charges one: none taken where
    /// the bucket holds fewer, `Err` with the milliseconds until it holds
    /// `n`, which a caller asking for more than a full bucket never sees
    /// ([`charge_runs`] refuses it first).
    fn take(&mut self, key: Key, n: f64, rate: f64, now: Instant) -> std::result::Result<(), u64> {
        if now.duration_since(self.turned) >= WINDOW {
            // Every caller of the older generation last called before the
            // last turn, a window or more ago: its bucket is full again.
            self.previous = std::mem::take(&mut self.current);
            self.turned = now;
        }
        let full = self.current.len() + self.previous.len() >= CALLERS;
        let b = match self.current.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let b = match self.previous.remove(v.key()) {
                    Some(b) => b,
                    None if full => {
                        let left = WINDOW.saturating_sub(now.duration_since(self.turned));
                        return Err((left.as_millis() as u64).max(1));
                    }
                    None => Bucket {
                        tokens: rate,
                        at: now,
                    },
                };
                v.insert(b)
            }
        };
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(rate);
        b.at = now;
        if b.tokens >= n {
            b.tokens -= n;
            return Ok(());
        }
        Err((((n - b.tokens) / rate * 1000.0).ceil() as u64).max(1))
    }

    /// `n` taken by `key` handed back, a full bucket's worth at most: the
    /// runs of a write put back.
    fn refund(&mut self, key: &Key, n: f64, rate: f64) {
        if let Some(b) = self.current.get_mut(key) {
            b.tokens = (b.tokens + n).min(rate);
        }
    }
}

static CALLERS_HELD: Mutex<Option<Callers>> = Mutex::new(None);

/// A scoped token's request holding a call into `db`, charged against
/// `rate` for its caller in that database once the token is known, before
/// anything of the statement is asked: 429 with `Retry-After` past it. The
/// server's token, and every request holding no call, is not held.
pub fn admitted<'a>(
    rate: u32,
    who: &Who,
    db: &Arc<RwLock<Database>>,
    mut stmts: impl Iterator<Item = &'a Statement>,
) -> Option<Response> {
    let scope = who.scope()?;
    if rate == 0 || !stmts.any(|s| matches!(s, Statement::Call(_))) {
        return None;
    }
    let key = (Arc::as_ptr(db) as usize, scope.caller());
    let now = Instant::now();
    let mut held = CALLERS_HELD.lock().unwrap_or_else(|e| e.into_inner());
    let Err(wait_ms) = held
        .get_or_insert_with(|| Callers::new(now))
        .charge(key, rate as f64, now)
    else {
        // Its runs are charged beside it, out of what the call left.
        RUNS.with(|r| {
            if let Some(runs) = r.borrow_mut().as_mut() {
                runs.called = true;
            }
        });
        return None;
    };
    Some(Response::retry_after(
        429,
        &format!(
            "too many calls: this server takes {rate} a second from one caller (--call-rate); \
             call again in {wait_ms} ms"
        ),
        wait_ms.div_ceil(1000),
    ))
}

/// The words a refusal of a write's runs starts with ([`charge_runs`]):
/// what a scoped call's refusal passes on whole (`access::told_call`).
pub const TOO_MANY_RUNS: &str = "too many runs";

/// The words a refusal of a write's runs that waiting lets through starts
/// with: 429, with `Retry-After` (`api::status_of`, [`wait_of`]).
pub const RUNS_WAIT: &str = "too many runs: ";

/// What a scoped request's runs of statements bound to its writes are
/// charged to (`docs/design/named-statements.md`, *A binding's
/// authority*): the database it runs in and its caller, as its calls are
/// ([`admitted`]), the rate, and what the request was charged so far.
struct Runs {
    db: usize,
    scope: Arc<crate::access::Scope>,
    rate: u32,
    /// Whether the request was charged a call as it was admitted: a
    /// call's runs are charged beside it, out of the same second.
    called: bool,
    /// The runs the request was charged since.
    spent: u32,
}

thread_local! {
    /// The scoped request this thread serves, while it is served
    /// ([`charging`]): the `Check` hook charges each binding its writes
    /// wake here, under the write lock, deep in the engine, which knows
    /// nothing of tokens -- a call's body included, which runs with no
    /// scope installed.
    static RUNS: RefCell<Option<Runs>> = const { RefCell::new(None) };
}

/// What [`charging`] set, put back as the request ends.
pub struct Charging(Option<Option<Runs>>);

impl Drop for Charging {
    fn drop(&mut self) {
        if let Some(before) = self.0.take() {
            RUNS.with(|r| *r.borrow_mut() = before);
        }
    }
}

/// Holds the runs a scoped token's request into `db` wakes to `rate`, for
/// as long as what it hands back lives: set where a request's token and
/// database are known (`handle`, MCP's endpoint), so every way a write is
/// sent -- `/query`, `/batch`, `/_call`, REST, MCP's `write`, a held
/// write's runs, each a `handle` whole -- is charged. Nothing for the
/// server's token, which is unheld, or under `--call-rate 0`.
pub fn charging(rate: u32, who: &Who, db: &Arc<RwLock<Database>>) -> Charging {
    let Who::Scoped(scope) = who else {
        return Charging(None);
    };
    if rate == 0 {
        return Charging(None);
    }
    let runs = Runs {
        db: Arc::as_ptr(db) as usize,
        scope: Arc::clone(scope),
        rate,
        called: false,
        spent: 0,
    };
    Charging(Some(RUNS.with(|r| r.borrow_mut().replace(runs))))
}

/// `rows` runs of a statement bound to a write of the scoped request this
/// thread serves, charged to its caller's `--call-rate` bucket in that
/// database before they run: a binding's run is a body run under the write
/// lock on a token's behalf, as a call's is. Past the bucket the write is
/// refused 429, its block put back, and what the request was charged for
/// runs before is handed back with it -- they never ran -- so the wait it
/// is told is for every run it woke and its retry is let through. A
/// request waking more runs than a full bucket holds, beside its call,
/// would wait for ever: refused (400), never told to come back, as a quota
/// where waiting makes no room is a refusal. Nothing outside a scoped
/// request ([`charging`]).
pub fn charge_runs(rows: usize) -> fennec_core::error::Result<()> {
    RUNS.with(|r| {
        let mut r = r.borrow_mut();
        let Some(runs) = r.as_mut() else {
            return Ok(());
        };
        let (rate, n) = (runs.rate, u32::try_from(rows).unwrap_or(u32::MAX));
        let woken = runs.spent.saturating_add(n);
        if woken.saturating_add(runs.called as u32) > rate {
            let beside = if runs.called { " beside its call" } else { "" };
            return Err(Error::Query(format!(
                "{TOO_MANY_RUNS} at once: the request's writes wake {woken} runs of statements \
                 bound to them{beside}, more than the {rate} a second --call-rate lets one \
                 caller make, so it would never be let through: write fewer rows at once"
            )));
        }
        let key = (runs.db, runs.scope.caller());
        let now = Instant::now();
        let mut held = CALLERS_HELD.lock().unwrap_or_else(|e| e.into_inner());
        let callers = held.get_or_insert_with(|| Callers::new(now));
        if callers
            .take(key.clone(), n as f64, rate as f64, now)
            .is_ok()
        {
            runs.spent = woken;
            return Ok(());
        }
        // Every run charged before is in the block this refusal puts
        // back, so none of them landed: a scoped request's writes are one
        // block -- `/query`, REST and MCP's `write` one statement, MCP no
        // JSON-RPC batch, a `/batch` one block (a scoped token compacts
        // nothing, so its every batch fits one: `handle_batch` asserts
        // it), a held write's every try a `handle` charged afresh. A path
        // landing a scoped request's statements as blocks of their own
        // must refund those of the block put back alone.
        callers.refund(&key, runs.spent as f64, rate as f64);
        let wait_ms = (callers.take(key, woken as f64, rate as f64, now)).map_or_else(|w| w, |_| 1);
        // Taken where the refund made room after all: handed back too.
        runs.spent = 0;
        Err(Error::Query(format!(
            "{RUNS_WAIT}this server runs {rate} statements bound to writes a second for one \
             caller (--call-rate), and the write woke {woken}; write again in {wait_ms} ms"
        )))
    })
}

/// The seconds a refusal of [`RUNS_WAIT`]'s says to wait: its `Retry-After`.
pub fn wait_of(message: &str) -> u64 {
    let ms = (message.rsplit_once(" in "))
        .and_then(|(_, t)| t.strip_suffix(" ms"))
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(1000);
    ms.div_ceil(1000).max(1)
}

/// Logs each `call` line naming a statement `db` does not hold, or holds
/// bound to writes, which no call runs, and each `wake` line naming one it
/// does not hold, or holds callable, which no write wakes: as the server
/// starts, as a tenant (`tenant`) opens, and after a `drop` took one away.
/// A typo in the policy grants nothing, and says so.
pub fn log_unheld(access: Option<&Access>, db: &Database, tenant: Option<&str>) {
    let Some(access) = access else {
        return;
    };
    let at = || {
        tenant
            .map(|t| format!("tenant `{t}`: "))
            .unwrap_or_default()
    };
    let t = db.statements();
    for (line, name) in access.call_grants() {
        match t.get(name).map(|s| s.after.is_some()) {
            None => crate::log!(
                "{}policy line {line} grants calling statement `{name}`, which the database \
                 does not hold: the line grants nothing until it does",
                at()
            ),
            Some(true) => crate::log!(
                "{}policy line {line} grants calling statement `{name}`, which is bound to \
                 writes: the writes it binds run it, and no call does, so the line grants \
                 nothing; grant waking it with `{name} wake`",
                at()
            ),
            Some(false) => {}
        }
    }
    for (line, name) in access.wake_grants() {
        match t.get(name).map(|s| s.after.is_some()) {
            None => crate::log!(
                "{}policy line {line} grants waking statement `{name}`, which the database \
                 does not hold: the line grants nothing until it does",
                at()
            ),
            Some(false) => crate::log!(
                "{}policy line {line} grants waking statement `{name}`, which is bound to no \
                 writes: `wake` grants a bound statement alone, so the line grants nothing; \
                 grant calling it with `{name} call`",
                at()
            ),
            Some(true) => {}
        }
    }
}

/// [`log_unheld`] after `stmts` ran, where one dropped a statement.
pub fn after_drop<'a>(
    access: Option<&Access>,
    db: &Database,
    tenant: Option<&str>,
    mut stmts: impl Iterator<Item = &'a Statement>,
) {
    if access.is_some() && stmts.any(|s| matches!(s, Statement::DropStatement { .. })) {
        log_unheld(access, db, tenant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(db: usize, caller: &str) -> Key {
        (db, caller.to_string())
    }

    /// A caller's calls are counted in the database they run in: tenant A's
    /// `sub` "1" spends nothing of tenant B's.
    #[test]
    fn a_caller_is_counted_by_its_tenant() {
        let now = Instant::now();
        let mut t = Callers::new(now);
        assert!(t.charge(key(1, "iss\u{0}1"), 2.0, now).is_ok());
        assert!(t.charge(key(1, "iss\u{0}1"), 2.0, now).is_ok());
        assert!(t.charge(key(1, "iss\u{0}1"), 2.0, now).is_err());
        assert!(t.charge(key(2, "iss\u{0}1"), 2.0, now).is_ok());
    }

    /// A table full of callers forgets none still counting: a caller who
    /// spent its calls and then made the table overflow with other
    /// subjects is still spent, and a newcomer waits for the turn rather
    /// than push one out. A turn forgets the callers that waited a window.
    #[test]
    fn a_flood_of_callers_hands_back_no_calls() {
        let now = Instant::now();
        let mut t = Callers::new(now);
        let me = key(1, "iss\u{0}me");
        assert!(t.charge(me.clone(), 2.0, now).is_ok());
        assert!(t.charge(me.clone(), 2.0, now).is_ok());
        assert!(t.charge(me.clone(), 2.0, now).is_err());
        let mut refused = 0;
        for i in 0..CALLERS + 100 {
            if t.charge(key(1, &format!("iss\u{0}{i}")), 2.0, now).is_err() {
                refused += 1;
            }
        }
        assert_eq!(refused, 101, "the newcomers past the table wait");
        assert!(t.charge(me.clone(), 2.0, now).is_err(), "still spent");
        assert!(t.current.len() + t.previous.len() <= CALLERS);
        // A window on, the table turns: the callers move to the older
        // generation, still held -- they called within the last two -- and
        // a caller's calls come back by the time alone.
        let later = now + WINDOW;
        assert!(t.charge(me.clone(), 2.0, later).is_ok());
        assert!(t.charge(key(1, "iss\u{0}new"), 2.0, later).is_err());
        // A second turn forgets those who did not call again.
        let last = later + WINDOW + Duration::from_millis(1);
        assert!(t.charge(key(1, "iss\u{0}new"), 2.0, last).is_ok());
        assert_eq!(t.previous.len(), 1, "me, from the window before");
        assert_eq!(t.current.len(), 1);
    }
}
