//! What a run reached, from the counters LLVM's sanitizer coverage puts on
//! every edge of the code it compiles: `make fuzz-smoke` builds with
//! `-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3
//! -Cllvm-args=-sanitizer-coverage-inline-8bit-counters`, the compiler's own
//! instrumentation, and each module's constructor hands its counters to
//! [`__sanitizer_cov_8bit_counters_init`] below -- the one function of
//! libFuzzer's runtime the instrumentation calls, written here, so the
//! fuzzer is guided by coverage and takes no crate. A build without the
//! flags (`cargo test`, a plain `make fuzz`) calls it never, and the fuzzer
//! mutates its seeds unguided.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The counters' regions handed over, as `(start, end)` addresses: one a
/// module, the same one again where modules share a section.
const MOST: usize = 64;
static REGIONS: [AtomicUsize; 2 * MOST] = [const { AtomicUsize::new(0) }; 2 * MOST];
static HELD: AtomicUsize = AtomicUsize::new(0);

/// Called by each instrumented module's constructor, before `main`, with its
/// counters: a byte an edge, counting up to 255 and wrapping.
///
/// # Safety
///
/// `start..stop` are the counters' bytes, alive for the process: the
/// instrumentation's own section.
#[no_mangle]
pub unsafe extern "C" fn __sanitizer_cov_8bit_counters_init(start: *mut u8, stop: *mut u8) {
    let (start, stop) = (start as usize, stop as usize);
    let held = HELD.load(Ordering::Relaxed);
    for i in 0..held {
        if REGIONS[2 * i].load(Ordering::Relaxed) == start {
            return;
        }
    }
    if held < MOST && start < stop {
        REGIONS[2 * held].store(start, Ordering::Relaxed);
        REGIONS[2 * held + 1].store(stop, Ordering::Relaxed);
        HELD.store(held + 1, Ordering::Relaxed);
    }
}

/// The counters, read and cleared after each run.
pub struct Coverage {
    regions: Vec<(usize, usize)>,
    /// For each counter the buckets of counts seen so far, a bit each.
    seen: Vec<u8>,
    /// How many counters have been seen at all.
    pub edges: usize,
}

impl Coverage {
    /// The counters the build carries, or `None` where it carries none.
    pub fn of_build() -> Option<Coverage> {
        let held = HELD.load(Ordering::Relaxed);
        let regions: Vec<(usize, usize)> = (0..held)
            .map(|i| {
                (
                    REGIONS[2 * i].load(Ordering::Relaxed),
                    REGIONS[2 * i + 1].load(Ordering::Relaxed),
                )
            })
            .collect();
        let total: usize = regions.iter().map(|(s, e)| e - s).sum();
        (total > 0).then(|| Coverage {
            regions,
            seen: vec![0; total],
            edges: 0,
        })
    }

    /// How many counters the build carries.
    pub fn counters(&self) -> usize {
        self.seen.len()
    }

    /// Reads the counters since the last call, clears them, and says
    /// whether any reached a bucket of counts never seen before -- an edge
    /// taken the first time, or taken 2, 3, 4-7, 8-15, 16-31, 32-127 or
    /// 128-255 times where it never was: AFL's buckets, so a loop run more
    /// times counts as new without every count being one.
    pub fn take_new(&mut self) -> bool {
        let Coverage {
            regions,
            seen,
            edges,
        } = self;
        let mut new = false;
        let mut at = 0;
        for &(start, end) in regions.iter() {
            let len = end - start;
            // SAFETY: the instrumentation's section, alive for the process;
            // the run that writes it is over, on this thread.
            let counters = unsafe { std::slice::from_raw_parts_mut(start as *mut u8, len) };
            // 64 counters at a time, most of them none: a run reaches a few
            // thousand edges of the build's hundreds of thousands, and the
            // scan is most of what a run of a quick target costs.
            let (blocks, tail) = counters.as_chunks_mut::<64>();
            for (b, block) in blocks.iter_mut().enumerate() {
                let (words, _) = block.as_chunks::<8>();
                if words.iter().fold(0, |o, w| o | u64::from_ne_bytes(*w)) == 0 {
                    continue;
                }
                for (j, c) in block.iter_mut().enumerate() {
                    if *c != 0 {
                        new |= note(&mut seen[at + b * 64 + j], edges, *c);
                        *c = 0;
                    }
                }
            }
            let base = at + blocks.len() * 64;
            for (j, c) in tail.iter_mut().enumerate() {
                if *c != 0 {
                    new |= note(&mut seen[base + j], edges, *c);
                    *c = 0;
                }
            }
            at += len;
        }
        new
    }

    /// Takes every counter the runs since the last call reached as seen in
    /// every bucket: the fuzzer's own code -- its mutator, its corpus --
    /// instrumented with the targets', which would otherwise read as new
    /// coverage each time a mutation took another path.
    pub fn ignore_reached(&mut self) {
        let Coverage {
            regions,
            seen,
            edges,
        } = self;
        let mut at = 0;
        for &(start, end) in regions.iter() {
            let len = end - start;
            // SAFETY: as in `take_new`.
            let counters = unsafe { std::slice::from_raw_parts_mut(start as *mut u8, len) };
            for (j, c) in counters.iter_mut().enumerate() {
                if *c != 0 {
                    if seen[at + j] == 0 {
                        *edges += 1;
                    }
                    seen[at + j] = 0xff;
                    *c = 0;
                }
            }
            at += len;
        }
    }
}

/// Notes a counter's `count` in its buckets `seen`, `edges` counting the
/// counters seen at all: whether the bucket is new.
fn note(seen: &mut u8, edges: &mut usize, count: u8) -> bool {
    let bucket = match count {
        1 => 1,
        2 => 2,
        3 => 4,
        4..=7 => 8,
        8..=15 => 16,
        16..=31 => 32,
        32..=127 => 64,
        _ => 128,
    };
    if *seen & bucket != 0 {
        return false;
    }
    if *seen == 0 {
        *edges += 1;
    }
    *seen |= bucket;
    true
}
