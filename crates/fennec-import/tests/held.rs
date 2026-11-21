//! The bytes a thread holds, counted by the binary's allocator: a test
//! holds a reader's memory to a bound by what it allocated, never by how
//! long it took. Counted a thread at a time, so the binary's other tests,
//! each on a thread of its own, count nowhere in it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The system's allocator, every allocation and release counted on the
/// thread that made it.
pub struct Counting;

thread_local! {
    // `const` and no destructor: reading them allocates nothing, which an
    // allocator's own bookkeeping must not.
    static HELD: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn moved(by: isize) {
    // `try_with`: a thread's allocations once its locals are gone count
    // nowhere, rather than panicking in the allocator.
    let _ = HELD.try_with(|held| {
        let now = held.get() + by;
        held.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

// SAFETY: the system's allocator, each call handed on as it came; the
// count reads and writes thread locals alone, which allocate nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: `l` is this call's layout, of a nonzero size as
        // `GlobalAlloc::alloc`'s caller guarantees, handed on as it came.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            moved(l.size() as isize);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // SAFETY: as `alloc`'s.
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            moved(l.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` is `System`'s, made with `l` -- every call here hands
        // on to it -- as `GlobalAlloc::dealloc`'s caller guarantees.
        unsafe { System.dealloc(p, l) };
        moved(-(l.size() as isize));
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        // SAFETY: `p` is `System`'s, made with `l`, and `size` is nonzero and
        // fits `isize` once rounded to `l.align()`, as
        // `GlobalAlloc::realloc`'s caller guarantees.
        let q = unsafe { System.realloc(p, l, size) };
        if !q.is_null() {
            moved(size as isize - l.size() as isize);
        }
        q
    }
}

/// Runs `f` on this thread and says the most it held at once above what
/// the thread held before, its answer included.
pub fn peak<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = HELD.with(Cell::get);
    PEAK.with(|p| p.set(base));
    let r = f();
    let top = PEAK.with(Cell::get);
    (r, (top - base).max(0) as usize)
}
