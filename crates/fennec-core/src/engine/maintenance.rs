//! `create index` and `compact` beside the database rather than in its way.
//!
//! Both are builds from the documents: an index from one field of each, a
//! compacted collection from the live ones and every index over them. Run
//! by [`Database::execute`], the whole build holds the write lock, and every
//! reader and writer waits it out -- ~10 s for an HNSW index over 100 000 x
//! 128. [`Database::maintain`] takes under the read lock only what the build
//! reads from -- each store as it stands, taken as a pinned read takes it -- reads
//! the documents and builds with no lock held, catches up with the writes
//! made meanwhile in rounds beside the database, and takes the write lock
//! only for the last few -- a graph's at most [`GRAPH_HELD`] -- and to put
//! the result in place. Read under the read lock, a compact's graphs'
//! vectors held it for seconds past the page cache, and every writer with
//! it -- and, a writer waiting, every reader.
//!
//! Those writes are known exactly, not guessed from the change ring (which
//! a long build would overflow): every write passes through
//! `Database::note`, which hands the id to the [`Tail`] of each maintenance
//! running on that collection. A schema change there -- another index, a
//! drop -- leaves what was built not fitting, and the maintenance says so
//! rather than putting it in place.

use super::*;
use crate::schema::VectorIndexSpec;
use std::sync::RwLock;
use std::time::Duration;

/// How long a maintenance beside the database held the database's lock,
/// each kind of hold at its longest: what a writer waited out at most, and
/// -- `RwLock` making a reader wait behind a waiting writer -- every reader
/// after it. Its build is a compact's graphs or a `create index`'s index;
/// its file a compact's rewrite. One `Instant` a hold, handed back by the
/// maintenance that held them ([`Database::maintain_held`]), never read
/// off the database after it: another may have run between. And the
/// graphs a compact kept as they stood. More kinds may come, so a caller
/// reads the fields and never builds one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Holds {
    /// The read lock the build's stores were taken under, the longest of
    /// a compact's, which takes each graph's on its own.
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
    /// The graphs a compact kept as they stood, tombstones and all, for
    /// the next: what a server's log names beside the holds.
    pub kept: Vec<KeptGraph>,
}

/// A graph a compact kept as it stood rather than put its rebuilt one in
/// place: its writes outran the rounds that link them beside the database
/// -- one core linking about 1 000 a second at 768 dimensions, a sustained
/// ingest of vectors may leave every compact so, its tombstones growing
/// until a quieter one -- a document of it did not read, or its vectors
/// would pass the nodes a graph holds.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeptGraph {
    pub collection: String,
    pub field: String,
    pub why: KeptFor,
}

/// Why a compact kept a graph as it stood ([`KeptGraph`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeptFor {
    /// The documents written during its last round, more than the write
    /// lock had left to link once the graphs before it took theirs
    /// ([`GRAPH_HELD`]).
    Behind(usize),
    /// A document of it did not read, or could not be looked up.
    Unread,
    /// Rebuilt, its live vectors and the writes made meanwhile would pass
    /// [`crate::vector::MAX_NODES`] -- each write counted as a node, which
    /// it may be -- refused before any of them is linked: as a round looks
    /// up a chunk of them, and under the write lock for the last.
    Full,
}

/// ``graph `docs.v` kept, 412 behind``, ``graph `docs.v` kept, a document
/// unread`` or ``graph `docs.v` kept, its writes would pass 4294967295
/// nodes``.
impl std::fmt::Display for KeptGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "graph `{}.{}` kept, ", self.collection, self.field)?;
        match self.why {
            KeptFor::Behind(n) => write!(f, "{n} behind"),
            KeptFor::Unread => f.write_str("a document unread"),
            KeptFor::Full => write!(
                f,
                "its writes would pass {} nodes",
                crate::vector::MAX_NODES
            ),
        }
    }
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
    /// The collection's store as it stood when the copy was taken, as a
    /// pin takes it ([`Taken`]): `values` is read from it with no lock held
    /// ([`Self::read`]), and it is let go of then, its charge with it --
    /// before the build, and before any other graph's store is taken.
    from: Option<Taken>,
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
    /// The store, as a pin takes it, and its schema as they stood when the
    /// copy was taken: `keys` is read from them with no lock held, as an
    /// [`IndexCopy`]'s values are.
    from: Option<(Taken, SchemaCell)>,
}

/// A collection's store as a pinned read takes it ([`Store::pinned`]): the
/// id index copied, 12 bytes a row, the segments and the mapped file
/// shared, and the stretches of the file an image writes from left out --
/// a read by id never asks for them, and a mapped tail of single writes
/// holds 16 bytes of them a write. Its id index is charged to the pins'
/// budget while it is held, as a pin's is ([`Database::pin`]): a long read
/// that would take the budget past it runs under the lock meanwhile. A
/// maintenance never declines for it -- reading under the lock instead is
/// what held a server 5-6 s -- and holds it only while it reads: a
/// compact takes each graph's just before that graph's read. Taken for
/// every graph at once, a collection's was held through the builds of
/// the graphs before its own, minutes of them, and past 5.6 to 11 million
/// rows -- its id index, 12 to 24 bytes a row, held and a pin's copy of it
/// past the 256 MiB budget -- every long read of it was declined
/// meanwhile, and ran under the lock.
struct Taken {
    store: Store,
    #[cfg(not(target_arch = "wasm32"))]
    charged: (Arc<std::sync::atomic::AtomicUsize>, usize),
}

impl std::ops::Deref for Taken {
    type Target = Store;

    fn deref(&self) -> &Store {
        &self.store
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Taken {
    fn drop(&mut self) {
        let (pins, bytes) = &self.charged;
        pins.fetch_sub(*bytes, std::sync::atomic::Ordering::Relaxed);
    }
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
    /// A graph whose vectors pass the bound as the graph places them
    /// (`VectorIndex::insert_held`), refused once the build is done: the
    /// copy holds every vector, and only placing them counts their nodes.
    #[cfg_attr(not(target_pointer_width = "64"), allow(dead_code))]
    Refused(Error),
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
    /// longest: what a server's log says of a compact or a `create index`,
    /// whoever ran it. Handed back rather than kept on the database, where
    /// a second maintenance finishing first would have its holds read for
    /// this one's.
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

    /// `store` as a pinned read takes it, charged to the pins' budget until
    /// it is let go of ([`Taken`]).
    fn taken(&self, store: &Store) -> Taken {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let bytes = store.pinned_bytes();
            self.pins
                .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
            Taken {
                store: store.pinned(),
                charged: (Arc::clone(&self.pins), bytes),
            }
        }
        // The browser module has no pins, and maintains nothing beside a
        // page (fennec-wasm calls no `maintain`).
        #[cfg(target_arch = "wasm32")]
        Taken {
            store: store.clone(),
        }
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
            from: Some(self.taken(&c.store)),
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
            Built::Refused(e) => return Err(e),
            Built::Vector(mut ix) => {
                // Each write may place a node: past the bound, refused here,
                // where nothing has changed yet -- placed, the last would
                // meet `alloc_links`' assert under the write lock. Asked
                // first: run again, it would be refused as surely.
                if past_nodes(&ix, ids.len()) {
                    return Err(crate::vector::graph_full(&copy.field));
                }
                // Each a link under the write lock: past the bound the
                // rounds fell behind the vectors written, and the index is
                // refused rather than every writer held while it catches up.
                let spec = copy.spec();
                if ids.len() > held_links(copy.dim(), &spec) {
                    let left = ids.len();
                    return Err(fell_behind(
                        &copy.collection,
                        &copy.field,
                        left,
                        copy.dim(),
                        &spec,
                    ));
                }
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
            from: Some((self.taken(&c.store), c.schema.clone())),
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
    /// holding tombstones, each by its collection and field, for
    /// [`Self::begin_graph`] to take one at a time. Nothing else is: the
    /// records stay in the file until the rewrite streams the live ones
    /// into the new one.
    #[cfg(not(target_arch = "wasm32"))]
    fn graphs_rebuilt(&self, which: Option<&str>) -> Result<Vec<(String, u32, String)>> {
        self.may_write(true)?;
        let targets: Vec<&str> = match which {
            Some(n) => {
                self.collection(n)?;
                vec![n]
            }
            None => self.order.iter().map(String::as_str).collect(),
        };
        let mut due = Vec::new();
        for name in targets {
            let c = &self.collections[name];
            for (field, ix) in &c.vectors {
                if ix.dead() > 0 {
                    due.push((name.to_string(), c.id, field.clone()));
                }
            }
        }
        Ok(due)
    }

    /// One of the graphs [`Self::graphs_rebuilt`] named, with its collection's
    /// store as it stands, to read its vectors from once the lock is let go
    /// ([`IndexCopy::read`]) as `create index` reads them, and its tail's
    /// token: taken just before that read, the store's charge to the pins'
    /// budget held only through it ([`Taken`]). The vectors read under this
    /// lock held it -- and every writer, and every reader behind a waiting
    /// writer -- 5 to 6 s in the P6 soak's dry run 5, a page fault each past
    /// the page cache (20 000 documents of a 2 GB file mapped in a
    /// container of 512 MB). `None` where the collection was dropped or
    /// made again, or the field's graph is gone, since it was named.
    #[cfg(not(target_arch = "wasm32"))]
    fn begin_graph(&self, name: &str, cid: u32, field: &str) -> Option<IndexCopy> {
        let c = self.collections.get(name).filter(|c| c.id == cid)?;
        c.vectors.get(field)?;
        let pos = c.schema.field_pos(field)?;
        Some(IndexCopy {
            token: self.watch(c.id),
            cid: c.id,
            collection: name.to_string(),
            field: field.to_string(),
            pos,
            kind: c.schema.fields[pos].index.clone(),
            ty: c.schema.fields[pos].ty.clone(),
            order: TextOrder::BYTES,
            values: Vec::new(),
            from: Some(self.taken(&c.store)),
            place: c.schema.place(pos),
        })
    }

    /// Puts the rebuilt graphs in place, each caught up with the writes
    /// the rounds left ([`catch_up_graphs`]), and names those it kept as
    /// they stood. The rewrite follows ([`rewrite_beside`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn finish_graphs(&mut self, built: Vec<BuiltIndex>) -> Result<Vec<KeptGraph>> {
        let tails: Vec<Option<Tail>> = built.iter().map(|b| self.unwatch(b.copy.token)).collect();
        self.refuse_if_failed()?;
        // Every collection asked before any graph is placed: a refusal
        // places none. Asked as each was placed, a compact of two whose
        // second was altered meanwhile put the first's graph in place and
        // then refused -- with the ids a round drained for it lost, so it
        // held a document's old vector and the graph keeper would have
        // saved it so.
        for (b, t) in built.iter().zip(&tails) {
            let moved = t.as_ref().is_none_or(|t| t.schema);
            if moved || self.named(b.copy.cid).as_deref() != Some(&b.copy.collection) {
                return Err(changed_while_compacted(&b.copy.collection));
            }
        }
        // What the write lock's links may cost, the graphs' together.
        let mut room = held_cost();
        let mut kept = Vec::new();
        for (b, t) in built.into_iter().zip(tails) {
            let keep = |why| KeptGraph {
                collection: b.copy.collection.clone(),
                field: b.copy.field.clone(),
                why,
            };
            // A document it cannot read leaves the graph as it stands,
            // tombstones and all, and answering as it did: one built
            // without it would not, and the compact is never refused for
            // what a document holds.
            if b.broken.is_some() {
                kept.push(keep(KeptFor::Unread));
                continue;
            }
            let Some(Tail { mut ids, .. }) = t else {
                unreachable!("every tail was asked for above")
            };
            ids.sort_unstable();
            ids.dedup();
            let mut ix = match b.index {
                Built::Vector(ix) => ix,
                // Its live vectors past the bound, or a round's writes with
                // them (`graph_round`): the graph they are in stays, as one
                // a document it cannot read leaves.
                Built::Refused(_) => {
                    kept.push(keep(KeptFor::Full));
                    continue;
                }
                _ => unreachable!("only graphs are rebuilt beside a mapped compact"),
            };
            // Each write made meanwhile places a node at most, one its own
            // write may not have -- a row whose other field it wrote is
            // taken out and put in again: past the bound, the graph stays
            // as it stood, as where a document does not read, rather than
            // meet `alloc_links`' assert under the write lock. Asked before
            // the links' cost, which it spends none of.
            if past_nodes(&ix, ids.len()) {
                kept.push(keep(KeptFor::Full));
                continue;
            }
            // The rounds fell behind the vectors written: the graph stays as
            // it stands, as where a document does not read, for the next
            // compact, rather than every writer held while it catches up.
            let cost = links_cost(ids.len(), b.copy.link_cost());
            if cost > room {
                kept.push(keep(KeptFor::Behind(ids.len())));
                continue;
            }
            room -= cost;
            let c = self.collections.get_mut(&b.copy.collection).unwrap();
            let caught: Result<()> = ids.into_iter().try_for_each(|id| {
                ix.remove(id);
                if let Some(Value::Vector(v)) = c.store.read_field(id, b.copy.pos)? {
                    ix.insert(id, &v);
                }
                Ok(())
            });
            // As where the copy was taken: a document it cannot read leaves
            // the graph as it stands.
            match caught {
                Ok(()) => {
                    c.vectors.insert(b.copy.field, ix);
                }
                Err(_) => kept.push(KeptGraph {
                    collection: b.copy.collection,
                    field: b.copy.field,
                    why: KeptFor::Unread,
                }),
            }
        }
        // The writes made meanwhile may have left the new graphs a tombstone
        // or two; those wait for the next compact rather than having the
        // graph rebuilt again under the lock.
        Ok(kept)
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
///
/// What bounds the last is the rounds' pace against the writers', and
/// nothing else. A round copies a write with no lock held for less than its
/// writer paid under the write lock: appended, put in the id index and
/// noted there, and a `put` of a few documents built and encoded there too.
/// A server builds and encodes a block's `put`s of [`PREPARE_AT`] documents
/// or more before the lock -- a `POST /<name>`'s, `/query`'s, `/batch`'s
/// ([`Database::prepare`], fennec-http's `early_put`) -- and still, beside
/// 1 000-row bulk puts into a collection with no index, each round copied a
/// third of the one before or less. The write lock copies what was written
/// during the last round and during the handover after it
/// ([`Beside::hand_over`]) of every row the rounds copied: at 30 000 rows a
/// second that took 48 ms and left 2 000-4 000 rows to a swap of 10-29 ms,
/// but beside a writer as fast as three CPUs went, 43 000-50 000 rows a
/// second, it took 9-11 s, and the swap copied the 550 000-620 000 written
/// meanwhile, every writer held 3.8-6.4 s (docs/design/storage.md). A
/// bound needs a design of its own: a compact cannot keep the old file as
/// it keeps a graph, and one refused past a bound would be refused at every
/// try beside a steady ingest; handing the copies over round by round,
/// which would bound the memory they hold till then as well, is the
/// likelier way. A graph's rounds have no margin at all -- a link costs as
/// much as the one the write that made it paid, and a block's puts link on
/// every core -- so the write lock links what costs [`GRAPH_HELD`] at
/// most. A document the store cannot look up fails the compact: the new
/// file would be without it.
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
fn catch_up(db: &RwLock<Database>, b: &mut Beside, longest: &mut Duration) -> Result<usize> {
    let tokens: Vec<u64> = b.parts.iter().map(|p| p.token).collect();
    let drained = drain(db, &tokens, longest)?;
    // A collection changed: the finish refuses the rewrite, and copies
    // nothing more.
    if drained.iter().any(Option::is_none) {
        return Ok(0);
    }
    let mut copied = 0;
    let mut docs = crate::store::Snapshot::default();
    for (p, mut ids) in b.parts.iter_mut().zip(drained.into_iter().flatten()) {
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
/// under one hold of the read lock, every tail's: `None` where a
/// collection's schema changed or it is gone, which the finish refuses.
#[cfg(not(target_arch = "wasm32"))]
fn drain(
    db: &RwLock<Database>,
    tokens: &[u64],
    longest: &mut Duration,
) -> Result<Vec<Option<Vec<DocId>>>> {
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
    let r = match g.collections.get(name).filter(|c| c.id == cid) {
        Some(live) => live.store.snapshot(ids, docs).map(|()| true),
        None => Ok(false),
    };
    *longest = (*longest).max(began.held());
    r
}

/// Catches the graphs built beside the database up with the writes made
/// while they were, in rounds: each round's ids drained from the tails at
/// once, their documents looked up under the read lock a few thousand at a
/// time and read after it, as the rewrite's rounds copy theirs
/// ([`catch_up`]), and each vector linked into its graph with no lock held
/// -- under the write lock, a link at 768 dimensions takes about a
/// millisecond ([`GRAPH_HELD`]), and a build of seconds meets thousands of
/// writes. A round whose links cost less than [`GRAPH_DONE`] at
/// [`HELD_DIM`] dimensions ends them, and the write lock takes the few
/// written during it, costing [`GRAPH_HELD`] at most. `retire`: a write is
/// taken out of the graph before it goes in again, as a compact's rebuilt
/// graph always took the writes made meanwhile (`finish_graphs`); else a
/// node holding the vector already is kept, as `create index` keeps it
/// (`finish_index`).
#[cfg(not(target_arch = "wasm32"))]
fn catch_up_graphs(
    db: &RwLock<Database>,
    built: &mut [BuiltIndex],
    retire: bool,
    longest: &mut Duration,
) -> Result<()> {
    for _ in 0..CATCH_UP_ROUNDS {
        let done = GRAPH_DONE * link_cost(HELD_DIM, &VectorIndexSpec::default());
        if graph_round(db, built, retire, longest)? < done {
            break;
        }
    }
    Ok(())
}

/// A round of [`catch_up_graphs`]: what the links it made cost
/// ([`link_cost`]), 0 once a collection changed, which the finish refuses
/// whole. A chunk that cannot
/// be looked up or read leaves its graph as it stood (`broken`), as one the
/// copy cannot read does: a compact keeps it, and `create index` is
/// refused. A chunk that may take its graph past the nodes a graph holds
/// ([`past_nodes`]) is linked into it by none: the graph is refused
/// (`Built::Refused`) -- a compact keeps it, and says so, and `create
/// index` is refused naming the field -- where linked, the last node met
/// `alloc_links`' assert with no lock held, the maintenance a panic.
#[cfg(not(target_arch = "wasm32"))]
fn graph_round(
    db: &RwLock<Database>,
    built: &mut [BuiltIndex],
    retire: bool,
    longest: &mut Duration,
) -> Result<u64> {
    let tokens: Vec<u64> = built.iter().map(|b| b.copy.token).collect();
    let drained = drain(db, &tokens, longest)?;
    if drained.iter().any(Option::is_none) {
        return Ok(0);
    }
    let mut caught: u64 = 0;
    let mut docs = crate::store::Snapshot::default();
    for (b, mut ids) in built.iter_mut().zip(drained.into_iter().flatten()) {
        // A graph left as it stood takes nothing more.
        let (Built::Vector(ix), None) = (&mut b.index, &b.broken) else {
            continue;
        };
        ids.sort_unstable();
        ids.dedup();
        caught = caught.saturating_add(links_cost(ids.len(), b.copy.link_cost()));
        let mut full = false;
        for chunk in ids.chunks(CATCH_UP_CHUNK) {
            if past_nodes(ix, chunk.len()) {
                full = true;
                break;
            }
            let linked = match looked_up(
                db,
                &b.copy.collection,
                b.copy.cid,
                chunk,
                &mut docs,
                longest,
            ) {
                Ok(true) => link_looked_up(ix, &docs, b.copy.place, retire),
                Ok(false) => return Ok(0),
                Err(e) => Err(e),
            };
            if let Err(e) = linked {
                b.broken = Some(e);
                break;
            }
        }
        if full {
            b.index = Built::Refused(crate::vector::graph_full(&b.copy.field));
        }
    }
    Ok(caught)
}

/// Whether `writes` written to the rows of `ix` may take it past
/// [`crate::vector::MAX_NODES`], each counted as the node it may place:
/// one a compact's graph takes out and puts in again, `create index`'s
/// one its row's node does not hold. Asked before any of them is linked,
/// by a round of a chunk and by the write lock of the last round's writes.
#[cfg(target_pointer_width = "64")]
#[inline]
fn past_nodes(ix: &VectorIndex, writes: usize) -> bool {
    ix.nodes() + writes > crate::vector::MAX_NODES
}

/// [`past_nodes`] where no graph can pass the bound: a 32-bit build's
/// memory stops far short of it.
#[cfg(not(target_pointer_width = "64"))]
#[inline(always)]
fn past_nodes(_: &VectorIndex, _: usize) -> bool {
    false
}

/// Links the documents a round looked up into `ix`, with no lock held: each
/// vector at `place` in its payload, and each document without one taken
/// out. `retire` as [`catch_up_graphs`] says.
#[cfg(not(target_arch = "wasm32"))]
fn link_looked_up(
    ix: &mut VectorIndex,
    docs: &crate::store::Snapshot,
    place: usize,
    retire: bool,
) -> Result<()> {
    #[cfg(test)]
    seam("round");
    docs.each(|id, payload| {
        match payload
            .map(|p| crate::store::field_at(p, place))
            .transpose()?
        {
            Some(Value::Vector(v)) => {
                if retire {
                    ix.remove(id);
                }
                ix.insert(id, &v);
            }
            _ => ix.remove(id),
        }
        Ok(())
    })
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
    /// The dimensions of the vectors a graph's copy holds.
    fn dim(&self) -> usize {
        match self.ty {
            DataType::Vector(dim, _) => dim,
            _ => 0,
        }
    }

    /// The settings of the graph built from this copy: its field's.
    fn spec(&self) -> VectorIndexSpec {
        match &self.kind {
            IndexKind::Vector(spec) => *spec,
            _ => VectorIndexSpec::default(),
        }
    }

    /// What a link into the graph built from this copy costs the lock
    /// ([`link_cost`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn link_cost(&self) -> u64 {
        link_cost(self.dim(), &self.spec())
    }

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
                match ix.insert_held(&items, false, &self.field) {
                    Ok(()) => Built::Vector(ix),
                    Err(e) => Built::Refused(e),
                }
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
        let due = {
            let g = read(db);
            let began = Began::now();
            let due = g.graphs_rebuilt(which);
            holds.copy = began.held();
            due?
        };
        let mut watching = Watching {
            db,
            tokens: Vec::with_capacity(due.len()),
        };
        // Each graph's store taken just before its vectors are read, and
        // let go once they are: its charge to the pins' budget held
        // through that read alone, never through another graph's build.
        let mut built = Vec::with_capacity(due.len());
        for (name, cid, field) in &due {
            let copy = {
                let g = read(db);
                let began = Began::now();
                let copy = g.begin_graph(name, *cid, field);
                holds.copy = holds.copy.max(began.held());
                copy
            };
            let copy = copy.ok_or_else(|| changed_while_compacted(name))?;
            watching.tokens.push(copy.token);
            if built.is_empty() {
                during();
            }
            built.push(copy.rebuilt());
        }
        if due.is_empty() {
            during();
        }
        catch_up_graphs(db, &mut built, true, &mut holds.rounds)?;
        let r = {
            let mut g = write(db);
            let began = Began::now();
            let r = g.finish_graphs(built);
            holds.put = began.held();
            r
        };
        watching.tokens.clear();
        holds.kept = r?;
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
/// A graph's round whose links cost less than this many at [`HELD_DIM`]
/// dimensions ends its rounds ([`catch_up_graphs`]): each is a link into
/// the graph under the write lock that follows, where a rewrite's is a
/// copy of the record.
#[cfg(not(target_arch = "wasm32"))]
const GRAPH_DONE: u64 = 64;
/// What the write lock's links cost at most once a graph's rounds end, its
/// graphs' together -- what was written during the last round -- counted
/// as this many links into graphs of [`HELD_DIM`] dimensions at the default
/// `m` and `ef_construction`: a link costs about its dimensions and 64
/// more, times its beam and its neighbours past the defaults
/// ([`link_cost`]), so the write lock links 128 at 768 dimensions, 66 at
/// 1 536, 33 at 3 072 and 1 331 at 16, and at 768 dimensions 64 at
/// `ef_construction` 400, 32 at `m` 32 with it and 10 at `m` 48 with 800
/// ([`held_links`]). A single link took at the median, into a graph of
/// 20 000 vectors on one core of an M-series Mac, 0.08 ms at 16
/// dimensions, 0.15 at 128, 0.38 at 384, 0.82-0.89 at 768, 1.80 at 1 536
/// and 3.7 at 3 072, and 1.14 ms into one of 100 000 x 768: the bound
/// holds every writer 85-125 ms into 20 000 vectors at any of those
/// dimensions and about a third more into 100 000, under the 0.19-0.31 s
/// the rewrite's copy holds the soak's file -- and 75-117 ms at the `m`
/// and `ef_construction` timed past the defaults. Counted in links alone,
/// 128 held it 230 ms at 1 536 dimensions and 470 at 3 072, and 10 at 16;
/// at dimensions alone, 1.0 s at `m` 48 and `ef_construction` 800.
/// Twice [`GRAPH_DONE`]: a round ends costing less than 64, and where the
/// links keep up with the vectors written the round after it costs less
/// again, so only a burst or writes that outran 16 rounds pass it. Past it
/// a compact keeps the graph as it stood, for the next, and says so
/// ([`KeptGraph`]), and `create index` is refused ([`fell_behind`]); with no
/// bound the last hold linked whatever the 16th round left, without end
/// where the writes outran the links. A graph whose one link costs more
/// than the whole bound -- at 768 dimensions `m` times `ef_construction`
/// past 409 600, each at its default at least: `m` 16 with an
/// `ef_construction` past 25 600 -- links none under the lock, and any
/// vector written during its last round keeps it or refuses its
/// `create index`.
const GRAPH_HELD: u64 = 128;
/// The dimensions [`GRAPH_HELD`] counts its links at.
const HELD_DIM: usize = 768;

/// What a link into a graph of `dim` dimensions built by `spec` costs the
/// lock, in dimensions at the default `m` and `ef_construction`: each
/// distance it measures reads them all, and the walk itself costs about
/// 64 more ([`GRAPH_HELD`]), each layer searched with a beam of
/// `ef_construction` whose every node's neighbours, up to `m` of them, are
/// measured (`VectorIndex::link_node`). The links timed at 16 to 3 072
/// dimensions fit the dimensions within a fifth: over at 128 and 384, under
/// at 1 536 and 3 072, where each dimension costs a little more. Into
/// 20 000 x 768 a link took 0.91 ms at the defaults, 1.47 at
/// `ef_construction` 400, 2.34 at 800, 3.65 at `m` 32 with 400 and 7.87 at
/// `m` 48 with 800: 1.6, 2.6, 4.0 and 8.6 times the default's, which the
/// product of the two ratios puts at 2, 4, 4 and 12 -- a hair under at
/// `m` 32 (4.0 against 4.01), over the rest by 24-56%. Below the defaults
/// a link is counted at theirs, never less, as nothing was timed there.
/// In `u64`, saturating: on a 32-bit target the products of a large
/// graph's settings, or of a round's millions of ids ([`links_cost`]),
/// wrapped `usize`.
fn link_cost(dim: usize, spec: &VectorIndexSpec) -> u64 {
    let base = VectorIndexSpec::default();
    let beam = spec.ef_construction.max(base.ef_construction) as u64;
    let m = spec.m.max(base.m) as u64;
    (dim as u64)
        .saturating_add(64)
        .saturating_mul(beam)
        .saturating_mul(m)
        .div_ceil((base.ef_construction * base.m) as u64)
}

/// What `links` links of `each` cost, saturating rather than wrapping a
/// 32-bit `usize` (5.2 million at 768 dimensions wrapped it).
#[cfg(not(target_arch = "wasm32"))]
fn links_cost(links: usize, each: u64) -> u64 {
    (links as u64).saturating_mul(each)
}

/// What the write lock's links cost at most, a compact's graphs' together
/// ([`GRAPH_HELD`]).
fn held_cost() -> u64 {
    GRAPH_HELD * link_cost(HELD_DIM, &VectorIndexSpec::default())
}

/// The links into a graph of `dim` dimensions built by `spec` that cost
/// the write lock what [`GRAPH_HELD`] links of [`HELD_DIM`] do at the
/// defaults: 0 where one costs more.
fn held_links(dim: usize, spec: &VectorIndexSpec) -> usize {
    usize::try_from(held_cost() / link_cost(dim, spec)).unwrap_or(usize::MAX)
}

/// `create index`'s refusal where the writes left more for the write lock
/// to link than it links into a graph of `dim` dimensions built by `spec`
/// ([`held_links`]).
fn fell_behind(
    collection: &str,
    field: &str,
    left: usize,
    dim: usize,
    spec: &VectorIndexSpec,
) -> Error {
    Error::Query(format!(
        "the graph of `{collection}.{field}` fell behind its writes: {left} documents were \
         written during its last round of catching up, and the write lock links {} at most \
         at {dim} dimensions, m {} and ef_construction {}; run `create index` again when \
         fewer vectors are being written",
        held_links(dim, spec),
        spec.m,
        spec.ef_construction
    ))
}

/// A compact's refusal where `collection` was dropped, altered or made
/// again while its graphs were built.
#[cfg(not(target_arch = "wasm32"))]
fn changed_while_compacted(collection: &str) -> Error {
    Error::Query(format!(
        "`{collection}` changed while it was compacted; run `compact` again"
    ))
}
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
    r
}

#[cfg(all(test, feature = "vector"))]
mod tests {
    use super::*;
    use crate::codec::{TAG_LIST, TAG_NULL};
    use crate::schema::{Field, VectorIndexSpec};
    use crate::value::VecPrec;

    fn put(db: &mut Database, id: i64, v: [f32; 2]) {
        put_to(db, "g", id, v)
    }

    fn put_to(db: &mut Database, collection: &str, id: i64, v: [f32; 2]) {
        db.execute(&Statement::Put {
            collection: collection.into(),
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

    /// limits.html's "vector index node count" where `create index` makes
    /// a graph: under the write lock (`build_graph`), beside the database as
    /// it is built (`IndexCopy::build`), and as it catches up with the
    /// writes made during the build -- a round's chunk (`graph_round`) and
    /// the last round's writes under the write lock (`finish_index`), where
    /// a placed node past the bound met `alloc_links`' assert. Each
    /// refuses a graph past `MAX_NODES` naming the field, the collection
    /// left unindexed, and takes one at it -- its nodes counted as the
    /// graph places them, so four rows sharing three vectors are taken
    /// where three nodes fit, which counted by rows were refused. Counted
    /// past the graph's own nodes ([`crate::vector::NODES_BESIDE`]): no
    /// test makes 2^32 vectors.
    #[test]
    fn create_index_refuses_a_graph_past_its_nodes() {
        use crate::vector::{MAX_NODES, NODES_BESIDE};
        struct Beside;
        impl Beside {
            fn counted(n: usize) -> Beside {
                NODES_BESIDE.with(|b| b.set(n));
                Beside
            }
        }
        impl Drop for Beside {
            fn drop(&mut self) {
                NODES_BESIDE.with(|b| b.set(0));
            }
        }
        let made = || {
            let mut db = Database::new();
            let schema = Schema::new(
                "g",
                vec![
                    Field::new("meta", DataType::Json),
                    Field::new("v", DataType::Vector(2, VecPrec::F32)),
                ],
            )
            .expect("schema");
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .expect("create");
            for id in 1..=3 {
                put(&mut db, id, [id as f32, 1.0]);
            }
            RwLock::new(db)
        };
        let index = Statement::CreateIndex {
            collection: "g".into(),
            field: "v".into(),
            kind: IndexKind::Vector(VectorIndexSpec::default().resolved()),
            if_not_exists: false,
        };
        let says = "field `v`: a vector index holds at most 4294967295 nodes";
        let refused = |r: Result<Response>, db: &RwLock<Database>, tag: &str| {
            assert!(
                matches!(&r, Err(Error::Query(m)) if m.starts_with(says)),
                "{tag}: {r:?}"
            );
            let db = db.read().unwrap();
            let g = &db.collections["g"];
            assert!(g.vectors.is_empty(), "{tag}: a graph");
            assert!(matches!(g.schema.fields[1].index, IndexKind::None), "{tag}");
        };
        // Three vectors: one node past the bound, then at it -- and so with
        // a fourth row holding the first's vector, which places no node.
        for (online, shared) in [(false, false), (true, false), (false, true), (true, true)] {
            let tag = format!(
                "{}{}",
                if online { "beside" } else { "under the lock" },
                if shared { ", shared" } else { "" }
            );
            let run = |db: &RwLock<Database>| match online {
                true => Database::maintain(db, &index).expect("maintained"),
                false => db.write().unwrap().execute(&index),
            };
            let made = || {
                let db = made();
                if shared {
                    put(&mut db.write().unwrap(), 4, [1.0, 1.0]);
                }
                db
            };
            let db = made();
            let r = {
                let _b = Beside::counted(MAX_NODES - 2);
                run(&db)
            };
            refused(r, &db, &tag);
            let _b = Beside::counted(MAX_NODES - 3);
            run(&db).expect("at the bound");
            let nodes = db.read().unwrap().collections["g"]
                .vectors
                .get("v")
                .map(|ix| ix.nodes());
            assert_eq!(nodes, Some(MAX_NODES), "{tag}");
        }
        // A write made during the build would place the node past it: met
        // by the round that catches up with it, before it links any, or,
        // written during the last round, by the write lock -- one past the
        // bound refused, and one at it taken.
        for (last, short) in [(false, 3), (false, 4), (true, 4), (true, 5)] {
            let tag = match last {
                false => "a round",
                true => "the last round's writes",
            };
            let lock = Arc::new(made());
            let rounds = std::rc::Rc::new(std::cell::Cell::new(0));
            let met = std::rc::Rc::clone(&rounds);
            let (r, _) = {
                let _b = Beside::counted(MAX_NODES - short);
                beside(&lock, &index, move |point, n, l| match (point, n) {
                    ("read", 0) => put(&mut writer(l), 4, [4.0, 1.0]),
                    ("round", 0) => {
                        met.set(met.get() + 1);
                        if last {
                            put(&mut writer(l), 5, [5.0, 1.0]);
                        }
                    }
                    _ => {}
                })
            };
            let past = short == if last { 4 } else { 3 };
            // A round refusing its chunk links none of it.
            assert_eq!(rounds.get(), usize::from(last || !past), "{tag}");
            if past {
                refused(r, &lock, tag);
                continue;
            }
            r.expect(tag);
            // Its own nodes, the rows' -- the count beside it let go of.
            let rows = if last { 5 } else { 4 };
            let nodes = lock.read().unwrap().collections["g"]
                .vectors
                .get("v")
                .map(|ix| (ix.len(), ix.nodes()));
            assert_eq!(nodes, Some((rows, rows)), "{tag}");
        }
    }

    /// A compact beside the database at a graph's bound: the graph built
    /// again without its tombstones catches up with the writes made during
    /// the build, each taken out and put in again -- a node each, though a
    /// write of another field placed none in the graph it wrote. Past the
    /// bound the graph stays as it stood, tombstone and all, as where a
    /// document does not read, and the compact says so (`KeptFor::Full`),
    /// where the last met `alloc_links`' assert: refused by the round
    /// catching up with the writes before it links any, or by the write
    /// lock for those written during the last round. At the bound the
    /// rebuilt graph is put in place.
    #[test]
    fn a_compact_whose_catch_up_would_pass_the_bound_keeps_its_graph() {
        use crate::vector::{MAX_NODES, NODES_BESIDE};
        let graph = |db: &Database| {
            let ix = &db.collection("g").expect("g").vectors["v"];
            (ix.len(), ix.dead(), ix.nodes())
        };
        for (last, short) in [(false, 3), (false, 4), (true, 3), (true, 4)] {
            let tag = match last {
                false => "a round",
                true => "the last round's writes",
            };
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
            put(&mut db, 1, [1.0, 0.2]);
            assert_eq!(graph(&db), (2, 1, 3));
            let lock = Arc::new(RwLock::new(db));
            let rounds = std::rc::Rc::new(std::cell::Cell::new(0));
            let met = std::rc::Rc::clone(&rounds);
            NODES_BESIDE.with(|b| b.set(MAX_NODES - short));
            // Each row's own vector again: no node in the graph written. Both
            // before the first round, or the second during it.
            let writes = move |point: &str, n: usize, l: &RwLock<Database>| match (point, n) {
                ("read", 0) => {
                    put(&mut writer(l), 1, [1.0, 0.2]);
                    if !last {
                        put(&mut writer(l), 2, [0.0, 1.0]);
                    }
                }
                ("round", 0) => {
                    met.set(met.get() + 1);
                    if last {
                        put(&mut writer(l), 2, [0.0, 1.0]);
                    }
                }
                _ => {}
            };
            let (r, holds) = beside(&lock, &Statement::Compact(None), writes);
            let stood = graph(&lock.read().unwrap());
            NODES_BESIDE.with(|b| b.set(0));
            r.expect(tag);
            let past = short == 3;
            // A round refusing its chunk links none of it.
            assert_eq!(rounds.get(), usize::from(last || !past), "{tag}");
            match past {
                true => {
                    assert_eq!(stood, (2, 1, MAX_NODES), "{tag}: the graph as it stood");
                    let kept = KeptGraph {
                        collection: "g".into(),
                        field: "v".into(),
                        why: KeptFor::Full,
                    };
                    assert_eq!(
                        kept.to_string(),
                        "graph `g.v` kept, its writes would pass 4294967295 nodes"
                    );
                    assert_eq!(holds.kept, [kept], "{tag}");
                }
                // Rebuilt without its tombstone, a tombstone each write.
                false => {
                    assert_eq!(stood, (2, 2, MAX_NODES), "{tag}: rebuilt");
                    assert!(holds.kept.is_empty(), "{tag}");
                }
            }
        }
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
        graphed_by(n, indexed, VectorIndexSpec::default())
    }

    /// [`graphed`], its graph built by `spec`.
    fn graphed_by(n: i64, indexed: bool, spec: VectorIndexSpec) -> Database {
        let mut db = Database::new();
        made_by(&mut db, "g", indexed, spec);
        filled(&mut db, "g", n);
        written_over(&mut db, "g", n);
        db
    }

    /// The collection `name`: `meta`, a vector `v` with its graph where
    /// `indexed` says, and a `tag` a second `create index` takes.
    fn made(db: &mut Database, name: &str, indexed: bool) {
        made_by(db, name, indexed, VectorIndexSpec::default())
    }

    /// [`made`], its graph built by `spec`.
    fn made_by(db: &mut Database, name: &str, indexed: bool, spec: VectorIndexSpec) {
        let v = Field::new("v", DataType::Vector(2, VecPrec::F32));
        let v = match indexed {
            true => v.indexed(IndexKind::Vector(spec.resolved())),
            false => v,
        };
        let fields = vec![
            Field::new("meta", DataType::Json),
            v,
            Field::new("tag", DataType::Text),
        ];
        db.execute(&Statement::CreateCollection {
            schema: Schema::new(name, fields).expect("schema"),
            if_not_exists: false,
        })
        .expect("create");
    }

    /// `n` documents in `name`, each with its vector.
    fn filled(db: &mut Database, name: &str, n: i64) {
        for id in 1..=n {
            put_to(db, name, id, vec_of(id, 0));
        }
    }

    /// Every tenth of `name`'s `n` documents written over: a tombstone each.
    fn written_over(db: &mut Database, name: &str, n: i64) {
        for id in (1..=n).step_by(10) {
            put_to(db, name, id, vec_of(id, 1));
        }
    }

    /// Every document's vector, in id order.
    fn vectors(db: &Database) -> Vec<(DocId, Vec<f32>)> {
        vectors_of(db, "g")
    }

    fn vectors_of(db: &Database, name: &str) -> Vec<(DocId, Vec<f32>)> {
        let c = db.collection(name).expect("the collection");
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

    /// Runs `stmt` beside the database, `at` called where it holds no lock
    /// -- `read` once a copy's first value is read, `round` once a round's
    /// documents are looked up -- with the point, how many times it was met
    /// before, and the database. A write there takes the lock with
    /// [`writer`], which fails while this thread holds the read lock. Its
    /// answer, and how long it held the lock.
    fn beside(
        lock: &Arc<RwLock<Database>>,
        stmt: &Statement,
        mut at: impl FnMut(&str, usize, &RwLock<Database>) + 'static,
    ) -> (Result<Response>, Holds) {
        let l = Arc::clone(lock);
        let (mut reads, mut rounds) = (0, 0);
        let seen = SEAM.with(|s| {
            s.borrow_mut().replace(Box::new(move |point: &str| {
                let met = match point {
                    "read" => &mut reads,
                    _ => &mut rounds,
                };
                *met += 1;
                at(point, *met - 1, &l);
            }))
        });
        assert!(seen.is_none());
        let r = Database::maintain_held(lock, stmt).expect("a maintenance");
        SEAM.with(|s| s.borrow_mut().take());
        r
    }

    /// The write lock, taken where a maintenance holds none: `try_write`
    /// fails while this thread holds the read lock.
    fn writer(l: &RwLock<Database>) -> std::sync::RwLockWriteGuard<'_, Database> {
        l.try_write().expect("a writer got through, no lock held")
    }

    /// Runs `stmt` beside the database, a writer getting through where it
    /// holds no lock: a hundred documents written over once the copy's first
    /// vector is read -- past `GRAPH_DONE`, so a second round runs -- and,
    /// once the first round has looked its documents up, one deleted and
    /// one made, each past every id the first round holds. The ids written,
    /// and how long it held the lock.
    fn written_beside(lock: &Arc<RwLock<Database>>, stmt: &Statement) -> (Vec<DocId>, Holds) {
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = Arc::clone(&written);
        let (r, holds) = beside(lock, stmt, move |point, met, l| {
            let mut w = w.lock().expect("written");
            match (point, met) {
                ("read", 0) => {
                    let mut g = writer(l);
                    for id in 1..=100 {
                        put(&mut g, id, vec_of(id, 2));
                        w.push(id as DocId);
                    }
                }
                ("round", 0) => {
                    let mut g = writer(l);
                    del(&mut g, 150);
                    put(&mut g, 400, vec_of(400, 0));
                    w.extend([150, 400]);
                }
                _ => {}
            }
        });
        r.expect("it ran");
        let written = written.lock().expect("written").clone();
        assert_eq!(written.len(), 102, "both seams were passed");
        (written, holds)
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
        compacted_beside(db, before);
    }

    /// [`a_compact_reads_its_graphs_with_no_lock_held`] of `db`: the graph
    /// it keeps is the one the read lock built, and every live document's
    /// vector is in it.
    fn compacted_beside(db: Database, before: Vec<(DocId, Vec<f32>)>) {
        let lock = Arc::new(RwLock::new(db));
        let (written, holds) = written_beside(&lock, &Statement::Compact(None));
        let db = lock.read().expect("lock");
        let ix = &db.collection("g").expect("g").vectors["v"];
        assert_eq!(
            ix.serialize_graph(),
            locked(&db, &before, &written, true).serialize_graph()
        );
        assert_eq!(ix.dump(1..=400), rebuilt(&db).dump(1..=400));
        assert_eq!(ix.len(), 300);
        // Each write meanwhile a tombstone, for the next compact.
        assert_eq!(ix.dead(), 101);
        assert!(holds.swap > Duration::ZERO && holds.longest() >= holds.swap);
    }

    /// The graph a rebuild at the end holds: every live document's vector.
    fn rebuilt(db: &Database) -> VectorIndex {
        let mut ix = VectorIndex::with_precision(2, spec(db), VecPrec::F32);
        ix.insert_batch(&vectors(db));
        ix
    }

    /// `create index` reads its field with no lock held too, and links the
    /// writes made meanwhile into its graph in rounds, keeping the node a
    /// document holds already.
    #[test]
    fn create_index_reads_its_field_with_no_lock_held() {
        let db = graphed(300, false);
        let before = vectors(&db);
        let lock = Arc::new(RwLock::new(db));
        let (written, holds) = written_beside(&lock, &create_index("v"));
        let db = lock.read().expect("lock");
        let ix = &db.collection("g").expect("g").vectors["v"];
        assert_eq!(
            ix.serialize_graph(),
            locked(&db, &before, &written, false).serialize_graph()
        );
        assert_eq!(ix.len(), 300);
        assert!(holds.put > Duration::ZERO);
    }

    /// `create index` on `g`'s `field`: a graph on `v`, a hash on `tag`.
    fn create_index(field: &str) -> Statement {
        let kind = match field {
            "v" => IndexKind::Vector(VectorIndexSpec::default().resolved()),
            _ => IndexKind::Hash { unique: false },
        };
        create_index_of(field, kind)
    }

    /// `create index` on `g`'s `v`, its graph built by `spec`.
    fn create_index_by(spec: VectorIndexSpec) -> Statement {
        create_index_of("v", IndexKind::Vector(spec.resolved()))
    }

    fn create_index_of(field: &str, kind: IndexKind) -> Statement {
        Statement::CreateIndex {
            collection: "g".into(),
            field: field.into(),
            kind,
            if_not_exists: false,
        }
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

    /// [`a_compact_reads_its_graphs_with_no_lock_held`] over a mapped file:
    /// the documents the copy reads in the file, every tenth written over
    /// since and held in memory, and the rewrite written beside it.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    #[test]
    fn a_compact_of_a_mapped_file_reads_its_graphs_with_no_lock_held() {
        let scratch = crate::engine::unwinding::Scratch::new("fennecdb-graphs-beside");
        let path = scratch.0.join("g.fennec");
        {
            let mut db = crate::fs::open(&path).expect("open");
            made(&mut db, "g", true);
            filled(&mut db, "g", 300);
            db.sync().expect("sync");
        }
        let mut db = crate::fs::open(&path).expect("open again");
        assert!(db.mapped, "a mapped file");
        written_over(&mut db, "g", 300);
        assert_eq!(db.collection("g").expect("g").vectors["v"].dead(), 30);
        let before = vectors(&db);
        compacted_beside(db, before);
    }

    /// A compact of several collections, one of them changed while its
    /// graphs were built -- dropped, altered, or given another index by a
    /// second `create index` beside it, once its graph's store was taken --
    /// is refused, and places no graph: each stands as the writes left it.
    /// Asked one at a time, the first's rebuilt graph went in place before
    /// the second's change refused the compact, without the write a round
    /// had drained for it -- document 200's old vector, which the graph
    /// keeper would then have saved. Dropped before its store was taken, it
    /// is refused as it is taken. Changed so before, its graph is built
    /// from it as it stands then.
    #[test]
    fn a_compact_of_several_collections_places_no_graph_where_one_changed() {
        for (change, at) in [
            ("drop", 0),
            ("drop", 1),
            ("alter", 0),
            ("alter", 1),
            ("index", 1),
        ] {
            let twin = || {
                let mut db = graphed(300, true);
                made(&mut db, "h", true);
                filled(&mut db, "h", 50);
                written_over(&mut db, "h", 50);
                db
            };
            let changed = move |g: &mut Database| match change {
                "drop" => g.execute(&Statement::DropCollection {
                    name: "h".into(),
                    if_exists: false,
                }),
                "alter" => g.execute(&Statement::AlterCollection {
                    collection: "h".into(),
                    change: crate::query::Alter::AddField(Field::new("more", DataType::Text)),
                }),
                _ => unreachable!("beside"),
            };
            let lock = Arc::new(RwLock::new(twin()));
            // `g`'s graph read at the first, `h`'s at the second.
            let (r, _) = beside(&lock, &Statement::Compact(None), move |point, met, l| {
                if (point, met) != ("read", at) {
                    return;
                }
                put(&mut writer(l), 200, vec_of(200, 2));
                match change {
                    "index" => {
                        let mut index = create_index("tag");
                        if let Statement::CreateIndex { collection, .. } = &mut index {
                            *collection = "h".into();
                        }
                        Database::maintain(l, &index)
                            .expect("a maintenance")
                            .expect("the second index");
                    }
                    _ => {
                        changed(&mut writer(l)).expect("the change");
                    }
                }
            });
            let graph = |db: &Database, name: &str| {
                let ix = &db.collection(name).expect("the collection").vectors["v"];
                (ix.dead(), ix.serialize_graph())
            };
            // Altered before its store was taken: both rebuilt, `g`'s with
            // the write's tombstone.
            if (change, at) == ("alter", 0) {
                r.expect("the compact");
                let db = lock.read().expect("lock");
                assert_eq!((graph(&db, "g").0, graph(&db, "h").0), (1, 0));
                continue;
            }
            assert!(
                matches!(&r, Err(Error::Query(m)) if m.starts_with("`h` changed while it was compacted")),
                "{change} at {at}: {r:?}"
            );
            let mut want = twin();
            put(&mut want, 200, vec_of(200, 2));
            let db = lock.read().expect("lock");
            assert_eq!(graph(&db, "g"), graph(&want, "g"), "{change} at {at}");
            assert_eq!(graph(&db, "g").0, 31, "{change} at {at}: no graph placed");
            if change != "drop" {
                assert_eq!(graph(&db, "h"), graph(&want, "h"), "{change} at {at}");
            }
        }
    }

    /// A compact takes each graph's store as a pin takes it just before it
    /// reads that graph's vectors, and lets it go once they are read, its
    /// charge to the pins' budget with it: a long read beside the build is
    /// pinned while the budget holds the read and one collection's charge.
    /// Taken all at once, each collection's store was held through the
    /// builds of the graphs before its own, and past a few million rows
    /// every long read ran under the lock for that while -- the stall a pin
    /// is there to prevent.
    #[test]
    fn a_long_read_is_pinned_while_a_compact_builds_its_graphs() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut db = Database::new();
        for name in ["a", "b", "c"] {
            made(&mut db, name, true);
            filled(&mut db, name, 300);
            written_over(&mut db, name, 300);
        }
        let charge = db.collection("a").expect("a").store.pinned_bytes();
        for name in ["b", "c"] {
            let c = db.collection(name).expect("the collection");
            assert_eq!(c.store.pinned_bytes(), charge);
        }
        // Room for the read's and one collection's charge, not for three.
        db.set_pin_at(0);
        db.set_pin_budget(charge * 5 / 2);
        let read = Statement::Select(Select {
            collection: "a".into(),
            ..Select::default()
        });
        let lock = Arc::new(RwLock::new(db));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        let (r, _) = beside(&lock, &Statement::Compact(None), move |point, met, l| {
            {
                let g = l.try_read().expect("no lock held");
                let held = g.pins.load(Relaxed);
                let none: &[Value] = &[];
                let pinned = g.pin(&[(&read, none)]).is_some();
                let mut s = s.lock().expect("seen");
                s.push((point.to_string(), met, held, pinned));
            }
            // A write for a round to link once `c`'s store is taken, every
            // charge let go by then.
            if (point, met) == ("read", 2) {
                put_to(&mut writer(l), "c", 7, vec_of(7, 2));
            }
        });
        r.expect("the compact");
        let at = |point: &str, met: usize, held: usize| (point.to_string(), met, held, true);
        assert_eq!(
            *seen.lock().expect("seen"),
            [
                at("read", 0, charge),
                at("read", 1, charge),
                at("read", 2, charge),
                at("round", 0, 0),
            ]
        );
        assert_eq!(lock.read().expect("lock").pins.load(Relaxed), 0);
    }

    /// A document written in one round and again in the next is caught up
    /// with twice, and the graph holds its last vector: every live
    /// document's, as a rebuild at the end does -- for a compact, a
    /// tombstone more than the read lock would have left, for the next.
    #[test]
    fn a_document_written_across_rounds_holds_its_last_vector() {
        for stmt in [Statement::Compact(None), create_index("v")] {
            let indexed = matches!(stmt, Statement::Compact(_));
            let lock = Arc::new(RwLock::new(graphed(300, indexed)));
            let (r, _) = beside(&lock, &stmt, |point, met, l| match (point, met) {
                // A round of 100, then one of the second write alone.
                ("read", 0) => {
                    for id in 1..=100 {
                        put(&mut writer(l), id, vec_of(id, 2));
                    }
                }
                ("round", 0) => put(&mut writer(l), 7, vec_of(7, 3)),
                _ => {}
            });
            r.expect("it ran");
            let db = lock.read().expect("lock");
            let ix = &db.collection("g").expect("g").vectors["v"];
            assert_eq!(ix.dump(1..=300), rebuilt(&db).dump(1..=300), "{stmt:?}");
            // Each of the 100 a tombstone, and the 7th's node of the first
            // round another.
            assert_eq!(ix.dead(), 101, "{stmt:?}");
        }
    }

    /// A vector written again unchanged, and one another document's node
    /// holds, are each one node ("a vector written again is one node"):
    /// the graph is the one the read lock built, node for node, and holds
    /// what a rebuild at the end holds.
    #[test]
    fn a_vector_written_unchanged_or_another_s_is_one_node() {
        for stmt in [Statement::Compact(None), create_index("v")] {
            let compact = matches!(stmt, Statement::Compact(_));
            let db = graphed(300, compact);
            let before = vectors(&db);
            let lock = Arc::new(RwLock::new(db));
            let (r, _) = beside(&lock, &stmt, |point, met, l| {
                if (point, met) == ("read", 0) {
                    let mut g = writer(l);
                    // As it stands, and document 5's.
                    put(&mut g, 3, vec_of(3, 0));
                    put(&mut g, 4, vec_of(5, 0));
                }
            });
            r.expect("it ran");
            let db = lock.read().expect("lock");
            let ix = &db.collection("g").expect("g").vectors["v"];
            assert_eq!(
                ix.serialize_graph(),
                locked(&db, &before, &[3, 4], compact).serialize_graph(),
                "{stmt:?}"
            );
            assert_eq!(ix.dump(1..=300), rebuilt(&db).dump(1..=300), "{stmt:?}");
            assert_eq!(ix.len(), 300, "{stmt:?}");
        }
    }

    /// A block put back while the graph is built leaves nothing to catch up
    /// with: its writes are noted only as it lands. The graph holds the
    /// document as it stood, and the write landed beside it.
    #[test]
    fn a_block_put_back_during_the_build_reaches_no_graph() {
        for stmt in [Statement::Compact(None), create_index("v")] {
            let compact = matches!(stmt, Statement::Compact(_));
            let db = graphed(300, compact);
            let before = vectors(&db);
            let lock = Arc::new(RwLock::new(db));
            let (r, _) = beside(&lock, &stmt, |point, met, l| {
                if (point, met) == ("read", 0) {
                    let mut g = writer(l);
                    g.begin().expect("begin");
                    put(&mut g, 8, vec_of(8, 5));
                    del(&mut g, 12);
                    g.rollback();
                    put(&mut g, 9, vec_of(9, 2));
                }
            });
            r.expect("it ran");
            let db = lock.read().expect("lock");
            let ix = &db.collection("g").expect("g").vectors["v"];
            assert_eq!(
                ix.serialize_graph(),
                locked(&db, &before, &[9], compact).serialize_graph(),
                "{stmt:?}"
            );
            assert_eq!(ix.dump(1..=300), rebuilt(&db).dump(1..=300), "{stmt:?}");
            assert_eq!(ix.len(), 300, "{stmt:?}");
        }
    }

    /// A second `create index` on the collection while a first builds its
    /// graph beside it: the second lands, and the first is refused, placing
    /// nothing.
    #[test]
    fn a_second_create_index_during_the_build_refuses_the_first() {
        let lock = Arc::new(RwLock::new(graphed(300, false)));
        let (r, _) = beside(&lock, &create_index("v"), |point, met, l| {
            if (point, met) == ("read", 0) {
                Database::maintain(l, &create_index("tag"))
                    .expect("a maintenance")
                    .expect("the second index");
            }
        });
        assert!(
            matches!(&r, Err(Error::Query(m)) if m.starts_with("`g` changed while the index was built")),
            "{r:?}"
        );
        let db = lock.read().expect("lock");
        let g = db.collection("g").expect("g");
        assert!(g.vectors.is_empty());
        assert!(matches!(g.schema.fields[1].index, IndexKind::None));
        assert!(matches!(
            g.schema.fields[2].index,
            IndexKind::Hash { unique: false }
        ));
    }

    /// What the last round leaves is linked under the write lock up to what
    /// costs `GRAPH_HELD` -- at 2 dimensions, 1 613 links, and as many
    /// fewer as `ef_construction` and `m` widen past the defaults each link's
    /// search: 201 at `ef_construction` 1 600, 100 at `m` 32 with it. Past it
    /// a compact keeps the graph the writes left, and says so, and
    /// `create index` is refused saying why, rather than every writer held
    /// out while it links them.
    #[test]
    fn past_the_last_hold_a_compact_keeps_its_graph_and_create_index_is_refused() {
        let at = |m, ef_construction| VectorIndexSpec {
            m,
            ef_construction,
            ..VectorIndexSpec::default()
        };
        for (spec, held) in [
            (at(16, 200), 1_613),
            (at(16, 1_600), 201),
            (at(32, 1_600), 100),
        ] {
            assert_eq!(held_links(2, &spec), held);
            past_the_last_hold(spec, held);
        }
    }

    /// [`past_the_last_hold_a_compact_keeps_its_graph_and_create_index_is_refused`]
    /// of a graph built by `spec`, whose last hold links `held`.
    fn past_the_last_hold(spec: VectorIndexSpec, held: usize) {
        let (m, efc) = (spec.m, spec.ef_construction);
        for left in [held, held + 1] {
            let all = 1..=(10 + left) as DocId;
            // Ten in the first round, which ends the rounds, and `left`
            // written during it, for the write lock.
            let writes = move |point: &str, met: usize, l: &RwLock<Database>| match (point, met) {
                ("read", 0) => {
                    for id in 1..=10 {
                        put(&mut writer(l), id, vec_of(id, 2));
                    }
                }
                ("round", 0) => {
                    for id in 11..11 + left as i64 {
                        put(&mut writer(l), id, vec_of(id, 3));
                    }
                }
                _ => {}
            };
            let lock = Arc::new(RwLock::new(graphed_by(300, true, spec)));
            let (r, holds) = beside(&lock, &Statement::Compact(None), writes);
            r.expect("the compact");
            let mut want = graphed_by(300, true, spec);
            for id in 1..=10 {
                put(&mut want, id, vec_of(id, 2));
            }
            for id in 11..11 + left as i64 {
                put(&mut want, id, vec_of(id, 3));
            }
            let db = lock.read().expect("lock");
            let ix = &db.collection("g").expect("g").vectors["v"];
            let stood = &want.collection("g").expect("g").vectors["v"];
            assert_eq!(ix.dump(all.clone()), stood.dump(all.clone()), "{left}");
            match left > held {
                // As the writes left it: 30 tombstones, and one each.
                true => {
                    assert_eq!(ix.serialize_graph(), stood.serialize_graph(), "{left}");
                    let kept = KeptGraph {
                        collection: "g".into(),
                        field: "v".into(),
                        why: KeptFor::Behind(left),
                    };
                    assert_eq!(kept.to_string(), format!("graph `g.v` kept, {left} behind"));
                    assert_eq!(holds.kept, [kept]);
                }
                // Rebuilt, a tombstone each write meanwhile to a document
                // it held.
                false => {
                    assert_eq!(ix.dead(), 10 + left.min(290), "{left}");
                    assert!(holds.kept.is_empty(), "{left}");
                }
            }
            assert!(holds.put > Duration::ZERO);
            drop(db);

            let lock = Arc::new(RwLock::new(graphed(300, false)));
            let (r, _) = beside(&lock, &create_index_by(spec), writes);
            let db = lock.read().expect("lock");
            let g = db.collection("g").expect("g");
            match left > held {
                true => {
                    let says = format!(
                        "the graph of `g.v` fell behind its writes: {left} documents were written \
                         during its last round of catching up, and the write lock links {held} at \
                         most at 2 dimensions, m {m} and ef_construction {efc}"
                    );
                    assert!(
                        matches!(&r, Err(Error::Query(m)) if m.starts_with(&says)),
                        "{r:?}"
                    );
                    assert!(g.vectors.is_empty());
                    assert!(matches!(g.schema.fields[1].index, IndexKind::None));
                }
                false => {
                    r.expect("the index");
                    let ix = &g.vectors["v"];
                    assert_eq!(ix.dump(all.clone()), rebuilt(&db).dump(all.clone()));
                }
            }
        }
    }

    /// A link's cost grows with the beam its search walks and the
    /// neighbours it measures, past the defaults: at the settings timed into
    /// 20 000 x 768 the last hold links as many fewer as each link costs
    /// more, every one near the default's 116 ms, and below the defaults
    /// as many as at them. One link costing past the whole bound leaves none
    /// held, and the widest settings a schema takes saturate, never wrap.
    #[test]
    fn a_link_costs_by_its_beam_and_its_neighbours() {
        let at = |m, ef_construction| VectorIndexSpec {
            m,
            ef_construction,
            ..VectorIndexSpec::default()
        };
        for (m, efc, held) in [
            (16, 200, 128),
            (16, 400, 64),
            (16, 800, 32),
            (32, 400, 32),
            (48, 800, 10),
            (2, 4, 128),
            (8, 100, 128),
            (16, 25_600, 1),
            (16, 25_601, 0),
            (100, 4_097, 0),
        ] {
            assert_eq!(
                held_links(768, &at(m, efc)),
                held,
                "m {m}, ef_construction {efc}"
            );
        }
        assert_eq!(held_links(3_072, &at(16, 200)), 33);
        let widest = at(crate::schema::MAX_M, u32::MAX as usize);
        let max_dim = crate::schema::MAX_DIM;
        assert_eq!(link_cost(max_dim, &widest), u64::MAX.div_ceil(3_200));
        assert_eq!(held_links(max_dim, &widest), 0);
    }

    /// What a round's links cost is a `u64`, saturating: on a 32-bit
    /// target the `usize` product of 5.2 million links at 768 dimensions
    /// wrapped, and a round past it read as one costing less than
    /// `GRAPH_DONE`, or a last hold past the bound as one under it.
    #[test]
    fn a_cost_of_links_never_wraps() {
        let each = link_cost(768, &VectorIndexSpec::default());
        assert_eq!(each, 832);
        assert_eq!(links_cost(5_200_000, each), 4_326_400_000);
        let most = links_cost(usize::MAX, each);
        assert!(most >= usize::MAX as u64 && most > held_cost(), "{most}");
        if usize::BITS == 64 {
            assert_eq!(most, u64::MAX);
        }
        assert_eq!(links_cost(2, u64::MAX), u64::MAX);
    }

    /// The write lock's links are bounded across a compact's graphs, not
    /// each graph's: of two collections each left 900 links at 2
    /// dimensions, together past the 1 613 the bound holds, the first's
    /// graph is put in place and the second's kept as its writes left it.
    #[test]
    fn the_last_hold_is_bounded_across_a_compact_s_graphs() {
        let twin = || {
            let mut db = graphed(300, true);
            made(&mut db, "h", true);
            filled(&mut db, "h", 300);
            written_over(&mut db, "h", 300);
            db
        };
        // Ten each once both are copied, which ends the rounds after the
        // first, and 900 each during it, for the write lock.
        let writes =
            |point: &str, met: usize, put: &mut dyn FnMut(&str, i64, i64)| match (point, met) {
                ("read", 1) => {
                    for name in ["g", "h"] {
                        (1..=10).for_each(|id| put(name, id, 2));
                    }
                }
                ("round", 0) => {
                    for name in ["g", "h"] {
                        (11..=910).for_each(|id| put(name, id, 3));
                    }
                }
                _ => {}
            };
        let lock = Arc::new(RwLock::new(twin()));
        let (r, holds) = beside(&lock, &Statement::Compact(None), move |point, met, l| {
            writes(point, met, &mut |name, id, gen| {
                put_to(&mut writer(l), name, id, vec_of(id, gen))
            })
        });
        r.expect("the compact");
        let mut want = twin();
        for (point, met) in [("read", 1), ("round", 0)] {
            writes(point, met, &mut |name, id, gen| {
                put_to(&mut want, name, id, vec_of(id, gen))
            });
        }
        let db = lock.read().expect("lock");
        let graph = |db: &Database, name: &str| {
            let ix = &db.collection(name).expect("the collection").vectors["v"];
            (ix.dump(1..=910), ix.serialize_graph())
        };
        let (g, h) = (graph(&db, "g"), graph(&db, "h"));
        // The first rebuilt, every live document's vector in it.
        assert_eq!(g.0, graph(&want, "g").0);
        assert_ne!(g.1, graph(&want, "g").1);
        // The second as its writes left it.
        assert_eq!(h, graph(&want, "h"));
        let kept = KeptGraph {
            collection: "h".into(),
            field: "v".into(),
            why: KeptFor::Behind(900),
        };
        assert_eq!(holds.kept, [kept]);
    }

    /// A document a round cannot look up -- `Store::snapshot` refusing it,
    /// not the read after -- leaves its graph as it stood, as one the copy
    /// cannot read does, and the compact goes on, the other collection's
    /// graph put in place.
    #[test]
    fn a_document_a_round_cannot_look_up_keeps_the_graph() {
        let mut db = graphed(300, true);
        made(&mut db, "h", true);
        filled(&mut db, "h", 50);
        written_over(&mut db, "h", 50);
        let stood = |db: &Database| {
            let ix = &db.collection("g").expect("g").vectors["v"];
            (ix.len(), ix.dead(), ix.serialize_graph())
        };
        let before = stood(&db);
        let lock = Arc::new(RwLock::new(db));
        let real = Arc::new(std::sync::Mutex::new(None));
        let r2 = Arc::clone(&real);
        let (r, holds) = beside(&lock, &Statement::Compact(None), move |point, met, l| {
            let mut g = writer(l);
            let c = g.collections.get_mut("g").expect("g");
            match (point, met) {
                // `g`'s 5th pointed past every segment, and noted as a
                // write is; `h` written once its copy is taken, for the
                // round to look up after `g`'s chunk.
                ("read", 1) => {
                    *r2.lock().expect("real") = c.store.loc(5);
                    let nowhere = crate::store::Loc {
                        seg: 9_999,
                        off: 0,
                        len: 8,
                    };
                    c.store.point(5, Some(nowhere));
                    let cid = c.id;
                    g.note(cid, 5);
                    put_to(&mut g, "h", 3, vec_of(3, 2));
                }
                // The round met it: put back, for the rewrite to copy.
                ("round", 0) => {
                    let at = r2.lock().expect("real").take();
                    assert!(at.is_some(), "the 5th was pointed away");
                    c.store.point(5, at);
                }
                _ => {}
            }
        });
        r.expect("the compact");
        assert!(real.lock().expect("real").is_none(), "the round met it");
        let db = lock.read().expect("lock");
        assert_eq!(stood(&db), before);
        let kept = KeptGraph {
            collection: "g".into(),
            field: "v".into(),
            why: KeptFor::Unread,
        };
        assert_eq!(kept.to_string(), "graph `g.v` kept, a document unread");
        assert_eq!(holds.kept, [kept]);
        // `h`'s rebuilt: its five tombstones gone, the write's one made.
        assert_eq!(db.collection("h").expect("h").vectors["v"].dead(), 1);
    }
}
