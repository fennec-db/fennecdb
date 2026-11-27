//! A compact of a file past the page cache copies a store's live records
//! reading the file in turn (docs/design/storage.md#reading-the-file-in-turn):
//! the same bytes, in id order, that [`Store::write_live`]'s walk by id
//! writes, read from the old file from its start to its end
//! ([`Store::compact_live`]).
//!
//! By id, the walk reads the mapped file wherever each id's last write put
//! its record: once updates at random keys have moved the records out of id
//! order, that is a page at a time at random, which past the page cache is
//! a read from the disk each -- a million 1 KB records, each updated once,
//! 2.04 GB of which 1.02 live, took 45 to 51 s and 530 000 major faults to
//! compact in a container of 512 MB. Here every live record's place in the
//! new file is known before a byte of it is read: the frames are laid in id
//! order, so each one's place is the sum of the frames before it
//! ([`Store::buckets`]). The records are read in the order they stand --
//! the mapped stretches, then the segments -- each one the id index points
//! at kept ([`Store::each_standing`]), and dealt into buckets of ids, each
//! at most the budget the room the page cache has sets ([`budget`]): a
//! stretch of a scratch file beside the database each ([`Scratch`]), written
//! through a buffer a bucket. Then each bucket in turn is read back into
//! one buffer, each record put at its place, and written into the new file.
//! Where the live records fit one bucket, no scratch file is made: they are
//! put in their places in the buffer as they are read.
//!
//! What it holds, and why: the buffer of one bucket and a place a slot of
//! the id index's span of it, 4 bytes, while the buckets are read back; the
//! buckets' buffers while they are dealt, 16 MiB together at the most
//! whatever the file ([`BUCKETS_MOST`]) -- where a compact by id peaked
//! already; and the collection's live bytes on the disk, once, for as long
//! as its record is written -- one collection at a time. It is asked only
//! where the volume gives that beside what the compact still writes, and
//! else the records are walked by id, as they are below [`FROM`], past
//! [`BUCKETS_MOST`] buckets or where the room is not known. A scratch file
//! that fails before a byte of the new file is written leaves them to the
//! walk by id too, said once ([`walked_by_id`]); one that fails after fails
//! the compact, as a write of the new file does.

use super::*;
use crate::crc::crc32c;
use crate::engine::ImageOut;
use crate::fs::Scratch;
use std::cell::Cell;
use std::path::PathBuf;

/// The live bytes below which a store's records are walked by id whatever
/// the room: at most a thousand pages of 4 KB read at random, a fraction of
/// a second from the disk, and no buffer nor scratch file to make. The perf
/// gate's compacts (10 000 rows, under 1 MB and 2 MB) stay below it, and so
/// ask the page cache nothing.
pub(crate) const FROM: usize = 4 << 20;

/// The most a bucket holds, and the buckets' buffers together while they
/// are dealt: a quarter of the room the page cache has, between these. The
/// buckets' size moved the time little and the memory much: compacting a
/// million 1 KB records, 2.04 GB of which 1.02 live, in a container of 512
/// MB, buckets of 64 MiB took 5.0-8.1 s and the process's anonymous memory
/// to 76-78 MB, of 32 MiB 5.2-7.9 s and 44-46 MB, of 16 MiB 4.6-6.6 s and
/// 28-30 MB -- where the walk by id peaked, 27-29.
const BUDGET_MAX: u64 = 16 << 20;
const BUDGET_MIN: u64 = 4 << 20;

/// A bucket's buffer while the records are dealt: the budget shared out
/// between the buckets, never under 64 KiB a write nor over 1 MiB, beyond
/// which a write is no faster. The unit tests' a few records long, so that
/// they flush, and write records longer than one, as a large file's do.
const CHUNK_MIN: usize = if cfg!(test) { 512 } else { 64 << 10 };
const CHUNK_MAX: usize = 1 << 20;

/// The most buckets the records are dealt into, so that the buffers
/// together hold at most [`BUDGET_MAX`], 16 MiB, whatever the file: 256 of
/// [`CHUNK_MIN`] each, or the budget shared out between fewer. Past them
/// -- 4 GiB of live records in a collection at the most budget, 1 GiB at
/// the least -- the records are walked by id. Where the buffers took
/// [`CHUNK_MIN`] each past 256 buckets, the deal held 1/256 of the live
/// bytes: 400 MiB for 100 GB of documents of 100 KB. The unit tests' a
/// thousand, so that buckets of a record each are dealt.
const BUCKETS_MOST: usize = if cfg!(test) {
    1024
} else {
    BUDGET_MAX as usize / CHUNK_MIN
};

/// What a bucket is read back through, a read at a time: the unit tests'
/// a few records long, so that frames cross from one read to the next.
const READ: usize = if cfg!(test) { 4096 } else { 1 << 20 };

/// What a scratch file asks of the volume beyond the collection's live
/// bytes twice -- the scratch, and the data record still to be written of
/// them -- for the records after it and the writes the database lands
/// meanwhile.
const MARGIN: u64 = 64 << 20;

/// The longest head a frame has: its op and two varints of ten bytes.
const HEAD_MAX: usize = 21;

/// A place no record is put at: a slot of an id no live record has, or one
/// a record was put at already.
const TAKEN: u32 = u32::MAX;

/// How a compact copies a store's live records into an out that asks for
/// them in turn ([`ImageOut::in_turn`]): the scratch file's name, the
/// budget of memory, and the scratch file's step (`fs::past_step`).
pub struct InTurn {
    temp: PathBuf,
    budget: usize,
    step: u64,
}

impl InTurn {
    /// What an out writing a compact's file asks of a store's records,
    /// about `live` bytes of a file `file` bytes long: in turn from [`FROM`]
    /// where the file is past the room the page cache has for it, or the
    /// process waits for the disk ([`budget`]), the scratch file made at
    /// `temp`'s name -- made only then, so an out that asks costs a compact
    /// that does not copy in turn no allocation.
    pub fn at(file: u64, live: u64, temp: impl FnOnce() -> PathBuf) -> Option<InTurn> {
        let (budget, room) = match ASSUMED.with(|a| a.get()) {
            Some(assumed) => (assumed?, crate::cache::room()),
            None if live < FROM as u64 => return None,
            None => {
                let room = crate::cache::room();
                // Asked only where it decides: a file past the room is
                // past it whatever the faults.
                let faulting = room.is_some_and(|r| file <= r) && crate::cache::faulting();
                (budget(file, room, faulting)?, room)
            }
        };
        Some(InTurn {
            temp: temp(),
            budget,
            step: crate::fs::past_step(room),
        })
    }
}

/// The memory a compact of a file `file` bytes long copies a store's live
/// records in, reading the file in turn: a quarter of `room`, the bytes the
/// page cache could hold of it (`cache::room`), between 4 and 16 MiB, where
/// the file is past that room or the process waits for the disk
/// (`cache::faulting`) -- the rule the side file is written past the cache
/// by (`Beside::past`) -- and `None`, the records walked by id, where it
/// fits or the room is not known: read from memory, by id costs nothing a
/// sort would save.
pub fn budget(file: u64, room: Option<u64>, faulting: bool) -> Option<usize> {
    match room {
        Some(r) if file > r || faulting => Some((r / 4).clamp(BUDGET_MIN, BUDGET_MAX) as usize),
        _ => None,
    }
}

thread_local! {
    /// The budget a test has a compact on this thread take, whatever the
    /// room and the live bytes: `Some(None)` by id ([`assume`]).
    static ASSUMED: Cell<Option<Option<usize>>> = const { Cell::new(None) };
    /// The stores this thread copied in turn: through a scratch file, and in
    /// memory ([`copied`]).
    static COPIED: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
    /// The stores this thread walked by id once their scratch file failed
    /// ([`fell_back`]).
    static FELL_BACK: Cell<u32> = const { Cell::new(0) };
    /// What a test has the next scratch file of this thread do wrong
    /// ([`break_scratch`]).
    static BROKEN: Cell<Option<Broken>> = const { Cell::new(None) };
}

/// Has a compact on this thread copy in turn, in `Some(budget)` bytes, or
/// by id (`Some(None)`), whatever the room and the live bytes; `None` goes
/// back to the room's rule: the tests' (`tests/in_turn.rs`), as
/// [`crate::cache::assume`] is.
#[doc(hidden)]
pub fn assume(budget: Option<Option<usize>>) {
    ASSUMED.with(|a| a.set(budget));
}

/// How many stores this thread has copied in turn so far: through a
/// scratch file, and in memory alone.
#[doc(hidden)]
pub fn copied() -> (u32, u32) {
    COPIED.with(|c| c.get())
}

/// How many stores this thread has walked by id once their scratch file
/// failed before a byte of the new file was written ([`walked_by_id`]).
#[doc(hidden)]
pub fn fell_back() -> u32 {
    FELL_BACK.with(|f| f.get())
}

/// What a test has the next scratch file on this thread do wrong: refuse
/// its first write, as a full disk would, or hand back the `k`th bucket
/// other than it was written, as a failing disk would -- its last byte, or
/// its first frame's length the longest a varint says.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Broken {
    Write,
    Byte(usize),
    Length(usize),
}

/// Has the next scratch file on this thread do `what` wrong ([`Broken`]).
#[doc(hidden)]
pub fn break_scratch(what: Option<Broken>) {
    BROKEN.with(|b| b.set(what));
}

/// A run of live records by id: the first id, where its frames start in
/// the data record's body -- and in the scratch file, laid out alike --
/// their bytes and how many.
#[derive(Debug)]
struct Bucket {
    first: DocId,
    at: u64,
    len: u64,
    n: usize,
}

/// Where each live record of a bucket goes in it, by id: a slot of the id
/// index's dense span each, and the sparse ids', ascending.
struct Places {
    first: DocId,
    dense: Vec<u32>,
    sparse: Vec<(DocId, u32)>,
}

impl Places {
    /// The place of `id`'s record, `TAKEN` once given.
    fn take(&mut self, id: DocId) -> Option<u32> {
        let slot = match id.checked_sub(self.first) {
            Some(i) if i < self.dense.len() as u64 => &mut self.dense[i as usize],
            _ => {
                let i = self.sparse.binary_search_by_key(&id, |e| e.0).ok()?;
                &mut self.sparse[i].1
            }
        };
        let at = std::mem::replace(slot, TAKEN);
        (at != TAKEN).then_some(at)
    }
}

/// The frame at `*p` of `bytes`: its op, id, where its payload starts and
/// its length, `*p` moved past it; `None` where it does not read.
fn frame_at(bytes: &[u8], p: &mut usize) -> Option<(u8, DocId, usize, usize)> {
    let op = *bytes.get(*p)?;
    *p += 1;
    let id = get_uvarint(bytes, p).ok()?;
    let len = get_uvarint(bytes, p).ok()?;
    let from = *p;
    let len = usize::try_from(len)
        .ok()
        .filter(|&l| l <= bytes.len() - from)?;
    *p = from + len;
    Some((op, id, from, len))
}

/// A put's head, as [`Store::write_live`] frames it.
fn head_into(head: &mut Vec<u8>, id: DocId, len: usize) {
    head.clear();
    head.push(OP_PUT);
    put_uvarint(head, id);
    put_uvarint(head, len as u64);
}

impl Store {
    /// [`Self::compact_live`] reading the file in turn, where `out` asks it
    /// ([`ImageOut::in_turn`]): `false`, nothing written, where the records
    /// are to be walked by id -- not mapped, under [`FROM`], not asked, past
    /// [`BUCKETS_MOST`] buckets, no room on the volume, a scratch file that
    /// failed before the new file was written, or a walk that did not meet
    /// every live record as the index has it, which the walk by id copies
    /// as it always did.
    pub(super) fn wrote_in_turn(&self, out: &mut dyn ImageOut) -> Result<bool> {
        let Some((base, _)) = &self.base else {
            return Ok(false);
        };
        // The live frames to the byte, kept as writes land: the room check
        // and the buckets' budget need no walk of the index for it.
        let live = self.live_bytes() as u64;
        let Some(t) = out.in_turn((**base).as_ref().len() as u64, live) else {
            return Ok(false);
        };
        let Some(buckets) = self.buckets(t.budget, BUCKETS_MOST) else {
            return Ok(false);
        };
        // The sparse ids sorted once, as `IdIndex::iter` sorts them, each
        // bucket given its run of them.
        let mut sparse: Vec<(DocId, u32)> = (self.index.under.iter())
            .chain(&self.index.sparse)
            .map(|(&id, l)| (id, l.len))
            .collect();
        sparse.sort_unstable_by_key(|e| e.0);
        let done = match buckets.len() {
            0 => return Ok(false),
            1 => self.in_memory(&buckets[0], &sparse, out)?,
            _ => self.through_scratch(&t, &buckets, &sparse, out)?,
        };
        if done {
            COPIED.with(|c| {
                let (s, m) = c.get();
                c.set(match buckets.len() {
                    1 => (s, m + 1),
                    _ => (s + 1, m),
                })
            });
        }
        Ok(done)
    }

    /// The live records cut into buckets of ids, each at most `cap` bytes
    /// of frames -- or one record, where it is longer -- laid out as the
    /// data record lays them: in id order, each frame where the one before
    /// it ends ([`after`], as `live_len` counts them). `None` once they are
    /// more than `most`.
    fn buckets(&self, cap: usize, most: usize) -> Option<Vec<Bucket>> {
        let mut all = Vec::new();
        let mut b = Bucket {
            first: 0,
            at: 0,
            len: 0,
            n: 0,
        };
        for (id, l) in self.index.walk(0) {
            let framed = after(0, id, l.len as u64) + l.len as u64;
            if b.n > 0 && b.len + framed > cap as u64 {
                if all.len() + 1 >= most {
                    return None;
                }
                let at = b.at + b.len;
                let next = Bucket {
                    first: id,
                    at,
                    len: 0,
                    n: 0,
                };
                all.push(std::mem::replace(&mut b, next));
            }
            if b.n == 0 {
                b.first = id;
            }
            b.len += framed;
            b.n += 1;
        }
        if b.n > 0 {
            all.push(b);
        }
        Some(all)
    }

    /// Where each live record of ids `first` to `end` (not included) goes
    /// in their bucket, by id, from 0: `sparse` every sparse id and its
    /// length, ascending, of which the bucket's run is found by halving.
    fn places(&self, first: DocId, end: DocId, sparse: &[(DocId, u32)]) -> Places {
        let (dense, floor) = (&self.index.dense, self.index.floor);
        let slot = |id: DocId| id.saturating_sub(floor).min(dense.len() as u64) as usize;
        let (lo, hi) = (slot(first), slot(end));
        let mut at = 0u64;
        let mut placed = |id: DocId, len: u32| {
            let here = at;
            at += after(0, id, len as u64) + len as u64;
            here as u32
        };
        // In id order: the sparse ids below the floor, the dense slots,
        // then those past the end.
        let from = sparse.partition_point(|e| e.0 < first);
        let to = sparse.partition_point(|e| e.0 < end);
        let below = sparse.partition_point(|e| e.0 < floor).clamp(from, to);
        let mut s: Vec<(DocId, u32)> = Vec::with_capacity(to - from);
        for &(id, len) in &sparse[from..below] {
            s.push((id, placed(id, len)));
        }
        let mut d = Vec::with_capacity(hi - lo);
        for (i, l) in dense[lo..hi].iter().enumerate() {
            d.push(match l.is_empty() {
                true => TAKEN,
                false => placed(floor + (lo + i) as u64, l.len),
            });
        }
        for &(id, len) in &sparse[below..to] {
            s.push((id, placed(id, len)));
        }
        Places {
            first: floor + lo as u64,
            dense: d,
            sparse: s,
        }
    }

    /// Each live record in the order it stands -- the mapped stretches by
    /// their place in the file, then the segments in turn -- its id and its
    /// payload: each frame read, and kept where the id index points at it.
    /// `false` where the walk did not meet every live record: a frame that
    /// does not read, or a record outside the stretches and the segments --
    /// a pinned store's, which keeps no stretch.
    fn each_standing(&self, mut f: impl FnMut(DocId, &[u8]) -> Result<()>) -> Result<bool> {
        let mut met = 0usize;
        if let Some((base, stretches)) = &self.base {
            let file: &[u8] = (**base).as_ref();
            for &(at, len) in stretches {
                let Some(bytes) = file.get(at as usize..(at + len) as usize) else {
                    return Ok(false);
                };
                let mut p = 0;
                while p < bytes.len() {
                    let Some((op, id, from, len)) = frame_at(bytes, &mut p) else {
                        return Ok(false);
                    };
                    let here = Loc::mapped(at + from as u64, len as u32);
                    if op == OP_PUT && self.index.get(id) == Some(here) {
                        f(id, &bytes[from..from + len])?;
                        met += 1;
                    }
                }
            }
        }
        for (s, seg) in self.segments.iter().enumerate() {
            let bytes = seg.data.as_slice();
            let mut p = 0;
            while p < bytes.len() {
                let Some((op, id, from, len)) = frame_at(bytes, &mut p) else {
                    return Ok(false);
                };
                let here = Loc {
                    seg: s as u32,
                    off: from as u32,
                    len: len as u32,
                };
                if op == OP_PUT && self.index.get(id) == Some(here) {
                    f(id, &bytes[from..from + len])?;
                    met += 1;
                }
            }
        }
        Ok(met == self.index.len())
    }

    /// The live records of one bucket put at their places in memory as
    /// they are read, and written out: no scratch file.
    fn in_memory(
        &self,
        b: &Bucket,
        sparse: &[(DocId, u32)],
        out: &mut dyn ImageOut,
    ) -> Result<bool> {
        let mut dest = vec![0u8; b.len as usize];
        let mut places = self.places(b.first, DocId::MAX, sparse);
        let mut astray = false;
        let mut head = Vec::with_capacity(HEAD_MAX);
        let met = self.each_standing(|id, payload| {
            head_into(&mut head, id, payload.len());
            let (h, n) = (head.len(), payload.len());
            let fits = self.index.get(id).is_some_and(|l| l.len as usize == n);
            match places.take(id).map(|at| at as usize) {
                Some(at) if fits && at + h + n <= dest.len() => {
                    dest[at..at + h].copy_from_slice(&head);
                    dest[at + h..at + h + n].copy_from_slice(payload);
                }
                _ => astray = true,
            }
            Ok(())
        })?;
        if !met || astray {
            return Ok(false);
        }
        out.write(&dest)?;
        Ok(true)
    }

    /// The live records dealt into the buckets' stretches of a scratch file
    /// as they are read, then each bucket read back in turn, its records
    /// put at their places, and written out. A scratch file that cannot be
    /// made or finds no room on the volume, or that fails before the first
    /// bucket is written out -- a write or a read refused, a bucket that
    /// does not read back as it was written -- leaves the records to the
    /// walk by id, nothing of the new file written ([`walked_by_id`]); one
    /// that fails after fails the copy, as a write of the new file does.
    fn through_scratch(
        &self,
        t: &InTurn,
        buckets: &[Bucket],
        sparse: &[(DocId, u32)],
        out: &mut dyn ImageOut,
    ) -> Result<bool> {
        let total = buckets.last().map_or(0, |b| b.at + b.len);
        let Some(mut scratch) = Scratch::create(&t.temp, 2 * total + MARGIN, t.step) else {
            return Ok(false);
        };
        // The new file's last step on disk and let go of before the deal
        // fills the scratch file's two: a collection's read back before
        // this one left up to a step of it dirty, three sixteenths of the
        // room together, where a step dirty and one on its way are the
        // bound. Its error is the new file's, which fails the copy.
        out.settle()?;
        let broken = BROKEN.with(|b| b.take());
        let sums = match self.deal(t, buckets, &mut scratch, broken) {
            Ok(Some(sums)) => sums,
            Ok(None) => return Ok(false),
            Err(e) => return Ok(walked_by_id(&t.temp, &e)),
        };
        let most = buckets.iter().map(|b| b.len).max().unwrap_or(0);
        let mut dest = vec![0u8; most as usize];
        let mut read = Vec::with_capacity(READ);
        for (k, b) in buckets.iter().enumerate() {
            let end = buckets.get(k + 1).map_or(DocId::MAX, |n| n.first);
            let dest = &mut dest[..b.len as usize];
            let back = match broken {
                Some(Broken::Byte(at)) if at == k => flip_last(&mut scratch, b),
                Some(Broken::Length(at)) if at == k => longest_length(&mut scratch, b),
                _ => Ok(()),
            }
            .and_then(|()| {
                let places = self.places(b.first, end, sparse);
                self.read_back(&scratch, b, places, &mut read, dest, sums[k])
            });
            match back {
                Ok(()) => {}
                // Nothing of the new file written yet.
                Err(e) if k == 0 => return Ok(walked_by_id(&t.temp, &e)),
                Err(e) => return Err(e),
            }
            scratch.let_go(b.at, b.len);
            out.write(dest)?;
        }
        Ok(true)
    }

    /// The live records dealt into the buckets' stretches of `scratch`, a
    /// buffer a bucket, summed as they go: each bucket's sum, or `None`
    /// where a record went astray or the walk missed one, the scratch file
    /// then left as it is.
    fn deal(
        &self,
        t: &InTurn,
        buckets: &[Bucket],
        scratch: &mut Scratch,
        broken: Option<Broken>,
    ) -> Result<Option<Vec<u32>>> {
        let chunk = (t.budget / buckets.len()).clamp(CHUNK_MIN, CHUNK_MAX);
        let mut bufs: Vec<Vec<u8>> = buckets.iter().map(|_| Vec::with_capacity(chunk)).collect();
        // What each bucket has written, and the sum of it.
        let mut put = vec![0u64; buckets.len()];
        let mut sums = vec![0u32; buckets.len()];
        let mut astray = false;
        let mut head = Vec::with_capacity(HEAD_MAX);
        let write = |scratch: &mut Scratch, at: u64, bytes: &[u8]| {
            if broken == Some(Broken::Write) {
                return Err(Error::Io("a test's scratch file refused a write".into()));
            }
            scratch.write_at(at, bytes)
        };
        let met = self.each_standing(|id, payload| {
            let k = buckets.partition_point(|b| b.first <= id).wrapping_sub(1);
            let Some(b) = buckets.get(k) else {
                astray = true;
                return Ok(());
            };
            head_into(&mut head, id, payload.len());
            let framed = head.len() + payload.len();
            let buf = &mut bufs[k];
            if put[k] + (buf.len() + framed) as u64 > b.len {
                astray = true;
                return Ok(());
            }
            sums[k] = crc32c(crc32c(sums[k], &head), payload);
            if buf.len() + framed > chunk && !buf.is_empty() {
                write(scratch, b.at + put[k], buf)?;
                put[k] += buf.len() as u64;
                buf.clear();
            }
            match framed > chunk {
                // Longer than a buffer: written as it is.
                true => {
                    write(scratch, b.at + put[k], &head)?;
                    write(scratch, b.at + put[k] + head.len() as u64, payload)?;
                    put[k] += framed as u64;
                }
                false => {
                    buf.extend_from_slice(&head);
                    buf.extend_from_slice(payload);
                }
            }
            Ok(())
        })?;
        for (k, b) in buckets.iter().enumerate() {
            if !bufs[k].is_empty() {
                write(scratch, b.at + put[k], &bufs[k])?;
                put[k] += bufs[k].len() as u64;
            }
        }
        let whole = met && !astray && buckets.iter().zip(&put).all(|(b, &p)| p == b.len);
        // Its last step on disk and let go of before the read back fills the
        // side file's first: left dirty, the two held two steps of a cgroup
        // that cannot reclaim them. One not whole is closed, never read.
        if whole {
            scratch.settle()?;
        }
        Ok(whole.then_some(sums))
    }

    /// Bucket `b`'s stretch of `scratch` read back through `read`, each
    /// frame put at its place in `dest`, and held to the sum it was dealt
    /// with.
    fn read_back(
        &self,
        scratch: &Scratch,
        b: &Bucket,
        mut places: Places,
        read: &mut Vec<u8>,
        dest: &mut [u8],
        sum: u32,
    ) -> Result<()> {
        read.clear();
        let mut region = Region {
            scratch,
            at: b.at,
            end: b.at + b.len,
            buf: std::mem::take(read),
            pos: 0,
            sum: 0,
        };
        let mut done = Ok(());
        for _ in 0..b.n {
            done = region.frame(self, &mut places, dest);
            if done.is_err() {
                break;
            }
        }
        let whole = region.read_whole() && region.sum == sum;
        *read = region.buf;
        done?;
        match whole {
            true => Ok(()),
            false => Err(unlike_written()),
        }
    }
}

/// A test's flipped byte: the last of bucket `b`, a payload's, one only
/// its sum tells.
fn flip_last(scratch: &mut Scratch, b: &Bucket) -> Result<()> {
    let at = b.at + b.len - 1;
    let mut byte = [0u8];
    scratch.read_at(at, &mut byte)?;
    scratch.write_at(at, &[!byte[0]])
}

/// A test's length read back: the first frame's of bucket `b`, `u64::MAX`
/// less one, written over its own and the payload's first bytes.
fn longest_length(scratch: &mut Scratch, b: &Bucket) -> Result<()> {
    let mut head = [0u8; HEAD_MAX];
    scratch.read_at(b.at, &mut head)?;
    let mut p = 1;
    get_uvarint(&head, &mut p)?;
    let mut long = head[..p].to_vec();
    put_uvarint(&mut long, u64::MAX - 1);
    scratch.write_at(b.at, &long)
}

/// The records of a store whose scratch file failed with `e` before a byte
/// of the new file was written, left to the walk by id: `false`, said on
/// stderr where the words are news -- a line a scratch file at `temp` and
/// its failure, so a disk that fails every compact says it once, and each
/// database's once -- as a server's line is written (fennec-http's `log!`),
/// never panicking where stderr is gone. Nothing of the database's own
/// file was touched: the scratch file, unlinked, gives its room back as it
/// closes, before the walk writes a record. A room the volume was asked for
/// is a snapshot, which a node compacting several tenants at once passes
/// for each; the first scratch write the disk then refuses ends here.
#[cold]
fn walked_by_id(temp: &std::path::Path, e: &Error) -> bool {
    use std::io::Write;
    static SAID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    FELL_BACK.with(|f| f.set(f.get() + 1));
    let line = format!(
        "{}: a compact's scratch file failed before its new file was written ({e}); the \
         collection's records were copied by id instead",
        temp.display()
    );
    let mut said = SAID.lock().unwrap_or_else(|p| p.into_inner());
    if said.as_deref() != Some(line.as_str()) {
        let _ = std::io::stderr().write_all(format!("{line}\n").as_bytes());
        *said = Some(line);
    }
    false
}

/// A bucket's stretch of the scratch file read back a buffer at a time,
/// summed as it is read.
struct Region<'a> {
    scratch: &'a Scratch,
    /// The next byte of the file to read into `buf`, and the stretch's end.
    at: u64,
    end: u64,
    buf: Vec<u8>,
    /// The bytes of `buf` taken.
    pos: usize,
    sum: u32,
}

impl Region<'_> {
    /// At least `n` bytes held, or all that is left of the stretch.
    fn fill(&mut self, n: usize) -> Result<()> {
        if self.buf.len() - self.pos >= n {
            return Ok(());
        }
        self.buf.drain(..self.pos);
        self.pos = 0;
        let have = self.buf.len();
        let more = ((self.end - self.at) as usize).min(READ - have);
        self.buf.resize(have + more, 0);
        self.scratch.read_at(self.at, &mut self.buf[have..])?;
        self.at += more as u64;
        Ok(())
    }

    /// The next frame put at its place in `dest`: refused where its record
    /// is not one of the bucket's live ones, as the id index has it, or
    /// was put already.
    fn frame(&mut self, st: &Store, places: &mut Places, dest: &mut [u8]) -> Result<()> {
        self.fill(HEAD_MAX)?;
        let held = &self.buf[self.pos..];
        let mut p = 1;
        let head = held.first().and_then(|&op| {
            let id = get_uvarint(held, &mut p).ok()?;
            let len = usize::try_from(get_uvarint(held, &mut p).ok()?).ok()?;
            Some((op, id, len))
        });
        let Some((op, id, len)) = head else {
            return Err(unlike_written());
        };
        let at = match places.take(id) {
            Some(at) if op == OP_PUT => at as usize,
            _ => return Err(unlike_written()),
        };
        // The length held to the index's before it is added to: read off a
        // failing disk it may be any `u64`, and `at + p + len` overflowed
        // into a panic under the lock, which fails the database
        // (`compact_unwound`). Both under 2^32 then, the sum cannot.
        let fits = st.index.get(id).is_some_and(|l| l.len as usize == len)
            && after(0, id, len as u64) == p as u64;
        if !fits {
            return Err(unlike_written());
        }
        let end = at + p + len;
        if end > dest.len() {
            return Err(unlike_written());
        }
        dest[at..at + p].copy_from_slice(&held[..p]);
        self.sum = crc32c(self.sum, &held[..p]);
        self.pos += p;
        let payload = &mut dest[at + p..end];
        let n = payload.len().min(self.buf.len() - self.pos);
        payload[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        let rest = &mut payload[n..];
        if !rest.is_empty() {
            if rest.len() as u64 > self.end - self.at {
                return Err(unlike_written());
            }
            self.scratch.read_at(self.at, rest)?;
            self.at += rest.len() as u64;
        }
        self.sum = crc32c(self.sum, payload);
        Ok(())
    }

    /// Whether every byte of the stretch was taken.
    fn read_whole(&self) -> bool {
        self.at == self.end && self.pos == self.buf.len()
    }
}

/// A bucket the scratch file gave back otherwise than it was written: a
/// failing disk's, as a read of the new file's that fails is the compact's
/// end.
#[cold]
fn unlike_written() -> Error {
    Error::Io("a compact's scratch file read back otherwise than it was written".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ImageOut;
    use std::path::Path;

    /// The rule: in turn only where the walk by id would read the disk --
    /// the file past the room, or the process waiting for it where the
    /// room is known -- in a quarter of the room, 4 to 16 MiB.
    #[test]
    fn a_compact_reads_in_turn_only_past_the_room() {
        let mb = 1u64 << 20;
        assert_eq!(budget(10, Some(9), false), Some(4 << 20));
        assert_eq!(budget(10, Some(10), false), None);
        assert_eq!(budget(10, Some(11), true), Some(4 << 20));
        assert_eq!(budget(10, None, false), None);
        assert_eq!(budget(10, None, true), None);
        assert_eq!(budget(2048 * mb, Some(400 * mb), false), Some(16 << 20));
        assert_eq!(budget(2048 * mb, Some(40 * mb), false), Some(10 << 20));
    }

    /// An out asks for the records in turn only from [`FROM`] live bytes,
    /// past the room or with the process waiting for the disk, unless a test
    /// says otherwise.
    #[test]
    fn an_out_asks_in_turn_from_its_live_bytes_past_the_room() {
        let temp = Path::new("db.fennec.sorting");
        let from = FROM as u64;
        crate::cache::assume(Some(Some(1 << 20)));
        assert!(InTurn::at(2 << 20, from - 1, || temp.to_path_buf()).is_none());
        let t = InTurn::at(2 << 20, from, || temp.to_path_buf()).expect("past the room");
        assert_eq!((t.temp.as_path(), t.budget), (temp, 4 << 20));
        // The scratch file's step follows the same room.
        assert_eq!(t.step, crate::fs::past_step(Some(1 << 20)));
        crate::cache::assume(Some(Some(160 << 20)));
        let t = InTurn::at(200 << 20, from, || temp.to_path_buf()).expect("past the room");
        assert_eq!((t.budget, t.step), (16 << 20, 10 << 20));
        crate::cache::assume(Some(None));
        assert!(InTurn::at(2 << 20, from, || temp.to_path_buf()).is_none());
        assume(Some(Some(7)));
        assert_eq!(
            InTurn::at(1, 1, || temp.to_path_buf()).map(|t| t.budget),
            Some(7)
        );
        assume(Some(None));
        crate::cache::assume(Some(Some(1)));
        assert!(InTurn::at(2 << 20, from, || temp.to_path_buf()).is_none());
        assume(None);
        crate::cache::assume(None);
    }

    /// The scratch file's step in the tests: a few records, so that every
    /// copy through one goes to disk in steps, a record across two of them
    /// and the one longer than a bucket across many.
    const STEP: u64 = 1_000;

    /// An out that asks for the records in turn, through a scratch file at
    /// `temp` written [`STEP`] at a time, in `budget` bytes; `settled` the
    /// bytes it held and the scratch files settled so far
    /// (`fs::scratch_settled`) as each settle of its own came.
    struct Asking {
        bytes: Vec<u8>,
        temp: PathBuf,
        budget: usize,
        settled: Vec<(usize, u32)>,
    }

    impl ImageOut for Asking {
        fn write(&mut self, bytes: &[u8]) -> Result<()> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
        fn at(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn patch(&mut self, _: u64, _: &[u8]) -> Result<()> {
            unreachable!("a store's records are not patched")
        }
        fn in_turn(&self, _file: u64, _live: u64) -> Option<InTurn> {
            Some(InTurn {
                temp: self.temp.clone(),
                budget: self.budget,
                step: STEP,
            })
        }
        fn settle(&mut self) -> Result<()> {
            let scratch = crate::fs::scratch_settled();
            self.settled.push((self.bytes.len(), scratch));
            Ok(())
        }
    }

    /// A store reading from two stretches of a mapped file, a record of
    /// another collection between them, and two segments in memory, its
    /// records written in an order no id order has: ids rewritten within a
    /// stretch, from one to the next and into memory, deleted, deleted and
    /// written again, four past the dense array, written out of their
    /// order, and one longer than a bucket. `payload` makes each record's
    /// bytes.
    fn scattered() -> Store {
        let payload = |id: u64, v: u64| {
            let n = 20 + (id * 37 + v * 11) % 700;
            let mut p = format!("<{id}:{v}>").into_bytes();
            p.resize(n as usize, b'a' + (id % 26) as u8);
            p
        };
        let mut first = Store::new();
        for id in 1..=300u64 {
            first.append(OP_PUT, id, &payload(id, 0));
        }
        for id in (1..=300u64).step_by(7) {
            first.append(OP_PUT, id, &payload(id, 1));
        }
        for id in (2..=300u64).step_by(13) {
            first.append(OP_DEL, id, &[]);
        }
        let mut second = Store::new();
        for id in (3..=300u64).rev().step_by(5) {
            second.append(OP_PUT, id, &payload(id, 2));
        }
        second.append(OP_PUT, 2, &payload(2, 3));
        for far in [1 << 41, (1 << 40) + 9, 1 << 40, (1 << 40) + 3] {
            second.append(OP_PUT, far, &payload(far, 4));
        }
        second.append(OP_PUT, 150, &vec![b'z'; 70_000]);
        let (a, b) = (first.image(), second.image());
        let mut file = vec![7u8; 4];
        file.extend_from_slice(&a);
        file.extend_from_slice(&[9u8; 33]);
        let b_at = file.len() as u64;
        file.extend_from_slice(&b);
        let base: Base = std::sync::Arc::new(file);
        let mut st = Store::new();
        st.replay_mapped(&base, 4, a.len() as u64, &mut |_| {})
            .unwrap();
        st.replay_mapped(&base, b_at, b.len() as u64, &mut |_| {})
            .unwrap();
        for id in (4..=300u64).step_by(9) {
            st.append(OP_PUT, id, &payload(id, 5));
        }
        st.append(OP_DEL, 2, &[]);
        st.segments.last_mut().unwrap().sealed = true;
        st.segments.push(Segment::default());
        for id in (6..=300u64).step_by(17) {
            st.append(OP_PUT, id, &payload(id, 6));
        }
        st.append(OP_PUT, 2, &payload(2, 7));
        st
    }

    /// A test's directory, removed as the test ends, failing or not.
    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A scratch file's name in a directory of the test's own.
    fn temp(tag: &str) -> (Dir, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fennec-in-turn-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.fennec.sorting");
        (Dir(dir), path)
    }

    /// The copy in turn writes the bytes the walk by id writes, through a
    /// scratch file in buckets of every size down to one record each, and in
    /// memory where one bucket holds them; the scratch file leaves no name,
    /// and each deal ends with it settled -- on disk, its pages let go of
    /// (`fs::Scratch::settle`) -- before a bucket is read back.
    #[test]
    fn a_copy_in_turn_writes_what_the_walk_by_id_writes() {
        let st = scattered();
        assert_eq!(st.segments.len(), 2);
        assert_eq!(st.base.as_ref().unwrap().1.len(), 2);
        let mut by_id = Vec::new();
        st.write_live(&mut by_id).unwrap();
        assert_eq!(by_id.len(), st.live_len());
        let (_dir, path) = temp("same");
        let before = copied();
        let settled = crate::fs::scratch_settled();
        for budget in [1, 700, 4_000, 20_000, 1 << 20] {
            let mut out = Asking {
                bytes: Vec::new(),
                temp: path.clone(),
                budget,
                settled: Vec::new(),
            };
            assert!(st.wrote_in_turn(&mut out).unwrap(), "{budget}");
            assert!(out.bytes == by_id, "budget {budget}");
            assert!(!path.exists());
        }
        let (s, m) = copied();
        assert_eq!((s - before.0, m - before.1), (4, 1));
        assert_eq!(crate::fs::scratch_settled() - settled, 4);
    }

    /// Two collections copied in turn into one new file, as a compact past
    /// the page cache copies each: the new file is settled before each deal
    /// -- the second after the first's read back wrote its bytes, before
    /// the second's scratch file is -- so its last step is not left dirty
    /// beside a scratch file's two.
    #[test]
    fn the_new_file_is_settled_before_each_deal() {
        let st = scattered();
        let mut by_id = Vec::new();
        st.write_live(&mut by_id).unwrap();
        let (_dir, path) = temp("two");
        let mut out = Asking {
            bytes: Vec::new(),
            temp: path.clone(),
            budget: 4_000,
            settled: Vec::new(),
        };
        let scratch = crate::fs::scratch_settled();
        assert!(st.wrote_in_turn(&mut out).unwrap());
        assert!(st.wrote_in_turn(&mut out).unwrap());
        assert_eq!(out.settled, [(0, scratch), (by_id.len(), scratch + 1)]);
        assert_eq!(crate::fs::scratch_settled(), scratch + 2);
        assert!(out.bytes == [&by_id[..], &by_id[..]].concat());
        // One bucket's copy, in memory, deals nothing and settles nothing.
        let mut out = Asking {
            bytes: Vec::new(),
            temp: path.clone(),
            budget: 1 << 20,
            settled: Vec::new(),
        };
        assert!(st.wrote_in_turn(&mut out).unwrap());
        assert!(out.settled.is_empty());
    }

    /// A scratch file that refuses a write, or reads the first bucket back
    /// otherwise than it was written -- a byte, or a length no record has,
    /// which summed before it is held to the index's overflows -- leaves
    /// the records to the walk by id, nothing of the new file written; one
    /// that reads a later bucket back so fails the copy, as a write of the
    /// new file does. None leaves its name.
    #[test]
    fn a_scratch_file_that_fails_before_the_new_file_leaves_the_walk_by_id() {
        let st = scattered();
        let (_dir, path) = temp("broken");
        let before = fell_back();
        let broken = [
            Broken::Write,
            Broken::Byte(0),
            Broken::Length(0),
            Broken::Byte(1),
            Broken::Length(1),
        ];
        for what in broken {
            break_scratch(Some(what));
            let mut out = Asking {
                bytes: Vec::new(),
                temp: path.clone(),
                budget: 4_000,
                settled: Vec::new(),
            };
            match what {
                Broken::Byte(1) | Broken::Length(1) => {
                    let e = st.wrote_in_turn(&mut out).unwrap_err();
                    assert!(matches!(e, Error::Io(_)), "{what:?}: {e}");
                    assert!(!out.bytes.is_empty(), "{what:?}: the first bucket written");
                }
                _ => {
                    assert!(!st.wrote_in_turn(&mut out).unwrap(), "{what:?}");
                    assert!(out.bytes.is_empty(), "{what:?}: written");
                }
            }
            assert!(!path.exists());
        }
        assert_eq!(fell_back() - before, 3);
    }

    /// `st`'s records laid in a mapped file, as an open that maps it reads
    /// them: one stretch, after 4 bytes of another record.
    fn mapped(st: &Store) -> Store {
        let image = st.image();
        let base: Base = std::sync::Arc::new([vec![7u8; 4], image.clone()].concat());
        let mut m = Store::new();
        m.replay_mapped(&base, 4, image.len() as u64, &mut |_| {})
            .unwrap();
        m
    }

    /// The copy in turn of `st` in buckets of `budget` bytes, held to the
    /// walk by id byte for byte.
    fn same_in_turn(st: &Store, budget: usize, path: &Path) {
        let mut by_id = Vec::new();
        st.write_live(&mut by_id).unwrap();
        let mut out = Asking {
            bytes: Vec::new(),
            temp: path.to_path_buf(),
            budget,
            settled: Vec::new(),
        };
        assert!(st.wrote_in_turn(&mut out).unwrap(), "{budget}");
        assert!(out.bytes == by_id, "budget {budget}");
    }

    /// Sparse ids -- each more than `MAX_DENSE_GAP` past the one before, so
    /// none joins the dense array -- spread over several buckets, written
    /// out of their order and again, each bucket given its run of them.
    #[test]
    fn sparse_ids_over_several_buckets_go_to_their_places() {
        let mut st = Store::new();
        for id in 1..=40u64 {
            st.append(OP_PUT, id, &vec![b'd'; 30 + id as usize]);
        }
        let far = |k: u64| 100_000 + k * 5_003;
        for k in (0..200u64).rev() {
            st.append(
                OP_PUT,
                far(k),
                &vec![b'a' + (k % 26) as u8; 20 + (k * 7 % 90) as usize],
            );
        }
        for k in (0..200u64).step_by(3) {
            st.append(OP_PUT, far(k), &[b'z'; 40]);
        }
        for k in (1..200u64).step_by(17) {
            st.append(OP_DEL, far(k), &[]);
        }
        let st = mapped(&st);
        assert!(st.index.sparse.len() > 150 && st.index.dense.len() == 40);
        let (_dir, path) = temp("sparse");
        for budget in [1, 900, 3_000] {
            let buckets = st.buckets(budget, BUCKETS_MOST).unwrap();
            let sparse = buckets.iter().filter(|b| b.first > 40).count();
            assert!(
                sparse >= 3,
                "budget {budget}: {sparse} buckets of sparse ids"
            );
            same_in_turn(&st, budget, &path);
        }
        assert!(!path.exists());
    }

    /// A store whose floor a cut raised, ids written again below it and
    /// others far past its end, copied in turn as the walk by id writes it:
    /// the ids below the floor first, each bucket given its run of them.
    #[test]
    fn ids_below_a_raised_floor_go_to_their_places() {
        let mut st = Store::new();
        for id in 1..=600u64 {
            let len = 20 + (id * 7 % 90) as usize;
            st.append(OP_PUT, id, &vec![b'a' + (id % 26) as u8; len]);
        }
        let mut st = mapped(&st);
        for id in 1..=400u64 {
            st.append(OP_DEL, id, &[]);
        }
        st.cut_ids();
        assert_eq!(st.index.floor, 401);
        for id in [5, 250, 120, 399] {
            st.append(OP_PUT, id, &[b'u'; 33]);
        }
        for far in [1 << 40, (1 << 40) + 5] {
            st.append(OP_PUT, far, &[b'f'; 50]);
        }
        assert_eq!((st.index.under.len(), st.index.sparse.len()), (4, 2));
        let (_dir, path) = temp("under");
        for budget in [1, 900, 3_000, 1 << 20] {
            same_in_turn(&st, budget, &path);
        }
        assert!(!path.exists());
    }

    /// Past [`BUCKETS_MOST`] buckets the records are walked by id, nothing
    /// written and no scratch file made: the buffers would hold more than
    /// their bound.
    #[test]
    fn past_the_most_buckets_the_records_are_walked_by_id() {
        let mut st = Store::new();
        for id in 1..=(BUCKETS_MOST as u64 + 10) {
            st.append(OP_PUT, id, b"a record");
        }
        let st = mapped(&st);
        assert!(st.buckets(1, BUCKETS_MOST).is_none());
        assert_eq!(
            st.buckets(1, BUCKETS_MOST + 10).unwrap().len(),
            BUCKETS_MOST + 10
        );
        // Two records of 11 or 12 framed bytes a bucket.
        assert_eq!(
            st.buckets(24, BUCKETS_MOST).unwrap().len(),
            BUCKETS_MOST / 2 + 5
        );
        let (_dir, path) = temp("most");
        let before = (copied(), fell_back());
        let mut out = Asking {
            bytes: Vec::new(),
            temp: path.clone(),
            budget: 1,
            settled: Vec::new(),
        };
        assert!(!st.wrote_in_turn(&mut out).unwrap());
        assert!(out.bytes.is_empty());
        assert_eq!((copied(), fell_back()), before);
        same_in_turn(&st, 24, &path);
    }

    /// A walk that misses a live record -- a pinned store's, which keeps no
    /// stretch of the mapped file -- leaves the records to the walk by id,
    /// nothing written, in memory or through a scratch file.
    #[test]
    fn a_walk_that_misses_a_record_leaves_the_walk_by_id() {
        let st = scattered().pinned();
        assert!(st.base.as_ref().unwrap().1.is_empty());
        let (_dir, path) = temp("pinned");
        let before = copied();
        for budget in [4_000, 1 << 20] {
            let mut out = Asking {
                bytes: Vec::new(),
                temp: path.clone(),
                budget,
                settled: Vec::new(),
            };
            assert!(!st.wrote_in_turn(&mut out).unwrap(), "{budget}");
            assert!(out.bytes.is_empty(), "{budget}");
        }
        assert_eq!(copied(), before);
        assert!(!path.exists());
    }

    /// Whatever stands at the scratch file's name -- a database a user
    /// named so, its lock beside it, or a link -- is left as it is, and the
    /// records are walked by id.
    #[cfg(unix)]
    #[test]
    fn what_stands_at_the_scratch_files_name_is_left_as_it_is() {
        let st = scattered();
        let (_dir, path) = temp("standing");
        let theirs = path.with_extension("theirs");
        std::fs::write(&path, b"a database of its own").unwrap();
        std::fs::write(crate::fs::beside(&path, "lock"), b"").unwrap();
        std::fs::write(&theirs, b"what a link leads to").unwrap();
        let before = copied();
        for standing in ["file", "link"] {
            if standing == "link" {
                assert_eq!(std::fs::read(&path).unwrap(), b"a database of its own");
                std::fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(&theirs, &path).unwrap();
            }
            let mut out = Asking {
                bytes: Vec::new(),
                temp: path.clone(),
                budget: 4_000,
                settled: Vec::new(),
            };
            assert!(!st.wrote_in_turn(&mut out).unwrap(), "{standing}");
            assert!(out.bytes.is_empty(), "{standing}");
            assert!(
                std::fs::symlink_metadata(&path).is_ok(),
                "{standing}: removed"
            );
        }
        assert_eq!(std::fs::read(&theirs).unwrap(), b"what a link leads to");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(copied(), before);
    }
}
