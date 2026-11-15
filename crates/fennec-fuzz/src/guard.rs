//! Whatever ends a run keeps its input. A panic is caught around the run,
//! and one the code under it caught and went on from is noted by the hook;
//! a stack overflow, a failed allocation and AddressSanitizer's report end
//! in `abort` (ASan told so by [`__asan_default_options`]), whose handler
//! writes the input from where [`arm`] left it with nothing but system
//! calls; a run past the timeout is written by the watchdog; an allocation
//! past the cap aborts before it is made -- what a length read off the
//! input and allocated before it was checked looks like.
//!
//! Each of those ends says the finding's recipe as well ([`say_recipe`]):
//! the target, the seed, the run and its input's hash, and the runs whose
//! inputs the corpus took -- all a maintainer needs to make the input
//! again ([`crate::reproduce`]), where CI keeps none (`--private`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The input of the run in flight, kept where the watchdog reads it.
static CURRENT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// The same bytes for the abort's handler, which takes no lock.
static PTR: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static LEN: AtomicUsize = AtomicUsize::new(0);
/// When the run in flight began, in milliseconds from [`BASE`] plus one; 0
/// between runs.
static BEGAN: AtomicU64 = AtomicU64::new(0);
static BASE: OnceLock<Instant> = OnceLock::new();
/// Where the abort's handler writes the input, NUL-ended, and the same
/// bytes for the handler, which takes no lock.
static ABORT_PATH: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ABORT_PTR: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static ABORT_LEN: AtomicUsize = AtomicUsize::new(0);
/// The watchdog's timeout and the file it writes, the target's of the
/// fuzzer that last called [`install`].
static WATCHED: Mutex<Option<(Duration, PathBuf)>> = Mutex::new(None);
static STARTED: std::sync::Once = std::sync::Once::new();
/// A panic's message, the first since the last [`take_panic`].
static PANIC: Mutex<Option<String>> = Mutex::new(None);
static PANICKED: AtomicBool = AtomicBool::new(false);
/// The most one allocation may ask for.
static CAP: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Whether a finding says its recipe alone: no message, no backtrace.
static PRIVATE: AtomicBool = AtomicBool::new(false);
/// The run in flight, counted from 0.
static RUN: AtomicU64 = AtomicU64::new(0);
/// The recipe's parts: the target's name; the command making the input
/// again, up to the run; the runs kept, as [`push_kept`] writes them; and
/// the last of them.
static NAME: Shared = Shared::new();
static FIXED: Shared = Shared::new();
static KEPT: Shared = Shared::new();
static LAST_KEPT: AtomicU64 = AtomicU64::new(0);

/// Bytes the abort's handler reads with no lock: held behind one for
/// whoever writes them, and published as a pointer and a length after
/// each write. Written between runs alone, on the fuzzer's thread, so a
/// handler -- on that thread or one a run made -- never meets a write.
struct Shared {
    held: Mutex<Vec<u8>>,
    ptr: AtomicPtr<u8>,
    len: AtomicUsize,
}

impl Shared {
    const fn new() -> Shared {
        Shared {
            held: Mutex::new(Vec::new()),
            ptr: AtomicPtr::new(std::ptr::null_mut()),
            len: AtomicUsize::new(0),
        }
    }

    /// The bytes changed by `edit`, then published.
    fn edit(&self, edit: impl FnOnce(&mut Vec<u8>)) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        self.len.store(0, Ordering::Release);
        edit(&mut held);
        self.ptr.store(held.as_mut_ptr(), Ordering::Release);
        self.len.store(held.len(), Ordering::Release);
    }

    /// The bytes as last published, read with no lock.
    fn read(&self) -> &[u8] {
        let (p, n) = (
            self.ptr.load(Ordering::Acquire),
            self.len.load(Ordering::Acquire),
        );
        if p.is_null() || n == 0 {
            return &[];
        }
        // SAFETY: `n` bytes at `p`, published after the write that made
        // them, and written again only between runs (above).
        unsafe { std::slice::from_raw_parts(p, n) }
    }
}

/// FNV-1a: an input's hash, which names its file and its recipe.
pub fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `n` in decimal, written into `buf` with no allocation.
fn digits(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &buf[at..]
}

/// `n` as sixteen hexadecimal digits, with no allocation.
fn hex(n: u64, buf: &mut [u8; 16]) -> &[u8] {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = b"0123456789abcdef"[((n >> (60 - 4 * i)) & 0xf) as usize];
    }
    buf
}

/// `n` in base 36 onto `out`.
fn radix36(mut n: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 13];
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize];
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[at..]);
}

/// A run kept, onto the runs a recipe names: its `step` from the one kept
/// before -- from 0 for the first -- in base 36, after a dot where a run
/// came before it.
pub fn push_kept(text: &mut Vec<u8>, step: u64) {
    if !text.is_empty() {
        text.push(b'.');
    }
    radix36(step, text);
}

/// AddressSanitizer's options where the build carries it, read before
/// `main`: its report ends in `abort`, whose handler keeps the input, and an
/// allocation it refuses is a report, never a null handed back. Leaks are
/// not looked for: a run that leaks is no crash, and macOS's has no LSan.
#[no_mangle]
pub extern "C" fn __asan_default_options() -> *const std::os::raw::c_char {
    c"abort_on_error=1:allocator_may_return_null=0:detect_leaks=0:handle_abort=0:symbolize=1"
        .as_ptr()
}

/// An allocator held to [`CAP`] an allocation: past it, the size is said and
/// the process aborts, its input kept.
pub struct Capped;

// SAFETY: the system's allocator, each call handed on as it came.
unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        capped(l.size());
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        capped(l.size());
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        capped(size);
        System.realloc(p, l, size)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[inline]
fn capped(size: usize) {
    if size > CAP.load(Ordering::Relaxed) {
        too_big(size);
    }
}

#[cold]
#[inline(never)]
fn too_big(size: usize) -> ! {
    if PRIVATE.load(Ordering::Relaxed) {
        sys::say(b"fennec-fuzz: an allocation past the cap (--max-alloc-mb)\n");
        std::process::abort();
    }
    // No allocation from here on: the number is written into the stack.
    let mut buf = [0u8; 20];
    sys::say(b"fennec-fuzz: an allocation of ");
    sys::say(digits(size as u64, &mut buf));
    sys::say(b" bytes, past the cap (--max-alloc-mb)\n");
    // And the way to it, whose frames name what read the length: the input
    // kept is the run's, but what asked may not replay from it alone. Its
    // capture allocates, a little at a time, through this allocator again,
    // so not under a cap so small that it would pass it too.
    if CAP.load(Ordering::Relaxed) >= 1 << 20 {
        let trace = std::backtrace::Backtrace::force_capture();
        eprintln!(
            "fennec-fuzz: on thread {:?}\n{trace}",
            std::thread::current().name()
        );
    }
    std::process::abort();
}

/// Sets up what keeps an input that ends the process: its files go in
/// `dir`, named for `target` and this process; a run past `timeout` is a
/// finding, as is an allocation past `cap` bytes. `private`, a finding
/// says its recipe alone ([`crate::Options::private`]).
pub fn install(dir: &Path, target: &str, timeout: Duration, cap: usize, private: bool) {
    let _ = BASE.set(Instant::now());
    let pid = std::process::id();
    let mut path = dir
        .join(format!("crash-{target}-{pid}"))
        .into_os_string()
        .into_encoded_bytes();
    path.push(0);
    {
        let mut held = ABORT_PATH.lock().unwrap_or_else(|e| e.into_inner());
        *held = path;
        ABORT_PTR.store(held.as_mut_ptr(), Ordering::Relaxed);
        ABORT_LEN.store(held.len(), Ordering::Relaxed);
    }
    *WATCHED.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((timeout, dir.join(format!("timeout-{target}-{pid}"))));
    CAP.store(cap, Ordering::Relaxed);
    PRIVATE.store(private, Ordering::Relaxed);
    STARTED.call_once(|| {
        sys::on_abort();
        std::panic::set_hook(Box::new(|info| {
            if !PANICKED.swap(true, Ordering::Relaxed) {
                let message = info.to_string();
                if !PRIVATE.load(Ordering::Relaxed) {
                    eprintln!("fennec-fuzz: {message}");
                    // Under RUST_BACKTRACE, as the default hook would: a
                    // replay's way to the line.
                    let trace = std::backtrace::Backtrace::capture();
                    if trace.status() == std::backtrace::BacktraceStatus::Captured {
                        eprintln!("{trace}");
                    }
                }
                *PANIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
            }
        }));
        std::thread::Builder::new()
            .name("fuzz-watchdog".into())
            .spawn(watch)
            .expect("the watchdog's thread");
    });
}

/// The recipe a finding of this run will say: `target`'s, from `seed` and
/// the start of fingerprint `start`, read from the corpus directory
/// `corpus` as well where it gave the start more than the seeds. No run
/// is kept yet.
pub fn recipe(target: &str, seed: u64, start: u64, corpus: Option<&Path>) {
    NAME.edit(|b| {
        b.clear();
        b.extend_from_slice(target.as_bytes());
    });
    FIXED.edit(|b| {
        b.clear();
        b.extend_from_slice(
            format!("fennec-fuzz reproduce {target} --seed {seed} --start {start:016x}").as_bytes(),
        );
        if let Some(dir) = corpus {
            b.extend_from_slice(format!(" --corpus {}", dir.display()).as_bytes());
        }
    });
    KEPT.edit(|b| {
        b.clear();
        // Room for some thousands of runs kept before it grows.
        b.reserve(64 << 10);
    });
    LAST_KEPT.store(0, Ordering::Relaxed);
}

/// The corpus took the input of `run`.
pub fn kept(run: u64) {
    let step = run - LAST_KEPT.swap(run, Ordering::Relaxed);
    KEPT.edit(|b| push_kept(b, step));
}

/// The recipe of the finding that ended the run in flight, on standard
/// error with system calls alone -- the abort's handler says it
/// ([`recipe_into`]).
pub fn say_recipe(kind: &[u8]) {
    recipe_into(kind, &mut |b| sys::say(b));
}

/// The recipe of the finding that ended the run in flight, a line handed to
/// `out` a part at a time with no allocation: what ended it (`kind`), the
/// run, its input's length and hash, and the command that makes the input
/// again ([`crate::Recipe::from_args`] reads its flags). Nothing where no
/// recipe was set up (a replay).
pub fn recipe_into(kind: &[u8], out: &mut dyn FnMut(&[u8])) {
    let fixed = FIXED.read();
    if fixed.is_empty() {
        return;
    }
    let (p, n) = (PTR.load(Ordering::Relaxed), LEN.load(Ordering::Relaxed));
    let input: &[u8] = match p.is_null() {
        true => &[],
        // SAFETY: the input `arm` left, which nothing moved since.
        false => unsafe { std::slice::from_raw_parts(p, n) },
    };
    let (mut d, mut l, mut h) = ([0u8; 20], [0u8; 20], [0u8; 16]);
    let hash = hex(fnv(input), &mut h);
    let run = digits(RUN.load(Ordering::Relaxed), &mut d);
    let kept = match KEPT.read() {
        [] => &b"-"[..],
        kept => kept,
    };
    for part in [
        b"fennec-fuzz: ",
        NAME.read(),
        b": ",
        kind,
        b" at run ",
        run,
        b", its input ",
        digits(input.len() as u64, &mut l),
        b" bytes of fnv ",
        hash,
        b"; made again by\n    ",
        fixed,
        b" --run ",
        run,
        b" --fnv ",
        hash,
        b" --kept ",
        kept,
        b"\n",
    ] {
        out(part);
    }
}

/// The input run `run` is about to be handed.
pub fn arm(run: u64, input: &[u8]) {
    RUN.store(run, Ordering::Relaxed);
    let mut cur = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    cur.clear();
    cur.extend_from_slice(input);
    PTR.store(cur.as_mut_ptr(), Ordering::Relaxed);
    LEN.store(cur.len(), Ordering::Relaxed);
    let now = BASE.get().map_or(0, |b| b.elapsed().as_millis() as u64);
    BEGAN.store(now + 1, Ordering::Release);
}

/// The run is over.
pub fn disarm() {
    BEGAN.store(0, Ordering::Release);
}

/// A panic since the last call, caught where it happened or not: its
/// message.
pub fn take_panic() -> Option<String> {
    if !PANICKED.swap(false, Ordering::Relaxed) {
        return None;
    }
    PANIC.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The watchdog: every tenth of the timeout, whether the run in flight
/// began longer ago than it, and if so its input written and the process
/// ended -- a run that loops for ever is a finding the run itself never
/// returns to report.
fn watch() {
    loop {
        let watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some((timeout, file)) = watched else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let step = (timeout / 10).clamp(Duration::from_millis(10), Duration::from_secs(1));
        let limit = timeout.as_millis().min(u64::MAX as u128) as u64;
        std::thread::sleep(step);
        let began = BEGAN.load(Ordering::Acquire);
        let now = BASE.get().map_or(0, |b| b.elapsed().as_millis() as u64) + 1;
        if began == 0 || now.saturating_sub(began) <= limit {
            continue;
        }
        let input = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // The same run still: a new one began after the lock was taken
        // would have moved `BEGAN`.
        if BEGAN.load(Ordering::Acquire) != began {
            continue;
        }
        let _ = std::fs::write(&file, &input);
        eprintln!(
            "fennec-fuzz: a run took more than {} ms; its input is {}",
            limit,
            file.display()
        );
        say_recipe(b"a run past the timeout");
        sys::exit(3);
    }
}

#[cfg(unix)]
mod sys {
    use std::os::raw::{c_char, c_int, c_uint, c_void};
    use std::sync::atomic::Ordering;

    extern "C" {
        fn signal(sig: c_int, handler: usize) -> usize;
        fn creat(path: *const c_char, mode: c_uint) -> c_int;
        fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
        fn close(fd: c_int) -> c_int;
        fn _exit(code: c_int) -> !;
    }

    const SIGABRT: c_int = 6;

    /// Writes `bytes` to standard error, a system call and nothing else.
    pub fn say(bytes: &[u8]) {
        // SAFETY: a write of the bytes `bytes` holds.
        unsafe { write_all(2, bytes.as_ptr(), bytes.len()) };
    }

    unsafe fn write_all(fd: c_int, mut p: *const u8, mut n: usize) {
        while n > 0 {
            let w = write(fd, p as *const c_void, n);
            if w <= 0 {
                return;
            }
            p = p.add(w as usize);
            n -= w as usize;
        }
    }

    pub fn exit(code: c_int) -> ! {
        // SAFETY: the process ends here, its other threads with it.
        unsafe { _exit(code) }
    }

    /// The abort's handler: the input in flight written where `install`
    /// said, with system calls alone -- it runs on whatever stack the abort
    /// left it, perhaps the one a stack overflow's handler ran on -- then
    /// the process ended.
    extern "C" fn aborted(_: c_int) {
        let began = super::BEGAN.load(Ordering::Acquire);
        let (p, n) = (
            super::PTR.load(Ordering::Relaxed),
            super::LEN.load(Ordering::Relaxed),
        );
        let (path, path_len) = (
            super::ABORT_PTR.load(Ordering::Relaxed),
            super::ABORT_LEN.load(Ordering::Relaxed),
        );
        if began != 0 && !p.is_null() && !path.is_null() && path_len > 1 {
            // SAFETY: a NUL-ended path and the input's bytes, each where
            // `install` and `arm` left them, which nothing has moved since
            // the run that aborted began.
            unsafe {
                let fd = creat(path as *const c_char, 0o644);
                if fd >= 0 {
                    write_all(fd, p, n);
                    close(fd);
                }
                say(b"fennec-fuzz: the process aborted on an input, written to ");
                write_all(2, path, path_len - 1);
                say(b"\n");
            }
            super::say_recipe(b"an abort");
        }
        exit(2);
    }

    pub fn on_abort() {
        // SAFETY: a handler of the C signature, installed once.
        unsafe {
            signal(SIGABRT, aborted as extern "C" fn(c_int) as usize);
        }
    }
}

#[cfg(not(unix))]
mod sys {
    /// Elsewhere an abort ends the process with its input unkept: the
    /// fuzzer runs on Linux and macOS.
    pub fn on_abort() {}

    pub fn say(bytes: &[u8]) {
        use std::io::Write;
        let _ = std::io::stderr().write_all(bytes);
    }

    pub fn exit(code: i32) -> ! {
        std::process::exit(code)
    }
}
