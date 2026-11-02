//! Write-bound statements run (`docs/design/named-statements.md`, *When a
//! binding runs*): in the block of the write that woke them, once a row,
//! after the statement's own rows.
//!
//! A writing statement over a collection whose mask (`Collection::bound`)
//! holds what it may do to a row asks [`Database::bind`] once, before it
//! changes anything: a broken or unread binding refuses the write there,
//! and otherwise the bindings it may wake are held for the statement. Each
//! row is then asked their conditions as it is written, beside
//! `after_write`, and only a row passing one is kept, with its pair and the
//! bindings it passed ([`Woken::keep`]): a `set` over a million rows of
//! which twelve pass keeps twelve pairs. As the statement ends --
//! its rows, hooks and `require` behind it -- [`Database::run_one`] runs
//! the kept rows' bindings, rows first and then names, each body's
//! statements through `run_one` with the row's values by place, waking
//! nothing (depth one). An error of any refuses the writer's statement,
//! whose block is put back as any block whose statement fails.
//!
//! A collection no statement is bound to pays the byte's look a writing
//! statement, and `run_one` one more, `Database::bound_due`, as it ends: no look
//! as a statement starts and none a row (`Database::erase` keeps a
//! `del`'s row where it reads it, and a `put`'s row asks `wake` beside a
//! `returning`'s look); nothing here is on its path.

use std::cell::Cell;
use std::sync::Arc;

use super::{Database, NoRow};
use crate::collate::TextOrder;
use crate::error::{Error, Result};
use crate::named::{Definition, MAX_WOKEN};
use crate::plugin::Registry;
use crate::query::{eval, EvalCtx, Expr, Response, RowAccess};
use crate::schema::{Schema, SchemaCell};
use crate::value::{DocId, Document, Value};

/// What the statement running woke: the bindings it may wake, and each
/// row passing one's condition with its pair. Taken by `run_one` as the
/// statement ends, run where it succeeded and let go of where it failed.
#[derive(Default)]
pub(super) struct Woken {
    /// The bindings the statement may wake, in name order: a row's
    /// `passed` is a bit each, by place (`named::MAX_BOUND` is 16).
    bound: Vec<Arc<Definition>>,
    /// Which side of the pair each binding's body reads, by place: a bit
    /// for `new.` ([`NEW_SIDE`]) and one for `old.` ([`OLD_SIDE`]). A row
    /// keeps a copy of a side only where a binding it passed reads it: a
    /// `set` binding reading `new.` alone keeps no `old` row.
    sides: Vec<u8>,
    /// The bound collection's schema, which a condition compares each
    /// field by (its collation and case fold, `Field::text_order`).
    schema: Option<SchemaCell>,
    /// Each row passing a condition, in the order the statement wrote
    /// them.
    pub(super) rows: Vec<Woke>,
    /// `now()`, read as the first row is asked a condition: the one time
    /// every condition and every run of the write reads.
    now: Option<i64>,
    /// The binding that refused the last write refused by one, for the
    /// server to tell ([`Database::woken`]); let go of as the next
    /// statement ends, or a block starts (`Database::bound_due`).
    refused_by: Option<String>,
}

/// A row that woke a binding: as written (`new.`), as it stood (`old.`),
/// and the bindings it passed, a bit each in [`Woken::bound`]'s order.
pub(super) struct Woke {
    new: Option<Document>,
    old: Option<Document>,
    passed: u32,
}

thread_local! {
    /// The rows asked a condition and the rows kept, on this thread: what
    /// [`Database::bound_counts`] answers a test holding that a write to a
    /// collection no statement is bound to asks and keeps none. Counted on
    /// the bound path alone.
    static COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

impl Woken {
    /// A row the statement wrote -- an `event` of its collection's, with
    /// the row as it stood and as written -- asked each binding of that
    /// event's condition, and kept with the bindings it passed where it
    /// passed one. The 10 001st row passing one refuses the write as it is
    /// written, never cut: no binding runs a part of a write.
    pub(super) fn keep(
        &mut self,
        registry: &Registry,
        clock: Option<i64>,
        event: u8,
        old: Option<&Document>,
        new: Option<&Document>,
    ) -> Result<()> {
        COUNTS.with(|c| c.set((c.get().0 + 1, c.get().1)));
        let now = match self.now {
            Some(t) => t,
            None => {
                // As the writer's statement reads it: the database's clock,
                // else the registry's `now`.
                let ctx = EvalCtx {
                    params: &[],
                    registry,
                    clock,
                };
                let t = match eval(&Expr::Call("now".into(), Vec::new()), &mut NoRow, &ctx)? {
                    Value::Timestamp(t) => t,
                    other => {
                        return Err(Error::Type(format!(
                            "`now()` answered {}, not a timestamp",
                            other.type_name()
                        )))
                    }
                };
                self.now = Some(t);
                t
            }
        };
        let ctx = EvalCtx {
            params: &[],
            registry,
            clock: Some(now),
        };
        let schema = self.schema.as_deref().expect("bound before a row is kept");
        let (mut passed, mut sides) = (0u32, 0u8);
        for (i, def) in self.bound.iter().enumerate() {
            let Some(b) = def.after.as_deref() else {
                continue;
            };
            if b.events & event == 0 {
                continue;
            }
            let pass = match &b.condition {
                None => true,
                Some(c) => {
                    let mut pair = Pair { new, old, schema };
                    eval(c, &mut pair, &ctx)? == Value::Bool(true)
                }
            };
            if pass {
                passed |= 1 << i;
                sides |= self.sides[i];
            }
        }
        if passed == 0 {
            return Ok(());
        }
        if self.rows.len() >= MAX_WOKEN {
            return Err(past_woken());
        }
        COUNTS.with(|c| c.set((c.get().0, c.get().1 + 1)));
        self.rows.push(Woke {
            new: new.filter(|_| sides & NEW_SIDE != 0).cloned(),
            old: old.filter(|_| sides & OLD_SIDE != 0).cloned(),
            passed,
        });
        Ok(())
    }
}

/// A write past `MAX_WOKEN` woken rows: a query error naming the bound,
/// its block put back.
#[cold]
#[inline(never)]
fn past_woken() -> Error {
    Error::Query(format!(
        "a write wakes the statements bound to it for at most {MAX_WOKEN} rows, and this one \
         woke more: nothing it wrote landed -- write fewer rows a statement"
    ))
}

/// The pair a binding's condition reads: `new.<field>`, the row as
/// written, and `old.<field>`, the row as it stood, each compared in its
/// field's collation and case fold as a read's filter compares it. A
/// side the event has none of reads as `null`, which no definition reaches:
/// `old.` in an `insert` binding and `new.` in a `del` one are refused at
/// the parse.
struct Pair<'a> {
    new: Option<&'a Document>,
    old: Option<&'a Document>,
    schema: &'a Schema,
}

/// What `new.` and `old.` name.
const NEW: &str = "new.";
const OLD: &str = "old.";

/// A binding's body reads the row as written, or as it stood.
const NEW_SIDE: u8 = 1;
const OLD_SIDE: u8 = 2;

impl RowAccess for Pair<'_> {
    fn id(&self) -> DocId {
        self.new.or(self.old).map_or(0, |d| d.id)
    }
    fn field(&mut self, name: &str) -> Result<Value> {
        let (doc, f) = match (name.strip_prefix(NEW), name.strip_prefix(OLD)) {
            (Some(f), _) => (self.new, f),
            (_, Some(f)) => (self.old, f),
            _ => {
                return Err(Error::Query(format!(
                    "`{name}`: a binding's condition reads `new.` and `old.` alone"
                )))
            }
        };
        Ok(doc.map_or(Value::Null, |d| value_at(d, f)))
    }
    fn text_order(&self, name: &str) -> TextOrder {
        let f = (name.strip_prefix(NEW))
            .or_else(|| name.strip_prefix(OLD))
            .unwrap_or(name);
        self.schema
            .field(f)
            .map_or(TextOrder::BYTES, |f| f.text_order())
    }
}

/// A field of a row, or a path into a `json` one, `id` the row's id.
fn value_at(d: &Document, f: &str) -> Value {
    match f {
        "id" => Value::Int(d.id as i64),
        f => d.at(f).cloned().unwrap_or(Value::Null),
    }
}

impl Database {
    /// A writing statement over `collection` that may cause `events`, which
    /// its mask holds: whether its rows are to be asked the conditions. A
    /// writer that wakes nothing ([`Self::unbound`], a binding's own runs)
    /// asks none. A broken binding of those events, or one this process
    /// holds unread, refuses the write before it changes anything, the
    /// condition unasked: no more to be trusted than its body, and a write
    /// landing without its rows is what a binding must never let happen.
    /// Asked once a statement, out of line: a collection no statement is
    /// bound to never comes here.
    #[cold]
    #[inline(never)]
    pub(super) fn bind(&mut self, collection: &str, events: u8) -> Result<bool> {
        if self.unbound {
            return Ok(false);
        }
        let t = Arc::clone(&self.statements);
        let mut bound = Vec::new();
        for s in t.bound_to(collection) {
            if s.after.as_ref().is_none_or(|a| a.1 & events == 0) {
                continue;
            }
            match (&s.broken, &s.tree) {
                (None, Some(tree)) => bound.push(Arc::clone(tree)),
                _ => return Err(self.bound_refused(collection, events)),
            }
        }
        if bound.is_empty() {
            return Ok(false);
        }
        let schema = self.collection(collection)?.schema.clone();
        let w = &mut self.woken;
        w.sides = (bound.iter())
            .map(|d| {
                d.after.as_ref().map_or(0, |b| {
                    b.places.iter().fold(0, |s, p| match p.old {
                        true => s | OLD_SIDE,
                        false => s | NEW_SIDE,
                    })
                })
            })
            .collect();
        w.bound = bound;
        w.schema = Some(schema);
        w.rows.clear();
        w.now = None;
        self.bound_due = true;
        Ok(true)
    }

    /// The statement `out` answered has ended with a binding's work due
    /// (`Database::bound_due`): the note of the statement before let go of, and
    /// where it woke rows and succeeded, their bindings run, its answer
    /// standing only where every run did; where it failed, the rows are
    /// let go of with the block its caller puts back. Rows a panic left
    /// half way are run as the next lone statement ends, in the block the
    /// panic left open, which that statement runs in
    /// ([`Database::execute_with`]): they land with the rows that woke
    /// them or not at all. A block starting lets them go
    /// ([`Self::settle_woken`]).
    #[cold]
    #[inline(never)]
    pub(super) fn ended(&mut self, out: Result<Response>) -> Result<Response> {
        // The note let go of with the rows: in the box, which keeps its
        // allocation.
        self.bound_due = false;
        let w = std::mem::take(&mut *self.woken);
        if w.rows.is_empty() {
            return out;
        }
        let out = out?;
        self.wake(w)?;
        Ok(out)
    }

    /// The binding `name` refused the statement running: noted for the
    /// server to tell, and let go of as the next statement ends.
    fn refused(&mut self, name: &str) {
        self.woken.refused_by = Some(name.to_string());
        self.bound_due = true;
    }

    /// The bindings `w`'s rows woke, run: each hook asked first whether
    /// the writer may wake each (`Hook::waking`, where a policy meets a
    /// binding), then the rows in the order written and, on each, the
    /// bindings it passed in name order, every body's statements through
    /// `run_one` with the row's values by place. The runs read the
    /// waking statement's `now()`, wake nothing themselves (depth one)
    /// and examine at most `--call-max-rows` rows together: a fresh budget
    /// for a statement a client sent, what the call has left inside a
    /// call's body. Each given back as the runs end, on a panic too.
    fn wake(&mut self, w: Woken) -> Result<()> {
        for (i, def) in w.bound.iter().enumerate() {
            let rows = w.rows.iter().filter(|r| r.passed & (1 << i) != 0).count();
            if rows == 0 {
                continue;
            }
            for h in self.registry.hooks() {
                if let Err(e) = h.waking(&def.name, rows) {
                    self.refused(&def.name);
                    return Err(e);
                }
            }
        }
        let now = w.now.or(self.clock);
        let clock = std::mem::replace(&mut self.clock, now);
        let unbound = std::mem::replace(&mut self.unbound, true);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match *self.calls.get_mut() {
                0 => self.bounded(|db| db.runs(&w)),
                _ => self.runs(&w),
            }
        }));
        self.clock = clock;
        self.unbound = unbound;
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    }

    /// Each kept row's bindings run, the binding that refused noted.
    fn runs(&mut self, w: &Woken) -> Result<()> {
        for row in &w.rows {
            for (i, def) in w.bound.iter().enumerate() {
                if row.passed & (1 << i) == 0 {
                    continue;
                }
                let Some(b) = def.after.as_deref() else {
                    continue;
                };
                let values: Vec<Value> = (b.places.iter())
                    .map(|p| {
                        let doc = if p.old { &row.old } else { &row.new };
                        doc.as_ref().map_or(Value::Null, |d| value_at(d, &p.field))
                    })
                    .collect();
                for s in &def.body {
                    if let Err(e) = self.run_one(s, &values) {
                        self.refused(&def.name);
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Rows a statement left kept, or a refusal's note, as a block or a
    /// reading call starts, which no statement's end may settle: what a
    /// panic left half way, let go of so that no block runs another's
    /// bindings. A lone statement settles as it ends ([`Self::ended`]).
    #[inline]
    pub(super) fn settle_woken(&mut self) {
        if self.bound_due {
            self.woken.rows.clear();
            self.bound_due = false;
            self.woken.refused_by = None;
        }
    }

    /// The statement bound to a collection's writes that refused the last
    /// write one refused -- its guard, a unique value, a bound, a hook --
    /// by name, for the server to tell its writer; `None` where none did.
    /// Read under the lock the write ran under, before the next statement.
    pub fn woken(&self) -> Option<&str> {
        self.woken.refused_by.as_deref()
    }

    /// The rows asked a binding's condition and the rows kept, on this
    /// thread, since it started: a test's look that a write to a
    /// collection no statement is bound to asks and keeps none.
    #[doc(hidden)]
    pub fn bound_counts() -> (usize, usize) {
        COUNTS.with(Cell::get)
    }
}
