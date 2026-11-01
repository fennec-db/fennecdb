//! A named statement called, listed and checked at its definition
//! (`docs/design/named-statements.md`, *Calls* and *Definitions*).
//!
//! A call is never read-only by its statement (`Statement::is_read_only`):
//! whether it writes is its definition's, which a `drop` and a `create` may
//! change between a question asked before a lock and the lock. So a call is
//! asked again under the lock it runs under ([`Database::call_reads`]). One
//! whose body writes runs under the write lock, inside the block the caller
//! opened: its statements land as one record with whatever else that block
//! holds, or none does. One whose body only reads runs through the read
//! executor ([`Database::read_call`]) over `&Database`, which cannot write
//! by its type -- under the read lock beside the other readers, or pinned
//! ([`Database::pin`]) -- and refuses a body that writes: a replica, a
//! read-only server and a marked read answer it, and it takes no write
//! lock, opens no block and moves no counter. Either way it is judged in
//! one order -- the statement there, the caller's claims, the arguments,
//! `check`, whether it is broken -- each answered before the next is
//! asked, and run with the definition the table holds under that lock: a
//! replacement lands under the write lock, so a call runs one version
//! whole. Who may call it, and with which claims, is the server's
//! (`fennec_http::access`); the engine knows nothing of tokens and runs the
//! text with the claims it is handed.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use super::{Database, NoRow};
use crate::error::{Error, Result};
use crate::named::{Args, Call, Definition, MAX_CLAIM_VALUES};
use crate::query::{eval, CmpOp, EvalCtx, Expr, Response, ResultSet, Row, Select, Sort, Statement};
use crate::schema::{IndexKind, Schema};
use crate::value::Value;

/// What a call is judged to run: the definition the table holds, and the
/// values its body is handed by place -- the arguments in the signature's
/// order, then the claims in theirs, as the parser numbered them.
pub struct Callee {
    pub def: Arc<Definition>,
    pub values: Vec<Value>,
}

/// A call that reads, answered ([`Database::read_call_json`]): its last
/// statement's rows written out as JSON, this many, or its answer.
pub enum CallRead {
    Json(usize),
    Answer(Response),
}

impl Database {
    /// `--call-max-rows`: the rows a call's statements may examine, counted
    /// where a scan, an index or an ordered walk hands them over; past it
    /// the call is refused and its block put back, never answered from the
    /// rows it reached ([`crate::named::CALL_MAX_ROWS`] unless told).
    pub fn set_call_max_rows(&mut self, n: usize) {
        self.call_max_rows = n.max(1);
    }

    /// The call judged, in the order a caller is answered: the statement
    /// (404), the claims it declares (403), the arguments (400), `check`
    /// (400) and whether it can run (409). Nothing of the body runs. The
    /// server asks it under the lock it runs the call under, before it holds
    /// the body to what the token may do (`append-only`), so that refusal
    /// comes after these, as a call's answers are ordered.
    pub fn callee(&self, call: &Call, params: &[Value]) -> Result<Callee> {
        let s = self
            .statements
            .get(&call.name)
            .ok_or_else(|| missing(&call.name))?;
        // Judged by the tree it runs, or, where its text reads as another
        // statement here, by that one's signature: the mark below comes
        // after the claims, the arguments and `check`, as the design orders
        // a call's refusals. A text this build reads as nothing at all --
        // or a process that installed no reader -- holds no claim to judge,
        // and is refused at once.
        let Some(def) = s.tree.as_ref().or(s.misread.as_ref()) else {
            return Err(broken(&call.name, s.broken.as_deref()));
        };
        let ctx = EvalCtx {
            params,
            registry: &self.registry,
            clock: self.clock,
        };
        let claims = claims(def, call.claims.as_ref(), &ctx)?;
        let mut values = arguments(def, &call.args, &ctx)?;
        values.extend(claims);
        if let Some(check) = &def.check {
            let ctx = EvalCtx {
                params: &values,
                ..ctx
            };
            if eval(check, &mut NoRow, &ctx)? != Value::Bool(true) {
                return Err(Error::Query(format!(
                    "statement `{}`: its `check` does not hold for these arguments",
                    def.name
                )));
            }
        }
        if let Some(why) = &s.broken {
            return Err(broken(&call.name, Some(why)));
        }
        // Only the tree it was defined as runs: a misread one is marked.
        let Some(def) = &s.tree else {
            return Err(broken(&call.name, None));
        };
        Ok(Callee {
            def: Arc::clone(def),
            values,
        })
    }

    /// `call <name> ...`: judged ([`Self::callee`]), then each statement of
    /// the body run with the call's values in the block open, its rows
    /// counted against `--call-max-rows`. The answer is the last
    /// statement's, as PostgreSQL's SQL functions answer theirs: an answer
    /// of every statement would make each guard's rows part of the API.
    pub(super) fn call(&mut self, call: &Call, params: &[Value]) -> Result<Response> {
        let callee = self.callee(call, params)?;
        // A body of reads in a block -- a `/batch`'s line -- reads as it
        // would alone, the block's writes before it among what it reads.
        if !callee.def.writes() {
            return self.read_call(&callee);
        }
        self.bounded(|db| {
            let mut last = Ok(Response::Affected(0));
            for s in &callee.def.body {
                last = db.run_one(s, &callee.values);
                if last.is_err() {
                    break;
                }
            }
            last
        })
    }

    /// Whether the statement `call` names only reads, as the table stands
    /// under the lock this is asked under: `Some(true)` where it reads and
    /// can run, `Some(false)` where its body writes, and `None` where no
    /// statement of that name can run here -- none is there, it is broken,
    /// or its text reads here as another statement -- which the call's
    /// judgment then answers ([`Self::callee`]) wherever it is asked. A
    /// replacement keeps whether a statement writes (its contract), so only
    /// a `drop` and a `create` change the answer, each under the write
    /// lock: asked under a read lock, it holds for as long as that lock.
    pub fn call_reads(&self, call: &Call) -> Option<bool> {
        let s = self.statements.get(&call.name)?;
        if s.broken.is_some() {
            return None;
        }
        s.tree.as_ref().map(|d| !d.writes())
    }

    /// The body of the statement `call` names, for a caller to ask what
    /// it writes -- `--max-memory` holds a call as it holds the writes its
    /// body makes (`fennec_http::over_ceiling`) -- or `None` where no
    /// statement of that name can run here, as [`Self::call_reads`]
    /// answers. Asked under a lock, it holds for as long as that lock.
    pub fn call_body(&self, call: &Call) -> Option<&[Statement]> {
        let s = self.statements.get(&call.name)?;
        if s.broken.is_some() {
            return None;
        }
        s.tree.as_ref().map(|d| &d.body[..])
    }

    /// The read executor: a call judged ([`Self::callee`]) whose body only
    /// reads, each statement of it run as [`Self::query`] runs one, with the
    /// call's values and its rows counted against `--call-max-rows`; the
    /// answer is the last statement's. Over `&Database`, it cannot write: a
    /// body that writes is refused ([`Error::ReadOnly`]), never run here,
    /// for its caller to run it under the write lock or, where writes are
    /// not taken, to refuse it. It opens no block, asks no lease and moves
    /// no counter.
    pub fn read_call(&self, callee: &Callee) -> Result<Response> {
        if callee.def.writes() {
            return Err(Error::ReadOnly(format!(
                "statement `{}` writes, and only a statement that reads runs here",
                callee.def.name
            )));
        }
        let _bound = Bound::new(self);
        let mut last = Ok(Response::Affected(0));
        for s in &callee.def.body {
            last = self.query(s, &callee.values);
            if last.is_err() {
                break;
            }
        }
        last
    }

    /// [`Self::read_call`], its last statement written straight onto `out`
    /// as JSON where [`Self::query_json`] writes it -- a plain `get` -- and
    /// answered as `read_call` answers it otherwise, `out` as it was. The
    /// statements before it are its guards, read as `query` reads them. A
    /// call's answer is its last statement's, and that one decoded into
    /// values to be written out again cost a call by id a third more than
    /// the `get` it runs, sent alone.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_call_json(&self, callee: &Callee, out: &mut String) -> Result<CallRead> {
        let Some((last, guards)) = callee.def.body.split_last() else {
            return self.read_call(callee).map(CallRead::Answer);
        };
        if callee.def.writes() {
            return self.read_call(callee).map(CallRead::Answer);
        }
        let _bound = Bound::new(self);
        for s in guards {
            self.query(s, &callee.values)?;
        }
        Ok(match self.query_json(last, &callee.values, out)? {
            Some(n) => CallRead::Json(n),
            None => CallRead::Answer(self.query(last, &callee.values)?),
        })
    }

    /// `f` -- a call's body run under the write lock -- with this thread's
    /// rows bounded by `--call-max-rows` and the call counted among the
    /// database's, both given back as it returns and as a panic unwinds out
    /// of it alike: given back only on a return, a hook's panic left the
    /// bound set, and every later statement of the database, a plain `get`
    /// among them, was refused as a call past its rows until it was opened
    /// again -- a connection's panic must unwind alone. [`Bound`]'s work,
    /// for a body that takes the database whole.
    fn bounded<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        *self.calls.get_mut() += 1;
        let before = LEFT.with(|l| l.replace(self.call_max_rows));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        LEFT.with(|l| l.set(before));
        *self.calls.get_mut() -= 1;
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    }

    /// What of a call a json field needs read as written
    /// ([`Database::exactly`]): an argument or a claim of a type keeping a
    /// list of numbers as written -- `json`, `geo`, a list of them -- given
    /// in the text, or the parameter holding them, which is then read as
    /// written whole.
    pub(super) fn call_exactly(&self, c: &Call, out: &mut super::Exactly) {
        let Some(def) = self.statements.get(&c.name).and_then(|s| s.tree.as_deref()) else {
            return;
        };
        let params: Vec<(&str, bool)> = def
            .params
            .iter()
            .map(|p| (&*p.name, p.ty.keeps_numbers()))
            .collect();
        let claims: Vec<(&str, bool)> = def
            .claims
            .iter()
            .map(|c| (c.claim(), c.ty.keeps_numbers()))
            .collect();
        exact_args(&c.args, &params, out);
        if let Some(a) = &c.claims {
            exact_args(a, &claims, out);
        }
    }

    /// `statements`: each named statement the database holds, by name.
    pub(super) fn list_statements(&self) -> Response {
        let rows = self.statements.iter().map(|s| row(s, false)).collect();
        Response::Rows(ResultSet {
            columns: columns(false),
            rows,
            ..ResultSet::default()
        })
    }

    /// `describe statement <name>`: its row, its text and what it depends
    /// on beside it. The server's token alone is answered it: a scoped
    /// token is told the statements it may call by `/_whoami`.
    pub(super) fn describe_statement(&self, name: &str) -> Result<Response> {
        let s = self.statements.get(name).ok_or_else(|| missing(name))?;
        Ok(Response::Rows(ResultSet {
            columns: columns(true),
            rows: vec![row(s, true)],
            ..ResultSet::default()
        }))
    }

    /// What a definition's body is held to against the schema, beside its
    /// dependencies ([`Definition::depends`]): each field a statement of it
    /// writes or filters on held by its collection, and -- where the body
    /// writes, so that every call runs under the write lock -- each
    /// statement's rows narrowed by an index or an ordered walk, unless the
    /// definition says `scans`: a body that walks a collection would walk
    /// it at every call every granted token makes, every other writer
    /// waiting. `--call-max-rows` bounds what one call may cost; this keeps
    /// the cost out of the definitions that never meant it.
    pub(super) fn body_checked(&self, def: &Definition) -> Result<()> {
        let schema_of = |c: &str| self.collections.get(c).map(|c| &*c.schema);
        let holes = vec![Value::Null; def.params.len() + def.claims.len()];
        for (i, st) in def.body.iter().enumerate() {
            let at = |why: String| {
                Error::Query(format!(
                    "statement `{}`, statement {} of its body: {why}",
                    def.name,
                    i + 1
                ))
            };
            let Some((c, filter, pick)) = shape(st) else {
                continue;
            };
            let Some(schema) = schema_of(c) else {
                continue;
            };
            if let Some(name) = unheld(schema, st) {
                return Err(at(format!("`{c}` holds no field `{name}`")));
            }
            if def.writes() && !def.scans && !narrowed(schema, st, filter, pick, &holes) {
                return Err(at(format!(
                    "no index narrows the rows it reads of `{c}`, so every call would walk the \
                     collection under the write lock: index what it filters on, or say `scans`"
                )));
            }
        }
        Ok(())
    }

    /// What a call's statements may still examine; `usize::MAX` outside
    /// one.
    #[inline]
    pub(super) fn call_room(&self) -> usize {
        match self.calls.load(Relaxed) {
            0 => usize::MAX,
            _ => LEFT.with(Cell::get),
        }
    }

    /// `n` rows a statement examined, charged to the call running on this
    /// thread, if one is.
    #[inline]
    pub(super) fn examine(&self, n: usize) -> Result<()> {
        if self.calls.load(Relaxed) == 0 {
            return Ok(());
        }
        LEFT.with(|l| {
            let left = l.get();
            if left != usize::MAX {
                if n > left {
                    return Err(self.past_call_rows());
                }
                l.set(left - n);
            }
            Ok(())
        })
    }

    /// A call past `--call-max-rows`: a query error naming the bound, its
    /// block put back by whoever opened it.
    #[cold]
    #[inline(never)]
    pub(super) fn past_call_rows(&self) -> Error {
        Error::Query(format!(
            "a call examines at most {} rows (--call-max-rows), and this one reached for more: \
             nothing it wrote landed",
            self.call_max_rows
        ))
    }
}

thread_local! {
    /// The rows the call running on this thread may still examine,
    /// `usize::MAX` outside one: `--call-max-rows`, set as a call's body
    /// starts and given back as it ends ([`Bound`], `Database::bounded`).
    /// The thread's, not the database's: a call that reads runs under the
    /// read lock beside other reads -- a plain `get`, another call -- which
    /// a bound held in the database would charge, or refuse.
    static LEFT: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// A call's body reading over a database, its rows bounded on this thread
/// by `--call-max-rows` and the call counted among the database's, both
/// given back as it is dropped -- on a return and on a panic unwinding out
/// of the body alike.
struct Bound<'a> {
    calls: &'a AtomicUsize,
    before: usize,
}

impl<'a> Bound<'a> {
    fn new(db: &'a Database) -> Self {
        db.calls.fetch_add(1, Relaxed);
        let before = LEFT.with(|l| l.replace(db.call_max_rows));
        Bound {
            calls: &db.calls,
            before,
        }
    }
}

impl Drop for Bound<'_> {
    fn drop(&mut self) {
        LEFT.with(|l| l.set(self.before));
        self.calls.fetch_sub(1, Relaxed);
    }
}

/// A statement no database holds, or one a scoped token is not granted:
/// the same words for both ([`crate::named`]).
fn missing(name: &str) -> Error {
    Error::NotFound(format!("statement `{name}`"))
}

/// A statement that cannot run: 409, with why.
fn broken(name: &str, why: Option<&str>) -> Error {
    Error::Exists(format!(
        "statement `{name}` is broken: {}",
        why.unwrap_or("its text did not read")
    ))
}

/// A constant of a call: a literal, a parameter or an expression of them.
fn value_of(e: &Expr, ctx: &EvalCtx) -> Result<Value> {
    match e {
        Expr::Lit(v) => Ok(v.clone()),
        e => eval(e, &mut NoRow, ctx),
    }
}

/// `{a: 1, b: 2}` or `$n`'s object: its members by name.
fn named(args: &Args, ctx: &EvalCtx, what: &str) -> Result<Vec<(String, Value)>> {
    match args {
        Args::None => Ok(Vec::new()),
        Args::Named(pairs) => pairs
            .iter()
            .map(|(k, e)| Ok((k.clone(), value_of(e, ctx)?)))
            .collect(),
        Args::Param(i) => match ctx.params.get(*i) {
            Some(Value::Object(members)) => Ok(members.clone()),
            Some(Value::Null) => Ok(Vec::new()),
            Some(other) => Err(Error::Query(format!(
                "`${}` holds the {what} by name, an object; it is {}",
                i + 1,
                other.type_name()
            ))),
            None => Err(Error::Query(format!("`${}` is not given", i + 1))),
        },
        Args::Placed(..) => Err(Error::Query(format!(
            "the {what} are given by name, not by place"
        ))),
    }
}

/// The claims `def` declares, read from what the call carries -- the
/// token's claims a scoped call is bound with, or `as {...}` from the
/// server's token -- each held to its declared type. A claim missing, or
/// held as `null`, `""`, an empty list or a value of another type, is a
/// claim the caller lacks, and the call is refused (403) rather than run
/// with that value in place: a filter with a hole in it would match rows it
/// was never meant to, as a policy rule naming such a claim does not apply.
fn claims(def: &Definition, given: Option<&Args>, ctx: &EvalCtx) -> Result<Vec<Value>> {
    if def.claims.is_empty() {
        return Ok(Vec::new());
    }
    let carried = match given {
        Some(a) => named(a, ctx, "claims")?,
        None => Vec::new(),
    };
    claims_from(def, &carried)
}

/// Whether `carried` -- a token's claims -- holds every claim `def`
/// declares, as a call judges them ([`claims`]): what MCP asks before it
/// lists a statement's tool, so a token that may never call it is told
/// nothing of its arguments.
pub fn claims_carried(def: &Definition, carried: &[(String, Value)]) -> bool {
    claims_from(def, carried).is_ok()
}

/// [`claims`] of the claims a call carries, evaluated.
fn claims_from(def: &Definition, carried: &[(String, Value)]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(def.claims.len());
    for c in &def.claims {
        let lacks = || {
            Error::Denied(format!(
                "statement `{}` reads the caller's `{}` claim, as {}, and the call carries no \
                 such claim",
                def.name,
                c.claim(),
                c.ty
            ))
        };
        let v = carried
            .iter()
            .find(|(k, _)| k == c.claim())
            .map(|(_, v)| v.clone())
            .ok_or_else(lacks)?;
        if !usable(&v, true) {
            return Err(lacks());
        }
        let v = v.coerce(&c.ty).map_err(|_| lacks())?;
        // A list claim's every value names someone, or none is taken.
        if let Value::List(items) = &v {
            if items.is_empty() || !items.iter().all(|i| usable(i, false)) {
                return Err(lacks());
            }
        }
        // Spread after `in` on every call, as a policy's rule spreads one:
        // refused past the bound a policy holds it to, never cut to it.
        let n = match &v {
            Value::List(items) => items.len(),
            Value::Vector(items) => items.len(),
            _ => 0,
        };
        if n > MAX_CLAIM_VALUES {
            return Err(Error::Denied(format!(
                "statement `{}` reads the caller's `{}` claim, a list of at most {} values, and \
                 the call carries {n}",
                def.name,
                c.claim(),
                MAX_CLAIM_VALUES
            )));
        }
        out.push(v);
    }
    Ok(out)
}

/// Whether a claim's value names anyone: a text with something in it, a
/// number, a truth or a time, or a list of those -- never `null`, `""`, an
/// object or an empty list, as the policy's rules read one.
fn usable(v: &Value, list: bool) -> bool {
    match v {
        Value::Text(t) => !t.is_empty(),
        Value::Int(_) | Value::Bool(_) | Value::Timestamp(_) => true,
        Value::Float(f) => f.is_finite(),
        Value::List(items) => list && !items.is_empty() && items.iter().all(|i| usable(i, false)),
        Value::Vector(v) => list && !v.is_empty(),
        _ => false,
    }
}

/// The arguments matched to the signature and read by each parameter's
/// type, as a document's field of that type reads a value: one missing,
/// extra, given twice, `null` where it has no default, or of a type its
/// parameter refuses is a query error naming it (400).
fn arguments(def: &Definition, args: &Args, ctx: &EvalCtx) -> Result<Vec<Value>> {
    let refused = |why: String| Error::Query(format!("statement `{}`: {why}", def.name));
    let mut given: Vec<Option<Value>> = vec![None; def.params.len()];
    let by_name = match args {
        Args::Placed(p, n) => {
            if p.len() > def.params.len() {
                return Err(refused(format!(
                    "it takes {} arguments, and {} were given",
                    def.params.len(),
                    p.len()
                )));
            }
            for (slot, e) in given.iter_mut().zip(p) {
                *slot = Some(value_of(e, ctx)?);
            }
            let n = n
                .iter()
                .map(|(k, e)| Ok((k.clone(), value_of(e, ctx)?)))
                .collect::<Result<Vec<_>>>()?;
            n
        }
        a => named(a, ctx, "arguments")?,
    };
    for (k, v) in by_name {
        let Some(at) = def.params.iter().position(|p| p.name == k) else {
            return Err(refused(format!("it takes no argument `{k}`")));
        };
        if given[at].is_some() {
            return Err(refused(format!("the argument `{k}` is given twice")));
        }
        given[at] = Some(v);
    }
    let mut out = Vec::with_capacity(def.params.len() + def.claims.len());
    for (p, v) in def.params.iter().zip(given) {
        let v = match (v, &p.default) {
            (Some(v), _) => v,
            (None, Some(d)) => d.clone(),
            (None, None) => {
                return Err(refused(format!(
                    "the argument `{}`, {}, is missing",
                    p.name, p.ty
                )))
            }
        };
        if v.is_null() {
            if p.default.is_none() {
                return Err(refused(format!(
                    "the argument `{}`, {}, is never null",
                    p.name, p.ty
                )));
            }
            out.push(Value::Null);
            continue;
        }
        let found = v.type_name();
        out.push(v.coerce(&p.ty).map_err(|e| {
            refused(match e {
                Error::Type(m) if !m.starts_with("expected") => {
                    format!("the argument `{}`, {}: {m}", p.name, p.ty)
                }
                _ => format!("the argument `{}` is {}, found {found}", p.name, p.ty),
            })
        })?);
    }
    Ok(out)
}

/// [`Database::call_exactly`] over one side of a call, its names and
/// whether each keeps a list of numbers as written, by place.
fn exact_args(args: &Args, keeps: &[(&str, bool)], out: &mut super::Exactly) {
    let by_name = |k: &str| keeps.iter().any(|&(n, kept)| kept && n == k);
    let named =
        |pairs: &[(String, Expr)]| pairs.iter().any(|(k, e)| e.reads_vectors() && by_name(k));
    match args {
        Args::Param(i) if keeps.iter().any(|&(_, kept)| kept) => out.params.push(*i),
        Args::Named(pairs) => out.text |= named(pairs),
        Args::Placed(placed, pairs) => {
            let at = |j: usize| keeps.get(j).is_some_and(|&(_, kept)| kept);
            out.text |= named(pairs)
                || placed
                    .iter()
                    .enumerate()
                    .any(|(j, e)| e.reads_vectors() && at(j));
        }
        _ => {}
    }
}

/// `statements`' columns, and `describe statement`'s after them.
fn columns(described: bool) -> Vec<String> {
    let mut c = vec![
        "name",
        "version",
        "signature",
        "writes",
        "comment",
        "broken",
    ];
    if described {
        c.extend(["answers", "text", "depends"]);
    }
    c.into_iter().map(String::from).collect()
}

/// A statement as `statements` and `describe statement` answer it.
fn row(s: &super::Stored, described: bool) -> Row {
    let text = |t: &str| Value::Text(t.to_string());
    let def = s.tree.as_deref();
    let mut values = vec![
        text(&s.name),
        Value::List(vec![
            Value::Int(s.version.0 as i64),
            Value::Int(s.version.1 as i64),
        ]),
        def.map_or(Value::Null, |d| Value::Text(signature(d))),
        def.map_or(Value::Null, |d| Value::Bool(d.writes())),
        def.and_then(|d| d.comment.as_deref())
            .map_or(Value::Null, text),
        s.broken.as_deref().map_or(Value::Null, text),
    ];
    if described {
        values.push(def.map_or(Value::Null, |d| {
            Value::List(d.answers().into_iter().map(Value::Text).collect())
        }));
        values.push(text(&s.text));
        values.push(Value::List(s.depends.iter().map(|d| text(d)).collect()));
    }
    Row {
        id: 0,
        values,
        score: None,
    }
}

/// `($from int, $to int) claims (sub text)`, as a caller writes a call to
/// it.
fn signature(d: &Definition) -> String {
    let mut s = String::from("(");
    for (i, p) in d.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('$');
        s.push_str(&p.name);
        s.push(' ');
        s.push_str(&p.ty.to_string());
        if p.default.is_some() {
            s.push_str(" = ...");
        }
    }
    s.push(')');
    if !d.claims.is_empty() {
        s.push_str(" claims (");
        for (i, c) in d.claims.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&c.name);
            s.push(' ');
            s.push_str(&c.ty.to_string());
        }
        s.push(')');
    }
    s
}

/// A pick's order, and whether it has a `limit`.
type Picked<'a> = Option<(&'a [Sort], bool)>;

/// The collection a body statement reads rows of, its filter and its pick
/// (`order` and `limit`); `None` for one that reads none: an `insert`.
fn shape(st: &Statement) -> Option<(&str, Option<&Expr>, Picked<'_>)> {
    match st {
        Statement::Update {
            collection,
            filter,
            pick,
            ..
        }
        | Statement::Delete {
            collection,
            filter,
            pick,
            ..
        } => Some((
            collection,
            filter.as_ref(),
            pick.as_deref().map(|p| (&p.order[..], p.limit.is_some())),
        )),
        Statement::Select(sel) => Some((
            &sel.collection,
            sel.filter.as_ref(),
            Some((&sel.order[..], sel.limit.is_some())),
        )),
        _ => None,
    }
}

/// Whether `schema` holds `name`: `id`, a field, a path, or a path's
/// field.
fn holds(schema: &Schema, name: &str) -> bool {
    name == "id"
        || schema.field(name).is_some()
        || schema.path(name).is_some()
        || crate::schema::split_path(name).is_some_and(|(root, _)| schema.field(root).is_some())
}

/// The first name `st` writes or filters on that its collection does not
/// hold. A `get`'s select list, order and the levels of a join or a
/// `lookup` may name an alias, which only running it tells from a field:
/// those are the statement's own checks', as anywhere.
fn unheld(schema: &Schema, st: &Statement) -> Option<String> {
    let mut names = Vec::new();
    match st {
        Statement::Update { set, filter, .. } => {
            for (k, e) in set {
                names.push(k.clone());
                e.referenced_fields(&mut names);
            }
            if let Some(f) = filter {
                f.referenced_fields(&mut names);
            }
        }
        Statement::Delete {
            filter: Some(f), ..
        } => f.referenced_fields(&mut names),
        Statement::Select(sel) if plain(sel) => {
            if let Some(f) = &sel.filter {
                f.referenced_fields(&mut names);
            }
        }
        _ => {}
    }
    names.into_iter().find(|n| !holds(schema, n))
}

/// A `get` over one collection's own fields: no join, no `lookup`.
fn plain(sel: &Select) -> bool {
    sel.flat.is_none() && sel.lookup.is_none()
}

/// Whether an index or an ordered walk narrows the rows `st` reads, as the
/// plan would read them with any values: an equality, an `in` or a range
/// over `id` or a field an index orders or hashes, an index over several
/// fields led by one, `match` or `near` (their own indexes, capped), a
/// pick an ordered index walks, or a `get` reading the first rows alone.
fn narrowed(
    schema: &Schema,
    st: &Statement,
    filter: Option<&Expr>,
    pick: Picked<'_>,
    holes: &[Value],
) -> bool {
    if let Statement::Select(sel) = st {
        if sel.near.is_some() || sel.matcher.is_some() || sel.fuse.is_some() {
            return true;
        }
        if filter.is_none() && sel.order.is_empty() && sel.limit.is_some() && plain(sel) {
            return true;
        }
    }
    let answers = |field: &str, range: bool| {
        if field == "id" {
            return true;
        }
        let indexed = schema
            .field(field)
            .or_else(|| schema.path(field))
            .is_some_and(|f| match f.index {
                IndexKind::Sorted { .. } => true,
                IndexKind::Hash { .. } => !range,
                _ => false,
            });
        indexed
            || schema
                .composites
                .iter()
                .any(|c| c.keys.first().is_some_and(|(k, _)| k == field))
    };
    if let Some(f) = filter {
        let mut eqs = Vec::new();
        f.conjunct_equalities(holes, &mut eqs);
        if eqs.iter().any(|(field, _)| answers(field, false)) {
            return true;
        }
        let mut ins = Vec::new();
        f.conjunct_in_sets(holes, &mut ins);
        if ins.iter().any(|(field, _)| answers(field, false)) {
            return true;
        }
        let mut ranges = Vec::new();
        f.conjunct_ranges(holes, &mut ranges);
        if ranges
            .iter()
            .any(|(field, op, _)| *op != CmpOp::Ne && answers(field, true))
        {
            return true;
        }
    }
    match pick {
        Some((order, true)) => order
            .first()
            .is_some_and(|s| s.expr.is_none() && answers(&s.field, true)),
        _ => false,
    }
}
