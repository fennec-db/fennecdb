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
/// [change][epoch][digest][name][depends][text]`. A write, which moves the
/// change counter by one (`writes_in`), rides in a block, and is applied by
/// a replica as a schema change is; an image writes each live definition
/// again after the collections. A version before it refuses it ("unknown
/// record kind 13") and a block holding one, never misreads it.
pub(super) const REC_STATEMENT: u8 = 13;
const OP_DEFINE: u8 = 1;
const OP_DROP: u8 = 2;

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
        encode(
            OP_DEFINE,
            self.version,
            self.epoch,
            self.digest,
            &self.name,
            &self.depends,
            &self.text,
        )
    }
}

/// The record's body for a drop of `name`, at `version`.
#[cfg(feature = "statements")]
pub(super) fn dropped(name: &str, version: (u64, u64)) -> Vec<u8> {
    encode(OP_DROP, version, 0, 0, name, &[], "")
}

fn encode(
    op: u8,
    (history, change): (u64, u64),
    epoch: u32,
    digest: u64,
    name: &str,
    depends: &[String],
    text: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + name.len() + text.len());
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
    encode_str(&mut out, text);
    out
}

/// A kind-13 record's body, read: a definition, or `Err` naming the
/// statement a drop takes away. An operation or a byte past the text this
/// version does not know is refused, never read as something else.
pub(super) fn decode(body: &[u8]) -> Result<std::result::Result<Stored, String>> {
    let cut = || Error::Corrupt("a record cut short".into());
    let op = *body.first().ok_or_else(cut)?;
    let unread = || Error::Corrupt("a statement record this version does not read".into());
    if op != OP_DEFINE && op != OP_DROP {
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
/// new `Arc`, never in place.
#[derive(Debug, Clone, Default)]
pub struct Statements(Vec<Stored>);

impl Statements {
    pub fn get(&self, name: &str) -> Option<&Stored> {
        self.at(name).ok().map(|i| &self.0[i])
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Stored> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn at(&self, name: &str) -> std::result::Result<usize, usize> {
        self.0.binary_search_by(|s| s.name.as_str().cmp(name))
    }

    /// Defines `s`, or replaces the statement of its name.
    pub(super) fn put(&mut self, s: Stored) {
        match self.at(&s.name) {
            Ok(i) => self.0[i] = s,
            Err(i) => self.0.insert(i, s),
        }
    }

    pub(super) fn remove(&mut self, name: &str) -> Option<Stored> {
        self.at(name).ok().map(|i| self.0.remove(i))
    }
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
    pub fn internally<R>(&mut self, f: impl FnOnce(&mut super::Database) -> R) -> R {
        let was = std::mem::replace(&mut self.internal, true);
        let r = f(self);
        self.internal = was;
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
            let why = unmet(s, &schema_of).map(|what| {
                let mut why = String::from("it ");
                why.push_str(&what);
                why.push_str(", which the schema no longer holds");
                why
            });
            if why != s.broken {
                marks.push((i, why));
            }
        }
        if marks.is_empty() && trees.is_empty() {
            return;
        }
        let t = changed(&mut self.statements);
        for (i, why) in marks {
            t.0[i].broken = why;
        }
        for (i, tree) in trees {
            t.0[i].tree = Some(tree);
        }
        for (i, tree) in misreads {
            t.0[i].misread = Some(tree);
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
                if let Some(what) = unmet(s, &schema_of) {
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
        use crate::named::{BOUND_NOT_BUILT, GRAMMAR_EPOCH, MAX_STATEMENTS, MAX_TEXT};
        // First, before a name held or the schema is asked: nothing runs a
        // binding yet, and one stored would let the writes it binds land
        // without its rows -- here every way in (`/query`, `/batch`, the
        // shell, the native library, a declared schema's apply) meets it.
        if def.after.is_some() {
            return Err(Error::Query(BOUND_NOT_BUILT.into()));
        }
        let name = &def.name;
        let held = self.statements.get(name);
        if let Some(old) = held {
            if !def.replace {
                return Err(Error::Exists(format!(
                    "statement `{name}`: `create or replace statement` replaces it"
                )));
            }
            // Held to the contract wherever the old tree is, broken or
            // not, as `declared::plan_statements` holds it: a mark is the
            // schema's lack, and a grant names the statement whatever
            // the schema holds -- a replacement that writes where the old
            // one read would start meaning more under the same line. A
            // text that misreads keeps no tree, and is defined again as
            // anything.
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
            broken: None,
            tree,
            misread: None,
        };
        self.statement_record(&stored.encode(), |t| t.put(stored))?;
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
        let body = dropped(name, (self.history.current(), self.next_change()));
        self.statement_record(&body, |t| drop(t.remove(name)))?;
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
        if let Some(b) = &mut self.block {
            b.was.push(super::Undo::Statements(before));
        }
        Ok(())
    }
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
