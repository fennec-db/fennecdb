//! `<file>.lock`, held exclusively for as long as a process has a file
//! open to write: two databases over one file corrupt it, whether they are
//! two processes or two opens in one. The native library takes it as an
//! app opens a file, `fennec mcp --file` as an agent's session does,
//! `fennec-server` as it serves a file and a `--dir` node as it opens a
//! tenant's, `fennec-shard` its directory, and the shell (`fennec <file>`,
//! `-c`, a script, `fennec import`, a backup or a restore into a file)
//! before it opens or writes one, so any two of them refuse each other.
//! Not compiled into the browser module, which has no files.
//!
//! On a Unix the lock is a record lock (`fcntl`), on Windows `LockFileEx`'s
//! over one byte past the holder's line ([`record`]). Elsewhere it is not
//! taken: [`hold`] makes the file and refuses nothing, in this process or
//! another. Nor does a lock reach across a file system that does not pass
//! it on: two processes on either side of a Docker Desktop bind mount, or
//! of a network file system that keeps no locks, each take the lock of the
//! same file.

use fennec_core::engine::{Durability, ImageOut, Sink};
use fennec_core::error::Result as CoreResult;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// `<path>.lock`, held exclusively ([`hold`]), and noted among this
/// process's ([`LOCKED`]) until dropped.
pub struct FileLock {
    file: Option<File>,
    /// The lock file's identity ([`key_at`]), under which [`LOCKED`] notes
    /// it.
    key: Key,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let mut locked = LOCKED.lock().unwrap_or_else(|e| e.into_inner());
        // Let go before the close on Windows: one the close leaves is let
        // go when the system gets to it, which its documentation leaves
        // open, and the hold that follows here could be refused meanwhile.
        #[cfg(windows)]
        if let Some(f) = &self.file {
            record::release(f);
        }
        // Closed before the note goes: an open that finds the file free
        // here finds the lock let go of too.
        drop(self.file.take());
        locked.retain(|k| *k != self.key);
        LET_GO.notify_all();
    }
}

/// A database's sink holding `<file>.lock` ([`holding`]) for as long as
/// the database holds the sink, which is its whole life: a sink is set
/// once, at the open. What a file whose database has several owners is
/// opened with -- a tenant's, which the registry, its follower and a
/// linking thread hold -- so the lock goes with the last of them, never
/// before: held by the tenant, a follower writing on after the registry
/// let the tenant go would have written a file the lock no longer kept.
/// Every method is passed on, the defaults too: one left to the trait's
/// default would skip the file's own (a rewrite built whole in memory, a
/// write behind the lock never run).
pub struct Holding {
    /// Dropped first, as fields drop in order: the file is closed before
    /// its lock is let go.
    sink: Box<dyn Sink>,
    _lock: FileLock,
}

/// What wraps a file's sink as it is opened (`fennec_core::fs::Wrap`) to
/// hold `lock` with it ([`Holding`]).
pub fn holding(lock: FileLock) -> impl FnOnce(Box<dyn Sink>) -> CoreResult<Box<dyn Sink>> {
    move |sink| Ok(Box::new(Holding { sink, _lock: lock }) as Box<dyn Sink>)
}

impl Sink for Holding {
    fn append(&mut self, bytes: &[u8]) -> CoreResult<()> {
        self.sink.append(bytes)
    }
    fn record(&mut self, seq: u64, bytes: &[u8]) -> CoreResult<()> {
        self.sink.record(seq, bytes)
    }
    fn append_deferred(&mut self, bytes: &[u8]) -> CoreResult<()> {
        self.sink.append_deferred(bytes)
    }
    fn rewrite(&mut self, bytes: &[u8]) -> CoreResult<()> {
        self.sink.rewrite(bytes)
    }
    fn rewrite_with(
        &mut self,
        image: &mut dyn FnMut(&mut dyn ImageOut) -> CoreResult<()>,
    ) -> CoreResult<()> {
        self.sink.rewrite_with(image)
    }
    fn remapped(&mut self) -> Option<fennec_core::store::Base> {
        self.sink.remapped()
    }
    fn written_through(&mut self) -> CoreResult<Option<fennec_core::store::Base>> {
        self.sink.written_through()
    }
    fn syncing(&self) -> bool {
        self.sink.syncing()
    }
    fn land(&mut self, seq: u64, spilled: &[&[u8]], record: &[u8]) -> CoreResult<()> {
        self.sink.land(seq, spilled, record)
    }
    fn side(&self) -> Option<std::path::PathBuf> {
        self.sink.side()
    }
    fn adopt(&mut self, side: &std::path::Path) -> CoreResult<()> {
        self.sink.adopt(side)
    }
    fn sync_existing(&mut self) -> CoreResult<()> {
        self.sink.sync_existing()
    }
    fn write_out(&mut self) -> CoreResult<()> {
        self.sink.write_out()
    }
    fn write_behind(&mut self, all: bool) -> Option<Durability> {
        self.sink.write_behind(all)
    }
    fn written_all(&self) -> bool {
        self.sink.written_all()
    }
    fn sync(&mut self) -> CoreResult<()> {
        self.sink.sync()
    }
    fn flush(&mut self) -> CoreResult<Option<Durability>> {
        self.sink.flush()
    }
}

/// Why [`hold`] did not take the lock.
#[derive(Debug)]
pub enum Refused {
    /// The file is open already, in this process or another.
    Held,
    /// The lock file could not be made or read.
    Io(std::io::Error),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Held => f.write_str(
                "it is open already, in this process or another: two databases over one file corrupt it",
            ),
            Refused::Io(e) => write!(f, "its lock file: {e}"),
        }
    }
}

/// The lock files this process holds, by identity ([`key_at`]). A record
/// lock is the process's, so it refuses only another process's open; a
/// second open here is refused by this list -- before the lock file is
/// opened again, since closing any descriptor of a file lets go of the
/// process's record locks on it. A Windows lock is the handle's, so the
/// system refuses a second handle here too, but only this list tells a
/// hold to wait for a holder in this process ([`hold_within`]) and a
/// refusal that the holder is this process ([`holder`]).
static LOCKED: Mutex<Vec<Key>> = Mutex::new(Vec::new());

/// Told each time this process lets a lock go, for [`hold_within`]'s wait.
static LET_GO: Condvar = Condvar::new();

/// A lock file's identity: its device and inode, or on Windows its volume
/// and file index.
type Key = (u64, u64);

/// The identity of the lock file at `name`, which [`LOCKED`] notes it by:
/// `None` where it cannot be read, or where the system names none, which
/// leaves every hold here to the lock alone.
#[cfg(unix)]
fn key_at(name: &std::path::Path) -> Option<Key> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(name).ok().map(|m| (m.dev(), m.ino()))
}

/// Through a handle opened for no access at all, as std's own `metadata`
/// asks: a second handle on Windows lets no lock go, as a second
/// descriptor would on a Unix.
#[cfg(windows)]
fn key_at(name: &std::path::Path) -> Option<Key> {
    use std::os::windows::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new().access_mode(0).open(name).ok()?;
    record::id(&f).ok()
}

#[cfg(not(any(unix, windows)))]
fn key_at(_: &std::path::Path) -> Option<Key> {
    None
}

/// The identity of the lock file open as `file`, `meta` its metadata.
#[cfg(unix)]
fn key_of(_: &File, meta: &std::fs::Metadata) -> std::io::Result<Key> {
    use std::os::unix::fs::MetadataExt;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn key_of(file: &File, _: &std::fs::Metadata) -> std::io::Result<Key> {
    record::id(file)
}

#[cfg(not(any(unix, windows)))]
fn key_of(_: &File, _: &std::fs::Metadata) -> std::io::Result<Key> {
    Ok((0, 0))
}

/// The lock file of the database at `path`: `<file>.lock` beside the file
/// the path names once a symbolic link in its place is followed
/// (`fennec_core::fs::resolved`), the file the open writes and a checkpoint
/// renames over -- named as written, a shell through a link to a served
/// file took a lock of its own and wrote beside the server. Hard links are
/// two names for one file that no name tells apart: each has a lock of its
/// own.
pub fn lock_path(path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    // Joined as the system's bytes, not as text: a lossy name would be
    // another file.
    let mut name = fennec_core::fs::resolved(path.as_ref()).into_os_string();
    name.push(".lock");
    name.into()
}

/// Holds `<path>.lock` exclusively for as long as the file is open. The
/// file itself cannot carry the lock: a checkpoint or a compact renames a
/// new file over it, and a lock on the old one would let a second opener
/// in. It is a record lock (`fcntl`'s `F_SETLK`), which belongs to the
/// process, not an `flock`, which belongs to the open file: a child
/// spawned while the lock file was open -- a test starting a server, an app
/// starting a helper -- shares that open file until its exec closes it, and
/// a close and an open again in that window found the `flock` still held
/// (37 of 20 000 beside a thread spawning `/usr/bin/true`, none with the
/// record lock). The system lets either go when the process ends, however.
/// On Windows it is `LockFileEx`'s, which belongs to the handle and is let
/// go with it, or with the process.
///
/// Once it is taken, the process's id and program are written into the
/// file, through the descriptor that holds the lock, for an opener it
/// refuses to name ([`holder`]). The lock is the record lock alone: what
/// the file holds decides nothing.
pub fn hold(path: impl AsRef<std::path::Path>) -> Result<FileLock, Refused> {
    hold_within(path, Duration::ZERO)
}

/// [`hold`], waiting up to `wait` for a holder in this process to let go.
/// A database this process let go of may be held a moment longer by what
/// ran beside it -- a tenant's follower ending its thread, a linking
/// thread's slice, the graph keeper's append -- and the open that follows
/// a close, a delete or a follow would be refused for nothing, a 500 to a
/// request racing it. Woken as each lock goes ([`LET_GO`]), never polled.
/// Another process's hold is refused at once, as [`hold`] refuses it: it
/// goes when that process chooses.
pub fn hold_within(path: impl AsRef<std::path::Path>, wait: Duration) -> Result<FileLock, Refused> {
    let name = lock_path(path);
    let deadline = Instant::now() + wait;
    let mut locked = LOCKED.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let here = |locked: &Vec<Key>| key_at(&name).is_some_and(|k| locked.contains(&k));
        if !here(&locked) {
            return take(name, locked);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Refused::Held);
        }
        // Noted under the guard the wait lets go of, so a test that sees
        // it ([`waiting_for`]) sees a hold that will be woken.
        lock_waiting().push(name.clone());
        locked = LET_GO
            .wait_timeout(locked, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
        let mut waiting = lock_waiting();
        if let Some(i) = waiting.iter().position(|n| *n == name) {
            waiting.swap_remove(i);
        }
    }
}

/// The lock files a [`hold_within`] in this process waits for.
static WAITING: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

fn lock_waiting() -> MutexGuard<'static, Vec<std::path::PathBuf>> {
    WAITING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a [`hold_within`] in this process waits for `path`'s lock to be
/// let go: what a test holding the holder at a gate waits for before it
/// opens it, so the hold is known to have waited.
pub fn waiting_for(path: impl AsRef<std::path::Path>) -> bool {
    let name = lock_path(path);
    lock_waiting().contains(&name)
}

/// Takes the lock file `name` no lock of this process holds, under
/// [`LOCKED`]'s guard.
fn take(
    name: std::path::PathBuf,
    mut locked: MutexGuard<'_, Vec<Key>>,
) -> Result<FileLock, Refused> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&name)
        .map_err(Refused::Io)?;
    let meta = lock.metadata().map_err(Refused::Io)?;
    let key = key_of(&lock, &meta).map_err(Refused::Io)?;
    #[cfg(any(unix, windows))]
    record::take(&lock)?;
    locked.push(key);
    // Written only now, under the lock: before it, the line was the
    // holder's to keep. Over the old one in place, and cut only where that
    // was of another length: with a truncate each time and the program's
    // name looked up each time, a hold was 50-58 us on an M1, where a
    // tenant's open is 100; now 32-36, 20 of them the lock file's open. A
    // failure costs only the name a refusal gives.
    let line = own_line();
    let _ =
        (&lock)
            .write_all(line.as_bytes())
            .and_then(|()| match meta.len() == line.len() as u64 {
                true => Ok(()),
                false => lock.set_len(line.len() as u64),
            });
    Ok(FileLock {
        file: Some(lock),
        key,
    })
}

/// This process's line ([`Holder::line`]), worked out once: its program
/// does not change while it runs.
fn own_line() -> &'static str {
    static LINE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LINE.get_or_init(|| Holder::this_process().line())
}

/// The process holding a lock file, as far as can be told ([`holder`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// Its id: the system's (`F_GETLK`), or the one it wrote into the file
    /// where the system names none.
    pub pid: Option<u32>,
    /// Its program's file name, as it wrote it when it took the lock --
    /// `fennec-server`, `fennec`, an app's: taken only where the id it wrote
    /// is the one the system names, or a line an earlier holder left could
    /// name the wrong program.
    pub program: Option<String>,
}

impl Holder {
    fn this_process() -> Holder {
        let exe = std::env::current_exe()
            .ok()
            .or_else(|| std::env::args_os().next().map(Into::into));
        // Named without Windows' `.exe`, as everywhere else: a refusal
        // tells a server from a shell by the name (fennec-cli's `held`).
        let program = exe
            .as_deref()
            .and_then(std::path::Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .map(|n| match n.strip_suffix(std::env::consts::EXE_SUFFIX) {
                Some(stem) if !stem.is_empty() => stem.to_string(),
                _ => n,
            })
            .map(|n| n.replace(['\n', '\r'], " "))
            .filter(|n| !n.trim().is_empty());
        Holder {
            pid: Some(std::process::id()),
            program,
        }
    }

    /// What [`hold`] writes into the file: `<pid> <program>`, a line.
    fn line(&self) -> String {
        format!(
            "{} {}\n",
            self.pid.unwrap_or(0),
            self.program.as_deref().unwrap_or("")
        )
    }

    /// A line [`line`](Self::line) wrote, read back: the id and the program.
    fn read(text: &str) -> Option<(u32, Option<String>)> {
        let line = text.lines().next()?;
        let (pid, program) = line.split_once(' ').unwrap_or((line, ""));
        let pid = pid.parse().ok().filter(|p| *p != 0)?;
        let program = Some(program.trim().to_string()).filter(|p| !p.is_empty());
        Some((pid, program))
    }

    /// Whether anything is known of it.
    pub fn is_known(&self) -> bool {
        self.pid.is_some() || self.program.is_some()
    }
}

/// `fennec-server, pid 4242`, as much of it as is known.
impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.program, self.pid) {
            (Some(p), Some(pid)) => write!(f, "{p}, pid {pid}"),
            (Some(p), None) => f.write_str(p),
            (None, Some(pid)) => write!(f, "pid {pid}"),
            (None, None) => f.write_str("a process the system does not name"),
        }
    }
}

/// Who holds `<path>.lock`, for a refusal to name: this process, where it
/// is among [`LOCKED`]; else the process the system says holds the record
/// lock (`F_GETLK`), with the program it wrote into the file as it took it
/// ([`hold`]). `None` where nothing holds it now, or the file cannot be
/// read. The lock file is opened only once it is known not to be this
/// process's own: closing a descriptor of it would let this process's lock
/// go. A holder that wrote nothing, an older release, is named by its id
/// alone. Windows names no holder: the line alone, read beside the locked
/// byte, which a holder let go of since may have left.
pub fn holder(path: impl AsRef<std::path::Path>) -> Option<Holder> {
    let name = lock_path(path);
    // Held across the open and the close, so no `hold` in this process
    // takes the lock meanwhile and has it let go by the close below.
    let locked = LOCKED.lock().unwrap_or_else(|e| e.into_inner());
    if key_at(&name).is_some_and(|k| locked.contains(&k)) {
        return Some(Holder::this_process());
    }
    let file = File::open(&name).ok()?;
    let mut text = String::new();
    // A line, and no more: the file could be anything a user named so.
    let _ = (&file).take(512).read_to_string(&mut text);
    let written = Holder::read(&text);
    #[cfg(unix)]
    let asked = record::holder(&file);
    #[cfg(not(unix))]
    let asked: Option<Option<i32>> = None;
    drop(file);
    drop(locked);
    match asked {
        // Free now: the holder let go since the refusal.
        Some(None) => None,
        // An id of 0 or less names no process here: a holder in another
        // pid namespace, a container's, whose own id the line holds and
        // this one cannot check, or a lock kept by an open file rather than
        // a process (-1, Linux's OFD locks). Held, and named by nothing.
        Some(Some(pid)) if pid <= 0 => Some(Holder {
            pid: None,
            program: None,
        }),
        Some(Some(pid)) => {
            let pid = pid as u32;
            Some(Holder {
                pid: Some(pid),
                program: written.and_then(|(p, program)| program.filter(|_| p == pid)),
            })
        }
        // The system cannot be asked: the line alone.
        None => written.map(|(pid, program)| Holder {
            pid: Some(pid),
            program,
        }),
    }
}

/// An exclusive record lock over the whole file, not waited for, and the
/// process holding one: `struct flock` and the commands as each system lays
/// them out -- the crate takes no `libc`.
#[cfg(unix)]
mod record {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    #[cfg(target_vendor = "apple")]
    #[repr(C)]
    struct Flock {
        start: i64,
        len: i64,
        pid: i32,
        kind: i16,
        whence: i16,
    }
    #[cfg(target_vendor = "apple")]
    const F_GETLK: i32 = 7;
    #[cfg(target_vendor = "apple")]
    const F_SETLK: i32 = 8;
    #[cfg(target_vendor = "apple")]
    const F_WRLCK: i16 = 3;

    // `off_t` is a `long` in the `struct flock` of `F_SETLK`, 32 bits on a
    // 32-bit Android.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[repr(C)]
    struct Flock {
        kind: i16,
        whence: i16,
        start: isize,
        len: isize,
        pid: i32,
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const F_GETLK: i32 = 5;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const F_SETLK: i32 = 6;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const F_WRLCK: i16 = 1;

    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    const F_UNLCK: i16 = 2;

    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    fn whole() -> Flock {
        Flock {
            kind: F_WRLCK,
            whence: 0,
            start: 0,
            len: 0,
            pid: 0,
        }
    }

    /// Takes the lock, or is refused it.
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    pub fn take(file: &File) -> Result<(), super::Refused> {
        let l = whole();
        match unsafe { fcntl(file.as_raw_fd(), F_SETLK, &l as *const Flock) } {
            0 => Ok(()),
            _ => Err(super::Refused::Held),
        }
    }

    /// The id of the process whose lock would refuse this one's, inside
    /// `None` where none would; `None` where the system could not be asked.
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    pub fn holder(file: &File) -> Option<Option<i32>> {
        let mut l = whole();
        if unsafe { fcntl(file.as_raw_fd(), F_GETLK, &mut l as *mut Flock) } != 0 {
            return None;
        }
        Some(if l.kind == F_UNLCK { None } else { Some(l.pid) })
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    pub fn take(file: &File) -> Result<(), super::Refused> {
        extern "C" {
            fn flock(fd: i32, op: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;
        match unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } {
            0 => Ok(()),
            _ => Err(super::Refused::Held),
        }
    }

    /// An `flock` names no holder.
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    pub fn holder(_: &File) -> Option<Option<i32>> {
        None
    }
}

/// `LockFileEx`'s exclusive lock over one byte, not waited for, let go with
/// `UnlockFileEx`, and a file's identity: kernel32's, declared here -- the
/// crate takes no `windows-sys`, and std's `File::try_lock` locks the whole
/// file. A Windows lock belongs to the handle: a second handle to the file,
/// in this process or another, is refused it, and closing another handle
/// lets none go. It is mandatory as well: a byte under it refuses another
/// handle's read. So the byte is past any line a holder writes ([`AT`]), and
/// a refused opener reads the line beside it ([`holder`](super::holder)).
#[cfg(windows)]
mod record {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    /// `OVERLAPPED`, whose offset is where the locked range starts.
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    /// `BY_HANDLE_FILE_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            len_low: u32,
            len_high: u32,
            at: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            file: *mut c_void,
            reserved: u32,
            len_low: u32,
            len_high: u32,
            at: *mut Overlapped,
        ) -> i32;
        fn GetFileInformationByHandle(file: *mut c_void, info: *mut Info) -> i32;
    }

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    const ERROR_IO_PENDING: i32 = 997;

    /// The byte locked, 1 GiB in, where SQLite takes its locks on Windows
    /// too: past any line, and an offset every Windows file system takes. A
    /// range past the end of a file is lockable.
    const AT: u64 = 1 << 30;

    fn at() -> Overlapped {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: AT as u32,
            offset_high: (AT >> 32) as u32,
            event: std::ptr::null_mut(),
        }
    }

    /// Takes the lock, or is refused it: held by another handle, or the
    /// system's error.
    pub fn take(file: &File) -> Result<(), super::Refused> {
        let mut o = at();
        let flags = LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY;
        // SAFETY: the handle is the file's, open across the call, and `o`
        // outlives it; std opens a file for synchronous I/O, so the call
        // returns only once it is answered, and FAIL_IMMEDIATELY waits for
        // no holder.
        if unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut o) } != 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(ERROR_LOCK_VIOLATION | ERROR_IO_PENDING) => Err(super::Refused::Held),
            _ => Err(super::Refused::Io(e)),
        }
    }

    /// Lets the lock [`take`] took go.
    pub fn release(file: &File) {
        let mut o = at();
        // SAFETY: as in `take`.
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut o) };
    }

    /// The file's volume and index: two names of one file read alike.
    pub fn id(file: &File) -> std::io::Result<(u64, u64)> {
        let mut info = Info::default();
        // SAFETY: the handle is the file's, `info` is the struct the call
        // fills, and both outlive it.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let index = (u64::from(info.index_high) << 32) | u64::from(info.index_low);
        Ok((u64::from(info.volume), index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("fennec-abi-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(format!("{}.lock", path.display()));
        path.to_string_lossy().into_owned()
    }

    /// The line a holder writes reads back as what it wrote; one that is
    /// not such a line names no one.
    #[test]
    fn a_holders_line_reads_back() {
        let me = Holder::this_process();
        let (pid, program) = Holder::read(&me.line()).unwrap();
        assert_eq!(Some(pid), me.pid);
        assert_eq!(program, me.program);
        assert_eq!(
            Holder::read("4242 fennec-server\n"),
            Some((4242, Some("fennec-server".into())))
        );
        assert_eq!(Holder::read("4242\n"), Some((4242, None)));
        for not in ["", "\n", "0 fennec", "fennec-server 4242", "-1 x"] {
            assert_eq!(Holder::read(not), None, "{not:?}");
        }
    }

    /// Held here, the holder is this process, named without the lock file
    /// opened again -- which would have let the lock go: a second hold is
    /// still refused after the asking. Let go, nothing holds it, whatever
    /// line the file still holds.
    #[cfg(any(unix, windows))]
    #[test]
    fn this_processs_own_lock_is_named_and_kept() {
        let p = scratch("own.fennec");
        let held = hold(&p).unwrap();
        let who = holder(&p).expect("a holder");
        assert_eq!(who.pid, Some(std::process::id()));
        assert!(matches!(hold(&p), Err(Refused::Held)));
        drop(held);
        // Read once it is let go: a read of it while held is a descriptor
        // closed, and the lock with it.
        let line = std::fs::read_to_string(format!("{p}.lock")).unwrap();
        assert!(
            line.starts_with(&format!("{} ", std::process::id())),
            "{line:?}"
        );
        #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
        assert_eq!(holder(&p), None);
        drop(hold(&p).unwrap());
    }

    /// What tells this test binary, started again, to hold a file's lock
    /// ([`holds_for_another_process`]).
    #[cfg(any(unix, windows))]
    const HOLD: &str = "FENNEC_ABI_LOCK_HOLD";

    /// What it says once it holds the lock: in a line, not as one, since
    /// the harness writes the test's name before what the test writes.
    #[cfg(any(unix, windows))]
    const HELD: &str = "<the other process holds the lock>";

    /// The other process [`another_process_is_refused_while_one_holds_it`]
    /// starts: this test alone, in this binary run again, holding the lock
    /// of the file `HOLD` names, saying so on stdout, and letting go as its
    /// stdin ends. Run with the rest, it does nothing.
    #[cfg(any(unix, windows))]
    #[test]
    fn holds_for_another_process() {
        let Some(p) = std::env::var_os(HOLD) else {
            return;
        };
        let held = hold(&p).expect("the other process's hold");
        let mut out = std::io::stdout();
        writeln!(out, "{HELD}").and_then(|()| out.flush()).unwrap();
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        drop(held);
    }

    /// Another process holding the file refuses this one's hold, is named
    /// by its id, and lets go as it closes the file or is killed. Before
    /// Windows took a lock, a second process there took the file beside the
    /// first, and both wrote it.
    #[cfg(any(unix, windows))]
    #[test]
    fn another_process_is_refused_while_one_holds_it() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let p = scratch("other.fennec");
        for killed in [false, true] {
            let mut other = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "lock::tests::holds_for_another_process"])
                .args(["--nocapture", "--test-threads=1"])
                .env(HOLD, &p)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut said = BufReader::new(other.stdout.take().unwrap());
            let mut line = String::new();
            while !line.contains(HELD) {
                line.clear();
                let read = said.read_line(&mut line).unwrap();
                assert!(read > 0, "the other process ended before it held the lock");
            }
            assert!(matches!(hold(&p), Err(Refused::Held)), "killed: {killed}");
            assert_eq!(holder(&p).and_then(|h| h.pid), Some(other.id()));
            if killed {
                other.kill().unwrap();
            } else {
                drop(other.stdin.take());
            }
            other.wait().unwrap();
            drop(hold(&p).expect("let go with the other process"));
        }
    }

    #[test]
    fn a_holder_reads_as_a_refusal_names_it() {
        let h = |program: Option<&str>, pid| Holder {
            pid,
            program: program.map(Into::into),
        };
        assert_eq!(
            h(Some("fennec-server"), Some(42)).to_string(),
            "fennec-server, pid 42"
        );
        assert_eq!(h(None, Some(42)).to_string(), "pid 42");
        assert_eq!(h(Some("fennec"), None).to_string(), "fennec");
        assert!(!h(None, None).is_known());
    }

    /// A link to a database -- absolute, relative, a link to a link, one
    /// whose file is not made yet -- and a directory reached through a
    /// link take the target's lock: named as written, a shell through a
    /// link wrote beside a server.
    #[cfg(unix)]
    #[test]
    fn a_file_through_a_link_takes_its_targets_lock() {
        use std::os::unix::fs::symlink;
        let p = scratch("target.fennec");
        std::fs::write(&p, b"").unwrap();
        let dir = std::path::Path::new(&p).parent().unwrap().to_path_buf();
        let at = |n: &str| dir.join(n);
        let links = ["link.fennec", "relative.fennec", "chain.fennec"];
        for n in links.iter().chain(&["linked-dir", "dangling.fennec"]) {
            let _ = std::fs::remove_file(at(n));
        }
        symlink(&p, at("link.fennec")).unwrap();
        symlink("target.fennec", at("relative.fennec")).unwrap();
        symlink("link.fennec", at("chain.fennec")).unwrap();
        symlink(&dir, at("linked-dir")).unwrap();
        symlink("not-yet.fennec", at("dangling.fennec")).unwrap();
        for n in links {
            assert_eq!(lock_path(at(n)), lock_path(&p), "{n}");
        }
        assert_eq!(lock_path(at("dangling.fennec")), at("not-yet.fennec.lock"));
        let held = hold(&p).unwrap();
        let through_dir = at("linked-dir").join("target.fennec");
        for other in links.map(at).into_iter().chain([through_dir]) {
            let refused = matches!(hold(&other), Err(Refused::Held));
            assert!(refused, "{}", other.display());
        }
        drop(held);
        drop(hold(at("chain.fennec")).unwrap());
    }

    /// A hold that may wait takes the lock once this process's holder lets
    /// it go, woken by the drop, and gives up after its wait; one that may
    /// not is refused at once.
    #[test]
    fn a_hold_waits_for_this_process_to_let_go() {
        let p = scratch("waits.fennec");
        let first = hold(&p).unwrap();
        assert!(matches!(hold(&p), Err(Refused::Held)));
        let wait = Duration::from_millis(50);
        let t = Instant::now();
        assert!(matches!(hold_within(&p, wait), Err(Refused::Held)));
        assert!(t.elapsed() >= wait);
        let q = p.clone();
        let waiter =
            std::thread::spawn(move || hold_within(&q, Duration::from_secs(600)).map(drop));
        // Let go only once the hold waits for it.
        while !waiting_for(&p) {
            std::thread::yield_now();
        }
        drop(first);
        assert!(waiter.join().unwrap().is_ok());
        drop(hold(&p).unwrap());
    }

    /// Every method of a sink holding a lock reaches the sink it holds: one
    /// left to the trait's default would run the default's body instead --
    /// `record` an `append`, `flush` a `sync`, `side` none.
    #[test]
    fn a_sink_holding_its_lock_passes_every_call_on() {
        use std::sync::{Arc, Mutex};
        struct Rec(Arc<Mutex<Vec<&'static str>>>);
        impl Rec {
            fn note(&self, what: &'static str) {
                self.0.lock().unwrap().push(what);
            }
            fn ok(&self, what: &'static str) -> CoreResult<()> {
                self.note(what);
                Ok(())
            }
        }
        impl Sink for Rec {
            fn append(&mut self, _: &[u8]) -> CoreResult<()> {
                self.ok("append")
            }
            fn record(&mut self, _: u64, _: &[u8]) -> CoreResult<()> {
                self.ok("record")
            }
            fn append_deferred(&mut self, _: &[u8]) -> CoreResult<()> {
                self.ok("append_deferred")
            }
            fn rewrite(&mut self, _: &[u8]) -> CoreResult<()> {
                self.ok("rewrite")
            }
            fn rewrite_with(
                &mut self,
                _: &mut dyn FnMut(&mut dyn ImageOut) -> CoreResult<()>,
            ) -> CoreResult<()> {
                self.ok("rewrite_with")
            }
            fn remapped(&mut self) -> Option<fennec_core::store::Base> {
                self.note("remapped");
                None
            }
            fn written_through(&mut self) -> CoreResult<Option<fennec_core::store::Base>> {
                self.note("written_through");
                Ok(None)
            }
            fn syncing(&self) -> bool {
                self.note("syncing");
                true
            }
            fn land(&mut self, _: u64, _: &[&[u8]], _: &[u8]) -> CoreResult<()> {
                self.ok("land")
            }
            fn side(&self) -> Option<std::path::PathBuf> {
                self.note("side");
                None
            }
            fn adopt(&mut self, _: &std::path::Path) -> CoreResult<()> {
                self.ok("adopt")
            }
            fn sync_existing(&mut self) -> CoreResult<()> {
                self.ok("sync_existing")
            }
            fn write_out(&mut self) -> CoreResult<()> {
                self.ok("write_out")
            }
            fn write_behind(&mut self, _: bool) -> Option<Durability> {
                self.note("write_behind");
                None
            }
            fn written_all(&self) -> bool {
                self.note("written_all");
                false
            }
            fn sync(&mut self) -> CoreResult<()> {
                self.ok("sync")
            }
            fn flush(&mut self) -> CoreResult<Option<Durability>> {
                self.note("flush");
                Ok(None)
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let p = scratch("holding.fennec");
        let mut s = holding(hold(&p).unwrap())(Box::new(Rec(Arc::clone(&seen)))).unwrap();
        s.append(b"").unwrap();
        s.record(1, b"").unwrap();
        s.append_deferred(b"").unwrap();
        s.rewrite(b"").unwrap();
        s.rewrite_with(&mut |_| Ok(())).unwrap();
        s.remapped();
        s.written_through().unwrap();
        assert!(s.syncing());
        s.land(1, &[], b"").unwrap();
        s.side();
        s.adopt(std::path::Path::new("x")).unwrap();
        s.sync_existing().unwrap();
        s.write_out().unwrap();
        s.write_behind(true);
        assert!(!s.written_all());
        s.sync().unwrap();
        s.flush().unwrap();
        let every = [
            "append",
            "record",
            "append_deferred",
            "rewrite",
            "rewrite_with",
            "remapped",
            "written_through",
            "syncing",
            "land",
            "side",
            "adopt",
            "sync_existing",
            "write_out",
            "write_behind",
            "written_all",
            "sync",
            "flush",
        ];
        assert_eq!(*seen.lock().unwrap(), every);
        // Held while the sink is, and let go with it.
        assert!(matches!(hold(&p), Err(Refused::Held)));
        drop(s);
        drop(hold(&p).unwrap());
    }
}
