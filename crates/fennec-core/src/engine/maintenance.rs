//! `create index` and `compact` beside the database rather than in its way.
//!
//! Both are builds from the documents: an index from one field of each, a
//! compacted collection from the live ones and every index over them. Run
//! by [`Database::execute`], the whole build holds the write lock, and every
//! reader and writer waits it out -- ~10 s for an HNSW index over 100 000 x
//! 128. [`Database::maintain`] takes under the read lock only what the build
//! reads from -- each store as it stands, cloned as a pin clones it -- reads
//! the documents and builds with no lock held, catches up with the writes
//! made meanwhile in rounds beside the database, and takes the write lock
//! only for the last few and to put the result in place. Read under the
//! read lock, a compact's graphs' vectors held it for seconds past the page
//! cache, and every writer with it -- and, a writer waiting, every reader.
//!
//! Those writes are known exactly, not guessed from the change ring (which
//! a long build would overflow): every write passes through
//! `Database::note`, which hands the id to the [`Tail`] of each maintenance
//! running on that collection. A schema change there -- another index, a
//! drop -- leaves what was built not fitting, and the maintenance says so
//! rather than putting it in place.

use super::*;
use std::sync::RwLock;
use std::time::Duration;

/// How long a maintenance beside the database held the database's lock,
/// each kind of hold at its longest: what a writer waited out at most, and
/// -- `RwLock` making a reader wait behind a waiting writer -- every reader
/// after it. Its build is a compact's graphs or a `create index`'s index;
/// its file a compact's rewrite. One `Instant` a hold
/// ([`Database::last_holds`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Holds {
    /// The read lock the build's stores were taken under.
    pub copy: Duration,
    /// The longest read hold of the build's catch-up rounds.
    pub rounds: Duration,
    /// The write lock the build was put in place under, with the writes
    /// the last round left.
    pub put: Duration,
    /// The read lock the rewrite's stores were taken under, each graph's
    /// record written there.
    pub file_copy: Duration,
    /// The longest read hold of the rewrite's catch-up rounds.
    pub file_rounds: Duration,
    /// The write lock the rewrite took the file's place under.
    pub swap: Duration,
}

impl Holds {
    /// The longest of them.
    pub fn longest(&self) -> Duration {
        [
            self.copy,
            self.rounds,
            self.put,
            self.file_copy,
            self.file_rounds,
            self.swap,
        ]
        .into_iter()
        .max()
        .unwrap_or_default()
    }
}

/// `the build's copy 1.2ms, rounds 0.4ms, put in place 0.9ms; the file's
/// copy 2.1ms, rounds 3.0ms, swap 6.2ms`, the file's half where there was
/// one: what a server's log line says of a maintenance.
impl std::fmt::Display for Holds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the build's copy {:.1?}, rounds {:.1?}, put in place {:.1?}",
            self.copy, self.rounds, self.put
        )?;
        if self.swap > Duration::ZERO {
            write!(
                f,
                "; the file's copy {:.1?}, rounds {:.1?}, swap {:.1?}",
                self.file_copy, self.file_rounds, self.swap
            )?;
        }
        Ok(())
    }
}

/// When a hold of the lock began, to say how long it was. The browser
/// module has no clock (`Instant::now` panics there) and maintains nothing
/// beside a page: there it is nothing, and every hold none.
#[derive(Clone, Copy)]
struct Began {
    #[cfg(not(target_arch = "wasm32"))]
    at: std::time::Instant,
}

impl Began {
    #[inline]
    fn now() -> Began {
        Began {
            #[cfg(not(target_arch = "wasm32"))]
            at: std::time::Instant::now(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[inline]
    fn held(self) -> Duration {
        self.at.elapsed()
    }

    #[cfg(target_arch = "wasm32")]
    #[inline]
    fn held(self) -> Duration {
        Duration::ZERO
    }
}

/// What a test runs where a maintenance holds no lock ([`SEAM`]).
#[cfg(test)]
type Seam = Box<dyn FnMut(&str)>;

#[cfg(test)]
thread_local! {
    /// What a test runs where a maintenance holds no lock: `read` once the
    /// first value of a copy is read from its store ([`IndexCopy::read`]),
    /// `round` once a graph's round has looked its documents up and before
    /// it links them ([`graph_round`]). A writer gets through there, and
    /// its writes reach what is built.
    static SEAM: std::cell::RefCell<Option<Seam>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn seam(at: &str) {
    let f = SEAM.with(|s| s.borrow_mut().take());
    if let Some(mut f) = f {
        f(at);
        SEAM.with(|s| *s.borrow_mut() = Some(f));
    }
}

/// The writes a maintenance has to catch up with.
pub(super) struct Tail {
    token: u64,
    cid: u32,
    ids: Vec<DocId>,
    /// The collection's schema changed, or it was dropped.
    schema: bool,
}

#[derive(Default)]
pub(super) struct Tails {
    next: u64,
    pub(super) list: Vec<Tail>,
}

impl Tails {
    /// The ids written under the maintenance `token` since it was watched or
    /// last drained, taken out of its tail; `None` once its collection's
    /// schema changed or it is gone, which the maintenance will refuse.
    #[cfg(not(target_arch = "wasm32"))]
    fn drain(&mut self, token: u64) -> Option<Vec<DocId>> {
        let t = self.list.iter_mut().find(|t| t.token == token)?;
        (!t.schema).then(|| std::mem::take(&mut t.ids))
    }

    /// Every collection changed under the maintenances: an image adopted.
    pub(super) fn all_changed(&mut self) {
        for t in &mut self.list {
            t.schema = true;
        }
    }

    pub(super) fn note(&mut self, cid: u32, id: DocId) {
        for t in self.list.iter_mut().filter(|t| t.cid == cid) {
            if id == SCHEMA_MARK {
                t.schema = true;
            } else {
                t.ids.push(id);
            }
        }
    }
}

/// An index's field, copied: `(id, value)` in id order.
struct IndexCopy {
    token: u64,
    cid: u32,
    collection: String,
    field: String,
    pos: usize,
    kind: IndexKind,
    ty: DataType,
    /// How the field's text compares: the collation a `@sorted` index
    /// orders its text in, and the fold a `nocase` field's keys are.
    order: TextOrder,
    values: Vec<(DocId, Option<Value>)>,
    /// The collection's store as it stood when the copy was taken, cloned
    /// under the read lock -- its records in the file and its segments
    /// shared, its id index copied, 12 bytes a row -- and shared with the
    /// collection's other graphs: `values` is read from it with no lock
    /// held ([`Self::read`]), and it is let go of then.
    from: Option<Arc<Store>>,
    /// Where the field is in a payload, past the dropped places before it:
    /// what a catch-up round reads of a document looked up under the lock
    /// and read after it ([`crate::store::field_at`]).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    place: usize,
}

/// An index over several fields' keys, copied: each row's in id order, and
/// the empty index they go into.
#[cfg(feature = "composite")]
struct CompositeCopy {
    token: u64,
    cid: u32,
    collection: String,
    index: Composite,
    keys: Vec<(DocId, crate::composite::RowKey)>,
    /// What the keys were made by: the empty index over the parts.
    keyed: CompositeIndex,
    /// The store and its schema as they stood when the copy was taken:
    /// `keys` is read from them with no lock held, as an [`IndexCopy`]'s
    /// values are.
    from: Option<(Store, SchemaCell)>,
}

#[cfg(feature = "composite")]
impl CompositeCopy {
    /// Reads every document's key from the store as it stood, with no lock
    /// held.
    fn read(&mut self) -> Result<()> {
        let Some((store, schema)) = self.from.take() else {
            return Ok(());
        };
        self.keys.reserve_exact(store.len());
        let keys = &mut self.keys;
        super::composite_keys(&store, &schema, &self.index, &self.keyed, &mut |id, k| {
            keys.push((id, k))
        })
    }
}

/// One of these is made a maintenance and moved into place once, so the
/// graph's size beside the other variants' costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
enum Built {
    Vector(VectorIndex),
    Hash(HashIndex),
    Text(TextIndex),
    #[cfg_attr(not(feature = "sorted"), allow(dead_code))]
    Sorted(SortedIndex),
    Sparse(SparseIndex),
    #[cfg_attr(not(all(feature = "sorted", feature = "geo")), allow(dead_code))]
    Geo(GeoIndex),
}

/// The index built from the copy, and the copy: taking a write back out of
/// a hash, text or ordered index needs the value it went in with.
struct BuiltIndex {
    copy: IndexCopy,
    index: Built,
    /// A document the copy or a catch-up round could not read: `create
    /// index` is refused for it, and a compact keeps the graph as it stood.
    broken: Option<Error>,
}

/// Takes the maintenances' tails off the list however the run ends: on an
/// error or a panic in the build, the write path would otherwise go on
/// collecting ids for nobody.
struct Watching<'a> {
    db: &'a RwLock<Database>,
    tokens: Vec<u64>,
}

impl Drop for Watching<'_> {
    fn drop(&mut self) {
        if self.tokens.is_empty() {
            return;
        }
        let mut g = self.db.write().unwrap_or_else(|e| e.into_inner());
        for t in &self.tokens {
            g.unwatch(*t);
        }
    }
}

/// A compact's rewrite beside the database: every collection as it stood
/// when it was taken, its image written into a side file with no lock held,
/// and under the write lock only the writes made meanwhile added to it
/// before it takes the file's place. Written under the lock, 2.1 s of a
/// 1 GB file's compact held every writer out.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
struct Beside {
    side: crate::fs::SideFile,
    /// Whether the stores read their records from the mapped file, and are
    /// pointed at the new one; a database read into memory keeps its live
    /// records in fresh segments instead.
    mapped: bool,
    parts: Vec<BesidePart>,
    history: Option<Vec<u8>>,
    /// The named statements as they stood, written after the collections:
    /// a table defining others at the swap -- a definition or a drop
    /// meanwhile -- leaves the image not fitting. A slot's mark meanwhile
    /// does not: the swap writes each schedule's last mark as it stands
    /// then, so a schedule at a minute's cadence never fails a compact.
    statements: Arc<super::Statements>,
    reclaimed: usize,
    /// Where the counter's header is in the side file, and where the body
    /// it measures begins.
    head_at: u64,
    body_at: u64,
    /// Where the records of the writes made meanwhile hold each store's
    /// frames, as they are copied in: what the database hands over to its
    /// file once the side file is.
    landed: Vec<super::handover::Landed>,
    landed_bytes: u64,
    /// Whether the new file would not fit in memory beside the old one --
    /// or the old one does not fit already, the process waiting for the
    /// disk: written past the page cache, and its pages read in once it is
    /// the database's file rather than before ([`Beside::write`]). The
    /// file it becomes, to read in then.
    past: bool,
    after: Option<crate::store::Base>,
}

/// One collection of a [`Beside`], as it stood.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
struct BesidePart {
    token: u64,
    cid: u32,
    name: String,
    schema: Vec<u8>,
    next_id: DocId,
    /// The store as it stood: its records in the old file shared, the ones
    /// in memory copied. Pointed at the side file once its image is
    /// written, and handed the writes made meanwhile.
    store: Store,
    /// Whether its dead records are left out.
    compact: bool,
    /// Each graph's field, record, and changes when it was written.
    graphs: Vec<(String, Vec<u8>, u64)>,
}

/// Lets the next rewrite beside the database go ahead however this one
/// ends.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
struct Rewriting<'a>(&'a RwLock<Database>);

#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
impl Drop for Rewriting<'_> {
    fn drop(&mut self) {
        read(self.0)
            .beside
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

/// The database to copy what the maintenance builds from. A block is open
/// only under the write lock its owner holds, so whatever a read lock
/// finds has landed.
fn read(db: &RwLock<Database>) -> std::sync::RwLockReadGuard<'_, Database> {
    db.read().unwrap_or_else(|e| e.into_inner())
}

/// The database to put the maintenance's result in.
fn write(db: &RwLock<Database>) -> std::sync::RwLockWriteGuard<'_, Database> {
    db.write().unwrap_or_else(|e| e.into_inner())
}

impl Database {
    /// Runs a `create index` or a `compact` beside the database: the build
    /// holds no lock, and the write lock is taken only to catch up with the
    /// writes made during it and put the result in place. `None` for any
    /// other statement.
    pub fn maintain(db: &RwLock<Database>, stmt: &Statement) -> Option<Result<Response>> {
        Self::maintain_with(db, stmt, &mut || {})
    }

    /// [`Self::maintain`], calling `during` once each copy is taken and
    /// before what is built from it, with no lock held: how a test makes
    /// its writes land while a build runs, every time rather than when the
    /// timing happens to allow. A compact of a mapped file takes two, its
    /// graphs' and then its file's.
    #[doc(hidden)]
    pub fn maintain_with(
        db: &RwLock<Database>,
        stmt: &Statement,
        during: &mut dyn FnMut(),
    ) -> Option<Result<Response>> {
        Self::maintained(db, stmt, during).map(|(r, _)| r)
    }

    /// [`Self::maintain`], and how long it held the lock, each hold at its
    /// longest: what a server's log says of a statement it ran so. Where
    /// it put its result in place, [`Self::last_holds`] says the same until
    /// the next.
    pub fn maintain_held(
        db: &RwLock<Database>,
        stmt: &Statement,
    ) -> Option<(Result<Response>, Holds)> {
        Self::maintained(db, stmt, &mut || {})
    }

    fn maintained(
        db: &RwLock<Database>,
        stmt: &Statement,
        during: &mut dyn FnMut(),
    ) -> Option<(Result<Response>, Holds)> {
        let mut holds = Holds::default();
        // What runs under the write lock throughout, its hold its put.
        let mut held = |run: &dyn Fn(&mut Database) -> Result<Response>| {
            let mut g = write(db);
            let began = Began::now();
            let r = run(&mut g);
            holds.put = began.held();
            g.note_holds(&r, holds);
            r
        };
        let r = match stmt {
            // A path's index -- a hash or an ordered one, a value a row, 11
            // and 21 ms over 100 000 documents -- is built under the write
            // lock, as `execute` builds it.
            Statement::CreateIndex { field, .. } if field.contains('.') => {
                held(&|g| g.execute_with(stmt, &[]))
            }
            Statement::CreateIndex {
                collection,
                field,
                kind,
                if_not_exists,
            } => index_online(
                db,
                collection,
                field,
                kind,
                *if_not_exists,
                during,
                &mut holds,
            ),
            #[cfg(feature = "composite")]
            Statement::CreateComposite {
                collection,
                index,
                if_not_exists,
            } => composite_online(db, collection, index, *if_not_exists, during, &mut holds),
            // Refused, the feature named, as `execute` refuses it.
            #[cfg(not(feature = "composite"))]
            Statement::CreateComposite { .. } => held(&|g| g.execute_with(stmt, &[])),
            Statement::Compact(which) => compact_online(db, which.as_deref(), during, &mut holds),
            _ => return None,
        };
        Some((r, holds))
    }

    /// Notes how long the maintenance that put its result in place under
    /// this write lock held the lock ([`Self::last_holds`]); one refused
    /// notes nothing.
    #[inline]
    fn note_holds<T>(&mut self, r: &Result<T>, holds: Holds) {
        #[cfg(not(target_arch = "wasm32"))]
        if r.is_ok() {
            self.holds = holds;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (r, holds);
    }

    fn watch(&self, cid: u32) -> u64 {
        let mut t = self.tails.lock().unwrap_or_else(|e| e.into_inner());
        t.next += 1;
        let token = t.next;
        t.list.push(Tail {
            token,
            cid,
            ids: Vec::new(),
            schema: false,
        });
        // Set under the read lock, read under the write lock: the lock
        // orders the two, so no stronger ordering is needed.
        self.watched
            .store(true, std::sync::atomic::Ordering::Relaxed);
        token
    }

    fn unwatch(&mut self, token: u64) -> Option<Tail> {
        let t = self.tails.get_mut().unwrap_or_else(|e| e.into_inner());
        let at = t.list.iter().position(|x| x.token == token)?;
        let tail = t.list.swap_remove(at);
        *self.watched.get_mut() = !t.list.is_empty();
        Some(tail)
    }

    /// Checks a `create index`: `Some` answers it without building anything.
    pub(super) fn check_index(
        &self,
        collection: &str,
        field: &str,
        kind: &IndexKind,
        if_not_exists: bool,
    ) -> Result<Option<Response>> {
        let c = self
            .collections
            .get(collection)
            .ok_or_else(|| Error::NotFound(format!("collection `{collection}`")))?;
        // A path into a json field takes an index of its own, beside the
        // field's.
        let path = c
            .schema
            .path_of(field)?
            .map(|_| crate::schema::Field::new(field, DataType::Json));
        let f = match c.schema.path(field).or(path.as_ref()) {
            Some(f) => f,
            None => c
                .schema
                .field(field)
                .ok_or_else(|| Error::NotFound(format!("field `{field}` in `{collection}`")))?,
        };
        // One of its own, or `@text` beside a `@hash`, `@unique` or
        // `@sorted` of its own (`Field::add_index`); a path keeps one.
        let mut with = f.clone();
        if (path.is_some() && f.index != IndexKind::None) || with.add_index(kind.clone()).is_err() {
            if if_not_exists {
                return Ok(Some(Response::Ok(format!("`{field}` is already indexed"))));
            }
            return Err(Error::Exists(format!("an index on field `{field}`")));
        }
        kind.check(field, &f.ty)?;
        if kind.ttl().is_some() {
            let other = crate::schema::Field::new(field, f.ty.clone()).indexed(kind.clone());
            crate::schema::one_expiry(collection, c.schema.fields.iter().chain([&other]))?;
            #[cfg(feature = "statements")]
            self.expiry_held(collection)?;
        }
        Ok(None)
    }

    /// What a write is refused for before it runs: a storage error, a
    /// replica's -- whose writes come from its primary -- or a lapsed lease.
    /// A compact changes no document, so the last two let it run.
    pub(super) fn may_write(&self, stmt_is_compact: bool) -> Result<()> {
        self.refuse_if_failed()?;
        if stmt_is_compact {
            return Ok(());
        }
        if self.history.following {
            return Err(Error::ReadOnly(
                "this database is a replica: its writes come from its primary".into(),
            ));
        }
        self.fenced()
    }

    /// [`Self::may_write`] for a write that lands as a block: the lease is
    /// asked as the block lands ([`Database::commit`]), where a write it no
    /// longer allows is put back. Asked here as well, it was asked twice a
    /// write: a lone `put` took 898 ns against 827 without a fence, and
    /// asked once takes 846 against 822.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn may_start(&self) -> Result<()> {
        self.refuse_if_failed()?;
        if self.history.following {
            return Err(Error::ReadOnly(
                "this database is a replica: its writes come from its primary".into(),
            ));
        }
        Ok(())
    }

    /// A page has no fence, so the two are one, and this is inlined: a
    /// function of its own was 130 bytes of the browser module, and one
    /// calling the other 14.
    #[cfg(target_arch = "wasm32")]
    #[inline(always)]
    pub(super) fn may_start(&self) -> Result<()> {
        self.may_write(false)
    }

    fn begin_index(
        &self,
        collection: &str,
        field: &str,
        kind: &IndexKind,
        if_not_exists: bool,
    ) -> Result<std::result::Result<IndexCopy, Response>> {
        self.may_write(false)?;
        if let Some(feature) = super::missing_feature(kind) {
            return Err(super::not_built("the index", feature));
        }
        if let Some(done) = self.check_index(collection, field, kind, if_not_exists)? {
            return Ok(Err(done));
        }
        // The documents are read once the lock is let go
        // ([`IndexCopy::read`]): past the page cache each may wait for the
        // disk.
        let c = &self.collections[collection];
        let pos = c.schema.field_pos(field).unwrap();
        Ok(Ok(IndexCopy {
            token: self.watch(c.id),
            cid: c.id,
            collection: collection.to_string(),
            field: field.to_string(),
            pos,
            kind: kind.resolved(),
            ty: c.schema.fields[pos].ty.clone(),
            order: c.schema.fields[pos].text_order(),
            values: Vec::new(),
            from: Some(Arc::new(c.store.clone())),
            place: c.schema.place(pos),
        }))
    }

    fn finish_index(&mut self, b: BuiltIndex) -> Result<Response> {
        let BuiltIndex {
            copy,
            index,
            broken,
        } = b;
        let tail = self.unwatch(copy.token);
        self.refuse_if_failed()?;
        // A long build outlives a lease: the index lands only if it still holds.
        self.fenced()?;
        let changed = || {
            Error::Query(format!(
                "`{}` changed while the index was built; run `create index` again",
                copy.collection
            ))
        };
        let tail = tail.ok_or_else(changed)?;
        if tail.schema || self.named(copy.cid).as_deref() != Some(&copy.collection) {
            return Err(changed());
        }
        // A document a catch-up round could not read, as one read below.
        if let Some(e) = broken {
            return Err(e);
        }
        // A statement bound after `del` meanwhile -- a definition notes no
        // collection's tail -- refuses a `@ttl` here as at the start: this
        // lands outside `commit`, where a block's schema change is held.
        #[cfg(feature = "statements")]
        if copy.kind.ttl().is_some() {
            self.expiry_held(&copy.collection)?;
        }
        let c = self.collections.get_mut(&copy.collection).unwrap();
        let pos = copy.pos;
        let mut ids = tail.ids;
        ids.sort_unstable();
        ids.dedup();
        let old = |id: DocId| {
            copy.values
                .binary_search_by_key(&id, |v| v.0)
                .ok()
                .and_then(|k| copy.values[k].1.as_ref())
        };
        // Beside its other where it has one, as `check_index` found it
        // could be: the schema is the one it was then (`tail.schema`).
        // Asked of a copy before any index goes into its map, and put in
        // place only once one has: a refusal here or a failed read below
        // leaves the maps and the schema both as they were.
        let mut field = c.schema.fields[pos].clone();
        field.add_index(copy.kind.clone())?;
        // The writes made during the build, applied as the write path would
        // have: out with the value the build saw, in with the one there now.
        match index {
            Built::Vector(mut ix) => {
                // `insert` keeps a node that holds the vector already, and
                // retires it otherwise.
                for id in ids {
                    match c.store.read_field(id, pos)? {
                        Some(Value::Vector(v)) => ix.insert(id, &v),
                        _ => ix.remove(id),
                    }
                }
                c.vectors.insert(copy.field.clone(), ix);
            }
            Built::Hash(mut ix) => {
                for id in ids {
                    if let Some(v) = old(id) {
                        ix.remove(&ix.key(v), id);
                    }
                    if let Some(v) = c.store.read_field(id, pos)? {
                        ix.add(ix.key(&v), id);
                    }
                }
                // Asked of the index as it stands once the writes made
                // meanwhile are in: one of them may be the second holder,
                // named as it wrote the value -- a `nocase` key is a fold,
                // and a key files a time, a list's too, as its int.
                if let (true, Some((v, a, b))) = (copy.kind.is_unique(), ix.shared()) {
                    let v = c.store.read_field(a, pos)?.unwrap_or(v);
                    return Err(Error::Duplicate(format!(
                        "`{}.{}` cannot be unique: documents {a} and {b} both hold {}",
                        copy.collection,
                        copy.field,
                        crate::json::named(&v)
                    )));
                }
                c.hashes.insert(copy.field.clone(), Derived::new(ix));
            }
            Built::Text(mut ix) => {
                for id in ids {
                    if let Some(Value::Text(t)) = old(id) {
                        ix.remove(id, t);
                    }
                    if let Some(Value::Text(t)) = c.store.read_field(id, pos)? {
                        ix.insert(id, &t);
                    }
                }
                c.texts.insert(copy.field.clone(), Derived::new(ix));
            }
            Built::Sorted(mut ix) => {
                for id in ids {
                    ix.remove(id, old(id));
                    if let Some(v) = c.store.read_field(id, pos)? {
                        ix.insert(id, Some(&v));
                    }
                }
                c.sorted.push((copy.field.clone(), Derived::new(ix)));
            }
            Built::Sparse(mut ix) => {
                for id in ids {
                    if let Some(Value::Sparse(_, e)) = old(id) {
                        ix.remove(id, e);
                    }
                    if let Some(Value::Sparse(_, e)) = c.store.read_field(id, pos)? {
                        ix.insert(id, &e);
                    }
                }
                c.sparse.push((copy.field.clone(), Derived::new(ix)));
            }
            Built::Geo(mut ix) => {
                for id in ids {
                    ix.remove(id, old(id));
                    if let Some(v) = c.store.read_field(id, pos)? {
                        ix.insert(id, Some(&v));
                    }
                }
                c.geo.push((copy.field.clone(), Derived::new(ix)));
            }
        }
        crate::schema::SchemaCell::make_mut(&mut c.schema).fields[pos] = field;
        let (cid, encoded) = (c.id, c.schema.encode());
        self.wal(REC_ALTER, cid, &encoded)?;
        self.note(cid, SCHEMA_MARK);
        // A schema change landed outside `commit`: the named statements'
        // marks worked out again, as a replica applying this record works
        // them out -- the index a broken statement probes may be back.
        #[cfg(feature = "statements")]
        self.read_statements();
        if let Some(w) = &self.watcher {
            w.notify(self.changes.seq());
        }
        Ok(Response::Ok(format!(
            "index built on `{}.{}`",
            copy.collection, copy.field
        )))
    }

    /// What the keys of an index over several fields are read from with no
    /// lock held, as [`Self::begin_index`] takes a field's.
    #[cfg(feature = "composite")]
    fn begin_composite(
        &self,
        collection: &str,
        comp: &Composite,
        if_not_exists: bool,
    ) -> Result<std::result::Result<CompositeCopy, Response>> {
        self.may_write(false)?;
        if let Some(done) = self.check_composite(collection, comp, if_not_exists)? {
            return Ok(Err(done));
        }
        let c = &self.collections[collection];
        let keyed = super::empty_composite(&c.schema, comp)
            .ok_or_else(|| super::not_built("the index", "composite"))?;
        Ok(Ok(CompositeCopy {
            token: self.watch(c.id),
            cid: c.id,
            collection: collection.to_string(),
            index: comp.clone(),
            keys: Vec::new(),
            keyed,
            from: Some((c.store.clone(), c.schema.clone())),
        }))
    }

    /// The index built beside the database put in place under the write
    /// lock, once the writes made meanwhile are in it: each out with the
    /// key the copy saw, in with the one its row holds now.
    #[cfg(feature = "composite")]
    fn finish_composite(
        &mut self,
        copy: CompositeCopy,
        mut built: CompositeIndex,
    ) -> Result<Response> {
        let CompositeCopy {
            token,
            cid,
            collection,
            index,
            keys,
            ..
        } = copy;
        let tail = self.unwatch(token);
        self.refuse_if_failed()?;
        self.fenced()?;
        let changed = || {
            Error::Query(format!(
                "`{collection}` changed while the index was built; run `create index` again"
            ))
        };
        let tail = tail.ok_or_else(changed)?;
        if tail.schema || self.named(cid).as_deref() != Some(&collection) {
            return Err(changed());
        }
        let c = self.collections.get_mut(&collection).unwrap();
        let mut ids = tail.ids;
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            if let Ok(k) = keys.binary_search_by_key(&id, |e| e.0) {
                built.remove(id, keys[k].1.clone());
            }
            if let Some(doc) = c.store.read(&c.schema, id)? {
                built.insert(id, super::composite_key(&built, &index, &doc));
            }
        }
        // Asked of the index as it stands once the writes made meanwhile
        // are in: one of them may be the second holder.
        if let (true, Some((a, b))) = (index.unique, built.shared()) {
            return Err(super::composite_shared(c, &index, a, b));
        }
        let text = index.keys_text();
        crate::schema::SchemaCell::make_mut(&mut c.schema)
            .composites
            .push(index.clone());
        c.composites.push((index, Derived::new(built)));
        let encoded = c.schema.encode();
        self.wal(REC_ALTER, cid, &encoded)?;
        self.note(cid, SCHEMA_MARK);
        // As `finish_index` does: the marks worked out again.
        #[cfg(feature = "statements")]
        self.read_statements();
        if let Some(w) = &self.watcher {
            w.notify(self.changes.seq());
        }
        Ok(Response::Ok(format!(
            "index built on `{collection}` ({text})"
        )))
    }

    /// The graphs a compact of a mapped database rebuilds: those of `which`
    /// holding tombstones, each with its collection's store as it stood, to
    /// read their vectors from once the lock is let go ([`IndexCopy::read`])
    /// as `create index` reads them. Nothing else is: the records stay in
    /// the file until the rewrite streams the live ones into the new one.
    /// The vectors read here held the read lock -- and every writer, and
    /// every reader behind a waiting writer -- 5 to 6 s in the P6 soak's
    /// dry run 5, a page fault each past the page cache (20 000 documents
    /// of a 2 GB file mapped in a container of 512 MB).
    #[cfg(not(target_arch = "wasm32"))]
    fn begin_graphs(&self, which: Option<&str>) -> Result<Vec<IndexCopy>> {
        self.may_write(true)?;
        let targets: Vec<String> = match which {
            Some(n) => {
                self.collection(n)?;
                vec![n.to_string()]
            }
            None => self.order.clone(),
        };
        let mut copies = Vec::new();
        for name in targets {
            let c = &self.collections[&name];
            let mut store: Option<Arc<Store>> = None;
            for (field, ix) in &c.vectors {
                if ix.dead() == 0 {
                    continue;
                }
                let pos = c.schema.field_pos(field).unwrap();
                let from = store.get_or_insert_with(|| Arc::new(c.store.clone()));
                copies.push(IndexCopy {
                    token: self.watch(c.id),
                    cid: c.id,
                    collection: name.clone(),
                    field: field.clone(),
                    pos,
                    kind: c.schema.fields[pos].index.clone(),
                    ty: c.schema.fields[pos].ty.clone(),
                    order: TextOrder::BYTES,
                    values: Vec::new(),
                    from: Some(Arc::clone(from)),
                    place: c.schema.place(pos),
                });
            }
        }
        Ok(copies)
    }

    /// Puts the rebuilt graphs in place, each caught up with the writes
    /// the rounds left ([`catch_up_graphs`]). The rewrite follows
    /// ([`rewrite_beside`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn finish_graphs(&mut self, built: Vec<BuiltIndex>) -> Result<()> {
        let tails: Vec<Option<Tail>> = built.iter().map(|b| self.unwatch(b.copy.token)).collect();
        self.refuse_if_failed()?;
        for (b, t) in built.into_iter().zip(tails) {
            let moved = t.as_ref().is_none_or(|t| t.schema);
            if moved || self.named(b.copy.cid).as_deref() != Some(&b.copy.collection) {
                return Err(Error::Query(format!(
                    "`{}` changed while it was compacted; run `compact` again",
                    b.copy.collection
                )));
            }
            // A document it cannot read leaves the graph as it stands,
            // tombstones and all, and answering as it did: one built
            // without it would not, and the compact is never refused for
            // what a document holds.
            if b.broken.is_some() {
                continue;
            }
            let Built::Vector(mut ix) = b.index else {
                unreachable!("only graphs are rebuilt beside a mapped compact")
            };
            let c = self.collections.get_mut(&b.copy.collection).unwrap();
            let mut ids = t.unwrap().ids;
            ids.sort_unstable();
            ids.dedup();
            let caught: Result<()> = ids.into_iter().try_for_each(|id| {
                ix.remove(id);
                if let Some(Value::Vector(v)) = c.store.read_field(id, b.copy.pos)? {
                    ix.insert(id, &v);
                }
                Ok(())
            });
            // As where the copy was taken: a document it cannot read leaves
            // the graph as it stands.
            if caught.is_ok() {
                c.vectors.insert(b.copy.field, ix);
            }
        }
        // The writes made meanwhile may have left the new graphs a tombstone
        // or two; those wait for the next compact rather than having the
        // graph rebuilt again under the lock.
        Ok(())
    }

    /// What a rewrite beside the database copies under the read lock: each
    /// collection's schema, counter and store as they stand -- its records
    /// in the mapped file shared, the ones in memory copied -- and its
    /// graphs' records, written here, as `save_graphs` writes them, with
    /// the writers held off. `None` where the sink has no file to write
    /// beside, and the rewrite holds the write lock instead.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    fn begin_beside(&self, which: Option<&str>) -> Result<Option<Beside>> {
        self.may_write(true)?;
        let Some(path) = self.sink.lock().unwrap_or_else(|e| e.into_inner()).side() else {
            return Ok(None);
        };
        let targets: Vec<&str> = match which {
            Some(n) => {
                self.collection(n)?;
                vec![n]
            }
            None => self.order.iter().map(String::as_str).collect(),
        };
        let side = crate::fs::SideFile::create(&path)?;
        let mut parts = Vec::with_capacity(self.order.len());
        for name in &self.order {
            let c = &self.collections[name];
            let graphs = c
                .vectors
                .iter()
                .filter(|(_, ix)| !ix.is_empty())
                .map(|(f, ix)| (f.clone(), ix.serialize_graph(), ix.changes()))
                .collect();
            parts.push(BesidePart {
                token: 0,
                cid: c.id,
                name: name.clone(),
                schema: c.schema.encode(),
                next_id: c.store.next_id(),
                store: c.store.clone(),
                compact: targets.contains(&name.as_str()),
                graphs,
            });
        }
        let reclaimed = targets
            .iter()
            .map(|n| self.collections[*n].store.dead_bytes())
            .sum();
        let history = (self.history.following || !self.history.lineage.is_empty())
            .then(|| self.history.record());
        for p in &mut parts {
            p.token = self.watch(p.cid);
        }
        Ok(Some(Beside {
            side,
            mapped: self.mapped,
            parts,
            history,
            statements: self.statements.clone(),
            reclaimed,
            head_at: 0,
            body_at: 0,
            landed: Vec::new(),
            landed_bytes: 0,
            past: false,
            after: None,
        }))
    }

    /// Adds to a rewrite written beside the database the writes made while
    /// it was, and puts it in place: under the write lock, the side file
    /// takes the file's place and each collection the store pointed at it.
    /// A collection created, dropped or altered meanwhile leaves the image
    /// not fitting, and the compact says so rather than put it in place.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    fn finish_beside(&mut self, mut b: Beside) -> Result<(Response, Vec<Store>)> {
        let began = std::time::Instant::now();
        let tails: Vec<Option<Tail>> = b.parts.iter().map(|p| self.unwatch(p.token)).collect();
        self.refuse_if_failed()?;
        let history = (self.history.following || !self.history.lineage.is_empty())
            .then(|| self.history.record());
        let same = self.order.len() == b.parts.len()
            && history == b.history
            && (Arc::ptr_eq(&self.statements, &b.statements)
                || self.statements.defines_as(&b.statements))
            && b.parts.iter().zip(&tails).all(|(p, t)| {
                t.as_ref().is_some_and(|t| !t.schema)
                    && self.collections.get(&p.name).is_some_and(|c| c.id == p.cid)
            });
        if !same {
            return Err(Error::Query(
                "the collections changed while they were compacted; run `compact` again".into(),
            ));
        }
        // What was written since the last round of catching up: the rest of
        // the image.
        let mut docs = crate::store::Snapshot::default();
        for (p, t) in b.parts.iter_mut().zip(tails) {
            let live = &self.collections[&p.name];
            let mut ids = t.unwrap().ids;
            ids.sort_unstable();
            ids.dedup();
            live.store.snapshot(&ids, &mut docs)?;
            copy_writes(p, &docs, &mut b.side, &mut b.landed, &mut b.landed_bytes)?;
            // An id handed out and deleted meanwhile left no record; it must
            // not come back.
            let next = live.store.next_id();
            if next > p.store.next_id() {
                p.store.raise_next_id(next);
                let mut counter = Vec::with_capacity(9);
                put_uvarint(&mut counter, next);
                put_record(&mut b.side, REC_NEXTID, p.cid, &counter)?;
            }
        }
        // Each schedule's last mark as it stands at the swap, after the
        // definitions the image holds: one landed meanwhile, or pending.
        let side = &mut b.side;
        self.last_marks(&mut |body| put_record(side, super::REC_STATEMENT, 0, body))?;
        let len = b.side.at();
        // In place, in bytes a mapping covers (`Beside::write`'s), which a
        // `&[u8]` must never see change: its stores are this compact's
        // alone until the swap below, on this thread, and no slice of it
        // outlives the read that took it (`fs::Mapping`).
        b.side
            .patch(b.head_at + 1, &self.changes.seq().to_le_bytes())?;
        b.side
            .patch(b.head_at + 9, &(len - b.body_at).to_le_bytes())?;
        b.side.sync()?;
        let r = self.sink_mut().adopt(b.side.path(), b.side.file());
        self.storage(r)?;
        b.side.adopted();
        // The file is the new one now: a panic before every store points
        // at it fails the database, as one in a compact under the lock does.
        self.unwinding(
            |db| Ok(db.adopted_beside(b, len, began)),
            Self::compact_unwound,
        )
    }

    /// [`Self::finish_beside`] once the side file took the file's place:
    /// each store pointed at it, and the stores they replace handed back.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    fn adopted_beside(
        &mut self,
        mut b: Beside,
        len: u64,
        began: std::time::Instant,
    ) -> (Response, Vec<Store>) {
        #[cfg(test)]
        super::unwinding::panic_at("adopted");
        *self.appended.get_mut() = len;
        // A database read into memory hands nothing over to its file.
        match b.mapped {
            true => {
                self.landed = std::mem::take(&mut b.landed);
                self.landed_bytes = b.landed_bytes;
            }
            false => self.forget_landed(),
        }
        // The stores they replace go back to the caller, to be let go of
        // once the lock is: their index, their segments and the old file's
        // mapping, the last of which unmapped the file -- 10 to 15 ms of a
        // 19 to 31 ms swap over a 1 GB file.
        let mut retired = Vec::with_capacity(b.parts.len());
        for p in b.parts {
            let c = self.collections.get_mut(&p.name).unwrap();
            retired.push(std::mem::replace(&mut c.store, p.store));
            // Each graph is as its record left it, and changed by the
            // writes after it.
            for (field, graph, changes) in &p.graphs {
                if let Some(ix) = c.vectors.get(field) {
                    let saved = ix.persisted();
                    saved.changes.store(*changes, Relaxed);
                    saved.at.store(len, Relaxed);
                    saved
                        .node_bytes
                        .store((graph.len() / ix.len().max(1)) as u64, Relaxed);
                }
            }
        }
        self.dirty = false;
        self.compacted_to(len);
        self.swap_held = began.elapsed();
        let done = Response::Ok(format!("compaction done, {} bytes reclaimed", b.reclaimed));
        (done, retired)
    }
}

/// Copies into a rewrite written beside the database the writes made since
/// it was taken, or since the last round: the ids drained from the tails at
/// once, and their documents a few thousand at a time -- looked up under the
/// read lock and copied after it ([`Store::snapshot`]). Copied under it, a
/// round held the writers out for as long as its reads of the file took:
/// one of 408 708 ids held it 592 ms, and with the pages first brought in
/// outside it a writer at 170k updates a second still ran at 910 through
/// the worst 100 ms of a 1 GB file's first round. A document written again
/// after its copy is in the tail again, for the next round. How many ids it
/// copied; the next round takes those written meanwhile, and the write lock
/// only the last few. `longest` is the longest hold of the read lock so
/// far.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
fn catch_up(db: &RwLock<Database>, b: &mut Beside, longest: &mut Duration) -> Result<usize> {
    let tokens: Vec<u64> = b.parts.iter().map(|p| p.token).collect();
    let Some(drained) = drain(db, &tokens, longest)? else {
        return Ok(0);
    };
    let mut copied = 0;
    let mut docs = crate::store::Snapshot::default();
    for (p, mut ids) in b.parts.iter_mut().zip(drained) {
        ids.sort_unstable();
        ids.dedup();
        copied += ids.len();
        for chunk in ids.chunks(CATCH_UP_CHUNK) {
            if !looked_up(db, &p.name, p.cid, chunk, &mut docs, longest)? {
                return Ok(0);
            }
            copy_writes(p, &docs, &mut b.side, &mut b.landed, &mut b.landed_bytes)?;
        }
    }
    Ok(copied)
}

/// The ids written under each of `tokens` since the last round, drained
/// under one hold of the read lock; `None` where a collection's schema
/// changed or it is gone, which the finish refuses.
#[cfg(not(target_arch = "wasm32"))]
fn drain(
    db: &RwLock<Database>,
    tokens: &[u64],
    longest: &mut Duration,
) -> Result<Option<Vec<Vec<DocId>>>> {
    let g = read(db);
    let began = Began::now();
    g.refuse_if_failed()?;
    let mut tails = g.tails.lock().unwrap_or_else(|e| e.into_inner());
    let drained = tokens.iter().map(|t| tails.drain(*t)).collect();
    drop(tails);
    *longest = (*longest).max(began.held());
    Ok(drained)
}

/// `ids` of the collection `name` looked up under one hold of the read lock
/// into `docs` ([`Store::snapshot`]), to be read once it is let go; `false`
/// where the collection is gone, which the finish refuses.
#[cfg(not(target_arch = "wasm32"))]
fn looked_up(
    db: &RwLock<Database>,
    name: &str,
    cid: u32,
    ids: &[DocId],
    docs: &mut crate::store::Snapshot,
    longest: &mut Duration,
) -> Result<bool> {
    let g = read(db);
    let began = Began::now();
    let Some(live) = g.collections.get(name).filter(|c| c.id == cid) else {
        return Ok(false);
    };
    live.store.snapshot(ids, docs)?;
    *longest = (*longest).max(began.held());
    Ok(true)
}

/// Catches the graphs built beside the database up with the writes made
/// while they were, in rounds: each round's ids drained from the tails at
/// once, their documents looked up under the read lock a few thousand at a
/// time and read after it, as the rewrite's rounds copy theirs
/// ([`catch_up`]), and each vector linked into its graph with no lock held
/// -- under the write lock, a link at 768 dimensions takes about a
/// millisecond, and a build of seconds meets thousands of writes. A round
/// catching fewer than [`GRAPH_DONE`] ends them, and the write lock takes
/// the few written during it. `retire`: a write is taken out of the graph
/// before it goes in again, as a compact's rebuilt graph always took the
/// writes made meanwhile (`finish_graphs`); else a node holding the vector
/// already is kept, as `create index` keeps it (`finish_index`).
#[cfg(not(target_arch = "wasm32"))]
fn catch_up_graphs(
    db: &RwLock<Database>,
    built: &mut [BuiltIndex],
    retire: bool,
    longest: &mut Duration,
) -> Result<()> {
    for _ in 0..CATCH_UP_ROUNDS {
        if graph_round(db, built, retire, longest)? < GRAPH_DONE {
            break;
        }
    }
    Ok(())
}

/// A round of [`catch_up_graphs`]: how many ids it caught, 0 once a
/// collection changed, which the finish refuses.
#[cfg(not(target_arch = "wasm32"))]
fn graph_round(
    db: &RwLock<Database>,
    built: &mut [BuiltIndex],
    retire: bool,
    longest: &mut Duration,
) -> Result<usize> {
    let tokens: Vec<u64> = built.iter().map(|b| b.copy.token).collect();
    let Some(drained) = drain(db, &tokens, longest)? else {
        return Ok(0);
    };
    let mut caught = 0;
    let mut docs = crate::store::Snapshot::default();
    for (b, mut ids) in built.iter_mut().zip(drained) {
        // A graph left as it stood takes nothing more.
        let (Built::Vector(ix), None) = (&mut b.index, &b.broken) else {
            continue;
        };
        ids.sort_unstable();
        ids.dedup();
        caught += ids.len();
        let place = b.copy.place;
        for chunk in ids.chunks(CATCH_UP_CHUNK) {
            if !looked_up(
                db,
                &b.copy.collection,
                b.copy.cid,
                chunk,
                &mut docs,
                longest,
            )? {
                return Ok(0);
            }
            #[cfg(test)]
            seam("round");
            let read = docs.each(|id, payload| {
                let v = payload
                    .map(|p| crate::store::field_at(p, place))
                    .transpose()?;
                match v {
                    Some(Value::Vector(v)) => {
                        if retire {
                            ix.remove(id);
                        }
                        ix.insert(id, &v);
                    }
                    _ => ix.remove(id),
                }
                Ok(())
            });
            if let Err(e) = read {
                b.broken = Some(e);
                break;
            }
        }
    }
    Ok(caught)
}

/// Each of `docs` (ascending, once each) takes the state it had when they
/// were looked up, over the one the image holds, in a data record appended
/// to the side file: inside the image, where the counter's header says it
/// stands. The store holds those frames alone in memory, and hands them
/// over as the ones of a record appended later ([`Database::hand_over`]).
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
fn copy_writes(
    p: &mut BesidePart,
    docs: &crate::store::Snapshot,
    side: &mut crate::fs::SideFile,
    landed: &mut Vec<super::handover::Landed>,
    landed_bytes: &mut u64,
) -> Result<()> {
    let mut frames = Vec::new();
    docs.each(|id, payload| {
        match payload {
            Some(payload) => frames.extend(p.store.append(OP_PUT, id, payload)),
            None if p.store.contains(id) => frames.extend(p.store.append(OP_DEL, id, &[])),
            None => {}
        }
        Ok(())
    })?;
    if !frames.is_empty() {
        let mut data = Summed::begin(side, REC_DATA, p.cid, frames.len())?;
        let at = data.at();
        super::handover::note(landed, landed_bytes, p.cid, frames.len(), at);
        data.write(&frames)?;
        data.end()?;
    }
    Ok(())
}

#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
impl Beside {
    /// Hands the frames the catch-up copied into each store over to the
    /// side file, as the database hands its own over to its file, with no
    /// lock held. Left to the database, the first write after the swap
    /// handed over the 400 MB the rounds had copied, under the write lock:
    /// readers waited 158 ms.
    fn hand_over(&mut self) -> Result<()> {
        if !self.mapped || self.landed.is_empty() {
            return Ok(());
        }
        let base = self.side.map()?;
        let mut runs = Vec::new();
        for p in &mut self.parts {
            runs.clear();
            runs.extend(
                self.landed
                    .iter()
                    .map(|l| l.run())
                    .filter(|r| r.0 == p.cid)
                    .map(|(_, len, at)| (len, at)),
            );
            // A store that refuses keeps its frames, and its runs stay noted
            // for the database's next handover.
            if !runs.is_empty() && p.store.hand_over(&base, &runs) {
                let cid = p.cid;
                self.landed.retain(|l| l.run().0 != cid);
            }
        }
        self.landed_bytes = self.landed.iter().map(|l| l.run().1).sum();
        Ok(())
    }

    /// Writes the image into the side file, with no lock held, in the
    /// layout `Database::image_into` writes, and points each store at it.
    fn write(&mut self) -> Result<()> {
        let side = &mut self.side;
        self.head_at = MAGIC.len() as u64;
        // The counter and the body's length, both put in under the lock.
        side.write(&image_head(0))?;
        self.body_at = side.at();
        if let Some(h) = &self.history {
            side.write(h)?;
        }
        // Read into memory, the live records are copied into fresh segments
        // here, with no lock held, and stay there: the database keeps them
        // in memory, as it was opened to. Over a mapped file the documents
        // holding a dropped field's value are written again into the copy's
        // segments, until it is pointed at the new file below. Either way a
        // dropped place's value becomes a `null` and the place stays.
        for p in self.parts.iter_mut().filter(|p| p.compact) {
            match self.mapped {
                false => p.store = p.store.compacted()?,
                true => p.store.reclaim()?,
            }
        }
        let mut placed = Vec::with_capacity(self.parts.len());
        for p in &self.parts {
            put_record(side, REC_CREATE, p.cid, &p.schema)?;
            let bytes = match p.compact {
                true => p.store.live_len(),
                false => p.store.image_len(),
            };
            // As `Database::image_into` writes it, the frames' index behind
            // the counter.
            let mut counter = Vec::with_capacity(9);
            put_uvarint(&mut counter, p.next_id);
            if bytes > 0 {
                p.store.image_index(p.compact, &mut counter);
            }
            put_record(side, REC_NEXTID, p.cid, &counter)?;
            if bytes > 0 {
                let mut data = Summed::begin(side, REC_DATA, p.cid, bytes)?;
                placed.push(Some(data.at()));
                match p.compact {
                    true => p.store.compact_live(&mut data)?,
                    false => p.store.write_image(&mut data)?,
                }
                data.end()?;
            } else {
                placed.push(None);
            }
            for (field, graph, _) in &p.graphs {
                let mut payload = Vec::with_capacity(field.len() + 9 + graph.len());
                crate::codec::encode_str(&mut payload, field);
                payload.extend_from_slice(graph);
                put_record(side, REC_GRAPH, p.cid, &payload)?;
            }
        }
        for st in self.statements.iter() {
            put_record(side, super::REC_STATEMENT, 0, &st.encode())?;
        }
        // The image on disk now, so that the fsync under the lock covers
        // only what is added there.
        side.sync()?;
        if !self.mapped {
            return Ok(());
        }
        let base = side.map()?;
        // Every page of the new file is touched here, with no lock held: left
        // to the readers, the first read of each after the swap waited on its
        // fault, the read p50 went from 2 to 100 us for two seconds and the
        // updates from 150k to 4k a second. Touched here a fault at a time,
        // 1.2 GB took 3 s; asked for ahead ([`crate::fs::touch`]), 0.7.
        //
        // Unless the two files do not fit in memory together: read in now,
        // the new file's pages push out the old one's, which the reads and
        // the writes need until the swap -- so they are read in after it,
        // the old file's pages gone with it. In a container held to 2 GB,
        // compacting at 1.4-1.9 GB into 1.1, read in before the swap the
        // writer ran at 6.2k updates a second through a compact and the
        // swap held the lock 24-60 ms, its last writes' pages pushed out;
        // after it, 18.6k and 6-14 ms, 5M updates in 85 s against 170.
        match self.past {
            false => crate::fs::touch(&base),
            true => self.after = Some(base.clone()),
        }
        for (p, at) in self.parts.iter_mut().zip(placed) {
            match at {
                Some(at) if p.compact => p.store.relocate_live(&base, at),
                Some(at) => p.store.relocate_image(&base, at),
                None => p.store.let_go(),
            }
        }
        Ok(())
    }
}

impl IndexCopy {
    /// Reads the field of every document of the store as it stood when the
    /// copy was taken, with no lock held, and lets the store go: a record
    /// there never moves -- the file is only appended to, and a store
    /// cloned shares nothing a write changes in place (`Arc::make_mut`), as
    /// a pinned read's does -- so what it reads is what the read lock would
    /// have read then. The writes made since are each in the copy's tail.
    fn read(&mut self) -> Result<()> {
        let Some(store) = self.from.take() else {
            return Ok(());
        };
        self.values.reserve_exact(store.len());
        for id in store.iter_ids() {
            self.values.push((id, store.read_field(id, self.pos)?));
            #[cfg(test)]
            if self.values.len() == 1 {
                seam("read");
            }
        }
        Ok(())
    }

    /// A compact's graph built from the copy -- or, where a document of it
    /// does not read, from nothing and marked so, for `finish_graphs` to
    /// keep the graph as it stood.
    #[cfg(not(target_arch = "wasm32"))]
    fn rebuilt(mut self) -> BuiltIndex {
        match self.read() {
            Ok(()) => self.build(),
            Err(e) => {
                self.values.clear();
                BuiltIndex {
                    broken: Some(e),
                    ..self.build()
                }
            }
        }
    }

    fn build(mut self) -> BuiltIndex {
        let index = match &self.kind {
            IndexKind::Vector(spec) => {
                let DataType::Vector(dim, prec) = self.ty else {
                    unreachable!("checked when the copy was taken")
                };
                let mut ix = VectorIndex::with_precision(dim, *spec, prec);
                // A vector comes out of the graph by its id alone, so the
                // copy gives them up rather than holding a second set.
                let items: Vec<(DocId, Vec<f32>)> = self
                    .values
                    .iter_mut()
                    .filter_map(|(id, v)| match v.take() {
                        Some(Value::Vector(v)) => Some((*id, v)),
                        _ => None,
                    })
                    .collect();
                ix.reserve(items.len());
                ix.insert_batch(&items);
                Built::Vector(ix)
            }
            IndexKind::Hash { .. } => {
                let mut ix = HashIndex::folding(self.order.fold);
                for (id, v) in &self.values {
                    if let Some(v) = v {
                        ix.add(ix.key(v), *id);
                    }
                }
                Built::Hash(ix)
            }
            IndexKind::Text(spec) => {
                let mut ix = TextIndex::new(*spec);
                for (id, v) in &self.values {
                    if let Some(Value::Text(t)) = v {
                        ix.insert(*id, t);
                    }
                }
                ix.shrink_to_fit();
                Built::Text(ix)
            }
            #[cfg(not(feature = "sorted"))]
            IndexKind::Sorted { .. } => unreachable!("`create index` refuses what the build lacks"),
            #[cfg(feature = "sorted")]
            IndexKind::Sorted { .. } => Built::Sorted(SortedIndex::build(
                &self.ty,
                self.order,
                &mut self.values.iter().map(|(id, v)| (*id, v.clone())),
            )),
            IndexKind::Inverted => {
                let mut ix = SparseIndex::new();
                for (id, v) in &self.values {
                    if let Some(Value::Sparse(_, e)) = v {
                        ix.insert(*id, e);
                    }
                }
                ix.shrink_to_fit();
                Built::Sparse(ix)
            }
            #[cfg(not(all(feature = "sorted", feature = "geo")))]
            IndexKind::Geo => unreachable!("`create index` refuses what the build lacks"),
            #[cfg(all(feature = "sorted", feature = "geo"))]
            IndexKind::Geo => {
                Built::Geo(GeoIndex::build(&mut self.values.iter().filter_map(
                    |(id, v)| Some((*id, crate::geo::point_in(v.as_ref()?)?)),
                )))
            }
            IndexKind::None => unreachable!("a create index names its kind"),
        };
        BuiltIndex {
            copy: self,
            index,
            broken: None,
        }
    }
}

fn index_online(
    db: &RwLock<Database>,
    collection: &str,
    field: &str,
    kind: &IndexKind,
    if_not_exists: bool,
    during: &mut dyn FnMut(),
    holds: &mut Holds,
) -> Result<Response> {
    let mut copy = {
        let g = read(db);
        let began = Began::now();
        let copy = g.begin_index(collection, field, kind, if_not_exists);
        holds.copy = began.held();
        match copy? {
            Ok(copy) => copy,
            Err(done) => return Ok(done),
        }
    };
    let mut watching = Watching {
        db,
        tokens: vec![copy.token],
    };
    during();
    copy.read()?;
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut built = copy.build();
    // A graph's build takes seconds, and the writes made meanwhile are
    // linked into it beside the database too. The other indexes' builds
    // take milliseconds -- 11 to 21 ms over 100 000 documents -- and catch
    // up under the write lock.
    #[cfg(not(target_arch = "wasm32"))]
    if matches!(built.index, Built::Vector(_)) {
        catch_up_graphs(
            db,
            std::slice::from_mut(&mut built),
            false,
            &mut holds.rounds,
        )?;
    }
    let r = {
        let mut g = write(db);
        let began = Began::now();
        let r = g.finish_index(built);
        holds.put = began.held();
        g.note_holds(&r, *holds);
        r
    };
    watching.tokens.clear();
    r
}

/// `create index` over several fields beside the database: the keys read
/// from the store as it stood and put in order with no lock held -- the
/// inserts are the build -- and the writes made meanwhile caught up with
/// under the write lock.
#[cfg(feature = "composite")]
fn composite_online(
    db: &RwLock<Database>,
    collection: &str,
    comp: &Composite,
    if_not_exists: bool,
    during: &mut dyn FnMut(),
    holds: &mut Holds,
) -> Result<Response> {
    let mut copy = {
        let g = read(db);
        let began = Began::now();
        let copy = g.begin_composite(collection, comp, if_not_exists);
        holds.copy = began.held();
        match copy? {
            Ok(copy) => copy,
            Err(done) => return Ok(done),
        }
    };
    let mut watching = Watching {
        db,
        tokens: vec![copy.token],
    };
    during();
    copy.read()?;
    let built = CompositeIndex::build(copy.keyed.parts().to_vec(), copy.keys.clone());
    let r = {
        let mut g = write(db);
        let began = Began::now();
        let r = g.finish_composite(copy, built);
        holds.put = began.held();
        g.note_holds(&r, *holds);
        r
    };
    watching.tokens.clear();
    r
}

fn compact_online(
    db: &RwLock<Database>,
    which: Option<&str>,
    during: &mut dyn FnMut(),
    holds: &mut Holds,
) -> Result<Response> {
    // No record is copied under a lock: the graphs holding tombstones are
    // rebuilt here, beside the database, and the rewrite -- the live records
    // streamed into a new file, 2.1 s for 1 GB, a dropped field's values
    // reclaimed on the way and its places kept -- is written beside it too.
    // Copying every record into a fresh store under the read lock and
    // building every index again, as a database read into memory once did,
    // took a 427 MB file to +899 MB of heap; the indexes are keyed by id, and
    // the documents are the same ones. A collection holding a dropped place
    // was compacted under the write lock, its places taken out, until a
    // compact kept them (docs/design/storage.md#dropped-places).
    #[cfg(not(target_arch = "wasm32"))]
    {
        let copies = {
            let g = read(db);
            let began = Began::now();
            let copies = g.begin_graphs(which);
            holds.copy = began.held();
            copies?
        };
        let mut watching = Watching {
            db,
            tokens: copies.iter().map(|c| c.token).collect(),
        };
        during();
        let mut built: Vec<BuiltIndex> = copies.into_iter().map(IndexCopy::rebuilt).collect();
        catch_up_graphs(db, &mut built, true, &mut holds.rounds)?;
        let r = {
            let mut g = write(db);
            let began = Began::now();
            let r = g.finish_graphs(built);
            holds.put = began.held();
            r
        };
        watching.tokens.clear();
        r?;
        rewrite_beside(db, which, during, holds)
    }
    // The browser module has no thread to build beside the page and no
    // file to write beside its own, and maintains nothing (fennec-wasm
    // calls no `maintain`): its compact is `execute`'s, under the lock.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (during, holds);
        let mut g = write(db);
        g.may_write(true)?;
        g.compact(which, true)
    }
}

/// Rounds of catching up with the writes made meanwhile before the write
/// lock is taken for the rest; each takes those written during the one
/// before, and a round under `CATCH_UP_DONE` ids ends them -- a graph's
/// rounds, under [`GRAPH_DONE`].
#[cfg(not(target_arch = "wasm32"))]
const CATCH_UP_ROUNDS: usize = 16;
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
const CATCH_UP_DONE: usize = 1_000;
/// A graph's round catching fewer ids than this ends its rounds
/// ([`catch_up_graphs`]): each is a link into the graph under the write
/// lock that follows, where a rewrite's is a copy of the record.
#[cfg(not(target_arch = "wasm32"))]
const GRAPH_DONE: usize = 64;
/// The documents a catch-up looks up under one hold of the read lock, and
/// copies after it: 4 096, held 95 to 245 us at the median and 3.1 ms at
/// the most, where copied under it they held it about 6 ms, and up to 291
/// with their pages read from the disk.
#[cfg(not(target_arch = "wasm32"))]
const CATCH_UP_CHUNK: usize = 4_096;

/// The rewrite a compact of a mapped file ends with: the live records
/// written into a side file beside the database, the writes made meanwhile
/// added under the write lock, and the side file put in the file's place.
/// Where the sink has no file to write beside, the rewrite holds the lock.
#[cfg(not(target_arch = "wasm32"))]
fn rewrite_beside(
    db: &RwLock<Database>,
    which: Option<&str>,
    during: &mut dyn FnMut(),
    holds: &mut Holds,
) -> Result<Response> {
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    {
        use std::sync::atomic::Ordering::AcqRel;
        if read(db).beside.swap(true, AcqRel) {
            return Err(Error::Query(
                "a compact is writing its file beside the database already".into(),
            ));
        }
        let _rewriting = Rewriting(db);
        let (begun, need) = {
            let g = read(db);
            let began = Began::now();
            let need = g.garbage();
            let begun = g.begin_beside(which);
            holds.file_copy = began.held();
            (begun?, need.file + need.live + need.kept)
        };
        if let Some(mut b) = begun {
            let room = crate::cache::room();
            // The old file and the new one, both read in before the swap --
            // or the old file alone, where the process waits for the disk.
            if b.mapped && room.is_some_and(|r| need > r || crate::cache::faulting()) {
                b.past = true;
                b.side.past_cache();
            }
            // Past the room, the old file read from its start to its end
            // rather than at random by id (`store::InTurn`).
            if b.mapped {
                b.side.copies_in_turn();
            }
            let mut watching = Watching {
                db,
                tokens: b.parts.iter().map(|p| p.token).collect(),
            };
            during();
            b.write()?;
            // The writes made while the image was written are copied in a
            // round at a time -- looked up under the read lock, copied after
            // it -- and the side file synced with no lock held, until a round
            // finds few: taken under the write lock all at once, the 200 000
            // updates a 4 s compact of a 1 GB file met held every reader 409
            // to 811 ms.
            for _ in 0..CATCH_UP_ROUNDS {
                let copied = catch_up(db, &mut b, &mut holds.file_rounds)?;
                b.side.sync()?;
                if copied < CATCH_UP_DONE {
                    break;
                }
            }
            b.hand_over()?;
            let after = b.after.take();
            // The guard goes at the end of the block, and the stores it
            // handed back after it.
            let r = {
                let mut g = write(db);
                let began = Began::now();
                let r = g.finish_beside(b);
                holds.swap = began.held();
                g.note_holds(&r, *holds);
                r
            };
            watching.tokens.clear();
            let done = r.map(|(done, _retired)| done)?;
            // Read in where it fits on its own; past the room it would push
            // out its own pages as it went.
            if let Some(base) =
                after.filter(|b| room.is_some_and(|r| (**b).as_ref().len() as u64 <= r))
            {
                crate::fs::touch(&base);
            }
            crate::cache::forget_faults();
            return Ok(done);
        }
    }
    let _ = during;
    let mut g = write(db);
    let began = Began::now();
    let r = g.compact(which, false);
    holds.swap = began.held();
    g.note_holds(&r, *holds);
    r
}

#[cfg(all(test, feature = "vector"))]
mod tests {
    use super::*;
    use crate::codec::{TAG_LIST, TAG_NULL};
    use crate::schema::{Field, VectorIndexSpec};
    use crate::value::VecPrec;

    fn put(db: &mut Database, id: i64, v: [f32; 2]) {
        db.execute(&Statement::Put {
            collection: "g".into(),
            docs: vec![vec![
                ("id".into(), Expr::Lit(Value::Int(id))),
                ("meta".into(), Expr::Lit(Value::Text("m".into()))),
                ("v".into(), Expr::Lit(Value::Vector(v.to_vec()))),
            ]],
            docs_param: None,
            insert: false,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        })
        .expect("put");
    }

    /// A compact builds a graph holding tombstones again from every
    /// document's vector: one it cannot read leaves the graph as it stood,
    /// tombstones and all -- answering as it did, where one built without
    /// the document would not -- and the compact goes on, under the lock
    /// and beside the database. No file brings such a document to it (an
    /// open, `create index`, a write and a replica's apply each read the
    /// vector first and refuse it: `tests/compact_places.rs`), so it is put
    /// in the store here as an open would have held it.
    #[test]
    fn a_graph_a_compact_cannot_rebuild_stays_as_it_stood() {
        for beside in [false, true] {
            let mut db = Database::new();
            let schema = Schema::new(
                "g",
                vec![
                    Field::new("meta", DataType::Json),
                    Field::new("v", DataType::Vector(2, VecPrec::F32))
                        .indexed(IndexKind::Vector(VectorIndexSpec::default().resolved())),
                ],
            )
            .expect("schema");
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .expect("create");
            put(&mut db, 1, [1.0, 0.0]);
            put(&mut db, 2, [0.0, 1.0]);
            // Written over: a tombstone, which a compact takes out by
            // building the graph again.
            put(&mut db, 1, [1.0, 0.2]);
            // `meta` 100 lists deep, then the vector.
            let mut deep = Vec::new();
            for _ in 0..100 {
                deep.extend_from_slice(&[TAG_LIST, 1]);
            }
            deep.push(TAG_NULL);
            crate::codec::encode_value(&mut deep, &Value::Vector(vec![0.6, 0.8]));
            let c = db.collections.get_mut("g").expect("g");
            c.store.append(OP_PUT, 3, &deep);
            assert!(c.store.read_field(3, 1).is_err());
            let graph = |db: &Database| {
                let ix = &db.collection("g").expect("g").vectors["v"];
                (ix.len(), ix.dead())
            };
            let stood = graph(&db);
            assert_eq!(stood.1, 1);
            let db = match beside {
                false => {
                    db.execute(&Statement::Compact(None)).expect("compact");
                    db
                }
                true => {
                    let lock = RwLock::new(db);
                    Database::maintain(&lock, &Statement::Compact(None))
                        .expect("a maintenance")
                        .expect("compact");
                    lock.into_inner().expect("lock")
                }
            };
            assert_eq!(graph(&db), stood, "beside: {beside}");
            assert_eq!(
                db.collection("g").expect("g").store.len(),
                3,
                "beside: {beside}"
            );
        }
    }

    /// `id`'s vector, the `gen`th it was written: no two alike once a
    /// cosine graph makes them units, so none is another's alias ("a vector
    /// written again is one node").
    fn vec_of(id: i64, gen: i64) -> [f32; 2] {
        [1.0, (gen * 100_000 + id) as f32]
    }

    fn del(db: &mut Database, id: i64) {
        db.execute(&Statement::Delete {
            collection: "g".into(),
            filter: Some(Expr::Cmp(
                CmpOp::Eq,
                Box::new(Expr::Field("id".into())),
                Box::new(Expr::Lit(Value::Int(id))),
            )),
            require: None,
            pick: None,
        })
        .expect("del");
    }

    /// `g` of `n` documents, each with a vector, every tenth written over:
    /// a graph of tombstones a compact rebuilds -- or, `indexed` false, no
    /// graph, for `create index` to build.
    fn graphed(n: i64, indexed: bool) -> Database {
        let mut db = Database::new();
        let v = Field::new("v", DataType::Vector(2, VecPrec::F32));
        let v = match indexed {
            true => v.indexed(IndexKind::Vector(VectorIndexSpec::default().resolved())),
            false => v,
        };
        let schema = Schema::new("g", vec![Field::new("meta", DataType::Json), v]).expect("schema");
        db.execute(&Statement::CreateCollection {
            schema,
            if_not_exists: false,
        })
        .expect("create");
        for id in 1..=n {
            put(&mut db, id, vec_of(id, 0));
        }
        for id in (1..=n).step_by(10) {
            put(&mut db, id, vec_of(id, 1));
        }
        db
    }

    /// Every document's vector, in id order.
    fn vectors(db: &Database) -> Vec<(DocId, Vec<f32>)> {
        let c = db.collection("g").expect("g");
        c.store
            .ids()
            .into_iter()
            .filter_map(|id| match c.store.read_field(id, 1).expect("read") {
                Some(Value::Vector(v)) => Some((id, v)),
                _ => None,
            })
            .collect()
    }

    fn spec(db: &Database) -> VectorIndexSpec {
        match db.collection("g").expect("g").schema.fields[1].index {
            IndexKind::Vector(spec) => spec,
            ref other => panic!("no graph: {other:?}"),
        }
    }

    /// The graph the read lock built, as every release before this change
    /// built it: the vectors as they stood when the copy was taken linked
    /// as a batch, then each document written meanwhile, in id order, taken
    /// out and put back in -- or, for `create index`, a node holding its
    /// vector already kept (`retire` false).
    fn locked(
        db: &Database,
        before: &[(DocId, Vec<f32>)],
        written: &[DocId],
        retire: bool,
    ) -> VectorIndex {
        let mut ix = VectorIndex::with_precision(2, spec(db), VecPrec::F32);
        ix.insert_batch(before);
        let now = vectors(db);
        let mut written = written.to_vec();
        written.sort_unstable();
        written.dedup();
        for id in written {
            match now.iter().find(|(d, _)| *d == id) {
                Some((_, v)) => {
                    if retire {
                        ix.remove(id);
                    }
                    ix.insert(id, v);
                }
                None => ix.remove(id),
            }
        }
        ix
    }

    /// Runs `stmt` beside the database, a writer getting through where it
    /// holds no lock: a hundred documents written over once the copy's first
    /// vector is read -- past `GRAPH_DONE`, so a second round runs -- and,
    /// once the first round has looked its documents up, one deleted and
    /// one made, each past every id the first round holds. Each write takes
    /// the write lock with `try_write`, which fails while this thread holds
    /// the read lock. The ids written.
    fn written_beside(lock: &Arc<RwLock<Database>>, stmt: &Statement) -> Vec<DocId> {
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (l, w) = (Arc::clone(lock), Arc::clone(&written));
        let mut rounds = 0;
        let seen = SEAM.with(|s| {
            s.borrow_mut().replace(Box::new(move |at: &str| {
                let mut w = w.lock().expect("written");
                let mut g = l.try_write().expect("a writer got through, no lock held");
                match at {
                    "read" if w.is_empty() => {
                        for id in 1..=100 {
                            put(&mut g, id, vec_of(id, 2));
                            w.push(id as DocId);
                        }
                    }
                    "round" if rounds == 0 => {
                        rounds += 1;
                        del(&mut g, 150);
                        put(&mut g, 400, vec_of(400, 0));
                        w.extend([150, 400]);
                    }
                    _ => {}
                }
            }))
        });
        assert!(seen.is_none());
        Database::maintain(lock, stmt)
            .expect("a maintenance")
            .expect("it ran");
        SEAM.with(|s| s.borrow_mut().take());
        let written = written.lock().expect("written").clone();
        assert_eq!(written.len(), 102, "both seams were passed");
        written
    }

    /// A compact reads its graphs' vectors with no lock held, catches up
    /// with the writes made meanwhile in rounds beside the database, and
    /// keeps the graph the read lock built, link for link -- the same
    /// documents a rebuild at its end would hold, each with its vector.
    #[test]
    fn a_compact_reads_its_graphs_with_no_lock_held() {
        let db = graphed(300, true);
        assert_eq!(db.collection("g").expect("g").vectors["v"].dead(), 30);
        let before = vectors(&db);
        let lock = Arc::new(RwLock::new(db));
        let written = written_beside(&lock, &Statement::Compact(None));
        let db = lock.read().expect("lock");
        let ix = &db.collection("g").expect("g").vectors["v"];
        assert_eq!(
            ix.serialize_graph(),
            locked(&db, &before, &written, true).serialize_graph()
        );
        let mut rebuilt = VectorIndex::with_precision(2, spec(&db), VecPrec::F32);
        rebuilt.insert_batch(&vectors(&db));
        assert_eq!(ix.dump(1..=400), rebuilt.dump(1..=400));
        assert_eq!(ix.len(), 300);
        // Each write meanwhile a tombstone, for the next compact.
        assert_eq!(ix.dead(), 101);
        let holds = db.last_holds();
        assert!(holds.swap > Duration::ZERO && holds.longest() >= holds.swap);
    }

    /// `create index` reads its field with no lock held too, and links the
    /// writes made meanwhile into its graph in rounds, keeping the node a
    /// document holds already.
    #[test]
    fn create_index_reads_its_field_with_no_lock_held() {
        let db = graphed(300, false);
        let before = vectors(&db);
        let lock = Arc::new(RwLock::new(db));
        let spec = VectorIndexSpec::default().resolved();
        let written = written_beside(
            &lock,
            &Statement::CreateIndex {
                collection: "g".into(),
                field: "v".into(),
                kind: IndexKind::Vector(spec),
                if_not_exists: false,
            },
        );
        let db = lock.read().expect("lock");
        let ix = &db.collection("g").expect("g").vectors["v"];
        assert_eq!(
            ix.serialize_graph(),
            locked(&db, &before, &written, false).serialize_graph()
        );
        assert_eq!(ix.len(), 300);
        assert!(db.last_holds().put > Duration::ZERO);
    }

    /// With no write beside it, a compact beside the database rebuilds the
    /// graph the one under the write lock does, byte for byte.
    #[test]
    fn a_graph_rebuilt_beside_is_the_one_the_lock_rebuilds() {
        let lock = RwLock::new(graphed(300, true));
        Database::maintain(&lock, &Statement::Compact(None))
            .expect("a maintenance")
            .expect("compact");
        let beside = lock.into_inner().expect("lock");
        let mut under = graphed(300, true);
        under.execute(&Statement::Compact(None)).expect("compact");
        let graph = |db: &Database| db.collection("g").expect("g").vectors["v"].serialize_graph();
        assert_eq!(graph(&beside), graph(&under));
        assert_eq!(beside.collection("g").expect("g").vectors["v"].dead(), 0);
    }

    /// A document a round cannot read leaves the graph as it stood, as one
    /// the copy cannot read does, and the compact goes on.
    #[test]
    fn a_document_a_round_cannot_read_keeps_the_graph() {
        let db = graphed(300, true);
        let stood = {
            let ix = &db.collection("g").expect("g").vectors["v"];
            (ix.len(), ix.dead(), ix.serialize_graph())
        };
        let lock = Arc::new(RwLock::new(db));
        let l = Arc::clone(&lock);
        let seen = SEAM.with(|s| {
            s.borrow_mut().replace(Box::new(move |at: &str| {
                if at != "read" {
                    return;
                }
                // `meta` 100 lists deep, as the test above writes it, and
                // noted as a write is.
                let mut deep = Vec::new();
                for _ in 0..100 {
                    deep.extend_from_slice(&[TAG_LIST, 1]);
                }
                deep.push(TAG_NULL);
                crate::codec::encode_value(&mut deep, &Value::Vector(vec![0.6, 0.8]));
                let mut g = l.try_write().expect("no lock held");
                let c = g.collections.get_mut("g").expect("g");
                c.store.append(OP_PUT, 5, &deep);
                let cid = c.id;
                g.note(cid, 5);
            }))
        });
        assert!(seen.is_none());
        let r = Database::maintain(&lock, &Statement::Compact(None)).expect("a maintenance");
        SEAM.with(|s| s.borrow_mut().take());
        r.expect("compact");
        let db = lock.read().expect("lock");
        let ix = &db.collection("g").expect("g").vectors["v"];
        assert_eq!((ix.len(), ix.dead(), ix.serialize_graph()), stood);
    }
}
