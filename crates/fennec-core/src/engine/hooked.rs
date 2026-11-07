//! A webhook consumer's events taken (`docs/design/named-statements.md`,
//! *Webhook consumers (F4)*, *Where an event is taken, and once*): an
//! event its sender signed, verified and parsed by the server
//! (`fennec_http`'s hooks, F4's slice 3), is taken by every statement of
//! its hook whose condition passes it, in name order, their writes and the
//! event's key one block -- so a sender's retry, a duplicate delivery and a
//! failover between a write and its answer take an event once in the
//! history that survives.
//!
//! The key is the `Idempotency-Key`'s machinery in a collection of its
//! own, [`HOOK_EVENTS`]: a row a key, `<hook>`, `U+001F` and the key, kept
//! until its `until` -- the hook's `keep` past its taking, each row its own
//! expiry, `@ttl(1ms)` -- out of every read at once after it, and swept as
//! every `@ttl` row is. A key held answers the event as its first taking
//! did and runs nothing, a second body under it too (decision 7).
//!
//! No server takes one yet: every way in refuses a hook's definition but a
//! test's ([`Database::allow_hooks`]). The browser module and the native
//! library keep the record and take no event.

use std::sync::Arc;

use super::due::Clocked;
use super::{Callee, Database, NoRow};
use crate::error::{Error, Result};
use crate::named::{event_at, Definition, Source};
use crate::query::{eval, truthy, CmpOp, EvalCtx, Expr, Response, RowAccess, Select, Statement};
use crate::schema::{Field, IndexKind, Schema};
use crate::value::{DataType, DocId, Value};

/// The collection an event's key is kept in: the server's own
/// (`declared::own`, named among `declared::OWN`), held by its `_` out of
/// a body, of the tenant's rows a quota counts (`Database::app_writes`),
/// of `/_changes` (`fennec_http::cdc`), of the listings and `/_schema`,
/// and out of every rule's reach, a client's statement changing none of
/// it (`docs/design/access.md#the-servers-own-collections`). Made by the
/// first event taken, never before.
pub const HOOK_EVENTS: &str = crate::declared::HOOK_EVENTS;

/// An event a hook's sender sent, verified and parsed by the server, as
/// [`Database::take_event`] takes it.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    /// The hook it was sent to, `/_hooks/<hook>`.
    pub hook: &'a str,
    /// Its key (`hook.id`): a value its signature covers, or the one
    /// header its hook was told to trust -- 1 to 255 bytes, no control
    /// character.
    pub key: &'a str,
    /// Its body, parsed exactly: an object.
    pub body: &'a Value,
    /// A digest of the body's bytes as they came, by which a second body
    /// under a held key is told from the first.
    pub digest: u64,
    /// `hook.at`: the time its signature covers, where its scheme signs one.
    pub at: Option<i64>,
    /// `hook.event`: GitHub's `X-GitHub-Event`, where it was sent.
    pub event: Option<&'a str>,
    /// How long its key is kept past its taking, in milliseconds: the
    /// hook's `keep`.
    pub keep: i64,
}

/// What a held key says of the event's first taking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldKey {
    /// The statements that took it.
    pub taken: u32,
    /// Whether this body is the first's, by its digest: another is
    /// answered as the first was all the same, never run (decision 7).
    pub same: bool,
}

/// What [`Database::take_event`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Took {
    /// Taken by this many statements: their writes and its key's row the
    /// block's.
    Ran(u32),
    /// No condition passed it: nothing written, no key kept.
    Passed,
    /// Its key was held: taken before, and answered as it was then.
    Held(HeldKey),
}

/// What a webhook consumer's condition reads: the event's paths and
/// `hook.event`, each as a `json` field's value is compared.
struct EventRow<'e, 'a>(&'e Event<'a>);

impl RowAccess for EventRow<'_, '_> {
    fn id(&self) -> DocId {
        0
    }

    fn field(&mut self, name: &str) -> Result<Value> {
        if let Some(keys) = name.strip_prefix("event.") {
            return Ok(event_at(self.0.body, keys).clone());
        }
        match name {
            "hook.event" => Ok(self.0.event.map_or(Value::Null, |t| Value::Text(t.into()))),
            _ => Err(Error::Query(format!(
                "a hook's condition reads `event.<path>` and `hook.event`, and no `{name}`"
            ))),
        }
    }
}

impl Database {
    /// Lets a webhook consumer be defined, which every way in refuses
    /// until a server takes events (`named::HOOKED_NOT_BUILT`): a test's,
    /// to hold a hook's record and drive its events, until F4's slice 3
    /// brings the server that takes them.
    #[doc(hidden)]
    pub fn allow_hooks(&mut self) {
        self.hooking = true;
    }

    /// The event's key as [`HOOK_EVENTS`] holds it, where it is held and
    /// not past its `until`: what its first taking says -- the statements
    /// that took it -- and whether this body is the first's. Over
    /// `&Database`: a duplicate is found under a read lock, beside the
    /// readers, and the first try still running is found again under the
    /// write lock ([`Self::take_event`]). Refused (400) for a key or a body
    /// no event has.
    pub fn event_kept(&self, e: &Event) -> Result<Option<HeldKey>> {
        let key = kept_key(e)?;
        self.kept(&key, e.digest)
    }

    /// Takes `e`: under the write lock, its key looked up again -- a first
    /// try running when a retry overlapped it has landed by now -- and
    /// answered from it where held ([`Took::Held`]); otherwise its hook's
    /// statements judged in name order, each condition asked and each
    /// taking statement's parameters read from the event by their types,
    /// its `check` asked, and their bodies run, each statement through
    /// `run_one` -- waking the bindings its writes reach, seeing the writes
    /// before it -- under one `--call-max-rows` for them all; then its
    /// key's row, last. `now()` is the event's taking, one instant read
    /// once: the key's `until` asked and kept, every `check`, every body
    /// and binding. In the block open -- its
    /// caller's to land, or to put back where this refuses -- or in one of
    /// its own, landed whole or put back, so the event lands with every
    /// write it made and its key, or with none.
    ///
    /// Refused, nothing run: a key or a body no event has (400); no
    /// statement taking the hook (409); one of them held unread -- a text
    /// a later grammar reads otherwise, a process with no reader -- every
    /// event of the hook, its condition unknown (409); one broken by its
    /// body, the events its condition takes (409); a parameter missing,
    /// `null` with no default or of another type, and `check` false (400).
    /// Passed, nothing written and no key kept, where no condition takes
    /// it. Each refusal of a statement's -- a guard, a clash, a binding's,
    /// `--call-max-rows` -- refuses the event whole, its block put back.
    /// A caller makes the block durable before it answers the sender, and
    /// a duplicate's answer waits for that as well (F4's slice 3).
    pub fn take_event(&mut self, e: &Event) -> Result<Took> {
        let key = kept_key(e)?;
        // The clock fixed before anything reads it, as a scheduled run's
        // is (`Clocked`): a `check` asked at one instant and a body run at
        // a later one could pass an event its body then reads as past, or
        // refuse one its body would take.
        let now = self.now()?;
        let before = self.clock.replace(now);
        let c = Clocked { db: self, before };
        c.db.take_at(e, &key, now)
    }

    /// [`Self::take_event`] with the clock fixed to `now`.
    fn take_at(&mut self, e: &Event, key: &str, now: i64) -> Result<Took> {
        if let Some(k) = self.kept(key, e.digest)? {
            return Ok(Took::Held(k));
        }
        let takers = self.judged(e, now)?;
        if takers.is_empty() {
            return Ok(Took::Passed);
        }
        // A storage error, a replica's: refused before anything runs, the
        // lease asked as the block lands.
        self.may_start()?;
        if self.block.is_some() {
            return self.taking(e, key, &takers, now).map(Took::Ran);
        }
        self.whole(|db| {
            db.open_block();
            match db.taking(e, key, &takers, now) {
                Ok(n) => db.commit().map(|_| Took::Ran(n)),
                Err(err) => {
                    db.rollback();
                    Err(err)
                }
            }
        })
    }

    /// `key`'s row, read as every read is: past its `until`, as if never
    /// kept.
    fn kept(&self, key: &str, digest: u64) -> Result<Option<HeldKey>> {
        if !self.collections.contains_key(HOOK_EVENTS) {
            return Ok(None);
        }
        let read = Statement::Select(Select {
            collection: HOOK_EVENTS.into(),
            project: Some(vec!["digest".into(), "taken".into()]),
            filter: Some(key_is()),
            ..Select::default()
        });
        let Response::Rows(rs) = self.query(&read, &[Value::Text(key.into())])? else {
            return Ok(None);
        };
        Ok(match rs.rows.first().map(|r| r.values.as_slice()) {
            Some([Value::Int(d), Value::Int(taken)]) => Some(HeldKey {
                taken: *taken as u32,
                same: *d == digest as i64,
            }),
            _ => None,
        })
    }

    /// The statements of `e`'s hook that take it, in name order, each with
    /// the values its parameters read from it, each `check` asked at `now`,
    /// the taking's instant its bodies read; refused as
    /// [`Self::take_event`] says. Over `&Database`: nothing of it writes,
    /// and a caller judging before the write lock hands it the instant it
    /// takes the event at.
    fn judged(&self, e: &Event, now: i64) -> Result<Vec<Callee>> {
        let hook = e.hook;
        let of_hook = || (self.statements.iter()).filter(|s| s.hook.as_deref() == Some(hook));
        if of_hook().next().is_none() {
            return Err(Error::Exists(format!("no statement takes hook `{hook}`")));
        }
        // One held unread -- a text a later grammar reads otherwise, a
        // process with no reader -- has a condition no one here knows:
        // every event of its hook is refused, never one it would have
        // taken passed over as taken by none.
        if let Some(s) = of_hook().find(|s| s.tree.is_none()) {
            return Err(Error::Exists(format!(
                "statement `{}`, which takes hook `{hook}`, is held unread: {}: define it again \
                 or drop it",
                s.name,
                s.broken
                    .as_deref()
                    .unwrap_or("no FennecQL reader is installed")
            )));
        }
        let ctx = EvalCtx {
            params: &[],
            registry: &self.registry,
            clock: Some(now),
        };
        let mut takers = Vec::new();
        for s in of_hook() {
            let (Some(def), Some(h)) = (&s.tree, s.tree.as_ref().and_then(|d| d.hook.as_ref()))
            else {
                continue;
            };
            if let Some(c) = &h.condition {
                if !truthy(&eval(c, &mut EventRow(e), &ctx)?) {
                    continue;
                }
            }
            // Broken by its body, a schema change by an internal writer
            // under what it reads or writes: its condition, of the event
            // and literals alone, still reads, so only the events it
            // takes are refused, its hook's others taken as before.
            if let Some(why) = &s.broken {
                return Err(Error::Exists(format!(
                    "statement `{}`, which takes hook `{hook}`, is broken: {why}: define it again \
                     or drop it",
                    s.name
                )));
            }
            let values = typed(def, &h.sources, e)?;
            if let Some(check) = &def.check {
                let ctx = EvalCtx {
                    params: &values,
                    ..ctx
                };
                if eval(check, &mut NoRow, &ctx)? != Value::Bool(true) {
                    return Err(Error::Query(format!(
                        "statement `{}`: its `check` does not hold for this event",
                        s.name
                    )));
                }
            }
            takers.push(Callee {
                def: Arc::clone(def),
                values,
                slot: None,
            });
        }
        Ok(takers)
    }

    /// `takers`' bodies run in the block open, in their order, the clock
    /// fixed to `now`, then `key`'s row: how many took the event.
    fn taking(&mut self, e: &Event, key: &str, takers: &[Callee], now: i64) -> Result<u32> {
        // As a block of statements starts: rows a panic left woken -- a
        // statement unwound out of a block its caller opened, which no
        // `whole` settled -- are no binding's of this event's
        // (`rows_a_panic_left_woken_wake_nothing_in_the_next_event`). No
        // `disarm`, which `execute_many` and a lone statement ask: nothing
        // here offers a preparation (`offer`), so a put of a body builds
        // its own documents, and one a panic left is let go of by the next
        // block that offers one.
        self.settle_woken();
        let until = now
            .checked_add(e.keep)
            .ok_or_else(|| Error::Query("a hook's `keep` runs past the clock".into()))?;
        let taken = takers.len() as u32;
        // One budget for every statement the event wakes and their
        // bindings, fresh an event.
        self.bounded(|db| {
            for t in takers {
                for s in &t.def.body {
                    db.run_one(s, &t.values)?;
                }
            }
            db.keep_key(key, e.digest, taken, until)
        })?;
        Ok(taken)
    }

    /// `key`'s row, the block's last write: the collection made where the
    /// first event finds none.
    fn keep_key(&mut self, key: &str, digest: u64, taken: u32, until: i64) -> Result<()> {
        if !self.collections.contains_key(HOOK_EVENTS) {
            let made = Statement::CreateCollection {
                schema: keys_schema()?,
                if_not_exists: true,
            };
            self.run_one(&made, &[])?;
        }
        let lit = |v: Value| Expr::Lit(v);
        let row = Statement::Put {
            collection: HOOK_EVENTS.into(),
            docs: vec![vec![
                ("key".into(), lit(Value::Text(key.into()))),
                ("digest".into(), lit(Value::Int(digest as i64))),
                ("taken".into(), lit(Value::Int(taken as i64))),
                ("until".into(), lit(Value::Timestamp(until))),
            ]],
            docs_param: None,
            insert: false,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        };
        self.run_one(&row, &[]).map(drop)
    }
}

/// `key = $1`.
fn key_is() -> Expr {
    Expr::Cmp(
        CmpOp::Eq,
        Box::new(Expr::Field("key".into())),
        Box::new(Expr::Param(0)),
    )
}

/// [`HOOK_EVENTS`]' fields: the key by its hash, the body's digest, the
/// statements that took it, and `until`, each row's own expiry -- a hook's
/// `keep` is its own, where `@ttl`'s duration is the collection's.
fn keys_schema() -> Result<Schema> {
    Schema::new(
        HOOK_EVENTS,
        vec![
            Field::new("key", DataType::Text).indexed(IndexKind::Hash { unique: false }),
            Field::new("digest", DataType::Int),
            Field::new("taken", DataType::Int),
            Field::new("until", DataType::Timestamp).indexed(IndexKind::Sorted { ttl: Some(1) }),
        ],
    )
}

/// `e`'s key as [`HOOK_EVENTS`] keeps it -- `<hook>`, `U+001F` and the key,
/// so one event sent to two hooks is taken once by each -- refused (400)
/// for a key or a body no event has, or no `keep`.
fn kept_key(e: &Event) -> Result<String> {
    // No control character: `U+001F` parts the hook from the key, and a
    // hook's name is a word.
    if e.key.is_empty() || e.key.len() > 255 || e.key.chars().any(char::is_control) {
        return Err(Error::Query(
            "an event's key is 1 to 255 bytes, with no control character".into(),
        ));
    }
    if !matches!(e.body, Value::Object(_)) {
        return Err(Error::Query("an event's body is a JSON object".into()));
    }
    if e.keep <= 0 {
        return Err(Error::Query(
            "a hook keeps an event's key for a time: its `keep` is past nothing".into(),
        ));
    }
    let mut k = String::with_capacity(e.hook.len() + 1 + e.key.len());
    k.push_str(e.hook);
    k.push('\u{1f}');
    k.push_str(e.key);
    Ok(k)
}

/// Each parameter of `def` read from `e` as its source says, by its type
/// as a document's field of that type reads a value -- no cast: a text of
/// digits is no `int`. A path leading nowhere or to `null` is a missing
/// value, its default where it has one, refused (400) otherwise, naming
/// the parameter and what it reads.
fn typed(def: &Definition, sources: &[Source], e: &Event) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(def.params.len());
    for (p, src) in def.params.iter().zip(sources) {
        let v = match src {
            Source::Event(keys) if keys.is_empty() => e.body.clone(),
            Source::Event(keys) => event_at(e.body, keys).clone(),
            Source::HookId => Value::Text(e.key.into()),
            Source::HookAt => e.at.map_or(Value::Null, Value::Timestamp),
            Source::HookEvent => e.event.map_or(Value::Null, |t| Value::Text(t.into())),
        };
        let read = || match src {
            Source::Event(keys) if keys.is_empty() => "event".to_string(),
            Source::Event(keys) => format!("event.{keys}"),
            Source::HookId => "hook.id".into(),
            Source::HookAt => "hook.at".into(),
            Source::HookEvent => "hook.event".into(),
        };
        let v = match (v, &p.default) {
            (Value::Null, Some(d)) => d.clone(),
            (Value::Null, None) => {
                return Err(Error::Query(format!(
                    "statement `{}`: `${}`, {}, reads `{}`, which the event does not hold",
                    def.name,
                    p.name,
                    p.ty,
                    read()
                )))
            }
            (v, _) => v,
        };
        if v.is_null() {
            out.push(Value::Null);
            continue;
        }
        let found = v.type_name();
        out.push(v.coerce(&p.ty).map_err(|_| {
            Error::Query(format!(
                "statement `{}`: `${}` is {}, and `{}` holds {found}",
                def.name,
                p.name,
                p.ty,
                read()
            ))
        })?);
    }
    Ok(out)
}
