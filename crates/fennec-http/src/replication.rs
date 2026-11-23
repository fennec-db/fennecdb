//! Log shipping: a primary streams the writes it appends to its file, and a
//! replica applies them to its own.
//!
//! **What is sent.** A primary's file is its log: an image, then each write
//! appended as a record. A replica is sent those records, whole and in
//! order, and applies each the way the write path made it
//! ([`Database::apply`]); its own file ends up holding the same records, so
//! it reopens where it stopped and asks for the rest. A replica too far
//! behind for the primary's buffer -- or on a history the primary's does
//! not continue ([`History`]) -- is sent an image instead.
//!
//! **Only what is on the primary's disk.** A record reaches the feed when it
//! is appended and leaves it once an fsync has covered it. A primary that
//! crashes therefore comes back holding every write any replica was sent,
//! and no replica ever has to be told to forget one. The price is latency:
//! under `--sync 250` a write reaches the replicas up to 250 ms later, under
//! `--sync always` right after its own fsync. `--sync off` makes nothing
//! durable before shutdown, so a server with replicas refuses it.
//!
//! **A durable write is answered once every stream has sent it.** Under
//! `--sync always` the answer waits, after the fsync, until each replica's
//! stream has written the record into its socket ([`Feed::wait_sent`],
//! [`SENT_WAIT`] at most): sent after the answer, a write the client was
//! told about was lost whenever the primary died between the two -- 3 runs
//! in 60 of Trellis's node killed under eight writers. Bytes a socket holds
//! reach the replica after the process that wrote them is killed; a machine
//! that dies takes them with it, so this is the process's guarantee, not
//! the machine's. A stream that misses the wait is not waited for again
//! until it has caught up, so a replica that stalls costs the writes one
//! wait, not one each.
//!
//! **Positions are change numbers.** Every write moves the change counter by
//! one, on the primary as on its replicas: a replica's position is its own
//! counter, a subscriber's cursor means the same change on either, and a
//! replica of a replica is fed exactly as its primary was.
//!
//! ## Wire format
//!
//! `GET /_replication?since=<seq>&history=<id, hex>` with
//! `Authorization: Bearer <replication token>` answers with a stream of
//! messages, `[tag: u8][length: u64 LE][payload]`:
//!
//! ```text
//! H  hello   [version: 1][image: u8][seq u64][durable u64][time_ms u64][history]
//! I  image   the database as of the hello's seq (when `image` is 1)
//! W  writes  [first seq u64][n u64][n x time_ms u64][n records]
//! K  alive   [seq u64][durable u64][time_ms u64]
//! E  end     why, in UTF-8; the stream closes after it
//! ```
//!
//! A write's time is when the primary appended it, which is what a restore
//! to a moment goes by (see [`crate::archive`]).
//!
//! `&image=1` asks for an image whatever the positions say: a replica sends
//! it after a record it could not apply.

use crate::crypto::ct_eq;
use crate::http::{Request, Response};
use fennec_core::prelude::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default size of a primary's feed. A replica that falls further behind is
/// sent an image. 64 MiB holds ~20 000 writes of a 768-dimension document,
/// about a minute of a sustained 300 writes/s.
pub const DEFAULT_BUFFER: usize = 64 << 20;

/// The feed is kept and dropped a chunk at a time: this much, or an eighth
/// of a smaller buffer, so that one still drops its oldest writes first.
const CHUNK: usize = 1 << 20;

/// The records one `W` message carries, at most; a larger record goes alone.
/// It is also what a replica applies under one hold of its write lock.
pub(crate) const MESSAGE: usize = 1 << 20;

/// The longest message a stream carries, an image or writes: an image is
/// the whole database, and a block's record goes alone however long it is
/// -- a block past 16 MB spills into its primary's file, and the feed
/// sends it whole ([`fennec_core::engine::landed_block`]). Past it is a
/// stream that lost its place, or no primary.
pub(crate) const LONGEST: u64 = 1 << 40;

/// The longest of the other messages -- a hello, a keep-alive, an end, a
/// node's word of a tenant's lineage or its leaving: a keep-alive is 24
/// bytes, and a hello's lineage an entry of 18 bytes at the most for each
/// promotion its database had, some 58 000 of them in this.
pub(crate) const SHORT: u64 = MESSAGE as u64;

/// What a message's body is given of memory before its bytes come: a
/// stream's writes in one message where no record goes alone --
/// [`MESSAGE`] of records and a time for each, fewer bytes than its record
/// -- so a steady stream's message is one allocation of its length, as it
/// always was. Past it the body grows as its bytes come.
const FIRST: usize = 2 * MESSAGE + 16;

/// A stream with nothing to send says so this often: the replica learns the
/// primary is there, and how far it has got. A stream behind says how far
/// its disk has got this often too, after the writes it sends. Public for a
/// router, which reads a promoted copy's last contact against it.
pub const ALIVE: Duration = Duration::from_secs(2);

/// A replica that hears nothing for this long takes the primary for gone and
/// connects again.
pub(crate) const SILENCE: Duration = Duration::from_secs(10);

/// How long a connection to the primary may take: its name's lookup, then
/// the connect to each address it stands for ([`crate::dial::connect`]).
pub(crate) const CONNECT: Duration = Duration::from_secs(5);

/// An image is sent once the primary's disk holds it; a primary whose disk
/// does not get there in this long is not sending it.
pub(crate) const DURABLE_WAIT: Duration = Duration::from_secs(30);

pub(crate) const VERSION: u8 = 1;

/// How long a durable write waits for the streams to send it. A stream
/// sends within microseconds of the fsync; one that has not in this long is
/// stalled -- its replica not reading, or its socket full -- and the write
/// is answered without it.
pub const SENT_WAIT: Duration = Duration::from_secs(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The id of a history forked from `history` -- a promotion, a restore:
/// `fresh`'s bottom half under the top half of the first history it took,
/// which names its lineage, the database it was made as; a history begun
/// from the root takes `fresh` whole, a lineage of its own. A client
/// orders positions by their term within a lineage alone: a tenant
/// deleted and made again under its name starts again at term 1, below
/// the old one's, and a client that held the old position refused home's
/// new one as a fenced primary's word on another branch, every read near
/// waiting its whole wait for good. Never an id the lineage holds.
pub fn fork_id(history: &fennec_core::history::History, fresh: u64) -> u64 {
    let Some(&(first, _)) = history.lineage.first() else {
        return fresh;
    };
    let mut low = (fresh & 0xffff_ffff).max(1);
    loop {
        let id = (first & !0xffff_ffff) | low;
        if history.lineage.iter().all(|&(held, _)| held != id) {
            return id;
        }
        low = (low % 0xffff_ffff) + 1;
    }
}

/// The lineage of a history: the top half of its first id, which every
/// fork keeps ([`fork_id`]) and a database made anew does not. `None` for
/// the root, which names nothing.
pub fn lineage_of(history: &fennec_core::history::History) -> Option<u32> {
    history.lineage.first().map(|&(id, _)| (id >> 32) as u32)
}

/// A lineage as a router and a node pass it: 8 hex digits.
pub fn lineage_text(lineage: u32) -> String {
    format!("{lineage:08x}")
}

/// A lineage passed as 8 hex digits.
pub fn parse_lineage(text: &str) -> Option<u32> {
    (text.len() == 8)
        .then(|| u32::from_str_radix(text, 16).ok())
        .flatten()
}

/// A history id nobody else holds: 64 bits from the operating system's
/// randomness (`RandomState` is seeded from it), never 0 -- the root's.
pub fn fresh_id() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.write_u32(std::process::id());
    h.finish().max(1)
}

// --------------------------------------------------------------------- feed

/// The writes a primary appended, kept for its replicas: the last
/// `capacity` bytes of them, and how far its disk holds them.
pub struct Feed {
    ring: Mutex<Ring>,
    cv: Condvar,
    /// The streams sending from it, and how far each has sent.
    senders: Mutex<Vec<Sender>>,
    sent_cv: Condvar,
    /// Told whenever the writes on disk move on, the feed starts over or
    /// closes: a tenant's, by the node's stream of every tenant
    /// ([`crate::node_stream`]), under the tenant's name. Unset everywhere
    /// else, where it costs a sync a load; set, two -- the streams' count is
    /// read before anything is told, and no stream connected tells nothing.
    told: OnceLock<(Arc<crate::node_stream::NodeStreams>, String)>,
    /// Durable writes that waited out their whole wait for a stream, which
    /// was then let lag: what a stream that holds writes up costs, counted
    /// so a test asks the count rather than a clock.
    waited_out: AtomicU64,
}

/// A stream as a durable write waits for it: the last write it put into its
/// socket, and whether it missed a wait and has not caught up since.
struct Sender {
    key: u64,
    sent: u64,
    lagging: bool,
    /// Set while the stream is held up elsewhere -- the node's stream of
    /// every tenant, busy with another tenant's image, an open or a full
    /// socket -- when no write waits for it: one stream for every tenant,
    /// it held each tenant's durable writes for its `SENT_WAIT` whenever it
    /// was busy with any one.
    held: Option<Arc<AtomicBool>>,
    /// A stream a promotion could take a write from: a file's replica, a
    /// tenant's pair standby or `--replicas` copy, never an archiver nor a
    /// read replica, which mark their requests (`archive=1`, `reader=1`) --
    /// what a hook's answer waits for ([`Feed::standby_sent`]). A tenant
    /// node's stream of every tenant (`node_stream`) is none.
    standby: bool,
}

impl Sender {
    fn waited_for(&self, upto: u64) -> bool {
        !self.lagging
            && self.sent < upto
            && !self
                .held
                .as_ref()
                .is_some_and(|h| h.load(Ordering::Relaxed))
    }
}

thread_local! {
    /// Set while a durability runs that no answer waits on: see
    /// [`unwaited`].
    static UNWAITED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `durable` -- a [`Durability`] -- without waiting for the streams to
/// send what it made durable: the syncer's and a closing tenant's, which no
/// client's answer waits on. The wait is what a durable write's answer is
/// held for; held in the syncer, a stream busy elsewhere kept every tenant
/// of a pass waiting a `SENT_WAIT` each, 5.06 s over five, the writes of a
/// pass unsynced meanwhile, and a close under its slot's lock.
pub fn unwaited<R>(durable: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            UNWAITED.with(|u| u.set(false));
        }
    }
    UNWAITED.with(|u| u.set(true));
    let _reset = Reset;
    durable()
}

struct Ring {
    chunks: VecDeque<Chunk>,
    bytes: usize,
    cap: usize,
    chunk: usize,
    /// The last write appended, and the last one on disk.
    seq: u64,
    durable: u64,
    /// Moves when what the ring holds stops leading to the database -- a
    /// replica that took an image: a stream begun before it ends.
    epoch: u64,
    /// The database is going: every stream ends rather than holding it open
    /// (a tenant being deleted or moved).
    closed: bool,
    /// The history the writes are on, as the database last wrote it: the
    /// node's stream sends it on when it moves (a promotion), before the
    /// writes after it. Taken from the history records the database
    /// appends ([`Tee`]), which no write's record carries.
    lineage: Vec<(u64, u64)>,
}

/// Consecutive records, the first write in them numbered `first`.
struct Chunk {
    first: u64,
    data: Vec<u8>,
    /// Where each record ends in `data`, the last write it holds -- a
    /// statement's writes are one record, a block's too -- and when it was
    /// appended.
    ends: Vec<usize>,
    lasts: Vec<u64>,
    times: Vec<u64>,
}

/// What [`Feed::changes_after`] finds.
pub(crate) enum Tail {
    /// Records, the first write in them numbered `first`; each record's
    /// last write and when it was appended.
    Records {
        first: u64,
        lasts: Vec<u64>,
        times: Vec<u64>,
        bytes: Vec<u8>,
    },
    Nothing,
    /// The writes after the cursor are no longer kept: the first that is.
    Behind(u64),
    /// The cursor is past the last write, which is: a database restored,
    /// or another one's cursor.
    Ahead(u64),
    Gone,
}

pub(crate) enum Next {
    /// The first write's number, the last's, the records' times, the
    /// records.
    Records(u64, u64, Vec<u64>, Vec<u8>),
    Nothing,
    /// The records after the cursor are no longer kept.
    Behind,
    /// The feed started over.
    Moved,
    /// The database this feed belongs to is going.
    Gone,
}

impl Feed {
    pub fn new(capacity: usize) -> Arc<Feed> {
        Arc::new(Feed {
            ring: Mutex::new(Ring {
                chunks: VecDeque::new(),
                bytes: 0,
                cap: capacity.max(1),
                chunk: CHUNK.min(capacity / 8).max(1),
                seq: 0,
                durable: 0,
                epoch: 0,
                closed: false,
                lineage: Vec::new(),
            }),
            cv: Condvar::new(),
            senders: Mutex::new(Vec::new()),
            sent_cv: Condvar::new(),
            told: OnceLock::new(),
            waited_out: AtomicU64::new(0),
        })
    }

    /// How many durable writes waited out their whole wait for a stream.
    pub fn waited_out(&self) -> u64 {
        self.waited_out.load(Ordering::Relaxed)
    }

    /// Has `streams` told of `name` whenever the writes on disk move on,
    /// the feed starts over or it closes. Once: a feed belongs to one
    /// tenant. Held strongly: a weak hold's upgrade was a compare-and-swap
    /// on one count every tenant's sync shared; the cycle it makes, feed to
    /// streams to a stream's feeds, is broken as the stream lets go of its
    /// feeds.
    pub(crate) fn tell_with(&self, streams: &Arc<crate::node_stream::NodeStreams>, name: &str) {
        let _ = self.told.set((Arc::clone(streams), name.to_string()));
    }

    fn tell(&self) {
        if let Some((streams, name)) = self.told.get() {
            streams.touch(name);
        }
    }

    /// A stream begins sending after write `at`. A stream already sending
    /// from it starts there again: the node's stream is put on a tenant's
    /// feed as it opens and again as it judges where it stands, and a
    /// second entry of one key, never moved on, would have kept a durable
    /// write waiting and the tenant from closing.
    #[cfg(test)]
    pub(crate) fn sending(&self, key: u64, at: u64) {
        self.sending_as(key, at, None, false);
    }

    /// [`Self::sending`], for a stream a promotion could take a write from
    /// (`standby`) or none -- an archiver's.
    pub(crate) fn sending_standby(&self, key: u64, at: u64, standby: bool) {
        self.sending_as(key, at, None, false);
        if let Some(x) = lock(&self.senders).iter_mut().find(|x| x.key == key) {
            x.standby = standby;
        }
    }

    /// [`Self::sending`], for a stream that `held` says is held up
    /// elsewhere, and `behind` where it starts behind -- to send an image
    /// first, or as it connects with every tenant to look at: no write
    /// waits for it until it has caught up.
    pub(crate) fn sending_as(
        &self,
        key: u64,
        at: u64,
        held: Option<&Arc<AtomicBool>>,
        behind: bool,
    ) {
        let mut s = lock(&self.senders);
        match s.iter_mut().find(|x| x.key == key) {
            Some(x) => {
                x.sent = at;
                x.lagging = behind;
            }
            None => s.push(Sender {
                key,
                sent: at,
                lagging: behind,
                held: held.cloned(),
                standby: false,
            }),
        }
    }

    /// A stream sending from it is held up elsewhere now: a write waiting
    /// for it looks again, and waits no more.
    pub(crate) fn wake_waits(&self) {
        let _held = lock(&self.senders);
        self.sent_cv.notify_all();
    }

    /// Whether a stream sending from it has not yet sent a write its disk
    /// holds: the node's stream, which a tenant waits for before it closes
    /// -- closed, its feed goes with it, and what the stream had not sent
    /// would have come again as an image. A replica's stream holds its
    /// tenant open anyway. Lagging or not: a stream that stopped reading is
    /// cut at its socket's timeout, and is no sender after that.
    pub(crate) fn unsent(&self) -> bool {
        let durable = lock(&self.ring).durable;
        lock(&self.senders).iter().any(|x| x.sent < durable)
    }

    /// The history the writes are on, as the database last wrote it.
    pub(crate) fn lineage(&self) -> Vec<(u64, u64)> {
        lock(&self.ring).lineage.clone()
    }

    /// Takes the history the database is on: as it opens or takes an
    /// image, and from each history record it appends.
    pub fn set_lineage(&self, lineage: &[(u64, u64)]) {
        let mut r = lock(&self.ring);
        if r.lineage != lineage {
            r.lineage = lineage.to_vec();
        }
    }

    /// The stream `key` has written everything up to `sent` into its
    /// socket; `caught_up` when that is every durable write there is, which
    /// has a stream that missed a wait waited for again.
    pub(crate) fn sent(&self, key: u64, sent: u64, caught_up: bool) {
        let mut s = lock(&self.senders);
        if let Some(x) = s.iter_mut().find(|x| x.key == key) {
            x.sent = x.sent.max(sent);
            x.lagging &= !caught_up;
        }
        drop(s);
        self.sent_cv.notify_all();
    }

    /// The stream `key` has ended.
    pub(crate) fn stopped(&self, key: u64) {
        lock(&self.senders).retain(|x| x.key != key);
        self.sent_cv.notify_all();
    }

    /// Waits until every stream that is keeping up has sent write `upto`,
    /// `timeout` at most; a stream that has not by then is lagging.
    pub fn wait_sent(&self, upto: u64, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut s = lock(&self.senders);
        loop {
            if !s.iter().any(|x| x.waited_for(upto)) {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                for x in s.iter_mut().filter(|x| x.waited_for(upto)) {
                    x.lagging = true;
                }
                self.waited_out.fetch_add(1, Ordering::Relaxed);
                return;
            }
            s = self
                .sent_cv
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Whether a stream a promotion could take a write from has sent write
    /// `upto`, waiting `timeout` at most -- a lagging one waited for too,
    /// which [`Self::wait_sent`] passes over -- or none is there to: a
    /// hook's 2xx, which its sender never sends again, waits for a standby
    /// to hold the event (`docs/design/named-statements.md`, *What a sender
    /// is told*), where a durable write's answer takes the streams keeping
    /// up. `false` past `timeout`: the answer is a 503 its sender retries,
    /// answered from the event's key once a standby holds it.
    pub fn standby_sent(&self, upto: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = lock(&self.senders);
        loop {
            let mut standbys = s.iter().filter(|x| x.standby).peekable();
            if standbys.peek().is_none() {
                return true;
            }
            if standbys.any(|x| x.sent >= upto) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self
                .sent_cv
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Starts over at `seq`, every write up to it on disk and none kept:
    /// the database was opened there, or replaced by an image.
    pub fn start(&self, seq: u64) {
        let mut r = lock(&self.ring);
        r.chunks.clear();
        r.bytes = 0;
        r.seq = seq;
        r.durable = seq;
        r.epoch += 1;
        drop(r);
        self.cv.notify_all();
        self.tell();
    }

    /// Ends every stream reading from this feed. A tenant being deleted or
    /// moved is held open by its replicas otherwise, and the delete waits
    /// for them until it gives up.
    pub fn close(&self) {
        lock(&self.ring).closed = true;
        self.cv.notify_all();
        self.tell();
    }

    /// Takes streams again after a [`Self::close`] whose reason fell through.
    pub fn reopen(&self) {
        lock(&self.ring).closed = false;
    }

    /// The last write appended.
    pub fn seq(&self) -> u64 {
        lock(&self.ring).seq
    }

    /// The last write on disk: the furthest a replica can be.
    pub fn durable(&self) -> u64 {
        lock(&self.ring).durable
    }

    pub(crate) fn epoch(&self) -> u64 {
        lock(&self.ring).epoch
    }

    /// Whether it holds no more streams: its database is going.
    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.ring).closed
    }

    /// A record numbered `seq`, as its last write: it holds as many as
    /// [`fennec_core::engine::writes_in`] says, a statement's or a block's.
    fn push(&self, seq: u64, bytes: &[u8]) {
        let writes = fennec_core::engine::writes_in(bytes).unwrap_or(1).max(1);
        let first = (seq + 1).saturating_sub(writes);
        let mut r = lock(&self.ring);
        if first != r.seq + 1 {
            // Not the write after the last one: what the ring holds no
            // longer leads here. Never seen, since every write passes
            // through; kept so a mistake costs images, not wrong replicas.
            r.chunks.clear();
            r.bytes = 0;
            r.epoch += 1;
            r.durable = r.durable.min(first.saturating_sub(1));
        }
        let room = match r.chunks.back() {
            Some(c) => c.data.len() + bytes.len() <= r.chunk,
            None => false,
        };
        if !room {
            r.chunks.push_back(Chunk {
                first,
                data: Vec::new(),
                ends: Vec::new(),
                lasts: Vec::new(),
                times: Vec::new(),
            });
        }
        let c = r.chunks.back_mut().unwrap();
        c.data.extend_from_slice(bytes);
        c.ends.push(c.data.len());
        c.lasts.push(seq);
        c.times.push(now_ms());
        r.bytes += bytes.len();
        r.seq = seq;
        while r.bytes > r.cap && r.chunks.len() > 1 {
            let old = r.chunks.pop_front().unwrap();
            r.bytes -= old.data.len();
        }
    }

    /// The records a follower has just applied from its primary, from
    /// write `first` on, take the times the primary sent with them, the
    /// message's `k`th record `times[k]`. Each that landed entered this feed
    /// through the sink ([`Feed::push`]), one entry a record, stamped with
    /// this server's clock as it was applied: they are the ring's newest.
    /// Where the apply said how many landed (`landed`, every record of the
    /// message) they are paired from the last back, which a ring letting go
    /// of the first of them as later ones came in -- a buffer smaller than
    /// one message -- leaves right. An apply refused part way landed a
    /// prefix and says nothing: paired from the first on, where the ring
    /// still holds the first; where it does not, they keep this clock's
    /// time until the image the follower now connects again for starts the
    /// feed over. Under the database's write lock, before the follower's
    /// flush: no record a follower applied is durable, so sendable, before
    /// its time is its primary's. After the apply, never in `push`: a
    /// primary's write runs the code it ran before.
    pub(crate) fn sent_times(&self, first: u64, landed: Option<usize>, times: &[u64]) {
        let mut g = lock(&self.ring);
        let r = &mut *g;
        // The ring's records holding writes from `first` on, and where the
        // earliest of them starts.
        let (mut held, mut start) = (0, None);
        for c in r.chunks.iter().rev() {
            let k = c.lasts.partition_point(|&l| l < first);
            held += c.lasts.len() - k;
            start = Some(if k > 0 { c.lasts[k - 1] + 1 } else { c.first });
            if k > 0 {
                break;
            }
        }
        let top = match landed {
            Some(n) => n,
            None if start == Some(first) => held,
            None => return,
        };
        if held > top || top > times.len() {
            return;
        }
        let durable = r.durable;
        let mut j = 0;
        'chunks: for c in r.chunks.iter_mut().rev() {
            for k in (0..c.lasts.len()).rev() {
                if j == held {
                    break 'chunks;
                }
                let last = c.lasts[k];
                debug_assert!(last > durable, "write {last} was sendable before its time");
                if last > durable {
                    c.times[k] = times[top - 1 - j];
                }
                j += 1;
            }
        }
    }

    /// A record appended and synced, as a [`Tee`] makes one: for tests.
    #[cfg(test)]
    pub(crate) fn push_durable(&self, seq: u64, bytes: &[u8]) {
        self.push(seq, bytes);
        self.mark_durable(seq);
    }

    fn mark_durable(&self, upto: u64) {
        let mut r = lock(&self.ring);
        let upto = upto.min(r.seq);
        if upto > r.durable {
            r.durable = upto;
            drop(r);
            self.cv.notify_all();
            self.tell();
        }
    }

    /// Whether every write after `cursor` is still kept.
    pub(crate) fn serves(&self, cursor: u64) -> bool {
        let r = lock(&self.ring);
        cursor == r.seq || cursor < r.seq && r.chunks.front().is_some_and(|c| c.first <= cursor + 1)
    }

    /// The durable records after `cursor`, up to about `max` bytes.
    pub(crate) fn next(&self, cursor: u64, epoch: u64, max: usize) -> Next {
        let r = lock(&self.ring);
        if r.closed {
            return Next::Gone;
        }
        if r.epoch != epoch {
            return Next::Moved;
        }
        if cursor >= r.durable {
            return Next::Nothing;
        }
        let first = cursor + 1;
        let Some(at) = r
            .chunks
            .iter()
            .position(|c| first >= c.first && c.lasts.last().is_some_and(|&l| first <= l))
        else {
            return Next::Behind;
        };
        // The record that starts at `first`. A cursor inside one -- a
        // block's -- is no place a replica stands, since it applies a record
        // whole; it takes an image.
        let c = &r.chunks[at];
        let k0 = c.lasts.partition_point(|&l| l < first);
        let starts = if k0 == 0 {
            c.first
        } else {
            c.lasts[k0 - 1] + 1
        };
        if starts != first {
            return Next::Behind;
        }
        let mut out = Vec::new();
        let mut times = Vec::new();
        let mut last = cursor;
        'chunks: for (i, c) in r.chunks.iter().enumerate().skip(at) {
            let mut k = if i == at { k0 } else { 0 };
            while k < c.ends.len() && c.lasts[k] <= r.durable {
                let start = if k == 0 { 0 } else { c.ends[k - 1] };
                let end = c.ends[k];
                if !out.is_empty() && out.len() + (end - start) > max {
                    break 'chunks;
                }
                out.extend_from_slice(&c.data[start..end]);
                times.push(c.times[k]);
                last = c.lasts[k];
                k += 1;
            }
            if k < c.ends.len() {
                break;
            }
        }
        if out.is_empty() {
            return Next::Nothing;
        }
        Next::Records(first, last, times, out)
    }

    /// The durable records holding the writes after `cursor`, up to about
    /// `max` bytes, for the change stream (`/_changes`): from the record
    /// holding the write after it, which a cursor inside a block's record
    /// starts before -- the reader passes over what it has had -- where a
    /// replica is sent an image. `Behind` names the first write kept.
    pub(crate) fn changes_after(&self, cursor: u64, max: usize) -> Tail {
        let r = lock(&self.ring);
        if r.closed {
            return Tail::Gone;
        }
        if cursor > r.seq {
            return Tail::Ahead(r.seq);
        }
        if cursor >= r.durable {
            return Tail::Nothing;
        }
        let first = cursor + 1;
        let oldest = r.chunks.front().map_or(r.seq + 1, |c| c.first);
        if first < oldest {
            return Tail::Behind(oldest);
        }
        let mut out = Tail::Records {
            first: 0,
            lasts: Vec::new(),
            times: Vec::new(),
            bytes: Vec::new(),
        };
        let Tail::Records {
            first: from,
            lasts,
            times,
            bytes,
        } = &mut out
        else {
            unreachable!()
        };
        'chunks: for c in &r.chunks {
            for k in 0..c.ends.len() {
                if c.lasts[k] < first {
                    continue;
                }
                if c.lasts[k] > r.durable || !bytes.is_empty() && bytes.len() >= max {
                    break 'chunks;
                }
                if bytes.is_empty() {
                    *from = if k == 0 { c.first } else { c.lasts[k - 1] + 1 };
                }
                let start = if k == 0 { 0 } else { c.ends[k - 1] };
                bytes.extend_from_slice(&c.data[start..c.ends[k]]);
                lasts.push(c.lasts[k]);
                times.push(c.times[k]);
            }
        }
        match bytes.is_empty() {
            true => Tail::Nothing,
            false => out,
        }
    }

    /// Wakes every stream waiting here to look again: the one shutdown
    /// ending them ([`crate::conn::end_all`]).
    fn nudge(&self) {
        let _r = lock(&self.ring);
        self.cv.notify_all();
    }

    /// Waits until a write after `cursor` is on disk, the feed starts over,
    /// or `timeout` passes.
    pub(crate) fn wait(&self, cursor: u64, epoch: u64, timeout: Duration) {
        self.wait_unless(cursor, epoch, timeout, || false)
    }

    /// [`Self::wait`], unless `ending()`, asked under the lock a nudge
    /// takes ([`Self::nudge`]): a stream the one shutdown ends never
    /// sleeps through the nudge that told it so.
    fn wait_unless(&self, cursor: u64, epoch: u64, timeout: Duration, ending: fn() -> bool) {
        let r = lock(&self.ring);
        if r.durable > cursor || r.epoch != epoch || ending() {
            return;
        }
        let _ = self
            .cv
            .wait_timeout(r, timeout)
            .unwrap_or_else(|e| e.into_inner());
    }
}

/// The sink a server with replicas writes through: its file, and the feed
/// they are sent from. A write enters the feed as it is appended and
/// becomes sendable once the file's fsync has covered it.
pub struct Tee {
    file: Box<dyn Sink>,
    feed: Arc<Feed>,
}

impl Sink for Tee {
    /// Passed on; a history record -- a fork, a replica following -- also
    /// moves the feed's lineage, which no write's record carries.
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.append(bytes)?;
        if let Some(h) = history_in(bytes) {
            self.feed.set_lineage(&h.lineage);
        }
        Ok(())
    }
    fn append_deferred(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.append_deferred(bytes)
    }
    fn sync_existing(&mut self) -> fennec_core::error::Result<()> {
        self.file.sync_existing()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn remapped(&mut self) -> Option<fennec_core::store::Base> {
        self.file.remapped()
    }
    /// Passed on: the records it writes are the file's, which a replica is
    /// sent once an fsync covers them, as before.
    #[cfg(not(target_arch = "wasm32"))]
    fn written_through(&mut self) -> fennec_core::error::Result<Option<fennec_core::store::Base>> {
        self.file.written_through()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn syncing(&self) -> bool {
        self.file.syncing()
    }
    /// Passed on: a write with no fsync makes nothing sendable.
    #[cfg(not(target_arch = "wasm32"))]
    fn write_behind(&mut self, all: bool) -> Option<fennec_core::engine::Durability> {
        self.file.write_behind(all)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn written_all(&self) -> bool {
        self.file.written_all()
    }
    /// Passed on, so a primary's `compact` writes its file beside the
    /// database as a server without replicas does.
    #[cfg(not(target_arch = "wasm32"))]
    fn side(&self) -> Option<std::path::PathBuf> {
        self.file.side()
    }
    /// The side file holds every write, and is fsynced before it takes the
    /// file's place.
    #[cfg(not(target_arch = "wasm32"))]
    fn adopt(&mut self, side: &std::path::Path) -> fennec_core::error::Result<()> {
        self.file.adopt(side)?;
        self.feed.mark_durable(self.feed.seq());
        Ok(())
    }
    fn record(&mut self, seq: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.record(seq, bytes)?;
        self.feed.push(seq, bytes);
        Ok(())
    }
    /// The parts into the file, its claim this file's; the record to the
    /// feed as it came, whose followers each make its claim their own.
    fn record_parts(
        &mut self,
        seq: u64,
        original: &[u8],
        parts: &[&[u8]],
    ) -> fennec_core::error::Result<()> {
        self.file.record_parts(seq, original, parts)?;
        self.feed.push(seq, original);
        Ok(())
    }
    fn durable(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicU64>> {
        self.file.durable()
    }
    fn set_durable(&mut self, at: u64) {
        self.file.set_durable(at)
    }
    /// The land into the file, which holds the spills; the block to the
    /// feed as the one block record it would have been, which a replica
    /// applies whole -- a land it could not, holding no spill.
    #[cfg(not(target_arch = "wasm32"))]
    fn land(
        &mut self,
        seq: u64,
        spilled: &[&[u8]],
        record: &[u8],
    ) -> fennec_core::error::Result<()> {
        let block = fennec_core::engine::landed_block(spilled, record)?;
        self.file.land(seq, spilled, record)?;
        self.feed.push(seq, &block);
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        // The image holds every write, and is fsynced before it replaces
        // the file.
        self.file.rewrite(bytes)?;
        self.feed.mark_durable(self.feed.seq());
        Ok(())
    }
    /// Passed on, so the file streams the image into place as it would
    /// without replicas. Left to the default, a checkpoint built the whole
    /// image in memory first: +40.4 MB of heap on a 40 MB file, against
    /// +1.0 MB through the file alone.
    fn rewrite_with(
        &mut self,
        image: &mut dyn FnMut(
            &mut dyn fennec_core::engine::ImageOut,
        ) -> fennec_core::error::Result<()>,
    ) -> fennec_core::error::Result<()> {
        self.file.rewrite_with(image)?;
        self.feed.mark_durable(self.feed.seq());
        Ok(())
    }
    fn sync(&mut self) -> fennec_core::error::Result<()> {
        let upto = self.feed.seq();
        self.file.sync()?;
        self.feed.mark_durable(upto);
        Ok(())
    }
    fn flush(&mut self) -> fennec_core::error::Result<Option<Durability>> {
        let upto = self.feed.seq();
        match self.file.flush()? {
            None => {
                self.feed.mark_durable(upto);
                Ok(None)
            }
            Some(durable) => {
                let feed = Arc::clone(&self.feed);
                Ok(Some(Box::new(move || {
                    // Spans of the request this runs for, when it is
                    // traced: the disk's part and the replicas' apart.
                    let syncing = crate::trace::span("fsync");
                    durable()?;
                    drop(syncing);
                    feed.mark_durable(upto);
                    if !UNWAITED.with(|u| u.get()) {
                        let sending = crate::trace::span("replication.wait_sent");
                        feed.wait_sent(upto, SENT_WAIT);
                        drop(sending);
                    }
                    Ok(())
                })))
            }
        }
    }
    /// Passed on, the feed told nothing: it comes after the last sync, which
    /// told it, and a durability still running tells it as before.
    #[cfg(not(target_arch = "wasm32"))]
    fn close_log(&mut self) {
        self.file.close_log()
    }
}

/// The history a history record holds, `[8][0][length][history]`; `None`
/// for any other record.
fn history_in(record: &[u8]) -> Option<History> {
    let body = fennec_core::engine::record_body(record, fennec_core::history::RECORD)?;
    History::decode(body).ok()
}

/// Opens a file for a server that has replicas, or is one, with its writes
/// going through a [`Tee`] to a feed of `buffer` bytes.
///
/// The file is fsynced as it is: a process that died leaves bytes it wrote
/// but never synced, and those are the first a replica could be sent. A
/// last record it cut short is cut off first, as `fs::open` does; it was
/// never synced, so no replica was sent it.
pub fn open(path: &str, buffer: usize) -> fennec_core::error::Result<(Database, Arc<Feed>)> {
    open_with(path, buffer, true)
}

/// [`open`], mapped like any other file or -- `mapped` false, `fennec-server
/// --no-mmap` -- read into memory.
pub fn open_with(
    path: &str,
    buffer: usize,
    mapped: bool,
) -> fennec_core::error::Result<(Database, Arc<Feed>)> {
    open_as(path, buffer, mapped, false, None)
}

/// [`open_with`] as a server opens its file: the vectors the open would
/// link into a graph are left for [`crate::link::beside`] to link
/// (`fennec_core::fs::open_serving`).
pub fn open_serving(
    path: &str,
    buffer: usize,
    mapped: bool,
) -> fennec_core::error::Result<(Database, Arc<Feed>)> {
    open_as(path, buffer, mapped, true, None)
}

/// [`open_serving`] holding `lock`, the file's `<file>.lock`, in the
/// database's sink (`fennec_abi::lock::Holding`): let go when the database
/// is, whoever holds it last -- a tenant's follower holds it beside the
/// registry.
pub fn open_serving_holding(
    path: &str,
    buffer: usize,
    mapped: bool,
    lock: fennec_abi::lock::FileLock,
) -> fennec_core::error::Result<(Database, Arc<Feed>)> {
    open_as(path, buffer, mapped, true, Some(lock))
}

fn open_as(
    path: &str,
    buffer: usize,
    mapped: bool,
    serving: bool,
    lock: Option<fennec_abi::lock::FileLock>,
) -> fennec_core::error::Result<(Database, Arc<Feed>)> {
    let feed = Feed::new(buffer);
    let made = Arc::clone(&feed);
    let wrap: Box<fennec_core::fs::Wrap<'_>> = Box::new(move |mut file| {
        file.sync_existing()?;
        let file = match lock {
            Some(lock) => fennec_abi::lock::holding(lock)(file)?,
            None => file,
        };
        Ok(Box::new(Tee { file, feed: made }) as Box<dyn Sink>)
    });
    let db = match serving {
        true => fennec_core::fs::open_serving(path, mapped, wrap)?,
        false => fennec_core::fs::open_with(path, mapped, wrap)?,
    };
    feed.set_lineage(&db.history().lineage);
    feed.start(db.change_seq());
    Ok((db, feed))
}

/// Puts a server's file's history where its flags say before anyone is
/// served -- one rule, where the database server and the router each kept a
/// copy. `--replica-of` has the file follow: a replica marks its file as one
/// before it has heard from its primary. A replica's file opens as a
/// primary only when `--promote` says so, and its history forks there, so
/// no replica of the old primary is streamed the new one's writes as if
/// they were its own; without either flag it is refused rather than opened
/// refusing every write. A primary gets a history of its own the first time
/// it has replicas -- the root history is every database's. `role` is what
/// a replica's file is to the server ("a replica's file", "a standby's
/// directory"), `takes` what a primary takes ("writes").
pub fn settle_history(
    db: &mut Database,
    path: &str,
    (role, takes): (&str, &str),
    replicating: bool,
    follows: bool,
    promote: bool,
) -> std::result::Result<(), String> {
    let following = db.history().following;
    let result = if follows {
        let lineage = db.history().lineage.clone();
        db.follow(lineage)
    } else if promote {
        if !following {
            crate::log!("--promote: {path} is not {role}; it opens as it is");
            return Ok(());
        }
        let id = fork_id(db.history(), fresh_id());
        let r = db.fork(id).and_then(|_| db.sync());
        if r.is_ok() {
            crate::log!(
                "promoted: {path} takes {takes} from change {} on, history {id:016x}",
                db.change_seq()
            );
        }
        r
    } else if following {
        return Err(format!(
            "{path} is {role}. Start it with --replica-of <primary> to go on following, \
             or with --promote to take {takes} -- its history forks there"
        ));
    } else if replicating && db.history().lineage.is_empty() {
        db.fork(fresh_id()).and_then(|_| db.sync())
    } else {
        Ok(())
    };
    result.map_err(|e| format!("could not settle {path}'s history: {e}"))
}

// ------------------------------------------------------------- the primary

/// A server's part in replication: the token replicas and operators
/// present, the feed it serves replicas from, and the primary it follows.
pub struct Replication {
    /// `None` for a server that keeps its feed for `/_changes` alone
    /// (`--cdc`): no replica is fed. Never an empty token, which an empty
    /// `Bearer` would match.
    token: Option<String>,
    feed: Option<Arc<Feed>>,
    follower: Option<Arc<Follower>>,
    streams: Mutex<Vec<Stream>>,
}

/// A replica being fed, as the primary's status lists it.
#[derive(Clone)]
struct Stream {
    key: u64,
    peer: String,
    sent: u64,
    image: bool,
}

impl Replication {
    pub fn new(
        token: Option<String>,
        feed: Option<Arc<Feed>>,
        follower: Option<Arc<Follower>>,
    ) -> Arc<Replication> {
        Arc::new(Replication {
            token: token.filter(|t| !t.is_empty()),
            feed,
            follower,
            streams: Mutex::new(Vec::new()),
        })
    }

    /// Ends the streams replicas are reading, and stops the follower: the
    /// database is going (a tenant deleted or moved).
    pub fn close_streams(&self) {
        if let Some(feed) = &self.feed {
            feed.close();
        }
        if let Some(f) = &self.follower {
            f.halt();
        }
    }

    /// Undoes [`Self::close_streams`] when the database stays after all -- a
    /// tenant delete that gave up with 409. Left closed, the tenant went on
    /// taking writes its replicas were never sent, and a replica stopped
    /// following for good.
    pub fn reopen_streams(&self, follower_name: String) {
        if let Some(feed) = &self.feed {
            feed.reopen();
        }
        if let Some(f) = &self.follower {
            let following =
                f.db.read()
                    .unwrap_or_else(|e| e.into_inner())
                    .history()
                    .following;
            if following {
                if let Err(e) = f.start(follower_name) {
                    crate::log!("could not follow again: {e}");
                }
            }
        }
    }

    /// The follower, on a server that follows one: what a promotion goes
    /// through (`/_admin/tenants/<t>/promote` on a replica node).
    pub fn follower(&self) -> Option<&Arc<Follower>> {
        self.follower.as_ref()
    }

    fn authorized(&self, req: &Request) -> bool {
        let given = req
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        let ok = self
            .token
            .as_ref()
            .is_some_and(|t| ct_eq(given.as_bytes(), t.as_bytes()));
        if ok {
            crate::audit::verified();
        }
        ok
    }

    /// The writes kept for replicas, which `/_changes` reads too.
    pub fn feed(&self) -> Option<&Arc<Feed>> {
        self.feed.as_ref()
    }
}

/// `/_replication/...` of a file's server or a router's directory,
/// answered here: the stream, the status and the promotion. `None` means
/// the request was a stream, which took the connection over and has
/// ended.
pub fn handle(
    out: &mut &crate::conn::Conn,
    db: &Arc<RwLock<Database>>,
    repl: &Replication,
    req: &Request,
) -> Option<Response> {
    answer(out, db, repl, req, promoting)
}

/// `/t/<tenant>/_replication/...` of a node of tenants: the stream and the
/// status, and no promotion. A tenant's copy is promoted by the node's
/// admin alone (`POST /_admin/tenants/<t>/promote`, `Tenants::promote_as`),
/// which a router's failover calls, held to the lease's fence and to the
/// lineage and history the router knows, its `.follows` forgotten, its
/// identity renewed, the node's stream told and the audit written. Here,
/// with the replication token every node of a region and every archiver
/// holds, the copy forked past all of it: the router counted it a standby
/// still, and without `--lease` it took writes beside its primary. The
/// path is 404 as any a node does not serve.
pub fn handle_tenant(
    out: &mut &crate::conn::Conn,
    db: &Arc<RwLock<Database>>,
    repl: &Replication,
    req: &Request,
) -> Option<Response> {
    answer(out, db, repl, req, |_, req| unknown(req))
}

/// The routes every replication answers, the token asked first; `rest`
/// answers any other.
fn answer(
    out: &mut &crate::conn::Conn,
    db: &Arc<RwLock<Database>>,
    repl: &Replication,
    req: &Request,
    rest: fn(&Replication, &Request) -> Response,
) -> Option<Response> {
    use crate::http::Method;
    if !repl.authorized(req) {
        return Some(
            Response::error(401, "invalid or missing replication token")
                .header("WWW-Authenticate", "Bearer"),
        );
    }
    match (req.method, req.segments().as_slice()) {
        (Method::Get, ["_replication"]) => {
            stream(out, db, repl, req);
            None
        }
        (Method::Get, ["_replication", "status"]) => Some(status(db, repl)),
        _ => Some(rest(repl, req)),
    }
}

/// `POST /_replication/promote`: a file's server's and a router's
/// directory's, never a tenant's ([`handle_tenant`]).
fn promoting(repl: &Replication, req: &Request) -> Response {
    use crate::http::Method;
    match (req.method, req.segments().as_slice()) {
        (Method::Post, ["_replication", "promote"]) => match &repl.follower {
            None => Response::error(409, "this server is not a replica"),
            Some(f) => match f.promote(fresh_id()) {
                Ok((seq, id)) => Response::json(
                    200,
                    format!("{{\"promoted\":true,\"seq\":{seq},\"history\":\"{id:016x}\"}}"),
                ),
                Err(e) => Response::error(500, &e.to_string()),
            },
        },
        _ => unknown(req),
    }
}

fn unknown(req: &Request) -> Response {
    Response::error(404, &format!("path `{}`", req.path))
}

impl Replication {
    /// A scrape's part of it: how many replicas a primary feeds, and how far
    /// a replica trails -- the same figures `/_replication/status` gives. A
    /// router's scrape takes its directory's.
    pub fn metrics(&self, out: &mut crate::metrics::Text, seq: u64) {
        if self.feed.is_some() {
            out.family("fennec_replicas", "gauge", "Replicas being fed now.");
            out.sample("fennec_replicas", &[], lock(&self.streams).len());
        }
        let Some(f) = &self.follower else {
            return;
        };
        let st = lock(&f.state).clone();
        out.family(
            "fennec_replica_connected",
            "gauge",
            "1 while this replica is connected to its primary.",
        );
        out.sample("fennec_replica_connected", &[], st.connected as u8);
        out.family(
            "fennec_replica_behind",
            "gauge",
            "Writes on the primary's disk this replica has not applied.",
        );
        out.sample(
            "fennec_replica_behind",
            &[],
            st.primary_durable.saturating_sub(seq),
        );
        if let Some(t) = st.contact {
            out.family(
                "fennec_replica_last_contact_seconds",
                "gauge",
                "Since the primary was last heard from, a keep-alive included.",
            );
            out.sample(
                "fennec_replica_last_contact_seconds",
                &[],
                t.elapsed().as_secs_f64(),
            );
        }
        out.family(
            "fennec_replica_images_total",
            "counter",
            "Whole images taken from the primary: at the start, and after falling behind its buffer.",
        );
        out.sample("fennec_replica_images_total", &[], st.images);
        out.family(
            "fennec_replica_reconnects_total",
            "counter",
            "Connections to the primary made again after one was lost.",
        );
        out.sample("fennec_replica_reconnects_total", &[], st.reconnects);
    }
}

fn status(db: &Arc<RwLock<Database>>, repl: &Replication) -> Response {
    let (seq, history, following) = {
        let g = db.read().unwrap_or_else(|e| e.into_inner());
        (g.change_seq(), g.history().current(), g.history().following)
    };
    let mut out = format!(
        "{{\"role\":\"{}\",\"seq\":{seq},\"history\":\"{history:016x}\"",
        if following { "replica" } else { "primary" }
    );
    if let Some(feed) = &repl.feed {
        out.push_str(&format!(",\"durable\":{},\"replicas\":[", feed.durable()));
        for (i, s) in lock(&repl.streams).iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"peer\":");
            fennec_core::json::escape_into(&mut out, &s.peer);
            out.push_str(&format!(",\"sent\":{},\"image\":{}}}", s.sent, s.image));
        }
        out.push(']');
    }
    if let Some(f) = &repl.follower {
        let st = lock(&f.state).clone();
        out.push_str(",\"primary\":");
        fennec_core::json::escape_into(&mut out, f.upstream.url());
        out.push_str(&format!(
            ",\"connected\":{},\"primary_durable\":{},\"behind\":{},\"last_contact_ms\":{},\"images\":{},\"reconnects\":{}",
            st.connected,
            st.primary_durable,
            st.primary_durable.saturating_sub(seq),
            st.contact.map_or(-1, |t| t.elapsed().as_millis() as i64),
            st.images,
            st.reconnects,
        ));
        if let Some(e) = &st.error {
            out.push_str(",\"error\":");
            fennec_core::json::escape_into(&mut out, e);
        }
    }
    out.push('}');
    Response::json(200, out)
}

pub(crate) fn message(out: &mut impl Write, tag: u8, parts: &[&[u8]]) -> io::Result<()> {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let mut head = [0u8; 9];
    head[0] = tag;
    head[1..].copy_from_slice(&(len as u64).to_le_bytes());
    out.write_all(&head)?;
    for p in parts {
        out.write_all(p)?;
    }
    Ok(())
}

pub(crate) fn u64s(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Records -- or an image, whose records follow its magic and counter --
/// as they are where the follower reads checked ones, and as a version
/// before checksums wrote them where it does not.
pub(crate) fn plain_unless(sums: bool, bytes: Vec<u8>) -> fennec_core::error::Result<Vec<u8>> {
    match sums {
        true => Ok(bytes),
        false if bytes.starts_with(&fennec_core::engine::MAGIC[..]) => {
            fennec_core::engine::plain_image(&bytes)
        }
        false => fennec_core::engine::plain_records(&bytes),
    }
}

/// Feeds a replica: a hello, an image when it needs one, then the writes as
/// they reach the disk. It takes the connection over and returns when the
/// replica goes away or has to start again.
fn stream(
    out: &mut &crate::conn::Conn,
    db: &Arc<RwLock<Database>>,
    repl: &Replication,
    req: &Request,
) {
    let Some(feed) = &repl.feed else {
        let _ = Response::error(409, "this server feeds no replicas").write(out, false, false);
        return;
    };
    let param = |k: &str| {
        req.query
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    let since: u64 = param("since").and_then(|v| v.parse().ok()).unwrap_or(0);
    let history = param("history")
        .and_then(|v| u64::from_str_radix(v, 16).ok())
        .unwrap_or(0);
    let force = param("image") == Some("1");
    // A follower that reads checked records says so. One that does not --
    // an older binary, which refuses the first -- is sent each record as a
    // version before checksums wrote it, the image too, so that a rolling
    // upgrade keeps it following ([`fennec_core::engine::plain_records`]).
    let sums = param("sums") == Some("1");
    // An archiver says so, and a read replica: no promotion takes a write
    // from either's stream, so no hook's answer waits for it
    // ([`Feed::standby_sent`]).
    let standby = param("archive") != Some("1") && param("reader") != Some("1");

    // Where the replica stands is judged against the database and the feed
    // at one moment, under the read lock: a write in between would leave
    // the image or the cursor one record off.
    let (seq, lineage, epoch, image) = {
        // An image of what has landed: a block's writes reach the replica
        // as the record it lands as, or never.
        let g = crate::held::read(db);
        let now = g.change_seq();
        let epoch = feed.epoch();
        let go_on = !force && g.history().continues(history, since, now) && feed.serves(since);
        if go_on {
            (since, g.history().clone(), epoch, None)
        } else {
            (now, g.history().clone(), epoch, Some(g.snapshot()))
        }
    };
    let image = match image.map(|i| plain_unless(sums, i)).transpose() {
        Ok(i) => i,
        Err(e) => {
            let _ = Response::error(500, &e.to_string()).write(out, false, false);
            return;
        }
    };
    // An image goes out once the primary's disk holds all of it.
    if image.is_some() {
        let deadline = Instant::now() + DURABLE_WAIT;
        while feed.durable() < seq {
            if Instant::now() > deadline {
                let _ = Response::error(503, "the primary's disk has not caught up")
                    .write(out, false, false);
                return;
            }
            feed.wait(seq - 1, epoch, Duration::from_millis(100));
        }
    }

    let _ = out.set_write_timeout(Some(SILENCE));
    let mut head = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\
         Cache-Control: no-cache, no-transform\r\nConnection: close\r\n\
         X-Accel-Buffering: no\r\n",
    );
    crate::request_id::with(|id| {
        if !id.is_empty() {
            head.push_str(crate::request_id::HEADER);
            head.push_str(": ");
            head.push_str(id);
            head.push_str("\r\n");
        }
    });
    crate::audit::push_verified(&mut head);
    head.push_str("\r\n");
    let key = fresh_id();
    lock(&repl.streams).push(Stream {
        key,
        peer: out.peer_addr().map(|a| a.to_string()).unwrap_or_default(),
        sent: seq,
        image: image.is_some(),
    });
    let _gone = Unlist(repl, key);

    let mut hello = vec![VERSION, image.is_some() as u8];
    hello.extend_from_slice(&u64s(&[seq, feed.durable(), now_ms()]));
    hello.extend_from_slice(&lineage.encode());
    let mut w = io::BufWriter::with_capacity(MESSAGE + 64, *out);
    let opened = w.write_all(head.as_bytes()).and_then(|_| {
        message(&mut w, b'H', &[&hello])?;
        if let Some(image) = &image {
            message(&mut w, b'I', &[image])?;
        }
        w.flush()
    });
    drop(image);
    if opened.is_err() {
        return;
    }

    // A durable write waits for this stream from here on: what it held
    // before, the image's, it already has.
    feed.sending_standby(key, seq, standby);
    let _stopped = Stopped(feed, key);
    let mut cursor = seq;
    // The one shutdown wakes the stream to end it, its end over TLS a
    // `close_notify`.
    out.on_ending({
        let feed = Arc::clone(feed);
        move || feed.nudge()
    });
    // When the replica was last told where this disk stands: the hello
    // told it.
    let mut told = Instant::now();
    loop {
        if crate::conn::ending() {
            return;
        }
        match feed.next(cursor, epoch, MESSAGE) {
            Next::Records(first, last, times, records) => {
                let n = times.len() as u64;
                let Ok(records) = plain_unless(sums, records) else {
                    let _ = message(&mut w, b'E', &[b"a record the feed holds does not read"])
                        .and_then(|_| w.flush());
                    return;
                };
                let sent = message(&mut w, b'W', &[&u64s(&[first, n]), &u64s(&times), &records])
                    .and_then(|_| w.flush());
                if sent.is_err() {
                    return;
                }
                cursor = last;
                feed.sent(key, cursor, false);
                if let Some(s) = lock(&repl.streams).iter_mut().find(|s| s.key == key) {
                    s.sent = cursor;
                }
                // Behind, a stream says where the disk stands every
                // `ALIVE` too, after the writes it sends: said only once
                // it had caught up, a replica up to the feed's buffer
                // behind -- its reads slower than the writes, its socket
                // full -- heard of no write it had not been sent, and its
                // `behind`, a node's `fennec_copies_behind`, a promotion's
                // answer and the router's line read 0. A replica of every
                // release reads a keep-alive between any two messages.
                // Asked once `feed.sent` let the writes waiting on this one
                // go, and sent in a flush of its own: in the `W`'s flush,
                // a `--sync always` write waited on the clock and, every
                // `ALIVE`, on the keep-alive's bytes too. A stream that has
                // sent every write on disk is not behind and says nothing.
                if told.elapsed() >= ALIVE && feed.durable() > cursor {
                    if message(&mut w, b'K', &[&alive(feed)])
                        .and_then(|_| w.flush())
                        .is_err()
                    {
                        return;
                    }
                    told = Instant::now();
                }
            }
            Next::Nothing => {
                feed.sent(key, cursor, true);
                feed.wait_unless(cursor, epoch, ALIVE, crate::conn::ending);
                if feed.durable() <= cursor {
                    if message(&mut w, b'K', &[&alive(feed)])
                        .and_then(|_| w.flush())
                        .is_err()
                    {
                        return;
                    }
                    told = Instant::now();
                }
            }
            Next::Behind => {
                let _ = message(
                    &mut w,
                    b'E',
                    &[b"fell behind the primary's buffer: connect again for an image"],
                )
                .and_then(|_| w.flush());
                return;
            }
            Next::Moved => {
                let _ = message(&mut w, b'E', &[b"the primary started over: connect again"])
                    .and_then(|_| w.flush());
                return;
            }
            Next::Gone => {
                let _ =
                    message(&mut w, b'E', &[b"the database is closing"]).and_then(|_| w.flush());
                return;
            }
        }
    }
}

/// A keep-alive's body: the feed's last write, the last on disk -- what a
/// replica's `behind` is counted to -- and the time.
fn alive(feed: &Feed) -> Vec<u8> {
    u64s(&[feed.seq(), feed.durable(), now_ms()])
}

/// Takes a finished stream off what durable writes wait for.
struct Stopped<'a>(&'a Feed, u64);
impl Drop for Stopped<'_> {
    fn drop(&mut self) {
        self.0.stopped(self.1);
    }
}

/// Takes a finished stream off the status list.
struct Unlist<'a>(&'a Replication, u64);
impl Drop for Unlist<'_> {
    fn drop(&mut self) {
        lock(&self.0.streams).retain(|s| s.key != self.1);
    }
}

// ------------------------------------------------------ the receiving side

/// A primary to be fed from: its address, and the token it asks for.
#[derive(Clone)]
pub struct Upstream {
    url: String,
    pub(crate) addr: String,
    /// What the stream hangs off: nothing for a file, `/t/<tenant>` for one
    /// tenant of a node that replicates its tenants.
    pub(crate) base: String,
    pub(crate) token: String,
    /// The stream is an archiver's or a backup's (`archive=1`): no
    /// promotion takes a write from it, so the primary's hooks do not wait
    /// for it ([`Feed::standby_sent`]).
    archiving: bool,
}

/// A message of the stream, decoded.
pub enum Message {
    /// Where the stream starts: `seq`, and the primary's disk at `durable`.
    /// An [`Message::Image`] follows when `image` is set.
    Hello {
        image: bool,
        seq: u64,
        durable: u64,
        lineage: Vec<(u64, u64)>,
    },
    Image(Vec<u8>),
    /// Writes numbered on from `first`, each with the time it was appended.
    Writes {
        first: u64,
        times: Vec<u64>,
        records: Vec<u8>,
    },
    Alive {
        durable: u64,
    },
    End(String),
}

impl Upstream {
    /// `url` is the primary's address, `http://host:port` or
    /// `https://host:port`, and for one tenant of a node
    /// `http://host:port/t/<tenant>`. Over `https://` the stream is TLS,
    /// the primary's certificate held to the process's CAs
    /// ([`crate::dial::trust_ca`], `--tls-ca`) before the token is sent.
    pub fn new(url: &str, token: String) -> std::result::Result<Upstream, String> {
        let (addr, base) = crate::dial::split_url(url)
            .map_err(|e| format!("a primary is http://host:port or https://host:port: {e}"))?;
        Ok(Upstream {
            url: url.trim_end_matches('/').to_string(),
            addr,
            base,
            token,
            archiving: false,
        })
    }

    /// This upstream, its stream marked an archiver's: the primary's hooks
    /// wait for no stream a promotion could not take a write from.
    pub fn archiving(mut self) -> Upstream {
        self.archiving = true;
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Checks an address given as `--replica-of`: `http://host:port`, and
    /// nothing after it. A path is how a tenant's own stream is reached
    /// (`/t/<tenant>`), never what an operator names -- and one typed by
    /// mistake was followed, against a 404 forever, rather than refused at
    /// start.
    pub fn check_node(url: &str) -> std::result::Result<(), String> {
        if !Upstream::new(url, String::new())?.base.is_empty() {
            return Err(format!(
                "--replica-of names a node, http://host:port or https://host:port, not a path \
                 on one: `{url}`"
            ));
        }
        Ok(())
    }

    /// Opens the stream for a database `since` changes in on `history`;
    /// `image` asks for one whatever the positions say.
    pub fn open(&self, since: u64, history: u64, image: bool) -> io::Result<Receiver> {
        self.request(connect(&self.addr)?, since, history, image, false, None)
    }

    /// [`Upstream::open`] over `s`, a connection to the primary made
    /// already: the request, and the answer's head. Where `stop` is given,
    /// every read of the socket looks at it first ([`Watched`]).
    fn request(
        &self,
        mut s: crate::dial::Stream,
        since: u64,
        history: u64,
        image: bool,
        reading: bool,
        stop: Option<Arc<AtomicBool>>,
    ) -> io::Result<Receiver> {
        s.set_read_timeout(Some(SILENCE))?;
        s.set_write_timeout(Some(SILENCE))?;
        // An archiver's stream and a read replica's say what they are: no
        // promotion takes a write from either, so no hook's answer waits
        // for them ([`Feed::standby_sent`]).
        let request = format!(
            "GET {}/_replication?since={since}&history={history:016x}&sums=1{}{}{} HTTP/1.1\r\n\
             Host: {}\r\nAuthorization: Bearer {}\r\n\r\n",
            self.base,
            if image { "&image=1" } else { "" },
            if self.archiving { "&archive=1" } else { "" },
            if reading { "&reader=1" } else { "" },
            crate::dial::authority(&self.addr),
            self.token
        );
        s.write_all(request.as_bytes())?;
        s.flush()?;
        let conn = s.socket().try_clone()?;
        let mut r = BufReader::with_capacity(MESSAGE + 64, Watched { s, stop });
        let (status, length) = read_head(&mut r)?;
        answered(&mut r, status, length, "primary")?;
        Ok(Receiver { r, conn })
    }
}

/// Past an answer's head: nothing where it is the stream's 200, the
/// refusal in its own words otherwise -- `who`'s, 64 KiB of them read at
/// most, shown as a peer's words are ([`peer_words`]).
pub(crate) fn answered(
    r: &mut impl Read,
    status: u16,
    length: Option<usize>,
    who: &str,
) -> io::Result<()> {
    if status == 200 {
        return Ok(());
    }
    let mut body = vec![0u8; length.unwrap_or(0).min(64 << 10)];
    r.read_exact(&mut body)?;
    Err(io::Error::other(format!(
        "the {who} answered {status}: {}",
        peer_words(&body)
    )))
}

/// The most of a peer's words a log line or `/_replication/status`'s
/// `error` shows ([`peer_words`]): a refusal's sentence whole, a JSON
/// error's with its braces.
pub(crate) const SHOWN: usize = 200;

/// A peer's bytes as a log line and `/_replication/status`'s `error` show
/// them: the first [`SHOWN`] characters, escaped as Rust writes a string --
/// a newline `\n`, an escape character `\u{1b}`, a quote `\"` -- inside
/// quotes, and how many bytes there were where that is not all of them.
/// An `E` message's text (up to [`SHORT`], any bytes) and a refusal's body
/// (64 KiB) went in as they came: whoever answers at a primary's address,
/// or stands on a plain-HTTP hop before it, wrote lines of its own into
/// the log -- a newline and the next line's prefix, a terminal's escapes --
/// and a mebibyte into it and into the status at every reconnect. The one
/// way every reader of a peer's stream shows its words: a replica's, an
/// archiver's, a backup's and a node archiver's -- and a router its nodes'
/// answers, in a log line and the errors it answers with
/// ([`peer_words_within`]).
pub fn peer_words(bytes: &[u8]) -> String {
    peer_words_within(bytes, SHOWN)
}

/// [`peer_words`], showing the first `most` characters: a router shows a
/// node's refusal whole, whose sentence runs past [`SHOWN`] to what an
/// operator does about it -- cut at 200, a failover's refusal of a copy of
/// another lineage lost its last 113 bytes and the `force` it names.
pub fn peer_words_within(bytes: &[u8], most: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut chars = text.chars();
    let shown: String = chars.by_ref().take(most).collect();
    let mut out = format!("{shown:?}");
    if chars.next().is_some() {
        out.push_str(&format!(
            " (the first {most} characters of {} bytes)",
            bytes.len()
        ));
    }
    out
}

/// The primary's connection, a follower's read only while it is not
/// halted: on Windows a halt's cut ends a read already waiting but not one
/// begun after it, and a primary sending a byte at a time -- each in
/// `SILENCE` -- kept that read, and the halt, going for as long as it
/// liked. Looked at before each read of the socket: an atomic's load
/// beside a system call.
struct Watched {
    s: crate::dial::Stream,
    stop: Option<Arc<AtomicBool>>,
}

impl Read for Watched {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
            return Err(io::Error::other("halted"));
        }
        self.s.read(out)
    }
}

/// An open stream, and its socket, which a halt shuts down.
pub struct Receiver {
    r: BufReader<Watched>,
    conn: TcpStream,
}

impl Receiver {
    /// A handle to cut the stream from another thread: a read waiting on
    /// it returns -- through a TLS session too, which reads the socket's
    /// end as the stream's.
    pub fn handle(&self) -> io::Result<TcpStream> {
        self.conn.try_clone()
    }

    pub fn receive(&mut self) -> io::Result<Message> {
        let (tag, body) = read_message(&mut self.r)?;
        decode(tag, body)
    }
}

/// A primary's message, from its kind and its body: a replica's, an
/// archiver's and a backup's reading alike.
fn decode(tag: u8, body: Vec<u8>) -> io::Result<Message> {
    let bad = || io::Error::other("the primary sent a message it should not");
    Ok(match tag {
        b'H' => {
            if body.len() < 26 || body[0] != VERSION {
                return Err(io::Error::other("the primary speaks another protocol"));
            }
            Message::Hello {
                image: body[1] == 1,
                seq: u64_at(&body, 2),
                durable: u64_at(&body, 10),
                lineage: History::decode(&body[26..])
                    .map_err(|e| io::Error::other(e.to_string()))?
                    .lineage,
            }
        }
        b'I' => Message::Image(body),
        b'W' => {
            let n = body.get(8..16).ok_or_else(bad)?;
            let n = u64::from_le_bytes(n.try_into().unwrap());
            // Its times before its records, held to the bytes the message
            // holds: `16 + 8 * n` wrapped in a release build for a count of
            // 2^61, and the times were collected 2^61 of them -- a panic of
            // the follower's thread, and of `fennec archive`, which aborts
            // on one, the process.
            let records = usize::try_from(n)
                .ok()
                .and_then(|n| n.checked_mul(8))
                .and_then(|b| b.checked_add(16))
                .filter(|&at| at <= body.len())
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "the primary's message counts {n} writes, more than its {} bytes hold",
                        body.len()
                    ))
                })?;
            Message::Writes {
                first: u64_at(&body, 0),
                times: (16..records)
                    .step_by(8)
                    .map(|at| u64_at(&body, at))
                    .collect(),
                records: body[records..].to_vec(),
            }
        }
        b'K' if body.len() >= 24 => Message::Alive {
            durable: u64_at(&body, 8),
        },
        // Its words go into a log line and the status's `error`: cut and
        // escaped, never as they came.
        b'E' => Message::End(peer_words(&body)),
        _ => return Err(bad()),
    })
}

/// A primary's answer read off `r` as a replica reads it -- [`read_head`],
/// then each message held to its kind's bound ([`read_message`]) and
/// decoded as [`Receiver::receive`] decodes it -- until one is refused or
/// the bytes end: how many messages it read, and what ended it. Given a
/// `copy`, an image its hello promised is taken as a follower takes one:
/// checked and loaded ([`loaded`]), then adopted by the copy -- its file
/// rewritten, its stores pointed at it (`Database::repoint`), its
/// statements bound again (`rebind`) -- and the copy made to follow the
/// hello's lineage. The fuzzer's `stream` target (`crates/fennec-fuzz`),
/// which hands it what a stand-in at a primary's address could send, and a
/// copy mapped as a server's is.
#[doc(hidden)]
pub fn read_answer(r: &mut impl BufRead, copy: Option<&mut Database>) -> (usize, io::Error) {
    let mut n = 0;
    match read_each(r, &mut n, copy) {
        Err(e) => (n, e),
        Ok(never) => match never {},
    }
}

fn read_each(
    r: &mut impl BufRead,
    n: &mut usize,
    mut copy: Option<&mut Database>,
) -> io::Result<std::convert::Infallible> {
    let (status, length) = read_head(r)?;
    answered(r, status, length, "primary")?;
    let mut promised = None;
    loop {
        let (tag, body) = read_message(r)?;
        let message = decode(tag, body)?;
        *n += 1;
        let Some(copy) = copy.as_deref_mut() else {
            continue;
        };
        match message {
            Message::Hello {
                image: true,
                seq,
                lineage,
                ..
            } => promised = Some((seq, lineage)),
            Message::Image(bytes) => {
                let Some((seq, lineage)) = promised.take() else {
                    return Err(io::Error::other(
                        "the primary sent an image it did not promise",
                    ));
                };
                let (fresh, bytes) = loaded(bytes, &lineage, seq)?;
                copy.adopt(fresh, &bytes).map_err(io::Error::other)?;
                copy.follow(lineage).map_err(io::Error::other)?;
            }
            _ => {}
        }
    }
}

/// A replica's side: connects to the primary, applies what it is sent, and
/// connects again when the stream ends -- until it is promoted.
/// What a follower tells of each hello.
pub enum Said {
    /// Taken: the replica holds a prefix of the primary's history.
    Taken,
    /// Refused as another tenant's: the primary is not of `expected`, the
    /// lineage this copy was told it follows.
    Refused { expected: u32 },
}

type Hello = Box<dyn Fn(Said) + Send + Sync>;

pub struct Follower {
    upstream: Upstream,
    db: Arc<RwLock<Database>>,
    /// This server's own feed, when it has replicas of its own.
    feed: Option<Arc<Feed>>,
    /// Push each write applied to disk before the next: `--sync always`,
    /// whose fsyncs happen on the write path the replica's writes bypass.
    sync_each: bool,
    state: Mutex<State>,
    /// Set by a halt; the session's reads look at it ([`Watched`]).
    stop: Arc<AtomicBool>,
    conn: Mutex<Option<TcpStream>>,
    /// Whether the thread has ended, set however it ends ([`Ends`]): what
    /// a halt waits on.
    ended: (Mutex<bool>, Condvar),
    /// Told each time the primary has said hello and the replica holds a
    /// prefix of its history -- its image adopted, or its place in that
    /// history found: a tenant's read replica answers marked reads from
    /// then on ([`crate::tenants::Tenant::ready`]) -- and where the primary
    /// is of another lineage than the one this copy was told it follows,
    /// and refused ([`Said`]).
    hello: Mutex<Option<Hello>>,
    /// The lineage the primary must be of ([`Follower::expect_lineage`]):
    /// another's is a tenant made again under the name, whose image this
    /// copy never takes.
    expect: Mutex<Option<u32>>,
    /// Set once a primary of another lineage was refused: the follower
    /// stops, the copy holding what it held, answering no marked read.
    refused: AtomicBool,
    /// A read replica's copy ([`Follower::read_replica`]): its stream says
    /// so, and no hook's answer waits for it, since no failover promotes
    /// one.
    reading: AtomicBool,
    /// The look past a socket's reads ([`Follower::halted`]) at which a
    /// test has the halt come: a halt set at that moment, which no event a
    /// test can wait on otherwise marks.
    #[cfg(test)]
    halt_at: Mutex<Option<&'static str>>,
}

#[derive(Clone, Default)]
struct State {
    connected: bool,
    /// The last write on the primary's disk, as it last said.
    primary_durable: u64,
    contact: Option<Instant>,
    images: u64,
    reconnects: u64,
    error: Option<String>,
}

impl Follower {
    pub fn new(
        url: &str,
        token: String,
        db: Arc<RwLock<Database>>,
        feed: Option<Arc<Feed>>,
        sync_each: bool,
    ) -> std::result::Result<Arc<Follower>, String> {
        Ok(Arc::new(Follower {
            upstream: Upstream::new(url, token)?,
            db,
            feed,
            sync_each,
            state: Mutex::new(State::default()),
            stop: Arc::new(AtomicBool::new(false)),
            conn: Mutex::new(None),
            // Nothing to wait for until `run` starts.
            ended: (Mutex::new(true), Condvar::new()),
            hello: Mutex::new(None),
            expect: Mutex::new(None),
            refused: AtomicBool::new(false),
            reading: AtomicBool::new(false),
            #[cfg(test)]
            halt_at: Mutex::new(None),
        }))
    }

    /// Whether a halt came, looked at past the socket's reads too ([`Watched`]
    /// looks before each of them): as an image has arrived, and once it is
    /// loaded, before the copy is changed. A halt during an image's check and
    /// load -- seconds to minutes for gigabytes -- waited for them and the
    /// adoption after them; a promotion, a release under the tenant's slot,
    /// every `get` of the tenant behind it and the node's shutdown waited
    /// with it. Walked away from, the copy holds what it held, and follows on
    /// from there once started again. A halt that comes during the check
    /// and load still waits for them -- nothing looks inside `load` -- and
    /// is met at the second look, before the adoption.
    fn halted(&self, _at: &'static str) -> bool {
        #[cfg(test)]
        self.halt_here(_at);
        self.stop.load(Ordering::SeqCst)
    }

    /// A test's halt, come at `at` (`halt_at`): the flag alone, the socket
    /// not cut -- as a halt's cut is to a read begun after it on Windows.
    #[cfg(test)]
    fn halt_here(&self, at: &'static str) {
        if *lock(&self.halt_at) == Some(at) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Marks this copy a read replica's, or not, from its next stream on:
    /// a primary waits for no read replica to hold a hook's event before
    /// its 2xx ([`Feed::standby_sent`]), a failover never promoting one.
    pub fn read_replica(&self, on: bool) {
        self.reading.store(on, Ordering::Relaxed);
    }

    /// Whether this is a read replica's copy, which no failover promotes.
    pub fn is_read_replica(&self) -> bool {
        self.reading.load(Ordering::Relaxed)
    }

    /// Has `f` called each time the primary says hello and the replica
    /// holds a prefix of its history, before any of its writes is applied,
    /// and where the primary is refused as of another lineage.
    pub fn on_hello(&self, f: Hello) {
        *lock(&self.hello) = Some(f);
    }

    /// Has the follower take nothing from a primary whose lineage is not
    /// `lineage` -- a read replica told which tenant it copies: a tenant
    /// deleted and made again under its name, on the very node it followed,
    /// had the copy take the new one's image, another owner's data in a
    /// region nobody had chosen for it. `None` takes any, as a node's
    /// standby and a tenant's own replica do.
    pub fn expect_lineage(&self, lineage: Option<u32>) {
        *lock(&self.expect) = lineage;
        self.refused.store(false, Ordering::SeqCst);
    }

    /// Whether a primary of another lineage was refused.
    pub fn refused(&self) -> bool {
        self.refused.load(Ordering::SeqCst)
    }

    /// Follows until promoted. Every stream that ends is connected again,
    /// sooner after a clean end than after a failure, a session's panic
    /// among them ([`Self::session_caught`]); the thread's end is said
    /// however it comes ([`Ends`]).
    pub fn run(&self) {
        *lock(&self.ended.0) = false;
        let _ends = Ends(&self.ended);
        self.refused.store(false, Ordering::SeqCst);
        let mut force_image = false;
        let mut pause = Duration::from_millis(100);
        while !self.stop.load(Ordering::SeqCst) {
            let r = self.session_caught(&mut force_image);
            {
                let mut st = lock(&self.state);
                st.connected = false;
                if let Err(e) = &r {
                    if !self.stop.load(Ordering::SeqCst) {
                        st.error = Some(e.to_string());
                    }
                }
            }
            // A replica whose own disk failed takes nothing more; it answers
            // reads from what it holds, as a primary does after one.
            let failed = self
                .db
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .failure()
                .map(String::from);
            if let Some(f) = failed {
                crate::log!("replica stopped: {f}");
                break;
            }
            // Another tenant of the name: nothing it sends is this copy's.
            if self.refused.load(Ordering::SeqCst) {
                if let Err(e) = &r {
                    crate::log!("replica stopped: {e}");
                }
                break;
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = &r {
                crate::log!("replication: {e}; connecting again");
            }
            lock(&self.state).reconnects += 1;
            let until = Instant::now() + pause;
            while Instant::now() < until && !self.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
            pause = if r.is_ok() {
                Duration::from_millis(100)
            } else {
                (pause * 2).min(Duration::from_secs(5))
            };
        }
    }

    /// A session, a panic anywhere in it its failure in words: the framing,
    /// an image's check and load, a hello's callback. Caught only around
    /// `apply`, one elsewhere ended the thread with nothing set -- the copy
    /// serving what it held for good, `running` saying it ran, and a halt
    /// waiting for it for ever, a tenant's slot held meanwhile by a release
    /// or a follow, and every `get` of the tenant, and the node's shutdown,
    /// waiting behind it. One that changed nothing of the copy is a session
    /// that failed, connected again after the back-off; one that left the
    /// copy part way through a change -- an image's adoption, a flush --
    /// failed the database first ([`Changing`]), and the loop stops on it.
    fn session_caught(&self, force_image: &mut bool) -> io::Result<()> {
        match std::panic::catch_unwind(AssertUnwindSafe(|| self.session(force_image))) {
            Ok(r) => r,
            Err(p) => Err(io::Error::other(format!(
                "a panic in the session: {}",
                crate::schedule::panic_words(&*p)
            ))),
        }
    }

    fn session(&self, force_image: &mut bool) -> io::Result<()> {
        let (since, history) = {
            let g = self.db.read().unwrap_or_else(|e| e.into_inner());
            (g.change_seq(), g.history().current())
        };
        // The connect is walked away from at a halt: a promotion halts the
        // follower before it forks, and one in the middle of reconnecting
        // to a dead primary waited out that connect -- its 5 s, and a
        // name's lookup besides -- and the failover with it.
        let s = crate::dial::connect_unless(&self.upstream.addr, CONNECT, &self.stop)?;
        // Handed to `halt` before the request goes out, so a primary that
        // takes the connection and never answers is cut as a stream is, not
        // waited for to the head's SILENCE. The socket beneath a TLS
        // session: shut down, a read through the session meets its end.
        *lock(&self.conn) = Some(s.socket().try_clone()?);
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let reading = self.reading.load(Ordering::Relaxed);
        let stop = Some(Arc::clone(&self.stop));
        let mut stream = self
            .upstream
            .request(s, since, history, *force_image, reading, stop)?;
        let Message::Hello {
            image,
            seq,
            durable,
            lineage,
        } = stream.receive()?
        else {
            return Err(io::Error::other("the primary did not say hello"));
        };
        let expected = *lock(&self.expect);
        if let Some(want) = expected {
            let theirs = lineage.first().map(|&(id, _)| (id >> 32) as u32);
            if theirs != Some(want) {
                self.refused.store(true, Ordering::SeqCst);
                if let Some(hello) = lock(&self.hello).as_ref() {
                    hello(Said::Refused { expected: want });
                }
                return Err(io::Error::other(format!(
                    "the primary is of lineage {}, not {}: another tenant of the name, which \
                     this copy takes nothing from",
                    theirs.map_or("none".into(), lineage_text),
                    lineage_text(want)
                )));
            }
        }
        if image {
            let Message::Image(bytes) = stream.receive()? else {
                return Err(io::Error::other("the primary promised an image"));
            };
            // A halt is looked at as the image has arrived and once it is
            // loaded ([`Self::halted`]): what is left to wait for once the
            // copy changes is the adoption, a rewrite of the file.
            if self.halted("an image arrived") {
                return Ok(());
            }
            // Loaded before the lock is taken: reads go on meanwhile.
            let (fresh, bytes) = loaded(bytes, &lineage, seq)?;
            if self.halted("an image loaded") {
                return Ok(());
            }
            let _changing = Changing(&self.db, "adopted its primary's image");
            let mut g = crate::held::write(&self.db);
            g.adopt(fresh, &bytes).map_err(io::Error::other)?;
            g.follow(lineage).map_err(io::Error::other)?;
            if let Some(feed) = &self.feed {
                // The image's history, which `follow` appends no record
                // for where it is the one the image held.
                feed.set_lineage(&g.history().lineage);
                feed.start(g.change_seq());
            }
            lock(&self.state).images += 1;
            *force_image = false;
        } else {
            let _changing = Changing(&self.db, "took its primary's history");
            let mut g = crate::held::write(&self.db);
            if g.change_seq() != since {
                return Err(io::Error::other("the replica moved while it connected"));
            }
            g.follow(lineage).map_err(io::Error::other)?;
        }
        {
            let mut st = lock(&self.state);
            st.connected = true;
            st.primary_durable = durable;
            st.contact = Some(Instant::now());
            st.error = None;
        }
        if let Some(hello) = lock(&self.hello).as_ref() {
            hello(Said::Taken);
        }

        loop {
            // A halt is looked at between messages, as an archiver looks at
            // its stop, never left to the socket's cut alone: on Windows the
            // cut -- a `shutdown` through a duplicate of the socket -- ends a
            // read already waiting, but a read begun after it reads on, the
            // primary's keep-alive every `ALIVE` keeping it short of its
            // `SILENCE`. A follower between two reads at the halt -- landing
            // a write, its fsync -- followed on for good, and the halt
            // waited for it for good: five runs of Windows CI hung so. One
            // that looked just before the cut ends after its next message.
            if self.halted("between messages") {
                return Ok(());
            }
            // A halt after the look, its cut missed by the read: met by the
            // read's own look ([`Watched`]), which a test holds here.
            #[cfg(test)]
            self.halt_here("in a message");
            match stream.receive()? {
                Message::Writes {
                    first,
                    times,
                    records,
                } => {
                    // The apply's own panic is caught below, the engine
                    // having put back or failed what it held; one past it --
                    // the times, the flush, the fsync -- fails the copy.
                    let _changing = Changing(&self.db, "landed its primary's writes");
                    let mut g = crate::held::write(&self.db);
                    if first != g.change_seq() + 1 {
                        *force_image = true;
                        return Err(io::Error::other(format!(
                            "the primary sent write {first}, the replica is at {}",
                            g.change_seq()
                        )));
                    }
                    // A panic applying them is caught here, the lock held: the
                    // engine put back the records not landed, or failed the
                    // database where one reached the sink. Let through, it
                    // ended the follower's thread with nothing in the log --
                    // the copy serving what it held, taking nothing more,
                    // and a promotion's halt waiting for a thread gone.
                    let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        g.apply(&records)
                    }));
                    // Each record that landed, the times its primary appended
                    // it at, before the lock goes and before the flush makes
                    // it sendable: entered in this server's feed, it was
                    // stamped with this clock as it was applied, and a copy
                    // of this one, its `/_changes` and an archive cut from it
                    // read each write a replication lag late -- a restore to
                    // a moment leaving out writes the primary had answered.
                    // Counted from the message's own records, a `writes_in`
                    // each, this read a third caller of it, which had the
                    // release build's LTO stop inlining it into
                    // `Feed::push`: a call more on every write a primary
                    // makes. The apply's count of records it landed needs
                    // none.
                    if let Some(feed) = &self.feed {
                        let landed = match &applied {
                            Ok(Ok(n)) => Some(*n),
                            _ => None,
                        };
                        feed.sent_times(first, landed, &times);
                    }
                    let failed = match applied {
                        Ok(Ok(_)) => None,
                        Ok(Err(e)) => Some(e.to_string()),
                        Err(p) => Some(format!("a panic: {}", crate::schedule::panic_words(&*p))),
                    };
                    if let Some(e) = failed {
                        *force_image = true;
                        return Err(io::Error::other(format!("could not apply a write: {e}")));
                    }
                    let at = g.change_seq();
                    let durable = if self.sync_each {
                        g.flush().map_err(io::Error::other)?
                    } else {
                        None
                    };
                    drop(g);
                    // The fsync runs without the lock, as a primary's does.
                    if let Some(durable) = durable {
                        if let Err(e) = durable() {
                            self.db.write().unwrap_or_else(|p| p.into_inner()).fail(&e);
                            return Err(io::Error::other(e.to_string()));
                        }
                    }
                    let mut st = lock(&self.state);
                    st.primary_durable = st.primary_durable.max(at);
                    st.contact = Some(Instant::now());
                }
                Message::Alive { durable } => {
                    let mut st = lock(&self.state);
                    st.primary_durable = durable;
                    st.contact = Some(Instant::now());
                }
                Message::End(why) => return Err(io::Error::other(why)),
                _ => return Err(io::Error::other("the primary sent a message it should not")),
            }
        }
    }

    /// Runs the follower on a thread of its own, named `name` -- and again
    /// after [`Self::halt`], from where it stopped. Nothing is started when
    /// it is running already. The three servers started it each their own
    /// way, and a tenant's dropped a failed start without a word: a replica
    /// that never followed, refusing writes, with nothing in the log.
    pub fn start(self: &Arc<Self>, name: String) -> io::Result<()> {
        let mut ended = lock(&self.ended.0);
        if !*ended {
            return Ok(());
        }
        self.stop.store(false, Ordering::SeqCst);
        *ended = false;
        drop(ended);
        let run = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(name)
            .spawn(move || run.run());
        if let Err(e) = spawned {
            *lock(&self.ended.0) = true;
            return Err(e);
        }
        Ok(())
    }

    /// Stops following and starts taking writes, on a history of its own,
    /// forked where the replica stands: of its lineage, `fresh`'s bottom
    /// half under its top half ([`fork_id`]). Returns that change and the
    /// history's id.
    pub fn promote(&self, fresh: u64) -> fennec_core::error::Result<(u64, u64)> {
        self.halt();
        let mut g = crate::held::write(&self.db);
        let id = fork_id(g.history(), fresh);
        g.fork(id)?;
        g.sync()?;
        Ok((g.change_seq(), id))
    }

    /// [`Self::promote`], and what the copy knew of its primary as it was
    /// promoted ([`Heard`]): whether it was connected as the promotion
    /// began, and, once its follower stopped, how far behind its primary's
    /// disk it stood and how long before it last heard from it.
    pub fn promote_heard(&self, fresh: u64) -> fennec_core::error::Result<(u64, u64, Heard)> {
        let connected = lock(&self.state).connected;
        let (seq, id) = self.promote(fresh)?;
        let st = lock(&self.state).clone();
        Ok((
            seq,
            id,
            Heard {
                connected,
                behind: st.primary_durable.saturating_sub(seq),
                last_contact_ms: st.contact.map(|t| t.elapsed().as_millis() as u64),
            },
        ))
    }

    /// Stops following and stays a replica: started again, it follows on
    /// from where it stopped. Waits for the thread's end, which the thread
    /// says however it ends ([`Ends`]), as an event. What bounds the wait
    /// is what the thread does once the flag is set: a read of the primary
    /// ends at the cut -- on Unix at once, on Windows at once where it
    /// waited already, and one begun after it at its next byte, which the
    /// read looks at the flag before ([`Watched`]), or `SILENCE` (10 s),
    /// the deadline every read has, from a primary gone silent; a connect
    /// is walked away from within 10 ms (`dial::connect_unless`), the
    /// back-off within 20; an image that has arrived is walked away from
    /// before its check and load and again before its adoption
    /// ([`Self::halted`]), a check and load already under way waited for
    /// -- minutes for many gigabytes -- and walked away from after; else
    /// one message landing under the write lock, and its fsync, or an
    /// adoption begun: the image written over the copy's file and synced, a
    /// write of the image's length, which the copy's memory then points
    /// at. A promotion and a tenant's release, which halt, end within that,
    /// and so do a `get` of the tenant and the node's shutdown, which wait
    /// for the tenant's slot a release holds as it halts.
    pub fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = lock(&self.conn).take() {
            let _ = s.shutdown(Shutdown::Both);
        }
        let mut ended = lock(&self.ended.0);
        while !*ended {
            ended = self.ended.1.wait(ended).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Whether the follower's thread is running: started, and not halted.
    pub fn running(&self) -> bool {
        !*lock(&self.ended.0)
    }

    /// Whether the replica is connected, and how far behind the primary's
    /// disk it is, for tests and measurement.
    pub fn progress(&self) -> (bool, u64) {
        let st = lock(&self.state);
        (st.connected, st.primary_durable)
    }

    /// Images taken so far.
    pub fn images(&self) -> u64 {
        lock(&self.state).images
    }
}

/// What a copy knew of its primary as it was promoted
/// ([`Follower::promote_heard`]), by the names `/_replication/status`
/// gives them: the admin's promotion answers them and a router's failover
/// logs them. A failover promotes a copy where it stands: a durable write
/// waits a [`SENT_WAIT`] at most for the streams keeping up and none for
/// one lagging, reconnecting or failed, so what a copy promoted not
/// connected, or behind, was never sent is lost -- a write its primary
/// answered among it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heard {
    /// Connected to its primary as the promotion began: true until a read
    /// of the stream fails. A primary's process killed ends the stream with
    /// it; its machine lost or cut off leaves the copy connected until
    /// `SILENCE` (10 s) passes with nothing heard, which a lease's lapse
    /// comes before: `last_contact_ms` past a keep-alive's [`ALIVE`] says
    /// it was not hearing from it.
    pub connected: bool,
    /// Writes on its primary's disk, as its primary last said, it had not
    /// applied: each of them lost to the promotion. A stream says so every
    /// [`ALIVE`], behind or caught up.
    pub behind: u64,
    /// How long before the promotion its primary was last heard from, a
    /// keep-alive's every [`ALIVE`] included; `None` where it was not since
    /// the copy was opened.
    pub last_contact_ms: Option<u64>,
}

impl Heard {
    /// Its fields as an answer's JSON writes them, each after a comma:
    /// `last_contact_ms` -1 for never, as the status gives it.
    pub fn json(&self) -> String {
        format!(
            ",\"connected\":{},\"behind\":{},\"last_contact_ms\":{}",
            self.connected,
            self.behind,
            self.last_contact_ms.map_or(-1, |ms| ms as i64)
        )
    }
}

/// A primary's image made the database a copy adopts: every record held
/// to its sum, which an open does not ask of an image's -- one damaged on
/// the way would be this file's whole -- then loaded into a database of its
/// own with the record saying it follows on its `lineage`, and held to the
/// change its hello said, `seq`. The image's own history is its
/// primary's, taking writes, and the record `follow` appends after the
/// rename went with a kill before a write behind: the restart opened the
/// copy as a primary's file, following nothing (`Tenants::open_as`); with
/// it in the bytes that replace the file, `follow` appends nothing. Room
/// for the record alone: grown as a `Vec` grows, a 40 MB image took 80. An
/// image at another change than its hello said is no primary's: refused
/// before it replaces anything, where a `debug_assert` stood, a panic in a
/// debug build that a peer's bytes reached. The follower's path and the
/// `stream` fuzz target's ([`read_answer`]).
fn loaded(mut bytes: Vec<u8>, lineage: &[(u64, u64)], seq: u64) -> io::Result<(Database, Vec<u8>)> {
    fennec_core::engine::check_records(&bytes).map_err(io::Error::other)?;
    let follows = History {
        lineage: lineage.to_vec(),
        following: true,
    }
    .record();
    // Asked, a refusal in words: the image is the database whole, and
    // its room taken as `reserve_exact` takes it, a refused one aborted the
    // process -- a node and every tenant it holds -- one allocation past
    // `read_kind`'s. `load`'s own allocations still abort on a refusal.
    bytes.try_reserve_exact(follows.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!(
                "an image of {} bytes: the allocator refused the {} asked for it and the \
                 record that follows it",
                bytes.len(),
                bytes.len().saturating_add(follows.len())
            ),
        )
    })?;
    bytes.extend_from_slice(&follows);
    let mut fresh = Database::new();
    fresh.load(&bytes).map_err(io::Error::other)?;
    if fresh.change_seq() != seq {
        return Err(io::Error::other(format!(
            "the primary's image is at change {}, its hello at {seq}",
            fresh.change_seq()
        )));
    }
    Ok((fresh, bytes))
}

/// Says a follower's thread ended as it drops: at the loop's end and in a
/// panic's unwinding alike, where a line at the end of `run` was skipped
/// and a halt waited for ever on a thread gone.
struct Ends<'a>(&'a (Mutex<bool>, Condvar));

impl Drop for Ends<'_> {
    fn drop(&mut self) {
        *lock(&self.0 .0) = true;
        self.0 .1.notify_all();
    }
}

/// Held while a session changes the copy past what the engine puts back
/// on its own: an image adopted and its history followed, this server's
/// feed started on it, a message's times, its flush and its fsync. A panic
/// unwinding through it -- after the write lock it was made before -- left
/// the file, the memory and the feed not known to agree, and fails the
/// database in words ([`Database::fail`]): no write lands on the copy and
/// no promotion forks it, its status says why, and the follower stops, as
/// after its disk failed. Opened again, the copy is its file's.
struct Changing<'a>(&'a RwLock<Database>, &'static str);

impl Drop for Changing<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let why = fennec_core::error::Error::Io(format!(
                "a panic as the follower {}: the copy's file and memory may disagree, and it \
                 takes nothing more until it is opened again",
                self.1
            ));
            self.0.write().unwrap_or_else(|p| p.into_inner()).fail(&why);
        }
    }
}

/// A connection to a primary's address, `https://` dialed over TLS, its
/// handshake within [`CONNECT`] too.
pub(crate) fn connect(addr: &str) -> io::Result<crate::dial::Stream> {
    crate::dial::connect(addr, CONNECT)
}

/// The status line and the length of the body, when it has one. The head
/// is held to the bound the server holds a request's to
/// ([`crate::http::MAX_HEADER_BYTES`], 64 KiB), its lines read by the
/// server's own reader with what is left of it ([`crate::http::read_line`]):
/// the status line, each header line and so the count of them -- a line
/// two bytes at the least -- within it, a line past it refused as its
/// bytes come, a buffer's fill past it at the most. The status line was
/// read to its newline however long, and a header line measured only once
/// its newline came: a stand-in sending a line with no end grew a
/// replica's memory with every byte, for as long as it sent one in each
/// `SILENCE`.
pub(crate) fn read_head(r: &mut impl BufRead) -> io::Result<(u16, Option<usize>)> {
    use crate::http::{Line, MAX_HEADER_BYTES};
    let mut left = MAX_HEADER_BYTES;
    let long = || {
        io::Error::other(format!(
            "the answer's head ran past {MAX_HEADER_BYTES} bytes"
        ))
    };
    let mut next = |line: &mut Vec<u8>| -> io::Result<usize> {
        line.clear();
        let n = crate::http::read_line(r, line, left).map_err(|e| match e {
            Line::Io(e) => e,
            Line::Long => long(),
        })?;
        left = left.checked_sub(n).ok_or_else(long)?;
        Ok(n)
    };
    let mut line = Vec::new();
    next(&mut line)?;
    let first = String::from_utf8_lossy(&line);
    let status = first
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other(format!("not an HTTP answer: {}", peer_words(&line))))?;
    let mut length = None;
    loop {
        if next(&mut line)? == 0 {
            return Err(io::Error::other("the answer's head did not end"));
        }
        let l = String::from_utf8_lossy(&line);
        let l = l.trim_end();
        if l.is_empty() {
            return Ok((status, length));
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().ok();
            }
        }
    }
}

/// What a primary sends of each kind of message, at the most ([`stream`]):
/// an image and writes [`LONGEST`], a hello, a keep-alive and an end
/// [`SHORT`]; `None` for a kind it never sends.
fn primary_sends(tag: u8) -> Option<u64> {
    match tag {
        b'I' | b'W' => Some(LONGEST),
        b'H' | b'K' | b'E' => Some(SHORT),
        _ => None,
    }
}

/// A primary's message off its stream, held to its kind's bound
/// ([`read_kind`]).
pub(crate) fn read_message(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    read_kind(r, primary_sends)
}

/// A message off a stream, its kind and its body: held to the bound
/// `longest` gives its kind before a byte of the body is read -- a kind it
/// gives none refused -- and read into memory as it comes. The length is
/// the peer's word: allocated whole before a byte came, a terabyte
/// announced in nine bytes was asked of the allocator, which Linux's
/// overcommit refuses, and a refused allocation aborts the process -- a
/// node with every tenant it holds, its supervisor's restart following the
/// same stream. The body is first given [`FIRST`] at the most, a steady
/// stream's message whole in one allocation of its length, and doubles,
/// to its length and no further, as its bytes fill it: memory follows what
/// came. A growth holds the bytes that came and the buffer twice as long
/// the allocator moves them into, three times what came at the most, and
/// a message read whole has held twice its length at the most: measured
/// (`tests/message_peak.rs`, a growth counted as the move it may be), the
/// allocator's high-water mark for an image of 40 MiB was 72 MiB, 1.80
/// times it, for one a byte past [`FIRST`] doubled twice twice it less a
/// byte, and for a terabyte announced and 3 MiB sent 6 MiB, three times
/// the [`FIRST`] that had come as it grew. Each allocation is asked with
/// `try_reserve_exact`, a refusal an error naming the kind, the length and
/// the bytes asked for: a refused allocation aborts the process -- a node
/// with every tenant it holds, `fennec archive` and `fennec backup`, which
/// abort on a panic too -- and a stand-in sends what it likes, the length
/// its kind allows.
pub(crate) fn read_kind(
    r: &mut impl Read,
    longest: fn(u8) -> Option<u64>,
) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 9];
    r.read_exact(&mut head)?;
    let (tag, len) = (head[0], u64::from_le_bytes(head[1..].try_into().unwrap()));
    let Some(most) = longest(tag) else {
        return Err(io::Error::other(format!(
            "a message of a kind none is sent of ({tag})"
        )));
    };
    let kind = tag as char;
    if len > most {
        return Err(io::Error::other(format!(
            "a `{kind}` message of {len} bytes, past the {most} one of its kind holds"
        )));
    }
    let cut = |e: io::Error| match e.kind() {
        io::ErrorKind::UnexpectedEof => io::Error::new(
            e.kind(),
            format!("a `{kind}` message cut short of its {len} bytes"),
        ),
        _ => e,
    };
    let len = usize::try_from(len).map_err(|_| {
        io::Error::other(format!(
            "a `{kind}` message of {len} bytes, more than this machine addresses"
        ))
    })?;
    let mut body = Vec::new();
    grown(&mut body, len.min(FIRST), kind, len)?;
    r.read_exact(&mut body).map_err(cut)?;
    while body.len() < len {
        let have = body.len();
        grown(&mut body, len.min(have.saturating_mul(2)), kind, len)?;
        r.read_exact(&mut body[have..]).map_err(cut)?;
    }
    Ok((tag, body))
}

/// `body` made `to` bytes long, zeroed past what it held, the allocator
/// asked for no more than that and its refusal an error in words: a
/// `kind` message of `len` bytes, and the bytes asked for.
fn grown(body: &mut Vec<u8>, to: usize, kind: char, len: usize) -> io::Result<()> {
    body.try_reserve_exact(to - body.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!(
                "a `{kind}` message of {len} bytes: the allocator refused the {to} asked for \
                 it"
            ),
        )
    })?;
    body.resize(to, 0);
    Ok(())
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fork keeps the top half of the first history its database took:
    /// its lineage, by which a client tells an older term of the same
    /// tenant from a tenant made again under the name. One begun from the
    /// root takes the fresh id whole, and no fork takes an id the lineage
    /// holds.
    #[test]
    fn a_fork_keeps_its_lineage_and_a_history_from_the_root_starts_one() {
        use fennec_core::history::History;
        let root = History::default();
        assert_eq!(fork_id(&root, 0x1234_5678_9abc_def0), 0x1234_5678_9abc_def0);
        let made = root.forked(0xaaaa_bbbb_0000_0001, 0);
        let promoted = fork_id(&made, 0x1111_2222_3333_4444);
        assert_eq!(promoted, 0xaaaa_bbbb_3333_4444);
        let again = made.forked(promoted, 10);
        assert_eq!(fork_id(&again, 0x9999_9999_3333_4444) >> 32, 0xaaaa_bbbb);
        // A bottom half the lineage holds already: the next one free.
        assert_eq!(
            fork_id(&again, 0x9999_9999_3333_4444),
            0xaaaa_bbbb_3333_4445
        );
        // 0's bottom half is 1, which the first history holds.
        assert_eq!(fork_id(&again, 0), 0xaaaa_bbbb_0000_0002);
    }

    fn record(n: usize) -> Vec<u8> {
        // [kind][cid][length][body], the body one frame `n` bytes long:
        // [op][id][length][payload].
        let plen = if n - 3 < 128 { n - 3 } else { n - 4 };
        let mut body = vec![1u8, 1];
        fennec_core::codec::put_uvarint(&mut body, plen as u64);
        body.extend(std::iter::repeat_n(0xab, plen));
        assert_eq!(body.len(), n);
        let mut r = vec![3, 1];
        fennec_core::codec::put_uvarint(&mut r, n as u64);
        r.extend(body);
        r
    }

    /// A record of `frames` writes, as a statement of that many documents
    /// or a block to one collection is appended.
    fn frames(frames: usize) -> Vec<u8> {
        let mut body = Vec::new();
        for id in 1..=frames as u64 {
            body.extend_from_slice(&[1, id as u8, 1, 0xcd]);
        }
        let mut r = vec![3, 1];
        fennec_core::codec::put_uvarint(&mut r, body.len() as u64);
        r.extend(body);
        r
    }

    #[test]
    fn a_record_of_many_writes_is_sent_whole_and_numbered_as_its_last() {
        let feed = Feed::new(1 << 20);
        feed.start(10);
        let epoch = feed.epoch();
        feed.push(11, &record(4));
        feed.push(15, &frames(4));
        feed.push(16, &record(4));
        feed.mark_durable(16);
        match feed.next(11, epoch, MESSAGE) {
            Next::Records(first, last, times, bytes) => {
                assert_eq!((first, last, times.len()), (12, 16, 2));
                assert_eq!(bytes, [frames(4), record(4)].concat());
            }
            _ => panic!("records expected"),
        }
        // Inside the record is no place a replica stands.
        assert!(matches!(feed.next(13, epoch, MESSAGE), Next::Behind));
        // Durable only up to its last write, it waits for it.
        let feed = Feed::new(1 << 20);
        feed.start(0);
        feed.push(3, &frames(3));
        feed.mark_durable(2);
        assert!(matches!(feed.next(0, feed.epoch(), MESSAGE), Next::Nothing));
    }

    #[test]
    fn only_what_is_on_disk_is_sent() {
        let feed = Feed::new(1 << 20);
        feed.start(10);
        let epoch = feed.epoch();
        for seq in 11..=15 {
            feed.push(seq, &record(4));
        }
        // Appended, not yet durable: nothing to send.
        assert!(matches!(feed.next(10, epoch, MESSAGE), Next::Nothing));
        feed.mark_durable(13);
        match feed.next(10, epoch, MESSAGE) {
            Next::Records(first, _, times, bytes) => {
                assert_eq!(first, 11);
                assert_eq!(times.len(), 3);
                assert_eq!(bytes, record(4).repeat(3));
            }
            _ => panic!("records expected"),
        }
        assert!(matches!(feed.next(13, epoch, MESSAGE), Next::Nothing));
        // A mark past what was appended counts only what was.
        feed.mark_durable(99);
        assert_eq!(feed.durable(), 15);
    }

    #[test]
    fn the_syncers_durability_waits_for_no_stream() {
        let dir = std::env::temp_dir().join(format!(
            "fennec-unwaited-{}-{}",
            std::process::id(),
            std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.fennec");
        let (mut db, feed) = open(path.to_str().unwrap(), 1 << 20).unwrap();
        db.execute(&fennec_ql::parse_one("create collection c (x int)").unwrap())
            .unwrap();
        if let Some(d) = db.flush().unwrap() {
            d().unwrap();
        }
        // A stream that sends nothing: a durable write's answer waits for
        // it, the syncer's durability does not.
        feed.sending(7, db.change_seq());
        let write = |db: &mut Database| {
            db.execute(&fennec_ql::parse_one("put c {x: 1}").unwrap())
                .unwrap();
            db.flush().unwrap().expect("a durability to run")
        };
        unwaited(write(&mut db)).unwrap();
        assert_eq!(feed.waited_out(), 0);
        let t0 = Instant::now();
        write(&mut db)().unwrap();
        assert!(t0.elapsed() >= SENT_WAIT);
        assert_eq!(feed.waited_out(), 1);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_small_buffer_drops_the_oldest_and_sends_behind() {
        // 8 000 bytes, kept in chunks of 1 000: three records each.
        let feed = Feed::new(8_000);
        feed.start(0);
        let epoch = feed.epoch();
        let big = record(250);
        let n = 200;
        for seq in 1..=n {
            feed.push(seq, &big);
        }
        feed.mark_durable(n);
        assert!(!feed.serves(0));
        assert!(matches!(feed.next(0, epoch, MESSAGE), Next::Behind));
        // The newest chunk holds the last two writes (200 = 66 x 3 + 2), and
        // nine whole ones fit beside it: 9 x 759 + 506 = 7 337 bytes, where
        // a tenth would make 8 096.
        let oldest = (0..n).find(|&s| feed.serves(s)).unwrap();
        assert_eq!(oldest, n - 29);
        assert!(matches!(
            feed.next(oldest - 1, epoch, MESSAGE),
            Next::Behind
        ));
        let Next::Records(first, _, times, _) = feed.next(oldest, epoch, MESSAGE) else {
            panic!("records expected");
        };
        assert_eq!((first, times.len()), (oldest + 1, 29));
        // A message stops at its size, across the line between two chunks
        // as anywhere.
        let across = oldest + 2;
        let Next::Records(first, _, times, bytes) = feed.next(across, epoch, 4 * big.len()) else {
            panic!("records expected");
        };
        assert_eq!((first, times.len()), (across + 1, 4));
        assert_eq!(bytes, big.repeat(4));
        // Starting over ends every stream begun before.
        feed.start(n);
        assert!(matches!(feed.next(n - 10, epoch, MESSAGE), Next::Moved));
    }

    /// A follower's feed holds each record it applied at the time its
    /// primary sent with it: a block's record one time for its writes, a
    /// message landed whole each record's own, one of which only a prefix
    /// landed -- the records after a refused one put back, never handed to
    /// the sink -- its prefix's, and a write of the copy's own after them,
    /// promoted, its own clock's.
    #[test]
    fn a_followers_feed_holds_the_times_its_primary_sent() {
        let feed = Feed::new(1 << 20);
        feed.start(10);
        let epoch = feed.epoch();
        // A message of four records, a write, a block of four, a write and
        // a write: landed whole.
        feed.push(11, &record(4));
        feed.push(15, &frames(4));
        feed.push(16, &record(4));
        feed.push(17, &record(4));
        feed.sent_times(11, Some(4), &[100, 200, 300, 400]);
        // The same, of which three landed: the apply was refused at the
        // fourth.
        feed.push(18, &record(4));
        feed.push(22, &frames(4));
        feed.push(23, &record(4));
        feed.sent_times(18, None, &[500, 600, 700, 800]);
        let before = now_ms();
        feed.push(24, &record(4));
        feed.mark_durable(24);
        let Next::Records(first, last, times, _) = feed.next(10, epoch, MESSAGE) else {
            panic!("records expected");
        };
        assert_eq!((first, last), (11, 24));
        assert_eq!(times[..7], [100, 200, 300, 400, 500, 600, 700]);
        assert!(times[7] >= before, "{times:?}");
        // Times for none of the records here move nothing.
        let feed = Feed::new(1 << 20);
        feed.start(10);
        feed.push(11, &record(4));
        let stamped = lock(&feed.ring).chunks[0].times.clone();
        feed.sent_times(12, Some(2), &[1, 2]);
        feed.sent_times(12, None, &[1, 2]);
        assert_eq!(lock(&feed.ring).chunks[0].times, stamped);
    }

    /// A message whose first records the ring let go of as its later ones
    /// came in -- a buffer smaller than what one apply puts in it -- gives
    /// each record still kept its own time, never the time of the record
    /// that came first; an apply refused part way, which says nothing of
    /// how many landed, then gives none, the image the follower connects
    /// again for starting the feed over.
    #[test]
    fn a_followers_records_the_ring_kept_take_their_own_times() {
        // 8 000 bytes in chunks of 1 000, a record of 3 000 a chunk: four
        // of them leave the last two.
        let big = record(3_000);
        let fed = |landed: Option<usize>| {
            let feed = Feed::new(8_000);
            feed.start(0);
            for seq in 1..=4 {
                feed.push(seq, &big);
            }
            assert!(!feed.serves(1));
            feed.sent_times(1, landed, &[10, 20, 30, 40]);
            feed.mark_durable(4);
            let Next::Records(first, _, times, _) = feed.next(2, feed.epoch(), MESSAGE) else {
                panic!("records expected");
            };
            assert_eq!(first, 3);
            times
        };
        assert_eq!(fed(Some(4)), [30, 40]);
        let before = now_ms();
        let times = fed(None);
        assert!(times.iter().all(|&t| t >= before), "{times:?}");
    }

    /// A stand-in's stream: a message's head announcing `len` bytes, then
    /// `given` bytes of its body and the stream's end. It notes the longest
    /// read asked of it: the most of the body held in memory at once.
    struct Announcing {
        head: Vec<u8>,
        given: usize,
        read: usize,
        longest: usize,
    }

    impl Announcing {
        fn new(tag: u8, len: u64, given: usize) -> Announcing {
            let mut head = vec![tag];
            head.extend_from_slice(&len.to_le_bytes());
            Announcing {
                head,
                given,
                read: 0,
                longest: 0,
            }
        }
    }

    impl Read for Announcing {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.longest = self.longest.max(out.len());
            let all = self.head.len() + self.given;
            let n = out.len().min(all - self.read);
            for (i, b) in out[..n].iter_mut().enumerate() {
                let at = self.read + i;
                *b = self.head.get(at).copied().unwrap_or(0);
            }
            self.read += n;
            Ok(n)
        }
    }

    /// A message's body is held in memory as it comes, never as it was
    /// announced: a write of a terabyte announced and three mebibytes sent
    /// is refused as cut short, a few mebibytes held at the most. The
    /// terabyte was asked of the allocator before a byte came, which
    /// Linux's overcommit refuses -- and a refused allocation aborts the
    /// process, a node with every tenant it holds.
    /// A message's body is its length and no more: a steady stream's in
    /// one allocation of it, as it always was, and one past [`FIRST`] grown
    /// to it and no further. A kind no primary sends, and a length past its
    /// kind's bound, are refused before a byte of the body is read.
    #[test]
    fn a_message_is_held_in_its_length_and_its_kind_bound() {
        for len in [0, 24, MESSAGE + 4096, 2 * MESSAGE + 16, 5 * MESSAGE + 3] {
            let mut bytes = vec![b'W'];
            bytes.extend_from_slice(&(len as u64).to_le_bytes());
            bytes.extend(std::iter::repeat_n(7u8, len));
            let (tag, body) = read_message(&mut &bytes[..]).unwrap();
            assert_eq!((tag, body.len(), body.capacity()), (b'W', len, len));
            assert!(body.iter().all(|&b| b == 7));
        }
        let mut unknown = Announcing::new(b'Z', 8, 8);
        let e = read_message(&mut unknown).unwrap_err().to_string();
        assert!(e.contains("kind") && unknown.read == 9, "{e}");
        let mut long = Announcing::new(b'K', SHORT + 1, 8);
        let e = read_message(&mut long).unwrap_err().to_string();
        assert!(e.contains(&SHORT.to_string()) && long.read == 9, "{e}");
    }

    #[test]
    fn a_message_is_held_as_it_comes_not_as_it_was_announced() {
        let mut r = Announcing::new(b'W', 1 << 40, 3 << 20);
        let e = read_message(&mut r).unwrap_err();
        assert!(
            r.longest <= 8 << 20,
            "a read of {} bytes asked for, {} sent: {e}",
            r.longest,
            3 << 20
        );
    }

    /// A refused allocation is an error in words naming the message's
    /// kind, its length and the bytes asked for, the body as it was: asked
    /// with `reserve_exact`, the refusal aborted the process -- a node with
    /// every tenant, `fennec archive`, `fennec backup`. 2^62 bytes is past
    /// every machine's address space and within what a `Vec` may ask.
    #[test]
    fn a_refused_allocation_is_an_error_in_words() {
        let mut body = vec![7u8; 16];
        let asked = 1usize << 62;
        let e = grown(&mut body, asked, 'I', asked).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::OutOfMemory, "{e}");
        let words = e.to_string();
        assert!(
            words.contains("`I`") && words.contains(&asked.to_string()),
            "{words}"
        );
        assert_eq!(body, [7u8; 16]);
    }

    /// A peer's words reach a log line and the status's `error` cut and
    /// escaped: a text of a mebibyte holding a newline, an escape character
    /// and a quote -- an `E` message's, a refusal's body, a status line --
    /// is shown as its first [`SHOWN`] characters, escaped, and its length.
    #[test]
    fn a_peers_words_are_cut_and_escaped() {
        let mut text = b"closing\nfennec-http: forged line \x1b[2J \"quoted\" ".to_vec();
        text.resize(1 << 20, b'x');
        let plain = |w: &str| {
            assert!(!w.contains('\n') && !w.contains('\x1b'), "{w:?}");
            assert!(w.len() < 2 * SHOWN, "{} bytes shown", w.len());
            assert!(
                w.contains(r"closing\nfennec-http")
                    && w.contains(r"\u{1b}[2J")
                    && w.contains(r#"\"quoted\""#),
                "{w}"
            );
        };
        let Ok(Message::End(why)) = decode(b'E', text.clone()) else {
            panic!("an end");
        };
        plain(&why);
        assert!(
            why.contains(&(1usize << 20).to_string()),
            "the length: {why}"
        );
        let e = answered(&mut &text[..], 403, Some(text.len()), "primary").unwrap_err();
        plain(&e.to_string());
        assert!(
            e.to_string().starts_with("the primary answered 403: "),
            "{e}"
        );
        // A status line of 60 KiB, within the head's bound, not an answer's.
        let mut head = b"\x1b[2J HTTP/1.1 ".to_vec();
        head.resize(60 << 10, b'x');
        head.extend_from_slice(b"\r\n\r\n");
        let e = read_head(&mut &head[..]).unwrap_err().to_string();
        assert!(
            !e.contains('\n')
                && !e.contains('\x1b')
                && e.len() < 2 * SHOWN
                && e.contains(r"\u{1b}"),
            "{e:?}"
        );
        // Short and plain, a peer's words are themselves, quoted.
        assert_eq!(peer_words(b"invalid token"), r#""invalid token""#);
    }

    /// A stream's answer as a primary writes one: its head, a hello at
    /// `seq` -- an image to follow or not -- and `then`.
    fn answer(image: bool, seq: u64, then: &[u8]) -> Vec<u8> {
        let mut hello = vec![VERSION, image as u8];
        for v in [seq, seq, 0] {
            hello.extend_from_slice(&v.to_le_bytes());
        }
        let lineage = History {
            lineage: vec![((7u64 << 32) | 1, 0)],
            following: false,
        };
        hello.extend_from_slice(&lineage.encode());
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\r\n".to_vec();
        out.extend_from_slice(&framed(b'H', &hello));
        out.extend_from_slice(then);
        out
    }

    /// A message as a primary frames one.
    fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut m = vec![tag];
        m.extend_from_slice(&(body.len() as u64).to_le_bytes());
        m.extend_from_slice(body);
        m
    }

    /// A database's image with two rows, and the change it is at.
    fn two_rows() -> (Vec<u8>, u64) {
        let mut db = Database::new();
        for q in [
            "create collection c (x int)",
            "put c {x: 1}",
            "put c {x: 2}",
        ] {
            db.execute(&fennec_ql::parse_one(q).unwrap()).unwrap();
        }
        (db.snapshot(), db.change_seq())
    }

    /// A stand-in primary for one connection on a port of its own: the
    /// request read to the end of its head, `answer` written, then `then`
    /// handed the connection until it returns.
    fn primary(answer: Vec<u8>, then: impl FnOnce(&mut TcpStream) + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
            }
            if s.write_all(&answer).is_ok() {
                then(&mut s);
            }
        });
        url
    }

    /// Holds a connection until `done`.
    fn held(done: Arc<AtomicBool>) -> impl FnOnce(&mut TcpStream) + Send + 'static {
        move |_: &mut TcpStream| {
            while !done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// Set as it drops: what a stand-in's loop stops on, however the test
    /// ends.
    struct Done(Arc<AtomicBool>);

    impl Drop for Done {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// An empty database made to follow, and a follower of `url` for it.
    fn copy_of(url: &str) -> (Arc<RwLock<Database>>, Arc<Follower>) {
        let mut db = Database::new();
        let lineage = db.history().lineage.clone();
        db.follow(lineage).unwrap();
        let db = Arc::new(RwLock::new(db));
        let f = Follower::new(url, "t".into(), Arc::clone(&db), None, false).unwrap();
        (db, f)
    }

    /// Whether `f`'s thread ended within `within`, waited for as the event
    /// its end is ([`Ends`]).
    fn ended_within(f: &Follower, within: Duration) -> bool {
        let g = lock(&f.ended.0);
        let (g, _) = f
            .ended
            .1
            .wait_timeout_while(g, within, |ended| !*ended)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }

    /// A halt as an image has arrived, or once it is loaded, is walked away
    /// from before the copy changes: the copy holds what it held, and the
    /// thread ends without the check, the load or the adoption it had left.
    /// Each look is held here by a halt come at that moment; without it the
    /// follower went on to adopt the image, and a promotion, a release
    /// under the tenant's slot and a node's shutdown waited for a check, a
    /// load and a rewrite of the whole image.
    #[test]
    fn a_halt_as_an_image_arrives_or_is_loaded_takes_none_of_it() {
        for at in ["an image arrived", "an image loaded"] {
            let (image, seq) = two_rows();
            let done = Arc::new(AtomicBool::new(false));
            let _done = Done(Arc::clone(&done));
            let url = primary(answer(true, seq, &framed(b'I', &image)), held(done));
            let (db, f) = copy_of(&url);
            *lock(&f.halt_at) = Some(at);
            f.start("fennec-test-halt-look".into()).unwrap();
            let ended = ended_within(&f, SILENCE);
            assert!(
                ended && f.images() == 0,
                "{at}: the follower took the image after its halt (ended {ended}, images {})",
                f.images()
            );
            assert_eq!(db.read().unwrap().change_seq(), 0, "{at}");
            f.halt();
        }
    }

    /// A halt whose cut the read in flight misses -- on Windows, one begun
    /// after the cut -- ends at the primary's next byte, the read looking at
    /// the halt before each read of the socket ([`Watched`]): a primary
    /// sending a message a byte at a time, each within `SILENCE`, kept that
    /// read and the halt going for as long as it liked. The halt here is
    /// the flag alone, come after the look between messages, so the read
    /// in the message is all that can meet it, on every platform; without
    /// `Watched`'s look the follower reads on and this fails at its bound.
    #[test]
    fn a_halt_a_read_in_a_message_missed_ends_at_the_next_byte() {
        let done = Arc::new(AtomicBool::new(false));
        let _done = Done(Arc::clone(&done));
        let mut then = vec![b'W'];
        then.extend_from_slice(&(MESSAGE as u64).to_le_bytes());
        then.push(0);
        let dribble = Arc::clone(&done);
        let url = primary(answer(false, 0, &then), move |s| {
            while !dribble.load(Ordering::SeqCst) && s.write_all(&[0]).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (_db, f) = copy_of(&url);
        *lock(&f.halt_at) = Some("in a message");
        f.start("fennec-test-halt-in-a-message".into()).unwrap();
        assert!(
            ended_within(&f, SILENCE),
            "the follower read on past its halt, the primary sending a byte every 20 ms"
        );
        f.halt();
    }

    /// A file's sink in memory, as the fuzzer's `stream` target makes one:
    /// a rewrite's bytes kept and handed back as the file mapped.
    struct Remapped(Option<fennec_core::store::Base>);

    impl Sink for Remapped {
        fn append(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
            Ok(())
        }

        fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
            self.0 = Some(Arc::new(bytes.to_vec()));
            Ok(())
        }

        fn remapped(&mut self) -> Option<fennec_core::store::Base> {
            self.0.take()
        }
    }

    /// The `stream` target's reader, given a copy mapped as a server's, takes
    /// an image its hello promised as a follower does: adopted, the copy's
    /// stores pointed at the rewritten bytes, following -- `adopt`'s
    /// `repoint` and `rebind`, which no target reached before. An image of
    /// records without sums, as a primary sends one to a follower that does
    /// not ask for them, is adopted too: the target's seed of one, whose
    /// mutations pass `check_records` and reach the adoption.
    #[test]
    fn an_answers_image_is_adopted_by_a_mapped_copy() {
        let (image, seq) = two_rows();
        let plain = fennec_core::engine::plain_image(&image).unwrap();
        assert_ne!(plain, image);
        for image in [image, plain] {
            let bytes = answer(true, seq, &framed(b'I', &image));
            let mut copy = Database::with_sink(Box::new(Remapped(None)));
            let empty: fennec_core::store::Base = Arc::new(Database::new().snapshot());
            copy.load_mapped(empty).unwrap();
            let (n, end) = read_answer(&mut &bytes[..], Some(&mut copy));
            assert_eq!((n, end.kind()), (2, io::ErrorKind::UnexpectedEof), "{end}");
            assert_eq!((copy.change_seq(), copy.adoptions()), (seq, 1));
            assert!(copy.history().following);
            let q = fennec_ql::parse_one("get c count").unwrap();
            let r = fennec_core::json::response_to_string(&copy.query(&q, &[]).unwrap());
            assert!(r.contains(r#""count":2"#), "{r}");
        }
    }
}
