//! The browser binding -- **no wasm-bindgen**.
//!
//! Why the raw ABI: wasm-bindgen needs `wasm-bindgen-cli`/`wasm-pack` and an
//! npm chain, and it adds a fair amount of glue to the output. Everything
//! fennecdb needs in the browser is "take text, return text". The exported
//! surface is therefore plain C ABI, and the output of `cargo build --target
//! wasm32-unknown-unknown` runs directly through `WebAssembly.instantiate`.
//! The JS glue totals ~100 lines (`web/fennec.js`).
//!
//! ## Memory contract
//! Every returned buffer is `[u32 length (LE)][contents]`. Once the caller
//! has read the contents it must call `fennec_free(ptr, 4 + length)`.

use fennec_core::collate;
use fennec_core::engine::Kept;
use fennec_core::json;
use fennec_core::prelude::*;
use std::alloc::{alloc, dealloc, Layout};
use std::cell::UnsafeCell;
use std::sync::{Arc, Mutex};

thread_local! {
    static HANDLES: UnsafeCell<Vec<Option<Slot>>> = const { UnsafeCell::new(Vec::new()) };
}

/// The handles' table. The module runs on one thread and its exports never
/// call one another, so no two of them hold it at once; native tests run
/// each on a thread of its own, with a table of its own. Reached through a
/// pointer rather than a closure handed to `LocalKey::with` and a
/// `RefCell`'s borrow: generic over each export's closure, those were 15
/// copies of the access and the borrow, 2.7 KB of the module and 0.7
/// brotli, and handed
/// the closure as `dyn` the work moved into each caller with the glue
/// around it, 319 bytes more.
fn table() -> &'static mut Vec<Option<Slot>> {
    // SAFETY: as above -- nothing else holds the table while an export
    // runs, and an export lets go of it before it returns.
    unsafe { &mut *HANDLES.with(UnsafeCell::get) }
}

/// A handle's database, and the journal `fennec_journal` started for it. In
/// one slot rather than a second table: that table's code was 1.5 KB of the
/// module.
struct Slot {
    db: Database,
    journal: Option<Arc<Mutex<Journal>>>,
    /// The bytes the last `fennec_load` took (`fennec_loaded`).
    loaded: usize,
    /// An image `fennec_snapshot_chunks` began, its chunks yet to be taken.
    chunks: Chunks,
}

/// A snapshot's chunk: a mebibyte, written into and never grown.
const CHUNK: usize = 1 << 20;

/// An image taken [`CHUNK`] bytes at a time. Into one `Vec` it grew by
/// doubling, old and new side by side: a 12 MB image took the module's
/// memory, which WebAssembly never gives back, up by 37 MB. Made whole in
/// chunks, it stood beside the rows until the first was taken: 50 000
/// 128-dim rows at 73 MB went to 109 checkpointed. So the stores' own
/// bytes are held rather than copied ([`Kept`]) -- a segment written to
/// meanwhile is copied then, and the image is the database as it stood --
/// and each chunk is made as it is taken.
#[derive(Default)]
struct Chunks {
    parts: Vec<Part>,
    len: u64,
    /// The first part not taken whole, and how much of it has been.
    next: usize,
    head: usize,
}

enum Part {
    /// Written by the image itself: its records' heads, schemas, graphs.
    Own(Vec<u8>),
    Kept(Kept),
}

impl Part {
    fn bytes(&self) -> &[u8] {
        match self {
            Part::Own(v) => v,
            Part::Kept(k) => k.bytes(),
        }
    }
}

impl Chunks {
    /// The next chunk: [`CHUNK`] bytes, or what is left.
    fn take(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CHUNK);
        while out.len() < CHUNK {
            let Some(part) = self.parts.get_mut(self.next) else {
                break;
            };
            let rest = &part.bytes()[self.head..];
            let n = rest.len().min(CHUNK - out.len());
            out.extend_from_slice(&rest[..n]);
            self.head += n;
            if self.head == part.bytes().len() {
                // Let go of as soon as it is taken.
                *part = Part::Own(Vec::new());
                self.next += 1;
                self.head = 0;
            }
        }
        out
    }
}

impl fennec_core::engine::ImageOut for Chunks {
    fn write(&mut self, mut bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.len += bytes.len() as u64;
        while !bytes.is_empty() {
            if !matches!(self.parts.last(), Some(Part::Own(v)) if v.len() < CHUNK) {
                self.parts.push(Part::Own(Vec::with_capacity(CHUNK)));
            }
            let Some(Part::Own(part)) = self.parts.last_mut() else {
                break;
            };
            let n = (CHUNK - part.len()).min(bytes.len());
            part.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
        }
        Ok(())
    }

    fn write_kept(&mut self, kept: Kept) -> fennec_core::error::Result<()> {
        self.len += kept.len as u64;
        if kept.len > 0 {
            self.parts.push(Part::Kept(kept));
        }
        Ok(())
    }

    fn at(&self) -> u64 {
        self.len
    }

    fn patch(&mut self, at: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        // The counter's header, once the image's length is known: in the
        // image's own bytes, the first it wrote.
        let mut start = 0usize;
        for part in self.parts.iter_mut() {
            let len = part.bytes().len();
            let at = at as usize;
            if at < start + len {
                if let Part::Own(v) = part {
                    if let Some(dst) = v.get_mut(at - start..at - start + bytes.len()) {
                        dst.copy_from_slice(bytes);
                        return Ok(());
                    }
                }
                break;
            }
            start += len;
        }
        Err(Error::Corrupt(
            "an image patched outside its own bytes".into(),
        ))
    }
}

/// What a page's database wrote since the page last took it: the frames
/// appended, or a whole image when a `compact` rewrote it, followed by the
/// frames after that. Either is what a file holds, so the page appends the
/// first to what it stored and replaces what it stored with the second.
#[derive(Default)]
struct Journal {
    image: Option<Vec<u8>>,
    tail: Vec<u8>,
}

/// The sink a journaling database writes through. `Arc<Mutex>` rather than
/// `Rc<RefCell>` only because a sink has to be `Send`; there is one thread.
struct JournalSink(Arc<Mutex<Journal>>);

impl Sink for JournalSink {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let mut j = self.0.lock().unwrap_or_else(|e| e.into_inner());
        j.tail.extend_from_slice(bytes);
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> Result<()> {
        let mut j = self.0.lock().unwrap_or_else(|e| e.into_inner());
        j.image = Some(bytes.to_vec());
        j.tail.clear();
        Ok(())
    }
}

// ------------------------------------------------------------- memory

/// Allocates a buffer of `len` bytes. The JS side uses it to write input.
#[no_mangle]
pub extern "C" fn fennec_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::NonNull::dangling().as_ptr();
    }
    unsafe { alloc(Layout::from_size_align_unchecked(len, 1)) }
}

/// Frees a `fennec_alloc` buffer or a returned one.
///
/// # Safety
/// `ptr` must come from `fennec_alloc(len)`, or be a returned buffer with
/// `len` its whole length (`4 +` the one written at its front), and not be
/// freed twice.
#[no_mangle]
pub unsafe extern "C" fn fennec_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe { dealloc(ptr, Layout::from_size_align_unchecked(len, 1)) }
}

/// Produces a `[u32 len][payload]` buffer and gives up ownership.
fn boxed(payload: &[u8]) -> *mut u8 {
    let total = 4 + payload.len();
    let ptr = fennec_alloc(total);
    unsafe {
        let len_bytes = (payload.len() as u32).to_le_bytes();
        std::ptr::copy_nonoverlapping(len_bytes.as_ptr(), ptr, 4);
        std::ptr::copy_nonoverlapping(payload.as_ptr(), ptr.add(4), payload.len());
    }
    ptr
}

/// The text JavaScript handed over, read where it lies: the page's buffer
/// outlives the call. Copied out, as it was, a page of 200 768-dim vectors'
/// parameters were 3 MB more to write before the first was read, and took
/// 23.7 ms to read against 21.9.
unsafe fn str_from<'a>(ptr: *const u8, len: usize) -> std::borrow::Cow<'a, str> {
    if ptr.is_null() || len == 0 {
        return "".into();
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.into(),
        Err(_) => lossy(bytes).into(),
    }
}

/// `bytes` with each sequence that is not UTF-8 replaced by U+FFFD, as
/// `String::from_utf8_lossy` replaces them -- `Utf8Error` names the same
/// sequences -- through the check every text goes through anyway: the
/// standard library's walk was 1.1 KB of the module for text that
/// `TextEncoder`, which writes what JavaScript hands over, never makes.
#[cold]
fn lossy(mut bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    loop {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let (good, bad) = bytes.split_at(e.valid_up_to());
                // `valid_up_to` is where the text stopped being UTF-8.
                out.push_str(unsafe { std::str::from_utf8_unchecked(good) });
                out.push('\u{FFFD}');
                bytes = bad
                    .get(e.error_len().unwrap_or(bad.len())..)
                    .unwrap_or_default();
            }
        }
    }
}

/// The answer for a handle no database holds.
fn no_handle(handle: u32) -> String {
    json::error_to_string(&Error::NotFound(format!("handle {handle}")))
}

// --------------------------------------------------------- lifecycle

/// Opens a new in-memory database and returns a handle.
#[no_mangle]
pub extern "C" fn fennec_open() -> u32 {
    let h = table();
    h.push(Some(Slot {
        db: Database::new(),
        journal: None,
        loaded: 0,
        chunks: Chunks::default(),
    }));
    (h.len() - 1) as u32
}

/// Closes the database and releases its memory.
#[no_mangle]
pub extern "C" fn fennec_close(handle: u32) {
    if let Some(slot) = table().get_mut(handle as usize) {
        *slot = None;
    }
}

#[no_mangle]
pub extern "C" fn fennec_version() -> *mut u8 {
    boxed(fennec_core::VERSION.as_bytes())
}

/// `f` over the handle's slot, `None` when no database holds it.
fn with_slot<T>(handle: u32, f: impl FnOnce(&mut Slot) -> T) -> Option<T> {
    table().get_mut(handle as usize)?.as_mut().map(f)
}

fn with_db<T>(handle: u32, f: impl FnOnce(&mut Database) -> T) -> Option<T> {
    with_slot(handle, |s| f(&mut s.db))
}

// ------------------------------------------------------------- query

/// Runs FennecQL. `params` is a JSON array (it may be empty), and `vectors`
/// the parameters that are vectors, handed over as `f32`s rather than as
/// text (`with_vectors`); a client with none passes none. `now` is the
/// time, in milliseconds since the epoch, a read of a collection whose rows
/// expire (`@ttl`) is answered at: the module has no clock, and is handed
/// every time. A client from before it passes none -- `NaN` here -- and
/// such a read is refused rather than answered at some time it was not.
/// Returns: JSON (`{"kind":"rows"|"affected"|"ok"|"schemas"|"error", ...}`).
///
/// # Safety
/// `sql_ptr`/`params_ptr`/`vectors_ptr` must be valid and of the given length.
#[no_mangle]
pub unsafe extern "C" fn fennec_query(
    handle: u32,
    sql_ptr: *const u8,
    sql_len: usize,
    params_ptr: *const u8,
    params_len: usize,
    vectors_ptr: *const u8,
    vectors_len: usize,
    now: f64,
) -> *mut u8 {
    let sql = str_from(sql_ptr, sql_len);
    let params_src = str_from(params_ptr, params_len);
    let vectors = match vectors_ptr.is_null() || vectors_len == 0 {
        true => &[][..],
        false => std::slice::from_raw_parts(vectors_ptr, vectors_len),
    };

    let now = now.is_finite().then_some(now as i64);
    with_db(handle, |db| db.set_clock(now));
    let out = run(handle, &sql, &params_src, vectors);
    boxed(out.as_bytes())
}

/// Reads, runs and answers, through the code the native library answers
/// with as well (`fennec-abi`).
fn run(handle: u32, sql: &str, params_src: &str, vectors: &[u8]) -> String {
    let wide = json::wide::count();
    let mut p = match fennec_abi::prepare(sql, params_src, vectors) {
        Ok(p) => p,
        Err(e) => return json::error_to_string(&e),
    };
    // A note left by anything before is not these statements'.
    collate::take_missing();
    let res = with_db(handle, |db| {
        fennec_abi::exact(db, &mut p, sql, params_src, vectors)?;
        // Natively, where nothing is counted, every parameter is looked at.
        if json::wide::count() != wide || cfg!(not(target_arch = "wasm32")) {
            p.params.iter_mut().for_each(as_double);
        }
        fennec_abi::execute(db, &p)
    });
    match res {
        None => no_handle(handle),
        Some(r) => fennec_abi::answer(&r),
    }
}

// -------------------------------------------------------------- schema

/// Compares the database with a schema declared in code -- a description,
/// `fennec_core::declared` -- and brings it there as `mode` says: 0 plans,
/// 1 applies (the migrations not yet recorded, then what only adds, one
/// block). `now` is when a migration is recorded. A replica's schema is
/// its server's, which the server compares (`POST /_schema/plan`): the
/// module carries no comparison it would not apply.
/// Returns `{"kind":"schema", ...}` (`fennec_abi::Outcome`) or an error,
/// naming the collation data a migration needs as `fennec_query`'s do.
/// Modes 3 and 4 take no request and describe the database: 3 as a
/// description, the native library's `FENNEC_SCHEMA_DESCRIBE` and a server's
/// `GET /_schema`, 4 as the FennecQL that makes it, `?as=fennecql` -- what
/// fennec studio shows of a database in the page.
///
/// # Safety
/// `ptr` must be valid and `len` bytes long.
#[cfg(feature = "schema")]
#[no_mangle]
pub unsafe extern "C" fn fennec_schema(
    handle: u32,
    ptr: *const u8,
    len: usize,
    mode: u32,
    now: f64,
) -> *mut u8 {
    let request = str_from(ptr, len);
    let now = now.is_finite().then_some(now as i64);
    collate::take_missing();
    let out = with_db(handle, |db| {
        db.set_clock(now);
        match mode {
            0 | 1 => fennec_abi::schema(db, &request, mode == 1, now).map(|o| o.json()),
            3 | 4 => Ok(fennec_abi::describe(db, mode == 4)),
            _ => Err(Error::Query(
                "the schema modes are 0, a plan, 1, an apply, 3 and 4, the database described"
                    .into(),
            )),
        }
    });
    let out = match out {
        None => no_handle(handle),
        Some(Ok(o)) => o,
        Some(Err(e)) => fennec_abi::refused(&e, 0, None),
    };
    boxed(out.as_bytes())
}

// ------------------------------------------------------------- blocks

/// Opens (0), lands (1) or puts back (2) a block of writes over several
/// calls of `fennec_query`: `Fennec.batch`, which has each statement answered
/// with its own parameters and the writes land whole or not at all, as a
/// server's `/batch`. Within one text the module opens a block of its own
/// (`fennec_abi::execute`); this is for statements handed over one by one,
/// none of which another call comes between. A statement that fails in the
/// block leaves it to be put back. Returns `{"kind":"ok",..}` or an error.
#[no_mangle]
pub extern "C" fn fennec_block(handle: u32, op: u32) -> *mut u8 {
    let r = with_db(handle, |db| match op {
        0 => db.begin(),
        1 => db.commit(),
        _ => {
            db.rollback();
            Ok(())
        }
    });
    let out = match r {
        None => no_handle(handle),
        Some(Ok(())) => String::from("{\"kind\":\"ok\"}"),
        Some(Err(e)) => json::error_to_string(&e),
    };
    boxed(out.as_bytes())
}

// ----------------------------------------------------------- collation

/// The collation data: `{"chunks":[names],"loaded":mask}`, a chunk's bit its
/// place in the list. The module carries `latin`; the others are handed to
/// it (`fennec_add_chunk`) from `collate/<name>.bin` beside it -- and, after
/// them, `zones`, the time zones' table a `bucket` by a zone's name reads.
#[no_mangle]
pub extern "C" fn fennec_collation() -> *mut u8 {
    let mut s = String::from("{\"chunks\":[");
    for (i, name) in collate::CHUNKS.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json::escape_into(&mut s, name);
    }
    let mut loaded = collate::loaded();
    if fennec_core::tz::PARTIAL {
        s.push_str(",\"zones\"");
        loaded |= (fennec_core::tz::loaded() as u32) << collate::CHUNKS.len();
    }
    s.push_str(&format!("],\"loaded\":{loaded}}}"));
    boxed(s.as_bytes())
}

/// Hands the module a chunk of collation data, the bytes of its `.bin`.
/// Returns its place in `fennec_collation`'s list, or -1 for bytes that are
/// not one of this module's chunks -- another version's among them.
///
/// # Safety
/// `ptr` must be valid and `len` bytes long.
#[no_mangle]
pub unsafe extern "C" fn fennec_add_chunk(ptr: *const u8, len: usize) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    match collate::add_chunk(bytes) {
        Some(name) => collate::CHUNKS
            .iter()
            .position(|n| *n == name)
            .map_or(-1, |i| i as i32),
        None if fennec_core::tz::add_chunk(bytes) => collate::CHUNKS.len() as i32,
        None => -1,
    }
}

// ------------------------------------------------------------- changes

/// What has changed since `since`:
/// `{"seq":N,"horizon":M,"collections":["a","b"]}`.
///
/// When `collections` is **null**, the cursor fell behind the ring, or a
/// collection written since was dropped, and it cannot be known which
/// collection changed: the caller must treat everything as stale.
/// Collection granularity is enough for live queries -- re-running a local
/// query is already sub-millisecond, and incremental bookkeeping does not
/// pay for itself on that budget.
///
/// This is how a caller learns what a statement wrote, rather than the
/// answer of `fennec_query` carrying it: asked once after a burst of writes
/// rather than built into every answer, it costs a write nothing, and a
/// binding with no live query never asks. A block's writes reach the ring
/// as it lands, so one put back names nothing. A `fennec_load` empties the
/// ring and sets the counter to the image's, which may be the very number
/// the caller holds: a load is the caller's to note, as everything stale.
///
/// Why `since` is an `f64`: a JS number is already an `f64` and the change
/// counter stays exact up to 2^53. As an `i64` it would need a `BigInt`
/// conversion at the ABI boundary -- buying only a range never reached.
#[no_mangle]
pub extern "C" fn fennec_changes(handle: u32, since: f64) -> *mut u8 {
    let since = if since.is_finite() && since >= 0.0 {
        since as u64
    } else {
        0
    };
    let out = with_db(handle, |db| fennec_abi::changes(db, since))
        .unwrap_or_else(|| "{\"seq\":0,\"horizon\":0,\"collections\":null}".to_string());
    boxed(out.as_bytes())
}

/// Sets the entry count of the change ring.
#[no_mangle]
pub extern "C" fn fennec_set_change_capacity(handle: u32, n: u32) {
    with_db(handle, |db| db.set_change_capacity(n.max(1) as usize));
}

// --------------------------------------------------------- persistence

/// Begins the database's image, to be taken [`CHUNK`] bytes at a time by
/// `fennec_snapshot_chunk`, and returns how many: the page stores each and
/// lets it go before the next is made, so the image is never whole in the
/// module, nor in the page -- a Durable Object's storage takes it a piece
/// at a time anyway.
#[no_mangle]
pub extern "C" fn fennec_snapshot_chunks(handle: u32) -> u32 {
    with_slot(handle, |s| {
        let mut out = Chunks::default();
        let _ = s.db.snapshot_into(&mut out);
        let n = (out.len as usize).div_ceil(CHUNK);
        s.chunks = out;
        n as u32
    })
    .unwrap_or(0)
}

/// The next chunk `fennec_snapshot_chunks` made, let go of here; empty when
/// none is left.
#[no_mangle]
pub extern "C" fn fennec_snapshot_chunk(handle: u32) -> *mut u8 {
    let part = with_slot(handle, |s| s.chunks.take()).unwrap_or_default();
    boxed(&part)
}

/// Starts keeping the database's writes for `fennec_drain`, or with `off`
/// stops. Off until asked for: a page that never drains would hold every
/// write it made -- which is why a page that lets its file go stops it. What
/// was written before is not kept; the page stores a snapshot first.
#[no_mangle]
pub extern "C" fn fennec_journal(handle: u32, off: u32) {
    with_slot(handle, |s| {
        if off != 0 {
            s.db.set_sink(Box::new(fennec_core::engine::NullSink));
            s.journal = None;
            return;
        }
        let journal = Arc::new(Mutex::new(Journal::default()));
        s.db.set_sink(Box::new(JournalSink(Arc::clone(&journal))));
        s.journal = Some(journal);
    });
}

/// What the database wrote since the last drain, and forgets it: `[0]` and
/// the frames to append to what the page stored, or `[1]` and an image to
/// replace it with. Empty frames when nothing was written or no journal
/// was started.
#[no_mangle]
pub extern "C" fn fennec_drain(handle: u32) -> *mut u8 {
    let taken = with_slot(handle, |s| {
        let mut j = s
            .journal
            .as_ref()?
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Some((j.image.take(), std::mem::take(&mut j.tail)))
    })
    .flatten();
    let mut out = Vec::new();
    match taken {
        Some((Some(image), tail)) => {
            out.push(1);
            out.extend_from_slice(&image);
            out.extend_from_slice(&tail);
        }
        Some((None, tail)) => {
            out.push(0);
            out.extend_from_slice(&tail);
        }
        None => out.push(0),
    }
    boxed(&out)
}

/// Loads from a byte image: 0 when it did, 1 when the bytes are not one,
/// and bit 1 when its collated text needs collation data the module has not
/// been handed -- a bit a chunk from bit 2, a chunk's bit its place in
/// `fennec_collation`'s list. Its `@sorted` indexes were then built comparing
/// without it, so the database is emptied, to be loaded again once the
/// module has it.
///
/// # Safety
/// `ptr` must be valid and `len` bytes long.
#[no_mangle]
pub unsafe extern "C" fn fennec_load(handle: u32, ptr: *const u8, len: usize) -> i32 {
    if ptr.is_null() || len == 0 {
        return 1;
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    collate::take_missing();
    loaded(with_slot(handle, |s| {
        s.loaded = s.db.load(bytes).ok()?;
        Some(missing(s))
    }))
}

/// The collation data a load found missing, the database emptied when it
/// did: its `@sorted` indexes were built without it.
fn missing(s: &mut Slot) -> u32 {
    let missing = s.db.collation_missing() | collate::take_missing();
    if missing != 0 {
        let mut empty = Database::new();
        empty.set_change_capacity(s.db.change_capacity());
        if let Some(j) = &s.journal {
            empty.set_sink(Box::new(JournalSink(Arc::clone(j))));
        }
        s.db = empty;
    }
    missing
}

/// What `fennec_load` and `fennec_load_owned` answer, written once for both.
fn loaded(out: Option<Option<u32>>) -> i32 {
    match out.flatten() {
        Some(0) => 0,
        Some(missing) => (2 | missing << 2) as i32,
        None => 1,
    }
}

/// [`fennec_load`] over bytes it takes: an allocation `fennec_alloc` made
/// for `len`, which the database keeps and reads its documents from rather
/// than copy them out of it. The caller does not free it.
///
/// # Safety
/// `ptr` must come from `fennec_alloc(len)` and not be used after.
#[no_mangle]
pub unsafe extern "C" fn fennec_load_owned(handle: u32, ptr: *mut u8, len: usize) -> i32 {
    if ptr.is_null() || len == 0 {
        return 1;
    }
    let bytes: Vec<u8> = unsafe { Vec::from_raw_parts(ptr, len, len) };
    let base: fennec_core::store::Base = Arc::new(bytes);
    collate::take_missing();
    loaded(with_slot(handle, |s| {
        s.loaded = s.db.load_mapped(base).ok()?;
        Some(missing(s))
    }))
}

/// The bytes the last `fennec_load` took: all of them, or those before a
/// last record a crash cut short. A page's file is cut back there before
/// anything is appended to it: a write appended after the torn bytes is
/// read back as the rest of them, and the next load loses it.
#[no_mangle]
pub extern "C" fn fennec_loaded(handle: u32) -> usize {
    with_slot(handle, |s| s.loaded).unwrap_or(0)
}

/// Returns the collection statistics as JSON.
#[no_mangle]
pub extern "C" fn fennec_stats(handle: u32) -> *mut u8 {
    let out = with_db(handle, |db| {
        let mut s = String::from("[");
        for (i, st) in db.stats().iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"name\":");
            json::escape_into(&mut s, &st.name);
            s.push_str(&format!(
                ",\"documents\":{},\"bytes\":{},\"dead_bytes\":{},\"segments\":{},\"vector_indexes\":[",
                st.documents, st.bytes, st.dead_bytes, st.segments
            ));
            for (j, v) in st.vector_indexes.iter().enumerate() {
                if j > 0 {
                    s.push(',');
                }
                s.push_str("{\"field\":");
                json::escape_into(&mut s, &v.field);
                s.push_str(&format!(
                    ",\"count\":{},\"dim\":{},\"arena_bytes\":{},\"precision\":\"{}\"}}",
                    v.count,
                    v.dim,
                    v.arena_bytes,
                    if v.precision == fennec_core::value::VecPrec::F16 { "f16" } else { "f32" }
                ));
            }
            s.push_str("]}");
        }
        s.push(']');
        s
    })
    .unwrap_or_else(|| "[]".to_string());
    boxed(out.as_bytes())
}

/// A parameter is a JavaScript value, which `JSON.stringify` wrote: a
/// double from 1e16 up as the shortest digits reading back as it, 2^60 as
/// `1152921504606847000` -- an int no double holds, another number than the
/// one the page held, and as an int and a float compare by their true
/// values, one finding none of 2^60's rows. No JavaScript value is an int
/// no double holds, so such an int is read as the double it rounds to, the
/// int that double is. None rounds to 2^63: `JSON.stringify` writes 2^63
/// and past as digits past an `i64`, read as the float, and the double
/// below it, 2^63 - 1024, as `9223372036854775000`, which rounds back to
/// it; changing the value's kind there cost the module 126 bytes of drop
/// glue for none of the page's values. Looked for in the page
/// instead, before its `JSON.stringify` -- a walk of its values -- a `run`
/// putting 1 000 documents took 2.3% longer, and after it, a match for 16
/// digits in a row, 30%, every long fraction a match; here only a text
/// whose reading met an int past 2^53 is looked through
/// (`json::wide::count`).
fn as_double(v: &mut Value) {
    const PAST: f64 = 9_223_372_036_854_775_808.0;
    match v {
        Value::Int(i) => {
            let d = *i as f64;
            if d < PAST {
                *i = d as i64;
            }
        }
        Value::List(items) => items.iter_mut().for_each(as_double),
        Value::Object(members) => members.iter_mut().for_each(|(_, v)| as_double(v)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An int no double holds is the double JavaScript wrote it from; any
    /// other is itself, inside lists and objects too. `9223372036854775000`,
    /// what `JSON.stringify` writes of 2^63 - 1024, the greatest double short
    /// of 2^63, is that double.
    #[test]
    fn a_parameter_is_the_double_javascript_held() {
        let p60 = 1i64 << 60;
        let mut v = Value::List(vec![
            Value::Int(1_152_921_504_606_847_000),
            Value::Int(p60 + 1),
            Value::Int(p60),
            Value::Int((1 << 53) + 1),
            Value::Int((1 << 53) + 3),
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
            Value::Int(-1_152_921_504_606_847_000),
            Value::Object(vec![("a".into(), Value::Int(1_152_921_504_606_847_000))]),
            Value::Float(0.5),
            Value::Int(9_223_372_036_854_775_000),
        ]);
        as_double(&mut v);
        let want = Value::List(vec![
            Value::Int(p60),
            Value::Int(p60),
            Value::Int(p60),
            Value::Int(1 << 53),
            Value::Int((1 << 53) + 4),
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
            Value::Int(-p60),
            Value::Object(vec![("a".into(), Value::Int(p60))]),
            Value::Float(0.5),
            Value::Int(9_223_372_036_854_774_784),
        ]);
        assert_eq!(v, want);
    }

    #[test]
    fn text_that_is_not_utf8_reads_as_from_utf8_lossy_reads_it() {
        let mut cases: Vec<Vec<u8>> = vec![
            b"\xff".to_vec(),
            b"a\xffb".to_vec(),
            b"\xe2\x82".to_vec(),
            b"x\xe2\x82".to_vec(),
            b"\xe2\x82y\xf0\x9f\x98".to_vec(),
            b"\xed\xa0\x80".to_vec(),
            b"\xc0\x80\xf8\x88\x80\x80\x80".to_vec(),
            "ok \u{1F600}".as_bytes().to_vec(),
        ];
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..5_000 {
            let mut b = Vec::new();
            for _ in 0..(x % 12) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b.push([b'a', 0x80, 0xc3, 0xa9, 0xe2, 0xf0, 0x9f, 0xff][(x % 8) as usize]);
            }
            cases.push(b);
        }
        for b in &cases {
            let want = String::from_utf8_lossy(b);
            let got = unsafe { str_from(b.as_ptr(), b.len()) };
            assert_eq!(got, want, "{b:?}");
        }
    }

    #[test]
    fn query_through_abi() {
        let h = fennec_open();
        let r = run(
            h,
            "create collection t (a int, e vector<2> @hnsw(l2))",
            "",
            &[],
        );
        assert!(r.contains("\"ok\""), "{r}");
        let r = run(h, r#"put t {a: 1, e: [1.0, 0.0]}"#, "", &[]);
        assert!(r.contains("\"affected\""), "{r}");
        let r = run(h, "get t near e $1 limit 1", "[[1.0, 0.0]]", &[]);
        assert!(r.contains("_score"), "{r}");
        let r = run(h, "broken query", "", &[]);
        assert!(r.contains("\"error\""), "{r}");
        fennec_close(h);
    }

    #[test]
    fn changes_through_abi() {
        let h = fennec_open();
        run(h, "create collection t (a int)", "", &[]);
        let at = fennec_changes(h, 0.0);
        let seq_only = read(at);
        assert!(seq_only.contains("\"collections\":[\"t\"]"), "{seq_only}");

        // A text whose last statement fails is put back whole: nothing to name.
        run(h, "create collection u (a int)", "", &[]);
        let seq = seq_of(h);
        let r = run(h, "put t {a: 1}; put u {a: \"x\"}", "", &[]);
        assert!(r.contains("error"), "{r}");
        let out = read(fennec_changes(h, seq as f64));
        assert!(out.contains("\"collections\":[]"), "{out}");
        run(h, "put t {a: 1}; put u {a: 2}", "", &[]);
        let out = read(fennec_changes(h, seq as f64));
        assert!(out.contains("\"collections\":[\"t\",\"u\"]"), "{out}");
        // A dropped collection has no name left: everything is stale.
        let seq = seq_of(h);
        run(h, "drop collection u", "", &[]);
        let out = read(fennec_changes(h, seq as f64));
        assert!(out.contains("\"collections\":null"), "{out}");

        // Shrink the ring and overflow it to check that the horizon rises.
        fennec_set_change_capacity(h, 1);
        run(h, "put t [{a: 1}, {a: 2}]", "", &[]);
        let out = read(fennec_changes(h, 0.0));
        assert!(out.contains("\"collections\":null"), "{out}");
        fennec_close(h);
    }

    /// The change counter, as `changeSeq` reads it.
    fn seq_of(h: u32) -> u64 {
        let out = read(fennec_changes(h, f64::MAX));
        let n = &out["{\"seq\":".len()..];
        n[..n.find(',').unwrap()].parse().unwrap()
    }

    /// Turns a `boxed` buffer into a string (`[u32 len][contents]`).
    fn read(ptr: *mut u8) -> String {
        unsafe {
            let mut len = [0u8; 4];
            std::ptr::copy_nonoverlapping(ptr, len.as_mut_ptr(), 4);
            let len = u32::from_le_bytes(len) as usize;
            let s =
                String::from_utf8_lossy(std::slice::from_raw_parts(ptr.add(4), len)).into_owned();
            fennec_free(ptr, 4 + len);
            s
        }
    }
}
