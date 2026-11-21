//! The most memory a primary's message holds as a replica reads it: the
//! allocator's high-water mark, counted by an allocator of this binary's
//! own on the reading thread alone, against the message's length and
//! against what had come of it. `replication::read_kind` grows a body as
//! its bytes come, doubling; a growth holds the bytes that came and the
//! buffer twice as long the allocator moves them into, counted here as
//! moved whether or not the allocator grew it in place. Its doc comment
//! and docs/design/replication.md give these numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    /// Whether this thread's allocations are counted, the bytes they hold
    /// now and the most they held.
    static ON: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

fn counted(f: impl FnOnce(usize, usize) -> (usize, usize)) {
    let _ = ON.try_with(|on| {
        if on.get() {
            let (live, peak) = f(LIVE.get(), PEAK.get());
            LIVE.set(live);
            PEAK.set(peak.max(live));
        }
    });
}

// SAFETY: every call is handed to the system's allocator as it came; the
// counting reads and writes this thread's cells alone, allocating nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        counted(|live, peak| (live + l.size(), peak));
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        counted(|live, peak| (live + l.size(), peak));
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        counted(|live, peak| (live.saturating_sub(l.size()), peak));
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, to: usize) -> *mut u8 {
        // Moved, the old block and the new are held at once.
        counted(|live, peak| (live - l.size().min(live) + to, peak.max(live + to)));
        unsafe { System.realloc(p, l, to) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The most bytes `f` held at once on this thread.
fn peak_of(f: impl FnOnce()) -> usize {
    LIVE.set(0);
    PEAK.set(0);
    ON.set(true);
    f();
    ON.set(false);
    PEAK.get()
}

const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\r\n";

/// A primary's answer: its head, a hello promising an image, and an image
/// announced `len` bytes long, `sent` of them sent.
fn answer(len: usize, sent: usize) -> Vec<u8> {
    let mut hello = vec![1u8, 1];
    for v in [1u64, 1, 0] {
        hello.extend_from_slice(&v.to_le_bytes());
    }
    let lineage = fennec_core::history::History {
        lineage: vec![((7u64 << 32) | 1, 0)],
        following: false,
    };
    hello.extend_from_slice(&lineage.encode());
    let mut out = HEAD.to_vec();
    out.push(b'H');
    out.extend_from_slice(&(hello.len() as u64).to_le_bytes());
    out.extend_from_slice(&hello);
    out.push(b'I');
    out.extend_from_slice(&(len as u64).to_le_bytes());
    out.resize(out.len() + sent, 0xab);
    out
}

/// The most a message read whole held, against its length: twice it at
/// the most, 1.8 times for an image of 40 MiB, and twice for one a byte
/// past where the last growth begins -- `FIRST` (2 MiB + 16) doubled twice,
/// and one byte. A message cut short held three times what had come of it
/// as it grew at the most: an image of a terabyte announced and 3 MiB sent,
/// refused as cut short, grew once, from `FIRST`. The few bytes besides --
/// the head's line, the hello -- are let go before the image is read.
#[test]
fn a_message_holds_twice_its_length_and_three_times_what_came() {
    const FIRST: usize = (2 << 20) + 16;
    const BESIDE: usize = 4 << 10;
    for len in [40 << 20, 4 * FIRST + 1] {
        let bytes = answer(len, len);
        let mut n = 0;
        let peak = peak_of(|| {
            let (read, end) = fennec_http::replication::read_answer(&mut &bytes[..], None);
            assert_eq!(end.kind(), std::io::ErrorKind::UnexpectedEof, "{end}");
            n = read;
        });
        assert_eq!(n, 2);
        let factor = peak as f64 / len as f64;
        eprintln!("a message of {len} bytes held {peak} at the most: {factor:.3} times it");
        assert!(
            peak <= 2 * len + BESIDE,
            "{peak} bytes held for a message of {len}"
        );
    }
    let sent = 3 << 20;
    let bytes = answer(1 << 40, sent);
    let peak = peak_of(|| {
        let (_, end) = fennec_http::replication::read_answer(&mut &bytes[..], None);
        assert_eq!(end.kind(), std::io::ErrorKind::UnexpectedEof, "{end}");
    });
    eprintln!(
        "a terabyte announced, {sent} bytes sent, held {peak} at the most: {:.3} times it",
        peak as f64 / sent as f64
    );
    assert!(
        peak <= 3 * FIRST + BESIDE,
        "{peak} bytes held for {FIRST} come as it grew"
    );
}
