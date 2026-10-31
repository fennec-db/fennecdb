//! What the server holds a named statement's call to beside its scope
//! (`docs/design/named-statements.md`, *Calls* and *Authority*):
//! `--call-rate`, the calls a second one caller makes, and the log of a
//! `call` line naming a statement its database does not hold.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use fennec_core::engine::Database;
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
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            return Ok(());
        }
        Err(((1.0 - b.tokens) / rate * 1000.0).ceil() as u64)
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
    let wait_ms = held
        .get_or_insert_with(|| Callers::new(now))
        .charge(key, rate as f64, now)
        .err()?;
    Some(Response::retry_after(
        429,
        &format!(
            "too many calls: this server takes {rate} a second from one caller (--call-rate); \
             call again in {wait_ms} ms"
        ),
        wait_ms.div_ceil(1000),
    ))
}

/// Logs each `call` line naming a statement `db` does not hold: as the
/// server starts, as a tenant (`tenant`) opens, and after a `drop` took
/// one away. A typo in the policy grants nothing, and says so.
pub fn log_unheld(access: Option<&Access>, db: &Database, tenant: Option<&str>) {
    let Some(access) = access else {
        return;
    };
    for (line, name) in access.call_grants() {
        if db.statements().get(name).is_none() {
            let at = tenant
                .map(|t| format!("tenant `{t}`: "))
                .unwrap_or_default();
            crate::log!(
                "{at}policy line {line} grants calling statement `{name}`, which the database \
                 does not hold: the line grants nothing until it does"
            );
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
