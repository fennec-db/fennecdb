//! What the server holds a named statement's call to beside its scope
//! (`docs/design/named-statements.md`, *Calls* and *Authority*):
//! `--call-rate`, the calls a second one caller makes, and the log of a
//! `call` line naming a statement its database does not hold.

use std::sync::Mutex;
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

/// Callers past this are forgotten all at once, which forgives each once
/// rather than let the map grow with every subject a token names.
const CALLERS: usize = 4096;

static BUCKETS: Mutex<Option<std::collections::HashMap<String, Bucket>>> = Mutex::new(None);

/// A scoped token's request holding a call, charged against `rate` for its
/// caller once the token is known, before anything of the statement is
/// asked: 429 with `Retry-After` past it. The server's token, and every
/// request holding no call, is not held.
pub fn admitted<'a>(
    rate: u32,
    who: &Who,
    mut stmts: impl Iterator<Item = &'a Statement>,
) -> Option<Response> {
    let scope = who.scope()?;
    if rate == 0 || !stmts.any(|s| matches!(s, Statement::Call(_))) {
        return None;
    }
    let key = scope.caller();
    let rate = rate as f64;
    let now = Instant::now();
    let mut held = BUCKETS.lock().unwrap_or_else(|e| e.into_inner());
    let map = held.get_or_insert_with(Default::default);
    if map.len() >= CALLERS && !map.contains_key(&key) {
        map.clear();
    }
    let b = map.entry(key).or_insert(Bucket {
        tokens: rate,
        at: now,
    });
    b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(rate);
    b.at = now;
    if b.tokens >= 1.0 {
        b.tokens -= 1.0;
        return None;
    }
    let wait_ms = ((1.0 - b.tokens) / rate * 1000.0).ceil() as u64;
    Some(Response::retry_after(
        429,
        &format!(
            "too many calls: this server takes {} a second from one caller (--call-rate); call \
             again in {wait_ms} ms",
            rate as u32
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
