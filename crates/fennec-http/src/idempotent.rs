//! `Idempotency-Key`: a write sent again is answered as it was the first
//! time, and not made again. A `put` with no id makes a row each time it
//! runs, so a client that retries a request whose answer it never got --
//! a timeout, a dropped connection -- wrote the row twice.
//!
//! The key and the answer are kept in the same block as the write
//! (`_idempotency`), under the write lock: the write and its key land
//! together or not at all, so a crash between the two cannot leave a
//! write whose retry makes it again, and a second request with the key
//! waits for the lock and finds it -- answered once the block holding it is
//! as durable, and as sent, as the first try's answer waited for it to be
//! ([`replay`]). A key is the subject's, for a scoped token, so no user is
//! handed another's answer; the request it came with is kept as a hash,
//! and the key sent with another request is refused (422), as the IETF
//! draft has it. Kept for `idempotency_ttl`: the collection's `at` is
//! `@ttl`, so a key past its time is out of every read at once and the
//! sweeper deletes it ([`crate::sweep`]), where a purge of its own once ran
//! on the write path at most once a minute. The change stream leaves these
//! writes out.

use crate::access::Who;
use crate::http::{Request, Response};
use fennec_core::prelude::*;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// The collection the keys are kept in, one of the server's own.
pub const KEYS: &str = fennec_core::declared::IDEMPOTENCY;

/// An answer longer than this is kept as its status and a note saying so:
/// a write's answer is a count, and a batch's reads can be long.
const KEPT_BODY: usize = 64 << 10;

/// The refusal of a key sent with a scoped token that has no subject.
const NO_SUBJECT: &str = "an Idempotency-Key needs a token with a subject (`sub`)";

/// The refusal of a key sent with a scoped token whose `sub` is no text
/// ([`crate::access::Scope::sub_not_text`]).
const SUB_NOT_TEXT: &str = "an Idempotency-Key needs a token with a subject, and the token's \
     `sub` claim must be a string (RFC 7519): a numeric id goes in as its digits, \"42\"";

/// A key, the subject's for a scoped token, and a hash of the request.
pub struct Key {
    name: String,
    request: i64,
}

/// The request's key, if it has one: `None` without, a refusal for one
/// that cannot be kept.
pub fn key(req: &Request, who: &Who) -> std::result::Result<Option<Key>, Response> {
    let Some(given) = req.header("idempotency-key") else {
        return Ok(None);
    };
    if given.is_empty() || given.len() > 255 {
        return Err(Response::error(400, "an Idempotency-Key is 1 to 255 bytes"));
    }
    // A key is kept as its owner, `U+001F` and the key: one holding that
    // character, beside a subject holding it too, named another subject's
    // key -- `sub` "a\u{1f}b" with key "c" is `sub` "a" with key "b\u{1f}c".
    // Without one in the key, the last `U+001F` parts the two.
    if given.chars().any(char::is_control) {
        return Err(Response::error(
            400,
            "an Idempotency-Key holds no control character",
        ));
    }
    // Keys are the subject's: two users' keys never meet. A scoped token
    // with no subject has no one to keep them for -- nor one whose `sub` is
    // a number, which names none: told so, an app minting its numeric ids
    // as `"sub": 42` knows what to change.
    let owner = match who {
        Who::Full => "",
        Who::Scoped(scope) => match scope.subject() {
            Some(sub) => sub,
            None => {
                let why = match scope.sub_not_text() {
                    true => SUB_NOT_TEXT,
                    false => NO_SUBJECT,
                };
                return Err(Response::error(400, why));
            }
        },
    };
    let mut name = String::with_capacity(owner.len() + 1 + given.len());
    name.push_str(owner);
    name.push('\u{1f}');
    name.push_str(given);
    Ok(Some(Key {
        name,
        request: fnv(&[
            req.method.name().as_bytes(),
            req.target.as_bytes(),
            &req.body,
        ]) as i64,
    }))
}

fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in parts {
        for &b in *p {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        h = (h ^ 0xff).wrapping_mul(0x100000001b3);
    }
    h
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The statements the keys take, parsed once: parsed for each keyed
/// write, with the collection made if missing every time, they were most
/// of what a key cost.
fn stmt(which: usize) -> &'static Statement {
    const SQL: [&str; 3] = [
        "get _idempotency select request, status, body, at where key = $1",
        "del _idempotency where key = $1",
        "put _idempotency {key: $1, request: $2, status: $3, body: $4, at: $5}",
    ];
    static PARSED: std::sync::OnceLock<Vec<Statement>> = std::sync::OnceLock::new();
    &PARSED.get_or_init(|| {
        SQL.iter()
            .map(|s| fennec_ql::parse_one(s).expect("the key statements parse"))
            .collect()
    })[which]
}

const GET: usize = 0;
const DEL: usize = 1;
const PUT: usize = 2;

/// The keys' collection as `ttl_ms` keeps them, made, or made to keep them
/// so: a server started again with another `--idempotency-ttl` moves the
/// expiry (`alter field at @ttl`), and a collection from before the expiry
/// -- `at int @sorted`, whose keys a purge let go of -- is made again, the
/// keys it held a day's retries at most.
fn ensure(db: &mut Database, ttl_ms: i64) -> fennec_core::error::Result<()> {
    let ttl = fennec_core::schema::ttl_text(ttl_ms.max(1) as u64);
    let at = db.collection(KEYS).ok().map(|c| {
        c.schema
            .field("at")
            .map(|f| (f.ty == DataType::Timestamp, f.index.ttl()))
    });
    let sql = match at {
        Some(Some((true, Some(t)))) if t == ttl_ms.max(1) as u64 => return Ok(()),
        Some(Some((true, Some(_)))) => {
            format!("alter collection {KEYS} alter field at @ttl({ttl})")
        }
        made => {
            if made.is_some() {
                db.execute(&Statement::DropCollection {
                    name: KEYS.into(),
                    if_exists: true,
                })?;
            }
            format!(
                "create collection {KEYS} (key text @hash, request int, status int, \
                 body text, at timestamp @ttl({ttl}))"
            )
        }
    };
    db.execute(&fennec_ql::parse_one(&sql)?)?;
    Ok(())
}

/// A key's kept answer, as a request sent with the key again is handed it
/// ([`replay`]), and what it waits for first.
pub struct Replay {
    sent: Response,
    durable: Option<fennec_core::engine::Durability>,
    /// The sink's error as the durability was asked for, `sent` its 500:
    /// reported to the engine once the lock is let go
    /// ([`Database::fail`]), as a durability's failure is.
    failed: Option<fennec_core::error::Error>,
}

impl Replay {
    /// `sent`, waiting for nothing.
    fn now(sent: Response) -> Replay {
        Replay {
            sent,
            durable: None,
            failed: None,
        }
    }

    /// The answer, once what it waits for ran -- with no lock held, as a
    /// write's durability runs ([`crate::await_durable`]). A failed fsync
    /// is the answer, as it is the first try's: a 500, never the success.
    pub fn answer(self, db: &RwLock<Database>) -> Response {
        if let Some(e) = &self.failed {
            crate::storage_failed(db, e);
            return self.sent;
        }
        match crate::await_durable(db, self.durable) {
            Ok(()) => self.sent,
            Err(e) => crate::error_response(&e),
        }
    }
}

/// The answer kept for `key`, as it was sent, found under a lock the
/// caller lets go of before [`Replay::answer`]: a request with the key
/// still running holds the write lock until its block has landed.
///
/// The block's fsync, and on a primary its wait for the streams to send
/// it, run after its write lock, while the key is read from the moment
/// the block lands: answered then, a replay -- the client's retry, the
/// router's of a cut connection -- handed back a success that a power
/// loss, or a failover to a standby the block never reached, took back,
/// and the client sent it no more. So, `durable` (`--sync always`), it
/// waits as that write's answer does: for the database's durability
/// ([`Database::durability`]), everything appended -- the key's block
/// among it -- on disk and sent to every stream keeping up, `SENT_WAIT` at
/// most. An fsync that covered it already makes that one run none, and
/// one under way is waited out at the disk. Under `--sync <ms>` a first
/// try's answer waits for no fsync, and a replay for none.
///
/// After a storage error ([`Database::failure`]) a replay is the refusal
/// every write meets, a 500, under either `--sync`: the write it answers
/// may be among what the disk lost -- a failed fsync, or under `--sync
/// <ms>` a failed write of the buffer it was in -- and whether it is, no
/// one here knows. Answered the kept success, its client held as made a
/// write the next open may not have. The 422 of a key sent with another
/// request says nothing of the write and waits for nothing.
pub fn replay(db: &Database, key: &Key, ttl_ms: i64, durable: bool) -> Option<Replay> {
    let sent = answered(db, key, ttl_ms)?;
    if sent.status == 422 {
        return Some(Replay::now(sent));
    }
    if let Err(e) = db.refuse_if_failed() {
        return Some(Replay::now(crate::error_response(&e)));
    }
    if !durable {
        return Some(Replay::now(sent));
    }
    Some(match db.durability() {
        Ok(durable) => Replay {
            sent,
            durable,
            failed: None,
        },
        Err(e) => Replay {
            sent: crate::error_response(&e),
            durable: None,
            failed: Some(e),
        },
    })
}

/// The answer kept for `key`, as it was sent.
fn answered(db: &Database, key: &Key, ttl_ms: i64) -> Option<Response> {
    db.collection(KEYS).ok()?;
    let fennec_core::query::Response::Rows(rs) =
        db.query(stmt(GET), &[Value::Text(key.name.clone())]).ok()?
    else {
        return None;
    };
    let row = rs.rows.first()?;
    let [Value::Int(request), Value::Int(status), Value::Text(body), Value::Timestamp(at)] =
        row.values.as_slice()
    else {
        return None;
    };
    // Past its time: as if never sent. A read leaves out a key past the
    // collection's expiry already; this is for a `--idempotency-ttl`
    // shortened since, until the next keyed write moves it.
    if now_ms() - at >= ttl_ms {
        return None;
    }
    if *request != key.request {
        return Some(Response::error(
            422,
            "this Idempotency-Key came with another request: a key is for one",
        ));
    }
    let mut sent =
        Response::json(*status as u16, body.clone()).header("Idempotent-Replayed", "true");
    // Where the database stands now, which holds the write: a client that
    // reads on a replica after the replayed answer waits for it there, as
    // after the first. Sent without, every SDK read a replay's `Fennec-Seq`
    // as none.
    if *status < 300 {
        sent.seq = Some(db.change_seq());
        sent.history = crate::http::history_of(db);
    }
    Some(sent)
}

/// Keeps `resp` as `key`'s answer, in the block the write is in, for
/// `ttl_ms`.
pub fn keep(
    db: &mut Database,
    key: &Key,
    resp: &Response,
    ttl_ms: i64,
) -> fennec_core::error::Result<()> {
    let fresh = db.collection(KEYS).is_err();
    ensure(db, ttl_ms)?;
    let body = match resp.body.len() <= KEPT_BODY {
        true => String::from_utf8_lossy(&resp.body).into_owned(),
        false => format!(
            "{{\"idempotent\":\"the first answer was {} bytes, more than is kept\"}}",
            resp.body.len()
        ),
    };
    let row = [
        Value::Text(key.name.clone()),
        Value::Int(key.request),
        Value::Int(resp.status as i64),
        Value::Text(body),
        Value::Timestamp(now_ms()),
    ];
    // A key sent again after its time is a new request: the row of the
    // first, if a read still finds it, is written over; one past the
    // expiry is the sweeper's.
    if !fresh {
        db.execute_with(stmt(DEL), &row[..1])?;
    }
    db.execute_with(stmt(PUT), &row)?;
    Ok(())
}
