//! Change feed: "these ids changed", not a transaction log.
//!
//! A replica can be fed in two ways. The classic route is a transaction
//! log: every write becomes an event and the subscriber applies the events
//! in order. fennecdb does *not* do that, because the price comes from two sides:
//!
//! - **Memory.** An event has to carry the document; a ring buffer holding
//!   768-dimensional embeddings reaches hundreds of MB within a few thousand writes.
//! - **Repetition.** Ten writes in a row to the same document are ten events;
//!   none of the intermediate states the subscriber sees is of any use.
//!
//! The feed here is **state based**: the ring holds only `(seq, cid, id)`
//! -- 24 bytes per entry, independent of document size. When the subscriber
//! reads, the row's *current* state is decoded from the store. Three results:
//!
//! 1. N writes to the same document collapse into one event.
//! 2. Re-applying is harmless (idempotent): "the current state of this id
//!    is this" yields the same result when applied twice.
//! 3. The shape filter works correctly for free. If a row is updated out of
//!    the filter, the subscriber gets "not there" -- no separate "left the
//!    set" event has to be produced. With a transaction log that would also
//!    require carrying the old image.
//!
//! The price: a subscriber cannot see intermediate states, and `seq` is a
//! *clock*, not a record. The right trade for a replica; anyone who wants an
//! audit log should not use this.

use crate::value::DocId;
use std::collections::VecDeque;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

/// Default entry count of the ring buffer (~96 KB).
///
/// This number directly decides **how far behind a subscriber may fall**:
/// when the buffer overflows the oldest marks are dropped and cursors
/// behind that point can no longer be caught up incrementally -- they get
/// reseeded. At 100 writes per second, 4096 entries is a window of about
/// 40 seconds.
pub const DEFAULT_CAPACITY: usize = 4096;

/// How many times its capacity the ring grows to at most while a seed
/// holds it ([`ChangeLog::hold`]), [`HELD_MOST`] marks at most: 262 144
/// marks, 6 MB, at the default capacity -- over five seconds of one
/// client's single writes back to back, 46 000 a second, where a seed of a
/// million rows took 0.6 to 0.9. Past it the oldest go as before, and the
/// seed is read again under the lock rather than sent.
#[cfg(not(target_arch = "wasm32"))]
pub const HELD_TIMES: usize = 64;

/// The marks a held ring grows to at most, 24 MB, whatever its capacity
/// short of it: a ring made larger is not made 64 times larger with it.
#[cfg(not(target_arch = "wasm32"))]
pub const HELD_MOST: usize = 1 << 20;

/// Document id of the schema-change mark. Real ids start at 1, so 0 never
/// collides.
pub const SCHEMA_MARK: DocId = 0;

/// The mark of a single write. It does *not* carry the document (module header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub seq: u64,
    pub cid: u32,
    pub id: DocId,
}

/// Result of reading everything after a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Since {
    /// The cursor fell behind the ring (or is ahead of it): incremental
    /// catch-up is impossible, the subscriber has to be reseeded.
    Reseed,
    /// These ids changed. Sorted and deduplicated; resolving the values from
    /// the store is left to the caller.
    Ids(Vec<DocId>),
}

pub struct ChangeLog {
    ring: VecDeque<Mark>,
    cap: usize,
    seq: u64,
    /// The **smallest** cursor that can still be caught up incrementally. If
    /// the oldest mark in the ring has `seq` H, the cursor that must see H is
    /// H-1, so the horizon is "oldest mark - 1". On an empty ring the horizon
    /// is `seq` itself: only a cursor saying "I saw everything" gets nothing.
    horizon: u64,
    /// The changes seeds hold the ring back to ([`Self::hold`]).
    #[cfg(not(target_arch = "wasm32"))]
    holds: Arc<Holds>,
}

/// The changes the seeds of streams hold the ring back to, each with how
/// many hold it, and the oldest of them, which every write asks.
#[cfg(not(target_arch = "wasm32"))]
struct Holds {
    held: Mutex<std::collections::BTreeMap<u64, usize>>,
    /// `u64::MAX` where none is held.
    oldest: AtomicU64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Holds {
    /// `held`'s oldest change made the one every write asks.
    fn settle(&self, held: &std::collections::BTreeMap<u64, usize>) {
        let oldest = held.keys().next().copied().unwrap_or(u64::MAX);
        self.oldest.store(oldest, Ordering::Release);
    }
}

/// The ring held back to the change it names ([`ChangeLog::hold`]) for as
/// long as this lives.
#[cfg(not(target_arch = "wasm32"))]
pub struct Hold {
    holds: Arc<Holds>,
    seq: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Hold {
    /// The change it holds the ring back to.
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Hold {
    fn drop(&mut self) {
        let mut held = self.holds.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = held.get_mut(&self.seq) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.seq);
            }
        }
        self.holds.settle(&held);
    }
}

impl Default for ChangeLog {
    fn default() -> Self {
        ChangeLog::new(DEFAULT_CAPACITY)
    }
}

impl ChangeLog {
    pub fn new(cap: usize) -> ChangeLog {
        ChangeLog {
            ring: VecDeque::new(),
            cap: cap.max(1),
            seq: 0,
            horizon: 0,
            #[cfg(not(target_arch = "wasm32"))]
            holds: Arc::new(Holds {
                held: Mutex::default(),
                oldest: AtomicU64::new(u64::MAX),
            }),
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn horizon(&self) -> u64 {
        self.horizon
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Resizes the ring buffer. When it shrinks the oldest entries are
    /// dropped and the horizon rises.
    pub fn set_capacity(&mut self, cap: usize) {
        self.cap = cap.max(1);
        while self.ring.len() > self.cap {
            self.evict();
        }
    }

    /// Sets the counter to the given value and empties the ring.
    ///
    /// Called at startup and after `load`: a database restored from a file
    /// has no history, only its *current* state. The horizon is therefore
    /// set equal to `seq` -- everyone but a cursor sitting exactly here gets
    /// reseeded. Saying "start over" out loud is the right thing to do,
    /// rather than silently sending an incomplete set of events.
    pub fn reset(&mut self, seq: u64) {
        self.ring.clear();
        self.seq = seq;
        self.horizon = seq;
    }

    /// Records a write and returns the new `seq`.
    pub fn record(&mut self, cid: u32, id: DocId) -> u64 {
        self.seq += 1;
        #[cfg(target_arch = "wasm32")]
        if self.ring.len() == self.cap {
            self.evict();
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.make_room();
        self.ring.push_back(Mark {
            seq: self.seq,
            cid,
            id,
        });
        self.seq
    }

    /// Holds the ring back to the change it stands at, past its capacity,
    /// for as long as the hold lives: a stream's seed read beside the
    /// writers (`Database::pin`) is at that change, and the stream goes on
    /// from it once the seed is sent. Dropped by its oldest marks as
    /// writes came, the ring of a stream whose writers outran its seed had
    /// it seeded again, and again, for as long as they did. Taken under
    /// the read lock, where no write is recording; held, the ring grows by
    /// a mark a write, [`HELD_TIMES`] its capacity at most ([`HELD_MOST`]),
    /// and let go the next write drops what is past the capacity.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn hold(&self) -> Hold {
        let mut held = self.holds.held.lock().unwrap_or_else(|e| e.into_inner());
        *held.entry(self.seq).or_default() += 1;
        self.holds.settle(&held);
        Hold {
            holds: Arc::clone(&self.holds),
            seq: self.seq,
        }
    }

    /// Room for a mark: the oldest dropped while the ring is full, but for
    /// a mark after the oldest change a hold names -- dropped, it would
    /// leave the horizon past that change -- while the ring is short of
    /// [`HELD_TIMES`] its capacity ([`HELD_MOST`]). A write into a ring
    /// with room asks nothing more.
    #[cfg(not(target_arch = "wasm32"))]
    fn make_room(&mut self) {
        if self.ring.len() < self.cap {
            return;
        }
        let oldest = self.holds.oldest.load(Ordering::Acquire);
        let most = (self.cap.saturating_mul(HELD_TIMES).min(HELD_MOST)).max(self.cap);
        let was = self.ring.len();
        while self.ring.len() >= self.cap {
            let held = self.ring.front().is_some_and(|m| m.seq > oldest);
            if held && self.ring.len() < most {
                break;
            }
            self.evict();
        }
        // Let go, a ring that grew gives its room back: kept, the 6 MB of
        // a held default ring stayed with the database. Still held at its
        // bound, it keeps it.
        if was > self.cap && self.ring.len() < self.cap {
            self.ring.shrink_to(self.cap);
        }
    }

    fn evict(&mut self) {
        if let Some(m) = self.ring.pop_front() {
            // The dropped mark is `m.seq`; the cursor that had to see it was
            // `m.seq - 1`. The smallest servable cursor is now `m.seq`.
            self.horizon = self.horizon.max(m.seq);
        }
    }

    /// Ids that changed **in this collection** after `since`.
    ///
    /// Returns [`Since::Reseed`] if `since` is behind the horizon or ahead of
    /// the counter (server restarted, cursor gone stale).
    pub fn since(&self, since: u64, cid: u32) -> Since {
        if since > self.seq || since < self.horizon {
            return Since::Reseed;
        }
        // The ring is sorted ascending by `seq`: the first relevant entry is
        // found by binary search, the whole buffer is never scanned.
        let start = self.ring.partition_point(|m| m.seq <= since);
        let mut out: Vec<DocId> = self
            .ring
            .iter()
            .skip(start)
            .filter(|m| m.cid == cid)
            .map(|m| m.id)
            .collect();
        out.sort_unstable();
        out.dedup();
        Since::Ids(out)
    }

    /// Collection ids that changed after `since` (deduplicated).
    /// For live queries in the browser: this much is enough to pick which
    /// query has to be re-run.
    pub fn changed_collections(&self, since: u64) -> Option<Vec<u32>> {
        if since > self.seq || since < self.horizon {
            return None;
        }
        let start = self.ring.partition_point(|m| m.seq <= since);
        // Sorted as `u64`s, the sort the ids take: one of `u32`s as well was
        // 2.9 KB of the browser module.
        let mut out: Vec<u64> = self.ring.iter().skip(start).map(|m| m.cid as u64).collect();
        out.sort_unstable();
        out.dedup();
        Some(out.into_iter().map(|cid| cid as u32).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seed's hold keeps every mark after its change past the capacity
    /// until it is let go; then the next write drops what is past it.
    #[test]
    fn a_hold_keeps_the_ring_back_to_its_change() {
        let mut log = ChangeLog::new(2);
        log.record(1, 1);
        let hold = log.hold();
        assert_eq!(hold.seq(), 1);
        for id in 2..=6 {
            log.record(1, id);
        }
        assert_eq!(log.since(1, 1), Since::Ids(vec![2, 3, 4, 5, 6]));
        assert_eq!(log.horizon(), 1);
        drop(hold);
        log.record(1, 7);
        assert_eq!(log.since(1, 1), Since::Reseed);
        assert_eq!(log.since(5, 1), Since::Ids(vec![6, 7]));
        assert_eq!(log.ring.len(), 2);
    }

    /// Held, the ring grows to `HELD_TIMES` its capacity and no further:
    /// past it the oldest go, the hold's change with them. Of two holds,
    /// the older keeps the ring.
    #[test]
    fn a_hold_keeps_no_more_than_its_bound() {
        let mut log = ChangeLog::new(2);
        let most = 2 * HELD_TIMES as u64;
        let hold = log.hold();
        for id in 1..=most {
            log.record(1, id);
        }
        assert_eq!(log.since(hold.seq(), 1), Since::Ids((1..=most).collect()));
        log.record(1, most + 1);
        assert_eq!(log.since(hold.seq(), 1), Since::Reseed);
        assert_eq!(log.ring.len() as u64, most);

        let mut log = ChangeLog::new(2);
        let first = log.hold();
        log.record(1, 1);
        let second = log.hold();
        log.record(1, 2);
        log.record(1, 3);
        drop(second);
        log.record(1, 4);
        assert_eq!(log.since(first.seq(), 1), Since::Ids(vec![1, 2, 3, 4]));
    }

    #[test]
    fn empty_log_serves_only_its_own_cursor() {
        let log = ChangeLog::new(8);
        assert_eq!(log.since(0, 1), Since::Ids(vec![]));
        assert_eq!(log.since(1, 1), Since::Reseed);
    }

    #[test]
    fn records_and_dedups() {
        let mut log = ChangeLog::new(8);
        log.record(1, 10);
        log.record(1, 11);
        log.record(1, 10);
        log.record(2, 99);
        assert_eq!(log.seq(), 4);
        assert_eq!(log.since(0, 1), Since::Ids(vec![10, 11]));
        assert_eq!(log.since(0, 2), Since::Ids(vec![99]));
        // A cursor that has seen the first two writes only gets the rest.
        assert_eq!(log.since(2, 1), Since::Ids(vec![10]));
        assert_eq!(log.since(4, 1), Since::Ids(vec![]));
    }

    #[test]
    fn overflow_raises_horizon_and_forces_reseed() {
        let mut log = ChangeLog::new(2);
        log.record(1, 1); // seq 1
        log.record(1, 2); // seq 2
        assert_eq!(log.horizon(), 0);
        log.record(1, 3); // seq 3 -> seq 1 is dropped
        assert_eq!(log.horizon(), 1);
        // Cursor 0 needed to see seq 1: no longer possible.
        assert_eq!(log.since(0, 1), Since::Reseed);
        // Cursor 1 can still be served.
        assert_eq!(log.since(1, 1), Since::Ids(vec![2, 3]));
    }

    #[test]
    fn reset_clears_history() {
        let mut log = ChangeLog::new(8);
        log.record(1, 1);
        log.reset(42);
        assert_eq!(log.seq(), 42);
        assert_eq!(log.horizon(), 42);
        assert_eq!(log.since(42, 1), Since::Ids(vec![]));
        assert_eq!(log.since(41, 1), Since::Reseed);
    }

    #[test]
    fn cursor_from_the_future_reseeds() {
        // The server may have restarted and left the counter behind.
        let mut log = ChangeLog::new(8);
        log.record(1, 1);
        assert_eq!(log.since(99, 1), Since::Reseed);
    }

    #[test]
    fn shrinking_capacity_evicts() {
        let mut log = ChangeLog::new(8);
        for i in 1..=8 {
            log.record(1, i);
        }
        log.set_capacity(3);
        assert_eq!(log.horizon(), 5);
        assert_eq!(log.since(5, 1), Since::Ids(vec![6, 7, 8]));
        assert_eq!(log.since(4, 1), Since::Reseed);
    }

    #[test]
    fn changed_collections_is_deduped() {
        let mut log = ChangeLog::new(8);
        log.record(1, 1);
        log.record(2, 1);
        log.record(1, 2);
        assert_eq!(log.changed_collections(0), Some(vec![1, 2]));
        assert_eq!(log.changed_collections(2), Some(vec![1]));
    }
}
