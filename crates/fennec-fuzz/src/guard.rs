//! Whatever ends a run keeps its input. A panic is caught around the run,
//! and one the code under it caught and went on from is noted by the hook;
//! a stack overflow, a failed allocation and AddressSanitizer's report end
//! in `abort` (ASan told so by [`__asan_default_options`]), whose handler
//! writes the input from where [`arm`] left it with nothing but system
//! calls; a run past the timeout is written by the watchdog; an allocation
//! past the cap aborts before it is made -- what a length read off the
//! input and allocated before it was checked looks like.

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
    // No allocation from here on: the number is written into the stack.
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut n = size;
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    sys::say(b"fennec-fuzz: an allocation of ");
    sys::say(&digits[at..]);
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
/// finding, as is an allocation past `cap` bytes.
pub fn install(dir: &Path, target: &str, timeout: Duration, cap: usize) {
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
    STARTED.call_once(|| {
        sys::on_abort();
        std::panic::set_hook(Box::new(|info| {
            if !PANICKED.swap(true, Ordering::Relaxed) {
                let message = info.to_string();
                eprintln!("fennec-fuzz: {message}");
                // Under RUST_BACKTRACE, as the default hook would: a replay's
                // way to the line.
                let trace = std::backtrace::Backtrace::capture();
                if trace.status() == std::backtrace::BacktraceStatus::Captured {
                    eprintln!("{trace}");
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

/// The input a run is about to be handed.
pub fn arm(input: &[u8]) {
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
