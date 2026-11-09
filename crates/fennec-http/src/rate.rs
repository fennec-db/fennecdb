//! What one caller -- a scoped token's subject and issuer -- may ask a
//! second of a database: `--request-rate`, every request it makes, and the
//! table of callers both it and `--call-rate` count in
//! (`docs/design/quotas.md`, *A caller's request rate*).

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Instant;

use fennec_core::engine::Database;

use crate::access::Scope;
use crate::http::Response;

/// What `--request-rate` is unless told: the requests a second one caller
/// makes of one database, every route counted -- `/query`, `/batch`,
/// `/_call`, REST, a subscription's opening, MCP, `/_whoami`. A scoped
/// token is a person's: a page opening with fifty requests, a search box
/// asking at every keystroke and a dozen live queries stay under a tenth
/// of it, while one caller in a loop, which on one connection asks a node
/// some 45 000 a second (`make requests-bench`: a row by id in 0.022 ms),
/// is held to a fiftieth of that, the rest of the node its neighbours'.
/// Ten times `--call-rate`'s 100: a call is a write path, a request most
/// often a read. A service asking more says so with `--request-rate 0`, or
/// asks with the server's token, which is never held.
pub const REQUEST_RATE: u32 = 1000;


/// A caller's bucket: refilled at the rate, holding a second's worth, and
/// when it was last charged, in seconds since its lock's callers were made
/// ([`Callers::born`]): one clock read a charge and one subtraction, where
/// `Instant`s took three, each a `timespec`'s carry.
struct Bucket {
    tokens: f64,
    at: f64,
}

/// The seconds a bucket takes to refill from empty: it holds a second's
/// worth at the rate. A caller that has not asked for a window holds a full
/// bucket, so forgetting it hands back nothing.
const WINDOW: f64 = 1.0;

/// The locks a table is cut into, a caller's by its key. One lock for every
/// scoped request of a node is a line every core's request passes through
/// in turn; sixteen, as the verified tokens' cache is cut
/// (`access::SHARDS`), leave two threads meeting at one only where their
/// callers' keys fall together, held for a lookup and a few floats.
const SHARDS: usize = 16;

/// The callers one lock holds, swept once a window of those who have not
/// asked for one. Kept as two generations a turn apart first, every
/// caller's entry moved from the older map to the newer as it next asked,
/// a hash, a free and a table grown again for each caller every second.
struct Callers {
    held: HashMap<Box<[u8]>, Bucket, Seeded>,
    /// When they were made: what a bucket's times count from.
    born: Instant,
    /// When the next sweep is due, in seconds since `born`.
    sweep: f64,
}

impl Callers {
    fn new(born: Instant) -> Self {
        Callers {
            held: HashMap::with_hasher(Seeded(RandomState::new().hash_one(0u64))),
            born,
            sweep: WINDOW,
        }
    }
}

/// The tables' hasher: a key eight bytes at a time folded through a 128-bit
/// multiply, from a seed `RandomState` draws for each lock's callers. SipHash,
/// the default, took some 290 instructions a key of 40 bytes, more than the
/// rest of the charge; a key is a signed token's issuer and subject and a
/// lock holds a few thousand at most, so a seed no client sees is defence
/// enough against keys chosen to meet.
#[derive(Clone, Copy)]
struct Seeded(u64);

impl BuildHasher for Seeded {
    type Hasher = Fold;
    fn build_hasher(&self) -> Fold {
        Fold(self.0)
    }
}

struct Fold(u64);

const MIX: u64 = 0x9e37_79b9_7f4a_7c15;

fn fold(a: u64, b: u64) -> u64 {
    let m = (a as u128).wrapping_mul(b as u128);
    (m as u64) ^ ((m >> 64) as u64)
}

impl Hasher for Fold {
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            h = fold(h ^ u64::from_le_bytes(*w), MIX);
        }
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        self.0 = fold(h ^ u64::from_le_bytes(last), MIX ^ rest.len() as u64);
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = fold(self.0 ^ n as u64, MIX);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Where a table's callers are counted and what it does with one more than
/// it holds.
pub(crate) struct Table {
    shards: [Mutex<Option<Callers>>; SHARDS],
    /// The callers a lock holds at most: those that asked within the last
    /// window or two.
    most: usize,
    /// Whether a caller the table has no room for is let through unheld
    /// rather than told to wait for the next sweep.
    open: bool,
}

/// `--call-rate`'s callers: 16 384 at most, some 100 bytes each. Past them
/// a new caller waits for the next sweep (429); none still counting is
/// forgotten -- emptied at once past 4 096, as it first was, the table gave
/// every caller its second back, and a caller holding enough tokens of
/// other subjects reset its own limit by flooding.
pub(crate) static CALLS: Table = Table::new(16_384, false);

/// `--request-rate`'s callers: 65 536 at most, some 100 bytes each, 6.5 MB
/// at the full. Past them a new caller is let through unheld, never
/// refused: a bound on by default must not turn a busy server's next user
/// away for the table's size, nor let a holder of many tokens fill it and
/// refuse everyone else; one counting is never forgotten, so a caller
/// cannot reset its own bucket by flooding either.
static REQUESTS: Table = Table::new(65_536, true);

impl Table {
    const fn new(most: usize, open: bool) -> Table {
        Table {
            shards: [const { Mutex::new(None) }; SHARDS],
            most: most / SHARDS,
            open,
        }
    }

    /// The lock `key`'s caller is counted under, held: its subject's last
    /// bytes, which tell one id from the next, and its database's address.
    /// The clock is read before the lock is taken.
    pub(crate) fn lock(&self, key: &[u8]) -> Held<'_> {
        let db = u64::from_le_bytes(key[..8].try_into().unwrap_or_default());
        let tail = key[key.len().saturating_sub(8)..]
            .iter()
            .fold(db >> 4, |a, &c| a.wrapping_mul(31).wrapping_add(c as u64));
        let at = Instant::now();
        let shard = &self.shards[tail as usize % SHARDS];
        let mut guard = shard.lock().unwrap_or_else(|e| e.into_inner());
        let born = guard.get_or_insert_with(|| Callers::new(at)).born;
        Held {
            guard,
            table: self,
            now: at.saturating_duration_since(born).as_secs_f64(),
        }
    }
}

/// A table's lock held for one caller's charge, the time it was taken at.
pub(crate) struct Held<'a> {
    guard: MutexGuard<'a, Option<Callers>>,
    table: &'a Table,
    now: f64,
}

impl Held<'_> {
    /// `n` taken from `key`'s bucket at `rate` a second: `Err` with the
    /// milliseconds until it holds `n`, none taken, or, in a table not
    /// `open`, until the next sweep where the table holds no room for a
    /// caller it does not hold. A caller asking for more than a full bucket
    /// never sees its wait end (`calls::charge_runs` refuses it first).
    pub(crate) fn take(&mut self, key: &[u8], n: f64, rate: f64) -> Result<(), u64> {
        let now = self.now;
        let Some(t) = self.guard.as_mut() else {
            return Ok(());
        };
        if now >= t.sweep {
            // Once a window, under the lock: a few thousand entries at
            // most, looked at in some tens of microseconds by the one
            // request that comes upon it.
            t.held.retain(|_, b| now - b.at < WINDOW);
            t.sweep = now + WINDOW;
        }
        if let Some(b) = t.held.get_mut(key) {
            return spend(b, n, rate, now);
        }
        if t.held.len() >= self.table.most {
            if self.table.open {
                return Ok(());
            }
            return Err((((t.sweep - now) * 1000.0).ceil() as u64).max(1));
        }
        let fresh = Bucket {
            tokens: rate,
            at: now,
        };
        spend(t.held.entry(Box::from(key)).or_insert(fresh), n, rate, now)
    }

    /// `n` taken by `key` handed back, a full bucket's worth at most: the
    /// runs of a write put back.
    pub(crate) fn refund(&mut self, key: &[u8], n: f64, rate: f64) {
        if let Some(b) = self.guard.as_mut().and_then(|t| t.held.get_mut(key)) {
            b.tokens = (b.tokens + n).min(rate);
        }
    }
}

/// `n` out of `b`, refilled to `now` first. A clock read before another
/// thread's charge may come to the lock after it: no time went by for it.
fn spend(b: &mut Bucket, n: f64, rate: f64, now: f64) -> Result<(), u64> {
    if now > b.at {
        b.tokens = (b.tokens + (now - b.at) * rate).min(rate);
        b.at = now;
    }
    if b.tokens >= n {
        b.tokens -= n;
        return Ok(());
    }
    Err((((n - b.tokens) / rate * 1000.0).ceil() as u64).max(1))
}

/// The key `scope`'s caller is counted by in the database at `db`: the
/// address, then the issuer, a nul and the subject -- a `sub` is unique
/// only within its issuer's, and names a person within its tenant alone,
/// so tenant A's `sub` "1" spends nothing of tenant B's. Made on the stack
/// where it fits: formatted into a `String` for every scoped request, it
/// was an allocation a request.
pub(crate) fn keyed<R>(db: usize, scope: &Scope, f: impl FnOnce(&[u8]) -> R) -> R {
    let (iss, sub) = scope.caller();
    let len = 9 + iss.len() + sub.len();
    let mut stack = [0u8; 120];
    let mut heap = Vec::new();
    let key = match len <= stack.len() {
        true => &mut stack[..len],
        false => {
            heap.resize(len, 0);
            &mut heap[..]
        }
    };
    key[..8].copy_from_slice(&(db as u64).to_le_bytes());
    key[8..8 + iss.len()].copy_from_slice(iss.as_bytes());
    key[8 + iss.len()] = 0;
    key[9 + iss.len()..].copy_from_slice(sub.as_bytes());
    f(key)
}

thread_local! {
    /// Set while a held write runs again (`respond`): the request it is
    /// was charged as it first ran.
    static AGAIN: Cell<bool> = const { Cell::new(false) };
}

/// `run`, a held write's run after its first: its token is asked again, its
/// request not charged again. A client's one request held for ten wakes
/// spent eleven of its second, and was answered 429 at the run past them,
/// the rows it waited for found.
pub(crate) fn again<R>(run: impl FnOnce() -> R) -> R {
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            AGAIN.with(|a| a.set(false));
        }
    }
    AGAIN.with(|a| a.set(true));
    let _done = Done;
    run()
}

/// Requests refused for their caller's `--request-rate`:
/// `fennec_request_rate_refused_total`.
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// The requests [`admitted`] refused so far.
pub fn refused() -> u64 {
    REFUSED.load(Ordering::Relaxed)
}

/// A scoped token's request into `db`, charged to its caller's bucket at
/// `rate` a second as its token is verified (`authenticate`), before
/// anything of it is read past its head: the 429 to answer past it, naming
/// its wait. The server's token and an unscoped server never come here;
/// `--request-rate 0` and a held write's runs after its first pay a branch.
pub(crate) fn admitted(rate: u32, db: &Arc<RwLock<Database>>, scope: &Scope) -> Option<Response> {
    if rate == 0 || AGAIN.with(Cell::get) {
        return None;
    }
    let r = rate as f64;
    let charged = keyed(Arc::as_ptr(db) as usize, scope, |key| {
        REQUESTS.lock(key).take(key, 1.0, r)
    });
    charged.err().map(|wait_ms| too_many(rate, wait_ms))
}

/// The 429 a caller past `--request-rate` is answered: `Retry-After` in
/// whole seconds, rounded up, as HTTP has it, and the body's `retry_after`
/// and `retry_after_ms`, the wait itself -- a bucket of 1 000 a second
/// refills a request in a millisecond, and a client told a second would
/// wait a thousand times too long. Nothing of the request ran, so a client
/// sends it again after the wait (`completed` is never in it).
#[cold]
fn too_many(rate: u32, wait_ms: u64) -> Response {
    REFUSED.fetch_add(1, Ordering::Relaxed);
    let secs = wait_ms.div_ceil(1000).max(1);
    let why = format!(
        "too many requests: this server takes {rate} a second from one caller \
         (--request-rate); ask again in {wait_ms} ms"
    );
    let mut body = String::from("{\"error\":");
    fennec_core::json::escape_into(&mut body, &why);
    body.push_str(&format!(
        ",\"retry_after\":{secs},\"retry_after_ms\":{wait_ms}}}"
    ));
    Response::json(429, body).header("Retry-After", &secs.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn key(db: u64, caller: &str) -> Vec<u8> {
        let mut k = db.to_le_bytes().to_vec();
        k.extend_from_slice(caller.as_bytes());
        k
    }

    /// One charge of `k` at `now` seconds, 2 a second, in a table of `most`
    /// callers a lock, `open` or not, holding `t`.
    fn charge(t: &mut Callers, most: usize, open: bool, k: &[u8], now: f64) -> Result<(), u64> {
        let table = Table {
            shards: [const { Mutex::new(None) }; SHARDS],
            most,
            open,
        };
        let mut shard = table.shards[0].lock().unwrap();
        *shard = Some(std::mem::replace(t, Callers::new(Instant::now())));
        let mut held = Held {
            guard: shard,
            table: &table,
            now,
        };
        let r = held.take(k, 1.0, 2.0);
        *t = held.guard.take().unwrap();
        r
    }

    /// A caller is counted in the database it asks: tenant A's `sub` "1"
    /// spends nothing of tenant B's.
    #[test]
    fn a_caller_is_counted_by_its_tenant() {
        let mut t = Callers::new(Instant::now());
        let one = key(1, "iss\u{0}1");
        assert!(charge(&mut t, 8, false, &one, 0.0).is_ok());
        assert!(charge(&mut t, 8, false, &one, 0.0).is_ok());
        assert!(charge(&mut t, 8, false, &one, 0.0).is_err());
        assert!(charge(&mut t, 8, false, &key(2, "iss\u{0}1"), 0.0).is_ok());
    }

    /// A table full of callers forgets none still counting: a caller who
    /// spent its bucket and then made the table overflow with other
    /// subjects is still spent, and a newcomer waits for the sweep rather
    /// than push one out -- or, in an open table, goes through unheld. A
    /// sweep forgets the callers that did not ask for a window.
    #[test]
    fn a_flood_of_callers_hands_back_nothing() {
        for open in [false, true] {
            let mut t = Callers::new(Instant::now());
            let most = 64;
            let me = key(1, "iss\u{0}me");
            let at = 0.5;
            assert!(charge(&mut t, most, open, &me, at).is_ok());
            assert!(charge(&mut t, most, open, &me, at).is_ok());
            assert!(charge(&mut t, most, open, &me, at).is_err());
            let mut refused = 0;
            for i in 0..most + 10 {
                let k = key(1, &format!("iss\u{0}{i}"));
                if charge(&mut t, most, open, &k, at).is_err() {
                    refused += 1;
                }
            }
            let past = if open { 0 } else { 11 };
            assert_eq!(refused, past, "the newcomers past the table, open {open}");
            assert!(charge(&mut t, most, open, &me, at).is_err(), "still spent");
            assert_eq!(t.held.len(), most);
            // The sweep at a second forgets no one: each asked within the
            // window. A newcomer still finds no room.
            let new = key(1, "iss\u{0}new");
            assert_eq!(charge(&mut t, most, open, &new, 1.0).is_err(), !open);
            assert_eq!(t.held.len(), most, "none forgotten while counting");
            // `me` asks again, its bucket back by the time alone; at the
            // next sweep the others have not asked for a window.
            assert!(charge(&mut t, most, open, &me, 1.6).is_ok());
            assert!(charge(&mut t, most, open, &new, 2.0).is_ok());
            assert_eq!(t.held.len(), 2, "me and new, the rest forgotten");
        }
    }

    /// The wait a bucket names lets its caller through, never sooner: at
    /// 2 a second, an empty bucket holds a request again 500 ms on.
    #[test]
    fn the_wait_named_is_the_wait() {
        let mut t = Callers::new(Instant::now());
        let (me, you) = (key(1, "iss\u{0}me"), key(1, "iss\u{0}you"));
        for k in [&me, &you] {
            assert!(charge(&mut t, 8, true, k, 0.25).is_ok());
            assert!(charge(&mut t, 8, true, k, 0.25).is_ok());
            assert_eq!(charge(&mut t, 8, true, k, 0.25), Err(500));
        }
        assert!(charge(&mut t, 8, true, &you, 0.749).is_err());
        assert!(charge(&mut t, 8, true, &me, 0.75).is_ok());
    }

    /// A clock read before another thread's charge, coming to the lock
    /// after it, takes nothing back from the bucket.
    #[test]
    fn a_late_clock_takes_nothing_back() {
        let mut t = Callers::new(Instant::now());
        let me = key(1, "iss\u{0}me");
        assert!(charge(&mut t, 8, true, &me, 0.5).is_ok());
        assert!(charge(&mut t, 8, true, &me, 0.4).is_ok());
        assert!(charge(&mut t, 8, true, &me, 0.4).is_err());
    }

    /// The hasher reads every byte of a key, its length too.
    #[test]
    fn keys_that_differ_hash_apart() {
        let s = Seeded(7);
        let a = s.hash_one(&key(1, "iss\u{0}user-1")[..]);
        let b = s.hash_one(&key(1, "iss\u{0}user-2")[..]);
        let c = s.hash_one(&key(2, "iss\u{0}user-1")[..]);
        let d = s.hash_one(&key(1, "iss\u{0}user-1\u{0}")[..]);
        assert!(a != b && a != c && a != d && b != c);
    }

    /// A held write's runs after its first are the request it is, charged
    /// once: a caller at one a second, its second spent, runs again and
    /// again under [`again`], and is held at its next request.
    #[test]
    fn a_held_write_s_runs_are_one_request() {
        let secret = b"thirty-two bytes and a few more, for HS256";
        let access = crate::access::Access::new(secret, "notes read\n").unwrap();
        let token = access.mint(r#"{"sub":"held"}"#).unwrap();
        let scope = access.scope(&token, 1).unwrap();
        let db = Arc::new(RwLock::new(Database::new()));
        assert!(admitted(1, &db, &scope).is_none(), "its first run");
        for _ in 0..10 {
            assert!(again(|| admitted(1, &db, &scope)).is_none(), "a run again");
        }
        let refused = admitted(1, &db, &scope).expect("its next request, held");
        assert_eq!(refused.status, 429);
        assert!(admitted(0, &db, &scope).is_none(), "--request-rate 0");
    }
}
