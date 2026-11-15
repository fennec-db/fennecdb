//! Subscription endpoint: `GET /<name>/changes` -- Server-Sent Events.
//!
//! **Why SSE and not WebSocket.** The stream is one way: the server pushes
//! changes, and the client already does its writes with ordinary
//! `POST`/`PATCH`/`DELETE`. For that, SSE is plain HTTP -- no handshake, no
//! framing, just a long-running response as far as proxies and intermediate
//! servers are concerned. It costs ~150 lines; WebSocket would need framing,
//! masking and ping/pong for the same job.
//!
//! **Why `fetch` rather than `EventSource`.** The browser's `EventSource`
//! cannot send headers, so it cannot carry `Authorization: Bearer`. The
//! client (`web/http.js`) therefore reads the stream with `fetch` +
//! `ReadableStream`; the wire format is still standard SSE and can be
//! watched with `curl`.
//!
//! ## Wire format
//! ```text
//! event: seed
//! data: {"seq":42,"rows":[{...}]}
//!
//! event: change
//! data: {"seq":43,"puts":[{...}],"dels":[7],"schema":false}
//!
//! : keepalive
//! ```
//!
//! `seed` means "this is the whole shape": the client replaces its local
//! copy. `change` is an incremental diff and **carries state**, not an
//! operation (see `fennec_core::changes`): re-applying it is harmless.

use crate::access::{Mask, Scope, Who};
use crate::api;
use crate::http::Request;
use crate::Config;
use fennec_core::changes::Hold;
use fennec_core::engine::{Lists, Written};
use fennec_core::prelude::*;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

/// The point that announces writes to waiting subscribers.
///
/// `fennec-core` knows no waiting primitive (there are no threads in WASM);
/// [`Watcher`] only says "the counter reached this point". The Condvar lives
/// here, on the server side.
///
/// Condvar instead of polling is about latency, not scale: a 50 ms polling
/// loop takes 20 read locks per second per subscriber and still adds 50 ms
/// of delay. Condvar zeroes both; a subscriber only wakes on a real write or
/// on the keep-alive timeout.
#[derive(Default)]
pub struct Hub {
    seq: Mutex<u64>,
    cv: Condvar,
    /// Number of live streams. Counted apart from normal requests:
    /// subscriptions are long lived, and sharing a single ceiling would let
    /// 100 subscribers close the server to ordinary requests.
    live: AtomicUsize,
    /// Set when the database behind the hub is going away (a tenant being
    /// deleted or moved). The open streams hold the database alive; they
    /// have to be told to let go, or a moved tenant's old copy would keep
    /// answering from a file that is no longer authoritative.
    closed: AtomicBool,
    /// Writes held until they can write (`Fennec-Wait`): woken here too.
    waits: crate::waits::Waits,
    /// Requests waiting here for a write their client made elsewhere
    /// (`Fennec-After`, [`Hub::reached`]): what a test waits on before it
    /// lets that write come.
    after_waiting: AtomicUsize,
}

impl Watcher for Hub {
    /// Set, not raised: a replica that takes an image can land on a lower
    /// change than it held, and a mark left above it would wake every
    /// stream at once, forever. Notifications come under the database's
    /// write lock, so they arrive in the order the counter moved.
    fn notify(&self, seq: u64) {
        self.mark(seq);
        // A load, a compact, a maintenance: whatever a held write reads.
        self.waits.wrote(None);
    }

    /// A block landed: held writes are woken by the collections it wrote
    /// alone, asked of the change ring only when some are held.
    fn landed(&self, db: &Database, before: u64) {
        let seq = db.change_seq();
        self.mark(seq);
        // On the committing thread: the request it answers asks, where its
        // lease lapsed before the answer, what it landed.
        crate::metrics::landed(seq);
        if self.waits.holds() {
            self.waits
                .wrote(db.changed_collections_since(before).as_deref());
        }
    }
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub::default())
    }

    /// The counter moved to `seq`: every stream looks.
    fn mark(&self, seq: u64) {
        let mut g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        *g = seq;
        self.cv.notify_all();
    }

    /// Wakes every stream waiting here to look again, the counter where it
    /// is: the one shutdown ending them ([`crate::conn::end_all`]).
    fn nudge(&self) {
        let _g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
    }

    /// Waits until it sees a counter greater than `after`. On timeout it
    /// returns as is -- the caller writes the keep-alive line.
    fn wait(&self, after: u64, timeout: Duration) -> u64 {
        let g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        if *g > after || self.is_closed() || crate::conn::ending() {
            return *g;
        }
        let (g, _) = self
            .cv
            .wait_timeout(g, timeout)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }

    /// Waits until `seq` says the database holds write `n`, or `until`:
    /// whether it does. A replica's writes come through `notify` as they
    /// are applied, so a read sent with `Fennec-After` waits for the write
    /// its client made on the primary.
    /// `look` says whether to stop, and the change the database is at.
    pub(crate) fn reached(
        &self,
        look: impl Fn() -> (bool, u64),
        until: std::time::Instant,
    ) -> bool {
        let mut waiting = None;
        loop {
            let (done, seq) = look();
            if done {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= until || self.is_closed() {
                return false;
            }
            waiting.get_or_insert_with(|| Counted::on(&self.after_waiting));
            // Past the change it looked at, or a tenth of a second: an
            // image adopted at the same change moves no counter.
            self.wait(seq, (until - now).min(Duration::from_millis(100)));
        }
    }

    /// How many requests wait here now for a write they were sent after
    /// (`Fennec-After`) -- reads, subscriptions and `/_changes` alike.
    pub fn after_waiting(&self) -> usize {
        self.after_waiting.load(Ordering::SeqCst)
    }

    /// Ends every stream on this hub: each one writes an `error` event and
    /// closes, and a client reconnecting lands wherever the tenant is now.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
        self.waits.close();
    }

    /// Takes subscriptions again after a `close` whose reason fell through:
    /// a tenant delete that gave up with 409 keeps the tenant.
    pub fn reopen(&self) {
        self.closed.store(false, Ordering::SeqCst);
        self.waits.reopen();
    }

    /// The writes held here until they can write.
    pub fn waits(&self) -> &crate::waits::Waits {
        &self.waits
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Reserves a stream slot; `None` when the ceiling is full.
    fn reserve(&self, max: usize) -> Option<StreamSlot<'_>> {
        let n = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        if max > 0 && n > max {
            self.live.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(StreamSlot(self))
    }
}

/// One more on a count for as long as it lives.
struct Counted<'a>(&'a AtomicUsize);

impl<'a> Counted<'a> {
    fn on(count: &'a AtomicUsize) -> Counted<'a> {
        count.fetch_add(1, Ordering::SeqCst);
        Counted(count)
    }
}

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct StreamSlot<'a>(&'a Hub);
impl Drop for StreamSlot<'_> {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Serves a subscription request and **consumes the connection**.
///
/// It is separate from the normal response path because of
/// `Content-Length`: an ordinary response knows its body and writes it in
/// one go, while a stream is a body with no end.
pub fn serve(
    out: &mut &crate::conn::Conn,
    db: &Arc<RwLock<Database>>,
    cfg: &Config,
    hub: &Arc<Hub>,
    req: &Request,
    who: &Who,
) {
    // Said as every 503 says it, to come back in a second: the sync reads
    // the wait from the body, where plain text left it at its own backoff.
    let Some(_slot) = hub.reserve(cfg.max_streams) else {
        let refused = crate::http::Response::retry_after(503, "too many subscriptions", 1);
        let _ = crate::cors(refused, cfg).write(out, false, false);
        return;
    };

    // Sent after a write its client made elsewhere (`Fennec-After`), a
    // stream waits for it here as a read does ([`crate::after`]): served
    // from a copy that has not caught up, its seed was from before the
    // client's own write -- a read replica a router sent it to, or one
    // left on the branch a failover abandoned. After its token and its
    // slots, so a refused one waits for nothing and one past the ceiling
    // is told so at once; before the shape is read, since a collection
    // the client made may not be here yet; before the head, so a 504
    // says where the copy stands and a router sends the stream home. A
    // stream resumed with `since` waits the same way, the header being
    // the client's position and `since` only its cursor.
    if req.header("fennec-after").is_some() {
        // The stream's own slot, taken above, holds the wait.
        if let Some(refused) = crate::after(req, db, hub, || Ok(None)) {
            let _ = crate::cors(refused, cfg).write(out, false, false);
            return;
        }
    }

    // Parsing needs the schema, and the schema needs a read lock. The lock is
    // released immediately: held for the whole stream it would stop all writes.
    //
    // A scoped stream of a shape that holds nothing -- `where=false`, which
    // `FennecHttp.live` opens to learn that what its query reads was
    // written -- is told of the writes to rows its token may read, and of
    // nothing else: it follows the token's own filter, its ids kept here and
    // never sent, and a write to one of them, or one of them leaving, is a
    // change holding no rows. ANDed with the token's filter, the shape
    // matched no row, a scoped stream hears only of rows it was sent, and
    // such a live query never ran again; told of every write, it would hand
    // the token the moments of other users' writes.
    //
    // A token whose read rules name fields is held to them as a read is
    // (`Scope::shaped`): the shape's terms over a field a row may not show
    // asked only where it does, and each row sent with the fields of the
    // rules admitting it (`mask`).
    //
    // A token whose rules read other collections has them bound under each
    // look's lock ([`Reach`]), from the shape as the subscriber sent it;
    // an inner `get` of the shape, the token's or anyone's, is answered
    // there too, as the list it then is.
    //
    // Where the database stands is read under the same lock, for the head
    // (`Fennec-Seq`, `Fennec-History`): a client running reads of its own
    // after the changes the stream names -- near, on a read replica, while
    // the stream stays home -- sends them after that history, which a seq
    // alone does not name across a failover. Read here, it costs the
    // stream no lock of its own.
    let (sub, at) = {
        let guard = crate::held::read(db);
        let at = crate::At::of(&guard);
        let sub = api::subscription(&guard, req).and_then(|mut sub| {
            let mut quiet = false;
            let mut view = None;
            let mut own = None;
            if let Who::Scoped(scope) = who {
                quiet = matches!(sub.filter, Some(Expr::Lit(Value::Bool(false))));
                let shape = if quiet { None } else { sub.filter.take() };
                if quiet {
                    sub.project = Some(vec!["id".to_string()]);
                }
                if who.reaches_out() {
                    own = Some((shape.clone(), sub.project.clone()));
                }
                let (shape, mask) = scope.shaped(&sub.collection, shape, &mut sub.project)?;
                view = Some(View {
                    scope: Arc::clone(scope),
                    mask,
                });
                sub.filter = scope.subscribed(&sub.collection, shape)?;
            }
            let reach = Reach::of(who, &sub, own);
            Ok((sub, quiet, view, reach))
        });
        (sub, at)
    };
    let (mut sub, quiet, mut view, mut reach) = match sub {
        Ok(s) => s,
        Err(e) => {
            // Over a collection the token is granted nothing on, the shape
            // read against its schema told nothing of it.
            let e = crate::path_refusal(who, req).unwrap_or(e);
            // The answer `/query` gives the same refusal, whole: the
            // message alone in the body, its class in the status, and
            // every header it carries -- `Retry-After`, a quota's
            // `Fennec-Quota`. `Display`'s `denied: ` and `query error: `
            // made a stream's 403 and 400 read apart from every other
            // route's, and a head of its own told only the wait.
            let _ = crate::cors(crate::error_response(&e), cfg).write(out, false, false);
            return;
        }
    };

    // The stream is one gzip member where the client takes gzip, each event
    // a piece of it its client reads whole as it comes.
    let gz = cfg.gzip.filter(|_| crate::gzip::wanted(req, cfg));
    let Ok(stream) = write_head(out, cfg, at, gz.is_some()) else {
        return;
    };
    // A client stalled during the stream must not hold the thread forever:
    // the write timeout returns an error and the loop exits.
    let _ = out.set_write_timeout(Some(cfg.stream_write_timeout));
    let conn: &crate::conn::Conn = out;
    let out = &mut Events {
        conn,
        gz: stream.zip(gz),
        broken: false,
    };

    // A scoped subscriber is told of a row leaving only if it was sent the
    // row. The shape's rule -- a changed id that does not match is a
    // deletion -- would otherwise hand every user the ids everyone else
    // writes. It costs the ids of the rows sent; an unscoped subscription
    // keeps the rule and holds nothing -- but one whose shape reads another
    // collection ([`Reach`]), whose rows come and go with no write to them
    // ([`enter_and_leave`]), which only the rows it was sent can tell.
    //
    // Masked, each id keeps a digest of the row as it was sent, and a row
    // written again that shows the same is not sent: a write to a field
    // the token does not see would otherwise tell it when that field
    // changed, which a scoped poll's content tag (`same_answer`) keeps
    // from it as well.
    let mut seen: Option<Seen> = (who.scope().is_some() || reach.is_some()).then(|| Seen {
        ids: Default::default(),
        digests: shows(&view).is_some(),
    });
    // A stream of a shape that holds nothing, unscoped, is told of every
    // write to the collection, and so of every row reaching its time.
    let everything = seen.is_none() && matches!(sub.filter, Some(Expr::Lit(Value::Bool(false))));
    // The images the database had adopted where the stream stands
    // (`Database::adoptions`): one adopted since replaced what it was
    // sent, possibly at the very change its cursor names, where the ring
    // shows nothing, and it is seeded again.
    let (mut expiry, mut adopted) = {
        let guard = crate::held::read(db);
        let expiry = Expiry::of(&guard, &sub.collection, everything);
        (expiry, guard.adoptions())
    };
    let began = now_ms() as i64;
    // The change ring held back to the seed's change until the first look
    // after it ([`seed`]), and to a pinned look's until the next ([`look`]).
    let mut held = None;
    // The stream's long `in` lists as its last look bound them -- an inner
    // `get`'s answer, kept from one look to the next ([`Lists`]) -- where
    // its filter reads another collection ([`Reach`]) and holds one, made
    // as its seed answers them. A plain stream's look evaluates its filter
    // a row at a time, a long list written in too, as it did.
    let mut lists = None;
    let mut cursor = match sub.since {
        // Resuming, a scoped subscriber is seeded all the same: the rows it
        // holds are not known here, and without them no deletion could be
        // told apart from someone else's write. So is one whose shape reads
        // another collection: nor is what that held at its cursor.
        Some(n) if seen.is_none() => n,
        _ => match seed(
            out,
            db,
            (&mut sub, &mut reach, &mut view, &mut lists),
            &mut seen,
            quiet,
        ) {
            Ok((seq, hold, images)) => {
                (held, adopted) = (Some(hold), images);
                // The seed left out every row past its time by then.
                if let Some(x) = &mut expiry {
                    x.told = Some(began);
                }
                seq
            }
            Err(_) => return,
        },
    };
    // The last write to the client, which the keep-alive is counted from,
    // and whether a round wrote since: stamped as the round waits, the
    // clock read once a round.
    let (mut wrote, mut sent) = (Instant::now(), false);

    // A token is checked as the stream opens, and a stream is held for as
    // long as its client stays: past the token's `exp` it is ended, as the
    // token's next request would be refused. Kept open, it went on
    // delivering what the token could no longer read -- a member taken
    // off a team heard the team until the client went away. The `error`
    // event says 401 and `token_expired`, which a client takes as it takes
    // a request refused for its token's expiry: a fresh token, then the
    // stream again.
    let expires = who.scope().and_then(|s| s.expires_ms());
    let left = || expires.map(|at| Duration::from_millis(at.saturating_sub(now_ms())));
    // The one shutdown wakes the stream to end it, its end over TLS a
    // `close_notify` its client reads as the end it is.
    conn.on_ending({
        let hub = Arc::clone(hub);
        move || hub.nudge()
    });

    loop {
        if crate::conn::ending() {
            return;
        }
        if left().is_some_and(|l| l.is_zero()) {
            let _ = event(out, "error", EXPIRED);
            return;
        }
        if hub.is_closed() {
            let _ = event(
                out,
                "error",
                "{\"error\":\"the tenant was closed on this node\"}",
            );
            return;
        }
        let looked = look(
            db,
            (cursor, adopted),
            (&mut sub, &mut reach, &mut view, &mut lists),
        );
        // Looked past the seed's change: the ring goes back to its
        // capacity, or to a pinned look's change, held till the next.
        drop(held.take());
        let (step, members) = match looked {
            Ok(l) => {
                held = l.hold;
                (Ok(l.step), l.members)
            }
            Err(e) => (Err(e), None),
        };
        match step {
            Err(e) => {
                // The collection may have been dropped: write the reason and
                // close rather than cutting off silently.
                let _ = event(out, "error", &error_json(&e));
                return;
            }
            Ok(Changes::Reseed) => {
                let began = now_ms() as i64;
                match seed(
                    out,
                    db,
                    (&mut sub, &mut reach, &mut view, &mut lists),
                    &mut seen,
                    quiet,
                ) {
                    Ok((seq, hold, images)) => (cursor, held, adopted) = (seq, Some(hold), images),
                    Err(_) => return,
                }
                sent = true;
                if let Some(x) = &mut expiry {
                    x.told = Some(began);
                    x.stale = true;
                }
            }
            Ok(Changes::Batch(mut b)) => {
                // A write to the collection may have moved its next moment.
                if !(b.puts.rows.is_empty() && b.dels.is_empty()) {
                    if let Some(x) = &mut expiry {
                        x.stale = true;
                    }
                }
                if let (Some(now), Some(seen)) = (members, &seen) {
                    enter_and_leave(&mut b, now, seen);
                }
                if let Some(seen) = &mut seen {
                    b.dels.retain(|id| seen.left(*id));
                    b.puts.rows.retain(|r| seen.sent(r));
                }
                // The cursor advances every round, even when the batch is
                // empty. The counter and the ring are shared across all
                // collections: if a subscriber of a quiet collection never
                // moved its cursor, it would be reseeded as soon as other
                // people's writes overflowed the ring. An empty batch is not
                // sent, but the cursor is current.
                let empty = b.puts.rows.is_empty() && b.dels.is_empty() && !b.schema_changed;
                if quiet {
                    b.puts.rows.clear();
                    b.dels.clear();
                }
                if !empty {
                    if event(out, "change", &change_json(&b)).is_err() {
                        return;
                    }
                    sent = true;
                }
                cursor = b.seq;
            }
        }

        // The shape's rows past their time since the last look, told as
        // deletions now, with no write to tell of them; then when the next
        // comes.
        let mut until = None;
        if let Some(x) = &mut expiry {
            let guard = crate::held::read(db);
            if x.due(now_ms() as i64) {
                match x.step(&guard, sub.filter.as_ref()) {
                    Err(e) => {
                        drop(guard);
                        let _ = event(out, "error", &error_json(&e));
                        return;
                    }
                    Ok(mut dels) => {
                        if let Some(seen) = &mut seen {
                            dels.retain(|id| seen.left(*id));
                        }
                        if !dels.is_empty() {
                            drop(guard);
                            // A stream of a shape that holds nothing is told
                            // a change happened, and not which rows.
                            if quiet {
                                dels.clear();
                            }
                            if event(out, "change", &expired_json(cursor, &dels)).is_err() {
                                return;
                            }
                            sent = true;
                            continue;
                        }
                    }
                }
            }
            until = x.next_at(&guard);
        }
        // A row of a collection the inner `get`s read reaching its time
        // leaves their lists with no write: the stream looks then.
        if let Some(at) = reach.as_ref().and_then(|r| r.next) {
            until = Some(until.map_or(at, |u| u.min(at)));
        }

        // Keep-alive: proxies and NAT tables drop silent connections, and a
        // write is how a stream learns its client went. A comment line --
        // valid in SSE, no event on the client's side -- once
        // `stream_keepalive` passed with nothing written, whatever woke the
        // stream. Written only where a wait ran its whole length, a stream
        // woken sooner each time -- by a row of its collection, or of one
        // its inner `get`s read, reaching its time, by lists reading the
        // time each second, by writes to other collections -- wrote none:
        // its proxy cut it, and one whose client had gone held its
        // `--max-streams` slot till its next change.
        let now = Instant::now();
        if !sent && now.saturating_duration_since(wrote) >= cfg.stream_keepalive {
            let alive = out.send(b": keepalive\n\n");
            if alive.is_err() {
                return;
            }
            sent = true;
        }
        if std::mem::take(&mut sent) {
            wrote = now;
        }
        let idle = now.saturating_duration_since(wrote);
        let mut wait = cfg.stream_keepalive.saturating_sub(idle);
        if let Some(l) = left() {
            wait = wait.min(l);
        }
        if let Some(at) = until {
            let left = at.saturating_sub(now_ms() as i64).max(1) as u64;
            wait = wait.min(Duration::from_millis(left));
        }
        hub.wait(cursor, wait);
    }
}

/// A subscription's rows past their `@ttl`, told at their time. A row past
/// its time leaves every read at once and the file only at the sweep, a
/// minute and its grace later, which is when a subscriber heard of it: a
/// courier whose phone went quiet stayed on a dispatcher's map 73 s after
/// it had gone from every read. So the stream works out when the next row
/// of its collection reaches its time ([`Database::wakes_at`], a step of
/// the field's index), sleeps until then if no write comes first, and
/// reads the rows that reached it since it last looked -- `expired()` and
/// the field's window, the shape ANDed in -- telling each as a deletion,
/// as the sweep's delete would be told: a scoped stream of the rows it was
/// sent alone, a shape that holds nothing (`FennecHttp.live`'s) that
/// something changed. The sweep's delete of the row comes after, a
/// deletion of what is gone already, which every client takes as it is.
struct Expiry {
    collection: String,
    field: String,
    ttl: i64,
    /// Every row of the collection's, not only the shape's: an unscoped
    /// stream of a shape that holds nothing, told of any write to the
    /// collection.
    everything: bool,
    /// Rows past their time by this moment, ms, have been told. `None` on a
    /// resumed stream, which tells every row past its time not yet swept.
    told: Option<i64>,
    /// The next moment a row of the collection reaches its time.
    next: Option<i64>,
    /// `next` is to be worked out again: a write to the collection, or a
    /// look, may have moved it.
    stale: bool,
}

impl Expiry {
    /// What a stream of `collection` tells of its rows reaching their time,
    /// if they do.
    fn of(db: &Database, collection: &str, everything: bool) -> Option<Expiry> {
        let c = db.collection(collection).ok()?;
        let (field, ttl) = c
            .schema
            .fields
            .iter()
            .find_map(|f| f.index.ttl().map(|t| (f.name.clone(), t)))?;
        Some(Expiry {
            collection: collection.to_string(),
            field,
            ttl: ttl.min(i64::MAX as u64) as i64,
            everything,
            told: None,
            next: None,
            stale: true,
        })
    }

    fn due(&self, now: i64) -> bool {
        self.told.is_none() || self.next.is_some_and(|at| at <= now)
    }

    /// The next moment a row reaches its time, worked out again where a
    /// write or a look may have moved it.
    fn next_at(&mut self, db: &Database) -> Option<i64> {
        if self.stale {
            let at = self.told.unwrap_or_else(|| now_ms() as i64);
            self.next = db.wakes_at(&self.collection, None, &[], at).ok().flatten();
            self.stale = false;
        }
        self.next
    }

    /// The rows of `shape` that reached their time since the last look.
    fn step(
        &mut self,
        db: &Database,
        shape: Option<&Expr>,
    ) -> fennec_core::error::Result<Vec<DocId>> {
        let now = now_ms() as i64;
        let field = || Box::new(Expr::Field(self.field.clone()));
        let past = |t: i64| Box::new(Expr::Lit(Value::Timestamp(t.saturating_sub(self.ttl))));
        let mut f = Expr::And(
            Box::new(Expr::Call("expired".into(), Vec::new())),
            Box::new(Expr::Cmp(CmpOp::Le, field(), past(now))),
        );
        if let Some(told) = self.told {
            f = Expr::And(
                Box::new(f),
                Box::new(Expr::Cmp(CmpOp::Gt, field(), past(told))),
            );
        }
        // The window first: the shape -- a token's rules, then its own
        // terms -- runs only over the rows that reached their time since
        // the last look, each one the stream could read then, and never
        // over one long expired and not yet swept, which a term of the
        // shape failing by its fields would tell without the `expired`
        // grant (access.md, *The rules are asked before the caller's
        // terms*).
        if let (false, Some(shape)) = (self.everything, shape) {
            f = Expr::And(Box::new(f), Box::new(shape.clone()));
        }
        let sel = Select {
            collection: self.collection.clone(),
            project: Some(vec!["id".to_string()]),
            filter: Some(f),
            ..Select::default()
        };
        let ids = match db.query(&Statement::Select(sel), &[])? {
            Response::Rows(rs) => rs.rows.iter().map(|r| r.id).collect(),
            _ => Vec::new(),
        };
        self.told = Some(now);
        self.stale = true;
        Ok(ids)
    }
}

/// What a scoped stream's rows are shown through: its token's scope -- as
/// bound, where its rules read other collections ([`Reach`]) -- and the
/// mask its shape took ([`Scope::shaped`]).
struct View {
    scope: Arc<Scope>,
    mask: Option<Mask>,
}

/// The scope and mask a stream's rows are shown through, where a mask is.
fn shows(view: &Option<View>) -> Option<(&Scope, &Mask)> {
    let v = view.as_ref()?;
    Some((&*v.scope, v.mask.as_ref()?))
}

/// A stream whose filter reads other collections: through the token's
/// rules of its collection ([`crate::access::Scope::bound`]), through an
/// inner `get` of the subscriber's shape (`where=msg in (get notes select
/// title)`), or through a rule ANDed into that `get`. Its filter is the
/// rules and the inner `get`s as they answer -- each `in (get ...)` the
/// `in` list it is ([`Database::answer_subqueries`]), where `eval`, which
/// judges each changed row, refuses one: such a stream sent its seed and
/// ended at the first write to its collection. Made again at a look
/// ([`look`]) where one of those collections was written since the
/// stream's change, where a row of one reached its time since they were
/// last answered, or where an inner `get` reads the time, once
/// [`CLOCK_EVERY`] passed since the read answering them ended -- the
/// stream wakes for either
/// ([`Reach::next`]); and the shape run again where the answer moved, its
/// rows diffed by id against those the stream was sent
/// ([`enter_and_leave`]): a row the write to the other collection let in
/// enters, one it shut out leaves, a deletion of a row the stream was
/// sent, its own collection unwritten. A customer following the courier
/// carrying their order hears the courier once the order is claimed and
/// nothing of them once it is delivered, with no write to `positions`
/// between; a board following `msg in (get notes select title)` hears the
/// message a note's title names once the note is written and its deletion
/// once the note is gone. The cost is an inner `get` a stream at each
/// write to what it reads -- answered on a pin where a query of it would
/// be, beside the writers -- and the shape run again where its list moved:
/// the diff alone sent, where a reseed would send every row the shape
/// holds at each such write. A stream whose filter reads no other
/// collection has none, and runs nothing more at a look.
struct Reach {
    /// The token's scope, where a rule of it reads another collection:
    /// bound again at each answer.
    rules: Option<Arc<Scope>>,
    collection: String,
    /// Under `rules`, the subscriber's own filter and list, which the
    /// rules are held to; else the stream's filter as its token holds it
    /// ([`Scope::subscribed`]), its inner `get`s unanswered, and its list.
    shape: Option<Expr>,
    project: Option<Vec<String>>,
    /// The collections the inner `get`s read, at any depth.
    reads: Vec<String>,
    /// An inner `get` reads the time: its filter, an `order` key, an item
    /// of its list (`access::reads_time`).
    clock: bool,
    /// When the lists are due by the time, worked out as they were last
    /// answered ([`Reach::next_after`]): the next moment a row of `reads`
    /// reaches its `@ttl`, one leaving them with no write, or where they
    /// read the time, [`CLOCK_EVERY`] after the read answering them ended.
    /// Asked of the collections then
    /// (`Database::wakes_at`), so a `@ttl` given one later -- a write to
    /// it, which answers them again -- is followed.
    next: Option<i64>,
}

/// How long lists an inner `get` reading the time makes are taken to hold,
/// ms: such a stream answers them again once this passed since the read
/// answering them ended, waking for it, as it does at a write to what they
/// read. Answered again at every look, they were answered at every write
/// anywhere, which wakes every stream: an inner `get` a stream a write,
/// whatever the write moved. Counted from when the read began -- the lock's
/// time, before the answer -- an inner `get` taking a second was due again
/// as it ended, and an idle stream answered it back to back, its thread
/// never let go.
const CLOCK_EVERY: i64 = 1_000;

/// A stream's filter, list and view as [`Reach::rules`] binds them.
type Answered = (Option<Expr>, Option<Vec<String>>, Option<View>);

impl Reach {
    /// What `sub`, its filter as its token holds it, reads beside its
    /// collection; `own` the subscriber's shape and list where the token's
    /// rules read other collections. `None` where its filter holds no
    /// inner `get`.
    fn of(
        who: &Who,
        sub: &api::Subscription,
        own: Option<(Option<Expr>, Option<Vec<String>>)>,
    ) -> Option<Reach> {
        let f = sub.filter.as_ref().filter(|f| f.has_subquery())?;
        let (reads, clock) = crate::access::inner_reads(f);
        let (rules, (shape, project)) = match (who, own) {
            (Who::Scoped(scope), Some(own)) => (Some(Arc::clone(scope)), own),
            _ => (None, (sub.filter.clone(), sub.project.clone())),
        };
        Some(Reach {
            rules,
            collection: sub.collection.clone(),
            shape,
            project,
            reads,
            clock,
            next: None,
        })
    }

    /// The stream's filter, list and view as the rules bind them over
    /// `db` now, the inner `get`s left in it for the caller to answer --
    /// on a pin, or under this lock where the pin declines. No view where
    /// no rule is bound here, the stream's own standing.
    fn rules(&self, db: &Database) -> fennec_core::error::Result<Answered> {
        let Some(scope) = &self.rules else {
            return Ok((self.shape.clone(), self.project.clone(), None));
        };
        // The rules of the collections the inner `get`s read answered as a
        // statement's are ([`Scope::bound`]): a rule's lists, held to
        // `MAX_CLAIM_VALUES`, under the lock as a statement's.
        let mut over = vec![self.collection.as_str()];
        over.extend(self.reads.iter().map(String::as_str));
        let bound = Arc::new(scope.bound_over(db, &over)?);
        let mut project = self.project.clone();
        let (shape, mask) = bound.shaped(&self.collection, self.shape.clone(), &mut project)?;
        let filter = bound.subscribed(&self.collection, shape)?;
        Ok((filter, project, Some(View { scope: bound, mask })))
    }

    /// Whether the lists are to be answered again at a look after `since`:
    /// a collection they read was written, or they are due by the time
    /// ([`Reach::next`]).
    fn due(&self, db: &Database, since: u64, now: i64) -> bool {
        self.next.is_some_and(|at| at <= now)
            || db
                .changed_collections_since(since)
                .is_none_or(|w| w.iter().any(|c| self.reads.contains(c)))
    }

    /// When lists answered at `now`, by a read that ended at `ended`, are
    /// due by the time: the next moment a row of a collection they read
    /// reaches its time past `now` -- under the lock, a pin holding no
    /// index -- or [`CLOCK_EVERY`] after `ended` where they read the time.
    fn next_after(&self, db: &Database, now: i64, ended: i64) -> Option<i64> {
        self.due_at(self.reaches(db, now), ended)
    }

    /// The next moment a row of a collection the lists read reaches its
    /// time past `now`: asked under the lock, a pin holding no index.
    fn reaches(&self, db: &Database, now: i64) -> Option<i64> {
        (self.reads.iter())
            .filter_map(|c| db.wakes_at(c, None, &[], now).ok().flatten())
            .min()
    }

    /// `reaches`, or [`CLOCK_EVERY`] after `ended` where the lists read the
    /// time, whichever comes first.
    fn due_at(&self, reaches: Option<i64>, ended: i64) -> Option<i64> {
        (reaches.into_iter())
            .chain(self.clock.then_some(ended.saturating_add(CLOCK_EVERY)))
            .min()
    }
}

/// Lists a pin answered, kept for a lock taken after it to run the shape
/// by where they still hold ([`Kept::holds`]): the change and the images
/// they were answered at, the time, and when the pin's read ended. A
/// seed's pin that reached for an index keeps them for the lock to read
/// by, and a look's whose lists moved, for the lock to run the shape by.
struct Kept {
    at: u64,
    images: u64,
    now: i64,
    ended: i64,
    answered: Answered,
}

impl Kept {
    /// Whether the lists hold under `db` at `now` as they did answered:
    /// the same images, nothing they read written since, and no row of it
    /// reaching its time since they were answered -- nor, reading the
    /// time, [`CLOCK_EVERY`] passed since the pin's read ended. The time is
    /// theirs: worked out from when the lock found them, a note reaching
    /// its time between left them holding it with no wake.
    fn holds(&self, db: &Database, r: &Reach, now: i64) -> bool {
        db.adoptions() == self.images
            && r.next_after(db, self.now, self.ended)
                .is_none_or(|at| at > now)
            && (db.changed_collections_since(self.at))
                .is_some_and(|w| !w.iter().any(|c| r.reads.contains(c)))
    }
}

/// What a look read: the changes after the stream's cursor, the rows its
/// shape holds now where its filter moved, and the change ring held back
/// to a pinned look's change, till the next.
struct Looked {
    step: Changes,
    members: Option<ResultSet>,
    hold: Option<Hold>,
}

/// Where a look whose lists are due answers them ([`look`]).
#[derive(Clone, Copy, PartialEq)]
enum Answer {
    /// The inner `get`s pinned on their own where a query of them would
    /// be, the rows written since the cursor beside them
    /// ([`Database::pin_lists`]) -- all a look reads where the lists did
    /// not move -- else under the lock.
    Apart,
    /// The shape pinned whole, its inner `get`s and all: lists answered
    /// apart moved, and what they read was written before the lock could
    /// run the shape by them.
    Whole,
    /// Under the lock, all of it: a pin reached for an index.
    Lock,
}

/// A stream's look after `cursor`, the database still on the images it
/// had `adopted`. Where its filter reads another collection that was
/// written ([`Reach::due`]), the rules are bound under the lock and the
/// inner `get`s answered first, where a query of them would be: on a pin
/// of what they read ([`Database::pin_lists`]), beside the writers -- a
/// list read off a million rows held every writer for its scan otherwise
/// -- with the rows written since the cursor beside them, judged there as
/// they stood ([`Pinned::changes_held`]); else under the lock. Only where
/// the lists moved is the shape run by them: under a lock where they still
/// hold ([`Kept`]), pinned where a query of the shape would be, the ring
/// held back to the pin's change till the next look; or, written again
/// meanwhile, the whole shape pinned, lists and all. Pinned with the shape
/// whatever the lists did, a look copied the id index of a million rows
/// at every write to ten notes. Under the lock where a pin reached for an
/// index.
fn look(
    db: &Arc<RwLock<Database>>,
    (cursor, adopted): (u64, u64),
    (sub, reach, view, lists): (
        &mut api::Subscription,
        &mut Option<Reach>,
        &mut Option<View>,
        &mut Option<Lists>,
    ),
) -> fennec_core::error::Result<Looked> {
    let mut answer = Answer::Apart;
    let mut kept: Option<Kept> = None;
    // The shape reached for an index on a pin: the lock runs it.
    let mut shape_out = false;
    loop {
        let guard = crate::held::read(db);
        let looked = |step| Looked {
            step,
            members: None,
            hold: None,
        };
        if guard.adoptions() != adopted {
            return Ok(looked(Changes::Reseed));
        }
        // The time asked only of a stream that reads another collection:
        // a plain one's look reads what it read.
        let now = match reach {
            Some(_) => now_ms() as i64,
            None => 0,
        };
        let Some(r) = reach.as_mut().filter(|r| r.due(&guard, cursor, now)) else {
            let mut step = changes(&guard, cursor, sub, lists.as_mut())?;
            if let (Changes::Batch(b), Some((scope, mask))) = (&mut step, shows(view)) {
                scope.shown_rows(mask, &guard, &mut b.puts);
            }
            return Ok(looked(step));
        };
        let Some(written) = guard.written_after(&sub.collection, cursor)? else {
            return Ok(looked(Changes::Reseed));
        };
        // The lists kept where they hold under this lock, answered already;
        // else the rules bound, and the inner `get`s answered below.
        // `ended` when the read answering the lists ended, which a kept
        // copy of them is due by the time from.
        let (mut answered, at, mut ended, mut done) = match kept.take() {
            Some(k) if k.holds(&guard, r, now) => (k.answered, k.now, k.ended, true),
            _ => (r.rules(&guard)?, now, now, false),
        };
        let (reaches, seq) = (r.reaches(&guard, at), guard.change_seq());
        let mut pinned = match (done, answer, &answered.0) {
            (false, Answer::Apart, Some(f)) => guard.pin_lists(f, &sub.collection, &written),
            (false, Answer::Whole, _) => pin_shape(&guard, &sub.collection, &answered),
            _ => None,
        };
        if !(done || pinned.is_some()) {
            if let Some(Err(e)) = On::Lock(&guard).answer(&mut answered.0) {
                return Err(e);
            }
            (done, ended) = (true, now_ms() as i64);
        }
        // Answered at this lock's moment and moved, the shape runs by them,
        // pinned where a query of it would be.
        let shape = done;
        let mut moved = done && !same_answer(&sub.filter, &answered.0);
        if moved && answer != Answer::Lock && !shape_out {
            pinned = pin_shape(&guard, &sub.collection, &answered);
        }
        let hold = pinned.as_ref().map(|_| guard.hold_changes());
        let guard = match pinned {
            Some(_) => {
                drop(guard);
                None
            }
            None => Some(guard),
        };
        let on = match (&pinned, &guard) {
            (Some(p), _) => On::Pin(p),
            (None, Some(g)) => On::Lock(g),
            (None, None) => continue,
        };
        if !done {
            let Some(read) = on.answer(&mut answered.0) else {
                answer = Answer::Lock;
                continue;
            };
            read?;
            ended = now_ms() as i64;
            moved = !same_answer(&sub.filter, &answered.0);
            // Apart, the pin holds the shape's written rows alone: the
            // lists are kept for a lock to run the shape by.
            if moved && answer == Answer::Apart {
                kept = Some(Kept {
                    at: seq,
                    images: adopted,
                    now: at,
                    ended,
                    answered,
                });
                answer = Answer::Whole;
                continue;
            }
        }
        let Some(read) = judge(on, written, &mut answered, moved, (sub, view, lists)) else {
            // The shape reached for an index: run by the same lists under
            // the lock; else all of it is the lock's.
            match shape {
                true => {
                    shape_out = true;
                    kept = Some(Kept {
                        at: seq,
                        images: adopted,
                        now: at,
                        ended,
                        answered,
                    });
                }
                false => answer = Answer::Lock,
            }
            continue;
        };
        // Due by the time from when this look's read ended: from its
        // start, lists taking a second to answer were due as it ended.
        if read.is_ok() {
            r.next = r.due_at(reaches, now_ms() as i64);
        }
        return read.map(|l| Looked { hold, ..l });
    }
}

/// A pin of the stream's whole shape, its filter `answered` as the rules
/// bound it, the inner `get`s answered or not, where a query of it would
/// be pinned. Out of line: a look asks it in two places.
#[inline(never)]
fn pin_shape(db: &Database, collection: &str, answered: &Answered) -> Option<Pinned> {
    let stmt = Statement::Select(shape_select(collection, &answered.0, &answered.1));
    db.pin(&[(&stmt, &[][..])])
}

/// The changes after `cursor` by the stream's filter, its long lists bound
/// as its last look bound them where it holds one.
fn changes(
    db: &Database,
    cursor: u64,
    sub: &api::Subscription,
    lists: Option<&mut Lists>,
) -> fennec_core::error::Result<Changes> {
    let (filter, project) = (sub.filter.as_ref(), sub.project.as_deref());
    let Some(lists) = lists else {
        return db.changes_since(&sub.collection, cursor, filter, project, &[]);
    };
    match db.written_after(&sub.collection, cursor)? {
        Some(w) => db.changes_held(&sub.collection, w, filter, project, Some(lists)),
        None => Ok(Changes::Reseed),
    }
}

/// Where a look whose lists are due reads: the database under the lock,
/// or a pin taken under it, whose reads answer `None` where they reached
/// for an index it does not hold.
enum On<'a> {
    Lock(&'a Database),
    Pin(&'a Pinned),
}

impl On<'_> {
    fn database(&self) -> &Database {
        match self {
            On::Lock(db) => db,
            On::Pin(p) => p.database(),
        }
    }

    /// The inner `get`s of a filter answered as the lists they are.
    fn answer(&self, f: &mut Option<Expr>) -> Option<fennec_core::error::Result<()>> {
        let Some(f) = f else {
            return Some(Ok(()));
        };
        match self {
            On::Lock(db) => Some(db.answer_subqueries(f)),
            On::Pin(p) => p.answer_subqueries(f),
        }
    }

    fn changes(
        &self,
        collection: &str,
        written: Written,
        (filter, project): (Option<&Expr>, Option<&[String]>),
        lists: Option<&mut Lists>,
    ) -> Option<fennec_core::error::Result<Changes>> {
        match self {
            On::Lock(db) => Some(db.changes_held(collection, written, filter, project, lists)),
            On::Pin(p) => p.changes_held(collection, written, filter, project, lists),
        }
    }

    fn query(&self, stmt: &Statement) -> Option<fennec_core::error::Result<Response>> {
        match self {
            On::Lock(db) => Some(db.query(stmt, &[])),
            On::Pin(p) => p.query(stmt, &[]),
        }
    }
}

/// A due look read on `on`, the stream's filter's inner `get`s answered
/// (`answered`, as the rules bound it): the changes `written` names judged
/// by it, and where the lists `moved`, the rows the shape holds now -- the
/// stream's filter, list, view and lists moved to it once every read came
/// back. `None` where the pin reached for an index, the stream and
/// `answered` as they were.
fn judge(
    on: On,
    written: Written,
    answered: &mut Answered,
    moved: bool,
    (sub, view, lists): (
        &mut api::Subscription,
        &mut Option<View>,
        &mut Option<Lists>,
    ),
) -> Option<fennec_core::error::Result<Looked>> {
    let mut fresh = match moved {
        true => Lists::of(answered.0.as_ref()),
        false => None,
    };
    let (f, pr, l, vw) = match moved {
        true => (
            &answered.0,
            &answered.1,
            fresh.as_mut(),
            answered.2.as_ref().map_or(&*view, |_| &answered.2),
        ),
        false => (&sub.filter, &sub.project, lists.as_mut(), &*view),
    };
    let shown = shows(vw);
    let mut step = match on.changes(&sub.collection, written, (f.as_ref(), pr.as_deref()), l)? {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    if let (Changes::Batch(b), Some((scope, mask))) = (&mut step, shown) {
        scope.shown_rows(mask, on.database(), &mut b.puts);
    }
    // The rows the shape holds now, read as the changes were, where the
    // lists moved.
    let members = match (&step, moved) {
        (Changes::Batch(_), true) => {
            let stmt = Statement::Select(shape_select(&sub.collection, f, pr));
            match on.query(&stmt)? {
                Ok(Response::Rows(mut rs)) => {
                    if let Some((scope, mask)) = shown {
                        scope.shown_rows(mask, on.database(), &mut rs);
                    }
                    Some(rs)
                }
                Ok(_) => None,
                Err(e) => return Some(Err(e)),
            }
        }
        _ => None,
    };
    if moved {
        (sub.filter, sub.project) = (answered.0.take(), answered.1.take());
        if let Some(v) = answered.2.take() {
            *view = Some(v);
        }
        *lists = fresh;
    }
    Some(Ok(Looked {
        step,
        members,
        hold: None,
    }))
}

/// The `get` of a stream's whole shape -- `filter` and `project` over its
/// collection -- as [`api::seed_select`] makes it of a subscription.
fn shape_select(collection: &str, filter: &Option<Expr>, project: &Option<Vec<String>>) -> Select {
    Select {
        collection: collection.to_string(),
        filter: filter.clone(),
        project: project.clone(),
        ..Default::default()
    }
}

/// Whether `new`, a stream's filter answered again, holds what `old`
/// does: the same tree, each `in` list the same values in any order. An
/// inner `get` with no `order` answers in the order it reads, which a
/// write moving a row, or a compact, changes with no value changed: by
/// position, the shape ran again for a list that had not moved.
fn same_answer(old: &Option<Expr>, new: &Option<Expr>) -> bool {
    match (old, new) {
        (Some(a), Some(b)) => a == b || same_expr(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// `a` and `b` alike but for the order of an `in` list's values.
fn same_expr(a: &Expr, b: &Expr) -> bool {
    use Expr::*;
    let same = |x: &Expr, y: &Expr| same_expr(x, y);
    match (a, b) {
        (In(x, xs), In(y, ys)) => same(x, y) && (xs == ys || same_values(xs, ys)),
        (And(a1, a2), And(b1, b2))
        | (Or(a1, a2), Or(b1, b2))
        | (Like(a1, a2), Like(b1, b2))
        | (Has(a1, a2), Has(b1, b2)) => same(a1, b1) && same(a2, b2),
        (Cmp(o, a1, a2), Cmp(p, b1, b2)) => o == p && same(a1, b1) && same(a2, b2),
        (Arith(o, a1, a2), Arith(p, b1, b2)) => o == p && same(a1, b1) && same(a2, b2),
        (Not(x), Not(y)) | (IsNull(x), IsNull(y)) => same(x, y),
        (Call(n, xs), Call(m, ys)) => {
            n == m && xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| same(x, y))
        }
        _ => a == b,
    }
}

/// Two lists of values, the same ones in any order: their encodings
/// sorted -- the very values, bit for bit -- and compared. Sorted as
/// byte slices, whose sort the binary holds already; read in a loop, as
/// `collect` into an `Option` built a copy of its own (628 bytes).
fn same_values(xs: &[Expr], ys: &[Expr]) -> bool {
    let keys = |items: &[Expr]| {
        let mut keys = Vec::with_capacity(items.len());
        for it in items {
            let Expr::Lit(v) = it else {
                return None;
            };
            let mut key = Vec::new();
            fennec_core::codec::encode_value(&mut key, v);
            keys.push(key);
        }
        Some(keys)
    };
    if xs.len() != ys.len() {
        return false;
    }
    let (Some(a), Some(b)) = (keys(xs), keys(ys)) else {
        return false;
    };
    let mut a: Vec<&[u8]> = a.iter().map(Vec::as_slice).collect();
    let mut b: Vec<&[u8]> = b.iter().map(Vec::as_slice).collect();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

/// The rows the shape holds `now`, as a change to a stream that was sent
/// `seen`: those it was not sent enter, and those it was sent and the
/// shape no longer holds leave -- a deletion, told only of a row it was
/// sent. Where its rows are masked every row goes, and those that show
/// what they showed when sent are left out by their digests: a row the
/// rules now admit by another field list shows other fields.
fn enter_and_leave(b: &mut ChangeBatch, now: ResultSet, seen: &Seen) {
    let held: std::collections::HashSet<DocId> = now.rows.iter().map(|r| r.id).collect();
    let put: std::collections::HashSet<DocId> = b.puts.rows.iter().map(|r| r.id).collect();
    if b.puts.rows.is_empty() {
        b.puts.columns = now.columns;
    }
    b.puts.rows.extend(
        now.rows
            .into_iter()
            .filter(|r| !put.contains(&r.id) && (seen.digests || !seen.ids.contains_key(&r.id))),
    );
    for id in seen.ids.keys() {
        if !held.contains(id) && !b.dels.contains(id) {
            b.dels.push(*id);
        }
    }
}

/// The rows a scoped stream was sent, by id: the ones it may be told
/// left. Where its rows are masked, each with a digest of the row as sent.
struct Seen {
    ids: std::collections::HashMap<DocId, u64>,
    digests: bool,
}

impl Seen {
    fn seed(&mut self, rows: &[Row]) {
        self.ids = rows.iter().map(|r| (r.id, self.digest(r))).collect();
    }

    /// Whether the row `id` was sent, forgetting it: a deletion is told.
    fn left(&mut self, id: DocId) -> bool {
        self.ids.remove(&id).is_some()
    }

    /// Whether `row`, written, is to be sent: unless it is masked and
    /// shows what it showed when last sent.
    fn sent(&mut self, row: &Row) -> bool {
        let d = self.digest(row);
        self.ids.insert(row.id, d) != Some(d) || !self.digests
    }

    /// The row's values as their encodings take them, hashed: `==` takes
    /// -0.0 for 0.0.
    fn digest(&self, row: &Row) -> u64 {
        use std::hash::{Hash, Hasher};
        if !self.digests {
            return 0;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut buf = Vec::new();
        for v in &row.values {
            buf.clear();
            fennec_core::codec::encode_value(&mut buf, v);
            buf.hash(&mut h);
        }
        h.finish()
    }
}

/// A change of the rows `dels` reaching their time, at the change the
/// stream stands at: no write moved it.
fn expired_json(seq: u64, dels: &[DocId]) -> String {
    let mut out = format!("{{\"seq\":{seq},\"puts\":[],\"dels\":[");
    for (i, id) in dels.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&id.to_string());
    }
    out.push_str("],\"schema\":false}");
    out
}

/// Sends the whole shape; the change it is at, the change ring held back
/// to it until the stream's first look after it, and the images the
/// database had adopted by then (`Database::adoptions`).
///
/// The rows and the change they are at are one moment: a write landing
/// between the two would be a row the seed lacks at a counter saying the
/// stream has it. Read under the lock, that moment was the whole read, and
/// every writer waited for it -- single writes beside a stream seeded from
/// a million rows waited out its scan and encoding. So the shape is pinned
/// under the lock ([`Database::pin`]) and read with none held, at the
/// change the pin took: writes go on beside it, and the stream tells each
/// as a change after that one, none twice and none missed. Under the same
/// lock a rule reading another collection is bound ([`Reach::rules`]), and
/// the shape's inner `get`s are answered on the pin, beside the writers,
/// as a query's are ([`Pinned::answer_subqueries`]) -- so the scope and
/// the lists are the pin's moment too, what the first look's changes are
/// judged by where no write came to what they read -- and the change ring
/// is held back to that change ([`Database::hold_changes`]): the writes
/// beside a long seed would otherwise leave the stream behind the ring's
/// horizon, to be seeded again, pinned again, for as long as they outran
/// the scan. What the pin declines -- a shape an index answers, one too
/// short to be worth it -- is read under the lock as before, its lists
/// answered there, as is a pinned read that reached for an index (`None`),
/// one the ring no longer reaches back to by its end, the hold's bound
/// passed, and one an image adopted meanwhile made stale: an image can land
/// on the pin's own change, where neither the ring nor the counter shows
/// it, and the pin read what it replaced. Lists the pin answered before its
/// read reached for an index are kept where they still hold ([`Kept`]),
/// rather than answered again under the lock.
fn seed(
    out: &mut Events,
    db: &Arc<RwLock<Database>>,
    (sub, reach, view, lists): (
        &mut api::Subscription,
        &mut Option<Reach>,
        &mut Option<View>,
        &mut Option<Lists>,
    ),
    seen: &mut Option<Seen>,
    quiet: bool,
) -> std::io::Result<(u64, Hold, u64)> {
    let mut pin = true;
    // Lists the pin answered, for the lock to read by where they hold.
    let mut kept: Option<Kept> = None;
    // When a row the lists read reaches its time past when they were
    // answered: the stream's `Reach::next` once the seed's read ended.
    let mut reaches = None;
    let read = loop {
        let guard = crate::held::read(db);
        let now = now_ms() as i64;
        let (hold, adopted) = (guard.hold_changes(), guard.adoptions());
        // The rules bound under the lock; the inner `get`s answered below.
        // Kept, the lists are due by the time from when they were answered.
        let (ruled, at) = match (reach.as_mut(), kept.take()) {
            (Some(r), Some(k)) if k.holds(&guard, r, now) => (Ok(Some((k.answered, true))), k.now),
            (Some(r), _) => (r.rules(&guard).map(|a| Some((a, false))), now),
            (None, _) => (Ok(None), now),
        };
        let ruled = match ruled {
            Ok(r) => r,
            Err(e) => {
                break seed_rows(&guard, Err(e), view, seen, quiet).map(|r| (r, hold, adopted))
            }
        };
        if let Some(r) = reach.as_ref() {
            reaches = r.reaches(&guard, at);
        }
        let stmt = Statement::Select(match &ruled {
            Some(((filter, project, _), _)) => shape_select(&sub.collection, filter, project),
            None => api::seed_select(sub),
        });
        let pinned = match (pin, &ruled) {
            // Answered already: the lock reads it, as it did.
            (_, Some((_, true))) | (false, _) => None,
            _ => guard.pin(&[(&stmt, &[][..])]),
        };
        let Some(p) = pinned else {
            let rows = match ruled {
                Some(((mut filter, project, v), done)) => {
                    let answered = match (&mut filter, done) {
                        (Some(f), false) => guard.answer_subqueries(f),
                        _ => Ok(()),
                    };
                    answered.and_then(|()| {
                        settle(sub, view, lists, (filter, project, v));
                        guard.query(&Statement::Select(api::seed_select(sub)), &[])
                    })
                }
                None => guard.query(&stmt, &[]),
            };
            break seed_rows(&guard, rows, view, seen, quiet).map(|r| (r, hold, adopted));
        };
        drop(guard);
        let (rows, answer, ended) = match ruled {
            Some(((mut filter, project, v), _)) => {
                if let Some(f) = &mut filter {
                    match p.answer_subqueries(f) {
                        // An inner `get` reached for an index: the lock's.
                        None => {
                            pin = false;
                            continue;
                        }
                        Some(Err(e)) => break Err(e),
                        Some(Ok(())) => {}
                    }
                }
                let ended = now_ms() as i64;
                let stmt = Statement::Select(shape_select(&sub.collection, &filter, &project));
                (p.query(&stmt, &[]), Some((filter, project, v)), ended)
            }
            None => (p.query(&stmt, &[]), None, now),
        };
        let Some(rows) = rows else {
            // Answered at the pin's change: kept, for the lock to read by
            // where they still hold.
            kept = answer.map(|answered| Kept {
                at: p.change_seq(),
                images: adopted,
                now,
                ended,
                answered,
            });
            pin = false;
            continue;
        };
        if let Some(a) = answer {
            settle(sub, view, lists, a);
        }
        let read = seed_rows(p.database(), rows, view, seen, quiet);
        drop(p);
        let guard = crate::held::read(db);
        let seq = hold.seq();
        let overrun = seq < guard.change_horizon() || seq > guard.change_seq();
        if read.is_ok() && (overrun || guard.adoptions() != adopted) {
            pin = false;
            continue;
        }
        break read.map(|r| (r, hold, adopted));
    };
    let (rows, hold, adopted) = match read {
        Ok(r) => r,
        Err(e) => {
            event(out, "error", &error_json(&e))?;
            return Err(std::io::Error::other("seeding failed"));
        }
    };
    if let Some(r) = reach.as_mut() {
        r.next = r.due_at(reaches, now_ms() as i64);
    }
    let seq = hold.seq();
    event(out, "seed", &format!("{{\"seq\":{seq},\"rows\":{rows}}}"))?;
    Ok((seq, hold, adopted))
}

/// The stream's filter, list and view as a seed answered them, and its
/// long lists to bind at its first look ([`Lists::of`]).
fn settle(
    sub: &mut api::Subscription,
    view: &mut Option<View>,
    lists: &mut Option<Lists>,
    (filter, project, v): Answered,
) {
    (sub.filter, sub.project) = (filter, project);
    if v.is_some() {
        *view = v;
    }
    *lists = Lists::of(sub.filter.as_ref());
}

/// The seed's rows as their event sends them, read on `db` -- the database
/// under the lock, or a pin's, whose schemas the mask reads -- and `seen`
/// told of them.
fn seed_rows(
    db: &Database,
    rows: fennec_core::error::Result<Response>,
    view: &Option<View>,
    seen: &mut Option<Seen>,
    quiet: bool,
) -> fennec_core::error::Result<String> {
    let Response::Rows(mut rs) = rows? else {
        return Ok(String::from("[]"));
    };
    if let Some((scope, mask)) = shows(view) {
        scope.shown_rows(mask, db, &mut rs);
    }
    if let Some(seen) = seen {
        seen.seed(&rs.rows);
    }
    Ok(match quiet {
        true => String::from("[]"),
        false => api::rows_json(&rs),
    })
}

/// The last event of a stream whose token's `exp` passed: what a refused
/// request says (`access::EXPIRED_WHY`), its status, and the code a client
/// tells an expiry by (`access::EXPIRED_CODE`), for it to fetch a fresh
/// token on. A constant written once, so the stream's look pays nothing
/// for it; `access`'s tests hold it to those two.
pub const EXPIRED: &str =
    r#"{"error":"the token has expired","status":401,"code":"token_expired"}"#;

/// Out of line: a stream reads the time at a dozen places, and with the
/// reads that count a list's second from its answer's end, inlined at each,
/// they were 1.6 KB of fennec-server's code (`__text` +1 984 bytes against
/// +384).
#[inline(never)]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn change_json(b: &ChangeBatch) -> String {
    let mut out = format!("{{\"seq\":{},\"puts\":", b.seq);
    out.push_str(&api::rows_json(&b.puts));
    out.push_str(",\"dels\":[");
    for (i, id) in b.dels.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&id.to_string());
    }
    out.push_str("],\"schema\":");
    out.push_str(if b.schema_changed { "true" } else { "false" });
    out.push('}');
    out
}

fn error_json(e: &Error) -> String {
    let mut body = String::from("{\"error\":");
    fennec_core::json::escape_into(&mut body, &e.to_string());
    body.push('}');
    body
}

/// A single SSE event. `data` is single-line JSON, so no line splitting is
/// needed -- the JSON encoder never produces a `\n`.
fn event(out: &mut Events, name: &str, data: &str) -> std::io::Result<()> {
    out.send(format!("event: {name}\ndata: {data}\n\n").as_bytes())
}

/// A stream's events as they go out: as written, or where its client takes
/// gzip each a piece of one member, deflated alone past `--http-gzip`'s
/// bytes and stored under them, ending on a byte its client's inflater
/// reads whole ([`crate::gzip::Stream`]). Between events it holds the
/// member's CRC and length and nothing else: no window, no tables, however
/// many streams are open (docs/design/http.md#compressed-answers).
struct Events<'a> {
    conn: &'a crate::conn::Conn,
    gz: Option<(crate::gzip::Stream, usize)>,
    /// A write failed: the client gone or stalled past
    /// `stream_write_timeout`, so the member's end is not written after.
    broken: bool,
}

impl Events<'_> {
    /// `bytes` out and flushed: an event, or the keep-alive's comment.
    fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut out = self.conn;
        let sent = match &mut self.gz {
            None => out.write_all(bytes),
            Some((stream, min)) => {
                let mut piece = Vec::with_capacity(bytes.len() / 4 + 16);
                stream.piece(bytes, *min, &mut piece);
                out.write_all(&piece)
            }
        }
        .and_then(|()| out.flush());
        self.broken |= sent.is_err();
        sent
    }
}

impl Drop for Events<'_> {
    /// The member's end, where the stream ends with its client still
    /// there: an inflater that holds a member cut short for an error
    /// (Node's zlib, Go's `gzip.Reader`) reads the end it is. Not after a
    /// failed write: a stalled client would hold the stream's slot another
    /// `stream_write_timeout` for it.
    fn drop(&mut self) {
        if let (Some((stream, _)), false) = (&self.gz, self.broken) {
            let mut end = Vec::with_capacity(10);
            stream.finish(&mut end);
            let mut out = self.conn;
            let _ = out.write_all(&end).and_then(|()| out.flush());
        }
    }
}

/// A stream's head; `at` where the database stood as the stream opened,
/// which its head names: the change its seed is at or after, and the
/// history every event's `seq` is a change of. A refusal is answered whole
/// in its place, as every route's is (`Response::write`). Where `gzip`,
/// the head says `Content-Encoding: gzip` and the member's own header goes
/// with it, the member begun.
fn write_head(
    out: &mut &crate::conn::Conn,
    cfg: &Config,
    at: crate::At,
    gzip: bool,
) -> std::io::Result<Option<crate::gzip::Stream>> {
    // `X-Accel-Buffering: no` turns off proxy buffering (nginx); otherwise
    // events pile up, arrive in bulk, and the stream stops being "live".
    let mut head = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
         Cache-Control: no-cache, no-transform\r\nConnection: close\r\n\
         X-Accel-Buffering: no\r\nFennec-Seq: ",
    );
    crate::http::push_number(&mut head, at.seq);
    head.push_str("\r\n");
    // The root's history names nothing, as every answer's does.
    if let Some(h) = at.history {
        head.push_str("Fennec-History: ");
        crate::http::push_history(&mut head, h);
        head.push_str("\r\n");
    }
    // Every answer's CORS headers, not the origin alone: without
    // `Access-Control-Expose-Headers` a page on another origin could not
    // read the head's `Fennec-History`.
    if let Some(origin) = &cfg.cors {
        crate::cors::push_into(&mut head, origin);
    }
    crate::request_id::with(|id| {
        if !id.is_empty() {
            head.push_str(&format!("{}: {id}\r\n", crate::request_id::HEADER));
        }
    });
    crate::audit::push_verified(&mut head);
    // `Vary` only where the stream is compressed: nothing keeps a stream
    // (`no-cache`), and one sent as it is is every client's.
    if gzip {
        head.push_str("Content-Encoding: gzip\r\nVary: Accept-Encoding\r\n");
    }
    head.push_str("\r\n");
    let mut head = head.into_bytes();
    let stream = gzip.then(|| crate::gzip::Stream::start(&mut head));
    out.write_all(&head)?;
    out.flush()?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list answered again holds what it held when its values are the
    /// same in any order: by position, an inner `get` reading its rows in
    /// another order ran the shape again for nothing.
    #[test]
    fn a_list_answered_in_another_order_has_not_moved() {
        let filter = |w: &str| match fennec_ql::parse_one(&format!("get t where {w}")).unwrap() {
            Statement::Select(s) => s.filter,
            _ => unreachable!(),
        };
        let a = filter(r#"msg in ["a", "b", "c"] and n > 1"#);
        assert!(same_answer(&a, &a));
        assert!(same_answer(
            &a,
            &filter(r#"msg in ["c", "a", "b"] and n > 1"#)
        ));
        for other in [
            r#"msg in ["a", "b", "d"] and n > 1"#,
            r#"msg in ["a", "b"] and n > 1"#,
            r#"msg in ["c", "a", "b"] and n > 2"#,
            r#"tag in ["c", "a", "b"] and n > 1"#,
        ] {
            assert!(!same_answer(&a, &filter(other)), "{other}");
        }
        assert!(!same_answer(&a, &None));
        assert!(same_answer(&None, &None));
    }

    /// Lists a pin answered hold under a later lock while nothing they
    /// read was written and no row of it reached its time since they were
    /// answered, or, reading the time, [`CLOCK_EVERY`] passed since the
    /// read answering them ended: their time is when they were answered.
    /// Worked out from when the lock found them, a note reaching its time
    /// between left the seed's lists holding it, and the stream no wake
    /// for it. Counted from when they were answered, lists taking a second
    /// to answer were due as their read ended, an idle stream answering
    /// them back to back.
    #[test]
    fn kept_lists_hold_while_what_they_read_stands() {
        let mut db = Database::new();
        let t1 = now_ms() as i64;
        for sql in [
            "create collection notes (title text, at timestamp @ttl(1s))".to_string(),
            "create collection board (msg text)".to_string(),
            // b1's note reaches its time at t1 + 1 500.
            format!(
                r#"put notes [{{title: "a1", at: {}}}, {{title: "b1", at: {}}}]"#,
                t1 + 3_600_000,
                t1 + 500
            ),
        ] {
            db.execute(&fennec_ql::parse_one(&sql).unwrap()).unwrap();
        }
        let mut r = Reach {
            rules: None,
            collection: "board".into(),
            shape: None,
            project: None,
            reads: vec!["notes".into()],
            clock: false,
            next: None,
        };
        let kept = |db: &Database, ended: i64| Kept {
            at: db.change_seq(),
            images: db.adoptions(),
            now: t1,
            ended,
            answered: (None, None, None),
        };
        let k = kept(&db, t1);
        assert_eq!(r.next_after(&db, t1, t1), Some(t1 + 1_500));
        // A row's time is counted from when the lists were answered, not
        // from when their read ended.
        assert_eq!(r.next_after(&db, t1, t1 + 1_200), Some(t1 + 1_500));
        assert!(k.holds(&db, &r, t1 + 1_000));
        assert!(!k.holds(&db, &r, t1 + 1_500), "b1 reached its time");
        // A write to what the stream reads alone, the lists moved.
        db.execute(&fennec_ql::parse_one(r#"put board {msg: "x"}"#).unwrap())
            .unwrap();
        assert!(k.holds(&db, &r, t1 + 1_000));
        db.execute(&fennec_ql::parse_one(r#"put notes {title: "c1"}"#).unwrap())
            .unwrap();
        assert!(!k.holds(&db, &r, t1 + 1_000));
        // Reading the time, a second from when their read ended.
        let k = kept(&db, t1);
        r.clock = true;
        assert_eq!(r.next_after(&db, t1, t1), Some(t1 + CLOCK_EVERY));
        assert!(k.holds(&db, &r, t1 + CLOCK_EVERY - 1));
        assert!(!k.holds(&db, &r, t1 + CLOCK_EVERY));
        // Their read ending 300 ms after they were answered, a second from
        // then, before b1's note reaches its time.
        let slow = t1 + 300;
        assert_eq!(r.next_after(&db, t1, slow), Some(slow + CLOCK_EVERY));
        let k = kept(&db, slow);
        assert!(k.holds(&db, &r, slow + CLOCK_EVERY - 1));
        assert!(!k.holds(&db, &r, slow + CLOCK_EVERY));
        // Due by a write to what they read, or by the time: reading the
        // time, a write to anything else does not answer them again.
        let since = db.change_seq();
        r.next = r.next_after(&db, t1, t1);
        db.execute(&fennec_ql::parse_one(r#"put board {msg: "y"}"#).unwrap())
            .unwrap();
        assert!(!r.due(&db, since, t1 + 10));
        assert!(r.due(&db, since, t1 + CLOCK_EVERY));
        db.execute(&fennec_ql::parse_one(r#"put notes {title: "d1"}"#).unwrap())
            .unwrap();
        assert!(r.due(&db, since, t1 + 10));
    }

    /// A replica that takes an image can land on a lower change than it
    /// held. The hub has to follow it down: kept at the old high mark, it
    /// told every stream reseeded at the new one that a write was waiting,
    /// and the stream spun on empty batches until writes caught up.
    #[test]
    fn the_mark_follows_the_database_down() {
        let hub = Hub::new();
        hub.notify(1000);
        hub.notify(900);
        let t = std::time::Instant::now();
        assert_eq!(hub.wait(900, Duration::from_millis(50)), 900);
        assert!(t.elapsed() >= Duration::from_millis(40));
    }
}
