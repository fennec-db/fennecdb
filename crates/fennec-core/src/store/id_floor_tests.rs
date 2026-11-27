//! The id index's floor (`IdIndex::cut`, `IdIndex::rebuild`), judged by a
//! model that knows nothing of the index -- the live ids, their payloads,
//! and the next id `allocate_id` gives -- and by main's index itself: its
//! code as it stood before the floor, driven by the same writes, whose
//! bytes the store's are held at or under and whose layout the store's
//! array is a suffix of.

use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// fennec-core's unit tests' allocator: `System`'s, counting the bytes
/// each thread holds -- every allocation's size as asked, less each one let
/// go -- so that an index's bytes are read as the allocator gave them, never
/// as the code under test counts them. Counted by its own `map_bytes`, as
/// the store's index was, a `map_bytes` halved passed this file's every
/// test: the figure the drop weighs and the figure the test held to main's
/// halved alike. A thread's own count, so that the tests running beside
/// this one move none of its figures; only differences of it are read. The
/// index is the crate's own, so only this binary reaches it: a binary of
/// its own could not.
struct Counting;

thread_local! {
    /// The bytes this thread was handed less those it let go, wrapping: an
    /// allocation another thread lets go is taken off that thread's.
    static HELD: Cell<usize> = const { Cell::new(0) };
}

/// The thread's count moved by `add` and `less`. A `const` thread local of
/// a `Cell`, which holds no destructor: reached from the allocator, it
/// allocates nothing and is never gone, a thread's last frees too.
fn noted(add: usize, less: usize) {
    HELD.with(|h| h.set(h.get().wrapping_add(add).wrapping_sub(less)));
}

// SAFETY: each method hands `System` the arguments it was handed, under
// the contract `GlobalAlloc` gives them, and gives back what it answered;
// the count beside it allocates nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the layout `alloc` was handed, nonzero in size by its contract.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            noted(layout.size(), 0);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            noted(layout.size(), 0);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: a block this allocator, so `System`, handed out with
        // `layout`, as `dealloc`'s contract says.
        unsafe { System.dealloc(ptr, layout) };
        noted(0, layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: a block `System` handed out with `layout`, and a new size
        // `realloc`'s contract holds, as it was handed them.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            noted(new_size, layout.size());
        }
        p
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The bytes what `make` makes holds of the allocator, while it is held.
fn asked<T>(make: impl FnOnce() -> T) -> usize {
    let before = HELD.with(Cell::get);
    let made = make();
    let bytes = HELD.with(Cell::get).wrapping_sub(before);
    drop(made);
    bytes
}

thread_local! {
    /// The bytes last read of each map, by where it lies: its capacity and
    /// length then, and its bytes ([`map_real`]).
    static READ: std::cell::RefCell<Vec<[usize; 4]>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A map's bytes as the allocator gave them: its clone's, which hashbrown
/// allocates with the map's own buckets, tombstones and all
/// ([`the_oracle_reads_a_maps_own_bytes`]). Read again only where its
/// capacity or its length moved since it was last read there: its buckets
/// grow only where its capacity does, to a figure past any it had, and a
/// clone of every map after every write made the property test seven times
/// slower. A map put where another lay -- a store reopened in the run's
/// place -- is forgotten first ([`forget_maps`]).
fn map_real(map: &HashMap<DocId, Loc>) -> usize {
    let at = map as *const HashMap<DocId, Loc> as usize;
    let seen = [at, map.capacity(), map.len()];
    let known = READ.with(|r| r.borrow().iter().find(|e| e[..3] == seen).map(|e| e[3]));
    if let Some(bytes) = known {
        return bytes;
    }
    let bytes = asked(|| map.clone());
    READ.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|e| e[0] != at);
        r.push([at, seen[1], seen[2], bytes]);
    });
    bytes
}

/// Every map's last reading forgotten: the run's store or main's index
/// laid out anew where the old one lay.
fn forget_maps() {
    READ.with(|r| r.borrow_mut().clear());
}

/// xorshift64*: reproducible from a seed, and enough to spray the orders.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, of: u64) -> bool {
        self.below(of) == 0
    }
}

/// Main's id index (`store.rs` at aa5c260e), its code as it was: the array
/// from id 1 to the highest, the sparse map past it, the same gap. Its map
/// hashes with the store's keys, so that the two maps, given the same ids
/// in the same order, take the same room; its bytes are its allocations',
/// as the store's are counted.
struct MainIds {
    dense: Vec<Loc>,
    sparse: HashMap<DocId, Loc>,
    sparse_floor: DocId,
}

impl MainIds {
    fn new(keys: &std::hash::RandomState) -> MainIds {
        MainIds {
            dense: Vec::new(),
            sparse: HashMap::with_hasher(keys.clone()),
            sparse_floor: 0,
        }
    }

    fn in_dense(&self, id: DocId) -> bool {
        id >= 1 && id <= self.dense.len() as u64
    }

    fn insert(&mut self, id: DocId, loc: Loc) {
        if self.in_dense(id) {
            self.dense[id as usize - 1] = loc;
            return;
        }
        if id >= 1 && id - self.dense.len() as u64 <= MAX_DENSE_GAP {
            self.dense.resize(id as usize, Loc::EMPTY);
            if self.sparse_floor <= id {
                self.take_over_sparse(id);
            }
            self.dense[id as usize - 1] = loc;
            return;
        }
        if self.sparse.is_empty() || id < self.sparse_floor {
            self.sparse_floor = id;
        }
        self.sparse.insert(id, loc);
    }

    fn take_over_sparse(&mut self, upto: DocId) {
        let mut keys: Vec<DocId> = self.sparse.keys().copied().collect();
        keys.sort_unstable();
        let mut end = upto;
        let mut taken = 0;
        for &k in &keys {
            if k > end && k - end > MAX_DENSE_GAP {
                break;
            }
            end = end.max(k);
            taken += 1;
        }
        if end > self.dense.len() as u64 {
            self.dense.resize(end as usize, Loc::EMPTY);
        }
        for &k in &keys[..taken] {
            if let Some(l) = self.sparse.remove(&k) {
                self.dense[k as usize - 1] = l;
            }
        }
        self.sparse_floor = keys.get(taken).copied().unwrap_or(DocId::MAX);
    }

    fn remove(&mut self, id: DocId) {
        if self.in_dense(id) {
            self.dense[id as usize - 1] = Loc::EMPTY;
            return;
        }
        self.sparse.remove(&id);
    }

    /// `id` pointed at `loc` where it is, as main's relocation rewrote
    /// each location: a map's insert makes room before it looks, and grew
    /// a full map that relocation never grew.
    fn relocated(&mut self, id: DocId, loc: Loc) {
        match self.in_dense(id) {
            true => self.dense[id as usize - 1] = loc,
            false => *self.sparse.get_mut(&id).unwrap() = loc,
        }
    }

    fn point(&mut self, id: DocId, loc: Option<Loc>) {
        match loc {
            Some(l) => self.insert(id, l),
            None => self.remove(id),
        }
    }

    /// Main's replay of a file write by write over its live rows.
    fn inserted(keys: &std::hash::RandomState, ids: &[(DocId, Loc)]) -> MainIds {
        let mut m = MainIds::new(keys);
        for &(id, l) in ids {
            m.insert(id, l);
        }
        m
    }

    /// Main's compact in memory (`Store::compacted`): room for the rows
    /// reserved, each inserted in id order.
    fn compacted(keys: &std::hash::RandomState, ids: &[(DocId, Loc)]) -> MainIds {
        let mut m = MainIds::new(keys);
        m.dense.reserve(ids.len());
        for &(id, l) in ids {
            m.insert(id, l);
        }
        m
    }

    /// Main's open of an image's index (`adopt_index`): on every core
    /// where the ids lie within the gap of each other from 0 and leave the
    /// array a quarter empty at most, an array of the last id's length --
    /// an image's index says where its shares end, one share's too; else
    /// in turn, room for the documents reserved and each pushed in id
    /// order.
    fn opened(keys: &std::hash::RandomState, ids: &[(DocId, Loc)], len: u64) -> MainIds {
        let mut m = MainIds::new(keys);
        let (count, mut was) = (ids.len() as u64, 0);
        let near = ids
            .iter()
            .all(|&(id, _)| id - std::mem::replace(&mut was, id) <= MAX_DENSE_GAP);
        if count > 0 && was <= count + count / 4 && near {
            m.dense = Vec::with_capacity(was as usize);
            m.dense.resize(was as usize, Loc::EMPTY);
            for &(id, l) in ids {
                m.dense[id as usize - 1] = l;
            }
            return m;
        }
        m.dense.reserve((ids.len() as u64).min(len / 3) as usize);
        for &(id, l) in ids {
            match id == m.dense.len() as u64 + 1 && m.sparse.is_empty() {
                true => m.dense.push(l),
                false => m.insert(id, l),
            }
        }
        m
    }

    /// Its array's allocation and its map's, as the allocator gave them.
    fn bytes(&self) -> usize {
        self.dense.capacity() * std::mem::size_of::<Loc>() + map_real(&self.sparse)
    }
}

/// The store's index's allocations, as [`MainIds::bytes`] reads main's:
/// its array's, its maps', and where ids are below the floor their box and
/// the walks' list of them, as the allocator gave them -- the list sorted
/// at a walk, a read, so held after the walks too ([`Run::judge`]).
fn bytes(ix: &IdIndex) -> usize {
    ix.dense.capacity() * std::mem::size_of::<Loc>()
        + map_real(&ix.sparse)
        + ix.under.as_ref().map_or(0, |u| {
            std::mem::size_of::<Under>() + map_real(&u.map) + asked(|| u.sorted.get().cloned())
        })
}

/// The store's index held to main's after every write: its bytes at or
/// under main's; its array main's from its floor to the same end, holding
/// the room main's holds at a floor of 1 and main's room's less the
/// floor's at most above it, `room` main's then; its sparse map main's,
/// to its room and its floor.
fn held(ix: &IdIndex, main: &MainIds, what: &str) {
    let shown = || {
        format!(
            "{} bytes where main's held {} (array {} of {} at floor {}, room {}, map {} of {} \
             with {} of {} below; main's array {} of {}, map {} of {})",
            bytes(ix),
            main.bytes(),
            ix.dense.len(),
            ix.dense.capacity(),
            ix.floor,
            ix.room,
            ix.sparse.len(),
            ix.sparse.capacity(),
            ix.below().0,
            ix.below().1,
            main.dense.len(),
            main.dense.capacity(),
            main.sparse.len(),
            main.sparse.capacity()
        )
    };
    assert!(bytes(ix) <= main.bytes(), "{what}: {}", shown());
    let end = ix.floor - 1 + ix.dense.len() as u64;
    assert_eq!(end, main.dense.len() as u64, "{what}: the end, {}", shown());
    match ix.floor {
        1 => assert_eq!(
            ix.dense.capacity(),
            main.dense.capacity(),
            "{what}: main's room, {}",
            shown()
        ),
        _ => assert!(
            ix.room == main.dense.capacity() as u64
                && ix.dense.capacity() as u64 + ix.floor - 1 <= ix.room,
            "{what}: within main's room, {}",
            shown()
        ),
    }
    assert!(
        ix.sparse.len() == main.sparse.len()
            && ix.sparse.capacity() == main.sparse.capacity()
            && ix.sparse_floor == main.sparse_floor,
        "{what}: main's map, its floor {} where main's is {}, {}",
        ix.sparse_floor,
        main.sparse_floor,
        shown()
    );
}

/// What the model holds: the live ids and what each was last written with,
/// and the next id `allocate_id` gives -- the highest given plus 1, taken
/// back by a block put back.
#[derive(Clone, Default)]
struct Model {
    live: BTreeMap<DocId, u64>,
    next_id: DocId,
}

use std::collections::BTreeMap;

fn payload(id: DocId, v: u64) -> Vec<u8> {
    format!("{id}:{v}").into_bytes()
}

/// The block open: where the store stood, each write's id and where its
/// row was, the model as it stood, and how many ids were below the floor.
struct Open {
    mark: Mark,
    undo: Vec<(DocId, Option<Loc>)>,
    model: Model,
    below: usize,
}

/// A store, the model and main's index driven together, a block at a time.
struct Run {
    st: Store,
    /// Main's index: laid out again at each open and compact as main's
    /// were, relocated in place at each checkpoint.
    main: MainIds,
    model: Model,
    version: u64,
    block: Option<Open>,
    /// Whether the open block has written yet: its first write cuts.
    wrote: bool,
    /// The ids last written as new ones, to be judged dense wherever
    /// main's are: one written again from outside, below a floor, is the
    /// map's until a rebuild.
    news: std::collections::BTreeSet<DocId>,
    cuts: usize,
    /// How many ids were written below the floor, and how many times it
    /// dropped to 1.
    belows: usize,
    drops: usize,
}

impl Run {
    fn new() -> Run {
        forget_maps();
        let st = Store::new();
        let main = MainIds::new(st.index.sparse.hasher());
        Run {
            st,
            main,
            model: Model {
                live: BTreeMap::new(),
                next_id: 1,
            },
            version: 0,
            block: None,
            wrote: false,
            news: Default::default(),
            cuts: 0,
            belows: 0,
            drops: 0,
        }
    }

    fn begin(&mut self) {
        assert!(self.block.is_none());
        let below = self.st.index.below().0;
        self.block = Some(Open {
            mark: self.st.mark(),
            undo: Vec::new(),
            model: self.model.clone(),
            below,
        });
        self.wrote = false;
    }

    /// What a block's write asks first (`Block::remember`): the cut at its
    /// first, and where the row was.
    fn remember(&mut self, id: DocId, mark: Mark) {
        let undo = &mut self.block.as_mut().unwrap().undo;
        if !self.wrote {
            let floor = self.st.index.floor;
            self.st.cut_ids_before(&mark);
            self.cuts += (self.st.index.floor != floor) as usize;
            self.wrote = true;
            held(&self.st.index, &self.main, "a cut");
            // Once a block first writes, the run the array starts with is
            // under half of it -- or the array is under `CUT_FROM`, or the
            // floor stands at the next id.
            let ix = &self.st.index;
            let lead = ix
                .dense
                .iter()
                .position(|l| !l.is_empty())
                .unwrap_or(ix.dense.len());
            assert!(
                lead < half(ix.dense.len())
                    || ix.dense.len() < CUT_FROM
                    || ix.floor >= mark.next_id,
                "lead {lead} of {} at floor {}, next {}",
                ix.dense.len(),
                ix.floor,
                mark.next_id
            );
        }
        undo.push((id, self.st.loc(id)));
    }

    /// A put of a new row, given the next id.
    fn put_new(&mut self) -> DocId {
        let mark = self.st.mark();
        let id = self.st.allocate_id();
        assert_eq!(id, self.model.next_id, "the next id");
        self.remember(id, mark);
        self.write(id);
        self.model.next_id += 1;
        self.news.insert(id);
        id
    }

    /// A put of `id`, new or over a row.
    fn put(&mut self, id: DocId) {
        if id < self.st.index.floor && !self.model.live.contains_key(&id) {
            self.belows += 1;
        }
        self.news.remove(&id);
        let mark = self.st.mark();
        self.remember(id, mark);
        self.write(id);
        self.model.next_id = self.model.next_id.max(id + 1);
    }

    fn write(&mut self, id: DocId) {
        self.version += 1;
        let floor = self.st.index.floor;
        self.st.append(OP_PUT, id, &payload(id, self.version));
        self.drops += (floor > 1 && self.st.index.floor == 1) as usize;
        let l = self.st.loc(id).unwrap();
        self.main.insert(id, l);
        self.model.live.insert(id, self.version);
        held(&self.st.index, &self.main, "a write");
    }

    fn del(&mut self, id: DocId) {
        if !self.model.live.contains_key(&id) {
            return;
        }
        let mark = self.st.mark();
        self.remember(id, mark);
        self.st.append(OP_DEL, id, &[]);
        self.main.remove(id);
        self.model.live.remove(&id);
        held(&self.st.index, &self.main, "a delete");
    }

    fn land(&mut self) {
        self.block = None;
    }

    /// The block put back as `Database::rewind` puts it: each write's id
    /// pointed back, the last first, then the store taken back to its mark.
    fn put_back(&mut self) {
        let Open {
            mark,
            undo,
            model,
            below,
        } = self.block.take().unwrap();
        for &(id, loc) in undo.iter().rev() {
            self.st.point(id, loc);
            self.main.point(id, loc);
            held(&self.st.index, &self.main, "a write put back");
        }
        self.st.rewind(mark);
        self.model = model;
        // No cut saw the block's deletes: it put back no id below the floor
        // -- fewer there, where the block's writes below it dropped it.
        assert!(
            self.st.index.below().0 <= below,
            "a block put back below the floor"
        );
        let back: Vec<DocId> = self.st.ids();
        let want: Vec<DocId> = self.model.live.keys().copied().collect();
        assert_eq!(back, want, "a block put back");
    }

    /// One block of `f`'s writes, landed or put back.
    fn block(&mut self, back: bool, f: impl FnOnce(&mut Run)) {
        self.begin();
        f(self);
        match back {
            true => self.put_back(),
            false => self.land(),
        }
    }

    fn oldest(&self, n: usize) -> Vec<DocId> {
        self.model.live.keys().take(n).copied().collect()
    }

    fn newest(&self, n: usize) -> Vec<DocId> {
        self.model.live.keys().rev().take(n).copied().collect()
    }

    /// The store and main's index opened from an image of the live rows:
    /// a reseed, or a restart. `walked`: replayed frame by frame, as a file
    /// with no image index is. Before its rebuild the store's index is
    /// main's, room and all.
    fn reseed(&mut self, walked: bool) {
        let mut image = Vec::new();
        self.st.write_live(&mut image).unwrap();
        let mut index = Vec::new();
        self.st.image_index(true, &mut index);
        let at = 4u64;
        let mut file = vec![0u8; at as usize];
        file.extend_from_slice(&image);
        let base: Base = std::sync::Arc::new(file);
        let len = image.len() as u64;
        let mut fresh = Store::new();
        match walked {
            true => drop(fresh.replay_mapped(&base, at, len, &mut |_| {}).unwrap()),
            false => assert!(fresh.adopt_index(&base, at, len, &index)),
        }
        fresh.raise_next_id(self.model.next_id);
        forget_maps();
        let ids: Vec<(DocId, Loc)> = fresh.index.walk(0).collect();
        let keys = fresh.index.sparse.hasher();
        self.main = match walked {
            true => MainIds::inserted(keys, &ids),
            false => MainIds::opened(keys, &ids, len),
        };
        held(&fresh.index, &self.main, "opened");
        fresh.rebuild_ids();
        self.st = fresh;
        self.rebuilt("an open", 1);
    }

    /// A compact in memory (`Store::compacted`): the store's rows copied
    /// into a fresh one, as main's copy inserted each.
    fn compacted(&mut self) {
        let fresh = self.st.compacted().unwrap();
        forget_maps();
        let ids: Vec<(DocId, Loc)> = fresh.index.walk(0).collect();
        self.main = MainIds::compacted(fresh.index.sparse.hasher(), &ids);
        self.st = fresh;
        self.rebuilt("a compact", 1);
    }

    /// The store pointed at an image of itself, as a checkpoint (`live`
    /// false) or a compact does; main's index relocated in place, its
    /// layout as it stood.
    fn relocate(&mut self, live: bool) {
        let mut image = Vec::new();
        match live {
            true => self.st.write_live(&mut image).unwrap(),
            false => self.st.write_image(&mut image).unwrap(),
        }
        let at = 4u64;
        let mut file = vec![0u8; at as usize];
        file.extend_from_slice(&image);
        let base: Base = std::sync::Arc::new(file);
        let floor = self.st.index.floor;
        match live {
            true => self.st.relocate_live(&base, at),
            false => self.st.relocate_image(&base, at),
        }
        for (id, l) in self.st.index.walk(0) {
            self.main.relocated(id, l);
        }
        self.rebuilt("a checkpoint", floor);
    }

    /// After a rebuild from a floor of `was`: no id below the floor, and
    /// the floor 1, half of main's array at least, the next id, or where
    /// it was -- never a small floor that a write below it drops.
    fn rebuilt(&mut self, what: &str, was: DocId) {
        let ix = &self.st.index;
        held(ix, &self.main, what);
        assert!(ix.under.is_none(), "{what}: ids below the floor");
        let mains = self.main.dense.len();
        assert!(
            [1, was, self.st.next_id()].contains(&ix.floor) || ix.floor > half(mains) as u64,
            "{what}: a floor of {} under half of main's {mains}",
            ix.floor
        );
    }

    /// Everything the model says, judged.
    fn judge(&mut self, what: &str) {
        judge_store(&self.st, &self.model, what);
        let ix = &self.st.index;
        held(ix, &self.main, what);
        let mut ours: Vec<DocId> = ix.sparse.keys().copied().collect();
        let mut mains: Vec<DocId> = self.main.sparse.keys().copied().collect();
        ours.sort_unstable();
        mains.sort_unstable();
        assert_eq!(ours, mains, "{what}: main's sparse ids");
        // A new id lands where main's does: in the array where its is.
        for &id in &self.news {
            if self.model.live.contains_key(&id) && self.main.in_dense(id) {
                assert!(
                    ix.slot(id).is_some(),
                    "{what}: new id {id} sparse at floor {}",
                    ix.floor
                );
            }
        }
    }
}

/// The store holds what the model holds: every live id reads back as last
/// written, in order, from any id, and nothing else; the index's counts as
/// its slots say.
fn judge_store(st: &Store, model: &Model, what: &str) {
    let want: Vec<DocId> = model.live.keys().copied().collect();
    assert_eq!(st.len(), want.len(), "{what}: len");
    assert_eq!(st.ids(), want, "{what}: ids");
    for (&id, &v) in &model.live {
        assert_eq!(
            st.raw(id).unwrap(),
            Some(&payload(id, v)[..]),
            "{what}: id {id}"
        );
    }
    // Forty walks from ids it holds and forty from after one at most,
    // each read to the end: one every 97 ids of a run's largest tables made
    // the judging quadratic, most of the test's time.
    let (on, after) = (97.max(want.len() / 40), 89.max(want.len() / 40));
    for from in [0, 1, want.len() as u64 / 2, 1 << 52]
        .into_iter()
        .chain(want.iter().step_by(on).copied())
        .chain(want.iter().step_by(after).map(|i| i + 1))
    {
        let got: Vec<DocId> = st.iter_ids_from(from).collect();
        let at = want.partition_point(|&i| i < from);
        assert_eq!(got, want[at..], "{what}: from {from}");
    }
    // A walk gives each id's location as a lookup reads it: the list of
    // the ids below the floor keeps theirs through writes over them.
    for (id, l) in st.index.walk(0) {
        assert_eq!(Some(l), st.loc(id), "{what}: id {id}'s location in a walk");
    }
    let ix = &st.index;
    assert!(ix.front_end <= half(ix.dense.len()), "{what}: front's end");
    let held = ix.dense[..ix.front_end]
        .iter()
        .filter(|l| !l.is_empty())
        .count();
    assert_eq!(ix.front, held, "{what}: front");
    let end = ix.floor - 1 + ix.dense.len() as u64;
    for &k in ix.sparse.keys() {
        assert!(k > end, "{what}: sparse id {k} not past the end {end}");
    }
    for &k in ix.under.iter().flat_map(|u| u.map.keys()) {
        assert!(
            k < ix.floor,
            "{what}: id {k} not under the floor {}",
            ix.floor
        );
    }
    assert!(
        ix.floor <= st.next_id(),
        "{what}: floor {} past next {}",
        ix.floor,
        st.next_id()
    );
    assert_eq!(st.next_id(), model.next_id, "{what}: next id");
}

/// A phase of the orders real workloads make, from the seed.
fn phase(r: &mut Run, g: &mut Rng) -> &'static str {
    let back = g.chance(6);
    let n = 1 + g.below(200) as usize;
    match g.below(18) {
        // A queue: new rows at the end, the oldest taken off.
        0 => {
            for _ in 0..1 + g.below(20) {
                r.block(false, |r| {
                    for _ in 0..n {
                        r.put_new();
                    }
                });
                let old = r.oldest(n);
                r.block(back, |r| old.into_iter().for_each(|id| r.del(id)));
            }
            "queue"
        }
        // A `@ttl` sweep: everything older than a cut, in one block.
        1 => {
            for _ in 0..n * 4 {
                r.block(false, |r| {
                    r.put_new();
                });
            }
            let keep = r.model.live.len() / (2 + g.below(4) as usize);
            let old = r.oldest(r.model.live.len() - keep);
            r.block(back, |r| old.into_iter().for_each(|id| r.del(id)));
            "sweep"
        }
        // A stack: the newest taken off.
        2 => {
            r.block(false, |r| {
                for _ in 0..n {
                    r.put_new();
                }
            });
            let new = r.newest(n);
            r.block(back, |r| new.into_iter().for_each(|id| r.del(id)));
            "stack"
        }
        // An outbox beside long-lived rows: rows written and sent one at a
        // time, a few kept.
        3 => {
            for _ in 0..n {
                let id = r.put_new_block();
                if !g.chance(50) {
                    r.block(false, |r| r.del(id));
                }
            }
            "outbox"
        }
        // Batches deleted oldest first, newest first or in no order.
        4 => {
            r.block(false, |r| {
                for _ in 0..n * 3 {
                    r.put_new();
                }
            });
            let mut ids: Vec<DocId> = r.model.live.keys().copied().collect();
            match g.below(3) {
                0 => ids.truncate(n * 2),
                1 => {
                    ids.reverse();
                    ids.truncate(n * 2);
                }
                _ => {
                    for i in (1..ids.len()).rev() {
                        ids.swap(i, g.below(i as u64 + 1) as usize);
                    }
                    ids.truncate(n * 2);
                }
            }
            for chunk in ids.chunks(1 + g.below(10) as usize) {
                let chunk = chunk.to_vec();
                r.block(back && g.chance(3), |r| {
                    chunk.into_iter().for_each(|id| r.del(id))
                });
            }
            "batches"
        }
        // Ids from outside, ascending, descending or in no order, about
        // the rows or below them -- under a raised floor too.
        5 | 6 => {
            let top = r.model.next_id + 50;
            let span = (n as u64 * (1 + g.below(8))).min(top);
            let from = g.below(top - span + 1).max(1);
            let mut ids: Vec<DocId> = (from..from + span)
                .step_by(1 + g.below(3) as usize)
                .collect();
            match g.below(3) {
                0 => {}
                1 => ids.reverse(),
                _ => {
                    for i in (1..ids.len()).rev() {
                        ids.swap(i, g.below(i as u64 + 1) as usize);
                    }
                }
            }
            r.block(back, |r| ids.into_iter().for_each(|id| r.put(id)));
            "explicit"
        }
        // Explicit ids deleted, oldest or in no order.
        7 => {
            let mut ids: Vec<DocId> = r.model.live.keys().copied().collect();
            for i in (1..ids.len()).rev() {
                ids.swap(i, g.below(i as u64 + 1) as usize);
            }
            ids.truncate(n);
            r.block(back, |r| ids.into_iter().for_each(|id| r.del(id)));
            "random deletes"
        }
        // A replica's pending row at 2^52, beside its primary's rows, and
        // gone again.
        8 => {
            let pending = (1u64 << 52) + g.below(3);
            r.block(back, |r| r.put(pending));
            if g.chance(2) {
                r.block(false, |r| r.del(pending));
            }
            "pending"
        }
        // A refused `del s where id <= X order id desc`: the oldest rows
        // deleted from the highest of them down, put back.
        9 => {
            let mut old = r.oldest(r.model.live.len() * 3 / 4);
            old.reverse();
            r.block(true, |r| old.into_iter().for_each(|id| r.del(id)));
            "refused descending delete"
        }
        // Rows written over, in a block put back or not.
        10 => {
            let ids: Vec<DocId> = r
                .model
                .live
                .keys()
                .step_by(1 + g.below(5) as usize)
                .copied()
                .collect();
            r.block(back, |r| ids.into_iter().for_each(|id| r.put(id)));
            "written over"
        }
        // A reseed or a restart, mapped or walked.
        11 => {
            r.reseed(g.chance(3));
            "reseed"
        }
        // A checkpoint or a compact.
        12 => {
            r.relocate(g.chance(2));
            "relocate"
        }
        // Ids from outside past the end, a stride apart -- times in
        // seconds an hour apart, the gap and either side of it among them --
        // in one block or a block each.
        14 => {
            let stride = [2, 64, 1_000, 3_600, 4_095, 4_096, 4_097, 10_000][g.below(8) as usize];
            let top = r.model.next_id - 1;
            let ids: Vec<DocId> = (1..=1 + g.below(60)).map(|k| top + k * stride).collect();
            match g.chance(2) {
                true => r.block(back, |r| ids.into_iter().for_each(|id| r.put(id))),
                false => ids.into_iter().for_each(|id| r.block(false, |r| r.put(id))),
            }
            "strided"
        }
        // A compact in memory.
        15 => {
            r.compacted();
            "compacted"
        }
        // A floor raised, and ids below it written and deleted in no
        // order, a walk now and then: their map grows with tombstones in
        // it, its capacity short of its buckets, and the walks' list of
        // them passes over ids deleted since it was sorted.
        17 => {
            r.block(false, |r| (0..n * 4).for_each(|_| _ = r.put_new()));
            let half = r.model.next_id / 2;
            let old: Vec<DocId> = r.model.live.range(..=half).map(|(&id, _)| id).collect();
            r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
            r.put_new_block();
            let floor = r.st.index.floor;
            let mut below: Vec<DocId> = Vec::new();
            for step in 0..(floor / 8).min(150) {
                if r.st.index.floor < floor {
                    break;
                }
                let k = 1 + g.below(32) as usize;
                let mut ids = Vec::new();
                let mut dels = Vec::new();
                for _ in 0..k {
                    match below.is_empty() || !g.chance(3) {
                        true => {
                            let id = 1 + g.below(floor - 1);
                            if !r.model.live.contains_key(&id) && !ids.contains(&id) {
                                ids.push(id);
                            }
                        }
                        false => dels.push(below.swap_remove(g.below(below.len() as u64) as usize)),
                    }
                }
                below.extend_from_slice(&ids);
                r.block(false, |r| {
                    ids.into_iter().for_each(|id| r.put(id));
                    dels.into_iter().for_each(|id| r.del(id));
                });
                if step % 4 == 3 {
                    let from = 1 + g.below(floor);
                    let at = r.model.live.range(from..).next().map(|(&id, _)| id);
                    assert_eq!(r.st.iter_ids_from(from).next(), at, "from {from}");
                    held(&r.st.index, &r.main, "a walk below the floor");
                }
            }
            "tombstones below the floor"
        }
        // A clone, read as it was after a phase of writes.
        13 => {
            let (clone, model) = (r.st.clone(), r.model.clone());
            let what = phase(r, g);
            judge_store(&clone, &model, "a clone");
            what
        }
        // A delete of every row, and new rows after it.
        _ => {
            let all: Vec<DocId> = r.model.live.keys().copied().collect();
            r.block(back, |r| all.into_iter().for_each(|id| r.del(id)));
            r.block(false, |r| {
                for _ in 0..n {
                    r.put_new();
                }
            });
            "emptied"
        }
    }
}

impl Run {
    fn put_new_block(&mut self) -> DocId {
        let mut id = 0;
        self.block(false, |r| id = r.put_new());
        id
    }
}

/// Sixty phases a seed, each drawn from the orders of writes real
/// workloads make, the store, the model and main's index driven alike:
/// held to main's after every write ([`held`]) and judged after each
/// phase ([`Run::judge`]). `FENNEC_ID_FLOOR_SEEDS=600` runs more.
#[test]
fn the_id_index_holds_every_workload_under_mains_memory_and_reads_back() {
    let seeds: u64 = std::env::var("FENNEC_ID_FLOOR_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let (mut cuts, mut belows, mut drops) = (0, 0, 0);
    // `FENNEC_ID_FLOOR_SEED` runs the one seed a failure names.
    let one = std::env::var("FENNEC_ID_FLOOR_SEED")
        .ok()
        .and_then(|v| v.parse().ok());
    for seed in one.map_or(1..=seeds, |s| s..=s) {
        let run = std::panic::catch_unwind(|| {
            let mut g = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let mut r = Run::new();
            for p in 0..60 {
                let what = phase(&mut r, &mut g);
                r.judge(&format!("seed {seed}, phase {p} ({what})"));
            }
            (r.cuts, r.belows, r.drops)
        });
        let (c, b, d) = run.unwrap_or_else(|e| {
            eprintln!("seed {seed} failed");
            std::panic::resume_unwind(e)
        });
        (cuts, belows, drops) = (cuts + c, belows + b, drops + d);
    }
    if one.is_some() {
        return;
    }
    assert!(cuts > seeds as usize, "the phases cut: {cuts}");
    assert!(
        belows > seeds as usize && drops > 0,
        "the phases wrote below the floor: {belows}, {drops} drops"
    );
    if std::env::var_os("FENNEC_ID_FLOOR_SEEDS").is_some() {
        eprintln!("{cuts} cuts, {belows} ids written below the floor, {drops} drops");
    }
}

/// A store of `n` new rows, each its own write.
fn rows(n: u64) -> Store {
    let mut st = Store::new();
    for _ in 0..n {
        let id = st.allocate_id();
        st.append(OP_PUT, id, &payload(id, 0));
    }
    st
}

#[test]
fn a_cut_raises_the_floor_once_the_first_half_is_empty() {
    let mut st = rows(1_000);
    assert_eq!(st.index.dense.capacity(), 1_024);
    for id in 1..=499 {
        st.append(OP_DEL, id, &[]);
    }
    st.cut_ids();
    assert_eq!(
        (st.index.floor, st.index.dense.len()),
        (1, 1_000),
        "id 500 holds the first half"
    );
    st.append(OP_DEL, 500, &[]);
    st.cut_ids();
    assert_eq!(
        (st.index.floor, st.index.dense.len()),
        (501, 500),
        "cut to the first held"
    );
    // Main's room past the floor, the floor's slots left for ids below it.
    assert_eq!(
        (st.index.room, st.index.dense.capacity()),
        (1_024, 524),
        "main's room"
    );
    assert!(st.index.sparse.is_empty());
    for id in 1..=500 {
        assert_eq!(st.raw(id).unwrap(), None);
    }
    for id in 501..=1_000 {
        assert_eq!(st.raw(id).unwrap(), Some(&payload(id, 0)[..]));
    }
    // The next new id lands in the array, past its end as ever.
    let id = st.allocate_id();
    st.append(OP_PUT, id, &payload(id, 0));
    assert_eq!((id, st.index.slot(id)), (1_001, Some(500)));
    // Every slot cut once: a queue's array stays within twice its rows'
    // doubling, and within main's room past the floor.
    for _ in 0..100_000 {
        st.cut_ids();
        let id = st.allocate_id();
        st.append(OP_PUT, id, &payload(id, 0));
        let oldest = st.iter_ids().next().unwrap();
        st.append(OP_DEL, oldest, &[]);
        let ix = &st.index;
        assert!(ix.dense.capacity() as u64 + ix.floor - 1 <= ix.room);
    }
    assert_eq!(st.len(), 501);
    assert!(st.index.dense.len() <= 1_002 && st.index.dense.capacity() <= 2_096);
    assert_eq!(st.index.room, 131_072, "main's room for 101 001 ids");
    // An array under `CUT_FROM` is not cut.
    let mut small = rows(100);
    for id in 1..=99 {
        small.append(OP_DEL, id, &[]);
    }
    small.cut_ids();
    assert_eq!((small.index.floor, small.index.dense.len()), (1, 100));
}

#[test]
fn an_id_below_the_floor_is_held_apart_until_a_rebuild() {
    let mut st = rows(1_000);
    for id in 1..=600 {
        st.append(OP_DEL, id, &[]);
    }
    st.cut_ids();
    assert_eq!(st.index.floor, 601);
    // Written below the floor: `under`'s, read and walked in id order.
    for id in [600, 7, 599] {
        st.append(OP_PUT, id, &payload(id, 1));
    }
    assert_eq!(
        (st.index.floor, st.index.below().0, st.index.sparse.len()),
        (601, 3, 0)
    );
    for id in [7, 599, 600] {
        assert_eq!(st.raw(id).unwrap(), Some(&payload(id, 1)[..]));
    }
    let ids = st.ids();
    assert_eq!(ids[..4], [7, 599, 600, 601]);
    // Past those, the dense slots are the walk's own loop's.
    let mut walk = st.index.walk(0);
    walk.by_ref().take(3).for_each(drop);
    assert_eq!(
        (walk.dense.len(), walk.next().map(|e| e.0)),
        (400, Some(601))
    );
    assert_eq!(
        st.iter_ids_from(8).take(3).collect::<Vec<_>>(),
        [599, 600, 601]
    );
    assert_eq!(st.iter_ids_from(600).next(), Some(600));
    // A rebuild lays the array out as main's: 7 in main's first half, the
    // floor at 1, main's room.
    st.rebuild_ids();
    assert_eq!(
        (st.index.floor, st.index.below().0, st.index.sparse.len()),
        (1, 0, 0)
    );
    assert_eq!(st.index.dense.capacity(), 1_024);
    assert_eq!(st.ids(), ids);
    // Past main's first half, the floor at the lowest of them.
    let mut far = rows(10_000);
    for id in 1..=9_000 {
        far.append(OP_DEL, id, &[]);
    }
    far.cut_ids();
    far.append(OP_PUT, 8_000, &payload(8_000, 1));
    far.append(OP_PUT, 6_000, &payload(6_000, 1));
    far.rebuild_ids();
    assert_eq!((far.index.floor, far.index.below().0), (6_000, 0));
    assert_eq!(far.index.dense.capacity(), 4_001);
    assert_eq!(far.raw(6_000).unwrap(), Some(&payload(6_000, 1)[..]));
}

/// An open of an image's index lays its ids out as main's open did: a
/// queue's past the gap from 1 in the sparse map, ids within it in the
/// array from 1 -- and the rebuild after it cuts that array where its
/// first half holds none.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_open_lays_the_ids_out_as_mains() {
    let opened = |ids: &[DocId]| {
        let mut st = Store::new();
        for &id in ids {
            st.append(OP_PUT, id, &payload(id, 0));
        }
        let mut image = Vec::new();
        st.write_live(&mut image).unwrap();
        let mut index = Vec::new();
        st.image_index(true, &mut index);
        let mut file = vec![0u8; 4];
        file.extend_from_slice(&image);
        let base: Base = std::sync::Arc::new(file);
        let mut fresh = Store::new();
        assert!(fresh.adopt_index(&base, 4, image.len() as u64, &index));
        let all: Vec<(DocId, Loc)> = fresh.index.walk(0).collect();
        let main = MainIds::opened(fresh.index.sparse.hasher(), &all, image.len() as u64);
        held(&fresh.index, &main, "opened");
        fresh.rebuild_ids();
        held(&fresh.index, &main, "rebuilt");
        for &id in ids {
            assert_eq!(fresh.raw(id).unwrap(), Some(&payload(id, 0)[..]));
        }
        fresh
    };
    let queue: Vec<DocId> = (1_000_000..1_003_000).collect();
    let st = opened(&queue);
    assert_eq!(
        (st.index.floor, st.index.dense.len(), st.index.sparse.len()),
        (1, 0, 3_000)
    );
    // Within the gap of 1: the array from 1, cut to its first id.
    let low: Vec<DocId> = (3_000..3_300).collect();
    let st = opened(&low);
    assert_eq!(
        (st.index.floor, st.index.dense.len(), st.index.sparse.len()),
        (3_000, 300, 0)
    );
}

/// An id below a floor within the gap of 2^64 is told apart before the
/// gap past the end is: else its slot was `floor + at`, wrapped, the id
/// given back as one past the top -- and a panic where overflows are
/// checked.
#[test]
fn an_id_below_a_floor_near_the_top_of_the_ids_is_held_apart() {
    let top = DocId::MAX - 10;
    let loc = |n: u32| Loc {
        seg: 0,
        off: n,
        len: 1,
    };
    let mut ix = IdIndex {
        floor: top,
        ..IdIndex::default()
    };
    ix.insert(top, loc(1));
    ix.insert(5, loc(2));
    assert_eq!((ix.ids(), ix.below().0), (vec![5, top], 1));
    assert_eq!((ix.get(5), ix.get(top)), (Some(loc(2)), Some(loc(1))));
}

/// A rebuild cuts the run the array starts with only where a running cut
/// would, its first half empty, and copies nothing else; the first half is
/// looked at only up to its first held slot.
#[test]
fn a_rebuild_cuts_the_lead_only_where_a_running_cut_would() {
    let mut st = rows(10_000);
    st.cut_ids();
    assert_eq!(
        (st.index.front, st.index.front_end),
        (1, 1),
        "one slot looked at"
    );
    st.append(OP_DEL, 1, &[]);
    let at = st.index.dense.as_ptr();
    st.rebuild_ids();
    assert_eq!(
        (st.index.floor, st.index.dense.as_ptr()),
        (1, at),
        "no copy"
    );
    for id in 2..=4_999 {
        st.append(OP_DEL, id, &[]);
    }
    st.rebuild_ids();
    assert_eq!(
        (st.index.floor, st.index.dense.as_ptr()),
        (1, at),
        "under half"
    );
    st.append(OP_DEL, 5_000, &[]);
    st.rebuild_ids();
    assert_eq!((st.index.floor, st.index.dense.len()), (5_001, 5_000));
    // One write below such a floor is no drop.
    st.append(OP_PUT, 1, &payload(1, 1));
    assert_eq!((st.index.floor, st.index.below().0), (5_001, 1));
}

/// A queue of 10 000 rows from 10 001, reopened -- in the sparse map, as
/// main's open put it -- whose blocks delete 3 000 and insert 3 000: held
/// at main's bytes, its ids moving in the map as main's do, the index
/// never laid out again mid-service. Laid out as an array past main's
/// reach, the fourth block moved every id into the map under the write
/// lock, 1.2 s at 10 million rows.
#[test]
fn a_queue_reopened_past_the_gap_moves_nothing_mid_service() {
    for walked in [false, true] {
        let mut r = Run::new();
        r.block(false, |r| {
            (0..20_000).for_each(|_| _ = r.put_new());
        });
        let old = r.oldest(10_000);
        r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
        r.reseed(walked);
        assert_eq!(
            (r.st.index.dense.len(), r.st.index.sparse.len()),
            (0, 10_000)
        );
        for round in 0..8 {
            let old = r.oldest(3_000);
            r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
            r.block(false, |r| (0..3_000).for_each(|_| _ = r.put_new()));
            let ix = &r.st.index;
            assert_eq!((ix.floor, ix.dense.len()), (1, 0), "round {round}");
            assert_eq!(bytes(ix), r.main.bytes(), "round {round}");
            r.judge(&format!("round {round}"));
        }
    }
}

/// A compact of 100 rows from 10 001 -- in the map, as main's compact
/// put them -- and then ids past the end two apart: main's map's bytes,
/// where an array past main's reach doubled to 102 400 slots by 25 651
/// ids, 1.23 MB against main's 820 KB.
#[test]
fn a_compact_past_the_gap_then_ids_two_apart_hold_mains_map() {
    let mut r = Run::new();
    r.block(false, |r| (0..10_100).for_each(|_| _ = r.put_new()));
    let old = r.oldest(10_000);
    r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
    r.compacted();
    assert_eq!((r.st.index.dense.len(), r.st.index.sparse.len()), (0, 100));
    let top = r.model.next_id - 1;
    for k in 1..=25_651 {
        r.block(false, |r| r.put(top + 2 * k));
    }
    let ix = &r.st.index;
    assert_eq!((ix.floor, ix.dense.len(), ix.sparse.len()), (1, 0, 25_751));
    assert_eq!(bytes(ix), r.main.bytes());
    r.judge("two apart");
}

/// Ids written back below a floor until it drops: the array laid out
/// again in main's room, from where it grows as main's does. Laid in an
/// allocation of its length, 1 000 slots, one new row doubled it to
/// 2 000, 24 000 bytes where main's 1 024 slots held it.
#[test]
fn the_drop_lays_the_array_out_in_mains_room() {
    let mut r = Run::new();
    r.block(false, |r| (0..1_000).for_each(|_| _ = r.put_new()));
    let old = r.oldest(600);
    r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
    r.block(false, |r| _ = r.put_new());
    assert_eq!(r.st.index.floor, 601);
    let mut id = 600;
    while r.st.index.floor > 1 {
        r.block(false, |r| r.put(id));
        id -= 1;
    }
    // Written back below a floor of 601: 600 down to `id + 1`.
    assert!(
        (100..=300).contains(&(600 - id)),
        "{} written back",
        600 - id
    );
    let ix = &r.st.index;
    assert_eq!(
        (ix.dense.capacity(), r.main.dense.capacity()),
        (1_024, 1_024)
    );
    r.block(false, |r| _ = r.put_new());
    assert_eq!(
        r.st.index.dense.capacity(),
        1_024,
        "the new row in main's room"
    );
    r.judge("dropped");
}

/// A rebuild leaves no small floor: ids written back below a floor are
/// laid out as main's, a floor of 1 where main's first half holds one of
/// them. Taken in from the floor down while each lay within the gap of
/// the one above, they left a floor of 2, and one write of id 1 copied the
/// array.
#[test]
fn a_rebuild_leaves_no_small_floor() {
    for live in [false, true] {
        let mut r = Run::new();
        r.block(false, |r| (0..1_000).for_each(|_| _ = r.put_new()));
        let old = r.oldest(600);
        r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
        r.block(false, |r| r.put(2));
        assert_eq!((r.st.index.floor, r.st.index.below().0), (601, 1));
        r.relocate(live);
        assert_eq!(r.st.index.floor, 1);
        let at = r.st.index.dense.as_ptr();
        r.block(false, |r| r.put(1));
        assert_eq!((r.st.index.floor, r.st.index.dense.as_ptr()), (1, at));
        r.judge("written below");
    }
}

/// A write over an id below the floor grows its map as a new id does --
/// hashbrown makes room before it looks -- and is held to the floor's
/// slots alike: 448 ids filling 512 buckets under a floor of 2 001, 21 864
/// bytes with their box and the walks' list's room, one written again took
/// the map to 1 024 buckets, 43 624 bytes where the floor saved 24 000,
/// with no drop asked.
#[test]
fn a_write_over_an_id_below_the_floor_is_held_to_its_slots() {
    let mut r = Run::new();
    r.block(false, |r| (0..4_000).for_each(|_| _ = r.put_new()));
    let old = r.oldest(2_000);
    r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
    r.block(false, |r| (1..=448).for_each(|id| r.put(id)));
    let ix = &r.st.index;
    assert_eq!(
        std::mem::size_of::<Under>(),
        88,
        "the box the figures count"
    );
    assert_eq!((ix.floor, ix.below()), (2_001, (448, 448)));
    r.block(false, |r| r.put(5));
    assert_eq!((r.st.index.floor, r.st.index.below().0), (1, 0), "dropped");
    r.judge("written over");
}

/// A walk passes over an id below the floor deleted since their list was
/// sorted, and where the list ends with such ids, gives the dense slots
/// after it: a delete keeps the list, which a walk would else sort again.
#[test]
fn a_walk_passes_over_ids_deleted_below_the_floor() {
    let mut st = rows(1_000);
    for id in 1..=600 {
        st.append(OP_DEL, id, &[]);
    }
    st.cut_ids();
    assert_eq!(st.index.floor, 601);
    for id in [9, 3, 500, 7, 599] {
        st.append(OP_PUT, id, &payload(id, 1));
    }
    assert_eq!(st.ids()[..6], [3, 7, 9, 500, 599, 601]);
    let list = |st: &Store| {
        st.index
            .under
            .as_ref()
            .and_then(|u| u.sorted.get())
            .map(|s| s.ids.len())
    };
    assert_eq!(list(&st), Some(5), "sorted at the walk");
    // Deleted: the list kept, the ids passed over -- the last two of it
    // too, the dense slots given after them.
    for id in [7, 500, 599] {
        st.append(OP_DEL, id, &[]);
    }
    assert_eq!(list(&st), Some(5), "kept by the deletes");
    assert_eq!(st.ids()[..4], [3, 9, 601, 602]);
    assert_eq!(st.ids().len(), 402);
    let mut walk = st.index.walk(0);
    walk.by_ref().take(3).for_each(drop);
    assert_eq!(
        (walk.dense.len(), walk.next().map(|e| e.0)),
        (399, Some(602))
    );
    for (from, first) in [
        (4, 9),
        (8, 9),
        (10, 601),
        (500, 601),
        (601, 601),
        (700, 700),
    ] {
        assert_eq!(st.iter_ids_from(from).next(), Some(first), "from {from}");
    }
    // Every id below the floor deleted: the walk starts at the dense slots.
    for id in [3, 9] {
        st.append(OP_DEL, id, &[]);
    }
    assert!(st.index.under.is_none());
    assert_eq!(st.ids()[0], 601);
    // A new id lets the list go, and the next walk sorts it again.
    for id in [42, 41] {
        st.append(OP_PUT, id, &payload(id, 2));
        assert_eq!(list(&st), None, "let go by a new id");
        assert_eq!(st.iter_ids_from(41).next(), Some(id));
    }
    assert_eq!(st.ids()[..3], [41, 42, 601]);
    // A write over one of them keeps it, its location written there too.
    st.append(OP_PUT, 42, &payload(42, 3));
    assert_eq!(list(&st), Some(2));
    assert_eq!(st.raw(42).unwrap(), Some(&payload(42, 3)[..]));
    let walked = st.index.walk(0).find(|e| e.0 == 42).map(|e| e.1);
    assert_eq!(walked, st.loc(42));
}

/// A pin's copy of an index whose list of the ids below the floor is
/// sorted takes the list with it, and its walks sort nothing: let go, each
/// long read a server pinned sorted the ids again in its copy. A pin of one
/// whose list an add let go sorts its own at its first walk, and its charge
/// counts that list as it counts one taken sorted.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_pin_walks_the_list_its_index_sorted() {
    let mut st = rows(1_000);
    for id in 1..=600 {
        st.append(OP_DEL, id, &[]);
    }
    st.cut_ids();
    assert_eq!(st.index.floor, 601);
    for id in [9, 3, 500, 7, 599] {
        st.append(OP_PUT, id, &payload(id, 1));
    }
    let list = |st: &Store| {
        st.index
            .under
            .as_ref()
            .and_then(|u| u.sorted.get())
            .map(|s| s.ids.len())
    };
    assert_eq!(list(&st), None, "let go by the adds");
    let charge = st.pinned_bytes();
    // Pinned before any walk: the copy sorts its own, the index none.
    let early = st.pinned();
    assert_eq!(list(&early), None);
    assert_eq!(early.ids()[..6], [3, 7, 9, 500, 599, 601]);
    assert_eq!(list(&early), Some(5), "sorted in the copy");
    assert_eq!(list(&st), None, "not in the index");
    // Sorted by a walk of the index, the charge the same.
    assert_eq!(st.ids()[..6], [3, 7, 9, 500, 599, 601]);
    assert_eq!(list(&st), Some(5));
    assert_eq!(st.pinned_bytes(), charge, "the list counted either way");
    // A delete keeps the list, 7 emptied in it; a pin takes it so. Sorted
    // again, the copy's list would hold the four ids left.
    st.append(OP_DEL, 7, &[]);
    let pin = st.pinned();
    assert_eq!(list(&pin), Some(5), "taken with the copy");
    assert_eq!(pin.ids()[..5], [3, 9, 500, 599, 601]);
    assert_eq!(pin.iter_ids_from(4).next(), Some(9));
    assert_eq!(list(&pin), Some(5), "walked, not sorted again");
    // The copy's own: a write over 9 in the index moves none of it.
    st.append(OP_PUT, 9, &payload(9, 2));
    assert_eq!(pin.raw(9).unwrap(), Some(&payload(9, 1)[..]));
    assert_eq!(st.raw(9).unwrap(), Some(&payload(9, 2)[..]));
    for (id, l) in pin.index.walk(0) {
        assert_eq!(Some(l), pin.loc(id), "id {id}'s location in the pin's walk");
    }
    for (id, l) in st.index.walk(0) {
        assert_eq!(Some(l), st.loc(id), "id {id}'s location in the index's walk");
    }
}

/// The oracle the tests hold the index's bytes to reads a map's own
/// allocation, tombstones and all: a map grown, deleted from and written
/// again, its bytes counted as it was built, against its clone's, and read
/// again where it moved.
#[test]
fn the_oracle_reads_a_maps_own_bytes() {
    let mut g = Rng(0x5eed);
    let mut live: Vec<DocId> = Vec::with_capacity(40_000);
    let l = Loc {
        seg: 0,
        off: 0,
        len: 1,
    };
    let mut map: HashMap<DocId, Loc> = HashMap::default();
    // The readings' own list allocated before the count starts.
    map_real(&map);
    let before = HELD.with(Cell::get);
    for step in 0..40_000u64 {
        match live.is_empty() || !g.chance(3) {
            true => {
                let id = 1 + g.below(1 << 40);
                if map.insert(id, l).is_none() {
                    live.push(id);
                }
            }
            false => {
                let id = live.swap_remove(g.below(live.len() as u64) as usize);
                map.remove(&id);
            }
        }
        if step % 31 == 0 {
            let own = HELD.with(Cell::get).wrapping_sub(before);
            assert_eq!(asked(|| map.clone()), own, "step {step}");
            assert_eq!(map_real(&map), own, "step {step}, read again");
        }
    }
}

/// The sparse map's floor is main's, whatever order its ids come in: an
/// id within the gap of the end, its sparse ids above its reach, takes
/// none over; one reaching them takes the run; a map emptied takes its
/// next id's as its floor.
#[test]
fn the_sparse_map_keeps_mains_floor() {
    let mut r = Run::new();
    r.block(false, |r| (0..5_000).for_each(|_| _ = r.put_new()));
    // Past the gap from the end, apart: the map's.
    for id in [12_000, 10_000, 30_000, 14_000] {
        r.block(false, |r| r.put(id));
    }
    // Within the gap of the end, below the map's floor: no take-over,
    // though 10 000 lies within the gap of it.
    r.block(false, |r| r.put(6_000));
    assert_eq!(r.st.index.sparse.len(), 4);
    r.block(false, |r| r.put(9_000));
    assert_eq!(r.st.index.sparse.len(), 4);
    // Reaching the map's floor: the run within the gap of each other
    // taken, 30 000 left, the map's floor past them.
    r.block(false, |r| r.put(10_500));
    assert_eq!(
        (r.st.index.sparse.len(), r.st.index.sparse_floor),
        (1, 30_000)
    );
    // Emptied, and a far id above where its floor was: its floor.
    r.block(false, |r| r.del(30_000));
    r.block(false, |r| r.put(50_000));
    r.block(false, |r| r.put(40_000));
    assert_eq!(r.st.index.sparse_floor, 40_000);
    // Behind a raised floor alike.
    let old: Vec<DocId> = r
        .model
        .live
        .keys()
        .copied()
        .filter(|&id| id < 12_000)
        .collect();
    r.block(false, |r| old.into_iter().for_each(|id| r.del(id)));
    r.block(false, |r| r.del(40_000));
    assert_eq!(r.st.index.floor, 12_000, "the block's first write cut");
    r.block(false, |r| r.del(50_000));
    r.block(false, |r| r.put(60_000));
    r.block(false, |r| r.put(51_000));
    assert_eq!(r.st.index.sparse_floor, 51_000);
    r.block(false, |r| r.put(18_000));
    r.judge("map's floor");
}

mod database {
    use super::*;
    use crate::engine::Database;
    use crate::query::{CmpOp, Expr, Statement};
    use crate::schema::{Field, Schema};
    use crate::value::{DataType, Value};

    fn made(db: &mut Database) {
        db.execute(&Statement::CreateCollection {
            schema: Schema::new("s", vec![Field::new("n", DataType::Int)]).unwrap(),
            if_not_exists: false,
        })
        .unwrap();
    }

    fn put(db: &mut Database, id: Option<DocId>) {
        let mut doc = vec![("n".to_string(), Expr::Lit(Value::Int(1)))];
        if let Some(id) = id {
            doc.push(("id".to_string(), Expr::Lit(Value::Int(id as i64))));
        }
        db.execute(&Statement::Put {
            collection: "s".into(),
            docs: vec![doc],
            docs_param: None,
            insert: false,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        })
        .unwrap();
    }

    fn del(db: &mut Database, id: DocId) {
        let by_id = Expr::Cmp(
            CmpOp::Eq,
            Box::new(Expr::Field("id".into())),
            Box::new(Expr::Lit(Value::Int(id as i64))),
        );
        db.execute(&Statement::Delete {
            collection: "s".into(),
            filter: Some(by_id),
            require: None,
            pick: None,
        })
        .unwrap();
    }

    fn ix(db: &Database) -> &IdIndex {
        &db.collection("s").unwrap().store.index
    }

    fn filled(db: &mut Database, n: u64) {
        made(db);
        for _ in 0..n {
            put(db, None);
        }
    }

    /// Deletes in a block move no floor while it is open, nor once it has
    /// landed: the block after it does, as it first writes.
    #[test]
    fn no_cut_is_made_inside_a_block() {
        let mut db = Database::new();
        filled(&mut db, 1_000);
        db.begin().unwrap();
        for id in 1..=700 {
            del(&mut db, id);
        }
        assert_eq!(ix(&db).floor, 1, "a block open");
        put(&mut db, None);
        assert_eq!(ix(&db).floor, 1, "its own write after");
        db.commit().unwrap();
        assert_eq!(ix(&db).floor, 1, "landed");
        put(&mut db, None);
        assert_eq!(
            (ix(&db).floor, ix(&db).sparse.len()),
            (701, 0),
            "the next block's"
        );
    }

    /// A block put back after deletes that leave the first half empty
    /// points them back in the array, and no cut follows.
    #[test]
    fn a_block_put_back_after_deletes_that_would_have_cut_keeps_its_ids_dense() {
        let mut db = Database::new();
        filled(&mut db, 1_000);
        db.begin().unwrap();
        // Oldest last, as a refused `del s where id <= 900 order id desc`.
        for id in (1..=900).rev() {
            del(&mut db, id);
        }
        db.rollback();
        assert_eq!(
            (ix(&db).floor, ix(&db).sparse.len(), ix(&db).len()),
            (1, 0, 1_000)
        );
        put(&mut db, None);
        assert_eq!(
            (ix(&db).floor, ix(&db).sparse.len(), ix(&db).len()),
            (1, 0, 1_001)
        );
    }

    /// Every record a primary appends, in order.
    #[derive(Clone, Default)]
    struct Records(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

    impl crate::engine::Sink for Records {
        fn append(&mut self, bytes: &[u8]) -> crate::error::Result<()> {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
        fn rewrite(&mut self, _: &[u8]) -> crate::error::Result<()> {
            Ok(())
        }
    }

    /// A replica cuts at a record's first frame for a collection, never at
    /// a later one: a record whose frames empty the first half and that is
    /// then refused puts its deletes back into the array, with the floor
    /// where it was. Cut at the frame after the deletes, the put-back
    /// pointed a deleted id below the floor.
    #[test]
    fn a_replica_cuts_at_no_frame_inside_a_record() {
        const REC_DATA: u8 = 3;
        let sink = Records::default();
        let mut primary = Database::with_sink(Box::new(sink.clone()));
        filled(&mut primary, 1_000);
        let mut replica = Database::new();
        replica.apply(&sink.0.lock().unwrap().concat()).unwrap();
        let cid = replica.collection("s").unwrap().id;
        let frames = |ops: &[(u8, DocId)]| {
            let body: Vec<u8> = ops
                .iter()
                .flat_map(|&(op, id)| Store::frame(op, id, &[]))
                .collect();
            crate::engine::record(REC_DATA, cid, &body)
        };
        // Lands: ids 1 to 499 deleted, id 500 the first half's last.
        let oldest: Vec<(u8, DocId)> = (1..=499).map(|id| (OP_DEL, id)).collect();
        replica.apply(&frames(&oldest)).unwrap();
        assert_eq!(ix(&replica).floor, 1);
        // Id 500 deleted, a frame after it, then one no write makes: the
        // record is put back whole.
        let refused = frames(&[(OP_DEL, 500), (OP_DEL, 900), (9, 901)]);
        assert!(replica.apply(&refused).is_err());
        let ix = ix(&replica);
        assert_eq!(
            (ix.floor, ix.below().0, ix.len()),
            (1, 0, 501),
            "put back below a floor"
        );
        assert!(replica.collection("s").unwrap().store.contains(500));
    }

    /// An open lays the array out from 1 and cuts it to its lowest id,
    /// taking back in an id written below the floor; a compact keeps the
    /// layout, and an open of its image puts ids past the gap from 1 in the
    /// map, as main's did.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    #[test]
    fn an_open_lays_the_array_out_from_the_lowest_id_and_a_compacts_open_as_mains() {
        let scratch = crate::engine::unwinding::Scratch::new("fennecdb-id-floor");
        let path = scratch.0.join("q.fennec");
        let below = {
            let mut db = crate::fs::open(&path).unwrap();
            made(&mut db);
            // A queue through 20 000 ids, 100 rows held.
            for k in 1..=20_000u64 {
                put(&mut db, None);
                if k > 100 {
                    del(&mut db, k - 100);
                }
            }
            assert!(ix(&db).floor > 19_000 && ix(&db).dense.len() <= 2 * CUT_FROM);
            // One written below the floor.
            let below = ix(&db).floor - 1;
            put(&mut db, Some(below));
            assert_eq!(ix(&db).below().0, 1);
            db.sync().unwrap();
            below
        };
        // The file replayed write by write spans every id it gave: the open
        // lays it out from the lowest, the one below the floor in it.
        let mut db = crate::fs::open(&path).unwrap();
        assert_eq!(
            (ix(&db).floor, ix(&db).below().0, ix(&db).sparse.len()),
            (below, 0, 0)
        );
        assert_eq!(ix(&db).dense.len() as u64, 20_000 - below + 1);
        assert_eq!(ix(&db).len(), 101);
        // Compacted: the layout kept. Opened from its image's index: past
        // the gap from 1, every id in the map, as main's open put them.
        db.execute(&Statement::Compact(None)).unwrap();
        assert_eq!((ix(&db).floor, ix(&db).sparse.len()), (below, 0));
        drop(db);
        let db = crate::fs::open(&path).unwrap();
        assert_eq!(
            (ix(&db).floor, ix(&db).dense.len(), ix(&db).sparse.len()),
            (1, 0, 101)
        );
    }
}
