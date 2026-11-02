//! Named statements as their database keeps them: record kind 13, the
//! table of the live ones in a cell of its own, and what a schema change
//! does to one (`docs/design/named-statements.md`, *Definitions*).
//!
//! The record is read in every build, the browser module's among them: a
//! file a server wrote opens in a page and in an app, and a build keeps
//! what its file holds through its compacts. So every build reads the
//! record, writes it again in its images and applies it from a replica,
//! keeping the text as it came. The rest is the `statements` feature's:
//! reading a text into its tree and checking its digest, defining one,
//! its dependencies, the refusal at a schema change's commit and the
//! broken mark -- which no build without it lists or calls by, and which
//! is never stored: the server that opens the file again finds it.

use std::sync::Arc;

use crate::codec::{decode_str, encode_str, get_uvarint, put_uvarint};
use crate::error::{Error, Result};
#[cfg(feature = "statements")]
use crate::schema::{IndexKind, Schema};

/// A named statement defined or dropped: `[13][0][length][op][history]
/// [change][epoch][digest][name][depends][text]`, and a bound one's
/// definition `[3][history][change][epoch][digest][name][depends]
/// [collection][events][text]`. A write, which moves the change counter by
/// one (`writes_in`) whatever its op, rides in a block, and is applied by a
/// replica as a schema change is; an image writes each live definition
/// again after the collections. A version before it refuses it ("unknown
/// record kind 13") and a block holding one, never misreads it.
pub(super) const REC_STATEMENT: u8 = 13;
const OP_DEFINE: u8 = 1;
const OP_DROP: u8 = 2;
/// A statement bound to a collection's writes (F2): op 1's fields, then the
/// collection and its events outside the text, so a build that cannot read
/// the text -- the browser module, a process with no reader -- still knows
/// which writes it binds. A build before it refuses the op as a record it
/// does not read, never taking it for a callable statement whose writes
/// land without their rows (`docs/design/named-statements.md`, *A
/// binding's record*).
const OP_BOUND: u8 = 3;

/// The events of a bound statement, a bit each, as its record writes them
/// and `named::Binding` names them: a row made, written over, taken out.
/// Here, beside the record, because every build reads them into a
/// collection's [`super::Collection::bound`], the module's among them.
pub(super) const INSERT: u8 = 1;
pub(super) const SET: u8 = 2;
pub(super) const DEL: u8 = 4;
#[cfg(feature = "statements")]
const _: () = assert!(
    INSERT == crate::named::Binding::INSERT
        && SET == crate::named::Binding::SET
        && DEL == crate::named::Binding::DEL
);

/// One named statement, as its database keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub name: String,
    /// `(history, change)`: the history the database was on and the change
    /// it was defined at -- after a promotion forks the history a change
    /// number alone names two definitions. Carried in the record through
    /// every compaction.
    pub version: (u64, u64),
    /// The grammar's epoch its text was parsed under
    /// (`fennec_core::named::GRAMMAR_EPOCH`).
    pub epoch: u32,
    /// The digest of the canonical tree its text parsed to
    /// (`Definition::digest`): an open parses the text again, and a tree
    /// with another digest is not run.
    pub digest: u64,
    /// What it depends on, by name, so a build that cannot parse it still
    /// knows what it touches: `c` a collection, `c.f` a field or a path,
    /// `c.f@hash` an index a `lookup` probes, `c.f@unique` a unique one an
    /// `insert` relies on, `c(a,b)@` an index over several fields a
    /// `lookup` reads and `c(a,b)@unique` a unique one ([`holds`]).
    pub depends: Vec<String>,
    /// The definition's own bytes, from `create` to its end.
    pub text: String,
    /// What a bound statement is bound to, from its record (op 3), never
    /// from its text: the collection and the events ([`INSERT`], [`SET`],
    /// [`DEL`]). `None` for a callable statement (op 1).
    pub after: Option<(String, u8)>,
    /// Why it cannot be called, where it cannot: never stored -- an open
    /// finds it again, from the digest and the schema.
    pub broken: Option<String>,
    /// The tree its text reads to, where this build read it: what a call
    /// runs, and what a replacement is held to.
    #[cfg(feature = "statements")]
    pub tree: Option<Arc<crate::named::Definition>>,
    /// The tree its text reads to in this build where that is not the tree
    /// it was defined as (`its text reads as another statement`): what a
    /// call is judged by -- the claims, the arguments, `check` -- before
    /// the 409 tells it broken, as a call's refusals are ordered, so a
    /// caller lacking its claims is told nothing of it. Never run, nor held
    /// as the contract a replacement keeps.
    #[cfg(feature = "statements")]
    pub misread: Option<Arc<crate::named::Definition>>,
}

impl Stored {
    /// The record's body for a definition.
    pub(super) fn encode(&self) -> Vec<u8> {
        let op = match self.after {
            Some(_) => OP_BOUND,
            None => OP_DEFINE,
        };
        let mut out = encode(
            op,
            self.version,
            self.epoch,
            self.digest,
            &self.name,
            &self.depends,
        );
        if let Some((collection, events)) = &self.after {
            encode_str(&mut out, collection);
            out.push(*events);
        }
        encode_str(&mut out, &self.text);
        out
    }
}

/// The record's body for a drop of `name`, at `version`.
#[cfg(feature = "statements")]
pub(super) fn dropped(name: &str, version: (u64, u64)) -> Vec<u8> {
    let mut out = encode(OP_DROP, version, 0, 0, name, &[]);
    encode_str(&mut out, "");
    out
}

/// A record's body up to its dependencies, which every op writes.
fn encode(
    op: u8,
    (history, change): (u64, u64),
    epoch: u32,
    digest: u64,
    name: &str,
    depends: &[String],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + name.len());
    out.push(op);
    put_uvarint(&mut out, history);
    put_uvarint(&mut out, change);
    put_uvarint(&mut out, epoch as u64);
    out.extend_from_slice(&digest.to_le_bytes());
    encode_str(&mut out, name);
    put_uvarint(&mut out, depends.len() as u64);
    for d in depends {
        encode_str(&mut out, d);
    }
    out
}

/// A kind-13 record's body, read: a definition, or `Err` naming the
/// statement a drop takes away. An operation, an event or a byte past the
/// text this version does not know is refused, never read as something
/// else.
pub(super) fn decode(body: &[u8]) -> Result<std::result::Result<Stored, String>> {
    let cut = || Error::Corrupt("a record cut short".into());
    let op = *body.first().ok_or_else(cut)?;
    let unread = || Error::Corrupt("a statement record this version does not read".into());
    if !matches!(op, OP_DEFINE | OP_DROP | OP_BOUND) {
        return Err(unread());
    }
    let mut p = 1;
    let version = (get_uvarint(body, &mut p)?, get_uvarint(body, &mut p)?);
    let epoch = get_uvarint(body, &mut p)? as u32;
    let mut d = [0u8; 8];
    d.copy_from_slice(body.get(p..p.saturating_add(8)).ok_or_else(cut)?);
    p += 8;
    let name = decode_str(body, &mut p)?;
    let n = get_uvarint(body, &mut p)? as usize;
    let mut depends = Vec::with_capacity(n.min(body.len()));
    for _ in 0..n {
        depends.push(decode_str(body, &mut p)?);
    }
    let after = match op {
        OP_BOUND => {
            let collection = decode_str(body, &mut p)?;
            let events = *body.get(p).ok_or_else(cut)?;
            p += 1;
            // No event, or one a later version names -- a bit past these
            // three: a binding this version cannot keep.
            if events == 0 || events & !(INSERT | SET | DEL) != 0 {
                return Err(unread());
            }
            Some((collection, events))
        }
        _ => None,
    };
    let text = decode_str(body, &mut p)?;
    if p != body.len() {
        return Err(unread());
    }
    if op == OP_DROP {
        return Ok(Err(name));
    }
    Ok(Ok(Stored {
        name,
        version,
        epoch,
        digest: u64::from_le_bytes(d),
        depends,
        text,
        after,
        broken: None,
        #[cfg(feature = "statements")]
        tree: None,
        #[cfg(feature = "statements")]
        misread: None,
    }))
}

/// The live named statements of a database, by name: in a cell of its own
/// (`Database::statements`), as a collection's schema is, so a reader holds
/// the table it read under a short lock and a definition changes it by a
/// new `Arc`, never in place. The bound ones are indexed by their
/// collection besides.
#[derive(Debug, Clone, Default)]
pub struct Statements {
    all: Vec<Stored>,
    /// Where each bound statement stands in `all`, by its collection and,
    /// within one, by name: a collection's bindings are a run, in the
    /// order a row runs them in. Places, not names: a copy of the table
    /// copies numbers, and the run is the statements themselves -- the
    /// names kept twice, sorted, retained and cloned with each table were
    /// 0.7 KB of the browser module. Made again whole wherever a bound
    /// statement is defined or dropped, or one is beside one, since each
    /// definition and drop moves the places after it. Not the browser
    /// module's, which asks only each collection's events
    /// ([`Self::events_of`]) at a schema change: the index, its walk and
    /// its upkeep were bytes of a module that reads no binding's tree.
    #[cfg(feature = "statements")]
    bound: Vec<u32>,
}

/// The collection a statement is bound to; `""`, which names none, for a
/// callable one.
#[cfg(feature = "statements")]
fn bound_of(s: &Stored) -> &str {
    s.after.as_ref().map_or("", |a| a.0.as_str())
}

impl Statements {
    pub fn get(&self, name: &str) -> Option<&Stored> {
        self.at(name).ok().map(|i| &self.all[i])
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Stored> {
        self.all.iter()
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// The statements bound to `collection`'s writes, in name order,
    /// broken or not.
    #[cfg(feature = "statements")]
    pub fn bound_to<'s>(&'s self, collection: &str) -> impl Iterator<Item = &'s Stored> + 's {
        let run = self.run(collection, false)..self.run(collection, true);
        self.bound[run].iter().map(|&i| &self.all[i as usize])
    }

    /// The events the statements bound to `collection` wake on, together:
    /// what its [`super::Collection::bound`] holds.
    #[cfg(feature = "statements")]
    pub fn events_of(&self, collection: &str) -> u8 {
        self.bound_to(collection)
            .fold(0, |e, s| e | s.after.as_ref().map_or(0, |a| a.1))
    }

    /// The same where no index is kept: the table walked, as the module
    /// asks it, at a schema change alone.
    #[cfg(not(feature = "statements"))]
    pub fn events_of(&self, collection: &str) -> u8 {
        self.all
            .iter()
            .filter_map(|s| s.after.as_ref())
            .filter(|a| a.0 == collection)
            .fold(0, |e, a| e | a.1)
    }

    /// Where `collection`'s run in [`Self::bound`] begins, or -- `past` --
    /// ends. Walked rather than halved: a database binds a handful, and
    /// `partition_point`'s two copies were 0.3 KB of the browser module.
    #[cfg(feature = "statements")]
    fn run(&self, collection: &str, past: bool) -> usize {
        use std::cmp::Ordering::{Equal, Less};
        let mut at = 0;
        for &i in &self.bound {
            match bound_of(&self.all[i as usize]).cmp(collection) {
                Less => at += 1,
                Equal if past => at += 1,
                _ => break,
            }
        }
        at
    }

    fn at(&self, name: &str) -> std::result::Result<usize, usize> {
        self.all.binary_search_by(|s| s.name.as_str().cmp(name))
    }

    /// Defines `s`, or replaces the statement of its name.
    pub(super) fn put(&mut self, s: Stored) {
        #[cfg(feature = "statements")]
        let bound = s.after.is_some();
        match self.at(&s.name) {
            Ok(i) => self.all[i] = s,
            Err(i) => self.all.insert(i, s),
        }
        #[cfg(feature = "statements")]
        self.reindex(bound);
    }

    pub(super) fn remove(&mut self, name: &str) -> Option<Stored> {
        let s = self.at(name).ok().map(|i| self.all.remove(i))?;
        #[cfg(feature = "statements")]
        self.reindex(s.after.is_some());
        Some(s)
    }

    /// [`Self::bound`] made again from `all`, walked in name order, each
    /// bound statement put at the end of its collection's run -- where a
    /// definition or a drop of a bound statement may change it, or one
    /// beside a bound one moves its place. A table binding nothing, as F1's
    /// are, walks nothing.
    #[cfg(feature = "statements")]
    fn reindex(&mut self, bound: bool) {
        if !bound && self.bound.is_empty() {
            return;
        }
        self.bound.clear();
        for i in 0..self.all.len() {
            if self.all[i].after.is_some() {
                let at = self.run(bound_of(&self.all[i]), true);
                self.bound.insert(at, i as u32);
            }
        }
    }
}

/// Where `s` is bound after `del` on a collection whose rows expire, what
/// it is in words. A row `@ttl` takes out leaves with no write -- every
/// read left it out at its time, and the sweep wakes nothing -- so a
/// binding promising to see every row its collection loses would miss
/// each, and a count kept by `after insert` and `after del` would count
/// them for ever: the two refuse each other, whichever comes second.
#[cfg(feature = "statements")]
pub(super) fn expiring<'a>(
    s: &Stored,
    schema_of: &dyn Fn(&str) -> Option<&'a Schema>,
) -> Option<String> {
    let (c, events) = s.after.as_ref()?;
    let expires = schema_of(c)?.fields.iter().any(|f| f.index.ttl().is_some());
    (events & DEL != 0 && expires).then(|| expiring_words(c))
}

#[cfg(feature = "statements")]
pub(super) fn expiring_words(c: &str) -> String {
    format!("is bound after `del` on `{c}`, whose rows `@ttl` takes out with no write to wake it")
}

/// The first of `s`'s dependencies `schema_of` no longer holds, and what
/// the statement does with it in words -- "reads `accounts.balance`".
#[cfg(feature = "statements")]
pub(super) fn unmet<'a>(
    s: &Stored,
    schema_of: &dyn Fn(&str) -> Option<&'a Schema>,
) -> Option<String> {
    s.depends
        .iter()
        .find(|d| !holds(d, schema_of))
        .map(|d| words(d))
}

/// A dependency's parts: its collection, what of it -- `.f` a field or a
/// path, `(a,b)` an index's keys, nothing the collection -- and the index
/// named after `@`. Cut with `find` and `get`, never an index into the
/// text: a slice that could panic brought a `char`'s formatting into the
/// browser module.
#[cfg(feature = "statements")]
fn parts(dep: &str) -> (&str, &str, Option<&str>) {
    let (body, index) = match dep.split_once('@') {
        Some((b, i)) => (b, Some(i)),
        None => (dep, None),
    };
    let end = body.find(['.', '(']).unwrap_or(body.len());
    (
        body.get(..end).unwrap_or(body),
        body.get(end..).unwrap_or_default(),
        index,
    )
}

/// Whether the schemas hold `dep`, one of a statement's [`Stored::depends`].
/// A form this version does not read holds: a later one wrote it, whose
/// checks are its own.
#[cfg(feature = "statements")]
pub(super) fn holds<'a>(dep: &str, schema_of: &dyn Fn(&str) -> Option<&'a Schema>) -> bool {
    let (c, what, index) = parts(dep);
    let Some(s) = schema_of(c) else {
        return false;
    };
    if let Some(keys) = what.strip_prefix('(').and_then(|k| k.strip_suffix(')')) {
        let unique = index == Some("unique");
        return s.composites.iter().any(|c| {
            let mut have = c.keys.iter();
            let leads = keys
                .split(',')
                .all(|k| have.next().is_some_and(|h| h.0 == k));
            leads && (!unique || (c.unique && have.next().is_none()))
        });
    }
    let Some(field) = what.strip_prefix('.') else {
        return what.is_empty();
    };
    let Some(f) = s.field(field).or_else(|| s.path(field)) else {
        return false;
    };
    match index {
        Some("unique") => f.index.is_unique(),
        Some("hash") => matches!(f.index, IndexKind::Hash { .. }),
        _ => true,
    }
}

/// What a statement does with `dep`, in words.
#[cfg(feature = "statements")]
fn words(dep: &str) -> String {
    let (c, what, index) = parts(dep);
    let (lead, tail) = match index {
        None => ("reads `", "`"),
        Some("unique") => ("relies on `", "` being unique"),
        Some(_) => ("looks up by the index on `", "`"),
    };
    let mut out = String::with_capacity(lead.len() + dep.len() + tail.len() + 8);
    out.push_str(lead);
    out.push_str(c);
    match what.strip_prefix('(') {
        Some(keys) => {
            out.push_str(" (");
            for (i, k) in keys.trim_end_matches(')').split(',').enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(k);
            }
            out.push(')');
        }
        None => out.push_str(what),
    }
    out.push_str(tail);
    out
}

/// `Arc::make_mut` by another name: a change to the table is a new table
/// wherever a reader holds the old one.
pub(super) fn changed(t: &mut Arc<Statements>) -> &mut Statements {
    Arc::make_mut(t)
}

/// Why a statement is broken where its text no longer reads as the tree
/// it was defined as: the parse's refusal, or another digest. Kept until
/// it is defined again, where a dependency's break lasts while the schema
/// lacks what it names ([`Database::read_statements`]).
#[cfg(feature = "statements")]
const MISREAD: &str = "its text ";

impl super::Database {
    /// The named statements this database holds: the table as it stands,
    /// for a reader to hold past its lock and ask again by `Arc::ptr_eq`.
    pub fn statements(&self) -> &Arc<Statements> {
        &self.statements
    }

    /// Runs `f` as a writer that cannot refuse a schema change: a schema
    /// change of its that breaks a named statement marks it broken rather
    /// than being refused -- a follower copying a collection again from
    /// its source, the sync's remake of a collection. Every other writer's
    /// is refused at its commit, naming the statement. A build without
    /// named statements holds none to refuse for.
    ///
    /// Such a writer copies, or remakes, what another made: it runs
    /// [`Self::unbound`] too.
    /// Given back on a panic too, as [`Self::unbound`] gives its flag
    /// back: given back only on a return, a panic in a follower's apply
    /// left every later schema change marking what it broke rather than
    /// refused.
    pub fn internally<R>(&mut self, f: impl FnOnce(&mut super::Database) -> R) -> R {
        let was = std::mem::replace(&mut self.internal, true);
        #[cfg(target_arch = "wasm32")]
        let r = self.unbound(f);
        #[cfg(not(target_arch = "wasm32"))]
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.unbound(f)));
        self.internal = was;
        #[cfg(not(target_arch = "wasm32"))]
        let r = r.unwrap_or_else(|p| std::panic::resume_unwind(p));
        r
    }

    /// Runs `f` as a writer whose writes wake no statement bound to them,
    /// and are refused by none -- a broken one's or an unread one's
    /// either: derived rows made elsewhere, or not at all. `--follow`'s
    /// mirror, whose collections copy another database's rows, derived
    /// ones among them; `fennec import`, a load, whose derived rows are a
    /// backfill its operator runs as a statement; a declared schema's
    /// migrations, which write what their text says; the sync's apply of
    /// a stream's rows and its remake; and [`Self::internally`]'s writers.
    /// A binding's own runs write so too (depth one). Every other writer
    /// wakes them (`docs/design/named-statements.md`, *When a binding
    /// runs*). Given back as `f` ends, on a panic too: one left set would
    /// let every later write land without its rows.
    pub fn unbound<R>(&mut self, f: impl FnOnce(&mut super::Database) -> R) -> R {
        let was = std::mem::replace(&mut self.unbound, true);
        // The browser module aborts on a panic, with nothing left to give
        // back to: its catch and its resume were 0.4 KB of it.
        #[cfg(target_arch = "wasm32")]
        let r = f(self);
        #[cfg(not(target_arch = "wasm32"))]
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.unbound = was;
        #[cfg(not(target_arch = "wasm32"))]
        let r = r.unwrap_or_else(|p| std::panic::resume_unwind(p));
        r
    }

    /// A kind-13 record into the table, as a load or a replica's apply
    /// finds it: read into its tree, and checked, by
    /// [`Self::read_statements`].
    pub(super) fn take_statement(&mut self, body: &[u8]) -> Result<()> {
        match decode(body)? {
            Ok(s) => changed(&mut self.statements).put(s),
            Err(name) => drop(changed(&mut self.statements).remove(&name)),
        }
        self.rebind();
        Ok(())
    }

    /// Reads each statement this build has not read into its tree yet --
    /// marked broken where its text no longer reads, or reads as a tree of
    /// another digest -- and marks broken each whose dependencies the
    /// schema no longer holds, or clears the mark where it holds them
    /// again. After an open, an image adopted, a replica's schema change
    /// and a block's that may not refuse; never failing any of them: an
    /// API's definition must not hold the data hostage.
    #[cfg(feature = "statements")]
    pub(super) fn read_statements(&mut self) {
        if self.statements.is_empty() {
            return;
        }
        let collections = &self.collections;
        let schema_of = |c: &str| collections.get(c).map(|c| &*c.schema);
        let mut marks: Vec<(usize, Option<String>)> = Vec::new();
        let mut trees: Vec<(usize, Arc<crate::named::Definition>)> = Vec::new();
        let mut misreads: Vec<(usize, Arc<crate::named::Definition>)> = Vec::new();
        for (i, s) in self.statements.iter().enumerate() {
            if s.broken.as_deref().is_some_and(|b| b.starts_with(MISREAD)) {
                continue;
            }
            if let (None, Some(read)) = (&s.tree, crate::named::reader()) {
                match read(&s.text).and_then(|d| Ok((d.digest()?, d))) {
                    // No record this build writes holds a schedule (op 4
                    // is F3's slice 2): a text reading as one, digest and
                    // all, is never loaded as callable or bound.
                    Ok((_, d)) if d.schedule.is_some() => {
                        marks.push((i, Some(misread(s, "reads as a scheduled statement"))));
                        continue;
                    }
                    // A definition's record (op 1) holds a callable
                    // statement, and a bound one's (op 3) the collection
                    // and events its text binds: a text reading as the
                    // other kind, or bound to other writes, digest and
                    // all, is none this build wrote -- never loaded as
                    // callable, and never bound to writes its record does
                    // not name, which the collection's mask is read from.
                    Ok((_, d)) if d.bound_as() != s.after.as_ref().map(|(c, e)| (&**c, *e)) => {
                        let how = match (&d.after, &s.after) {
                            (Some(_), None) => "reads as a bound statement",
                            (None, _) => "reads as a callable statement",
                            (Some(_), Some(_)) => "binds other writes than its record",
                        };
                        marks.push((i, Some(misread(s, how))));
                        continue;
                    }
                    Ok((digest, d)) if digest == s.digest => trees.push((i, Arc::new(d))),
                    Ok((_, d)) => {
                        marks.push((i, Some(misread(s, "reads as another statement"))));
                        misreads.push((i, Arc::new(d)));
                        continue;
                    }
                    Err(e) => {
                        marks.push((i, Some(misread(s, &format!("no longer reads: {e}")))));
                        continue;
                    }
                }
            }
            let why = match unmet(s, &schema_of) {
                Some(what) => Some(format!("it {what}, which the schema no longer holds")),
                // An internal writer made the collection `@ttl` under an
                // `after del` binding, which refuses its writes since.
                None => expiring(s, &schema_of).map(|what| format!("it {what}")),
            };
            if why != s.broken {
                marks.push((i, why));
            }
        }
        if marks.is_empty() && trees.is_empty() {
            return;
        }
        let t = changed(&mut self.statements);
        for (i, why) in marks {
            t.all[i].broken = why;
        }
        for (i, tree) in trees {
            t.all[i].tree = Some(tree);
        }
        for (i, tree) in misreads {
            t.all[i].misread = Some(tree);
        }
    }

    /// A block's schema changes held to the named statements, against the
    /// schema the block leaves, as it lands: one breaking a statement is
    /// refused naming it -- in a build that defines statements, from a
    /// writer that may refuse -- and otherwise marks it broken, the mark
    /// put back with the block where it does not land.
    #[cfg(feature = "statements")]
    pub(super) fn statements_hold(&mut self, b: &mut super::Block) -> Result<()> {
        if b.heads.iter().all(|h| h.0 == super::REC_DATA) {
            return Ok(());
        }
        if !self.internal {
            let collections = &self.collections;
            let schema_of = |c: &str| collections.get(c).map(|c| &*c.schema);
            for s in self.statements.iter().filter(|s| s.broken.is_none()) {
                if let Some(what) = unmet(s, &schema_of).or_else(|| expiring(s, &schema_of)) {
                    return Err(Error::Query(format!(
                        "statement `{}` {what}: replace or drop it first",
                        s.name
                    )));
                }
            }
        }
        let before = self.statements.clone();
        self.read_statements();
        if !Arc::ptr_eq(&before, &self.statements) {
            b.was.push(super::Undo::Statements(before));
        }
        Ok(())
    }

    /// `create [or replace] statement`: its record written, the table
    /// changed by a new `Arc`. Checked against the schema as it stands --
    /// each collection it names held, each `lookup` answered by an index --
    /// and against the schema the block leaves as it lands
    /// ([`Self::statements_hold`]).
    #[cfg(feature = "statements")]
    pub(super) fn define_statement(
        &mut self,
        def: &crate::named::Definition,
    ) -> Result<crate::query::Response> {
        use crate::named::{GRAMMAR_EPOCH, MAX_STATEMENTS, MAX_TEXT, SCHEDULED_NOT_BUILT};
        // First, before a name held or the schema is asked: nothing runs a
        // schedule yet, and one stored would be a slot no one runs without
        // a word -- here every way in (`/query`, `/batch`, the shell, the
        // native library, a declared schema's apply) meets it.
        if def.schedule.is_some() {
            return Err(Error::Query(SCHEDULED_NOT_BUILT.into()));
        }
        let name = &def.name;
        let held = self.statements.get(name);
        if let Some(old) = held {
            if !def.replace {
                return Err(Error::Exists(format!(
                    "statement `{name}`: `create or replace statement` replaces it"
                )));
            }
            // The kind, and a binding's collection and events, from the
            // record: known where the old text reads no tree here, and
            // what a `call` or `wake` line names it for.
            let was = old.after.as_ref().map(|(c, e)| (c.as_str(), *e));
            if let Some(why) = crate::named::kind_kept(name, def.bound_as(), was) {
                return Err(Error::Query(why));
            }
            // Held to the contract wherever the old tree is, broken or
            // not, as `declared::plan_statements` holds it: a mark is the
            // schema's lack, and a grant names the statement whatever
            // the schema holds -- a replacement that writes where the old
            // one read would start meaning more under the same line. A
            // text that misreads keeps no tree, and is defined again as
            // anything of its kind.
            if let Some(tree) = &old.tree {
                if let Some(why) = def.breaks_contract(tree) {
                    return Err(Error::Query(why));
                }
            }
        } else if self.statements.len() >= MAX_STATEMENTS {
            return Err(Error::Query(format!(
                "a database holds at most {MAX_STATEMENTS} named statements"
            )));
        }
        if def.text.is_empty() || def.text.len() > MAX_TEXT {
            return Err(Error::Query(format!(
                "a definition is its text, of 1 to {MAX_TEXT} bytes, as the parser read it"
            )));
        }
        if let Some(b) = &def.after {
            self.binding_checked(def, b)?;
        }
        let collections = &self.collections;
        let depends = def.depends(&|c| collections.get(c).map(|c| &*c.schema))?;
        self.body_checked(def)?;
        let replaced = held.is_some();
        let digest = def.digest()?;
        // The tree its record will be read back into, read from its text
        // as an open reads it: a `Definition` cloned was 5.7 KB of the
        // server for a statement read once.
        let tree = crate::named::reader()
            .and_then(|read| read(&def.text).ok())
            .filter(|t| t.digest().ok() == Some(digest))
            .map(Arc::new);
        let stored = Stored {
            name: name.clone(),
            version: (self.history.current(), self.next_change()),
            epoch: GRAMMAR_EPOCH,
            digest,
            depends,
            text: def.text.clone(),
            after: def.after.as_ref().map(|b| (b.collection.clone(), b.events)),
            broken: None,
            tree,
            misread: None,
        };
        let after = stored.after.clone();
        self.statement_record(&stored.encode(), |t| t.put(stored))?;
        if let Some((c, events)) = &after {
            super::bound::note_changed(name, (c, *events));
        }
        Ok(crate::query::Response::Ok(match replaced {
            true => format!("statement `{name}` replaced"),
            false => format!("statement `{name}` defined"),
        }))
    }

    /// `drop statement [if exists]`.
    #[cfg(feature = "statements")]
    pub(super) fn drop_statement(
        &mut self,
        name: &str,
        if_exists: bool,
    ) -> Result<crate::query::Response> {
        if self.statements.get(name).is_none() {
            return match if_exists {
                true => Ok(crate::query::Response::Ok(format!("no statement `{name}`"))),
                false => Err(Error::NotFound(format!("statement `{name}`"))),
            };
        }
        let after = self.statements.get(name).and_then(|s| s.after.clone());
        let body = dropped(name, (self.history.current(), self.next_change()));
        self.statement_record(&body, |t| drop(t.remove(name)))?;
        if let Some((c, events)) = &after {
            super::bound::note_changed(name, (c, *events));
        }
        Ok(crate::query::Response::Ok(format!(
            "statement `{name}` dropped"
        )))
    }

    /// The number the next write takes: the counter's, past the writes the
    /// open block holds -- a definition's version.
    #[cfg(feature = "statements")]
    fn next_change(&self) -> u64 {
        self.changes.seq() + self.block.as_ref().map_or(0, |b| b.heads.len() as u64) + 1
    }

    /// A kind-13 record written, the table changed by `change`, and the
    /// table before kept for the block to put back.
    #[cfg(feature = "statements")]
    fn statement_record(
        &mut self,
        body: &[u8],
        change: impl FnOnce(&mut Statements),
    ) -> Result<()> {
        self.wal(REC_STATEMENT, 0, body)?;
        self.note(0, crate::changes::SCHEMA_MARK);
        let before = self.statements.clone();
        change(changed(&mut self.statements));
        self.rebind();
        if let Some(b) = &mut self.block {
            b.was.push(super::Undo::Statements(before));
        }
        Ok(())
    }

    /// What a binding's definition is held to beside a callable one's: its
    /// collection held, bound by at most `named::MAX_BOUND` statements,
    /// `after del` refused where its rows expire, each field `new.` and
    /// `old.` read held by it, and depth one kept whole: no collection it
    /// is bound to written by it, none another statement is bound to, and
    /// none another binding writes bound by it ([`chained`]).
    #[cfg(feature = "statements")]
    fn binding_checked(
        &self,
        def: &crate::named::Definition,
        b: &crate::named::Binding,
    ) -> Result<()> {
        use crate::named::MAX_BOUND;
        let name = &def.name;
        let c = &b.collection;
        let schema = &self.collection(c)?.schema;
        if self
            .statements
            .bound_to(c)
            .filter(|s| s.name != *name)
            .count()
            >= MAX_BOUND
        {
            return Err(Error::Query(format!(
                "a collection is bound by at most {MAX_BOUND} statements, and `{c}` is"
            )));
        }
        if b.events & DEL != 0 && schema.fields.iter().any(|f| f.index.ttl().is_some()) {
            return Err(Error::Query(format!(
                "statement `{name}` {}: bind it after `insert` or `set` alone, or take `@ttl` \
                 off `{c}` first",
                expiring_words(c)
            )));
        }
        if let Some(f) = b.unheld(schema) {
            return Err(Error::Query(format!(
                "statement `{name}`: `{c}` holds no field `{f}`"
            )));
        }
        match chained(&self.statements, def, c) {
            Some(why) => Err(Error::Query(why)),
            None => Ok(()),
        }
    }

    /// A `@ttl` about to be given `collection`, held to the statements
    /// bound after `del` to it: refused naming the first, from a writer
    /// that may refuse -- what a block's commit refuses
    /// ([`Self::statements_hold`]), asked here too by a `create index`
    /// built beside the database, which lands outside it. A writer that
    /// cannot refuse marks it broken as the change lands.
    #[cfg(feature = "statements")]
    pub(super) fn expiry_held(&self, collection: &str) -> Result<()> {
        if self.internal {
            return Ok(());
        }
        let del = |s: &&Stored| s.after.as_ref().is_some_and(|a| a.1 & DEL != 0);
        match self
            .statements
            .bound_to(collection)
            .find(|s| s.broken.is_none() && del(s))
        {
            Some(s) => Err(Error::Query(format!(
                "statement `{}` {}: replace or drop it first",
                s.name,
                expiring_words(collection)
            ))),
            None => Ok(()),
        }
    }

    /// Each collection's [`super::Collection::bound`] set from the table,
    /// wherever the table or the collections change: a definition or a
    /// drop, a block put back, an open, a replica's apply, an image
    /// adopted, a collection made. A look at each collection, at a schema
    /// change alone, and out of line: each place calls it.
    #[inline(never)]
    pub(super) fn rebind(&mut self) {
        let t = &self.statements;
        for (name, c) in self.collections.iter_mut() {
            c.bound = t.events_of(name);
        }
    }

    /// A writing statement over `collection` that may wake the statements
    /// bound to it on `events` -- its [`super::Collection::bound`] and what
    /// the statement may do to a row -- refused before it changes
    /// anything, the condition unread: where one of them is broken, or
    /// held unread by a process with no FennecQL reader (409), and in a
    /// build that runs none -- the browser module -- in words of its own.
    /// A write landing without the rows its binding would write is the
    /// one thing a binding must never let happen: the writes stop, loudly,
    /// until it is defined again or dropped. Where several would refuse,
    /// the first by name answers.
    #[cold]
    #[inline(never)]
    pub(super) fn bound_refused(&self, collection: &str, events: u8) -> Error {
        #[cfg(feature = "statements")]
        if let Some(s) = self.statements.bound_to(collection).find(|s| {
            s.after.as_ref().is_some_and(|a| a.1 & events != 0)
                && (s.broken.is_some() || s.tree.is_none())
        }) {
            let name = &s.name;
            return Error::Exists(match &s.broken {
                Some(why) => format!(
                    "the write would wake `{name}`, which is broken: {why}: define it again or \
                     drop it"
                ),
                None => format!(
                    "the write would wake `{name}`, whose text this process holds unread (no \
                     FennecQL reader installed): define it again or drop it"
                ),
            });
        }
        let _ = events;
        Error::Query(format!(
            "`{collection}` has statements bound to its writes, which this module does not \
             run: write it through a server"
        ))
    }
}

/// Where `def`, a binding of `collection`, would make a chain -- what a
/// binding's writes wake, which is nothing (depth one) -- the refusal in
/// words: a binding writing the collection it is bound to, which would
/// move the rows its writer is still walking and answering; one writing a
/// collection another statement is bound to, which would sleep through
/// every row the first wrote; and one bound to a collection another
/// binding writes, the same in the other order. Each by collection,
/// whatever the events: a refusal can be lifted later, a cascade never
/// taken back. A binding read from no tree here -- broken by a misread,
/// or unread -- writes nothing a definition can know of, and refuses its
/// own writes.
#[cfg(feature = "statements")]
fn chained(t: &Statements, def: &crate::named::Definition, collection: &str) -> Option<String> {
    let name = &def.name;
    let written = def.written();
    if written.iter().any(|(w, _)| *w == collection) {
        return Some(format!(
            "statement `{name}` is bound to `{collection}` and writes it: a binding writes no \
             collection it is bound to, whose rows its writer is still writing"
        ));
    }
    for (w, _) in &written {
        if let Some(other) = t.bound_to(w).find(|s| s.name != *name) {
            return Some(format!(
                "statement `{name}` writes `{w}`, which `{}` is bound to: a binding's writes \
                 wake nothing, so `{}` would miss every row `{name}` wrote -- drop one of them",
                other.name, other.name
            ));
        }
    }
    let writes_it = |s: &&Stored| {
        s.name != *name
            && (s.tree.as_ref()).is_some_and(|d| d.written().iter().any(|(w, _)| *w == collection))
    };
    if let Some(other) = t.iter().filter(|s| s.after.is_some()).find(writes_it) {
        return Some(format!(
            "statement `{name}` is bound to `{collection}`, which `{}` writes: a binding's \
             writes wake nothing, so `{name}` would miss every row `{}` wrote -- drop one of \
             them",
            other.name, other.name
        ));
    }
    None
}

/// A statement's text no longer reading as its tree, in words.
#[cfg(feature = "statements")]
fn misread(s: &Stored, how: &str) -> String {
    format!(
        "{MISREAD}{how} under grammar epoch {}, where it was defined under {}: define it again",
        crate::named::GRAMMAR_EPOCH,
        s.epoch
    )
}
