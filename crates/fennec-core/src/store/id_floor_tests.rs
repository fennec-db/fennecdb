//! The id index's floor (`IdIndex::cut`, `IdIndex::rebuild`), judged by a
//! model that knows nothing of the index: the live ids, their payloads, and
//! the next id `allocate_id` gives. Main's index -- the array from id 1, as
//! it was before the floor -- is laid out beside it from the same writes,
//! for the memory the floor is held under.

use super::*;

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

/// Main's id index, as it stood before the floor: the array from id 1 to
/// the highest, the sparse map past it, the same gap. Its code as it was,
/// so that its allocations are what main's were.
#[derive(Default, Clone)]
struct MainIds {
    dense: Vec<Loc>,
    sparse: HashMap<DocId, Loc>,
    sparse_floor: DocId,
    /// The most ids its map held at once: what its buckets are sized by.
    peak: usize,
}

impl MainIds {
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
                let mut keys: Vec<DocId> = self.sparse.keys().copied().collect();
                keys.sort_unstable();
                let mut end = id;
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
            self.dense[id as usize - 1] = loc;
            return;
        }
        if self.sparse.is_empty() || id < self.sparse_floor {
            self.sparse_floor = id;
        }
        self.sparse.insert(id, loc);
        self.peak = self.peak.max(self.sparse.len());
    }

    fn remove(&mut self, id: DocId) {
        if self.in_dense(id) {
            self.dense[id as usize - 1] = Loc::EMPTY;
            return;
        }
        self.sparse.remove(&id);
    }

    fn point(&mut self, id: DocId, loc: Option<Loc>) {
        match loc {
            Some(l) => self.insert(id, l),
            None => self.remove(id),
        }
    }

    fn is_dense(&self, id: DocId) -> bool {
        self.in_dense(id)
    }

    /// Main's open of an image's index read in turn (`adopt_in_turn` as it
    /// was): room for the documents reserved, each pushed in id order.
    fn opened(ids: &[(DocId, Loc)], len: u64) -> MainIds {
        let mut m = MainIds::default();
        m.dense.reserve((ids.len() as u64).min(len / 3) as usize);
        for &(id, l) in ids {
            match id == m.dense.len() as u64 + 1 && m.sparse.is_empty() {
                true => m.dense.push(l),
                false => m.insert(id, l),
            }
        }
        m
    }

    fn bytes(&self) -> usize {
        held(self.dense.len(), self.peak)
    }
}

/// The bytes an id index holds: its array's slots, and its map's buckets
/// -- a `(DocId, Loc)` and a control byte each -- as many as the most ids
/// it held at once take, which no delete gives back. The slots, not the
/// array's allocation, which each side doubles from where it was last
/// allocated whole: an array of 175 slots reallocated at a reseed had
/// doubled to 256 where main's, never reallocated, had doubled from an
/// odd length to 200. Nor the map's own `capacity`, its ids and the room
/// left before it grows, which its deletes' tombstones take from as its
/// hasher's random keys place them: two maps of the same ids said 108 and
/// 112.
fn held(slots: usize, peak: usize) -> usize {
    slots * std::mem::size_of::<Loc>() + buckets(peak) * 25
}

/// The buckets a map takes for `peak` ids, as hashbrown sizes them.
fn buckets(peak: usize) -> usize {
    match peak {
        0 => 0,
        1..=3 => 4,
        4..=7 => 8,
        n => (n * 8).div_ceil(7).next_power_of_two(),
    }
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
    /// Main's index as main has it: laid out again from id 1 at each open,
    /// where a file whose lowest id lies past the gap put every row in the
    /// sparse map.
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
    /// The most ids the store's maps held at once -- the sparse one and
    /// `under` -- and their room as last seen: less, and one let its
    /// buckets go.
    peak: [usize; 2],
    room: [usize; 2],
    /// How many ids were written below the floor.
    belows: usize,
    /// Where main's open put every row in the map: the most the store held
    /// over main's index, and what it was.
    over: (f64, String),
}

impl Run {
    fn new() -> Run {
        Run {
            st: Store::new(),
            main: MainIds::default(),
            model: Model {
                live: BTreeMap::new(),
                next_id: 1,
            },
            version: 0,
            block: None,
            wrote: false,
            news: Default::default(),
            cuts: 0,
            peak: [0; 2],
            room: [0; 2],
            belows: 0,
            over: (0.0, String::new()),
        }
    }

    fn begin(&mut self) {
        assert!(self.block.is_none());
        let below = self.st.index.under.len();
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
        self.st.append(OP_PUT, id, &payload(id, self.version));
        let l = self.st.loc(id).unwrap();
        self.main.insert(id, l);
        self.model.live.insert(id, self.version);
        self.peaks();
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
            self.peaks();
            self.main.point(id, loc);
        }
        self.st.rewind(mark);
        self.model = model;
        // No cut saw the block's deletes: it put back no id below the floor
        // -- fewer there, where the block's writes below it dropped it.
        assert!(
            self.st.index.under.len() <= below,
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
    /// with no image index is.
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
        fresh.rebuild_ids();
        let ids: Vec<(DocId, Loc)> = fresh.index.walk(0).collect();
        self.main = match walked {
            true => {
                let mut m = MainIds::default();
                for &(id, l) in &ids {
                    m.insert(id, l);
                }
                m
            }
            false => MainIds::opened(&ids, len),
        };
        self.st = fresh;
        (self.peak, self.room) = ([0; 2], [0; 2]);
        self.peaks();
    }

    /// Each map's most ids, since it last let its room go.
    fn peaks(&mut self) {
        let ix = &self.st.index;
        for (k, m) in [&ix.sparse, &ix.under].into_iter().enumerate() {
            if m.capacity() < self.room[k] {
                self.peak[k] = m.len();
            }
            self.room[k] = m.capacity();
            self.peak[k] = self.peak[k].max(m.len());
        }
    }

    /// The bytes the store's index holds, as [`held`] counts them.
    fn real(&self) -> usize {
        held(self.st.index.dense.len(), self.peak[0]) + buckets(self.peak[1]) * 25
    }

    /// The store pointed at an image of itself, as a checkpoint (`live`
    /// false) or a compact does; main's index relocated in place.
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
        match live {
            true => self.st.relocate_live(&base, at),
            false => self.st.relocate_image(&base, at),
        }
        self.peaks();
        // Main's relocation rewrote each location where it was: its layout
        // is as it stood. Inserted again, an id its map held within the
        // gap of its end grew its array.
    }

    /// Everything the model says, judged.
    fn judge(&mut self, what: &str) {
        judge_store(&self.st, &self.model, what);
        let ix = &self.st.index;
        let shown = |m: &MainIds| {
            format!(
                "{} bytes where main's held {} (array {} of {} at floor {}, map {} with {} \
                 below; main's array {} of {}, map {} of {})",
                self.real(),
                m.bytes(),
                ix.dense.len(),
                ix.dense.capacity(),
                ix.floor,
                ix.sparse.len(),
                ix.under.len(),
                m.dense.len(),
                m.dense.capacity(),
                m.sparse.len(),
                m.sparse.capacity()
            )
        };
        // Memory never above main's for the same writes, wherever main's
        // array spans the store's: the ids written below the floor too,
        // which the floor's drop holds under the slots it saved. Where the
        // store's array ends past main's -- laid by an open from a lowest
        // id main's array never reached, or over rows main's end's gap
        // left in its map -- main's layout is another, its rows in one
        // map, and the store's is measured, not judged.
        let real = self.real();
        let main = self.main.bytes();
        let end = ix.floor - 1 + ix.dense.len() as u64;
        match end <= self.main.dense.len() as u64 {
            true => assert!(real <= main, "{what}: {}", shown(&self.main)),
            false => {
                let r = real as f64 / main as f64;
                if r > self.over.0 {
                    self.over = (
                        r,
                        format!(
                            "{what}: {} rows, {}",
                            self.model.live.len(),
                            shown(&self.main)
                        ),
                    );
                }
            }
        }
        // A new id lands where main's does: in the array where its is.
        for &id in &self.news {
            if self.model.live.contains_key(&id) && self.main.is_dense(id) {
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
    for from in [0, 1, want.len() as u64 / 2, 1 << 52]
        .into_iter()
        .chain(want.iter().step_by(97).copied())
        .chain(want.iter().step_by(89).map(|i| i + 1))
    {
        let got: Vec<DocId> = st.iter_ids_from(from).collect();
        let at = want.partition_point(|&i| i < from);
        assert_eq!(got, want[at..], "{what}: from {from}");
    }
    let ix = &st.index;
    assert_eq!(ix.front, ix.held(half(ix.dense.len())), "{what}: front");
    let end = ix.floor - 1 + ix.dense.len() as u64;
    for &k in ix.sparse.keys() {
        assert!(k > end, "{what}: sparse id {k} not past the end {end}");
    }
    for &k in ix.under.keys() {
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
    match g.below(15) {
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
/// workloads make, the store, the model and main's index driven alike and
/// judged after each (`Run::judge`). `FENNEC_ID_FLOOR_SEEDS=600` runs
/// more, and says the most the store held over main's where main's layout
/// is another.
#[test]
fn the_id_index_holds_every_workload_under_mains_memory_and_reads_back() {
    let seeds: u64 = std::env::var("FENNEC_ID_FLOOR_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let mut over = (0.0, String::new());
    let (mut cuts, mut belows) = (0, 0);
    for seed in 1..=seeds {
        let mut g = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut r = Run::new();
        for p in 0..60 {
            let what = phase(&mut r, &mut g);
            r.judge(&format!("seed {seed}, phase {p} ({what})"));
        }
        cuts += r.cuts;
        if r.over.0 > over.0 {
            over = r.over;
        }
        belows += r.belows;
    }
    assert!(cuts > seeds as usize, "the phases cut: {cuts}");
    assert!(
        belows > seeds as usize,
        "the phases wrote below the floor: {belows}"
    );
    if std::env::var_os("FENNEC_ID_FLOOR_SEEDS").is_some() {
        eprintln!("{cuts} cuts, {belows} ids written below the floor");
        eprintln!("over main's map, at most {:.2}x: {}", over.0, over.1);
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
    let room = st.index.dense.capacity();
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
    assert_eq!(st.index.dense.capacity(), room, "the allocation kept");
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
    // Every slot cut once: a queue's array stays twice its rows.
    for _ in 0..100_000 {
        st.cut_ids();
        let id = st.allocate_id();
        st.append(OP_PUT, id, &payload(id, 0));
        let oldest = st.iter_ids().next().unwrap();
        st.append(OP_DEL, oldest, &[]);
    }
    assert_eq!(st.len(), 501);
    assert!(st.index.dense.len() <= 1_002 && st.index.dense.capacity() == room);
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
        (st.index.floor, st.index.under.len(), st.index.sparse.len()),
        (601, 3, 0)
    );
    for id in [7, 599, 600] {
        assert_eq!(st.raw(id).unwrap(), Some(&payload(id, 1)[..]));
    }
    let ids = st.ids();
    assert_eq!(ids[..4], [7, 599, 600, 601]);
    assert_eq!(
        st.iter_ids_from(8).take(3).collect::<Vec<_>>(),
        [599, 600, 601]
    );
    assert_eq!(st.iter_ids_from(600).next(), Some(600));
    // A rebuild takes in those within the gap of the floor, from it down;
    // 7 is within the gap of 599.
    st.rebuild_ids();
    assert_eq!(
        (st.index.floor, st.index.under.len(), st.index.sparse.len()),
        (7, 0, 0)
    );
    assert_eq!(st.ids(), ids);
    // One far below stays the map's, as one far past the end does.
    let mut far = rows(10_000);
    for id in 1..=9_000 {
        far.append(OP_DEL, id, &[]);
    }
    far.cut_ids();
    far.append(OP_PUT, 3, &payload(3, 1));
    far.append(OP_PUT, 8_000, &payload(8_000, 1));
    far.rebuild_ids();
    assert_eq!((far.index.floor, far.index.under.len()), (8_000, 1));
    assert_eq!(far.raw(3).unwrap(), Some(&payload(3, 1)[..]));
}

/// An open of an image's index lays its ids out from the lowest -- a
/// queue's, past the gap, which main put in its map -- where the array
/// holds one in two slots at least; ids spread wider stay in the map, as
/// main's open put them.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_open_lays_ids_past_the_gap_out_from_the_lowest_where_they_fill_the_array() {
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
        for &id in ids {
            assert_eq!(fresh.raw(id).unwrap(), Some(&payload(id, 0)[..]));
        }
        fresh
    };
    let queue: Vec<DocId> = (1_000_000..1_003_000).collect();
    let st = opened(&queue);
    assert_eq!(
        (st.index.floor, st.index.dense.len(), st.index.sparse.len()),
        (1_000_000, 3_000, 0)
    );
    // Every other id: one slot in two held, still the array's.
    let halves: Vec<DocId> = (1_000_000..1_006_000).step_by(2).collect();
    assert_eq!(opened(&halves).index.sparse.len(), 0);
    // Times in seconds an hour apart: each in the map.
    let hours: Vec<DocId> = (0..1_000).map(|k| 1_700_000_000 + k * 3_600).collect();
    let st = opened(&hours);
    assert_eq!(
        (st.index.floor, st.index.dense.len(), st.index.sparse.len()),
        (1, 0, 1_000)
    );
    // Within the gap of 1, they lie in the array main's did too.
    let low: Vec<DocId> = (0..100).map(|k| 1 + k * 3).collect();
    assert_eq!(opened(&low).index.sparse.len(), 0);
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

    /// An open and a compact lay the array out from the lowest id, and
    /// take back in an id written below the floor.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    #[test]
    fn an_open_and_a_compact_lay_the_array_out_from_the_lowest_id() {
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
            assert_eq!(ix(&db).under.len(), 1);
            db.sync().unwrap();
            below
        };
        // The file replayed write by write spans every id it gave: the open
        // lays it out from the lowest, the one below the floor in it.
        let mut db = crate::fs::open(&path).unwrap();
        assert_eq!(
            (ix(&db).floor, ix(&db).under.len(), ix(&db).sparse.len()),
            (below, 0, 0)
        );
        assert_eq!(ix(&db).dense.len() as u64, 20_000 - below + 1);
        assert_eq!(ix(&db).len(), 101);
        // Compacted, and opened from its image's index: the same.
        db.execute(&Statement::Compact(None)).unwrap();
        assert_eq!((ix(&db).floor, ix(&db).sparse.len()), (below, 0));
        drop(db);
        let db = crate::fs::open(&path).unwrap();
        assert_eq!(
            (ix(&db).floor, ix(&db).sparse.len(), ix(&db).len()),
            (below, 0, 101)
        );
        assert!(ix(&db).dense.capacity() <= 2 * ix(&db).dense.len());
    }
}
