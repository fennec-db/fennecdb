//! What the server holds a named statement's call to beside its scope
//! (`docs/design/named-statements.md`, *Calls* and *Authority*):
//! `--call-rate`, the calls a second one caller makes and the runs of the
//! statements bound to its writes, and the log of a `call` or `wake` line
//! naming a statement its database does not hold as that line grants it.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use fennec_core::engine::Database;
use fennec_core::error::Error;
use fennec_core::query::Statement;

use crate::access::{Access, Scope, Who};
use crate::http::Response;
use crate::rate;

/// What `--call-rate` is unless told: the calls a second one caller -- a
/// scoped token's subject and issuer -- makes. A statement granted to every
/// user is a write path every user can lean on, which a tenant's quota
/// bounds in bytes and rows but not in time; a hundred a second is more
/// than a person's app makes, and a service calling more says so with
/// `--call-rate 0`.
pub const CALL_RATE: u32 = 100;

/// A caller's calls are a token bucket in [`crate::rate::CALLS`], refilled
/// at the rate, holding a second's worth, as MCP's tool calls are held
/// (`mcp::admitted`). Who a call is counted for: the database it runs in --
/// a tenant's own on a node, which holds one `Database` a tenant
/// (`Tenants`) -- and the caller ([`crate::access::Scope::caller`],
/// [`crate::rate::keyed`]). A `sub` names a person within its tenant
/// alone: counted by the caller only, tenant A's `sub` "1" spent tenant
/// B's.
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
    let charged = rate::keyed(Arc::as_ptr(db) as usize, scope, |key| {
        rate::CALLS.lock(key).take(key, 1.0, rate as f64)
    });
    let Err(wait_ms) = charged else {
        // Its runs are charged beside it, out of what the call left.
        scope.runs().called.store(true, Ordering::Relaxed);
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
/// authority*): the database it runs in, as its calls are ([`admitted`]),
/// and the rate, where the request is charged at all ([`Runs::to`]), and
/// what the request was charged so far. Held in the token's scope, which
/// is made for each request (`authenticate`), and read only as a binding
/// wakes ([`charge_runs`]): a scoped write waking nothing pays nothing for
/// it. A thread-local set and put back around every scoped request, as it
/// was first, cost each some 50 instructions (F2's slice 4).
#[derive(Default)]
pub struct Runs {
    to: Option<(usize, u32)>,
    /// Whether the request was charged a call as it was admitted: a
    /// call's runs are charged beside it, out of the same second.
    called: AtomicBool,
    /// The runs the request was charged since.
    spent: AtomicU32,
}

impl Runs {
    /// The runs a scoped token's request into `db` wakes, held to `rate`:
    /// set where a request's token and database are known (`handle`, MCP's
    /// endpoint), so every way a write is sent -- `/query`, `/batch`,
    /// `/_call`, REST, MCP's `write`, a held write's runs, each a `handle`
    /// whole -- is charged. Nothing under `--call-rate 0`.
    pub fn to(rate: u32, db: &Arc<RwLock<Database>>) -> Runs {
        Runs {
            to: (rate != 0).then_some((Arc::as_ptr(db) as usize, rate)),
            ..Runs::default()
        }
    }

    /// The same request's, for the scope bound for its statements
    /// (`Scope::bound`), which its writes then run as.
    pub fn copied(&self) -> Runs {
        Runs {
            to: self.to,
            called: AtomicBool::new(self.called.load(Ordering::Relaxed)),
            spent: AtomicU32::new(self.spent.load(Ordering::Relaxed)),
        }
    }
}

/// `rows` runs of a statement bound to a write of `caller`'s request --
/// the token whose write is running or whose call's body is
/// (`access::charged_to`) -- charged to its `--call-rate` bucket in that
/// database before they run: a binding's run is a body run under the write
/// lock on a token's behalf, as a call's is. Past the bucket the write is
/// refused 429, its block put back, and what the request was charged for
/// runs before is handed back with it -- they never ran -- so the wait it
/// is told is for every run it woke and its retry is let through. A
/// request waking more runs than a full bucket holds, beside its call,
/// would wait for ever: refused (400), never told to come back, as a quota
/// where waiting makes no room is a refusal. Nothing for the server's
/// token, which is unheld, or a request charged nothing ([`Runs::to`]).
pub fn charge_runs(caller: Option<&Scope>, rows: usize) -> fennec_core::error::Result<()> {
    let Some(scope) = caller else {
        return Ok(());
    };
    let runs = scope.runs();
    let Some((db, rate)) = runs.to else {
        return Ok(());
    };
    let n = u32::try_from(rows).unwrap_or(u32::MAX);
    let (called, spent) = (
        runs.called.load(Ordering::Relaxed),
        runs.spent.load(Ordering::Relaxed),
    );
    let woken = spent.saturating_add(n);
    if woken.saturating_add(called as u32) > rate {
        let beside = if called { " beside its call" } else { "" };
        return Err(Error::Query(format!(
            "{TOO_MANY_RUNS} at once: the request's writes wake {woken} runs of statements \
             bound to them{beside}, more than the {rate} a second --call-rate lets one \
             caller make, so it would never be let through: write fewer rows at once"
        )));
    }
    rate::keyed(db, scope, |key| {
        let mut callers = rate::CALLS.lock(key);
        if callers.take(key, n as f64, rate as f64).is_ok() {
            runs.spent.store(woken, Ordering::Relaxed);
            return Ok(());
        }
        // Every run charged before is in the block this refusal puts back,
        // so none of them landed: a scoped request's writes are one block
        // -- `/query`, REST and MCP's `write` one statement, MCP no
        // JSON-RPC batch, a `/batch` one block (a scoped token compacts
        // nothing, so its every batch fits one: `handle_batch` asserts
        // it), a held write's every try a `handle` charged afresh. A path
        // landing a scoped request's statements as blocks of their own
        // must refund those of the block put back alone.
        callers.refund(key, spent as f64, rate as f64);
        let wait_ms = (callers.take(key, woken as f64, rate as f64)).map_or_else(|w| w, |_| 1);
        Err(wait_ms)
    })
    .map_err(|wait_ms| {
        // Taken where the refund made room after all: handed back too.
        runs.spent.store(0, Ordering::Relaxed);
        Error::Query(format!(
            "{RUNS_WAIT}this server runs {rate} statements bound to writes a second for one \
             caller (--call-rate), and the write woke {woken}; write again in {wait_ms} ms"
        ))
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
/// bound to writes or run by the clock, which no call of a token's runs,
/// and each `wake` line naming one it does not hold, or holds callable or
/// scheduled, which no write wakes: as the server starts, as a tenant
/// (`tenant`) opens, and after a `drop` took one away. A typo in the
/// policy grants nothing, and says so.
pub fn log_unheld(access: Option<&Access>, db: &Database, tenant: Option<&str>) {
    use fennec_core::named::Kind;
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
        match t.get(name).map(|s| s.kind()) {
            None => crate::log!(
                "{}policy line {line} grants calling statement `{name}`, which the database \
                 does not hold: the line grants nothing until it does",
                at()
            ),
            Some(Kind::Bound(..)) => crate::log!(
                "{}policy line {line} grants calling statement `{name}`, which is bound to \
                 writes: the writes it binds run it, and no call does, so the line grants \
                 nothing; grant waking it with `{name} wake`",
                at()
            ),
            Some(Kind::Scheduled) => crate::log!(
                "{}policy line {line} grants calling statement `{name}`, which runs by the \
                 clock: a token held to a policy never calls it, so the line grants nothing",
                at()
            ),
            Some(Kind::Hooked) => crate::log!(
                "{}policy line {line} grants calling statement `{name}`, which takes a hook's \
                 events: a token held to a policy never calls it, so the line grants nothing",
                at()
            ),
            Some(Kind::Called) => {}
        }
    }
    for (line, name) in access.wake_grants() {
        match t.get(name).map(|s| s.kind()) {
            None => crate::log!(
                "{}policy line {line} grants waking statement `{name}`, which the database \
                 does not hold: the line grants nothing until it does",
                at()
            ),
            Some(Kind::Called) => crate::log!(
                "{}policy line {line} grants waking statement `{name}`, which is bound to no \
                 writes: `wake` grants a bound statement alone, so the line grants nothing; \
                 grant calling it with `{name} call`",
                at()
            ),
            Some(Kind::Scheduled) => crate::log!(
                "{}policy line {line} grants waking statement `{name}`, which runs by the \
                 clock: `wake` grants a statement bound to writes alone, and no write wakes \
                 it, so the line grants nothing",
                at()
            ),
            Some(Kind::Hooked) => crate::log!(
                "{}policy line {line} grants waking statement `{name}`, which takes a hook's \
                 events: `wake` grants a statement bound to writes alone, and no write wakes \
                 it, so the line grants nothing",
                at()
            ),
            Some(Kind::Bound(..)) => {}
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
