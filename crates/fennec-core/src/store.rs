//! Append-only segment store -- *no buffer pool*.
//!
//! In classic databases disk pages are copied into a buffer pool in user
//! space; consistency, eviction policy and locking cost all come from
//! there. fennecdb skips that entirely:
//!
//! - Segments are *immutable*. A write only appends to the end of the
//!   active segment.
//! - The byte sequence on disk and the byte sequence in memory are in the
//!   same format. There is therefore no "caching" stage; a read decodes
//!   straight off the arena slice.
//! - The only auxiliary structure is the offset index from id to location.
//!
//! The result: no eviction policy, no dirty pages, no checkpoint.

use crate::codec::*;
use crate::error::{Error, Result};
use crate::schema::Schema;
use crate::value::{DocId, Document, Value};
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;

#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
mod in_turn;
#[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
pub use in_turn::{assume as assume_in_turn, break_scratch, copied, fell_back, Broken, InTurn};

pub const OP_PUT: u8 = 1;
pub const OP_DEL: u8 = 2;

/// Once the active segment passes this size it is sealed and a new one opens.
pub const SEGMENT_MAX: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub seg: u32,
    /// Start of the payload inside the segment (past the frame header).
    pub off: u32,
    pub len: u32,
}

impl Loc {
    const EMPTY: Loc = Loc {
        seg: u32::MAX,
        off: 0,
        len: 0,
    };
    #[inline]
    fn is_empty(&self) -> bool {
        self.seg == u32::MAX
    }

    /// A payload left in the mapped file, at byte `at` of it: the top bit of
    /// `seg` says so, and its other 31 bits are the offset's high word, so a
    /// location stays 12 bytes however large the file.
    fn mapped(at: u64, len: u32) -> Loc {
        Loc {
            seg: MAPPED | (at >> 32) as u32,
            off: at as u32,
            len,
        }
    }
}

/// The bit of [`Loc::seg`] that marks a payload in the mapped file.
const MAPPED: u32 = 1 << 31;

/// Where a location lands in an image of its store's frames as they stand,
/// given where each stretch and segment starts in it
/// ([`Store::image_starts`]).
fn image_place(l: Loc, stretches: &[(u64, u64)], segments: &[u64]) -> u64 {
    match l.seg & MAPPED != 0 {
        true => {
            let from = ((l.seg & !MAPPED) as u64) << 32 | l.off as u64;
            let k = stretches.partition_point(|s| s.0 <= from) - 1;
            stretches[k].1 + (from - stretches[k].0)
        }
        false => segments[l.seg as usize] + l.off as u64,
    }
}

/// The head of a put's frame of `len` bytes to `id`: its op and two
/// varints, each as [`put_uvarint`] writes it, the shortest.
#[inline(always)]
fn head_len(id: DocId, len: u64) -> u64 {
    1 + varint_len(id) + varint_len(len)
}

/// The bytes [`put_uvarint`] writes `v` in, a byte for each 7 of its bits:
/// `(log2 * 9 + 73) / 64`, protobuf's `VarintSize64`, an add, a shift and
/// no divide -- 6 instructions on aarch64, where `bits.div_ceil(7)` took 10.
/// A put over a row, which counts its head into the version it leaves dead
/// ([`dead_frame`]), took 29 instructions more than one that counted the
/// payload alone with the divide, 14 with this (`make instr-compare
/// TRACE=fennec_http::handle`, the 530th put of `replays`).
#[inline(always)]
fn varint_len(v: u64) -> u64 {
    ((63 - (v | 1).leading_zeros() as u64) * 9 + 73) / 64
}

/// Where the payload of a put of `len` bytes to `id` starts, framed right
/// after `end`: past the frame's head.
#[cfg(not(target_arch = "wasm32"))]
fn after(end: u64, id: DocId, len: u64) -> u64 {
    end + head_len(id, len)
}

/// The bytes the version of `id` at `old` takes, head and payload: what a
/// write over it or a delete of it leaves dead ([`Store::dead_bytes`]).
/// Its payload's length alone, as was counted, left a head of 3 bytes or
/// more a version uncounted, so that `total_bytes - dead_bytes` -- the live
/// records `/_metrics` says, and those a compact is due against -- was the
/// live records and every dead version's head: 250 rows of 3 409 bytes,
/// 200 of them written over three times and 50 deleted, read as 5 628
/// (fennec-server's `the_data_bytes_are_the_live_records`). Natively in
/// the write's own code; the browser module holds it once, which grew it 91
/// bytes, where a copy in each of its three callers grew it 141.
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
#[cfg_attr(target_arch = "wasm32", inline(never))]
fn dead_frame(id: DocId, old: Loc) -> usize {
    (head_len(id, old.len as u64) + old.len as u64) as usize
}

/// The first byte of an image's index of a data record, after the id
/// counter in the counter record before it ([`Store::image_index`]). A
/// binary that does not know it reads the counter alone, as every reader
/// of the record did.
pub const FRAMES_MARK: u8 = 1;

/// The first byte of an image's index that says, behind its documents,
/// where each `INDEX_SHARE` of them starts ([`Store::image_index`]), for an
/// open to read the shares on every core. A binary that knows only
/// [`FRAMES_MARK`] walks the record, as one before either did.
pub const FRAMES_SPLIT: u8 = 2;

/// The bytes a store reads records from without having copied them: a file
/// the operating system maps in on native targets (`fs::open_mapped`), and
/// in the browser the image a load was handed (`fennec_load_owned`), kept
/// rather than copied a document at a time into segments -- 10 000 rows of
/// 768 dimensions restored held 97 MB that way, and hold 66. A concrete
/// `Vec` there: through `dyn AsRef` the module was 126 bytes larger.
#[cfg(not(target_arch = "wasm32"))]
pub type Base = std::sync::Arc<dyn AsRef<[u8]> + Send + Sync>;
#[cfg(target_arch = "wasm32")]
pub type Base = std::sync::Arc<Vec<u8>>;

/// Documents looked up under a lock and read after it
/// ([`Store::snapshot`]): each one's place in the mapped file, or its
/// payload copied out of memory, or nothing for one deleted.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub struct Snapshot {
    base: Option<Base>,
    held: Vec<u8>,
    docs: Vec<(DocId, Place)>,
}

#[cfg(not(target_arch = "wasm32"))]
enum Place {
    File(u64, u32),
    Held(usize, u32),
    Gone,
}

/// The value at `place` of a payload a [`Snapshot`] handed back, read as
/// [`Store::read_field`] reads it -- `null` where the payload ends before
/// it -- for a catch-up that looks its documents up under a lock and reads
/// them after it. `place` is past the dropped places before the field
/// (`Schema::place`), as the payload holds them.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn field_at(buf: &[u8], place: usize) -> Result<Value> {
    let mut pos = 0;
    for _ in 0..place {
        skip_field(buf, &mut pos)?;
    }
    match pos < buf.len() {
        true => decode_value(buf, &mut pos),
        false => Ok(Value::Null),
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Snapshot {
    /// Each document in the order it was asked for, its payload or `None`.
    pub fn each(&self, mut f: impl FnMut(DocId, Option<&[u8]>) -> Result<()>) -> Result<()> {
        let file: &[u8] = self.base.as_ref().map_or(&[], |b| (**b).as_ref());
        for (id, place) in &self.docs {
            let payload = match *place {
                Place::File(at, len) => Some(
                    file.get(at as usize..at as usize + len as usize)
                        .ok_or_else(|| Error::Corrupt("offset outside the mapped file".into()))?,
                ),
                Place::Held(from, len) => Some(&self.held[from..from + len as usize]),
                Place::Gone => None,
            };
            f(*id, payload)?;
        }
        Ok(())
    }
}

/// A varint of an image's index: a byte apiece for most of what one holds
/// -- an id's step, a place where the frame before put it -- so that byte
/// is read inline. Through `get_uvarint`, a call apiece, and an insert into
/// the id index asking its slot first, a million documents took 9.5 ms.
#[cfg(not(target_arch = "wasm32"))]
#[inline(always)]
fn uvarint(bytes: &[u8], p: &mut usize) -> Option<u64> {
    match bytes.get(*p) {
        Some(&b) if b < 0x80 => {
            *p += 1;
            Some(b as u64)
        }
        _ => get_uvarint(bytes, p).ok(),
    }
}

/// The next document of an image's index, from `p` -- its id moved on from
/// `id` and its payload's end from `end` -- as the place and length of its
/// payload in a record `len` bytes long; `None` where the index cannot
/// describe such a record.
#[cfg(not(target_arch = "wasm32"))]
#[inline(always)]
fn index_entry(
    docs: &[u8],
    p: &mut usize,
    id: &mut DocId,
    end: &mut u64,
    len: u64,
) -> Option<(u64, u64)> {
    let (step, n, off) = (uvarint(docs, p)?, uvarint(docs, p)?, uvarint(docs, p)?);
    // Ascending from 0 a step past 0 at a time, so no id is 0; `DocId::MAX`
    // is no write's either (`written_id`): taken, the store's next id was
    // one past it, which overflowed, where the walk the open goes on to
    // refuses the frame putting it.
    *id = id
        .checked_add(step)
        .filter(|&i| step > 0 && i != DocId::MAX && n <= u32::MAX as u64)?;
    let pos = (after(*end, *id, n) as i64)
        .checked_add(crate::codec::unzigzag(off))
        .filter(|&p| p > 0)? as u64;
    *end = pos.checked_add(n).filter(|&e| e <= len)?;
    Some((pos, n))
}

/// An image's index from `p` on, a document after another: the id index
/// it gives the record `len` bytes at `at`, and its last id.
#[cfg(not(target_arch = "wasm32"))]
fn adopt_in_turn(
    docs: &[u8],
    mut p: usize,
    count: u64,
    at: u64,
    len: u64,
) -> Option<(IdIndex, DocId)> {
    let mut fresh = IdIndex::default();
    // A frame is three bytes at the least: a count past that is not this
    // record's, and is not reserved for.
    fresh.reserve(count.min(len / 3) as usize);
    let (mut id, mut end) = (0, 0);
    for _ in 0..count {
        let (pos, n) = index_entry(docs, &mut p, &mut id, &mut end, len)?;
        fresh.push_above(id, Loc::mapped(at + pos, n as u32));
    }
    (p == docs.len()).then_some((fresh, id))
}

/// Documents of an image's index an open reads at a time on a thread.
#[cfg(not(target_arch = "wasm32"))]
const INDEX_SHARE: usize = 16_384;

/// [`adopt_in_turn`] a share of [`INDEX_SHARE`] documents at a time by
/// whichever thread is free, each from where `ends` says the one before it
/// ended, into its own stretch of the dense array, which starts at the
/// first document's id: `None` where the shares do not read as `ends` says,
/// or the ids would leave the dense array a quarter empty or put one in the
/// sparse map -- read in turn then, as `insert` would place them. In turn a
/// million documents took 4.8 ms.
#[cfg(not(target_arch = "wasm32"))]
fn adopt_shares(
    docs: &[u8],
    from: usize,
    ends: &[u8],
    count: u64,
    at: u64,
    len: u64,
) -> Option<(IdIndex, DocId)> {
    let mut marks = vec![[from as u64, 0, 0]];
    for e in ends.as_chunks::<24>().0 {
        let (w, _) = e.as_chunks::<8>();
        marks.push([0, 1, 2].map(|i| u64::from_le_bytes(w[i])));
    }
    let shares = marks.len() - 1;
    let [stop, top, _] = marks[shares];
    // The array's floor, the first document's id -- its step from 0, which
    // its share reads again in full -- read ahead: the ids an image no
    // longer holds below it take no slot.
    let floor = uvarint(docs, &mut { from }).filter(|&f| f > 0)?;
    let span = top.checked_sub(floor - 1)?;
    if shares as u64 != count.div_ceil(INDEX_SHARE as u64)
        || count > docs.len() as u64 / 3
        || stop != docs.len() as u64
        || span > count + count / 4
        || marks
            .windows(2)
            .any(|w| w[1][0] <= w[0][0] || w[1][1] <= w[0][1])
    {
        return None;
    }
    // The id before a share's first slot: the one before its first
    // document, the floor's for the first share, whose ids count from 0.
    let below = |m: [u64; 3]| m[1].max(floor - 1);
    let mut dense: Vec<Loc> = Vec::with_capacity(span as usize);
    let mut parts = Vec::with_capacity(shares);
    let mut rest = &mut dense.spare_capacity_mut()[..span as usize];
    for w in marks.windows(2) {
        let (part, after) = rest.split_at_mut(below(w[1]).checked_sub(below(w[0]))? as usize);
        parts.push(Mutex::new(part));
        rest = after;
    }
    let threads = crate::threads();
    let mut states = vec![(); threads.min(shares)];
    let done = crate::vector::spread(shares, &mut states, |_, i| {
        let mut part = parts[i].lock().unwrap_or_else(|e| e.into_inner());
        let ([p, first, mut end], next) = (marks[i], marks[i + 1]);
        let base = below(marks[i]);
        let (mut p, mut id, mut was, mut filled) = (p as usize, first, base, 0);
        let docs_here = (count - (i * INDEX_SHARE) as u64).min(INDEX_SHARE as u64);
        for _ in 0..docs_here {
            let (pos, n) = index_entry(docs, &mut p, &mut id, &mut end, len)?;
            // At or below the id before the share's first slot only where
            // `ends` says wrong: a share counting from below the floor.
            if id <= was || id - was > MAX_DENSE_GAP {
                return None;
            }
            was = id;
            let slot = (id - base - 1) as usize;
            for gap in part.get_mut(filled..slot)? {
                gap.write(Loc::EMPTY);
            }
            part.get_mut(slot)?.write(Loc::mapped(at + pos, n as u32));
            filled = slot + 1;
        }
        let whole = filled == part.len();
        (whole && [p as u64, id, end] == next).then_some(())
    });
    drop(parts);
    if done.len() != shares || done.contains(&None) {
        return None;
    }
    // SAFETY: every share wrote each of its slots -- its documents' and
    // the gaps between them -- or said it had not, which returned above.
    unsafe { dense.set_len(span as usize) };
    // The span ends at the last document's id, `top`.
    let last = span as usize - 1;
    let fresh = IdIndex {
        dense,
        floor,
        last,
        before: last,
        count: count as usize,
        ..IdIndex::default()
    };
    Some((fresh, top))
}

/// Document id -> location mapping.
///
/// Ids are produced consecutively from 1 by `allocate_id`, so a dense array
/// is enough in the overwhelming majority of cases: no hashing, a single
/// index. Ids supplied from outside that fall far beyond the range land in
/// the sparse map, so `put docs {id: 10_000_000, ...}` does not blow up
/// memory.
///
/// The array starts at a floor, the id of its first slot
/// (docs/design/storage.md#the-id-indexs-floor). From 1, a collection whose
/// ids climb while its old rows go -- sessions under `@ttl`, a queue -- held
/// 12 bytes for every id it ever gave, a few thousand rows' index growing 57
/// MB a day at 55 ids a second, and no compact or restart gave a byte back.
/// The empty slots before the first held one are room: a write within the
/// gap below the first held id takes one and moves nothing, and where the
/// room is spent the array is copied with a quarter of its span more of it
/// ([`Self::lower`]), so the array grows down as it grows up, by doubling.
/// The empty runs at either end -- before the first held slot, and past the
/// last, which deletes of the highest ids leave -- are cut once each is
/// more than the span from the first held slot to the last and
/// [`CUT_FROM`] ([`Self::past_edge`], one walk for both ends and both runs held to their
/// bounds at either's move), all of them at a compact's or a checkpoint's
/// relocation ([`Self::relocate`]), and an open's array starts at the first
/// id it reads and ends at the last; an id past the end, or in the room,
/// takes the array to it where it would be half held. An id below the
/// floor reads as absent.
///
/// Invariant: no sparse id is inside the held range, the first held slot
/// to the end. `get` looks only at the dense slot for an id in that range,
/// so a sparse entry the dense array grew over would vanish from every
/// lookup while `ids()` still listed it -- which is what happened before
/// [`IdIndex::insert`] moved them over. An empty slot of the room may be a
/// sparse id's, which `get` and `remove` ask the map for.
///
/// And no sparse id is within the gap of the held range, either side: the
/// array grown to within it of one, or taken down to within it, takes it
/// in, and the ids it then reaches the same way. The sparse map holds only
/// ids the gap rule keeps out, and while they outnumber the array's, 64 or
/// more, their largest run takes the array where it holds more
/// ([`Self::rebase`]): a floor the first id set high -- a replica's pending
/// row at 2^52, a COPY whose first row is its last id -- or a block's
/// deletes put back from the lowest left every row below it sparse, sorted
/// at every walk, until the next open.
#[derive(Clone)]
struct IdIndex {
    /// `dense[i]` -> id `floor + i`
    dense: Vec<Loc>,
    /// The id of `dense[0]`, at least 1. The range ends at an id it took or
    /// grew to below one it took, so below `DocId::MAX`, which no write
    /// gives ([`written_id`]): `floor + i` never wraps.
    floor: DocId,
    /// The first slot of `dense` holding a location, or `dense.len()`,
    /// which is only where the array is empty: an array left with no
    /// location is let go of ([`Self::past_edge`]). The slots before it are
    /// the room.
    lead: usize,
    /// Every slot strictly between `lead` and this one is empty: where the
    /// look for the next held slot goes on from once `lead`'s is emptied,
    /// so an id deleted and written again at the front does not walk the
    /// empty run behind it each time. `lead` where nothing is known.
    after: usize,
    /// The last slot of `dense` holding a location, 0 where the array is
    /// empty: `lead` at the other end. The slots past it are empty, cut
    /// once they are more than [`CUT_FROM`]. Without it the array's end never moved down, and ids
    /// written downward while the highest go -- reverse timestamps under
    /// `@ttl` -- held 12 bytes for every id they ever gave, as the array
    /// from 1 did for ids climbing.
    last: usize,
    /// Every slot strictly between this one and `last` is empty: `after`
    /// at the other end, so an id deleted and written again at the end
    /// does not walk the empty run before it each time. `last` where
    /// nothing is known.
    before: usize,
    /// Two runs of empty slots, each from its first slot up to its second
    /// (not included), `(0, 0)` for none: the runs the walks past an
    /// emptied end crossed, and those `after` and `before` knew and lost
    /// to a write further out. A walk that reaches one steps over it, a
    /// write into one keeps its longer side, and a run noted joins one it
    /// meets or, [`HOLE_FROM`] slots or more, takes the place of the one
    /// further from both ends. `after` and `before` know the
    /// run next to an end alone: a batch of new ids appended past an empty
    /// run and deleted oldest first walked the run back at every batch --
    /// 250 ns a write for batches of two beside a long-lived row, where an
    /// array from 1 took 4 -- and a block of pushes and pops put back
    /// walked it for every row. One run, the longest kept, left a batch
    /// walking past an old one deeper in; the latest kept, a block put back
    /// lost the run below its rows to the one between them.
    holes: [(usize, usize); 2],
    /// A `HashMap` rather than an ordered map: a `BTreeMap` here made the
    /// browser module 4.4 KB larger and its HNSW build 15% slower, the whole
    /// id index no longer inlining into the hot paths at `opt-level = "z"`.
    sparse: HashMap<DocId, Loc>,
    /// No sparse id above the dense range is within the gap of this one or
    /// below it: the lowest such id less [`MAX_DENSE_GAP`], so the array
    /// grown to it is within the gap of a sparse id and takes it in, still
    /// one comparison where the array grows. Those below the first held id
    /// it never grows over. A lower bound, not the exact figure: a removal leaves it
    /// where it was, which costs at most one scan that finds nothing and
    /// sets it right. `DocId::MAX` for none.
    sparse_floor: DocId,
    /// No sparse id below the first held one is within the gap of this one
    /// or above it: the highest such id plus [`MAX_DENSE_GAP`], so a write
    /// that takes the array down asks the map for ids to take with it only
    /// where one may be in reach -- the map sorted at every such write was
    /// quadratic -- one comparison. An upper bound, as `sparse_floor` is a
    /// lower one; 0 for none.
    sparse_ceil: DocId,
    count: usize,
    /// The writes before the next [`Self::rebase`] looks for a run to take
    /// the array: each sparse insert, and each delete from the array while
    /// the map holds [`REBASE_FROM`] ids or more, counts one, and a look
    /// starts it again at the ids it sorted. So a look sorts ids at most
    /// twice the writes since the last, and a delete from the array asks
    /// one too: following the map's growth alone, a rebase undone -- a
    /// block put back, its far rows deleted -- left the rows it had moved
    /// into the map there. Never more than the map holds ([`Self::trim`]):
    /// left where a look set it while the map emptied -- its ids deleted,
    /// or taken into the array -- the count outlived them, and the next
    /// far run waited for it, sparse, where a run that outnumbered the
    /// array would have taken it at once.
    rebase_in: usize,
    /// The slots a cut, a lowering or a rebase copied, the rebases that
    /// looked, the sparse ids lowerings sorted, the empty slots the walks
    /// past an emptied end stepped over, the empty slots the array grew by,
    /// and the sparse ids the looks sorted: what the amortised bounds are
    /// held to.
    #[cfg(test)]
    moved: (usize, usize, usize, usize, usize, usize),
}

impl Default for IdIndex {
    fn default() -> Self {
        IdIndex {
            dense: Vec::new(),
            floor: 1,
            lead: 0,
            after: 0,
            last: 0,
            before: 0,
            holes: [(0, 0); 2],
            sparse: HashMap::new(),
            sparse_floor: DocId::MAX,
            sparse_ceil: 0,
            count: 0,
            rebase_in: 0,
            #[cfg(test)]
            moved: (0, 0, 0, 0, 0, 0),
        }
    }
}

/// The largest gap still considered worth extending the dense array for.
const MAX_DENSE_GAP: u64 = 4096;

/// The fewest empty slots before the first held one that a write cuts,
/// beside a quarter of the span ([`IdIndex::past_edge`]): below it, 48 KB,
/// a cut would move the array for less than it gives back. And the most
/// past the last held one a write leaves uncut beside the span: what an id
/// written within the gap past it and deleted leaves.
const CUT_FROM: usize = MAX_DENSE_GAP as usize;

/// The fewest empty slots a walk crossed that it notes as a hole where it
/// meets none ([`IdIndex::note_hole`]): fewer cost the next walk little.
const HOLE_FROM: usize = 64;

/// The fewest sparse ids, outnumbering the array's, that a write looks
/// among for a run to take the array ([`IdIndex::rebase`]): fewer are
/// sorted at a walk in about a microsecond, and a handful of a replica's
/// pending rows never move it.
const REBASE_FROM: usize = 64;

impl IdIndex {
    /// Allocated bytes. Approximate for `HashMap`: hashbrown also keeps one
    /// control byte per bucket.
    fn bytes(&self) -> usize {
        use std::mem::size_of;
        self.dense.capacity() * size_of::<Loc>()
            + self.sparse.capacity() * (size_of::<DocId>() + size_of::<Loc>() + 1)
    }

    /// The location of `id`. The range is asked on the `u64` side: on
    /// WASM32 `usize` is 32 bits and `id as usize` truncates silently -- an
    /// id like 2^52 (the browser client's optimistic rows sit in exactly
    /// that range) wrapped to zero and went to `dense[0 - 1]`. An id below
    /// the floor wraps past every slot: one subtraction and one comparison,
    /// as the range from 1 took, the floor read beside the array. An empty
    /// slot asks the map, as a slot of the room may be a sparse id's: the
    /// map answers at once while it holds nothing. Asking it only for a
    /// slot before `lead` -- a comparison more, on an empty slot alone --
    /// changed what `get`'s callers inlined, 16 instructions a lookup by id
    /// (gate's `core-get-id` +0.16%), and the map's lookup out of line made
    /// an open's walk 8 instructions an id dearer (`file-open` +1.1%).
    #[inline]
    fn get(&self, id: DocId) -> Option<Loc> {
        let i = id.wrapping_sub(self.floor);
        if i < self.dense.len() as u64 {
            let l = self.dense[i as usize];
            if !l.is_empty() {
                return Some(l);
            }
        }
        self.sparse.get(&id).copied()
    }

    fn insert(&mut self, id: DocId, loc: Loc) {
        let i = id.wrapping_sub(self.floor);
        let len = self.dense.len() as u64;
        if i >= len {
            // Within the gap past the range's last id, the array grows to
            // it. An id below the floor wrapped to near 2^64, past the gap
            // but for a range ending within it of 2^64, which `id >= floor`
            // tells apart.
            if i - len >= MAX_DENSE_GAP || id < self.floor {
                return self.insert_apart(id, loc);
            }
            let i = i as usize;
            if len == 0 {
                (self.lead, self.after, self.last, self.before) = (i, i, i, i);
            }
            #[cfg(test)]
            {
                self.moved.4 += i - len as usize;
            }
            self.dense.resize(i + 1, Loc::EMPTY);
            // Keep the invariant: the sparse ids the array now covers move
            // into it, and the slot is filled below once they are placed.
            // Moving them does not change the count.
            if self.sparse_floor <= id {
                self.take_over_sparse(id);
            } else {
                // The new last held slot, the slots between it and the
                // last one empty: filled here, where the path below asked
                // whether the slot was empty and whether it was before
                // `after`.
                self.dense[i] = loc;
                self.count += 1;
                (self.before, self.last) = (self.last, i);
                return;
            }
        }
        let i = i as usize;
        let slot = &mut self.dense[i];
        if slot.is_empty() {
            // Before `after` or past `before`: in the room or past the last
            // held slot, or in a run known empty at either end; or in a
            // hole. The comparisons `placed` makes here, its cases out of
            // line.
            let [(a, b), (c, d)] = self.holes;
            if i < self.after
                || i > self.before
                || i.wrapping_sub(a) < b - a
                || i.wrapping_sub(c) < d - c
            {
                return self.insert_gap(id, loc);
            }
            *slot = loc;
            self.count += 1;
        } else {
            *slot = loc;
        }
    }

    /// [`Self::insert`] of an id into an empty slot before `after` or past
    /// `before`: from `lead` on it is placed there, and in the room it is
    /// taken as one below the first held id is ([`Self::insert_apart`]).
    #[inline(never)]
    fn insert_gap(&mut self, id: DocId, loc: Loc) {
        let i = (id - self.floor) as usize;
        if i < self.lead {
            return self.insert_apart(id, loc);
        }
        self.dense[i] = loc;
        self.count += 1;
        self.placed(i);
    }

    /// [`Self::insert`] of an id neither in the held range nor within the
    /// gap past it: the first of an index holding none, where the array
    /// starts -- an open of an image whose first id is past the gap, a
    /// compact's fresh store, put its rows in the sparse map from 1 --
    /// one within the gap below the first held id, or in the room where
    /// the array from it would be half held, which the array is taken down
    /// to ([`Self::lower`]); one past the end where the array grown to it
    /// would be half held, which it is grown to; and any other into the
    /// sparse map, a slot of the room's included, whose runs a look may
    /// then give the array or take into it ([`Self::rebase`]). Apart, as
    /// it is the rare one.
    #[inline(never)]
    fn insert_apart(&mut self, id: DocId, loc: Loc) {
        if id >= 1 && self.lead == self.dense.len() && !self.sparse.contains_key(&id) {
            // The array holds nothing: it may start wherever its first id
            // is, and takes in the sparse ids within the gap of it. Started
            // at the floor the last cut left, its end, ids written downward
            // into an emptied array -- a block's rows put back beside a row
            // in the map -- each spanned the array from them to that end.
            self.dense.clear();
            (self.floor, self.lead, self.after, self.holes) = (id, 0, 0, [(0, 0); 2]);
            // The bounds kept as bounds: a sparse id past the old end may
            // now be below the id, and one below the old start past it.
            let (above, below) = (self.sparse_floor, self.sparse_ceil);
            if self.count == 0 {
                (self.sparse_floor, self.sparse_ceil) = (DocId::MAX, 0);
            } else {
                if above.saturating_add(MAX_DENSE_GAP) < id {
                    self.sparse_ceil = below.max(id - 1 + MAX_DENSE_GAP);
                }
                if below.saturating_sub(MAX_DENSE_GAP) > id {
                    self.sparse_floor = above.min(id.saturating_sub(MAX_DENSE_GAP));
                }
            }
            self.insert(id, loc);
            if id <= self.sparse_ceil {
                self.lower(id);
            }
            return;
        }
        let first = self.floor + self.lead as u64;
        // The array taken to an id further than the gap where it would
        // still hold a slot in two: past its end, the next id once the
        // highest rows went and a cut, a relocation or an open dropped
        // their slots -- `del s where id > 10000` of 15 000 rows sent every
        // row after it to the map, `get s limit 1` 1.5 us to 101 us -- and
        // in the room before the first held slot, the next id falling once
        // the lowest went. Not below the floor: a copy of the array a write
        // there, deleted again, would take turns with the cut. An id
        // already sparse stays so.
        let held = (self.count - self.sparse.len()) as u64;
        let half = |reach: u64| reach < 2 * held && !self.sparse.contains_key(&id);
        let last = self.floor + self.last as u64;
        if id >= 1
            && id < first
            && (first - id <= MAX_DENSE_GAP || id >= self.floor && half(last - id))
        {
            // In the room, no sparse id in reach, nothing moves: the
            // lowering's whole work, asked here. `lower` is out of line
            // since it has three callers, and called for every id written
            // downward it made a write and the delete of the last held id
            // 14.1 ns where 0d20451a, inlining it, took 12.6 (11.6 now).
            if id < self.floor || id <= self.sparse_ceil {
                self.lower(id);
            }
            let i = (id - self.floor) as usize;
            self.dense[i] = loc;
            self.count += 1;
            return self.placed(i);
        }
        if id > first && half(id - first) {
            let i = (id - self.floor) as usize;
            #[cfg(test)]
            {
                self.moved.4 += i + 1 - self.dense.len();
            }
            self.dense.resize(i + 1, Loc::EMPTY);
            if self.sparse_floor <= id {
                self.take_over_sparse(id);
            }
            self.dense[i] = loc;
            self.count += 1;
            return self.placed(i);
        }
        // Past the range it is more than the gap past its last slot, and
        // below it more than the gap below its first held id, so neither
        // bound overflows.
        if id > first {
            self.sparse_floor = self.sparse_floor.min(id - MAX_DENSE_GAP);
        } else {
            self.sparse_ceil = self.sparse_ceil.max(id + MAX_DENSE_GAP);
        }
        if self.sparse.insert(id, loc).is_none() {
            self.count += 1;
        }
        self.rebase(id);
    }

    /// Takes the array down to `id`, within the gap below its first held
    /// one, and every sparse id within the gap below it, and below them,
    /// moving them in. Into the room before the first held slot, where it
    /// reaches that far, moving nothing; else into a copy of the array from
    /// it with room of a quarter of the slots from the first held one to
    /// the last below the lowest id it takes, never below 1 -- so ids
    /// written downward copy the array as ids written upward do, once its
    /// span grows by a part of itself, rather than once a gap: by the gap
    /// alone, ids each the gap below the last copied the array at every
    /// write, 2.05e9 slots for 1 000 of them. The copy leaves out the empty
    /// slots past the last held one, as a cut drops them. The room is no
    /// part of the gap's reach: an id more than the gap below the first
    /// held one is sparse wherever it is, so writes each just below the
    /// room leave the array where it is, where room counted from the floor
    /// grew it by a quarter a write, 2^40 slots in 85. Of the sparse ids,
    /// only those below the first held one are sorted, and only where
    /// `sparse_ceil` says one may be in reach.
    fn lower(&mut self, id: DocId) {
        let first = self.floor + self.lead as u64;
        let mut low = id;
        let mut taken = Vec::new();
        if id <= self.sparse_ceil {
            taken = self.sparse_in(0, first);
            #[cfg(test)]
            {
                self.moved.2 += taken.len();
            }
            // Those above the id come into the range it opens, then those
            // within the gap below it, and below them.
            let mut n = taken.len();
            while n > 0 && (taken[n - 1] > id || low - taken[n - 1] <= MAX_DENSE_GAP) {
                n -= 1;
                low = low.min(taken[n]);
            }
            self.sparse_ceil = n.checked_sub(1).map_or(0, |n| taken[n] + MAX_DENSE_GAP);
            taken.drain(..n);
        }
        if low < self.floor {
            let lead = self.lead;
            // None held where the array's ids are gone and the map's kept.
            let held = match lead < self.dense.len() {
                true => self.last + 1 - lead,
                false => 0,
            };
            let to = low.saturating_sub(held as u64 / 4).max(1);
            let by = (self.floor - to) as usize;
            // Into an array of its own, the empty slots first: shifting the
            // slots in place took a copy and a fill of the browser module's
            // own. Those before `lead` are empty, and are not copied.
            let mut dense = Vec::with_capacity(lead + by + held);
            dense.resize(lead + by, Loc::EMPTY);
            #[cfg(test)]
            {
                self.moved.4 += lead + by;
            }
            dense.extend_from_slice(&self.dense[lead..lead + held]);
            let len = dense.len();
            self.dense = dense;
            self.floor = to;
            // The holes let go of, as at a cut: the copy walks the span
            // already, and the next walks find them again.
            self.holes = [(0, 0); 2];
            match held {
                // `placed` takes the first id it is given as both ends.
                0 => (self.lead, self.after, self.last, self.before) = (len, len, 0, 0),
                _ => {
                    (self.lead, self.last) = (lead + by, self.last + by);
                    self.after = (self.after + by).min(len);
                    self.before += by;
                }
            }
            #[cfg(test)]
            {
                self.moved.0 += held;
            }
        }
        for k in taken {
            if let Some(l) = self.sparse.remove(&k) {
                let i = (k - self.floor) as usize;
                self.dense[i] = l;
                self.placed(i);
            }
        }
        self.trim();
    }

    /// Keeps `lead` and `after`, and `last` and `before`, saying what they
    /// say once slot `i`, empty, was given a location: each end as the
    /// other's mirror.
    #[inline]
    fn placed(&mut self, i: usize) {
        // Asked as `insert` asks it, two comparisons where neither holds
        // it: a loop asking each end of each hole was 0.5 ns of a write and
        // a delete.
        let [(a, b), (c, d)] = self.holes;
        if i.wrapping_sub(a) < b - a || i.wrapping_sub(c) < d - c {
            self.fill_hole(i);
        }
        if i < self.after {
            if i < self.lead {
                // The run `after` knew is lost to the write: a hole. Called
                // only where there is one: out of line, the call was 0.3 ns
                // of each id written downward and its delete.
                if self.after > self.lead + 1 {
                    self.note_hole(self.lead + 1, self.after);
                }
                (self.lead, self.after) = (i, self.lead);
            } else {
                self.after = i;
            }
        }
        if i > self.before {
            if i > self.last {
                if self.last > self.before + 1 {
                    self.note_hole(self.before + 1, self.last);
                }
                (self.last, self.before) = (i, self.last);
            } else {
                self.before = i;
            }
        }
    }

    /// Slot `i` of a hole was given a location: the hole keeps its longer
    /// side. Out of line, as `placed` is inlined wherever a slot is filled.
    #[inline(never)]
    fn fill_hole(&mut self, i: usize) {
        for h in &mut self.holes {
            if i >= h.0 && i < h.1 {
                *h = match i - h.0 >= h.1 - i {
                    true => (h.0, i),
                    false => (i + 1, h.1),
                };
            }
        }
    }

    /// Notes the slots from `lo` up to `hi`, every one empty, as a hole:
    /// joined to one it meets, or, [`HOLE_FROM`] slots or more, in place
    /// of the one further from both ends -- a walk starts at an end. A
    /// shorter run costs its walk little, and in place of the shorter
    /// hole, it threw out the one below a batch's rows for the few between
    /// them.
    #[inline(never)]
    fn note_hole(&mut self, lo: usize, hi: usize) {
        if hi <= lo {
            return;
        }
        let meets = |h: (usize, usize)| h.0 < h.1 && lo <= h.1 && h.0 <= hi;
        let [x, y] = self.holes;
        let far = |h: (usize, usize)| match h.0 < h.1 {
            true => {
                h.0.saturating_sub(self.lead)
                    .min(self.last.saturating_sub(h.1))
            }
            false => usize::MAX,
        };
        self.holes = match (meets(x), meets(y)) {
            (true, true) => [(lo.min(x.0).min(y.0), hi.max(x.1).max(y.1)), (0, 0)],
            (true, false) => [(lo.min(x.0), hi.max(x.1)), y],
            (false, true) => [x, (lo.min(y.0), hi.max(y.1))],
            _ if hi - lo < HOLE_FROM => return,
            _ if far(x) >= far(y) => [(lo, hi), y],
            _ => [x, (lo, hi)],
        };
    }

    /// Moves the sparse entries within the gap of the dense array, just
    /// grown to `upto`, into it -- and with them every one it can reach
    /// under the same gap rule, growing further to do so. The keys are
    /// sorted once, so a run of sparse ids is taken in one pass: taking
    /// only those at or below `upto` meant one scan of the whole map per
    /// insert while the array grew through the run. Those below the first
    /// held id, in the room or below the floor, stay. Out of line: called
    /// from `insert` alone, it was inlined there, and a new id at the end,
    /// the open's and every insert's path, took 10 instructions more
    /// (gate's `file-open` +0.69%, `core-insert` +0.11%).
    #[inline(never)]
    fn take_over_sparse(&mut self, upto: DocId) {
        let floor = self.floor;
        let keys = self.sparse_in(floor + self.lead as u64, DocId::MAX);
        let mut end = upto;
        let mut taken = 0;
        for &k in &keys {
            if k > end && k - end > MAX_DENSE_GAP {
                break;
            }
            end = end.max(k);
            taken += 1;
        }
        let span = (end - floor + 1) as usize;
        if span > self.dense.len() {
            #[cfg(test)]
            {
                self.moved.4 += span - self.dense.len();
            }
            self.dense.resize(span, Loc::EMPTY);
        }
        for &k in &keys[..taken] {
            if let Some(l) = self.sparse.remove(&k) {
                let i = (k - floor) as usize;
                self.dense[i] = l;
                self.placed(i);
            }
        }
        self.sparse_floor = keys.get(taken).map_or(DocId::MAX, |&k| k - MAX_DENSE_GAP);
        self.trim();
    }

    /// After the sparse map lost ids -- deleted, or taken into the array --
    /// the look's count is held to what it now holds ([`Self::rebase_in`]),
    /// and the map is built again at its size once its room is more than
    /// four times its ids and 64: the room of each id it held stayed until
    /// the next open, a far run deleted down to one id kept the room of
    /// all of it. A map of a few ids keeps its room: let go of whenever it
    /// emptied, one long-lived row beside rows put and deleted one at a
    /// time allocated it again at every write.
    fn trim(&mut self) {
        let n = self.sparse.len();
        self.rebase_in = self.rebase_in.min(n);
        if self.sparse.capacity() > 4 * n + REBASE_FROM {
            // Through `insert`, which the module holds already: `collect`
            // brought its own, about 300 bytes.
            let was = std::mem::take(&mut self.sparse);
            self.sparse.reserve(n);
            for (k, l) in was {
                self.sparse.insert(k, l);
            }
        }
        if n == 0 {
            (self.sparse_floor, self.sparse_ceil) = (DocId::MAX, 0);
        }
    }

    /// A write that counts toward the next look ([`Self::rebase_in`]), `id`
    /// the id it wrote to the map, 0 for a delete. A look sorts the sparse
    /// ids. Their largest run -- ids each within the gap of the one before
    /// -- takes the array where it holds as many ids as the array does,
    /// the run holding `id` where two are as large, the array's own going
    /// into the sparse map: one long-lived row beside rows put and deleted
    /// one at a time sent 98% of them through the map, a hash and an
    /// allocation each, where the array held one id and waited for 64
    /// sparse ones. Else the run next to the array either side is taken in
    /// where the hole between them is no longer than its ids. Every id of
    /// a run, and of the array, is more than the gap from the other (the
    /// second invariant), so neither is within it of the new range. A look
    /// comes where the map holds as many ids as the array, or the nearest
    /// sparse id is within as many slots of it, once as many writes as the
    /// last sorted have come -- those bring at most as many ids, so each
    /// look sorts at most twice the writes since -- or at once while the
    /// map holds [`REBASE_FROM`] or fewer. A write's own, under the write
    /// lock, as the cut is.
    #[inline(never)]
    fn rebase(&mut self, id: DocId) {
        self.rebase_in = self.rebase_in.saturating_sub(1);
        let (n, held) = (self.sparse.len() as u64, self.count - self.sparse.len());
        // A run may take the array where the map holds as many ids, and be
        // taken in where the nearest sparse id either side, as the bounds
        // say, is within as many slots of it: a look otherwise would only
        // put the next off.
        let (first, last) = (self.floor + self.lead as u64, self.floor + self.last as u64);
        let under = (self.sparse_ceil.checked_sub(MAX_DENSE_GAP))
            .is_some_and(|top| top < first && first - top <= n);
        let over = self.sparse_floor != DocId::MAX
            && (self.sparse_floor + MAX_DENSE_GAP).saturating_sub(last) <= n;
        let near = held > 0 && (under || over);
        if self.rebase_in > 0 && n > REBASE_FROM as u64 || n < held as u64 && !near {
            return;
        }
        #[cfg(test)]
        {
            self.moved.1 += 1;
        }
        let keys = self.sparse_in(0, DocId::MAX);
        #[cfg(test)]
        {
            self.moved.5 += keys.len();
        }
        // The largest run, and the runs next to the array either side.
        let (mut run, mut from, mut below, mut above) = (0..0, 0, 0..0, 0..0);
        for i in 1..=keys.len() {
            if keys.get(i).is_none_or(|&k| k - keys[i - 1] > MAX_DENSE_GAP) {
                let mine = keys[from] <= id && id <= keys[i - 1];
                if i - from > run.len() || i - from == run.len() && mine {
                    run = from..i;
                }
                match keys[from] > first {
                    false => below = from..i,
                    true if above.is_empty() => above = from..i,
                    true => {}
                }
                from = i;
            }
        }
        // A run next to the array across a hole no longer than its ids is
        // taken in, the array grown or lowered over the hole: rows put
        // back below a hole their deletes left, the array's first held id
        // the gap beyond them, stayed sparse while the array outnumbered
        // them. Only that close: a write a quarter of the array below it
        // and its delete, again and again, copied the array a pair. No
        // sparse id is in the held range, so a run is wholly below the
        // first held id or past the last.
        if held > 0 && run.len() < held {
            if !below.is_empty() && first - keys[below.end - 1] <= below.len() as u64 {
                self.lower(keys[below.start]);
            } else if !above.is_empty() && keys[above.start] - last <= above.len() as u64 {
                self.take_over_sparse(keys[above.start]);
            }
        } else if !run.is_empty() {
            // The array's ids into the sparse map, and an empty array from
            // the run's first id, which the take-over grows over the run
            // and no further: the next sparse id is more than the gap past
            // it.
            let was = std::mem::take(&mut self.dense);
            #[cfg(test)]
            {
                self.moved.0 += was.len() - self.lead;
            }
            for (i, &l) in was.iter().enumerate().skip(self.lead) {
                if !l.is_empty() {
                    self.sparse.insert(self.floor + i as u64, l);
                }
            }
            (self.floor, self.lead, self.after) = (keys[run.start], 0, 0);
            (self.last, self.before, self.holes) = (0, 0, [(0, 0); 2]);
            // The bound below the run as it is: the highest sparse id
            // below it, the array's last among them where the array was
            // below. Left unknown, the next look waited for a write within
            // the gap below: the array's ids, or the run's below a hole
            // within its count, stayed sparse while the map held fewer ids
            // than the run.
            let top = keys[..run.start].last().copied();
            let top = match held > 0 && last < keys[run.start] {
                true => top.max(Some(last)),
                false => top,
            };
            self.sparse_ceil = top.map_or(0, |k| k + MAX_DENSE_GAP);
            self.take_over_sparse(self.floor);
        }
        self.rebase_in = keys.len().min(self.sparse.len());
    }

    fn remove(&mut self, id: DocId) -> Option<Loc> {
        let i = id.wrapping_sub(self.floor);
        if i < self.dense.len() as u64 {
            let i = i as usize;
            let slot = &mut self.dense[i];
            if !slot.is_empty() {
                let old = *slot;
                *slot = Loc::EMPTY;
                self.count -= 1;
                if i == self.lead {
                    self.past_edge::<false>();
                } else if i == self.last {
                    self.past_edge::<true>();
                }
                // A delete from the array counts toward the next look while
                // the map holds enough ids to be looked among: where it
                // took a run's rows and the array's went in, those of the
                // run going -- a block put back, the run's rows deleted --
                // bring the array's back out.
                if self.sparse.len() >= REBASE_FROM {
                    self.rebase(0);
                }
                return Some(old);
            }
            // An empty slot past the room: no id. In the room, a sparse
            // id's, maybe.
            if i >= self.lead {
                return None;
            }
        }
        let old = self.sparse.remove(&id);
        if old.is_some() {
            self.count -= 1;
            self.trim();
        }
        old
    }

    /// The first held slot or, `END`, the last was just emptied: that end
    /// goes on to the next held slot inward, looked for from where its
    /// last look stopped (`after`, `before`), stepping over the holes it
    /// reaches ([`Self::holes`]) and noting the run it crossed as one, and
    /// the empty runs are cut ([`Self::cut`]). The front's once it is more
    /// than the span from the first held slot to the last and the room a
    /// cut keeps -- a quarter of the span, [`CUT_FROM`] at most -- so a cut
    /// moves fewer slots than it drops, each slot moved a constant number
    /// of times however long ids
    /// climb, and the run is at most about the span; and more than
    /// [`CUT_FROM`] and a quarter of the span, the most a lowering leaves
    /// ([`Self::lower`]: the gap, and a quarter of the span below it), so
    /// an id written below the first held one and deleted brings no cut,
    /// and the next takes the room. Cut to the first held slot once the run
    /// was half the array, the lowering and the cut took turns, 169 000
    /// slots a pair beside 100 000 rows. The end's once it is more than
    /// [`CUT_FROM`] and the span: its cut moves nothing and nothing lowers
    /// into it, and a new id, the highest ever given plus 1, lands where
    /// the end stood, so the run is the hole between the rows and the next
    /// new id. Cut at [`CUT_FROM`], it sent every new id past the hole to
    /// the map -- `del s where id > 10000` of 15 000 rows, then 5 000 new
    /// ones, all sparse -- or, taken back over the hole by the next write
    /// ([`Self::insert_apart`]: the array half held at least), filled the
    /// hole again at every write once a row put and deleted there emptied
    /// it. The holes keep the walks back over it to a few slots a write.
    /// The array spans about twice the ids between the first held one and
    /// the last.
    /// An array left with no location is let go of whatever its length,
    /// moving nothing. A write's own, under the write lock: a compact may
    /// never come, as below 64 MB or in the browser, where only the page
    /// asks one.
    ///
    /// One walk for both ends, each walked in its own order -- the end's
    /// slots counted from the last -- and both runs held to their bounds at
    /// every move of either, so neither end can be left out of what the
    /// other keeps: the first form moved the front alone, and ids written
    /// downward while the highest went kept every slot past the last held
    /// one, 26.3 MB for 3 000 rows after 2 million ids. One source, a copy
    /// for each end (`END`): asked which end at run time, an id written and
    /// the first held one deleted took 7.8 ns where the front's walk alone
    /// took 6.5, and 6.8 so (the index alone, release, aarch64).
    #[inline(never)]
    fn past_edge<const END: bool>(&mut self) {
        let len = self.dense.len();
        let side = |p: usize| if END { len - 1 - p } else { p };
        let (edge, known) = match END {
            false => (self.lead, self.after),
            true => (self.last, self.before),
        };
        let mut k = (side(edge) + 1).max(side(known));
        // Held where the walk starts -- a queue's oldest row deleted, the
        // next one held -- it asks no hole: the run it leaves behind is
        // past the new edge, where no walk goes. Asked at every delete of
        // an edge, the holes made a write and the delete of the first held
        // id 9.7 ns where 0d20451a took 7.4.
        if k == len || self.dense[side(k)].is_empty() {
            k = self.walk_past(END, k, edge);
            // Past the last slot only from the front, the array then empty:
            // the end's walk stops at the first held slot at the latest.
            if k == len {
                self.lead = len;
                return self.cut(0);
            }
        }
        match END {
            false => (self.lead, self.after) = (k, k),
            true => (self.last, self.before) = (side(k), side(k)),
        }
        let span = self.last + 1 - self.lead;
        let room = (span / 4).min(CUT_FROM);
        let most = (span + room).max(CUT_FROM + span / 4);
        if self.lead > most {
            self.cut(room);
        } else if len - 1 - self.last > CUT_FROM.max(span) {
            self.cut(self.lead);
        }
    }

    /// The walk of [`Self::past_edge`] from `k`, an empty slot, to the next
    /// held one, in the end's own order where `end` -- `k` counted from the
    /// last slot -- stepping over the holes it reaches, two at most; and
    /// the run from `edge`, the slot just emptied, to the one found noted
    /// as a hole. Returns the slot found, the length where none is. One
    /// copy for both ends, out of line, a call only where a walk crosses
    /// an empty slot: `past_edge`'s two copies held it twice in the
    /// browser module.
    #[inline(never)]
    fn walk_past(&mut self, end: bool, mut k: usize, edge: usize) -> usize {
        let len = self.dense.len();
        let flip = |(lo, hi): (usize, usize)| match end {
            false => (lo, hi),
            true => (len - hi.min(len), len - lo.min(len)),
        };
        let [x, y] = self.holes;
        let holes = [flip(x), flip(y)];
        loop {
            for (a, b) in holes {
                if k >= a && k < b {
                    k = b;
                }
            }
            let mut stop = len;
            for (a, b) in holes {
                if k < a && a < b {
                    stop = stop.min(a);
                }
            }
            // Over a slice of the run, one bound a slot: indexed up to a
            // hole, every read was checked against the length besides, a
            // fifth slower a slot (a row moved up a gap at a time, 3.3 us a
            // move where it took 2.7).
            let empty = match end {
                false => {
                    let run = &self.dense[k..stop];
                    let mut j = 0;
                    while j < run.len() && run[j].is_empty() {
                        j += 1;
                    }
                    j
                }
                true => {
                    let run = &self.dense[len - stop..len - k];
                    let mut j = run.len();
                    while j > 0 && run[j - 1].is_empty() {
                        j -= 1;
                    }
                    run.len() - j
                }
            };
            k += empty;
            #[cfg(test)]
            {
                self.moved.3 += empty;
            }
            if k == len || k < stop {
                break;
            }
        }
        // Every slot from the emptied edge to the one found is empty.
        if k < len {
            match end {
                false => self.note_hole(edge, k),
                true => self.note_hole(len - k, edge + 1),
            }
        }
        k
    }

    /// Cuts every empty slot past the last held one, and those before the
    /// first held one but the last `keep`, the floor moved past them, and
    /// gives back the allocation past four times what is left -- never
    /// below [`CUT_FROM`]'s, so a queue that empties and fills again keeps
    /// its allocation. The end's slots go with no move, a `Vec` shortened,
    /// and keep no room: the allocation is the end's room, as an array
    /// grows there without moving what it holds. The front's move the held
    /// span down. An array holding nothing is let go of whole.
    fn cut(&mut self, keep: usize) {
        let (lead, len) = (self.lead, self.dense.len());
        if lead == len {
            self.floor += len as u64;
            self.dense.clear();
            (self.lead, self.after, self.last, self.before) = (0, 0, 0, 0);
            self.holes = [(0, 0); 2];
        } else {
            let end = self.last + 1;
            self.dense.truncate(end);
            self.after = self.after.min(end);
            let by = lead - keep.min(lead);
            // The holes let go of: a cut moves or drops as many slots as
            // the walks that find them again cross, and shifted with the
            // slots they were 165 bytes of the browser module.
            self.holes = [(0, 0); 2];
            if by > 0 {
                self.dense.copy_within(lead.., lead - by);
                self.dense.truncate(end - by);
                #[cfg(test)]
                {
                    self.moved.0 += end - lead;
                }
                self.floor += by as u64;
                (self.lead, self.after, self.last) = (lead - by, self.after - by, end - 1 - by);
                // Below `lead` only where it is `last`, the one held slot.
                self.before = self.before.saturating_sub(by);
            }
        }
        let keep = self.dense.len().max(CUT_FROM);
        if self.dense.capacity() / 4 > keep {
            self.dense.shrink_to(2 * keep);
        }
    }

    #[inline]
    fn contains(&self, id: DocId) -> bool {
        self.get(id).is_some()
    }

    fn len(&self) -> usize {
        self.count
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn reserve(&mut self, n: usize) {
        self.dense.reserve(n);
    }

    /// [`Self::insert`] of an id above every one the index holds -- an
    /// image's index read in order: the next id pushed onto the dense
    /// array, with no slot to ask first, and any other inserted.
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(always)]
    fn push_above(&mut self, id: DocId, loc: Loc) {
        let len = self.dense.len();
        if id.wrapping_sub(self.floor) == len as u64 && self.sparse.is_empty() {
            (self.before, self.last) = (self.last, len);
            self.dense.push(loc);
            self.count += 1;
        } else {
            self.insert(id, loc);
        }
    }

    /// Returns the ids in ascending order.
    fn ids(&self) -> Vec<DocId> {
        let mut out: Vec<DocId> = Vec::with_capacity(self.count);
        out.extend(self.iter());
        out
    }

    /// The sparse ids from `from` up to `to` (not included), ascending.
    /// Usually none, and nothing allocated. Ids alone: sorted with their
    /// locations, the module held a sort of its own for the pairs, 3.4 KB.
    /// One filter for every caller: filters of the map's keys of their own
    /// were 400 bytes of the browser module.
    fn sparse_in(&self, from: DocId, to: DocId) -> Vec<DocId> {
        let mut ids: Vec<DocId> = self
            .sparse
            .keys()
            .copied()
            .filter(|&k| k >= from && k < to)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// The ids in ascending order, lazily: [`Self::walk`]'s.
    #[inline]
    fn iter(&self) -> impl Iterator<Item = DocId> + '_ {
        self.walk(0).map(|(id, _)| id)
    }

    /// [`Self::iter`] from `from` on. Only a native scan starts from an id
    /// (`Expr::conjunct_id_floor`).
    #[inline]
    fn iter_from(&self, from: DocId) -> impl Iterator<Item = DocId> + '_ {
        self.walk(from).map(|(id, _)| id)
    }

    /// The ids from `from` on and their locations, ascending: the sparse
    /// ones below the first held id, in the room or below the floor, the
    /// dense part from its first held slot or `from`'s, and the sparse ones
    /// above it (see the invariant). Only the sparse ids, usually none, are
    /// collected and sorted up front. One constructor out of line, and one
    /// `next` for every walk.
    #[inline(never)]
    fn walk(&self, from: DocId) -> Walk<'_> {
        let ids = self.sparse_in(from, DocId::MAX);
        let first = self.floor + self.lead as u64;
        let mut below = 0;
        while ids.get(below).is_some_and(|&id| id < first) {
            below += 1;
        }
        let skip =
            (from.saturating_sub(self.floor).min(self.dense.len() as u64) as usize).max(self.lead);
        let slots = self.dense.get(skip..).unwrap_or_default();
        // The slots wait while the sparse ids below them are given.
        let (dense, rest) = match below {
            0 => (slots, &[][..]),
            _ => (&[][..], slots),
        };
        Walk {
            dense: dense.iter(),
            id: self.floor + skip as u64,
            apart: Apart {
                rest,
                map: &self.sparse,
                ids,
                at: 0,
                below,
            },
        }
    }

    /// The array from its first held slot, and that slot's id: the range
    /// the copy in turn places, the room left out as a walk leaves it.
    #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
    fn held(&self) -> (&[Loc], DocId) {
        (&self.dense[self.lead..], self.floor + self.lead as u64)
    }

    /// Replaces every location with `f`'s, visiting the ids in `iter`'s
    /// order, the empty slots before the first held one and past the last
    /// cut first, the room with them: a compact or a checkpoint walks the
    /// array anyway. The sparse ids below the floor so cut are those below
    /// the first held id, and come first; a sparse id in the room, left
    /// uncut, would come after the array.
    fn relocate(&mut self, mut f: impl FnMut(DocId, Loc) -> Loc) {
        self.cut(0);
        let ids = self.sparse_in(0, DocId::MAX);
        let floor = self.floor;
        relocate_sparse(&mut self.sparse, &ids, (floor, true), &mut f);
        for (i, l) in self.dense.iter_mut().enumerate() {
            if !l.is_empty() {
                *l = f(floor + i as u64, *l);
            }
        }
        relocate_sparse(&mut self.sparse, &ids, (floor, false), &mut f);
    }
}

/// [`IdIndex::relocate`] of the sparse ids of `ids` below `floor`, or
/// those at or past it: apart, as they are usually none, and called on
/// both sides of the dense walk.
#[inline(never)]
fn relocate_sparse(
    sparse: &mut HashMap<DocId, Loc>,
    ids: &[DocId],
    (floor, below): (DocId, bool),
    f: &mut impl FnMut(DocId, Loc) -> Loc,
) {
    for &id in ids {
        if let Some(l) = sparse.get_mut(&id).filter(|_| (id < floor) == below) {
            *l = f(id, *l);
        }
    }
}

/// [`IdIndex::walk`]: the sparse ids below the first held id, then the
/// dense slots that hold a location, then the sparse ids above it, each id
/// with its location. A `next` walks the dense slots and nothing else while
/// they last: they wait in [`Apart`] while the ids below them are given,
/// and the sparse ids are asked for out of line, so none of their state is
/// carried through a caller's loop. As a `Chain` of the three parts each id
/// went through two `Chain`s, and a compact of 10 000 rows, whose walks go
/// through them four times, counted 6.5% more instructions under CI's gate.
/// The location comes with the id, so a walk that reads it -- a compact's,
/// an image's -- does not look the id up again.
pub(crate) struct Walk<'a> {
    /// The dense slots in hand, not walked yet.
    dense: std::slice::Iter<'a, Loc>,
    /// The id of the next of them.
    id: DocId,
    apart: Apart<'a>,
}

/// What a [`Walk`] gives apart from the dense slots in hand.
struct Apart<'a> {
    /// The dense slots, while the sparse ids below them are given.
    rest: &'a [Loc],
    /// The sparse map, and the ids of it the walk lists, ascending.
    map: &'a HashMap<DocId, Loc>,
    ids: Vec<DocId>,
    /// The next of them to give.
    at: usize,
    /// How many of them are below the dense slots.
    below: usize,
}

impl<'a> Apart<'a> {
    /// The next sparse id and its location, and the dense slots once the
    /// last id below them was given. A pair, and the caller's `next`
    /// returning what came back: a loop there to walk the slots handed over
    /// kept the walk's place in memory, 5 instructions an id more in a
    /// compact's walks.
    #[inline(never)]
    fn next(&mut self) -> (Option<(DocId, Loc)>, Option<&'a [Loc]>) {
        let next = self
            .ids
            .get(self.at)
            .and_then(|&id| Some((id, *self.map.get(&id)?)));
        self.at += 1;
        match self.at == self.below {
            true => (next, Some(std::mem::take(&mut self.rest))),
            false => (next, None),
        }
    }
}

impl Iterator for Walk<'_> {
    type Item = (DocId, Loc);

    #[inline]
    fn next(&mut self) -> Option<(DocId, Loc)> {
        for &l in self.dense.by_ref() {
            let id = self.id;
            self.id += 1;
            if !l.is_empty() {
                return Some((id, l));
            }
        }
        let (next, dense) = self.apart.next();
        if let Some(dense) = dense {
            self.dense = dense.iter();
        }
        next
    }
}

/// Document id -> a number above zero, for the indexes that keep one a
/// document: the text index's lengths and the vector index's nodes, held
/// one up. Dense where the ids are and sparse where they are not, as
/// [`IdIndex`] is, under the same gap: a `HashMap` lookup was the largest
/// cost of a BM25 merge, and filling one with 100 000 nodes 2.1 of a 16.7
/// ms open. A sparse number the dense array grows over stays in the map --
/// read there while its slot is empty, moved into the slot when its id is
/// written again -- rather than be taken over as the id index takes its
/// locations: that was 1 KB of the browser module.
#[derive(Default, Debug, PartialEq)]
#[cfg_attr(not(any(feature = "text", feature = "vector")), allow(dead_code))]
pub(crate) struct DocMap {
    /// `dense[i]` -> id `i + 1`; 0 is none.
    dense: Vec<u32>,
    sparse: HashMap<DocId, u32>,
    count: usize,
}

#[cfg_attr(not(any(feature = "text", feature = "vector")), allow(dead_code))]
impl DocMap {
    /// On the `u64` side, as [`IdIndex::get`] asks its range.
    #[inline]
    fn in_dense(&self, id: DocId) -> bool {
        id >= 1 && id <= self.dense.len() as u64
    }

    /// The number under `id`, 0 for none.
    #[inline]
    pub fn get(&self, id: DocId) -> u32 {
        if self.in_dense(id) {
            let n = self.dense[id as usize - 1];
            if n != 0 || self.sparse.is_empty() {
                return n;
            }
        }
        self.sparse.get(&id).copied().unwrap_or(0)
    }

    /// Puts `n`, above zero, under `id`, handing back the number it had.
    pub fn insert(&mut self, id: DocId, n: u32) -> Option<u32> {
        if !self.in_dense(id) && id >= 1 && id - self.dense.len() as u64 <= MAX_DENSE_GAP {
            self.dense.resize(id as usize, 0);
        }
        let old = match self.in_dense(id) {
            true => match std::mem::replace(&mut self.dense[id as usize - 1], n) {
                0 => self.take_sparse(id),
                old => old,
            },
            false => self.sparse.insert(id, n).unwrap_or(0),
        };
        if old != 0 {
            return Some(old);
        }
        self.count += 1;
        None
    }

    pub fn remove(&mut self, id: DocId) -> Option<u32> {
        let old = match self.in_dense(id) {
            true => match std::mem::replace(&mut self.dense[id as usize - 1], 0) {
                0 => self.take_sparse(id),
                old => old,
            },
            false => self.take_sparse(id),
        };
        if old == 0 {
            return None;
        }
        self.count -= 1;
        Some(old)
    }

    /// The sparse number under `id`, taken out; 0 for none.
    fn take_sparse(&mut self, id: DocId) -> u32 {
        match self.sparse.is_empty() {
            true => 0,
            false => self.sparse.remove(&id).unwrap_or(0),
        }
    }

    #[cfg_attr(not(feature = "text"), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.count
    }

    #[cfg_attr(not(feature = "text"), allow(dead_code))]
    pub fn clear(&mut self) {
        *self = DocMap::default();
    }

    #[cfg_attr(not(feature = "vector"), allow(dead_code))]
    pub fn reserve(&mut self, n: usize) {
        self.dense.reserve(n);
    }

    /// Allocated bytes, approximate for the `HashMap` as [`IdIndex::bytes`].
    pub fn bytes(&self) -> usize {
        self.dense.capacity() * 4 + self.sparse.capacity() * (std::mem::size_of::<DocId>() + 4 + 1)
    }

    #[cfg_attr(not(feature = "text"), allow(dead_code))]
    pub fn shrink_to_fit(&mut self) {
        self.dense.shrink_to_fit();
        self.sparse.shrink_to_fit();
    }
}

#[derive(Default, Clone)]
pub struct Segment {
    pub data: SegmentBytes,
    pub sealed: bool,
}

/// A segment's bytes: shared on native targets, so that a store cloned for
/// a rewrite beside the database (`compact` on a server) shares its sealed
/// segments rather than copying them -- a gigabyte written since the open
/// was a gigabyte copied under the read lock. The open one is copied the
/// first time it is written to while a clone holds it. In the browser an
/// image being taken holds them the same way ([`crate::engine::Kept`]),
/// rather than a copy of every one.
pub type SegmentBytes = std::sync::Arc<Vec<u8>>;

/// Whether a frame may put document `id`, refused where frames are read --
/// an open's replay, a replica's apply -- as damage, or a primary's record
/// no write made, where it is an id no write gives: 0, which `allocate_id`
/// starts past and a put naming is given another, or `DocId::MAX`, past
/// which `next_id` has nowhere to go. Taken, 0 went into the id index's
/// sparse map, and the dense array growing over it indexed slot -1, a
/// panic; `DocId::MAX` set `next_id` to 0 -- the id the next write was
/// given -- wrapping (the fuzzer's `image` target found both).
#[inline]
pub(crate) fn written_id(id: DocId) -> Result<()> {
    match id {
        0 | DocId::MAX => Err(unwritten_id(id)),
        _ => Ok(()),
    }
}

#[cold]
#[inline(never)]
fn unwritten_id(id: DocId) -> Error {
    Error::Corrupt(format!("a frame puts document {id}, an id no write gives"))
}

/// The bytes a document's payload holds at most: a [`Loc`] keeps its
/// length in a `u32`. Every release from 0.1.0 through 0.2.0 kept a longer
/// one's length cut to its low 32 bits (`Store::append`, later
/// `land_frame`), and an open read the frame's length again and cut it so
/// too: the row read back short, or not at all. A document of two
/// `vector<1000000000>` fields reaches it natively, as a `set` copying a
/// large text into many fields does, and an import of SQLite rows holding
/// several large values.
pub const MAX_PAYLOAD: usize = u32::MAX as usize;

/// [`MAX_PAYLOAD`], or what a test's thread holds payloads to instead
/// ([`PAYLOAD_MOST`]): a document of 4 GiB is more than a test's machine
/// holds twice over.
#[cfg(all(not(test), not(target_arch = "wasm32")))]
#[inline(always)]
pub(crate) fn payload_most() -> usize {
    MAX_PAYLOAD
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn payload_most() -> usize {
    PAYLOAD_MOST.with(std::cell::Cell::get)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
thread_local! {
    /// The bytes a test's thread holds a payload to ([`payload_most`]).
    pub(crate) static PAYLOAD_MOST: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_PAYLOAD) };
}

/// Whether a write's encoded document fits a frame ([`MAX_PAYLOAD`]):
/// asked where a `put`, a `set` and an upsert encode the document, before
/// the row written over leaves its indexes, so a refusal changes nothing
/// and the block is put back whole. A query error, as every limit is. A
/// 32-bit build has none: its `Vec` stops at `isize::MAX`, 2 GiB.
#[cfg(target_pointer_width = "64")]
#[inline(always)]
pub fn payload_fits(len: usize) -> Result<()> {
    match len > payload_most() {
        true => Err(payload_past(len)),
        false => Ok(()),
    }
}

#[cfg(target_pointer_width = "64")]
#[cold]
#[inline(never)]
fn payload_past(len: usize) -> Error {
    Error::Query(format!(
        "a document is at most {} bytes encoded, a u32's length, not {len}",
        payload_most()
    ))
}

/// Whether a frame read -- an open's replay, a replica's apply -- puts a
/// payload a write could have written ([`payload_fits`]): longer is damage,
/// or a primary's record no write made.
#[cfg(target_pointer_width = "64")]
#[inline(always)]
pub(crate) fn frame_fits(len: usize) -> Result<()> {
    match len > payload_most() {
        true => Err(frame_past(len)),
        false => Ok(()),
    }
}

/// Whether the frames of a record's body `bytes` put payloads a write could
/// have written ([`frame_fits`]), read before any of them lands. Asked only
/// of a body longer than a payload may be: a frame is held to what is left
/// of its body, so a shorter body's all fit. One compare a record, where
/// one a frame would be 100 000 instructions more an open of 100 000 rows.
/// A frame that does not read is left to the walk, which refuses it.
#[cfg(target_pointer_width = "64")]
#[cold]
#[inline(never)]
pub(crate) fn frames_fit(bytes: &[u8]) -> Result<()> {
    let mut pos = 0;
    while pos < bytes.len() {
        let op = bytes[pos];
        pos += 1;
        let (Ok(_), Ok(len)) = (get_uvarint(bytes, &mut pos), get_uvarint(bytes, &mut pos)) else {
            return Ok(());
        };
        let len = len as usize;
        if len > bytes.len() - pos {
            return Ok(());
        }
        if op == OP_PUT {
            frame_fits(len)?;
        }
        pos += len;
    }
    Ok(())
}

#[cfg(target_pointer_width = "64")]
#[cold]
#[inline(never)]
fn frame_past(len: usize) -> Error {
    Error::Corrupt(format!(
        "a frame puts a payload of {len} bytes, past the {} a write takes",
        payload_most()
    ))
}

/// A frame running past the end of the data record holding it, refused as
/// a replica's apply refuses one. A record reaches a replay whole by its
/// length, and past the durable point by its sum: a crash's torn tail is
/// the record the open's walk cuts (`Database::load`), never a frame inside
/// one. Stopped there as at a torn tail -- what this replay did before
/// records had lengths -- the frames after it vanished with no word, and
/// the open went on as if the file held no more of them.
#[cold]
#[inline(never)]
fn cut_frame() -> Error {
    Error::Corrupt("a write record cut short".into())
}

/// A frame whose op is neither [`OP_PUT`] nor [`OP_DEL`], by its byte and
/// the frame's place in the file, as an unknown record kind is refused:
/// a later version's op, or damage below the durable point, which a load
/// reads unchecked. Taken as a frame that changed nothing, as every
/// release through 0.2.0 took it, the version before it was counted dead
/// while the index still pointed at it, the frame counted live, and the
/// next compact dropped it with no word.
#[cold]
#[inline(never)]
fn unknown_op(op: u8, at: u64) -> Error {
    Error::Corrupt(format!("unknown write op {op} at {at}"))
}

/// The segment's bytes to append to.
#[inline]
fn grow(b: &mut SegmentBytes) -> &mut Vec<u8> {
    std::sync::Arc::make_mut(b)
}

/// Where a store stood before a block of writes, for [`Store::rewind`] to
/// take it back there.
#[derive(Clone, Copy)]
pub struct Mark {
    segments: usize,
    /// The last segment's length, and whether it was sealed: a block that
    /// filled it sealed it and opened another.
    len: usize,
    sealed: bool,
    /// The stretches of the mapped file the store read from: a block that
    /// spilled into the file took its frames in from there
    /// ([`Store::hand_over`]), and put back, it lets them go.
    stretches: usize,
    next_id: DocId,
    dead_bytes: usize,
    total_bytes: usize,
}

impl Mark {
    /// Where the store stood before a block that has since spilled its
    /// frames into the file, which the store took them in from with the
    /// records before them: the first `stretches` stretches, and nothing in
    /// memory.
    pub fn spilled(self, stretches: usize) -> Mark {
        Mark {
            segments: 1,
            len: 0,
            sealed: false,
            stretches,
            ..self
        }
    }
}

/// Where the frames a store held in memory went when it took them in from
/// the file ([`Store::hand_over`]): a place in the segments it let go of,
/// by where its segment started in them all, moved into the run that held
/// it. What a block that spilled remembers of where a document was before
/// it wrote one, to point it back there.
#[cfg(not(target_arch = "wasm32"))]
pub struct Moved {
    starts: Vec<u64>,
    runs: Vec<(u64, u64)>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Moved {
    /// `l`'s place in the file, where it was in memory; as it is otherwise.
    pub fn loc(&self, l: Loc) -> Loc {
        if l.seg & MAPPED != 0 || l.is_empty() {
            return l;
        }
        let Some(&start) = self.starts.get(l.seg as usize) else {
            return l;
        };
        let at = start + l.off as u64;
        // The run holding it: they tile the segments, in order.
        let mut from = 0;
        for &(len, run_at) in &self.runs {
            if at < from + len {
                return Loc::mapped(run_at + (at - from), l.len);
            }
            from += len;
        }
        l
    }
}

/// Cloned, a store is what it held at that moment: the records in the
/// mapped file shared, the ones in memory copied -- what a rewrite beside
/// the database writes from, with no lock held.
#[derive(Clone)]
pub struct Store {
    segments: Vec<Segment>,
    /// Records that stayed in a mapped file rather than being copied into a
    /// segment: the file as it stood when opened, and the stretches of it
    /// that are this collection's, in order -- the ones it held then, and
    /// those of the records written since that the store took in from it
    /// ([`Self::hand_over`]) -- what `image` writes back.
    base: Option<(Base, Vec<(u64, u64)>)>,
    index: IdIndex,
    next_id: DocId,
    /// The bytes of the frames written over or deleted, and of the deletes
    /// ([`Self::dead_bytes`]): what a compact is due against.
    dead_bytes: usize,
    total_bytes: usize,
    /// The collection's dropped places (`Schema::dropped`): a field's
    /// position, as every reader passes it, is a place further in the
    /// payload for each before it. Kept here, set wherever the schema
    /// changes, so that a read of one field takes no schema.
    dropped: Vec<usize>,
    /// The dropped places every payload here holds a `null` at, or ends
    /// before: those a compact reclaimed ([`Self::reclaims`]). No write
    /// fills one again -- a write puts a `null` there, and a block put back
    /// points its ids at payloads held before it -- so the compacts after
    /// the first walk no document for them, as none walked where a compact
    /// took the places out. Not in the file: an open walks them once more.
    cleared: Vec<usize>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Store {
        Store {
            segments: vec![Segment::default()],
            base: None,
            index: IdIndex::default(),
            next_id: 1,
            dead_bytes: 0,
            total_bytes: 0,
            dropped: Vec::new(),
            cleared: Vec::new(),
        }
    }

    /// The schema's dropped places, which every read by position passes
    /// over ([`Self::dropped`]).
    pub fn set_dropped(&mut self, dropped: &[usize]) {
        self.dropped.clear();
        self.dropped.extend_from_slice(dropped);
    }

    /// Whether a payload may hold a value at a dropped place: a drop no
    /// compact has reclaimed since the store was read in ([`Self::cleared`]).
    fn reclaims(&self) -> bool {
        self.dropped != self.cleared
    }

    /// Each document holding a value at a dropped place written again as a
    /// write after the drop writes it ([`crate::schema::reclaim`]),
    /// appended as a write is: what a compact of a mapped file does before
    /// the rewrite copies the live records into the new file and points the
    /// store at it ([`Self::relocate_live`]), so that only the documents
    /// written again are held in memory meanwhile, and only once a drop.
    /// Every place stays where it is, and the documents read as they did.
    pub fn reclaim(&mut self) -> Result<()> {
        if !self.reclaims() {
            return Ok(());
        }
        let mut out = Vec::new();
        for id in self.index.ids() {
            let Some(loc) = self.index.get(id) else {
                continue;
            };
            let payload = self.payload(loc)?;
            if crate::schema::reclaims(&self.dropped, payload)
                && crate::schema::reclaim(&self.dropped, payload, &mut out)
            {
                self.append(OP_PUT, id, &out);
            }
        }
        self.cleared.clone_from(&self.dropped);
        Ok(())
    }

    /// Where the field at `pos` is in a payload.
    #[inline]
    fn place(&self, pos: usize) -> usize {
        match self.dropped.is_empty() {
            true => pos,
            false => crate::schema::place(&self.dropped, pos),
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
    pub fn next_id(&self) -> DocId {
        self.next_id
    }
    /// The bytes of the frames no read reaches: every version written over
    /// or deleted, head and payload, and every delete's own frame -- what a
    /// compact gives back.
    pub fn dead_bytes(&self) -> usize {
        self.dead_bytes
    }
    /// The bytes of every frame the store holds, live and dead.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
    /// The bytes of the live frames, kept as writes land and read in O(1):
    /// what a scrape of `/_metrics` says a collection holds. The same as
    /// [`Self::live_len`] wherever each frame's varints are the shortest,
    /// as every writer here makes them.
    pub fn live_bytes(&self) -> usize {
        self.total_bytes.saturating_sub(self.dead_bytes)
    }
    /// The record bytes held in memory: every one, unless some stayed in a
    /// mapped file, whose pages the operating system keeps or lets go of.
    pub fn heap_bytes(&self) -> usize {
        self.segments.iter().map(|s| s.data.len()).sum()
    }
    /// Bytes allocated by the offset index. Small next to the segment bytes,
    /// but a fixed per-document cost: in a collection of small documents it
    /// can reach a third of the total. A mapped store also keeps where each
    /// of its data records sits in the file, 16 bytes a record -- one a
    /// write for the tail since the last checkpoint: 16.7 of the 53.8 MB a
    /// mapped file of a million writes held, and uncounted before.
    pub fn index_bytes(&self) -> usize {
        if let Some((_, stretches)) = &self.base {
            return self.index.bytes() + stretches.capacity() * 16;
        }
        self.index.bytes()
    }
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
    pub fn contains(&self, id: DocId) -> bool {
        self.index.contains(id)
    }
    /// Document ids, **in ascending order**.
    pub fn ids(&self) -> Vec<DocId> {
        self.index.ids()
    }
    /// The same ids without collecting them: a scan that stops at its
    /// `limit` should not first build a list of every id in the collection.
    pub fn iter_ids(&self) -> impl Iterator<Item = DocId> + '_ {
        self.index.iter()
    }
    /// The ids from `from` on, ascending.
    pub fn iter_ids_from(&self, from: DocId) -> impl Iterator<Item = DocId> + '_ {
        self.index.iter_from(from)
    }
    /// Reserves room up front for a known record count.
    pub fn reserve(&mut self, n: usize) {
        self.index.reserve(n);
    }

    pub fn allocate_id(&mut self) -> DocId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Where the store stands: what [`Self::rewind`] takes it back to.
    pub fn mark(&self) -> Mark {
        let last = self.segments.last();
        Mark {
            segments: self.segments.len(),
            len: last.map_or(0, |s| s.data.len()),
            sealed: last.is_some_and(|s| s.sealed),
            stretches: self.base.as_ref().map_or(0, |(_, s)| s.len()),
            next_id: self.next_id,
            dead_bytes: self.dead_bytes,
            total_bytes: self.total_bytes,
        }
    }

    /// Where `id`'s record is, if it has one: what a block remembers of the
    /// ids it writes, to point them back.
    pub fn loc(&self, id: DocId) -> Option<Loc> {
        self.index.get(id)
    }

    /// Whether `id`'s record lies at or past `first`'s, which the running
    /// statement appended: appended since, as the segments are appended
    /// to in order. A statement's records are all in memory -- a block
    /// spills, and a store hands over to the file, only between
    /// statements -- so one in the mapped file is older.
    pub fn wrote_after(&self, first: DocId, id: DocId) -> bool {
        match (self.index.get(first), self.index.get(id)) {
            (Some(f), Some(l)) => {
                debug_assert!(f.seg & MAPPED == 0, "a statement's record is in memory");
                l.seg & MAPPED == 0 && (l.seg, l.off) >= (f.seg, f.off)
            }
            _ => false,
        }
    }

    /// Points `id` at `loc`, or at nothing: where it was before a write
    /// that is put back.
    pub fn point(&mut self, id: DocId, loc: Option<Loc>) {
        match loc {
            Some(l) => self.index.insert(id, l),
            None => drop(self.index.remove(id)),
        }
    }

    /// Takes the store back to `mark`, dropping every record appended since
    /// -- a block of writes that did not land, each id it wrote pointed
    /// back first ([`Self::point`]). A block appends to the segments, so
    /// they are cut back where they stood; one that spilled into the file
    /// had the store take those frames in from there, and the stretches are
    /// cut back as well ([`Mark::spilled`]).
    pub fn rewind(&mut self, mark: Mark) {
        if let Some((_, stretches)) = &mut self.base {
            stretches.truncate(mark.stretches);
        }
        self.segments.truncate(mark.segments.max(1));
        if let Some(last) = self.segments.last_mut() {
            if last.data.len() > mark.len {
                grow(&mut last.data).truncate(mark.len);
            }
            last.sealed = mark.sealed;
        }
        self.next_id = mark.next_id;
        self.dead_bytes = mark.dead_bytes;
        self.total_bytes = mark.total_bytes;
    }

    /// Raises the counter to at least `v`; never lowers it.
    ///
    /// The counter is normally derived from the records ([`Store::append`]),
    /// but since `compact` drops tombstones the highest deleted id vanishes
    /// from the image altogether. The persistence layer pushes that level
    /// through this gate. No lowering: handing out an id again silently
    /// binds everything holding it to the wrong row.
    pub fn raise_next_id(&mut self, v: DocId) {
        self.next_id = self.next_id.max(v);
    }

    fn active(&mut self) -> &mut Segment {
        if self.segments.last().map(|s| s.data.len()).unwrap_or(0) >= SEGMENT_MAX {
            if let Some(last) = self.segments.last_mut() {
                last.sealed = true;
                // A segment grows by doubling and is sealed just past 8 MiB,
                // so it held up to twice its records: a 1 GB file's took
                // 1.66 GB of heap. Sealed, it grows no more; one reallocation
                // per 8 MiB gives the rest back -- unless a clone holds it,
                // which a copy to give back the slack would not be worth.
                if let Some(data) = std::sync::Arc::get_mut(&mut last.data) {
                    data.shrink_to_fit();
                }
            }
            self.segments.push(Segment::default());
        }
        self.segments.last_mut().unwrap()
    }

    /// Encodes a document in schema field order, a dropped place a `null`.
    pub fn encode_doc(schema: &Schema, doc: &Document) -> Vec<u8> {
        let mut payload = Vec::with_capacity(64);
        Store::encode_doc_into(schema, doc, &mut payload);
        payload
    }

    /// [`Self::encode_doc`] onto the end of `payload`. Each value encoded
    /// where it is: copied first, a text's or a list's allocation and its
    /// freeing were 4% of a put's time in a block of 1 000.
    pub fn encode_doc_into(schema: &Schema, doc: &Document, payload: &mut Vec<u8>) {
        let mut at = 0;
        for (i, f) in schema.fields.iter().enumerate() {
            // The dropped places before this field's: none, most of the
            // time, and the loop not entered.
            while at < schema.place(i) {
                payload.push(crate::codec::TAG_NULL);
                at += 1;
            }
            let v = doc.get(&f.name).unwrap_or(&Value::Null);
            // The field type is passed along: `vector<N, f16>` halves in the record.
            encode_value_as(payload, v, Some(&f.ty));
            at += 1;
        }
    }

    /// Produces the frame (header + payload). The same byte sequence goes to
    /// the segment and to the persistent sink.
    pub fn frame(op: u8, id: DocId, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 16);
        out.push(op);
        put_uvarint(&mut out, id);
        put_uvarint(&mut out, payload.len() as u64);
        out.extend_from_slice(payload);
        out
    }

    /// Appends the frame to the active segment and updates the index.
    /// Returns the full frame written to the segment (for the persistence layer).
    pub fn append(&mut self, op: u8, id: DocId, payload: &[u8]) -> Vec<u8> {
        let frame = Store::frame(op, id, payload);
        self.land_frame(op, id, &frame, payload.len());
        frame
    }

    /// [`Self::append`] for a block's write: the frame written onto the
    /// end of `out`, the block's frames, and copied from there into the
    /// segment -- made in a `Vec` of its own and copied twice, its
    /// allocation and its freeing were 6% of a put's time under the write
    /// lock in a block of 1 000.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn append_onto(&mut self, op: u8, id: DocId, payload: &[u8], out: &mut Vec<u8>) {
        let from = out.len();
        out.push(op);
        put_uvarint(out, id);
        put_uvarint(out, payload.len() as u64);
        out.extend_from_slice(payload);
        self.land_frame(op, id, &out[from..], payload.len());
    }

    /// [`Self::append`] of a frame as a record holds it, `payload_len` its
    /// payload's bytes at its end: a replica's apply takes its primary's
    /// frames as they came. Made again in a `Vec` of their own, allocated,
    /// copied and freed, they took 660 to 680 instructions a record, more
    /// than the record's put-back costs (`docs/design/engine.md`, *Blocks*).
    pub fn append_frame(&mut self, op: u8, id: DocId, frame: &[u8], payload_len: usize) {
        self.land_frame(op, id, frame, payload_len);
    }

    /// The frame put in the active segment and the index pointed at it.
    #[inline(always)]
    fn land_frame(&mut self, op: u8, id: DocId, frame: &[u8], payload_len: usize) {
        let seg_ix = {
            let _ = self.active();
            self.segments.len() - 1
        };
        let seg = &mut self.segments[seg_ix];
        let header_len = frame.len() - payload_len;
        let off = seg.data.len() + header_len;
        let data = grow(&mut seg.data);
        // Doubled as a `Vec` would be, but never past the record that
        // seals it: doubled past 8 MiB, a segment of 768-dim documents took
        // 12.7 MB beside the 6.3 it was copied out of, and the browser
        // module's memory never gives a peak back.
        let need = data.len() + frame.len();
        if data.capacity() < need {
            let to = (data.capacity() * 2)
                .min(SEGMENT_MAX + frame.len())
                .max(need);
            data.reserve_exact(to - data.len());
        }
        data.extend_from_slice(frame);
        self.total_bytes += frame.len();

        // Each arm counts the version it leaves dead: counted before the
        // match, a frame of neither op left its row's version counted dead
        // with the index still pointing at it.
        match op {
            OP_PUT => {
                if let Some(old) = self.index.get(id) {
                    self.dead_bytes += dead_frame(id, old);
                }
                self.index.insert(
                    id,
                    Loc {
                        seg: seg_ix as u32,
                        off: off as u32,
                        len: payload_len as u32,
                    },
                );
                if id >= self.next_id {
                    self.next_id = id + 1;
                }
            }
            OP_DEL => {
                if let Some(old) = self.index.remove(id) {
                    self.dead_bytes += dead_frame(id, old);
                }
                self.dead_bytes += frame.len();
            }
            // Never another: the write path hands `append` and
            // `append_onto` one of the two, and a replica's apply hands
            // `append_frame` a primary's frame only once it read its op as
            // one of them (`Database::apply_frames` refuses any other before
            // the frame lands). A replay refuses one itself
            // ([`unknown_op`]).
            _ => debug_assert!(false, "a frame of op {op} landed"),
        }
    }

    #[inline]
    fn payload(&self, loc: Loc) -> Result<&[u8]> {
        if loc.seg & MAPPED != 0 {
            // A store over a file has one, and in the browser one over the
            // image it was loaded from (`Database::load_mapped`).
            if let Some((b, _)) = &self.base {
                let at = (((loc.seg & !MAPPED) as u64) << 32 | loc.off as u64) as usize;
                let file: &[u8] = (**b).as_ref();
                return file
                    .get(at..at + loc.len as usize)
                    .ok_or_else(|| Error::Corrupt("offset outside the mapped file".into()));
            }
        }
        let seg = self
            .segments
            .get(loc.seg as usize)
            .ok_or_else(|| Error::Corrupt("no such segment".into()))?;
        let s = loc.off as usize;
        let e = s + loc.len as usize;
        seg.data
            .get(s..e)
            .ok_or_else(|| Error::Corrupt("offset outside the segment".into()))
    }

    /// The payload held for `id`, as it was written: what a rewrite copies
    /// without decoding it.
    pub fn raw(&self, id: DocId) -> Result<Option<&[u8]>> {
        match self.index.get(id) {
            Some(loc) => self.payload(loc).map(Some),
            None => Ok(None),
        }
    }

    /// The documents of `ids` as they stand, to be copied once the lock
    /// they were looked up under is let go: a payload in the mapped file by
    /// its place -- the file is only appended to, so the bytes there stay
    /// what they are -- and one held in memory copied out, since a write
    /// may change its segment. Copied under the read lock, a catch-up's
    /// page faults on the file held the writers out with it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn snapshot(&self, ids: &[DocId], into: &mut Snapshot) -> Result<()> {
        into.docs.clear();
        into.held.clear();
        into.base = self.base.as_ref().map(|(b, _)| b.clone());
        for &id in ids {
            let place = match self.index.get(id) {
                None => Place::Gone,
                Some(l) if l.seg & MAPPED != 0 && into.base.is_some() => {
                    Place::File(((l.seg & !MAPPED) as u64) << 32 | l.off as u64, l.len)
                }
                Some(l) => {
                    let from = into.held.len();
                    into.held.extend_from_slice(self.payload(l)?);
                    Place::Held(from, l.len)
                }
            };
            into.docs.push((id, place));
        }
        Ok(())
    }

    /// Decodes the whole document: a dropped place passed over, and a
    /// field the payload ends before -- added after it was written -- read
    /// as `null` ([`Schema::read_doc`]).
    pub fn read(&self, schema: &Schema, id: DocId) -> Result<Option<Document>> {
        let Some(loc) = self.index.get(id) else {
            return Ok(None);
        };
        schema.read_doc(id, self.payload(loc)?).map(Some)
    }

    /// [`Self::read`] of `id`'s record where [`Self::loc`] found it: a
    /// replica's apply remembers the place before it reads the row.
    pub fn read_at(&self, schema: &Schema, id: DocId, loc: Loc) -> Result<Document> {
        schema.read_doc(id, self.payload(loc)?)
    }

    /// Decodes a single field; the fields before it are skipped without
    /// allocating. Filter evaluation and projection use this path. A
    /// payload that ends before it holds `null` there: the field was added
    /// after the document was written, which no document is rewritten for.
    pub fn read_field(&self, id: DocId, field_pos: usize) -> Result<Option<Value>> {
        self.read_field_in::<false>(id, field_pos)
    }

    /// Whether a read of the field at `field_pos` passes a dropped place:
    /// what picks the reader that passes its `null` by its byte
    /// ([`Self::read_field_in`]), once a query.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn passes(&self, field_pos: usize) -> bool {
        self.dropped
            .first()
            .is_some_and(|&d| d < self.place(field_pos))
    }

    /// [`Self::read_field`], each value it passes stepped over by
    /// [`crate::codec::skip_passing`] where `PASS` says: an order by a field
    /// past a dropped place paid a call into the skip, 44 instructions, a
    /// row. A second copy where a caller asks for one, as
    /// [`Self::read_fields_passing`] is, so that no other collection's read
    /// asks which.
    #[inline(always)]
    pub(crate) fn read_field_in<const PASS: bool>(
        &self,
        id: DocId,
        field_pos: usize,
    ) -> Result<Option<Value>> {
        let Some(loc) = self.index.get(id) else {
            return Ok(None);
        };
        let buf = self.payload(loc)?;
        let mut pos = 0;
        for _ in 0..self.place(field_pos) {
            match PASS {
                true => crate::codec::skip_passing(buf, &mut pos)?,
                false => crate::codec::skip_field(buf, &mut pos)?,
            }
        }
        match pos < buf.len() {
            true => Ok(Some(decode_value(buf, &mut pos)?)),
            false => Ok(Some(Value::Null)),
        }
    }

    /// The value at `keys` (`source.rank`) inside the `json` field at
    /// `field_pos`: `null` where the path leads nowhere or the document
    /// ends before the field, `None` when there is no such document.
    pub fn read_path(&self, id: DocId, field_pos: usize, keys: &str) -> Result<Option<Value>> {
        let mut v = [Value::Null];
        Ok(self
            .read_paths(id, &[(field_pos, keys)], &mut v)?
            .then(|| std::mem::replace(&mut v[0], Value::Null)))
    }

    /// The values at `paths` -- each a json field's position and the keys
    /// past it -- into `out`, from one look-up of the document: the fields
    /// before each skipped once, and two paths into one field reading it
    /// from where the first found it. A text lands in the text its slot
    /// held, as [`Self::read_fields`] has it. `false` when there is no such
    /// document.
    pub fn read_paths(
        &self,
        id: DocId,
        paths: &[(usize, &str)],
        out: &mut [Value],
    ) -> Result<bool> {
        let Some(loc) = self.index.get(id) else {
            return Ok(false);
        };
        let buf = self.payload(loc)?;
        let (mut at, mut pos) = (0usize, 0usize);
        for (slot, &(field, keys)) in out.iter_mut().zip(paths) {
            let want = self.place(field);
            if want < at {
                (at, pos) = (0, 0);
            }
            while at < want {
                crate::codec::skip_field(buf, &mut pos)?;
                at += 1;
            }
            match pos < buf.len() {
                true => crate::codec::decode_path_into(buf, pos, keys, slot)?,
                false => *slot = Value::Null,
            }
        }
        Ok(true)
    }

    /// Decodes the values at `places` -- ascending, each a field's place in
    /// the payload as [`Schema::place`] gives it -- into `out`, in one pass
    /// over the document that skips the others: an aggregate reads two or
    /// three fields of every row, and a `read_field` each would skip the
    /// fields before them once per field. A text lands in the text `out`
    /// held there, if it did. `false` when there is no such document.
    ///
    /// Places rather than positions: worked out once a query by the caller,
    /// not once a row here -- the compare that tells a collection with no
    /// dropped field took a scan of a million rows by one condition 21.2 ->
    /// 21.8 ms.
    pub fn read_fields(&self, id: DocId, places: &[usize], out: &mut Vec<Value>) -> Result<bool> {
        self.read_fields_in::<false>(id, places, out)
    }

    /// [`Self::read_fields`] of a collection holding a dropped place before
    /// one of `places`, every `null` it passes stepped over by its byte
    /// ([`crate::codec::skip_passing`]): every write after a drop, and
    /// every compact, makes a dropped place's value one, and a call into
    /// the skip for it was 44 instructions a row. A second copy of the
    /// reader rather than a test in the one every scan runs, which every
    /// other collection's rows would pay.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_fields_passing(
        &self,
        id: DocId,
        places: &[usize],
        out: &mut Vec<Value>,
    ) -> Result<bool> {
        self.read_fields_in::<true>(id, places, out)
    }

    /// [`Self::read_fields`], each value it passes stepped over by
    /// [`crate::codec::skip_passing`] where `PASS` says.
    #[inline(always)]
    fn read_fields_in<const PASS: bool>(
        &self,
        id: DocId,
        places: &[usize],
        out: &mut Vec<Value>,
    ) -> Result<bool> {
        let Some(loc) = self.index.get(id) else {
            return Ok(false);
        };
        let buf = self.payload(loc)?;
        // Slots past the places are the caller's -- a filter keeps its
        // paths' values there -- and are left as they are.
        let mut pos = 0usize;
        let mut at = 0usize;
        for (i, &want) in places.iter().enumerate() {
            // Past the payload's end nothing is skipped: a field added
            // after the document was written is not in it, and reads as
            // `null` below.
            while at < want {
                match PASS {
                    true => crate::codec::skip_passing(buf, &mut pos)?,
                    false => crate::codec::skip_field(buf, &mut pos)?,
                }
                at += 1;
            }
            // A text into the text the slot held: a scan reads a field of
            // every row into the same slot, and a text decoded anew was a
            // malloc and a free a row. Anything else decoded as it was.
            match (buf.get(pos), out.get_mut(i)) {
                (Some(&crate::codec::TAG_TEXT), Some(Value::Text(s))) => {
                    pos += 1;
                    crate::codec::decode_text_into(buf, &mut pos, s)?;
                }
                // A point into the two floats its slot held, as a text:
                // decoded anew it was a list allocated and freed a row.
                (Some(&crate::codec::TAG_GEO), Some(Value::List(p))) if p.len() == 2 => {
                    let raw = buf
                        .get(pos + 1..pos + 17)
                        .ok_or_else(|| Error::Corrupt("point outside the segment".into()))?;
                    let (lon, lat) = crate::codec::geo_at(raw);
                    (p[0], p[1]) = (Value::Float(lon), Value::Float(lat));
                    pos += 17;
                }
                (None, Some(slot)) => *slot = Value::Null,
                (None, None) => out.push(Value::Null),
                (_, Some(slot)) => *slot = decode_value(buf, &mut pos)?,
                (_, None) => out.push(decode_value(buf, &mut pos)?),
            }
            at += 1;
        }
        Ok(true)
    }

    /// Decodes a vector field into the given buffer -- with no intermediate
    /// allocation.
    ///
    /// Allocating a `Vec<f32>` per node while restoring the graph was a
    /// noticeable part of the startup time; here a single buffer is reused.
    /// The stored document's bytes from the value at `field_pos` on, or
    /// `None` when there is no such document.
    fn field_at(&self, id: DocId, field_pos: usize) -> Result<Option<&[u8]>> {
        let Some(loc) = self.index.get(id) else {
            return Ok(None);
        };
        let buf = self.payload(loc)?;
        let mut pos = 0usize;
        for _ in 0..self.place(field_pos) {
            crate::codec::skip_field(buf, &mut pos)?;
        }
        Ok(Some(&buf[pos.min(buf.len())..]))
    }

    /// Whether the stored document holds a vector at `field_pos`, read off
    /// the value's tag without decoding it.
    pub fn has_vector(&self, id: DocId, field_pos: usize) -> Result<bool> {
        Ok(matches!(
            self.field_at(id, field_pos)?.and_then(|b| b.first()),
            Some(&(crate::codec::TAG_VECTOR | crate::codec::TAG_VECTOR_F16))
        ))
    }

    /// The point the stored document holds at `field_pos`, its 16 bytes read
    /// where they lie; `None` for a document that holds none there, or
    /// none at all.
    pub fn read_point(&self, id: DocId, field_pos: usize) -> Result<Option<(f64, f64)>> {
        let Some(buf) = self.field_at(id, field_pos)? else {
            return Ok(None);
        };
        match (buf.first(), buf.get(1..17)) {
            (Some(&crate::codec::TAG_GEO), Some(raw)) => Ok(Some(crate::codec::geo_at(raw))),
            (Some(&crate::codec::TAG_GEO), None) => {
                Err(Error::Corrupt("point outside the segment".into()))
            }
            _ => Ok(None),
        }
    }

    pub fn read_vector_into(
        &self,
        id: DocId,
        field_pos: usize,
        out: &mut Vec<f32>,
    ) -> Result<bool> {
        let Some(buf) = self.field_at(id, field_pos)? else {
            return Ok(false);
        };
        let mut pos = 0usize;
        // A `vector<N, f16>` field is stored halved, under its own tag.
        // Reading only the f32 one, every such field looked empty: `rerank`
        // over it returned no rows, and a restore found no vector for any
        // node and rebuilt the graph on every open.
        let half = match buf.get(pos) {
            Some(&crate::codec::TAG_VECTOR) => false,
            Some(&crate::codec::TAG_VECTOR_F16) => true,
            _ => return Ok(false),
        };
        pos += 1;
        let n = crate::codec::get_uvarint(buf, &mut pos)? as usize;
        // Its bytes counted without overflowing, as the codec counts them
        // (`codec::decode_value`): `n * 4` of a count near 2^62 overflowed
        // -- in a release build it wrapped to a few bytes, and the reserve
        // below asked for the count -- reached from an open restoring a
        // graph (the fuzzer's `image` target).
        let end = n
            .checked_mul(if half { 2 } else { 4 })
            .and_then(|b| b.checked_add(pos))
            .filter(|&end| end <= buf.len())
            .ok_or_else(|| Error::Corrupt("vector outside the segment".into()))?;
        out.clear();
        out.reserve(n);
        if half {
            // Widened as the arena widens, without a branch: the vectors read
            // here are for measuring, and through `codec::f32_from_f16`'s
            // subnormal branch the loop stayed scalar -- ordering a quantized
            // index's candidates took most of its query that way.
            let (words, _) = buf[pos..end].as_chunks::<2>();
            out.extend(
                words
                    .iter()
                    .map(|w| crate::vector::half(u16::from_le_bytes(*w))),
            );
        } else {
            let (words, _) = buf[pos..end].as_chunks::<4>();
            out.extend(words.iter().map(|w| f32::from_le_bytes(*w)));
        }
        Ok(true)
    }

    /// Replays every record from a file/byte image.
    pub fn replay(&mut self, bytes: &[u8]) -> Result<usize> {
        self.replay_noting(bytes, 0, &mut |_| {})
    }

    /// [`Self::replay`], handing each record's id to `note` -- how an open
    /// learns which documents the writes after a checkpoint touched. `at`
    /// is where `bytes` start in the file, for a refusal to say where.
    ///
    /// Each frame is copied as it stands into the segment `append` would
    /// have put it in, with the bookkeeping `append` does: framed afresh
    /// into a `Vec` of its own first, then copied again, a record at a time,
    /// it was 17 of a 25 ms load of 100 000 x 128 read into memory.
    pub fn replay_noting(
        &mut self,
        bytes: &[u8],
        at: u64,
        note: &mut dyn FnMut(DocId),
    ) -> Result<usize> {
        #[cfg(target_pointer_width = "64")]
        if bytes.len() > payload_most() {
            frames_fit(bytes)?;
        }
        let mut pos = 0usize;
        let mut count = 0usize;
        while pos < bytes.len() {
            let start = pos;
            let op = bytes[pos];
            pos += 1;
            let id = get_uvarint(bytes, &mut pos)?;
            let len = get_uvarint(bytes, &mut pos)? as usize;
            // Held to what is left rather than added: a length near 2^64
            // wrapped `pos + len` past the test, and the frame was sliced
            // backwards (the fuzzer's `image` target found it).
            if len > bytes.len() - pos {
                return Err(cut_frame());
            }
            let frame = &bytes[start..pos + len];
            pos += len;
            let rest = bytes.len() - start;
            // The op matched before the frame changes anything, once: an op
            // of neither is refused with the store as it was, and each arm
            // counts the version it leaves dead.
            match op {
                OP_PUT => {
                    written_id(id)?;
                    if let Some(old) = self.index.get(id) {
                        self.dead_bytes += dead_frame(id, old);
                    }
                    let (seg, off) = self.copy_frame(frame, len, rest);
                    let loc = Loc {
                        seg,
                        off: off as u32,
                        len: len as u32,
                    };
                    self.index.insert(id, loc);
                    if id >= self.next_id {
                        self.next_id = id + 1;
                    }
                }
                OP_DEL => {
                    if let Some(old) = self.index.remove(id) {
                        self.dead_bytes += dead_frame(id, old);
                    }
                    self.copy_frame(frame, len, rest);
                    self.dead_bytes += frame.len();
                }
                _ => return Err(unknown_op(op, at + start as u64)),
            }
            note(id);
            count += 1;
        }
        Ok(count)
    }

    /// A replayed frame, its payload `len` bytes at its end, copied into
    /// the active segment and counted: the segment and where the payload
    /// starts in it. `rest`: the bytes the load has left, this frame's
    /// first. Natively in each of `replay_noting`'s arms; the browser
    /// module holds it once, a call.
    #[cfg_attr(not(target_arch = "wasm32"), inline(always))]
    fn copy_frame(&mut self, frame: &[u8], len: usize, rest: usize) -> (u32, usize) {
        // `active`'s test here and its seal a call, once a segment: with
        // `active` in both arms, seal and all, it was a call every frame,
        // 16 instructions a frame more than the replay before, which
        // inlined it in one place (`fs::open_in_memory` of `file-open`'s
        // 100 000 rows, +1.3%); so, the same open counts 1.5% fewer. The
        // browser module calls `active` from its one copy: the test apart
        // grew it 122 bytes.
        #[cfg(not(target_arch = "wasm32"))]
        let seg = {
            let last = self.segments.len() - 1;
            match self.segments[last].data.len() < SEGMENT_MAX {
                true => &mut self.segments[last],
                false => self.active(),
            }
        };
        #[cfg(target_arch = "wasm32")]
        let seg = self.active();
        // A segment the load opens is sized for the records it will hold,
        // rather than grown by doubling past them.
        if seg.data.is_empty() {
            grow(&mut seg.data).reserve(rest.min(SEGMENT_MAX + len));
        }
        let off = seg.data.len() + (frame.len() - len);
        grow(&mut seg.data).extend_from_slice(frame);
        self.total_bytes += frame.len();
        (self.segments.len() as u32 - 1, off)
    }

    /// [`Self::replay_noting`] over records that stay where they are: the
    /// `len` bytes at `at` in `base`, the mapped file. The index points into
    /// the file and nothing is copied, so a record is read from pages the
    /// operating system brings in, and can drop again, rather than from
    /// memory the process holds.
    pub fn replay_mapped(
        &mut self,
        base: &Base,
        at: u64,
        len: u64,
        note: &mut dyn FnMut(DocId),
    ) -> Result<usize> {
        let file: &[u8] = (**base).as_ref();
        let bytes = file
            .get(at as usize..(at + len) as usize)
            .ok_or_else(|| Error::Corrupt("data record outside the mapped file".into()))?;
        // A frame's length held to a location's before one is cut to it.
        #[cfg(target_pointer_width = "64")]
        if bytes.len() > payload_most() {
            frames_fit(bytes)?;
        }
        let mut pos = 0usize;
        let mut count = 0usize;
        while pos < bytes.len() {
            let start = pos;
            let op = bytes[pos];
            pos += 1;
            let id = get_uvarint(bytes, &mut pos)?;
            let plen = get_uvarint(bytes, &mut pos)? as usize;
            // Held to what is left, as in `replay_noting`.
            if plen > bytes.len() - pos {
                return Err(cut_frame());
            }
            // The bookkeeping `append` does, with the payload left in place,
            // the op matched first as in `replay_noting`. The frame's length
            // is worked out in each arm: worked out before the match, beside
            // the start the refusal names, it took the register holding the
            // record's bytes, reloaded each frame -- an instruction a record
            // more, 100 000 an open of `file-open`'s 100 000 rows.
            match op {
                OP_PUT => {
                    written_id(id)?;
                    if let Some(old) = self.index.get(id) {
                        self.dead_bytes += dead_frame(id, old);
                    }
                    self.total_bytes += pos + plen - start;
                    self.index
                        .insert(id, Loc::mapped(at + pos as u64, plen as u32));
                    if id >= self.next_id {
                        self.next_id = id + 1;
                    }
                }
                OP_DEL => {
                    if let Some(old) = self.index.remove(id) {
                        self.dead_bytes += dead_frame(id, old);
                    }
                    let frame_len = pos + plen - start;
                    self.total_bytes += frame_len;
                    self.dead_bytes += frame_len;
                }
                _ => return Err(unknown_op(op, at + start as u64)),
            }
            pos += plen;
            note(id);
            count += 1;
        }
        let (_, stretches) = self.base.get_or_insert_with(|| (base.clone(), Vec::new()));
        stretches.push((at, pos as u64));
        Ok(count)
    }

    /// Points the store at the file a checkpoint has just written its image
    /// into, `at` where its data record's body starts. The image is the
    /// store's frames as they stand -- the mapped stretches, then the
    /// segments -- so each location moves by where its stretch or segment
    /// landed, and the new file is not read: walking its frames took 1.8 to
    /// 2.3 s of a 1 GB file's checkpoint, all of it under the write lock.
    // In the browser, where `Base` is a type of no value, the body is
    // unreachable, which is the point.
    #[allow(unreachable_code)]
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
    pub fn relocate_image(&mut self, base: &Base, at: u64) {
        let (stretches, segments, len) = self.image_starts();
        self.index
            .relocate(|_, l| Loc::mapped(at + image_place(l, &stretches, &segments), l.len));
        self.base = Some((base.clone(), vec![(at, len)]));
        self.segments = vec![Segment::default()];
    }

    /// Where each stretch of the mapped file -- by where it was in the file
    /// -- and each segment starts in an image of the store's frames as they
    /// stand ([`Self::write_image`]), and the image's length.
    fn image_starts(&self) -> (Vec<(u64, u64)>, Vec<u64>, u64) {
        let mut stretches: Vec<(u64, u64)> = Vec::new();
        let mut to = 0;
        if let Some((_, old)) = &self.base {
            for &(from, len) in old {
                stretches.push((from, to));
                to += len;
            }
        }
        let mut segments = Vec::with_capacity(self.segments.len());
        for s in &self.segments {
            segments.push(to);
            to += s.data.len() as u64;
        }
        (stretches, segments, to)
    }

    /// Appends to `out` an index of the data record an image writes of this
    /// store -- its frames as they stand, or with `live` the live ones
    /// framed afresh in id order ([`Self::write_live`]): the record's
    /// length, the bytes in it that are dead, and for each live document in
    /// id order its id less the one before, its payload's length, and where
    /// the payload starts less where it would if its frame followed the one
    /// before ([`after`]) -- nothing, as a record is written, and a byte a
    /// document for an image of 590-byte records where the offset itself
    /// took two. An open takes the locations from it ([`Self::adopt_index`])
    /// rather than walking the frames' heads, each of which says where the
    /// next one is: a chain of cache misses, 5.5 of a 14.2 ms open at
    /// 100 000 x 128.
    ///
    /// After the documents, where each `INDEX_SHARE` of them ends -- the
    /// byte of the index, the id and the place the next share counts from
    /// -- eight bytes each, and how many: each a share an open reads on a
    /// thread of its own ([`FRAMES_SPLIT`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn image_index(&self, live: bool, out: &mut Vec<u8>) {
        let (stretches, segments, len) = self.image_starts();
        let (len, dead) = match live {
            true => (self.live_len() as u64, 0),
            false => (len, self.dead_bytes as u64),
        };
        let from = out.len();
        out.push(FRAMES_SPLIT);
        put_uvarint(out, len);
        put_uvarint(out, dead);
        put_uvarint(out, self.index.len() as u64);
        let mut head = Vec::with_capacity(16);
        let (mut id_was, mut pos_was, mut to) = (0, 0, 0);
        let mut ends = Vec::with_capacity(self.index.len().div_ceil(INDEX_SHARE));
        for (written, (id, l)) in self.index.walk(0).enumerate() {
            if written > 0 && written % INDEX_SHARE == 0 {
                ends.push([(out.len() - from) as u64, id_was, pos_was]);
            }
            let pos = match live {
                true => {
                    head.clear();
                    head.push(OP_PUT);
                    put_uvarint(&mut head, id);
                    put_uvarint(&mut head, l.len as u64);
                    to += head.len() as u64;
                    let pos = to;
                    to += l.len as u64;
                    pos
                }
                false => image_place(l, &stretches, &segments),
            };
            put_uvarint(out, id - id_was);
            put_uvarint(out, l.len as u64);
            let expected = after(pos_was, id, l.len as u64);
            put_uvarint(out, crate::codec::zigzag(pos as i64 - expected as i64));
            (id_was, pos_was) = (id, pos + l.len as u64);
        }
        ends.push([(out.len() - from) as u64, id_was, pos_was]);
        for v in ends.iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(ends.len() as u32).to_le_bytes());
    }

    /// Takes a data record's frames from its image's index
    /// ([`Self::image_index`]) rather than walking their heads, the record
    /// `len` bytes at `at` of the mapped file: `false`, the store untouched,
    /// where the index does not describe that record -- a store holding
    /// records already, another length, ids not ascending, a payload past
    /// the record's end, bytes left over -- and the caller walks it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn adopt_index(&mut self, base: &Base, at: u64, len: u64, index: &[u8]) -> bool {
        if !self.index.is_empty() || self.base.is_some() || self.total_bytes != 0 {
            return false;
        }
        // The documents' part, and the shares' ends behind it where the
        // index carries them.
        let (docs, ends) = match index.first() {
            Some(&FRAMES_MARK) => (index, &[][..]),
            Some(&FRAMES_SPLIT) => {
                let Some(at) = index.len().checked_sub(4) else {
                    return false;
                };
                let k =
                    u32::from_le_bytes([index[at], index[at + 1], index[at + 2], index[at + 3]]);
                let Some(from) = (k as usize).checked_mul(24).and_then(|t| at.checked_sub(t))
                else {
                    return false;
                };
                (&index[..from], &index[from..at])
            }
            _ => return false,
        };
        let mut p = 1;
        let (Some(whole), Some(dead), Some(count)) = (
            uvarint(docs, &mut p),
            uvarint(docs, &mut p),
            uvarint(docs, &mut p),
        ) else {
            return false;
        };
        if whole != len || dead > len {
            return false;
        }
        // Read on every core where the shares say where each starts and
        // the ids leave no room for the sparse map, and in turn where not
        // -- or where they say wrong, the index then judged whole as it
        // always was.
        let shares = (ends.len() > 1)
            .then(|| adopt_shares(docs, p, ends, count, at, len))
            .flatten();
        let Some((fresh, last)) = shares.or_else(|| adopt_in_turn(docs, p, count, at, len)) else {
            return false;
        };
        self.index = fresh;
        self.total_bytes = len as usize;
        self.dead_bytes = dead as usize;
        self.next_id = self.next_id.max(last + 1);
        self.base = Some((base.clone(), vec![(at, len)]));
        true
    }

    /// [`Self::relocate_image`] for the image [`Self::write_live`] wrote: the
    /// live documents alone, in `iter`'s order, each a put -- so each lands
    /// where the frames before it end.
    // In the browser, where `Base` is a type of no value, the body is
    // unreachable, which is the point.
    #[allow(unreachable_code)]
    pub fn relocate_live(&mut self, base: &Base, at: u64) {
        let mut head = Vec::with_capacity(16);
        let mut to = at;
        self.index.relocate(|id, l| {
            head.clear();
            head.push(OP_PUT);
            put_uvarint(&mut head, id);
            put_uvarint(&mut head, l.len as u64);
            to += head.len() as u64;
            let moved = Loc::mapped(to, l.len);
            to += l.len as u64;
            moved
        });
        self.total_bytes = (to - at) as usize;
        self.dead_bytes = 0;
        self.base = Some((base.clone(), vec![(at, to - at)]));
        self.segments = vec![Segment::default()];
    }

    /// Takes the records the segments hold in from the file, and lets the
    /// segments go: the file holds each run of this store's frames a record
    /// appended since it was last read or rewritten, `(length, place)` in
    /// `landed`, in the order they were appended. Held until a restart or a
    /// compact, the documents written since the open were 787 MB of 250 000
    /// 768-dim ones, beside the 768 MB of their vectors the graph holds. The
    /// file holds the same bytes, so a document reads the same from it;
    /// `base` maps it as far as it now goes.
    ///
    /// `landed` has to account for every frame the segments hold, in order,
    /// or nothing moves and `false` comes back: frames no record of the file
    /// holds -- a compact's, built in memory -- stay where they are, and so
    /// do the ones after them, whose place in an image comes after theirs.
    /// Either way the store reads from `base`, the same file mapped further.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn hand_over(&mut self, base: &Base, landed: &[(u64, u64)]) -> bool {
        self.hand_over_keeping(base, landed, 0).is_some()
    }

    /// [`Self::hand_over`], saying where the segments' frames went
    /// ([`Moved`]) and how many stretches the store held once it took the
    /// first `keep` runs in -- a block's spill hands over the records that
    /// landed before the block with its own frames, and a rollback keeps
    /// the ones before it ([`Mark::spilled`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn hand_over_keeping(
        &mut self,
        base: &Base,
        landed: &[(u64, u64)],
        keep: usize,
    ) -> Option<(Moved, usize)> {
        let moved = self.landed_places(landed);
        if let Some((b, _)) = &mut self.base {
            *b = base.clone();
        }
        let moved = moved?;
        let mut starts = Vec::with_capacity(self.segments.len());
        let mut to = 0;
        for s in &self.segments {
            starts.push(to);
            to += s.data.len() as u64;
        }
        let kept = self.base.as_ref().map_or(0, |(_, s)| s.len());
        if landed.is_empty() {
            let runs = Vec::new();
            return Some((Moved { starts, runs }, kept));
        }
        for (id, loc) in moved {
            self.index.insert(id, loc);
        }
        let (_, stretches) = self.base.get_or_insert_with(|| (base.clone(), Vec::new()));
        let mut kept = stretches.len();
        for (k, &(len, at)) in landed.iter().enumerate() {
            if len > 0 {
                // Never across `keep`: a rollback cuts the stretches there.
                match stretches.last_mut() {
                    Some(last) if last.0 + last.1 == at && k != keep => last.1 += len,
                    _ => stretches.push((at, len)),
                }
            }
            if k + 1 == keep {
                kept = stretches.len();
            }
        }
        self.segments = vec![Segment::default()];
        let runs = landed.to_vec();
        Some((Moved { starts, runs }, kept))
    }

    /// Whether runs of these lengths account for every frame the segments
    /// hold, in order: what [`Self::hand_over`] asks before anything moves,
    /// asked before the records are written.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn would_hand_over(&self, lens: &[u64]) -> bool {
        let runs: Vec<(u64, u64)> = lens.iter().map(|&l| (l, 0)).collect();
        self.landed_places(&runs).is_some()
    }

    /// Where in the file each document the segments hold its version of
    /// goes, by [`Self::hand_over`]'s `landed`; `None` where the runs do not
    /// account for the segments frame for frame. Nothing moves here: a run
    /// that fails half way leaves the store as it was.
    #[cfg(not(target_arch = "wasm32"))]
    fn landed_places(&self, landed: &[(u64, u64)]) -> Option<Vec<(DocId, Loc)>> {
        let held: u64 = self.segments.iter().map(|s| s.data.len() as u64).sum();
        if landed.iter().map(|l| l.0).sum::<u64>() != held {
            return None;
        }
        let mut moved = Vec::new();
        let (mut s, mut o) = (0usize, 0usize);
        for &(len, at) in landed {
            let (mut left, mut at) = (len as usize, at);
            while left > 0 {
                // A run a seal split goes on at the start of the next
                // segment: a frame is never split, `append` seals first.
                while s < self.segments.len() && o == self.segments[s].data.len() {
                    (s, o) = (s + 1, 0);
                }
                let seg = self.segments.get(s)?;
                let n = left.min(seg.data.len() - o);
                let run = &seg.data[o..o + n];
                let mut p = 0;
                while p < n {
                    let op = run[p];
                    p += 1;
                    let id = get_uvarint(run, &mut p).ok()?;
                    let plen = get_uvarint(run, &mut p).ok()? as usize;
                    if p + plen > n {
                        return None;
                    }
                    // The version the store reads is this one: a later
                    // write of the id points elsewhere, and a delete
                    // nowhere.
                    let was = Loc {
                        seg: s as u32,
                        off: (o + p) as u32,
                        len: plen as u32,
                    };
                    if op == OP_PUT && self.index.get(id) == Some(was) {
                        moved.push((id, Loc::mapped(at + p as u64, plen as u32)));
                    }
                    p += plen;
                }
                (o, left, at) = (o + n, left - n, at + n as u64);
            }
        }
        Some(moved)
    }

    /// The store as it stands, for a long read to run on once the lock is
    /// let go ([`crate::engine::Pinned`]): the id index copied, the
    /// segments and the mapped file shared -- a write copies the open
    /// segment the first time it appends to it while a pin holds it, and a
    /// handover or a compact lets go of what the pin still reads -- and the
    /// stretches left out. They say where the records are for an image to
    /// write them, which a read never does, and a tail of single writes
    /// held 16 bytes of them a write.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn pinned(&self) -> Store {
        self.pinned_with(self.index.clone())
    }

    /// The store as [`Self::pinned`] takes it, with the id index `index`.
    #[cfg(not(target_arch = "wasm32"))]
    fn pinned_with(&self, index: IdIndex) -> Store {
        Store {
            segments: self.segments.clone(),
            base: self.base.as_ref().map(|(b, _)| (b.clone(), Vec::new())),
            index,
            next_id: self.next_id,
            dead_bytes: self.dead_bytes,
            total_bytes: self.total_bytes,
            dropped: self.dropped.clone(),
            cleared: Vec::new(),
        }
    }

    /// [`Self::pinned`] of the documents `ids` alone, those of them the
    /// store holds: the id index of a few rows rather than a copy of the
    /// whole one, for a read of those rows on a pin ([`crate::engine::Pinned`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn pinned_rows(&self, ids: &[DocId]) -> Store {
        let mut index = IdIndex::default();
        for &id in ids {
            if let Some(at) = self.index.get(id) {
                index.insert(id, at);
            }
        }
        self.pinned_with(index)
    }

    /// What [`Self::pinned`] copies: the id index.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn pinned_bytes(&self) -> usize {
        self.index.bytes()
    }

    /// The ids the id index holds in its sparse map rather than its array:
    /// those a walk in id order collects and sorts first.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sparse_ids(&self) -> usize {
        self.index.sparse.len()
    }

    /// A store the rewrite wrote no data record for: nothing live, and no
    /// hold on the file it read from.
    pub fn let_go(&mut self) {
        self.base = None;
        self.segments = vec![Segment::default()];
        self.total_bytes = 0;
        self.dead_bytes = 0;
    }

    /// Moves the live records into fresh segments and drops the tombstones.
    /// The image the file gets is written from the store afterwards; one
    /// built here too was a second copy of the live data, thrown away.
    pub fn compact(&mut self) -> Result<()> {
        *self = self.compacted()?;
        Ok(())
    }

    /// The live records in a fresh store, the dead ones left behind, each
    /// value at a dropped place a `null` ([`Self::reclaim`]) -- and `self`
    /// untouched, so the database goes on using it while a maintenance
    /// builds beside it.
    pub fn compacted(&self) -> Result<Store> {
        let mut fresh = Store::new();
        fresh.next_id = self.next_id;
        fresh.dropped.clone_from(&self.dropped);
        fresh.cleared.clone_from(&self.dropped);
        let reclaims = self.reclaims();
        let mut out = Vec::new();
        let ids = self.index.ids();
        fresh.reserve(ids.len());
        for id in ids {
            let loc = self.index.get(id).unwrap();
            let payload = self.payload(loc)?;
            // The walk that copies nothing first, natively: the browser
            // module compacts when its page asks, and is smaller without it.
            let changed = reclaims
                && (cfg!(target_arch = "wasm32")
                    || crate::schema::reclaims(&self.dropped, payload))
                && crate::schema::reclaim(&self.dropped, payload, &mut out);
            match changed {
                true => fresh.append(OP_PUT, id, &out),
                false => fresh.append(OP_PUT, id, payload),
            };
        }
        Ok(fresh)
    }

    /// Bytes [`Self::write_image`] writes: the record bytes this store
    /// holds, mapped and in memory together.
    pub fn image_len(&self) -> usize {
        self.total_bytes
    }

    /// Bytes [`Self::write_live`] writes. Counted rather than taken from
    /// [`Self::live_bytes`], which is the same for every frame a writer here
    /// makes: a file's frame may hold a varint longer than the shortest,
    /// which `dead_bytes` counts as the shortest, and a length that is off
    /// makes the record after the data one unreadable.
    pub fn live_len(&self) -> usize {
        let mut head = Vec::with_capacity(16);
        let mut total = 0usize;
        for (id, loc) in self.index.walk(0) {
            head.clear();
            head.push(OP_PUT);
            put_uvarint(&mut head, id);
            put_uvarint(&mut head, loc.len as u64);
            total += head.len() + loc.len as usize;
        }
        total
    }

    /// The live records, framed afresh, in id order: what `compact` writes
    /// into the new file ([`Self::compact_live`]), walked by id, nothing
    /// gathered in memory on the way.
    pub fn write_live(&self, out: &mut dyn crate::engine::ImageOut) -> Result<()> {
        let mut head = Vec::with_capacity(16);
        for (id, loc) in self.index.walk(0) {
            let payload = self.payload(loc)?;
            head.clear();
            head.push(OP_PUT);
            put_uvarint(&mut head, id);
            put_uvarint(&mut head, payload.len() as u64);
            out.write(&head)?;
            out.write(payload)?;
        }
        Ok(())
    }

    /// [`Self::write_live`]'s bytes into a compact's file: where `out` asks
    /// for them in turn -- the old file past the page cache ([`in_turn`]) --
    /// read from the old file's start to its end, else walked by id. A
    /// method apart from `write_live`, which an image a pin sends calls
    /// alone: the check inlined beside its loop there cost the loop a
    /// register, a pinned image of 10 000 rows 19 000 to 25 000 instructions
    /// more (macOS's counts).
    pub fn compact_live(&self, out: &mut dyn crate::engine::ImageOut) -> Result<()> {
        #[cfg(all(feature = "std-fs", any(unix, windows), target_pointer_width = "64"))]
        if self.wrote_in_turn(out)? {
            return Ok(());
        }
        self.write_live(out)
    }

    /// Whether the records are read from a mapped file rather than held in
    /// memory (`fs::open_mapped`).
    pub fn is_mapped(&self) -> bool {
        self.base.is_some()
    }

    /// The mapped file the records are read from, as a reader is handed it
    /// (`fs::tests`, whose readers' tests are a 64-bit Unix's).
    #[cfg(all(test, unix, target_pointer_width = "64"))]
    pub(crate) fn mapped_base(&self) -> Option<&Base> {
        self.base.as_ref().map(|(b, _)| b)
    }

    /// The records, written into `out` rather than gathered into a `Vec`:
    /// what a checkpoint of a large collection would otherwise hold beside
    /// the data.
    pub fn write_image(&self, out: &mut dyn crate::engine::ImageOut) -> Result<()> {
        // Held rather than copied in the browser: an image taken a chunk at
        // a time held every chunk of it beside the rows until the first was
        // stored, 30 MB of 50 000 128-dim rows.
        #[cfg(target_arch = "wasm32")]
        {
            use crate::engine::Kept;
            if let Some((base, stretches)) = &self.base {
                for &(at, len) in stretches {
                    let (at, len) = (at as usize, len as usize);
                    out.write_kept(Kept {
                        whole: base.clone(),
                        at,
                        len,
                    })?;
                }
            }
            for s in &self.segments {
                let len = s.data.len();
                out.write_kept(Kept {
                    whole: s.data.clone(),
                    at: 0,
                    len,
                })?;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some((base, stretches)) = &self.base {
                let file: &[u8] = (**base).as_ref();
                for &(at, len) in stretches {
                    out.write(&file[at as usize..(at + len) as usize])?;
                }
            }
            for s in &self.segments {
                out.write(s.data.as_slice())?;
            }
        }
        Ok(())
    }

    /// Byte image of the whole store (to persist or to move it).
    pub fn image(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_bytes);
        // The records left in the mapped file came first; the segments hold
        // what was written after the file was opened.
        if let Some((base, stretches)) = &self.base {
            let file: &[u8] = (**base).as_ref();
            for &(at, len) in stretches {
                out.extend_from_slice(&file[at as usize..(at + len) as usize]);
            }
        }
        for s in &self.segments {
            out.extend_from_slice(s.data.as_slice());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Field;
    use crate::value::DataType;

    /// limits.html's "document payload": a write takes a payload of
    /// `u32::MAX` bytes and refuses one more, a query error naming the
    /// bound; a frame read off a file or a primary's record past it is
    /// damage. A location is three `u32`s, the length one of them. The
    /// engine's refusals, before a write changes anything, are driven at a
    /// bound a test's thread holds payloads to
    /// (`engine::unwinding::torn::a_document_past_a_frames_length_is_refused_and_its_block_put_back`).
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn a_payload_of_u32_max_bytes_is_taken_and_one_more_refused() {
        assert_eq!(std::mem::size_of::<Loc>(), 12);
        let most = Loc::EMPTY.len.wrapping_sub(1) as usize;
        assert_eq!(
            (most, MAX_PAYLOAD, payload_most()),
            (u32::MAX as usize, most, most)
        );
        assert_eq!(payload_fits(MAX_PAYLOAD), Ok(()));
        assert_eq!(frame_fits(MAX_PAYLOAD), Ok(()));
        assert_eq!(
            payload_fits(MAX_PAYLOAD + 1),
            Err(Error::Query(
                "a document is at most 4294967295 bytes encoded, a u32's length, not 4294967296"
                    .into()
            ))
        );
        assert_eq!(
            frame_fits(MAX_PAYLOAD + 1),
            Err(Error::Corrupt(
                "a frame puts a payload of 4294967296 bytes, past the 4294967295 a write takes"
                    .into()
            ))
        );
    }

    fn schema() -> Schema {
        Schema::new(
            "t",
            vec![
                Field::new("a", DataType::Text),
                Field::new("b", DataType::Int),
            ],
        )
        .unwrap()
    }

    /// A field added after a document was written reads `null` where its
    /// payload ends, by every way a field is read; a dropped place is
    /// passed over by all of them, and a payload cut inside a value is
    /// still refused.
    #[test]
    fn a_payload_ends_early_and_a_dropped_place_is_passed_over() {
        let mut st = Store::new();
        let mut doc = Document::default();
        doc.set("a", Value::Text("x".into()));
        doc.set("b", Value::Int(7));
        st.append(OP_PUT, 1, &Store::encode_doc(&schema(), &doc));
        // `b` dropped, then `c` added: `a` at 0, a dropped place at 1, `c`
        // at 2, where the payload has ended.
        let mut later = schema();
        later.fields.remove(1);
        later.dropped = vec![1];
        later.fields.push(Field::new("c", DataType::Int));
        st.set_dropped(&later.dropped);
        let read = st.read(&later, 1).unwrap().unwrap();
        assert_eq!(read.get("a"), Some(&Value::Text("x".into())));
        assert_eq!(read.get("c"), Some(&Value::Null));
        assert_eq!(st.read_field(1, 1).unwrap(), Some(Value::Null));
        assert_eq!(st.read_field(1, 0).unwrap(), Some(Value::Text("x".into())));
        let mut out = Vec::new();
        let places: Vec<usize> = (0..2).map(|p| later.place(p)).collect();
        assert_eq!(places, [0, 2]);
        assert!(st.read_fields(1, &places, &mut out).unwrap());
        assert_eq!(out, [Value::Text("x".into()), Value::Null]);
        // Written now, the dropped place is a null and `c` is where it reads.
        let mut doc = Document::default();
        doc.set("a", Value::Text("y".into()));
        doc.set("c", Value::Int(3));
        let payload = Store::encode_doc(&later, &doc);
        assert_eq!(payload[3], crate::codec::TAG_NULL);
        st.append(OP_PUT, 2, &payload);
        assert_eq!(st.read_field(2, 1).unwrap(), Some(Value::Int(3)));
        assert_eq!(
            later.read_doc(2, &payload).unwrap().get("c"),
            Some(&Value::Int(3))
        );
        // Read passing a dropped place's `null` by its byte, or its value
        // where one was written before the drop: the same values.
        for id in [1, 2] {
            let (mut plain, mut passing) = (Vec::new(), Vec::new());
            assert!(st.read_fields(id, &places, &mut plain).unwrap());
            assert!(st.read_fields_passing(id, &places, &mut passing).unwrap());
            assert_eq!(plain, passing, "{id}");
        }
        assert!(!crate::schema::reclaims(&later.dropped, &payload));
        assert!(crate::schema::reclaims(
            &later.dropped,
            st.raw(1).unwrap().unwrap()
        ));
        // Reclaimed, the value at the place is gone and the place stays:
        // the payload ends where a write's would, every value read as it
        // was. The one written now holds a `null` there already, as
        // nothing to reclaim.
        let mut reclaimed = Vec::new();
        let before = st.raw(1).unwrap().unwrap();
        assert!(crate::schema::reclaim(
            &later.dropped,
            before,
            &mut reclaimed
        ));
        // `a` alone: the dropped place ended the payload.
        assert_eq!(reclaimed, before[..3]);
        assert_eq!(
            later.read_doc(1, &reclaimed).unwrap(),
            later.read_doc(1, before).unwrap()
        );
        let mut nothing = Vec::new();
        assert!(!crate::schema::reclaim(
            &later.dropped,
            &payload,
            &mut nothing
        ));
        assert!(nothing.is_empty());
        let compacted = st.compacted().unwrap();
        assert_eq!(compacted.raw(1).unwrap().unwrap(), reclaimed);
        assert_eq!(compacted.raw(2).unwrap().unwrap(), payload);
        assert!(!compacted.reclaims());
        // A payload cut inside a value is no field added after it.
        let cut = &payload[..payload.len() - 1];
        assert!(later.read_doc(2, cut).is_err());
    }

    /// A stored vector's count is held to the bytes it lies in without
    /// overflowing: `n * 4` of 2^62 overflowed where an open restoring a
    /// graph read the documents' vectors (the fuzzer's `image` target, its
    /// input the target's seed `vector-count-past-usize`), and wrapped in a
    /// release build to a few bytes, the count then reserved whole. Each
    /// is refused as a vector outside the segment; one that fits is read.
    #[test]
    fn a_stored_vectors_count_past_its_bytes_is_refused() {
        use crate::codec::{put_uvarint, TAG_VECTOR, TAG_VECTOR_F16};
        let mut st = Store::new();
        let mut id = 0;
        for tag in [TAG_VECTOR, TAG_VECTOR_F16] {
            for n in [1u64 << 62, 1 << 63, u64::MAX, 5] {
                let mut payload = vec![tag];
                put_uvarint(&mut payload, n);
                payload.extend_from_slice(&[0; 8]);
                id += 1;
                st.append(OP_PUT, id, &payload);
                let read = st.read_vector_into(id, 0, &mut Vec::new());
                assert!(
                    matches!(&read, Err(Error::Corrupt(m)) if m == "vector outside the segment"),
                    "{tag} {n}: {read:?}"
                );
            }
            let mut payload = vec![tag];
            put_uvarint(&mut payload, 2);
            payload.extend_from_slice(&[0; 8]);
            id += 1;
            st.append(OP_PUT, id, &payload);
            let mut out = Vec::new();
            assert!(st.read_vector_into(id, 0, &mut out).unwrap());
            assert_eq!(out, [0.0, 0.0]);
        }
    }

    #[test]
    fn a_schema_writes_its_dropped_places_and_reads_them_back() {
        let mut s = schema();
        s.fields.remove(0);
        s.dropped = vec![0];
        s.fields.push(Field::new("c", DataType::Text));
        let bytes = s.encode();
        assert_eq!(Schema::decode(&bytes, &mut 0).unwrap(), s);
        assert_eq!((s.place(0), s.place(1), s.width()), (1, 2, 3));
    }

    #[test]
    fn put_read_delete_replay() {
        let sc = schema();
        let mut st = Store::new();
        let id = st.allocate_id();
        let doc = Document {
            id,
            fields: vec![
                ("a".into(), Value::Text("x".into())),
                ("b".into(), Value::Int(7)),
            ],
        };
        st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc));
        assert_eq!(
            st.read(&sc, id).unwrap().unwrap().get("b"),
            Some(&Value::Int(7))
        );
        // field-level read
        assert_eq!(st.read_field(id, 1).unwrap(), Some(Value::Int(7)));

        let image = st.image();
        let mut st2 = Store::new();
        st2.replay(&image).unwrap();
        assert_eq!(st2.len(), 1);

        st.append(OP_DEL, id, &[]);
        assert_eq!(st.len(), 0);
    }

    /// limits.html's "gap in an externally given id": an id at most 4 096
    /// past the dense array's end grows it, one further goes into the
    /// sparse map -- a path, not a refusal: either way the id is found.
    #[test]
    fn a_gap_of_4096_grows_the_dense_array_and_one_more_does_not() {
        let at = Loc {
            seg: 0,
            off: 0,
            len: 1,
        };
        let mut ix = IdIndex::default();
        ix.insert(1, at);
        ix.insert(1 + MAX_DENSE_GAP, at);
        assert_eq!(
            (ix.dense.len() as u64, ix.sparse.len()),
            (1 + MAX_DENSE_GAP, 0)
        );
        let far = 2 + 2 * MAX_DENSE_GAP;
        ix.insert(far, at);
        assert_eq!(
            (ix.dense.len() as u64, ix.sparse.len()),
            (1 + MAX_DENSE_GAP, 1)
        );
        for id in [1, 1 + MAX_DENSE_GAP, far] {
            assert_eq!(ix.get(id), Some(at), "{id}");
        }
    }

    #[test]
    fn id_index_handles_sparse_ids() {
        let sc = schema();
        let mut st = Store::new();
        let mk = |i: i64| Document {
            id: 0,
            fields: vec![
                ("a".into(), Value::Text(format!("v{i}"))),
                ("b".into(), Value::Int(i)),
            ],
        };
        // dense range
        for i in 1..=100u64 {
            st.append(OP_PUT, i, &Store::encode_doc(&sc, &mk(i as i64)));
        }
        // small gap -> still dense
        st.append(OP_PUT, 200, &Store::encode_doc(&sc, &mk(200)));
        // large gap -> falls into the sparse map, memory must not blow up
        st.append(OP_PUT, 9_000_000_000, &Store::encode_doc(&sc, &mk(-1)));

        assert_eq!(st.len(), 102);
        assert!(st.contains(1) && st.contains(100) && st.contains(200));
        assert!(st.contains(9_000_000_000));
        assert!(!st.contains(150));
        assert_eq!(st.read_field(200, 1).unwrap(), Some(Value::Int(200)));
        assert_eq!(
            st.read_field(9_000_000_000, 1).unwrap(),
            Some(Value::Int(-1))
        );
        assert_eq!(st.read_field(150, 1).unwrap(), None);

        // ids must come back in ascending order (dense + sparse merged)
        let ids = st.ids();
        assert_eq!(ids.len(), 102);
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "not sorted");
        assert_eq!(*ids.last().unwrap(), 9_000_000_000);

        // deletion has to work on both paths
        st.append(OP_DEL, 50, &[]);
        st.append(OP_DEL, 9_000_000_000, &[]);
        assert_eq!(st.len(), 100);
        assert!(!st.contains(50) && !st.contains(9_000_000_000));

        // a replay must give the same result
        let mut st2 = Store::new();
        st2.replay(&st.image()).unwrap();
        assert_eq!(st2.len(), 100);
        assert_eq!(st2.ids(), st.ids());
    }

    /// An image's index of a data record gives the store walking the record
    /// would: the same ids at the same places, the same counts. And an index
    /// that does not describe the record leaves the store as it was, for
    /// the walk.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_image_index_gives_the_store_its_walk_gives() {
        let sc = schema();
        let doc = |i: i64| Document {
            id: 0,
            fields: vec![
                ("a".into(), Value::Text("v".repeat(i as usize % 300))),
                ("b".into(), Value::Int(i)),
            ],
        };
        let mut st = Store::new();
        for id in 1..=400u64 {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(id as i64)));
        }
        // Rewritten, deleted, and ids past the dense array's gap.
        for id in (1..=400u64).step_by(7) {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(-(id as i64))));
        }
        for id in (3..=400u64).step_by(11) {
            st.append(OP_DEL, id, &[]);
        }
        st.append(OP_PUT, 1 << 40, &Store::encode_doc(&sc, &doc(5)));
        for live in [false, true] {
            let mut image = Vec::new();
            match live {
                true => st.write_live(&mut image).unwrap(),
                false => st.write_image(&mut image).unwrap(),
            }
            let mut index = Vec::new();
            st.image_index(live, &mut index);
            // Four bytes before the record, as a file has its head.
            let at = 4u64;
            let mut file = vec![0u8; at as usize];
            file.extend_from_slice(&image);
            let base: Base = std::sync::Arc::new(file);
            let len = image.len() as u64;
            let mut walked = Store::new();
            walked.replay_mapped(&base, at, len, &mut |_| {}).unwrap();
            let mut taken = Store::new();
            assert!(taken.adopt_index(&base, at, len, &index), "live {live}");
            held_shape(&taken.index, &format!("live {live}"));
            assert_eq!(taken.ids(), walked.ids(), "live {live}");
            for id in walked.ids() {
                assert_eq!(
                    taken.read_field(id, 1).unwrap(),
                    walked.read_field(id, 1).unwrap()
                );
            }
            let counts = |s: &Store| (s.len(), s.total_bytes(), s.dead_bytes(), s.next_id());
            assert_eq!(counts(&taken), counts(&walked), "live {live}");
            // Another length, cut short, a byte more, an id twice, a payload
            // past the end: refused, the store left empty. A payload moved
            // within the record is not seen, as a frame's head that says
            // another length is not by the walk.
            let hand = |entries: &[(u64, u64, u64)]| {
                let mut ix = vec![FRAMES_MARK];
                for v in [len, 0, entries.len() as u64] {
                    put_uvarint(&mut ix, v);
                }
                for &(step, n, off) in entries {
                    for v in [step, n, off] {
                        put_uvarint(&mut ix, v);
                    }
                }
                ix
            };
            let (twice, past) = (hand(&[(5, 1, 0), (0, 1, 0)]), hand(&[(1, len, 0)]));
            for (bad, what) in [
                (&index[..], len + 1),
                (&index[..index.len() - 1], len),
                (&[&index[..], &[0]].concat()[..], len),
                (&twice[..], len),
                (&past[..], len),
            ] {
                let mut refused = Store::new();
                assert!(!refused.adopt_index(&base, at, what, bad), "live {live}");
                assert_eq!(counts(&refused), (0, 0, 0, 1), "live {live}");
            }
        }
    }

    /// An image's index whose document is `DocId::MAX`, an id no write
    /// gives, is not taken -- taken, the store's next id was one past it,
    /// which overflowed, on the mapped open a server runs and the fuzzer's
    /// `image` target did not -- and the walk an open goes on to refuses
    /// the frame putting it, so the open is refused, mapped or not.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_image_index_reaching_the_last_id_is_not_taken() {
        let sc = schema();
        let payload = Store::encode_doc(
            &sc,
            &Document {
                id: 0,
                fields: vec![("b".into(), Value::Int(1))],
            },
        );
        // Written at the id before the last, then made the last where the
        // frame and the index each say it: ten bytes either way, its first
        // a step on.
        let mut almost = Vec::new();
        put_uvarint(&mut almost, DocId::MAX - 1);
        let mut last = Vec::new();
        put_uvarint(&mut last, DocId::MAX);
        assert_eq!(almost.len(), last.len());
        let made_last = |bytes: &mut Vec<u8>| {
            let at = bytes
                .windows(almost.len())
                .position(|w| w == &almost[..])
                .expect("the id before the last");
            bytes[at..at + last.len()].copy_from_slice(&last);
        };
        for live in [false, true] {
            let mut st = Store::new();
            st.append(OP_PUT, DocId::MAX - 1, &payload);
            let mut image = Vec::new();
            match live {
                true => st.write_live(&mut image).unwrap(),
                false => st.write_image(&mut image).unwrap(),
            }
            let mut index = Vec::new();
            st.image_index(live, &mut index);
            made_last(&mut image);
            made_last(&mut index);
            let at = 4u64;
            let mut file = vec![0u8; at as usize];
            file.extend_from_slice(&image);
            let base: Base = std::sync::Arc::new(file);
            let len = image.len() as u64;
            let mut taken = Store::new();
            assert!(!taken.adopt_index(&base, at, len, &index), "live {live}");
            assert_eq!((taken.len(), taken.next_id()), (0, 1), "live {live}");
            match taken.replay_mapped(&base, at, len, &mut |_| {}) {
                Err(Error::Corrupt(m)) => assert_eq!(
                    m,
                    "a frame puts document 18446744073709551615, an id no write gives"
                ),
                other => panic!("live {live}: {other:?}"),
            }
        }
    }

    /// An image's index read a share at a time on every core gives the
    /// store the walk gives -- ids past a gap, rewritten and deleted, each
    /// at its place -- and so does one whose ids leave the dense array a
    /// quarter empty or reach past its gap, read in turn instead, or whose
    /// shares' ends are wrong. One whose documents are wrong is refused.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_image_index_read_in_shares_gives_the_store_its_walk_gives() {
        let sc = schema();
        let doc = |i: u64| Document {
            id: 0,
            fields: vec![
                ("a".into(), Value::Text("v".repeat(i as usize % 9))),
                ("b".into(), Value::Int(i as i64)),
            ],
        };
        let n = 2 * INDEX_SHARE as u64 + 500;
        // Dense, then with a gap past the dense array's, then a quarter
        // empty, then dense from far past 1 -- the ids below it written
        // and deleted before the image: the first and the last read in
        // shares, into an array from its first id, the others in turn.
        let layouts: [&dyn Fn(u64) -> u64; 4] = [
            &|i| i + 1 + i / 1000 * 7,
            &|i| {
                if i < n / 2 {
                    i + 1
                } else {
                    i + 1 + MAX_DENSE_GAP + 1
                }
            },
            &|i| i * 2 + 1,
            &|i| i + 50_001 + i / 1000 * 7,
        ];
        for (k, id_of) in layouts.iter().enumerate() {
            let mut st = Store::new();
            for i in 0..n {
                st.append(OP_PUT, id_of(i), &Store::encode_doc(&sc, &doc(i)));
            }
            for i in (0..n).step_by(13) {
                st.append(OP_PUT, id_of(i), &Store::encode_doc(&sc, &doc(i + 1)));
            }
            for i in (5..n).step_by(17) {
                st.append(OP_DEL, id_of(i), &[]);
            }
            for live in [false, true] {
                let what = format!("layout {k}, live {live}");
                let mut image = Vec::new();
                match live {
                    true => st.write_live(&mut image).unwrap(),
                    false => st.write_image(&mut image).unwrap(),
                }
                let mut index = Vec::new();
                st.image_index(live, &mut index);
                let at = 4u64;
                let mut file = vec![0u8; at as usize];
                file.extend_from_slice(&image);
                let base: Base = std::sync::Arc::new(file);
                let len = image.len() as u64;
                let mut walked = Store::new();
                walked.replay_mapped(&base, at, len, &mut |_| {}).unwrap();
                let shares = u32::from_le_bytes(index[index.len() - 4..].try_into().unwrap());
                assert_eq!(
                    shares as u64,
                    (st.len() as u64).div_ceil(INDEX_SHARE as u64)
                );
                // Read in shares where the ids leave the dense array full.
                let table = index.len() - 4 - 24 * shares as usize;
                let (docs, ends) = (&index[..table], &index[table..index.len() - 4]);
                let mut p = 1;
                let count = [(); 3].map(|_| uvarint(docs, &mut p).unwrap())[2];
                let in_shares = adopt_shares(docs, p, ends, count, at, len).is_some();
                assert_eq!(in_shares, k == 0 || k == 3, "{what}");
                // As written; its first share's end moved one byte on; the
                // last share's first id no step on from the one before.
                let mut moved = index.clone();
                moved[table] ^= 1;
                let mut gone = index.clone();
                let start = table + 24 * (shares as usize - 2);
                let start = u64::from_le_bytes(index[start..start + 8].try_into().unwrap());
                gone[start as usize] = 0;
                for (ix, takes) in [(&index, true), (&moved, true), (&gone, false)] {
                    let mut taken = Store::new();
                    let took = taken.adopt_index(&base, at, len, ix);
                    assert_eq!(took, takes, "{what}");
                    if !took {
                        continue;
                    }
                    held_shape(&taken.index, &what);
                    assert_eq!(taken.ids(), walked.ids(), "{what}");
                    for id in walked.ids() {
                        assert_eq!(
                            taken.read_field(id, 1).unwrap(),
                            walked.read_field(id, 1).unwrap(),
                            "{what}"
                        );
                    }
                    let counts =
                        |s: &Store| (s.len(), s.total_bytes(), s.dead_bytes(), s.next_id());
                    assert_eq!(counts(&taken), counts(&walked), "{what}");
                    assert_eq!(taken.index.dense.len(), walked.index.dense.len(), "{what}");
                    assert_eq!(
                        taken.index.sparse.len(),
                        walked.index.sparse.len(),
                        "{what}"
                    );
                }
            }
        }
    }

    /// A `DocMap` keeps a number the dense array grows over, as the id
    /// index keeps a location: the text index's lengths were read from the
    /// dense slot the array had grown over, 0, and a document scored as
    /// empty.
    #[test]
    fn a_doc_map_keeps_what_the_dense_array_grows_over() {
        let mut m = DocMap::default();
        for (id, n) in [(10_000u64, 7u32), (5_000, 5), (4_000, 4), (8_000, 8)] {
            assert_eq!(m.insert(id, n), None);
        }
        assert_eq!(m.len(), 4);
        for (id, n) in [(4_000u64, 4u32), (5_000, 5), (8_000, 8), (10_000, 7)] {
            assert_eq!(m.get(id), n, "{id}");
        }
        assert_eq!(m.insert(10_000, 9), Some(7));
        assert_eq!(m.insert(11_000, 11), None);
        assert_eq!(m.len(), 5);
        assert_eq!(m.remove(5_000), Some(5));
        assert_eq!(m.remove(5_000), None);
        assert_eq!((m.get(5_000), m.get(10_000), m.len()), (0, 9, 4));
        // Past the gap stays sparse, as does the id 0 when the array grows.
        assert_eq!(m.insert(1 << 52, 3), None);
        assert_eq!(m.insert(0, 1), None);
        assert_eq!(m.insert(12_000, 12), None);
        assert_eq!((m.get(1 << 52), m.get(0), m.get(12_000)), (3, 1, 12));
        assert_eq!(m.len(), 7);
    }

    /// A sparse id the dense array later grows over has to move into it.
    /// Before it did, `get t where id = 5000` found nothing, `ids()` listed
    /// 5000 out of order, and `count` still counted it.
    #[test]
    fn sparse_ids_move_into_the_dense_array_as_it_grows() {
        let sc = schema();
        let doc = |i: i64| Document {
            id: 0,
            fields: vec![
                ("a".into(), Value::Text(format!("v{i}"))),
                ("b".into(), Value::Int(i)),
            ],
        };
        let mut st = Store::new();
        // The array starts at 1. 10 000 and 5 000 are too far past its end:
        // sparse. 4 000 is within the gap: the array grows to it, and to 5
        // 000, within the gap of 4 000. 8 000 takes 10 000 in the same way.
        // (The first id of an empty index is where its array starts, so
        // without 1 all four would be dense from 10 000 down.)
        for id in [1u64, 10_000, 5_000, 4_000, 8_000] {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(id as i64)));
        }
        assert_eq!(st.len(), 5);
        assert!(st.contains(5_000), "the covered sparse id vanished");
        assert_eq!(st.read_field(5_000, 1).unwrap(), Some(Value::Int(5_000)));
        assert_eq!(st.ids(), vec![1, 4_000, 5_000, 8_000, 10_000]);
        assert_eq!(st.iter_ids().collect::<Vec<_>>(), st.ids());

        // Overwriting an id while the array grows over it counts it once.
        st.append(OP_PUT, 10_000, &Store::encode_doc(&sc, &doc(-10)));
        st.append(OP_PUT, 11_000, &Store::encode_doc(&sc, &doc(11_000)));
        assert_eq!(st.len(), 6);
        assert_eq!(st.read_field(10_000, 1).unwrap(), Some(Value::Int(-10)));
        assert_eq!(st.ids(), vec![1, 4_000, 5_000, 8_000, 10_000, 11_000]);
        for from in [
            0,
            1,
            4_000,
            4_001,
            8_000,
            9_999,
            10_000,
            11_000,
            11_001,
            u64::MAX,
        ] {
            let want: Vec<_> = st.ids().into_iter().filter(|&i| i >= from).collect();
            assert_eq!(
                st.iter_ids_from(from).collect::<Vec<_>>(),
                want,
                "from {from}"
            );
        }

        let mut st2 = Store::new();
        st2.replay(&st.image()).unwrap();
        assert_eq!(st2.ids(), st.ids());
        assert!(st2.contains(5_000));

        // A run of sparse ids is taken over in one go when the array, from
        // 1, comes within the gap of it: 16 000 takes 20 000 to 20 499. The
        // array holds more than the run, which so does not take it.
        let mut st = Store::new();
        for id in 1u64..=600 {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(id as i64)));
        }
        for id in 20_000u64..20_500 {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(id as i64)));
        }
        assert_eq!((st.index.floor, st.index.sparse.len()), (1, 500));
        for id in (4_000u64..=16_000).step_by(4_000) {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(id as i64)));
        }
        assert_eq!((st.len(), st.index.sparse.len()), (1_104, 0));
        for id in [4_000u64, 16_000, 20_000, 20_001, 20_499] {
            assert!(st.contains(id), "{id} vanished");
        }
        let ids = st.ids();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "not sorted");
        assert_eq!(ids.len(), 1_104);
    }

    /// A store takes its frames in from a file that holds them only where
    /// the runs account for every one, in order, and reads the same after:
    /// a run short of a frame, or one that ends inside one, moves nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_handover_moves_nothing_the_runs_do_not_account_for() {
        let sc = schema();
        let doc = |i: i64, pad: usize| Document {
            id: 0,
            fields: vec![
                ("a".into(), Value::Text(format!("v{i}{}", "x".repeat(pad)))),
                ("b".into(), Value::Int(i)),
            ],
        };
        let mut st = Store::new();
        // Past a segment, so a run goes on in the next one.
        let mut frames = Vec::new();
        for i in 1..=2200u64 {
            frames.extend(st.append(OP_PUT, i, &Store::encode_doc(&sc, &doc(i as i64, 4000))));
        }
        frames.extend(st.append(OP_PUT, 7, &Store::encode_doc(&sc, &doc(-7, 3))));
        frames.extend(st.append(OP_DEL, 8, &[]));
        assert!(st.segment_count() > 1);
        let read = |st: &Store| -> Vec<Option<Value>> {
            (1..=2200).map(|id| st.read_field(id, 0).unwrap()).collect()
        };
        let (before, image) = (read(&st), st.image());

        // The file: something else first, then the frames, as records
        // appended after an open hold them.
        let mut file = vec![0u8; 100];
        file.extend_from_slice(&frames);
        let base: Base = std::sync::Arc::new(file);
        let whole = frames.len() as u64;
        let last = Store::frame(OP_DEL, 8, &[]).len() as u64;
        for runs in [
            vec![(whole - last, 100)],
            vec![(whole - 1, 100), (1, 100 + whole - 1)],
            vec![(10, 100), (whole - 10, 110)],
        ] {
            assert!(!st.hand_over(&base, &runs), "{runs:?}");
            assert_eq!(read(&st), before);
            assert!(st.heap_bytes() as u64 >= whole);
        }
        // Two runs, split where a record would end.
        let split = Store::frame(OP_PUT, 1, &Store::encode_doc(&sc, &doc(1, 4000))).len() as u64;
        assert!(st.hand_over(&base, &[(split, 100), (whole - split, 100 + split)]));
        assert_eq!(st.heap_bytes(), 0);
        assert_eq!(read(&st), before);
        assert_eq!(
            st.read_field(7, 0).unwrap(),
            Some(Value::Text("v-7xxx".into()))
        );
        assert!(!st.contains(8));
        // Written back as it stood.
        assert_eq!(st.image(), image);
    }

    #[test]
    fn compaction_drops_tombstones() {
        let sc = schema();
        let mut st = Store::new();
        for i in 0..100 {
            let id = st.allocate_id();
            let doc = Document {
                id,
                fields: vec![
                    ("a".into(), Value::Text(format!("v{i}"))),
                    ("b".into(), Value::Int(i)),
                ],
            };
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc));
        }
        for id in 1..=50u64 {
            st.append(OP_DEL, id, &[]);
        }
        assert!(st.dead_bytes() > 0);
        st.compact().unwrap();
        assert_eq!(st.len(), 50);
        assert_eq!(st.dead_bytes(), 0);
        let mut st2 = Store::new();
        st2.replay(&st.image()).unwrap();
        assert_eq!(st2.len(), 50);
    }

    /// [`varint_len`] is the bytes [`put_uvarint`] writes, at each edge of
    /// every width: a frame's head counted short or long puts the live
    /// bytes off, and an image index's places with them ([`after`]).
    #[test]
    fn a_varints_length_is_the_bytes_it_is_written_in() {
        let mut out = Vec::new();
        for k in 0..64u32 {
            for v in [(1u64 << k) - 1, 1 << k, (1 << k) + 1, u64::MAX >> (63 - k)] {
                out.clear();
                put_uvarint(&mut out, v);
                assert_eq!(varint_len(v), out.len() as u64, "{v}");
            }
        }
        assert_eq!(varint_len(u64::MAX), 10);
    }

    /// A version written over or deleted leaves its whole frame dead, head
    /// and payload, so the live bytes -- `fennec_data_bytes`, read with no
    /// walk -- are the live frames to the byte, what a compact writes of
    /// them: as writes land and a block is put back, and as a load walks
    /// the frames in memory or in the file or takes an image's index. Ids
    /// and lengths cross every width their varints take. Counted by the
    /// payload alone, a version written over left its head live.
    #[test]
    fn the_live_bytes_are_the_live_frames_to_the_byte() {
        let ids = [1u64, 126, 16_382, 2_097_150, 1 << 40];
        let lens = [0usize, 1, 127, 128, 300, 16_384];
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let write = |st: &mut Store, r: u64, i: usize| {
            let id = ids[(r % 5) as usize] + (r >> 8) % 4;
            match (r >> 16) % 4 {
                0 => st.append(OP_DEL, id, &[]),
                _ => st.append(OP_PUT, id, &vec![i as u8; lens[(r >> 24) as usize % 6]]),
            };
        };
        let held = |st: &Store, what: &str| {
            assert_eq!(st.live_bytes(), st.live_len(), "{what}");
            assert_eq!(
                st.compacted().unwrap().total_bytes(),
                st.live_bytes(),
                "{what}"
            );
        };
        let mut st = Store::new();
        for i in 0..400 {
            write(&mut st, next(), i);
            assert_eq!(st.live_bytes(), st.live_len(), "write {i}");
        }
        assert!(st.dead_bytes() > 0 && !st.is_empty());
        held(&st, "written");

        // A block put back: each id pointed where it was, then the segments
        // and the counts cut back to the mark.
        let (mark, live) = (st.mark(), st.live_bytes());
        let mut was = Vec::new();
        for i in 0..50 {
            let r = next();
            let id = ids[(r % 5) as usize] + (r >> 8) % 4;
            was.push((id, st.loc(id)));
            match r % 2 {
                0 => st.append(OP_DEL, id, &[]),
                _ => st.append(OP_PUT, id, &[i as u8; 200]),
            };
        }
        for &(id, loc) in was.iter().rev() {
            st.point(id, loc);
        }
        st.rewind(mark);
        assert_eq!(st.live_bytes(), live);
        held(&st, "put back");
        for i in 400..600 {
            write(&mut st, next(), i);
        }
        held(&st, "written again");

        // Loaded again: walked in memory, walked in the file, taken from an
        // image's index -- each counts what the store counted.
        let counts = |s: &Store| (s.len(), s.total_bytes(), s.dead_bytes(), s.live_bytes());
        let mut walked = Store::new();
        walked.replay(&st.image()).unwrap();
        assert_eq!(counts(&walked), counts(&st));
        held(&walked, "replayed");
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut image = Vec::new();
            st.write_image(&mut image).unwrap();
            let mut index = Vec::new();
            st.image_index(false, &mut index);
            let len = image.len() as u64;
            let base: Base = std::sync::Arc::new([vec![0u8; 4], image].concat());
            let mut mapped = Store::new();
            mapped.replay_mapped(&base, 4, len, &mut |_| {}).unwrap();
            assert_eq!(counts(&mapped), counts(&st));
            held(&mapped, "walked in the file");
            let mut taken = Store::new();
            assert!(taken.adopt_index(&base, 4, len, &index));
            assert_eq!(counts(&taken), counts(&st));
            held(&taken, "taken from the index");
        }
    }

    /// A store's documents and counts, and its frames as they stand: what a
    /// refused frame must leave as it was.
    type Standing = (Vec<(DocId, Option<Loc>)>, [usize; 4], DocId, Vec<u8>);

    fn standing(s: &Store) -> Standing {
        let ids = s.ids().into_iter().map(|id| (id, s.loc(id))).collect();
        let counts = [s.len(), s.total_bytes(), s.dead_bytes(), s.live_bytes()];
        (ids, counts, s.next_id(), s.image())
    }

    /// Documents 1 to 20 put, 7 written over and 3 deleted: a store whose
    /// counts a frame refused after them would move.
    fn twenty() -> Store {
        let sc = schema();
        let doc = |i: i64| Document {
            id: 0,
            fields: vec![
                ("a".into(), Value::Text(format!("v{i}"))),
                ("b".into(), Value::Int(i)),
            ],
        };
        let mut st = Store::new();
        for id in 1..=20u64 {
            st.append(OP_PUT, id, &Store::encode_doc(&sc, &doc(id as i64)));
        }
        st.append(OP_PUT, 7, &Store::encode_doc(&sc, &doc(-7)));
        st.append(OP_DEL, 3, &[]);
        st
    }

    /// A frame whose op is neither a put nor a del -- a later version's,
    /// or damage below the durable point, which a load reads unchecked --
    /// is refused by its byte and its place, and changes nothing: no count,
    /// no document, no segment. Replayed as a frame of no effect, it
    /// counted the version of a document it named dead while the index
    /// still pointed at it, counted itself live, and a compact dropped it.
    /// Over a document the store holds and one it does not, and after a
    /// frame of its own record, which is taken as the open is then refused.
    #[test]
    fn an_unknown_op_is_refused_by_its_byte() {
        let image = twenty().image();
        for op in [0u8, 3, 0x7f, 0xff] {
            for id in [5u64, 400] {
                for after_one in [false, true] {
                    let what = format!("op {op} on {id}, after one {after_one}");
                    let mut st = Store::new();
                    st.replay(&image).unwrap();
                    let before = standing(&st);
                    let mut record = Vec::new();
                    if after_one {
                        record = Store::frame(OP_PUT, 8, &[0x01]);
                    }
                    let place = record.len() as u64;
                    record.extend(Store::frame(op, id, &[0x01, 0x02]));
                    let mut noted = Vec::new();
                    let r = st.replay_noting(&record, 1_000, &mut |id| noted.push(id));
                    let said = format!("unknown write op {op} at {}", 1_000 + place);
                    assert!(
                        matches!(&r, Err(Error::Corrupt(m)) if *m == said),
                        "{what}: {r:?}"
                    );
                    if after_one {
                        assert_eq!(noted, [8], "{what}");
                        continue;
                    }
                    assert!(noted.is_empty(), "{what}");
                    assert_eq!(standing(&st), before, "{what}");
                }
            }
        }
    }

    /// [`an_unknown_op_is_refused_by_its_byte`] for a record replayed where
    /// it lies in the file, whose place the refusal names: the store as it
    /// was, no stretch of the file taken.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_unknown_op_in_the_file_is_refused_by_its_byte() {
        let image = twenty().image();
        for op in [0u8, 3, 0x7f, 0xff] {
            for id in [5u64, 400] {
                let what = format!("op {op} on {id}");
                let bad = Store::frame(op, id, &[0x01, 0x02]);
                let file = [vec![0u8; 4], image.clone(), bad.clone()].concat();
                let base: Base = std::sync::Arc::new(file);
                let mut st = Store::new();
                st.replay_mapped(&base, 4, image.len() as u64, &mut |_| {})
                    .unwrap();
                let before = standing(&st);
                let stretches = st.base.as_ref().map(|b| b.1.clone());
                let at = 4 + image.len() as u64;
                let mut noted = Vec::new();
                let r = st.replay_mapped(&base, at, bad.len() as u64, &mut |id| noted.push(id));
                let said = format!("unknown write op {op} at {at}");
                assert!(
                    matches!(&r, Err(Error::Corrupt(m)) if *m == said),
                    "{what}: {r:?}"
                );
                assert!(noted.is_empty(), "{what}");
                assert_eq!(standing(&st), before, "{what}");
                assert_eq!(st.base.as_ref().map(|b| b.1.clone()), stretches, "{what}");
            }
        }
    }

    /// An externally supplied large id has to land in the sparse map -- and
    /// **on a 32-bit target too**. Comparing via `id as usize` turned 2^52
    /// into zero on WASM32 and sent it to `dense[0 - 1]`; the browser
    /// client's optimistic rows sit in exactly that range.
    #[test]
    fn ids_beyond_u32_land_in_the_sparse_map() {
        let sc = Schema::new(
            "t",
            vec![crate::schema::Field::new("a", crate::value::DataType::Int)],
        )
        .unwrap();
        let mut st = Store::new();
        let doc = |id: DocId, a: i64| Document {
            id,
            fields: vec![("a".into(), Value::Int(a))],
        };
        st.append(OP_PUT, 1, &Store::encode_doc(&sc, &doc(1, 1)));

        let big: DocId = 1 << 52;
        st.append(OP_PUT, big, &Store::encode_doc(&sc, &doc(big, 2)));
        assert!(st.contains(big));
        assert_eq!(st.len(), 2);
        assert_eq!(
            st.read(&sc, big).unwrap().unwrap().get("a"),
            Some(&Value::Int(2))
        );
        // The dense part must not have grown: an array of 2^52 entries could
        // not be allocated in the first place.
        assert!(st.index_bytes() < 4096);

        st.append(OP_DEL, big, &[]);
        assert!(!st.contains(big));
        assert!(st.contains(1), "the small id must be unaffected");

        // The exact 2^32 boundary: truncation lands on 0 here as well.
        let edge: DocId = 1 << 32;
        st.append(OP_PUT, edge, &Store::encode_doc(&sc, &doc(edge, 3)));
        assert!(st.contains(edge));
        assert_eq!(st.ids(), vec![1, edge]);
    }

    /// What an id index says of itself whatever was written to it: `lead`
    /// its first held slot and `last` its last, nothing held between `lead`
    /// and `after` nor between `before` and `last`, an array holding
    /// nothing let go, the empty runs before the first held slot and past
    /// the last no more than a cut leaves below its trigger, no sparse id
    /// in the held range nor within the gap of it, either side, nor one
    /// above it within the gap of `sparse_floor` or below it within the gap
    /// of `sparse_ceil`, and the look's count no more than the map holds.
    fn held_shape(ix: &IdIndex, what: &str) {
        let len = ix.dense.len();
        assert!(ix.floor >= 1, "{what}: floor {}", ix.floor);
        let first = ix.dense.iter().position(|l| !l.is_empty()).unwrap_or(len);
        assert_eq!(ix.lead, first, "{what}: lead");
        assert!(first < len || len == 0, "{what}: an empty array is let go");
        assert!(ix.lead <= ix.after && ix.after <= len, "{what}: after");
        for (lo, hi) in ix.holes {
            assert!(
                lo <= hi && hi <= len && ix.dense[lo..hi].iter().all(Loc::is_empty),
                "{what}: the hole {lo}..{hi} of {len}"
            );
        }
        if ix.after > ix.lead + 1 {
            let between = &ix.dense[ix.lead + 1..ix.after];
            assert!(between.iter().all(Loc::is_empty), "{what}: after");
        }
        match ix.dense.iter().rposition(|l| !l.is_empty()) {
            None => assert_eq!((ix.last, ix.before), (0, 0), "{what}: last"),
            Some(last) => {
                assert_eq!(ix.last, last, "{what}: last");
                assert!(ix.before <= last, "{what}: before");
                if last > ix.before + 1 {
                    let between = &ix.dense[ix.before + 1..last];
                    assert!(between.iter().all(Loc::is_empty), "{what}: before");
                }
                let span = last + 1 - first;
                let most = (span + (span / 4).min(CUT_FROM)).max(CUT_FROM + span / 4);
                assert!(
                    ix.lead <= most && len - 1 - last <= CUT_FROM.max(span),
                    "{what}: {} slots of room before {span} and {} past them",
                    ix.lead,
                    len - 1 - last
                );
            }
        }
        assert!(
            ix.rebase_in <= ix.sparse.len(),
            "{what}: a look {} writes away, {} sparse",
            ix.rebase_in,
            ix.sparse.len()
        );
        let (low, end) = (ix.floor + ix.lead as u64, ix.floor + len as u64);
        for &k in ix.sparse.keys() {
            match k < low {
                true => assert!(
                    low - k > MAX_DENSE_GAP && k + MAX_DENSE_GAP <= ix.sparse_ceil,
                    "{what}: {k} within the gap below {low}, or of the sparse ceiling {}",
                    ix.sparse_ceil
                ),
                false => assert!(
                    k >= end && k - end >= MAX_DENSE_GAP && k - MAX_DENSE_GAP >= ix.sparse_floor,
                    "{what}: {k} in the held range from {low} to {end}, within the gap past \
                     it, or of the sparse floor {}",
                    ix.sparse_floor
                ),
            }
        }
    }

    /// [`held_shape`], and the ids and the count `model`'s.
    fn held_as(ix: &IdIndex, model: &std::collections::BTreeMap<DocId, Loc>, what: &str) {
        held_shape(ix, what);
        assert_eq!(ix.len(), model.len(), "{what}: count");
        let ids: Vec<DocId> = model.keys().copied().collect();
        assert_eq!(ix.ids(), ids, "{what}: ids");
        for (&id, &l) in model {
            assert_eq!(ix.get(id), Some(l), "{what}: {id}");
        }
    }

    /// The id index against a map of what it was given, through every way
    /// a write reaches it: ids climbing while the lowest or the highest go,
    /// ids written below the first held one -- near it, into the room or a
    /// copy below the floor, and far, which go to the sparse map -- ids far
    /// past the end, deletes anywhere, ids deleted and written again as a
    /// block put back does, and a compact's or a checkpoint's relocation,
    /// which visits them in order; and a clone taken before the floor
    /// moves, as a pin takes it, reads as it was.
    #[test]
    fn the_id_index_answers_as_a_map_while_its_floor_moves() {
        use std::collections::BTreeMap;
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut draw = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let at = |id: DocId, n: u32| Loc {
            seg: n,
            off: id as u32,
            len: (id >> 32) as u32,
        };
        let mut ix = IdIndex::default();
        let mut model: BTreeMap<DocId, Loc> = BTreeMap::new();
        let (mut next, mut gone) = (1u64, Vec::new());
        let (mut ups, mut downs, mut below) = (0, 0, 0);
        for step in 0..150_000u32 {
            let lowest = model.keys().next().copied();
            let floor = ix.floor;
            let put = |ix: &mut IdIndex, model: &mut BTreeMap<DocId, Loc>, id: DocId| {
                ix.insert(id, at(id, step));
                model.insert(id, at(id, step));
            };
            match draw(100) {
                0..=39 => {
                    put(&mut ix, &mut model, next);
                    next += 1 + draw(3) / 2;
                }
                40..=55 => {
                    if let Some(id) = lowest {
                        assert_eq!(ix.remove(id), model.remove(&id), "{step}");
                        gone.push(id);
                    }
                }
                56..=63 => {
                    if let Some(id) = model.keys().next_back().copied() {
                        assert_eq!(ix.remove(id), model.remove(&id), "{step}");
                        gone.push(id);
                    }
                }
                64..=71 => {
                    let k = draw(next + 1);
                    if let Some(id) = model.range(k..).next().map(|(&id, _)| id) {
                        assert_eq!(ix.remove(id), model.remove(&id), "{step}");
                        gone.push(id);
                    }
                }
                72..=78 => {
                    if let Some(lo) = lowest {
                        let id = lo.saturating_sub(1 + draw(2 * MAX_DENSE_GAP)).max(1);
                        put(&mut ix, &mut model, id);
                    }
                }
                79..=80 => put(&mut ix, &mut model, 1 + draw(1 << 40)),
                81..=88 => {
                    for _ in 0..1 + draw(40) {
                        if let Some(id) = gone.pop() {
                            put(&mut ix, &mut model, id);
                        }
                    }
                }
                89..=94 => {
                    for _ in 0..8 {
                        let id = draw(next + 10_000);
                        assert_eq!(ix.get(id), model.get(&id).copied(), "{step}: {id}");
                    }
                }
                95..=96 => {
                    let from = draw(next + 100);
                    let want: Vec<DocId> = model.range(from..).map(|(&id, _)| id).collect();
                    assert_eq!(ix.iter_from(from).collect::<Vec<_>>(), want, "{step}");
                }
                97 => {
                    let mut seen = Vec::new();
                    ix.relocate(|id, l| {
                        seen.push(id);
                        at(id, l.seg + 1)
                    });
                    for l in model.values_mut() {
                        l.seg += 1;
                    }
                    let ids: Vec<DocId> = model.keys().copied().collect();
                    assert_eq!(seen, ids, "{step}");
                }
                _ => {
                    let (pin, was) = (ix.clone(), model.clone());
                    while let Some(&id) = model.keys().next() {
                        if draw(2_000) == 0 {
                            break;
                        }
                        assert_eq!(ix.remove(id), model.remove(&id), "{step}");
                        gone.push(id);
                    }
                    held_as(&pin, &was, &format!("{step}: the clone"));
                }
            }
            ups += (ix.floor > floor) as usize;
            downs += (ix.floor < floor) as usize;
            below = below.max(ix.sparse.keys().filter(|&&k| k < ix.floor).count());
            if step % 499 == 0 {
                held_as(&ix, &model, &format!("step {step}"));
            }
        }
        held_as(&ix, &model, "the end");
        assert!(
            ups > 100 && downs > 100 && below > 3,
            "{ups} {downs} {below}"
        );
    }

    /// An index whose array starts at its first id, 10 000, and an id far
    /// below it, each holding one: the id written last takes the array,
    /// 10 000 into the sparse map. One written within the gap of 10 000
    /// makes the larger run, which takes it back, the far one sparse; one
    /// written within the gap of the first held id takes the floor down to
    /// it, and the next, within the gap of that sparse id, takes it into
    /// the array with room of a quarter of the array below it, down to 1.
    #[test]
    fn a_floor_lowered_over_a_sparse_id_takes_it_in() {
        use std::collections::BTreeMap;
        let at = |id: DocId| Loc {
            seg: 0,
            off: id as u32,
            len: 1,
        };
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        let mut floors = Vec::new();
        for id in [10_000u64, 3, 6_000, 2_000, 100] {
            ix.insert(id, at(id));
            model.insert(id, at(id));
            held_as(&ix, &model, &format!("{id}"));
            floors.push((ix.floor, ix.sparse.len()));
        }
        let want = [(10_000, 0), (3, 1), (6_000, 1), (1, 0), (1, 0)];
        assert_eq!(floors, want);
    }

    /// A collection whose ids climb while its oldest rows go -- sessions
    /// under `@ttl`, a queue -- holds an index of about twice its live
    /// span, cut as the ids go, not a slot for every id it ever gave: 200
    /// 000 ids with 3 000 live held 3.1 MB from 1, its array grown by
    /// doubling to 262 144 slots, here 98 KB. An id below the floor reads
    /// as absent; one written there is found.
    #[test]
    fn ids_climbing_through_deletes_hold_the_index_to_their_span() {
        let mut st = Store::new();
        let live = 3_000u64;
        let mut most = 0;
        for id in 1..=200_000u64 {
            st.append(OP_PUT, id, b"a session");
            if id > live {
                st.append(OP_DEL, id - live, &[]);
            }
            most = most.max(st.index.bytes());
        }
        let bound = 4 * (live as usize + CUT_FROM) * std::mem::size_of::<Loc>();
        assert!(most <= bound, "{most} bytes, past {bound}");
        assert_eq!(st.ids(), (200_001 - live..=200_000).collect::<Vec<_>>());
        assert!(st.index.floor > 200_000 - 3 * live, "{}", st.index.floor);
        assert!(!st.contains(1) && st.loc(st.index.floor - 1).is_none());
        // Below the first held id: one within the gap is in the array, in
        // the room before it or below the floor, one far is sparse; both
        // read.
        let (near, far) = (200_001 - live - 10, 7);
        st.append(OP_PUT, near, b"near");
        st.append(OP_PUT, far, b"far");
        assert!(st.contains(near) && st.contains(far) && st.index.floor <= near);
        assert_eq!(st.index.sparse.len(), 1);
        assert_eq!(&st.ids()[..2], &[far, near]);
        // A queue that empties at every write keeps no slot.
        let mut q = Store::new();
        for id in 1..=50_000u64 {
            q.append(OP_PUT, id, b"a job");
            q.append(OP_DEL, id, &[]);
        }
        let shape = (q.len(), q.index.dense.len(), q.index.floor);
        assert_eq!(shape, (0, 0, 50_001));
        assert!(q.index.bytes() <= 4 * CUT_FROM * std::mem::size_of::<Loc>());
    }

    /// Ids written to an index in turn, and a model of them.
    fn written(
        ix: &mut IdIndex,
        model: &mut std::collections::BTreeMap<DocId, Loc>,
        ids: impl IntoIterator<Item = DocId>,
    ) {
        for id in ids {
            let at = Loc {
                seg: 0,
                off: id as u32,
                len: 1,
            };
            ix.insert(id, at);
            model.insert(id, at);
        }
    }

    /// Ids deleted from an index in turn, and from its model.
    fn deleted(
        ix: &mut IdIndex,
        model: &mut std::collections::BTreeMap<DocId, Loc>,
        ids: impl IntoIterator<Item = DocId>,
    ) {
        for id in ids {
            assert_eq!(ix.remove(id), model.remove(&id), "{id}");
        }
    }

    /// Every order of writes ends with the rows dense, as an array from 1
    /// holds them, and the sparse map holding only the ids the gap keeps
    /// apart: a block's deletes from the highest id down put back from the
    /// lowest up, under the floor the deletes cut up, outnumbering the rows
    /// left or not; rows written below a
    /// replica's pending one, which came first, before it goes; the rows of
    /// an array emptied below a pending one written again, as a reseed
    /// does; and a load whose first id is its last, the rest in turn --
    /// and in no order, which ended dense before too. Before, the four left
    /// 137 500, 40 000, 15 904 and 45 903 sparse, each walk sorting them,
    /// until an open.
    #[test]
    fn every_order_of_writes_ends_with_the_rows_dense() {
        use std::collections::BTreeMap;
        let pending = 1 << 52;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=200_000);
        deleted(&mut ix, &mut model, (1..=150_000).rev());
        assert_eq!(ix.floor + ix.lead as u64, 150_001);
        written(&mut ix, &mut model, 1..=150_000);
        held_as(&ix, &model, "put back");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "put back");
        // No more than the array from 1 held, 262 144 slots.
        assert!(ix.bytes() <= 3_145_728, "{} bytes", ix.bytes());

        // Half the rows so, the deletes' put-back never outnumbering the
        // array: the array taken down within the gap of them takes them all.
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=200_000);
        deleted(&mut ix, &mut model, (1..=100_000).rev());
        written(&mut ix, &mut model, 1..=100_000);
        held_as(&ix, &model, "half put back");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "half put back");

        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, [pending]);
        written(&mut ix, &mut model, 1..=40_000);
        held_as(&ix, &model, "below a pending row");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 1), "below a pending row");
        deleted(&mut ix, &mut model, [pending]);
        held_as(&ix, &model, "the pending row gone");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "the pending row gone");

        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, (1..=20_000).chain([pending]));
        deleted(&mut ix, &mut model, 1..=20_000);
        assert_eq!((ix.floor, ix.dense.len()), (20_001, 0));
        written(&mut ix, &mut model, 1..=20_000);
        held_as(&ix, &model, "reseeded");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 1), "reseeded");

        // The last id first, then the rest in turn; then every id once in
        // an order drawn, the first the highest.
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, [50_000].into_iter().chain(1..50_000));
        held_as(&ix, &model, "the last first");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "the last first");
        let mut ids: Vec<DocId> = (1..50_000).collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for i in (1..ids.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ids.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, [50_000].into_iter().chain(ids));
        held_as(&ix, &model, "in no order");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "in no order");
    }

    /// The rows of the array deleted, from its end, below a run of sparse
    /// ids larger than what is left hand the run the array: an index whose
    /// first id was far above the rows written after it ends as an array
    /// from 1 holds them.
    #[test]
    fn rows_deleted_below_a_sparse_run_hand_it_the_array() {
        use std::collections::BTreeMap;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1_000_000..1_100_000);
        written(&mut ix, &mut model, 1..=50_000);
        assert_eq!((ix.floor, ix.sparse.len()), (1_000_000, 50_000));
        deleted(&mut ix, &mut model, (1_000_001..1_100_000).rev());
        held_as(&ix, &model, "deleted");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 1));
    }

    /// A cut moves no more slots than it drops, so however long ids climb
    /// through deletes each is moved a bounded number of times: 60 000
    /// live rows whose ids climb to 300 000 move fewer slots than they
    /// delete. Cut at every 4 096 empty slots, whatever the array, they
    /// moved the 60 000 live ones each time, 3.5 million.
    #[test]
    fn a_cut_moves_no_more_slots_than_it_drops() {
        let (live, to) = (60_000u64, 300_000u64);
        let mut ix = IdIndex::default();
        let at = Loc {
            seg: 0,
            off: 0,
            len: 1,
        };
        for id in 1..=to {
            ix.insert(id, at);
            if id > live {
                ix.remove(id - live);
            }
        }
        assert_eq!(ix.ids(), (to - live + 1..=to).collect::<Vec<_>>());
        let deletes = (to - live) as usize;
        assert!(ix.moved.0 <= deletes, "{} slots moved", ix.moved.0);
    }

    /// Ids written each below the one before take the room a quarter of
    /// the array leaves at a time: 100 000 of them below 100 000 rows copy
    /// it four times, 576 562 slots, where a gap at a time copied it 25
    /// times, 3.7 million. Each written just past the room the one before
    /// left is more than the gap below the first held id, and sparse: the
    /// array lowered by its quarter each time spanned 4 million slots after
    /// 29 of them, and 2^40 after 85.
    #[test]
    fn ids_written_downward_lower_the_floor_a_quarter_at_a_time() {
        use std::collections::BTreeMap;
        let (from, rows) = (1_000_000u64, 100_000u64);
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + rows);
        written(&mut ix, &mut model, (from - rows..from).rev());
        held_as(&ix, &model, "downward");
        let moved = ix.moved.0;
        assert!(moved <= 4 * 2 * rows as usize, "{moved} slots moved");

        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, [1 << 22]);
        for _ in 0..60 {
            let id = ix.floor - 1;
            written(&mut ix, &mut model, [id]);
        }
        held_as(&ix, &model, "past each quarter");
        let len = ix.dense.len();
        assert!(len <= 61 * (MAX_DENSE_GAP as usize + 1), "{len} slots");
    }

    /// A rebase that finds no run larger than the array looks again only
    /// once the sparse ids have doubled: 100 rows from 1 and 5 000 ids
    /// each more than the gap from the next look 6 times, the array where
    /// it was. Looking at every write past 100 sorted the ids 4 900 times.
    #[test]
    fn a_rebase_looks_again_once_the_sparse_ids_double() {
        use std::collections::BTreeMap;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=100);
        let apart = (1..=5_000u64).map(|k| 1_000_000 + k * (MAX_DENSE_GAP + 1));
        written(&mut ix, &mut model, apart);
        held_as(&ix, &model, "apart");
        assert_eq!((ix.floor, ix.dense.len(), ix.moved.1), (1, 100, 6));
    }

    /// Ids written downward copy the array as ids written upward grow it,
    /// once its span grows by a part of itself: every third id from 600
    /// 000 down copies 1.5 million slots, where the array lowered by the
    /// gap alone below half held copied it at every gap, 44 million; and 1
    /// 000 ids each the gap below the last, beside 1 000 rows, copy about
    /// four times the 4 million slots they span, where they copied the
    /// array at every write, 2.05e9 slots.
    #[test]
    fn ids_written_downward_copy_the_array_as_ids_written_upward_do() {
        use std::collections::BTreeMap;
        let rows = 200_000u64;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, (1..=rows).rev().map(|k| 3 * k));
        held_as(&ix, &model, "every third");
        let span = ix.dense.len() - ix.lead;
        assert_eq!((span, ix.sparse.len()), (3 * rows as usize - 2, 0));
        assert!(ix.moved.0 <= 6 * span, "{} slots copied", ix.moved.0);

        let from = 10_000_000u64;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + 1_000);
        written(
            &mut ix,
            &mut model,
            (1..=1_000).map(|k| from - k * MAX_DENSE_GAP),
        );
        held_as(&ix, &model, "a gap apart");
        let span = ix.dense.len() - ix.lead;
        assert_eq!(span as u64, 1_000 * MAX_DENSE_GAP + 1_000);
        assert!(ix.moved.0 <= 6 * span, "{} slots copied", ix.moved.0);
    }

    /// An id written just below the first held one and deleted, again and
    /// again, beside 100 000 rows copies the array once, as does one just
    /// below the floor: the room the lowering leaves holds the next, and
    /// the cut it would bring waits for more than the room. Cut to the
    /// first held slot, the lowering and the cut took turns, 169 000 slots
    /// a pair. Right after a cut, the room it keeps holds them, and they
    /// copy nothing.
    #[test]
    fn a_lowering_and_a_cut_do_not_take_turns() {
        use std::collections::BTreeMap;
        let (from, rows) = (1_000_000u64, 100_000usize);
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + rows as u64);
        for k in 0..2_000 {
            let id = match k % 2 {
                0 => ix.floor - 1,
                _ => ix.floor + ix.lead as u64 - 1,
            };
            written(&mut ix, &mut model, [id]);
            if k % 250 < 2 {
                held_as(&ix, &model, &format!("{k}: written"));
            }
            deleted(&mut ix, &mut model, [id]);
        }
        held_as(&ix, &model, "the pairs");
        assert!(ix.moved.0 <= rows, "{} slots copied", ix.moved.0);

        // Rows climbing through deletes, stopped at the array's third cut,
        // then the pairs below the first held id.
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        let (mut id, mut cuts) = (0, 0);
        while cuts < 3 {
            let floor = ix.floor;
            id += 1;
            written(&mut ix, &mut model, [id]);
            if id > rows as u64 {
                deleted(&mut ix, &mut model, [id - rows as u64]);
            }
            cuts += (ix.floor != floor) as usize;
        }
        let (was, first) = (ix.moved.0, ix.floor + ix.lead as u64);
        let span = ix.dense.len() - ix.lead;
        assert_eq!(ix.lead, (span / 4).min(CUT_FROM), "the room a cut keeps");
        for _ in 0..1_000 {
            written(&mut ix, &mut model, [first - 1]);
            deleted(&mut ix, &mut model, [first - 1]);
        }
        held_as(&ix, &model, "the pairs after a cut");
        assert_eq!(ix.moved.0, was, "slots copied");
    }

    /// A rebase undone brings the rows it moved into the map back: 10 000
    /// rows from 1 and 15 000 written from 5 000 000 -- the run taking the
    /// array once it outnumbers the rows -- deleted again from the last,
    /// as a block put back, or from the first, an import gone, leave the
    /// rows in the array and the map empty, after a look or two. Following
    /// the map's growth alone, the looks waited for 20 000 sparse ids, and
    /// the 10 000 rows stayed sparse, and the 5 000 written after them.
    #[test]
    fn a_rebase_undone_brings_the_rows_back_into_the_array() {
        use std::collections::BTreeMap;
        let far = 5_000_000u64;
        for from_the_last in [true, false] {
            let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
            written(&mut ix, &mut model, 1..=10_000);
            written(&mut ix, &mut model, far..far + 15_000);
            assert_eq!((ix.floor, ix.sparse.len()), (far, 10_000));
            let looks = ix.moved.1;
            match from_the_last {
                true => deleted(&mut ix, &mut model, (far..far + 15_000).rev()),
                false => deleted(&mut ix, &mut model, far..far + 15_000),
            }
            held_as(&ix, &model, &format!("from the last: {from_the_last}"));
            assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "{from_the_last}");
            assert!(ix.moved.1 - looks <= 2, "{} looks", ix.moved.1 - looks);
            written(&mut ix, &mut model, 10_001..=15_000);
            assert_eq!((ix.floor, ix.sparse.len()), (1, 0), "{from_the_last}");
        }
        // Undone in part, 4 500 of the far rows left: the deletes from the
        // array count from the first, while the map holds 64 ids or more,
        // and the 10 000 rows outnumber what is left after as many deletes
        // as the look sorted. Counted only once the map outnumbered the
        // array, the deletes before -- 5 000 of them -- counted for
        // nothing, and the rows stayed sparse.
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=10_000);
        written(&mut ix, &mut model, far..far + 15_000);
        deleted(&mut ix, &mut model, (far + 4_500..far + 15_000).rev());
        held_as(&ix, &model, "in part");
        assert_eq!((ix.floor, ix.sparse.len()), (1, 4_500), "in part");
    }

    /// A lowering copies the span from the first held slot to the last
    /// alone, with room of a quarter of it: 5 000 rows, the highest 4 000
    /// deleted -- the slots past the last held one fewer than a cut waits
    /// for -- and one written just below copy the 1 000 left, with 250
    /// slots of room. Counted to the array's end, the room was 1 250, past
    /// what the cut's bound allows once that row is deleted with a gap
    /// below it.
    #[test]
    fn a_lowering_copies_the_held_span_alone() {
        use std::collections::BTreeMap;
        let (from, rows) = (1_000_000u64, 5_000u64);
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + rows);
        deleted(&mut ix, &mut model, (from + 1_000..from + rows).rev());
        assert_eq!((ix.dense.len(), ix.last), (rows as usize, 999));
        written(&mut ix, &mut model, [from - 1]);
        held_as(&ix, &model, "lowered");
        let shape = (ix.lead, ix.dense.len(), ix.moved.0);
        assert_eq!(shape, (250, 250 + 1 + 1_000, 1_000));
    }

    /// An id the gap from the held range is in the array, either side,
    /// and one a slot further is sparse; a sparse id the gap from where the
    /// array grows or is taken down to is taken in. Each bound moved by one
    /// leaves one id sparse within the gap, which `held_as` refuses.
    #[test]
    fn an_id_the_gap_from_the_held_range_is_taken_in_either_side() {
        use std::collections::BTreeMap;
        let gap = MAX_DENSE_GAP;
        for (ids, sparse) in [
            (vec![1, 2 + gap, 2], 0),
            (vec![1, 1 + gap], 0),
            (vec![1, 2 + gap], 1),
            (vec![10_000, 10_000 - gap - 1, 9_999], 0),
            (vec![10_000, 10_000 - gap], 0),
            (vec![10_000, 10_000 - gap - 1], 1),
            (
                vec![10_000, 10_000 - gap - 1, 10_000 - 2 * gap - 1, 9_999],
                0,
            ),
        ] {
            let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
            written(&mut ix, &mut model, ids.iter().copied());
            held_as(&ix, &model, &format!("{ids:?}"));
            assert_eq!(ix.sparse.len(), sparse, "{ids:?}");
        }
    }

    /// A write that takes the array down sorts the sparse ids only where
    /// one may be within reach, and only those below the array: 100 000
    /// ids written downward beside 10 000 sparse ids far above it and 100
    /// below where they end sort none, and the one within the gap of the
    /// highest below sorts the 100 and takes that one. Sorting the whole
    /// map at each lowering, the writes sorted it at every one.
    #[test]
    fn a_lowering_sorts_only_the_sparse_ids_below_in_reach() {
        use std::collections::BTreeMap;
        let (from, apart) = (1_000_000u64, MAX_DENSE_GAP + 1);
        let low = from - 100_000;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + 100);
        written(
            &mut ix,
            &mut model,
            (1..=10_000).map(|k| (1 << 40) + k * apart),
        );
        written(&mut ix, &mut model, (1..=100).map(|k| low - k * apart));
        written(&mut ix, &mut model, (low..from).rev());
        held_as(&ix, &model, "downward");
        assert_eq!((ix.moved.2, ix.sparse.len()), (0, 10_100));
        written(&mut ix, &mut model, [low - 1]);
        held_as(&ix, &model, "in reach");
        assert_eq!((ix.moved.2, ix.sparse.len()), (100, 10_099));
    }

    /// A collection whose ids fall while its highest rows go -- reverse
    /// timestamps under `@ttl` -- holds an index of about its live span,
    /// as ids climbing do: 3 000 live rows from 2^40 downward through 200
    /// 000 ids held 2.8 MB while the array's end was never cut, and 26.3
    /// MB through 2 million. The end's cut moves nothing.
    #[test]
    fn ids_falling_through_deletes_hold_the_index_to_their_span() {
        let mut st = Store::new();
        let (live, top) = (3_000u64, 1u64 << 40);
        let mut most = 0;
        for k in 0..200_000u64 {
            st.append(OP_PUT, top - k, b"a session");
            if k >= live {
                st.append(OP_DEL, top - k + live, &[]);
            }
            most = most.max(st.index.bytes());
        }
        let bound = 4 * (live as usize + CUT_FROM) * std::mem::size_of::<Loc>();
        assert!(most <= bound, "{most} bytes, past {bound}");
        let ids: Vec<DocId> = (top - 199_999..top - 199_999 + live).collect();
        assert_eq!(st.ids(), ids);
        let ix = &st.index;
        assert!(ix.dense.len() - 1 - ix.last <= live as usize + CUT_FROM);
        assert_eq!(ix.sparse.len(), 0);
    }

    /// Rows deleted from the highest down, nothing written below them, give
    /// their slots back as they go, moving none: 100 000 rows deleted down
    /// to the lowest 1 000 leave an array of at most the span and the run
    /// a cut waits for, and an allocation of four times that. A lowering
    /// drops the run too, so ids falling cut it either way; here only the
    /// end's own cut does.
    #[test]
    fn rows_deleted_from_the_highest_down_give_their_slots_back() {
        use std::collections::BTreeMap;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=100_000);
        deleted(&mut ix, &mut model, (1_001..=100_000).rev());
        held_as(&ix, &model, "deleted");
        let most = 1_000 + CUT_FROM + 1_000 / 4;
        let (len, cap) = (ix.dense.len(), ix.dense.capacity());
        assert!(len <= most && cap <= 4 * most, "{len} slots, {cap} held");
        assert_eq!(ix.moved.0, 0, "slots moved");
    }

    /// The run past the last held slot does not hold off the front's cut:
    /// 14 000 rows, the highest 4 000 deleted -- a run the end keeps -- and
    /// the lowest 6 000: the front is cut once its run passes the 4 000
    /// rows left and their room. Counted to the array's end, as 3edf5df5
    /// counted the span, the 4 000 empty slots past the last held one
    /// raised the bound to 10 000, and the front kept 6 000 slots for 4 000
    /// rows.
    #[test]
    fn the_run_past_the_last_held_slot_does_not_hold_off_the_fronts_cut() {
        use std::collections::BTreeMap;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=14_000);
        deleted(&mut ix, &mut model, (10_001..=14_000).rev());
        assert_eq!(ix.dense.len() - 1 - ix.last, 4_000);
        deleted(&mut ix, &mut model, 1..=6_000);
        held_as(&ix, &model, "both ends");
        assert!(ix.floor > 1, "the front cut");
    }

    /// One row moved a gap at a time, the one before it deleted, holds the
    /// array to about the gap, down as up: moved down 10 000 times, the
    /// array's end never cut, it reached 532 MB.
    #[test]
    fn a_row_moved_a_gap_at_a_time_holds_the_array_to_the_gap() {
        use std::collections::BTreeMap;
        for down in [true, false] {
            let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
            let mut id = 1u64 << 40;
            written(&mut ix, &mut model, [id]);
            let mut most = 0;
            for _ in 0..10_000 {
                let next = match down {
                    true => id - MAX_DENSE_GAP,
                    false => id + MAX_DENSE_GAP,
                };
                written(&mut ix, &mut model, [next]);
                deleted(&mut ix, &mut model, [id]);
                id = next;
                most = most.max(ix.dense.capacity());
            }
            held_as(&ix, &model, &format!("down: {down}"));
            let bound = 4 * (MAX_DENSE_GAP as usize + 1);
            assert!(most <= bound, "down: {down}, {most} slots");
        }
    }

    /// An id written within the gap past the last held one and deleted,
    /// again and again, beside 100 000 rows copies, fills and walks
    /// nothing, as one written below the first held one does: the end's
    /// cut waits for more than the gap, and `before` remembers where the
    /// held slots stop. Cut at every delete of the last held slot, the run
    /// was filled again at every pair.
    #[test]
    fn an_id_written_past_the_last_and_deleted_moves_nothing() {
        use std::collections::BTreeMap;
        let (from, rows) = (1_000_000u64, 100_000u64);
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + rows);
        let past = from + rows - 1 + MAX_DENSE_GAP;
        let cap = ix.dense.capacity();
        for k in 0..1_000 {
            written(&mut ix, &mut model, [past - k % 2]);
            deleted(&mut ix, &mut model, [past - k % 2]);
        }
        held_as(&ix, &model, "the pairs");
        assert_eq!((ix.moved.0, ix.moved.3), (0, 0), "slots copied and walked");
        assert_eq!(ix.dense.capacity(), cap);
        // The run past the last held slot kept, not filled again a pair.
        assert_eq!(ix.dense.len() as u64, rows + MAX_DENSE_GAP, "slots");
    }

    /// Ids written past the last held one and deleted from the inside out
    /// walk back at most two gaps a write: 20 ids each a gap past the one
    /// before, deleted in the order written, round after round beside 100
    /// 000 rows; and two ids 20 000 and 40 000 past the last held one after
    /// the highest 50 000 rows were deleted, the run past it kept as it is
    /// within the span. Before the holes ([`IdIndex::holes`]), the run so
    /// kept was walked back 40 000 slots a round, and the first form of
    /// round 5 held the run to the gap for it.
    #[test]
    fn ids_deleted_from_the_inside_out_walk_back_two_gaps_a_write() {
        use std::collections::BTreeMap;
        let (from, rows) = (1_000_000u64, 100_000u64);
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, from..from + rows);
        let mut writes = 0;
        for _ in 0..50 {
            let ids: Vec<DocId> = (1..=20)
                .map(|k| from + rows - 1 + k * MAX_DENSE_GAP)
                .collect();
            written(&mut ix, &mut model, ids.iter().copied());
            deleted(&mut ix, &mut model, ids);
            writes += 20;
        }
        deleted(&mut ix, &mut model, (from + rows / 2..from + rows).rev());
        let last = from + rows / 2 - 1;
        for _ in 0..100 {
            let ids = [last + 20_000, last + 40_000];
            written(&mut ix, &mut model, ids);
            deleted(&mut ix, &mut model, ids);
            writes += 2;
        }
        held_as(&ix, &model, "inside out");
        let bound = 2 * MAX_DENSE_GAP as usize * writes;
        assert!(
            ix.moved.3 <= bound,
            "{} slots walked, past {bound}",
            ix.moved.3
        );
    }

    /// A relocation visits the ids in order with a sparse id in the room
    /// before the first held slot -- rows 100 000 to 108 999, the first 5
    /// 000 deleted and the last 300, then 100 005 written, more than the
    /// gap below the first held id -- and leaves the array the held span
    /// alone, both ends cut: the room uncut, the sparse id came after the
    /// array.
    #[test]
    fn a_relocation_visits_a_sparse_id_in_the_room_first() {
        use std::collections::BTreeMap;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 100_000..109_000);
        deleted(&mut ix, &mut model, 100_000..105_000);
        deleted(&mut ix, &mut model, (108_700..109_000).rev());
        written(&mut ix, &mut model, [100_005]);
        assert_eq!((ix.floor, ix.lead), (100_000, 5_000));
        assert_eq!(ix.dense.len() - 1 - ix.last, 300);
        assert!(ix.sparse.contains_key(&100_005));
        let mut seen = Vec::new();
        ix.relocate(|id, l| {
            seen.push(id);
            l
        });
        assert_eq!(seen, model.keys().copied().collect::<Vec<_>>());
        held_as(&ix, &model, "relocated");
        assert_eq!((ix.floor, ix.lead, ix.dense.len()), (105_000, 0, 3_700));
    }

    /// The look's count is held to what the sparse map holds: 1 000 rows
    /// from 1, 10 000 ids a million apart written and deleted, and 5 000
    /// rows from 2^45 -- whose run takes the array at its 1 001st row, as
    /// without the far ids before. Left at the 6 016 writes the last look
    /// set, the count outlived the ids it counted, and all 5 000 stayed
    /// sparse, 1 016 writes short of a look.
    #[test]
    fn the_looks_count_is_held_to_what_the_map_holds() {
        use std::collections::BTreeMap;
        let (mut ix, mut model) = (IdIndex::default(), BTreeMap::new());
        written(&mut ix, &mut model, 1..=1_000);
        let apart: Vec<DocId> = (1..=10_000u64).map(|k| k * 1_000_000).collect();
        written(&mut ix, &mut model, apart.iter().copied());
        deleted(&mut ix, &mut model, apart);
        assert_eq!((ix.sparse.len(), ix.rebase_in), (0, 0));
        let far = 1u64 << 45;
        written(&mut ix, &mut model, far..far + 1_001);
        assert_eq!((ix.floor, ix.sparse.len()), (far, 1_000));
        written(&mut ix, &mut model, far + 1_001..far + 5_000);
        held_as(&ix, &model, "far");
        assert_eq!((ix.floor, ix.sparse.len()), (far, 1_000));
    }

    /// A seeded draw.
    struct Draw(u64);

    impl Draw {
        fn n(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n.max(1)
        }
    }

    /// An open block's writes, each id with the location it had before.
    type Undo = Vec<(DocId, Option<Loc>)>;

    /// The model a workload is held to, which knows nothing of the index:
    /// the live ids and their locations; the id a new row is given, the
    /// highest ever given plus 1 as `Store::allocate_id` gives it -- raised
    /// by any id written, lowered by a block put back alone
    /// (`Store::rewind`); and the writes of an open block, to put back.
    #[derive(Default)]
    struct Workload {
        live: std::collections::BTreeMap<DocId, Loc>,
        next: DocId,
        block: Option<(DocId, Undo)>,
        writes: usize,
        stamp: u32,
    }

    impl Workload {
        /// `id` written, as a `put` writes it.
        fn put(&mut self, ix: &mut IdIndex, id: DocId) {
            self.stamp += 1;
            let at = Loc {
                seg: self.stamp,
                off: id as u32,
                len: (id >> 32) as u32,
            };
            ix.insert(id, at);
            let was = self.live.insert(id, at);
            if let Some((_, undo)) = &mut self.block {
                undo.push((id, was));
            }
            self.next = self.next.max(id + 1);
            self.writes += 1;
            if was.is_none() && self.live.len() <= 64 {
                self.lands_dense(ix, id);
            }
        }

        /// A new row, given the next id.
        fn row(&mut self, ix: &mut IdIndex) -> DocId {
            let id = self.next.max(1);
            self.put(ix, id);
            id
        }

        fn del(&mut self, ix: &mut IdIndex, id: DocId) {
            let was = self.live.remove(&id);
            assert_eq!(ix.remove(id), was, "{id}");
            if was.is_some() {
                if let Some((_, undo)) = &mut self.block {
                    undo.push((id, was));
                }
                self.writes += 1;
            }
        }

        /// The `n` oldest ids, or the `n` newest.
        fn ends(&self, n: usize, newest: bool) -> Vec<DocId> {
            match newest {
                true => self.live.keys().rev().take(n).copied().collect(),
                false => self.live.keys().take(n).copied().collect(),
            }
        }

        /// A live id drawn from the range, or none.
        fn any(&self, d: &mut Draw) -> Option<DocId> {
            let (lo, hi) = (*self.live.keys().next()?, *self.live.keys().next_back()?);
            let k = lo + d.n(hi - lo + 1);
            self.live.range(k..).next().map(|(&id, _)| id)
        }

        /// The block put back, its last write first, as a block that does
        /// not land points its ids back (`Store::point`) and rewinds the
        /// next id (`Store::rewind`).
        fn put_back(&mut self, ix: &mut IdIndex) {
            let (next, undo) = self.block.take().unwrap();
            for (id, was) in undo.into_iter().rev() {
                match was {
                    Some(l) => {
                        ix.insert(id, l);
                        self.live.insert(id, l);
                    }
                    None => {
                        assert!(ix.remove(id).is_some(), "{id}");
                        self.live.remove(&id);
                    }
                }
                self.writes += 1;
            }
            self.next = next;
        }

        /// A new id written into a table of a few rows lands in the array
        /// where its run is as large as any other: one long-lived row
        /// beside rows put and deleted one at a time sent every row after
        /// the 4 098th to the sparse map, a hash and an allocation a write.
        fn lands_dense(&self, ix: &IdIndex, id: DocId) {
            let ids: Vec<DocId> = self.live.keys().copied().collect();
            let runs = live_runs(&ids, |a, b| a.min(b));
            let at = ids.binary_search(&id).unwrap();
            let mine = runs.iter().find(|r| r.contains(&at)).unwrap().len();
            if runs.iter().all(|r| r.len() <= mine) {
                assert!(
                    !ix.sparse.contains_key(&id),
                    "{id} sparse, its run of {mine} the largest of {ids:?}"
                );
            }
        }
    }

    /// The model's live ids in runs: split where two are more than the gap
    /// apart, and neighbouring runs joined while the hole between them is
    /// no more than `join` of the ids each side holds.
    fn live_runs(
        ids: &[DocId],
        join: impl Fn(usize, usize) -> usize,
    ) -> Vec<std::ops::Range<usize>> {
        let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
        for i in 0..ids.len() {
            match runs.last_mut() {
                Some(r) if ids[i] - ids[i - 1] <= MAX_DENSE_GAP => r.end = i + 1,
                _ => runs.push(i..i + 1),
            }
        }
        let mut k = 0;
        while k + 1 < runs.len() {
            let (a, b) = (runs[k].clone(), runs[k + 1].clone());
            if ids[b.start] - ids[a.end - 1] - 1 <= join(a.len(), b.len()) as u64 {
                runs[k] = a.start..b.end;
                runs.remove(k + 1);
                k = k.saturating_sub(1);
            } else {
                k += 1;
            }
        }
        runs
    }

    /// What a workload's index is after each phase, by the model: it
    /// answers as the model does; the live run -- the model's largest, its
    /// ids each within the gap of the next or across a hole no longer than
    /// half the smaller side -- is dense once it outnumbers the rest four
    /// times over and by 64, after the looks writes would bring, on a copy,
    /// counted on the model's ids, so an id sent sparse fails it (when a
    /// look comes is held apart: its count never more than the map holds,
    /// and what it sorts by the cost); the array's
    /// allocation is within four times the most its cuts leave, three and
    /// a quarter times the live span and two `CUT_FROM`s, the live span the
    /// wider of the model's widest run -- a hole joined where it is within
    /// twice the ids either side holds -- and the model's live ids the
    /// slots reach, dense or not, which the gap may have chained across
    /// rows since deleted, as an array from 1 did; and the map's within
    /// four times the live ids.
    fn holds_workload(ix: &IdIndex, w: &Workload, what: &str) {
        held_as(ix, &w.live, what);
        let ids: Vec<DocId> = w.live.keys().copied().collect();
        // A copy of the index after the looks writes would bring: when a
        // look comes is held apart, its count never more than the map holds
        // (`held_shape`) and what it sorts by the cost below.
        let mut looked = ix.clone();
        for _ in 0..8 {
            let was = looked.sparse.len();
            looked.rebase_in = 0;
            looked.rebase(0);
            if looked.sparse.len() == was {
                break;
            }
        }
        let runs = live_runs(&ids, |a, b| a.min(b) / 2);
        if let Some(r) = runs.iter().max_by_key(|r| r.len()) {
            let rest = ids.len() - r.len();
            if r.len() > 4 * rest + REBASE_FROM {
                let ids = &ids[r.clone()];
                let sparse = ids.iter().filter(|k| looked.sparse.contains_key(k)).count();
                assert_eq!(
                    sparse,
                    0,
                    "{what}: {sparse} of the live run's {} ids sparse after a look, from {} to {}, {rest} beside it",
                    ids.len(),
                    ids[0],
                    ids[ids.len() - 1],
                );
            }
        }
        // The live ids the array's slots reach, whichever of them the
        // index holds in the map, and the model's widest run.
        let (from, to) = (ix.floor, ix.floor + ix.dense.len() as u64);
        let reached = w.live.range(from..to.max(from));
        let span = match (reached.clone().next(), reached.last()) {
            (Some((a, _)), Some((b, _))) => (b - a + 1) as usize,
            _ => 0,
        };
        let widest = live_runs(&ids, |a, b| 2 * (a + b))
            .iter()
            .map(|r| (ids[r.end - 1] - ids[r.start] + 1) as usize)
            .max()
            .unwrap_or(0);
        let span = span.max(widest);
        let (cap, most) = (ix.dense.capacity(), 4 * (13 * span / 4 + 2 * CUT_FROM));
        assert!(cap <= most, "{what}: {cap} slots for a span of {span}");
        let map = ix.sparse.capacity();
        assert!(
            map <= 4 * ids.len() + 64,
            "{what}: the map holds room for {map}, {} live",
            ids.len()
        );
    }

    /// One phase of a workload, of `kind`, its writes a `scale`th as many.
    /// Returns whether its writes reach the gap from the rows a write at a
    /// time, as only ids given from outside can.
    fn workload_phase(
        ix: &mut IdIndex,
        w: &mut Workload,
        d: &mut Draw,
        kind: usize,
        scale: u64,
    ) -> bool {
        let gap = MAX_DENSE_GAP;
        let (lo, hi) = match (w.live.keys().next(), w.live.keys().next_back()) {
            (Some(&lo), Some(&hi)) => (lo, hi),
            _ => (w.next.max(1), w.next.max(1)),
        };
        match kind {
            // A queue, or sessions under `@ttl`: ids climbing, the oldest
            // deleted once the table holds its count, one at a time or in
            // a sweep's batches.
            0 | 1 => {
                let (keep, batch) = (1 + d.n(2_000) as usize, [1, 2 + d.n(63) as usize][kind]);
                for _ in 0..(2_000 + d.n(14_000)) / scale {
                    w.row(ix);
                    if w.live.len() >= keep + batch {
                        for id in w.ends(batch, false) {
                            w.del(ix, id);
                        }
                    }
                }
            }
            // A stack, or an outbox: rows added and the newest deleted.
            2 => {
                for _ in 0..(1_000 + d.n(8_000)) / scale {
                    match d.n(100) < 55 {
                        true => drop(w.row(ix)),
                        false => w.ends(1, true).into_iter().for_each(|id| w.del(ix, id)),
                    }
                }
            }
            // Rows written in batches and deleted, oldest first, newest
            // first or in no order, beside what the table holds.
            3 => {
                let (b, order) = (2 + d.n(15), d.n(3));
                for _ in 0..(200 + d.n(2_000)) / scale {
                    let mut ids: Vec<DocId> = (0..b).map(|_| w.row(ix)).collect();
                    match order {
                        0 => {}
                        1 => ids.reverse(),
                        _ => (1..ids.len())
                            .rev()
                            .for_each(|i| ids.swap(i, d.n(i as u64 + 1) as usize)),
                    }
                    ids.into_iter().for_each(|id| w.del(ix, id));
                }
            }
            // Rows written, `del s where id > X` deleting the newest in
            // either order, then new rows.
            4 => {
                for _ in 0..d.n(20_000) / scale {
                    w.row(ix);
                }
                let mut ids = w.ends(d.n(w.live.len() as u64 + 1) as usize, true);
                if d.n(2) == 0 {
                    ids.reverse();
                }
                let m = ids.len() as u64;
                ids.into_iter().for_each(|id| w.del(ix, id));
                for _ in 0..d.n(2 * m + 2_000) / scale {
                    w.row(ix);
                }
            }
            // Rows put and deleted beside what the table holds, one at a
            // time or a few: an outbox beside long-lived rows.
            5 => {
                let b = 1 + d.n(4);
                for _ in 0..(1_000 + d.n(9_000)) / scale {
                    let ids: Vec<DocId> = (0..b).map(|_| w.row(ix)).collect();
                    ids.into_iter().for_each(|id| w.del(ix, id));
                }
            }
            // Ids given from outside, ascending past the rows, descending
            // below them or in no order, some deleted as they come.
            6 => {
                let dir = d.n(3);
                let mut id = match dir {
                    0 => hi + 1 + d.n(2 * gap),
                    _ => lo.saturating_sub(1 + d.n(2 * gap)).max(1),
                };
                for _ in 0..(200 + d.n(3_000)) / scale {
                    w.put(ix, id);
                    if d.n(3) == 0 {
                        if let Some(k) = w.any(d) {
                            w.del(ix, k);
                        }
                    }
                    id = match dir {
                        0 => id + 1 + d.n(3),
                        1 => id.saturating_sub(1 + d.n(3)).max(1),
                        _ => (lo + d.n(hi - lo + 1 + 2 * gap)).saturating_sub(gap).max(1),
                    };
                }
                return dir == 2;
            }
            // A replica's pending row at 2^52 beside the rows its primary
            // gives, climbing, the oldest going; the pending row answered
            // or not.
            7 => {
                let pending = (1 << 52) + d.n(64);
                w.put(ix, pending);
                for id in (hi + 1..).take(((500 + d.n(5_000)) / scale) as usize) {
                    w.put(ix, id);
                    if d.n(2) == 0 {
                        w.ends(1, false).into_iter().for_each(|k| w.del(ix, k));
                    }
                }
                if d.n(3) > 0 {
                    w.del(ix, pending);
                }
            }
            // Writes reaching the gap from the rows, as only ids given from
            // outside do: a sawtooth past either end, a row moved a gap at
            // a time, a far run written and deleted, all but one of it or
            // kept, writes and deletes anywhere within the gap of the rows.
            8 => {
                match d.n(4) {
                    0 => {
                        let up = d.n(2) == 0;
                        let (mut id, mut ids) = (if up { hi } else { lo }, Vec::new());
                        for _ in 0..1 + d.n(60) {
                            id = match up {
                                true => id + 1 + d.n(gap),
                                false => id.saturating_sub(1 + d.n(gap)).max(1),
                            };
                            w.put(ix, id);
                            ids.push(id);
                        }
                        if d.n(2) == 0 {
                            ids.reverse();
                        }
                        ids.into_iter().for_each(|id| w.del(ix, id));
                    }
                    1 => {
                        let up = d.n(2) == 0;
                        let mut id = if up { hi } else { lo };
                        w.put(ix, id);
                        for _ in 0..(50 + d.n(400)) / scale {
                            let next = match up {
                                true => id + 1 + d.n(gap),
                                false => id.saturating_sub(1 + d.n(gap)).max(1),
                            };
                            w.put(ix, next);
                            if next != id {
                                w.del(ix, id);
                            }
                            id = next;
                        }
                    }
                    2 => {
                        let (far, m) = ((1 << 44) + d.n(1 << 40), d.n(3_000) / scale);
                        (0..m).for_each(|k| w.put(ix, far + k));
                        match d.n(4) {
                            0 => (0..m).rev().for_each(|k| w.del(ix, far + k)),
                            1 => (0..m).for_each(|k| w.del(ix, far + k)),
                            2 => (1..m).for_each(|k| w.del(ix, far + k)),
                            _ => {}
                        }
                    }
                    _ => {
                        for _ in 0..(200 + d.n(1_500)) / scale {
                            let id = (lo + d.n(hi - lo + 1 + 2 * gap)).saturating_sub(gap).max(1);
                            match d.n(2) {
                                0 => w.put(ix, id),
                                _ => w.del(ix, id),
                            }
                        }
                    }
                }
                return true;
            }
            // Rows updated and deleted anywhere.
            _ => {
                for _ in 0..(200 + d.n(3_000)) / scale {
                    if let Some(id) = w.any(d) {
                        match d.n(2) {
                            0 => w.put(ix, id),
                            _ => w.del(ix, id),
                        }
                    }
                }
            }
        }
        false
    }

    /// The id index under the orders of writes real workloads make, mixed
    /// and drawn from a seed, held after each phase to what the model alone
    /// says of it ([`holds_workload`]) and to the work its writes cost:
    /// queues and sessions, stacks and outboxes, batches deleted in any
    /// order, the newest rows deleted then new ones written, rows put and
    /// deleted beside long-lived ones, ids from outside ascending,
    /// descending and in no order, a pending row at 2^52, the sawtooths and
    /// far runs only ids from outside make, blocks of each kind put back,
    /// relocations, and clones taken before a phase and read after it as a
    /// pin reads them. The slots copied, walked over, grown by or sorted
    /// in a phase are a few a write, or three gaps a write where its ids
    /// reach the gap from the rows; and the array and the map it held at
    /// the phase's start three times, a copy and a walk from either end,
    /// and those it ends with eight times, as rows put back below the
    /// first held id a few apart copy the array once its span has grown by
    /// a quarter, five times its last span at most. `FENNEC_ID_SEEDS` runs
    /// that many seeds more.
    #[test]
    fn the_id_index_holds_every_workload_dense_in_its_span_and_work() {
        let more = std::env::var("FENNEC_ID_SEEDS").map_or(0, |s| s.parse().unwrap());
        let mut kinds = [0usize; 12];
        for seed in 0..2 + more {
            workload(0x5851_f42d_4c95_7f2d ^ (seed << 32 | seed), 150, &mut kinds);
        }
        assert!(kinds.iter().all(|&k| k >= 10), "{kinds:?}");
    }

    fn workload(seed: u64, phases: usize, kinds: &mut [usize; 12]) {
        let mut d = Draw(seed);
        let (mut ix, mut w) = (IdIndex::default(), Workload::default());
        let cost = |ix: &IdIndex| ix.moved.0 + ix.moved.2 + ix.moved.3 + ix.moved.4 + ix.moved.5;
        for phase in 0..phases {
            let kind = d.n(12) as usize;
            kinds[kind] += 1;
            let pin = (d.n(4) == 0).then(|| (ix.clone(), w.live.clone()));
            let (was, writes, owed) = (cost(&ix), w.writes, ix.dense.len() + ix.sparse.len());
            let gaps = match kind {
                10 => {
                    let mut seen = Vec::new();
                    ix.relocate(|id, l| {
                        seen.push(id);
                        l
                    });
                    assert!(
                        seen.iter().eq(w.live.keys()),
                        "{seed:x} phase {phase}: relocated"
                    );
                    false
                }
                11 => {
                    w.block = Some((w.next, Vec::new()));
                    let kind = d.n(10) as usize;
                    let gaps = workload_phase(&mut ix, &mut w, &mut d, kind, 4);
                    w.put_back(&mut ix);
                    gaps
                }
                _ => workload_phase(&mut ix, &mut w, &mut d, kind, 1),
            };
            let what = format!("{seed:x} phase {phase}, kind {kind}");
            holds_workload(&ix, &w, &what);
            if let Some((pin, live)) = pin {
                held_as(&pin, &live, &format!("{what}: the clone"));
            }
            let (spent, writes) = (cost(&ix) - was, w.writes - writes);
            let most = [16, 3 * MAX_DENSE_GAP as usize][gaps as usize] * writes
                + 3 * owed
                + 8 * (ix.dense.len() + ix.sparse.len())
                + 4 * CUT_FROM;
            assert!(
                spent <= most,
                "{what}: {spent} slots copied, walked, grown or sorted for {writes} writes"
            );
        }
    }
}
