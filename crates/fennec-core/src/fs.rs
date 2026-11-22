//! Native file persistence layer.
//!
//! The file's contents are in *the same format* as the engine's in-memory
//! byte image. A write = append to the file. Open = a single replay pass.
//! No separate WAL + data file pair, no checkpoint, no buffer pool of its own.

use crate::engine::{Database, Durability, ImageOut, Sink, MAGIC};
use crate::error::Result;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Write buffer. One `write` call per document meant one syscall per
/// document; that was the dominant cost during bulk loading.
const WRITE_BUF: usize = 1 << 20;

/// What the buffer holds at most while a durability fsyncs the file: past
/// [`WRITE_BUF`] an append writes it unless the disk is busy, and past this
/// whether or not. A 60 ms fsync of a ledger's writes left 0.5 MB.
const WRITE_HELD: usize = 8 << 20;

/// What a write behind the database ([`Sink::write_behind`]) waits for the
/// buffer to hold: a block or a few, a write each. On macOS a write that
/// takes a file's dirty pages past each 8 MB waits for the disk -- 1.5 to
/// 11 ms, the file mapped or not -- and under the write lock, a buffer
/// written as it outgrew [`WRITE_BUF`] held four readers beside blocks of
/// 1 000 writes up to 6 ms every 8 MB.
#[cfg(not(target_arch = "wasm32"))]
const BEHIND_AT: usize = 64 << 10;

/// Appends collect in memory and reach the file only in `sync`, in the
/// durability `flush` hands out, or when they outgrow `WRITE_BUF`.
///
/// Not a `BufWriter` over the file. `flush` runs under the database's
/// exclusive lock, and a `write` there waits for the disk whenever an fsync
/// of the same file is under way on another thread -- on macOS's APFS, 27%
/// of them took 0.5-3.3 ms with eight writers under `--sync always`, the
/// lock held all the while. The bytes are therefore written by whoever
/// runs the fsync, outside the lock.
///
/// That is also where writes arriving together are made durable together.
/// Each durability knows how many bytes had been appended when it was
/// handed out; the first to take the disk writes and fsyncs everything
/// pending, and the ones queued behind it find their bytes already durable
/// and return without an fsync of their own.
pub struct FileSink {
    /// The appends not yet written.
    pending: Arc<Mutex<Vec<u8>>>,
    /// Bytes appended since the file was opened or rewritten.
    appended: u64,
    /// Held for every write and fsync, and taken before `pending` whenever
    /// bytes leave it, so they reach the file in the order they came.
    disk: Arc<Mutex<Disk>>,
    path: PathBuf,
    /// The file mapped with room past its end, which a mapped database's
    /// stores read from and take the records appended since in from
    /// ([`Sink::written_through`]). Let go of when a rewrite puts another
    /// file in its place.
    #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
    mapping: Option<Arc<Mapping>>,
    /// Whether a caller writes the buffer once it has let go of the
    /// database ([`Sink::write_behind`]): an append then leaves it for that
    /// until it holds [`WRITE_HELD`].
    behind: bool,
    /// How far the file is durable ([`Sink::durable`]): what each record
    /// appended claims, set by every fsync that completes, the sync log's
    /// included, to the bytes it covered.
    durable: Arc<AtomicU64>,
    /// Where the file ended, or was durable to if further, once a database
    /// took the sink ([`Sink::set_durable`]) or rewrote it: a close appends
    /// a mark where this sink appended past it and an fsync covered
    /// everything ([`Drop`]) -- a file opened and closed with nothing
    /// written gains no byte. `None` for a sink no database loaded, which
    /// appends nothing of its own.
    marks_past: Option<u64>,
}

struct Disk {
    file: File,
    /// Bytes appended that have been written, and that have been fsynced.
    written: u64,
    synced: u64,
    /// Where the next byte written goes in the file, and the point every
    /// byte before which a completed fsync covered, shared with the sink.
    end: u64,
    durable: Arc<AtomicU64>,
    /// No fsync of this sink has run, and the file holds bytes it did not
    /// write: an earlier process's, which that process's crash may have
    /// left in the page cache alone. A durability ([`Sink::flush`]) fsyncs
    /// them once even where it covers nothing appended -- a replayed
    /// `Idempotency-Key` after a restart answers a write that process
    /// landed (`Database::durability`) -- the file itself, never through
    /// the sync log ([`Disk::sync_inherited`]). A plain sync, a close's,
    /// leaves them: the shell syncs as it exits, after a one-shot read
    /// too, which would fsync for nothing.
    inherited: bool,
    /// fsyncs run, for the test that they are shared.
    #[cfg(test)]
    fsyncs: usize,
    /// A failed write or fsync. The kernel may have dropped the pages it
    /// could not write, so nothing is reported durable after one.
    failed: Option<crate::error::Error>,
    /// What a sync writes into the sync log rather than fsync the file.
    #[cfg(unix)]
    tail: Tail,
}

#[cfg(unix)]
thread_local! {
    static KEEP_LOG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Has the files this thread opens keep a sync log ([`crate::synclog`])
/// where the platform would not -- macOS, whose `F_FULLFSYNC` costs the
/// same either way -- so that a test there goes through it.
#[doc(hidden)]
#[cfg(unix)]
pub fn keep_sync_log(on: bool) {
    KEEP_LOG.with(|k| k.set(on));
}

/// The bytes written since the last sync, which the next one puts into the
/// sync log ([`crate::synclog`]) and syncs there, rather than fsync the
/// file: on ext4 an fsync of a file that grew commits the journal, 356 us
/// in Docker's VM where the log's takes 65.
#[cfg(unix)]
struct Tail {
    /// Whether this sink keeps a log (`synclog::ENABLED`, or a test's).
    keeps: bool,
    log_path: PathBuf,
    /// Opened at the first sync that begins a generation; `None` where it
    /// could not be made, and every sync then fsyncs the file.
    log: Option<crate::synclog::SyncLog>,
    /// The log could not be made: not tried again until the next open,
    /// since each try writes its 260 KB -- on a full disk, at every sync.
    unmade: bool,
    /// The file's length: where the next byte written goes.
    end: u64,
    /// The bytes written since the last sync and where they start, while
    /// they fit an entry.
    since: Vec<u8>,
    since_at: u64,
    /// The next sync fsyncs the file itself and begins a generation: what
    /// was written since outgrew an entry, or the file is new to the sink
    /// -- whose bytes an earlier process may have left unsynced.
    direct: bool,
}

#[cfg(unix)]
impl Tail {
    fn new(path: &Path, end: u64) -> Tail {
        Tail {
            keeps: crate::synclog::ENABLED || KEEP_LOG.with(|k| k.get()),
            log_path: crate::synclog::path_for(path),
            log: None,
            unmade: false,
            end,
            since: Vec::new(),
            since_at: end,
            direct: true,
        }
    }

    /// `bytes` reached the file.
    fn wrote(&mut self, bytes: &[u8]) {
        self.end += bytes.len() as u64;
        if !self.keeps || self.direct {
            return;
        }
        if self.since.len() + bytes.len() > crate::synclog::ENTRY_MAX {
            self.direct = true;
            self.since = Vec::new();
        } else {
            self.since.extend_from_slice(bytes);
        }
    }

    /// The file starts again at `end`, synced: a cut, a rewrite.
    fn reset(&mut self, end: u64) {
        self.end = end;
        self.since.clear();
        self.since_at = end;
        self.direct = true;
    }

    /// Makes what was written since the last sync durable: through the log
    /// while it fits there, else by an fsync of the file, after which the
    /// log begins a generation from where the file now stands.
    fn sync(&mut self, file: &File) -> std::io::Result<()> {
        if self.keeps && !self.direct {
            if self.since.is_empty() {
                return Ok(());
            }
            if let Some(log) = self
                .log
                .as_mut()
                .filter(|l| l.takes(self.since_at, self.since.len()))
            {
                log.append(self.since_at, &self.since)?;
                self.since.clear();
                self.since_at = self.end;
                return Ok(());
            }
        }
        file.sync_data()?;
        self.since.clear();
        self.since_at = self.end;
        if self.keeps {
            if self.log.is_none() && !self.unmade {
                self.log = crate::synclog::SyncLog::open(&self.log_path, file).ok();
                self.unmade = self.log.is_none();
            }
            if let Some(log) = &mut self.log {
                log.begin(file, self.end)?;
                self.direct = false;
            }
        }
        Ok(())
    }

    /// Lets the log go before another file is renamed into this one's
    /// place: the file is synced first, so that nothing is durable only in
    /// the log, and the removal is made durable before the rename, so that
    /// no crash leaves the log beside the file that replaced this one.
    fn forget(&mut self, file: &File) -> std::io::Result<()> {
        if self.log.is_none() && !self.log_path.is_file() {
            return Ok(());
        }
        file.sync_data()?;
        self.log = None;
        remove_synced(&self.log_path)
    }

    /// The database closes: the file is synced and its log removed, so a
    /// node keeps no 260 KB beside each tenant that has closed. A crash
    /// leaves it, and the next open applies it. Whether the file was
    /// synced.
    ///
    /// None is begun after it: a server lets the log go before it exits
    /// ([`Sink::close_log`]), and a durability handed out before that and
    /// run after it -- a `--sync always` write's, its first sync not yet
    /// run -- fsyncs the file; begun again, the log was left beside the
    /// file the exit came after.
    fn close(&mut self, file: &File) -> bool {
        let kept = self.log.take().is_some() || (self.keeps && self.log_path.is_file());
        self.keeps = false;
        let synced = kept && file.sync_data().is_ok();
        if synced {
            let _ = std::fs::remove_file(&self.log_path);
        }
        synced
    }
}

/// The file `path` names once a symbolic link in its place is followed,
/// links to links too: what an open writes, a checkpoint or a compact
/// renames over and `<file>.lock` is named after. Renamed over as given, a
/// link was replaced by the new image -- a regular file of its own, beside
/// which a shell through the link wrote while the server wrote the file
/// the link had led to, and the file opened no more. A directory reached
/// through a link needs nothing, since the files beside go through the
/// same link to the same directory, so the path is resolved no further
/// than its last name: one `lstat` where it is no link. `canonicalize`
/// cost 20 us more an open, and fails for a file not made yet.
pub fn resolved(path: &Path) -> PathBuf {
    let mut real = path.to_path_buf();
    // As many links as the system follows in one path before it gives up.
    for _ in 0..40 {
        match std::fs::symlink_metadata(&real) {
            Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(&real) {
                Ok(to) if to.is_absolute() => real = to,
                Ok(to) => {
                    let dir = real.parent().map(Path::to_path_buf);
                    real = dir.unwrap_or_default().join(to);
                }
                Err(_) => break,
            },
            _ => break,
        }
    }
    real
}

/// `path` with `.suffix` after its whole name: the files a database keeps
/// beside its own. Its extension replaced (`with_extension`), `x.db` and
/// `x.fennec` would share them.
pub fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

/// `options` made to make a file its owner's alone: mode 0600 on a Unix,
/// whatever the umask, set by the `open` that makes it -- never a `chmod`
/// after, which leaves the file a moment in the umask's mode, long enough
/// for another user's `open` to keep a descriptor. Every file fennecdb
/// makes holds rows, secrets or the server's `_` collections, and the
/// umask's 0644 let every user of the machine read them. A file there
/// already keeps the mode it has: an operator's `chmod 640` for a backup
/// group is never narrowed. On Windows a file takes its directory's access
/// list, and this changes nothing (a secret's file is `owner_only`'s,
/// fennec-http). `tools/file_create_check.py` holds every file the crates
/// make to these helpers.
pub fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(options, 0o600);
    options
}

/// `open`'s `O_NOFOLLOW`: the path's last name is opened only where it is
/// no link (`ELOOP` otherwise). Each target's own number, as its libc's
/// `fcntl.h` has it, the way fennec-abi's `F_SETLK` is read: Linux's
/// differs by processor -- the kernel's generic `0o400000`, and
/// `0o100000` where an architecture's own header moves it (arm, arm64,
/// powerpc, m68k) -- the BSDs' and Apple's is `0x100`, illumos' `0x20000`.
/// A Unix not listed fails to build, naming this: a `0` there left every
/// open beside a database following a link, unsaid, as armv7 Android's
/// library did until this list. fennec-server's hand-over opens with it
/// too (`user::sys::AS_FOUND`).
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "s390x",
        target_arch = "loongarch64",
        target_arch = "mips",
        target_arch = "mips64",
        target_arch = "sparc64"
    )
))]
pub const NOFOLLOW: i32 = 0o400000;
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "m68k"
    )
))]
pub const NOFOLLOW: i32 = 0o100000;
#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub const NOFOLLOW: i32 = 0x100;
#[cfg(any(target_os = "illumos", target_os = "solaris"))]
pub const NOFOLLOW: i32 = 0x20000;
#[cfg(all(
    unix,
    not(any(
        all(
            any(target_os = "linux", target_os = "android"),
            any(
                target_arch = "x86",
                target_arch = "x86_64",
                target_arch = "riscv64",
                target_arch = "s390x",
                target_arch = "loongarch64",
                target_arch = "mips",
                target_arch = "mips64",
                target_arch = "sparc64",
                target_arch = "arm",
                target_arch = "aarch64",
                target_arch = "powerpc",
                target_arch = "powerpc64",
                target_arch = "m68k"
            )
        ),
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "illumos",
        target_os = "solaris"
    ))
))]
compile_error!(
    "fennec_core::fs::NOFOLLOW: this Unix's O_NOFOLLOW is not known; add it from its libc's fcntl.h"
);

/// [`private`] for a file kept beside a database and opened again when
/// there -- its sync log, its lock -- never opened through a link put at
/// its name: the open would write, and a mode set land, on whatever file
/// the link names. A side file written whole is [`create_private`]'s.
pub fn private_beside(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(options, NOFOLLOW);
    private(options)
}

/// A side file made [`private`], written from its start and read back: an
/// image, a backup, a record renamed into its place. Made by this open
/// alone (`O_CREAT | O_EXCL`), which follows no link put at its name: one
/// there -- a link, or a file an earlier process left, wider under a
/// release that took the umask's mode -- is removed and the file made
/// anew. Truncated in place, a file kept its mode, and a reader that
/// opened it then read every byte written to it now; opened through a
/// link, the image went into the file the link names, and the mode set on
/// it after with it.
pub fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        private(options.create_new(true));
        match options.open(path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(path)?;
                options.open(path)
            }
            made => made,
        }
    }
    #[cfg(not(unix))]
    options.create(true).truncate(true).open(path)
}

/// [`create_private`] for a small record to be renamed over `like`, given
/// `like`'s mode before a byte is written ([`mode_like`]).
pub fn create_like(path: &Path, like: &Path) -> std::io::Result<File> {
    let file = create_private(path)?;
    mode_like(&file, like)?;
    Ok(file)
}

/// `from` copied into a new file at `to`, made as [`create_private`]
/// makes one, and returned open to read back and to write on at its end:
/// nothing opens `to` again by its name. An archive's image into a
/// restore's `.restoring` or a consolidating round's file (fennec-http's
/// `assemble_into`). On Apple's systems an image no other user may read is
/// cloned ([`cloned`]), APFS sharing its blocks, where a copy writes them
/// all: 1 GiB and its fsync took 0.004-0.008 s cloned and 0.81-1.98 s
/// copied on a Mac (docs/design/durability.md#files-modes).
/// Elsewhere through a 1 MiB buffer, which on Linux is no buffer at all:
/// `io::copy` from a `File` into a `BufWriter<File>` is std's
/// `kernel_copy`, the empty buffer flushed and both descriptors handed to
/// `copy_file_range` -- a reflink where the file system shares blocks --
/// as the system's `fs::copy` hands them. On Windows too it writes through
/// the one handle: a copy opened again after `CopyFileExW` could be held
/// between by a copier sharing no writer (32) -- a sync tool, an indexer
/// -- and a consolidating round's image refused.
pub fn copy_private(from: &Path, to: &Path) -> std::io::Result<File> {
    let mut source = File::open(from)?;
    #[cfg(target_vendor = "apple")]
    if let Some(mut clone) = cloned(&source, to)? {
        clone.seek(SeekFrom::End(0))?;
        return Ok(clone);
    }
    let mut out = BufWriter::with_capacity(WRITE_BUF, create_private(to)?);
    std::io::copy(&mut source, &mut out)?;
    out.into_inner()
        .map_err(std::io::IntoInnerError::into_error)
}

/// `source` cloned to `to` (`fclonefileat`), the clone open to read and
/// write, or `None` where it is to be copied instead. Only an image whose
/// mode gives its owner read and write and no one else anything: a clone
/// is made with its source's mode, so a wider image's -- one 0.2.0 made
/// with the umask's `0644` -- would be another user's to open until an
/// `fchmod`, long enough to keep a descriptor reading every write after,
/// and a `0400` one's could not be opened to be written. With
/// `CLONE_NOFOLLOW` a link at `to` is `EEXIST` as any file there is,
/// removed and the clone made again once, as `create_private` does;
/// without it the clone follows one that names no file and is made where
/// it points (macOS 26.1), which the system's `fs::copy`, passing no flag,
/// did. `CLONE_NOOWNERCOPY`: made by root, the clone is root's, as a file
/// root makes is, not the image's owner's. Another file system
/// (`ENOTSUP`), another volume (`EXDEV`) or a system without a flag
/// (`EINVAL`) copies instead. The clone gives back no descriptor, so the
/// name is opened again, as no link (`O_NOFOLLOW`), and held to what the
/// clone made -- one regular file of one name, the process's, on the
/// image's volume and of its length -- and to the name ([`still_named`]):
/// another file a writer of the directory put there between is refused,
/// but for one of the user's own of that length renamed there from beside
/// it, a writer the database's directory trusts anyway (SECURITY.md).
#[cfg(target_vendor = "apple")]
fn cloned(source: &File, to: &Path) -> std::io::Result<Option<File>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    extern "C" {
        fn fclonefileat(src: i32, dst_dir: i32, dst: *const std::ffi::c_char, flags: u32) -> i32;
        fn geteuid() -> u32;
    }
    // <sys/fcntl.h>, <sys/clonefile.h> and <sys/errno.h>, the same on
    // every Apple system.
    const AT_FDCWD: i32 = -2;
    const CLONE_NOFOLLOW: u32 = 0x1;
    const CLONE_NOOWNERCOPY: u32 = 0x2;
    const EEXIST: i32 = 17;
    const EXDEV: i32 = 18;
    const EINVAL: i32 = 22;
    const ENOTSUP: i32 = 45;
    let image = source.metadata()?;
    if !image.is_file() || image.mode() & 0o677 != 0o600 {
        return Ok(None);
    }
    let name = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    let mut again = true;
    // SAFETY: a descriptor open across the call and a string it only reads.
    while unsafe {
        fclonefileat(
            source.as_raw_fd(),
            AT_FDCWD,
            name.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
        )
    } != 0
    {
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(EEXIST) if again => {
                again = false;
                match std::fs::remove_file(to) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            Some(ENOTSUP | EXDEV | EINVAL) => return Ok(None),
            _ => return Err(e),
        }
    }
    let clone = private_beside(OpenOptions::new().read(true).write(true)).open(to)?;
    let made = clone.metadata()?;
    // SAFETY: no argument, and it cannot fail.
    let me = unsafe { geteuid() };
    if !made.is_file()
        || made.nlink() != 1
        || made.uid() != me
        || (made.dev(), made.len()) != (image.dev(), image.len())
    {
        return Err(std::io::Error::other(format!(
            "{} is not the clone made of the image: another file was put at its name",
            to.display()
        )));
    }
    still_named(&clone, to)?;
    if made.mode() & 0o777 != 0o600 {
        clone.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(test)]
    CLONED_HERE.with(|c| c.set(c.get() + 1));
    Ok(Some(clone))
}

#[cfg(all(test, target_vendor = "apple"))]
thread_local! {
    /// The images [`cloned`] cloned on this thread, for a test that holds
    /// [`copy_private`] to its clone.
    static CLONED_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The permission bits a file `new` made by this process takes from
/// `like`, the file it is to stand for: `like`'s, but for a group's where
/// `new`'s group is another -- the process's or its directory's, where
/// `chgrp backup` gave `like` the backup group, whose read would go to
/// every member of the other: on macOS a directory's group, often `staff`,
/// every local user. `None` where `like` is not the process's user's, as
/// `new` is: a file another user put at a name fennecdb writes -- a link
/// in a shared directory -- lends it nothing (G14's review, F5). Set-id
/// bits are no data file's.
#[cfg(unix)]
pub fn kept_mode(new: &std::fs::Metadata, like: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    if like.uid() != new.uid() {
        return None;
    }
    let mode = like.mode() & 0o777;
    Some(match like.gid() == new.gid() {
        true => mode,
        false => mode & !0o070,
    })
}

/// Gives `new`, a file open to be renamed over `like`, the mode `like`
/// has ([`kept_mode`]): a file replaced -- by a checkpoint, a compact, a
/// restore or a backup with `--force`, a tenant's record -- keeps the mode
/// an operator set on it, which would otherwise go with the inode the
/// rename unlinks. On the open file (`fchmod`), never by its name, which a
/// link could have moved. Owner and group are the rename's, never chowned:
/// a group kept across a rewrite is a setgid directory's
/// (docs/design/durability.md#files-modes). Nothing where `like` is not
/// there: `new` stays its owner's alone. Nothing on Windows, where a
/// renamed file keeps the access list it was made with.
pub fn mode_like(new: &File, like: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let like = match std::fs::metadata(like) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let made = new.metadata()?;
        // Set only where it differs. A `chmod` is a metadata write the
        // file system journals: on APFS it cost each compact of the perf
        // gate's 10 000 rows some 100 000 instructions of the kernel's,
        // 1-2%, where an `fstat` costs next to nothing, and a database its
        // owner's alone -- the usual one -- has the image's mode already.
        if let Some(mode) = kept_mode(&made, &like) {
            if made.permissions().mode() & 0o777 != mode {
                new.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (new, like);
    Ok(())
}

/// Fails where `path` no longer names, as no link, the file `file` is
/// open on: what a rename by that name would move is then another file
/// than the one written through `file` -- a link or a file put there by a
/// writer of its directory since -- and is not renamed into a database's
/// place. An `fstat` and an `lstat`, a rewrite: `swap_in`'s, and before
/// each rename of fennec-http's archive, a backup and an unseal.
#[cfg(unix)]
pub fn still_named(file: &File, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let (open, there) = (file.metadata()?, std::fs::symlink_metadata(path)?);
    if (open.dev(), open.ino()) != (there.dev(), there.ino()) {
        return Err(std::io::Error::other(format!(
            "{} is no longer the file written: another was put at its name, which is not \
             renamed into the database's place",
            path.display()
        )));
    }
    Ok(())
}

/// `path` and the directories above it that are missing, each made its
/// owner's alone (0700 on a Unix) as [`private`] makes a file: a node's
/// tenants, an archive. One there already keeps its mode.
pub fn create_dir_private(path: &Path) -> std::io::Result<()> {
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(path)
}

/// One new directory at `path`, its owner's alone (0700 on a Unix): one
/// there already, or a link at the name, is refused (`AlreadyExists`)
/// rather than used. A directory of a process's own in one every user
/// writes -- `fennec verify`'s in the temp directory -- where a name made
/// by `recursive` or a file's open could be another user's, put there
/// first.
pub fn create_dir_new(path: &Path) -> std::io::Result<()> {
    // Borrowed mutably, not bound `mut`: only a Unix sets the mode, and a
    // `mut` no other build uses is denied there (`unused_mut`).
    let dir = &mut std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(dir, 0o700);
    dir.create(path)
}

/// Removes the file at `path`, if there is one, and makes its removal
/// durable.
#[cfg(unix)]
fn remove_synced(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_dir(path).map_err(|e| std::io::Error::other(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes the sync log beside `path` ([`crate::synclog`]), durably: what
/// renames another file into a database's place -- a restore, a backup
/// unsealed, a tenant's import -- does it first, so that the log of the
/// file that was there is not applied to the new one. Nothing where the
/// platform keeps no logs.
pub fn forget_sync_log(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return remove_synced(&crate::synclog::path_for(&resolved(path)));
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Disk {
    /// Writes whatever is pending.
    fn write_pending(&mut self, pending: &Mutex<Vec<u8>>) -> Result<()> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let bytes = std::mem::take(&mut *lock(pending));
        if let Err(e) = self.file.write_all(&bytes) {
            self.failed = Some(e.into());
            return Err(self.failed.clone().unwrap());
        }
        self.written += bytes.len() as u64;
        self.end += bytes.len() as u64;
        #[cfg(unix)]
        self.tail.wrote(&bytes);
        Ok(())
    }

    /// Writes whatever is pending and then `bytes`, which do not go through
    /// the buffer.
    fn write_through(&mut self, pending: &Mutex<Vec<u8>>, bytes: &[u8]) -> Result<()> {
        self.write_pending(pending)?;
        if let Err(e) = self.file.write_all(bytes) {
            self.failed = Some(e.into());
            return Err(self.failed.clone().unwrap());
        }
        self.written += bytes.len() as u64;
        self.end += bytes.len() as u64;
        #[cfg(unix)]
        self.tail.wrote(bytes);
        Ok(())
    }

    /// Makes the first `upto` appended bytes durable, unless they are --
    /// and, `inherited` asked, the bytes the file held before them, once.
    /// What `inherited` costs a durability: where its bytes are covered,
    /// the test of it below (and of the flag where it is asked); where it
    /// fsyncs, the store clearing the flag. A plain sync passes `false`.
    fn sync(&mut self, pending: &Mutex<Vec<u8>>, upto: u64, inherited: bool) -> Result<()> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        if self.synced >= upto {
            if inherited && self.inherited {
                return self.sync_inherited();
            }
            return Ok(());
        }
        self.write_pending(pending)?;
        #[cfg(unix)]
        let synced = self.tail.sync(&self.file);
        #[cfg(not(unix))]
        let synced = self.file.sync_data();
        if let Err(e) = synced {
            self.failed = Some(e.into());
            return Err(self.failed.clone().unwrap());
        }
        #[cfg(test)]
        {
            self.fsyncs += 1;
            FSYNCS_HERE.with(|n| n.set(n.get() + 1));
        }
        self.synced = self.written;
        self.inherited = false;
        self.durable.store(self.end, Ordering::Release);
        Ok(())
    }

    /// The bytes an earlier process left, fsynced by a durability that
    /// covers nothing appended: the file itself, as the first sync of an
    /// open does, but with no sync log begun -- the log is for the bytes
    /// this sink writes, and an app that opened its file and wrote
    /// nothing, whose `fennec_sync` lands here, made 260 KB of it on
    /// Android for this. What was written meanwhile is covered too, the
    /// fsync being the whole file's; the log's generation begins at the
    /// next sync, which still fsyncs the file (`Tail::direct`, set until
    /// a sync runs, as `inherited` is).
    #[cold]
    fn sync_inherited(&mut self) -> Result<()> {
        if let Err(e) = self.file.sync_data() {
            self.failed = Some(e.into());
            return Err(self.failed.clone().unwrap());
        }
        #[cfg(test)]
        {
            self.fsyncs += 1;
            FSYNCS_HERE.with(|n| n.set(n.get() + 1));
        }
        self.synced = self.written;
        self.inherited = false;
        self.durable.store(self.end, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    /// The fsyncs a file's sink ran on this thread, for a test that opens
    /// a database and holds no sink of its own to count them on.
    static FSYNCS_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl FileSink {
    /// Renames `from`, a whole image fsynced and open as `image`, over the
    /// file and appends to it from then on. Whatever was pending is in the
    /// image: written after it, it would be there twice. A failure after
    /// the rename leaves the sink failed, so nothing waiting is told its
    /// write is durable.
    fn swap_in(
        &self,
        disk: &mut Disk,
        #[cfg(not(windows))] image: File,
        from: &Path,
    ) -> Result<()> {
        let swapped = (|| -> Result<File> {
            // The handle that wrote the image, or one that followed no link
            // to it ([`Sink::adopt`]), and the file the sink appends to
            // after: it takes the mode of the file it replaces on this
            // handle (`mode_like`), so an operator's `chmod 640` lasts
            // through every compact. Opened again by its name, as up to
            // G14's review, a link put at `.compacting` meanwhile led the
            // open, the `fchmod` and every append after to the file it
            // named.
            #[cfg(not(windows))]
            let mut f = image;
            #[cfg(not(windows))]
            mode_like(&f, &self.path)?;
            // The rename goes by the name: one another file was put at
            // since is renamed over nothing.
            #[cfg(unix)]
            still_named(&f, from)?;
            #[cfg(unix)]
            disk.tail.forget(&disk.file)?;
            #[cfg(not(windows))]
            std::fs::rename(from, &self.path)?;
            #[cfg(not(windows))]
            sync_dir(&self.path)?;
            // On Windows the rename is tried again while another handle
            // refuses it, and the file flushed through the handle opened
            // here before its directory is (docs/design/windows.md).
            #[cfg(windows)]
            let mut f = windows::swap(from, &self.path)?;
            f.seek(SeekFrom::End(0))?;
            Ok(f)
        })();
        lock(&self.pending).clear();
        match swapped {
            Ok(f) => {
                // Everything appended so far is in the image, and the image
                // is on disk.
                let len = f.metadata().map_or(0, |m| m.len());
                #[cfg(unix)]
                disk.tail.reset(len);
                disk.file = f;
                disk.written = self.appended;
                disk.synced = self.appended;
                disk.inherited = false;
                // The image is on disk whole, and nothing after it yet.
                disk.end = len;
                self.durable.store(len, Ordering::Release);
                Ok(())
            }
            Err(e) => {
                disk.failed = Some(e.clone());
                Err(e)
            }
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<(FileSink, Vec<u8>)> {
        let (mut file, path) = FileSink::create(path)?;
        let mut existing = Vec::new();
        file.read_to_end(&mut existing)?;
        file.seek(SeekFrom::End(0))?;
        Ok((FileSink::over(file, path), existing))
    }

    /// Opens the file for reading and appending, creating it with the magic
    /// alone when it is missing or empty.
    fn create(path: impl AsRef<Path>) -> Result<(File, PathBuf)> {
        // The file a link leads to, kept as the sink's path: every rewrite
        // renames over it, leaving the link a link.
        let path = resolved(path.as_ref());
        // An existing file is the database: never truncated, its mode kept.
        let mut file = private(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false),
        )
        .open(&path)?;
        if file.metadata()?.len() == 0 {
            // A log left beside a file of that name is not this one's.
            #[cfg(unix)]
            let _ = std::fs::remove_file(crate::synclog::path_for(&path));
            // Out of the search indexer's reach, one handle fewer that may
            // refuse a rename over it.
            #[cfg(windows)]
            windows::mark_unindexed(&file);
            file.write_all(&MAGIC[..])?;
            file.flush()?;
            file.seek(SeekFrom::Start(0))?;
        } else {
            // What the last process made durable through the sync log and
            // the machine lost from the file goes back before it is read.
            #[cfg(unix)]
            crate::synclog::recover(&file, &crate::synclog::path_for(&path))?;
        }
        Ok((file, path))
    }

    /// A sink appending to `file`, which is positioned at its end.
    fn over(file: File, path: PathBuf) -> FileSink {
        let end = file.metadata().map_or(0, |m| m.len());
        let durable = Arc::new(AtomicU64::new(0));
        FileSink {
            pending: Arc::new(Mutex::new(Vec::new())),
            appended: 0,
            disk: Arc::new(Mutex::new(Disk {
                file,
                written: 0,
                synced: 0,
                end,
                durable: Arc::clone(&durable),
                inherited: true,
                #[cfg(test)]
                fsyncs: 0,
                failed: None,
                #[cfg(unix)]
                tail: Tail::new(&path, end),
            })),
            path,
            #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
            mapping: None,
            behind: false,
            durable,
            marks_past: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cuts the file back to its first `len` bytes, before anything is
    /// appended: past them is a record a crash left cut short (see
    /// [`Database::load`]). Left there, it swallowed the first write
    /// appended after it -- acknowledged, and gone on the next open.
    /// `marked` (`Database::marks_cut`), a close's mark ends the records
    /// kept (`engine::synced_mark`): they are durable once this returns,
    /// and the mark claims them, so the next open reads the file as this
    /// one did. The records that claimed how far it was durable may have
    /// gone with the tail: left unmarked, the next open found it durable
    /// only as far as the records kept claim, and cut a synced record the
    /// disk had flipped a bit of, which this open read unchecked below that
    /// point, with every record after it
    /// (docs/design/storage.md#file-format). Where the file now ends, past
    /// the mark.
    ///
    /// Three fsyncs, each before what would claim more than the disk holds
    /// without it -- the pages one fsync covers reach the disk in no set
    /// order, a file's length among them: the file first, the earlier
    /// process's pages and the torn tail with them, before a mark says they
    /// are durable (one fsync after the mark, the mark kept and an earlier
    /// page lost, read that page's zeros as the rows it held); the mark,
    /// written over the torn record's first bytes, before the cut (cut and
    /// marked under one fsync, the shorter length kept and the mark lost,
    /// the file was an unmarked cut again); the cut last. A power cut after
    /// any step leaves the torn file, the mark followed by the tail it
    /// heads, or the file cut and marked, each read as this open read it,
    /// the next open that writes it leaving it as this one does (a cut
    /// right after a whole mark is not marked again). The mark's eight
    /// bytes torn across two sectors leave no frame at the cut: the records
    /// kept alone then say how far the file was durable, and a row below
    /// this open's point failing its sum is refused by the records past the
    /// cut, still there (docs/design/storage.md, *The cut's order*).
    pub fn cut(&mut self, len: usize, marked: bool) -> Result<u64> {
        // Out of the mapping first, which then covers no byte past the
        // file's end: a page wholly past it is the process's end to read
        // (`SIGBUS`), and the appends after the cut land in those bytes, in
        // place, under every slice of the mapping a reader takes
        // (`Mapping::as_ref`). Only an open cuts, before anything reads.
        // The mark is past what it covers too: no record points there.
        #[cfg(all(unix, target_pointer_width = "64"))]
        if let Some(m) = &self.mapping {
            m.cover(len);
        }
        let mut disk = lock(&self.disk);
        let end = match marked {
            // No claim decides what the next open reads of a file of no
            // sums, nor of one a whole mark ends the records of: the cut
            // and its fsync.
            false => {
                cut_file(&disk.file, len as u64)?;
                synced_step(&disk.file)?;
                len as u64
            }
            true => {
                let mark = crate::engine::synced_mark();
                let end = (len + mark.len()) as u64;
                synced_step(&disk.file)?;
                disk.file.seek(SeekFrom::Start(len as u64))?;
                disk.file.write_all(&mark)?;
                #[cfg(test)]
                stepped(CutStep::Write(len as u64, mark.clone()));
                synced_step(&disk.file)?;
                cut_file(&disk.file, end)?;
                synced_step(&disk.file)?;
                end
            }
        };
        disk.file.seek(SeekFrom::End(0))?;
        // The file's fsync, what the earlier process left among it.
        disk.inherited = false;
        disk.end = end;
        self.durable.store(end, Ordering::Release);
        #[cfg(unix)]
        disk.tail.reset(end);
        Ok(end)
    }
}

/// `file` cut, or grown, to `len` bytes ([`FileSink::cut`]). Windows cuts
/// no file a view of it maps, and says so by name (`windows::cut_refused`).
fn cut_file(file: &File, len: u64) -> Result<()> {
    let cut = file.set_len(len);
    #[cfg(test)]
    if cut.is_ok() {
        stepped(CutStep::SetLen(len));
    }
    match cut {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(e) => Err(windows::cut_refused(e)),
        #[cfg(not(windows))]
        Err(e) => Err(e.into()),
    }
}

/// `file`'s fsync, a step of [`FileSink::cut`].
fn synced_step(file: &File) -> Result<()> {
    file.sync_data()?;
    #[cfg(test)]
    stepped(CutStep::Sync);
    Ok(())
}

/// A step of [`FileSink::cut`] that completed, as a test replays what a
/// power cut after each leaves on the disk.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum CutStep {
    Sync,
    Write(u64, Vec<u8>),
    SetLen(u64),
}

#[cfg(test)]
thread_local! {
    /// The steps of the cuts this thread made since it last took them.
    static CUT_STEPS: std::cell::RefCell<Vec<CutStep>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn stepped(step: CutStep) {
    CUT_STEPS.with(|s| s.borrow_mut().push(step));
}

impl FileSink {
    /// Another file took this one's place, durable to its end: a close
    /// marks it only once an fsync covered more.
    fn rewritten(&mut self) {
        if let Some(past) = &mut self.marks_past {
            *past = self.durable.load(Ordering::Acquire);
        }
    }

    /// The buffer holding `held` bytes after an append: written behind the
    /// database -- by the caller once it lets the lock go -- unless that
    /// has fallen behind by `WRITE_HELD`.
    #[inline(always)]
    fn outgrown(&mut self, held: usize) -> Result<()> {
        if held >= WRITE_BUF && !(self.behind && held < WRITE_HELD) {
            // Not while a durability fsyncs the file, whose writes wait for
            // it: this runs under the database's lock, and every request
            // would wait out the fsync too. The first append past the
            // buffer once it is done writes them, or the next durability;
            // past `WRITE_HELD` they wait it out here.
            let disk = match self.disk.try_lock() {
                Ok(disk) => Some(disk),
                Err(std::sync::TryLockError::WouldBlock) if held < WRITE_HELD => None,
                Err(std::sync::TryLockError::WouldBlock) => Some(lock(&self.disk)),
                Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            };
            if let Some(mut disk) = disk {
                disk.write_pending(&self.pending)?;
            }
        }
        Ok(())
    }
}

impl Sink for FileSink {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        self.appended += bytes.len() as u64;
        // A record the buffer could not hold is written where it lies, as
        // the buffer would have been once it held it: copied in first, a
        // block of 50 000 768-dim rows was 154 MB more of heap for the
        // time of the write, which macOS's allocator then kept.
        if bytes.len() >= WRITE_BUF {
            return lock(&self.disk).write_through(&self.pending, bytes);
        }
        let held = {
            let mut pending = lock(&self.pending);
            pending.extend_from_slice(bytes);
            pending.len()
        };
        self.outgrown(held)
    }
    /// A replica's record, its claim this file's, as its parts: appended
    /// under one lock -- the buffer's, or the disk's for a record the
    /// buffer cannot hold -- so that no durability writes the file between
    /// them. A syncer takes its durability under the database's lock and
    /// runs it after: appended a part at a time, its fsync could write out
    /// the head and body alone and store the durable point inside the
    /// record, which every record after it then claimed, and a power cut
    /// that took the trailer alone refused the open where it cuts
    /// (`a_record_in_parts_is_never_split_by_a_sync`). Apart from
    /// [`Self::append`], which a lone write's record takes: a loop over
    /// one part there cost every write.
    fn record_parts(&mut self, _seq: u64, _original: &[u8], parts: &[&[u8]]) -> Result<()> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        self.appended += len as u64;
        if len >= WRITE_BUF {
            let mut disk = lock(&self.disk);
            for p in parts {
                disk.write_through(&self.pending, p)?;
            }
            return Ok(());
        }
        let held = {
            let mut pending = lock(&self.pending);
            for p in parts {
                pending.extend_from_slice(p);
            }
            pending.len()
        };
        self.outgrown(held)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn durable(&self) -> Option<Arc<AtomicU64>> {
        Some(Arc::clone(&self.durable))
    }
    fn set_durable(&mut self, at: u64) {
        self.durable.fetch_max(at, Ordering::AcqRel);
        let end = lock(&self.disk).end;
        self.marks_past = Some(self.durable.load(Ordering::Acquire).max(end));
    }
    /// Pending until the durability runs, however large: written here, a
    /// kept graph's record waited out any fsync under way as well.
    fn append_deferred(&mut self, bytes: &[u8]) -> Result<()> {
        self.appended += bytes.len() as u64;
        lock(&self.pending).extend_from_slice(bytes);
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> Result<()> {
        self.rewrite_with(&mut |out| out.write(bytes))
    }

    /// The image goes to a side file that is renamed over the database, so
    /// until the rename the old file stands whole: an image that cannot be
    /// written -- a full disk during `compact` -- leaves the appends still
    /// pending there to reach the old file, and a durability waiting on them
    /// finds them on disk. They were cleared before the image once, and a
    /// failed one then let `--sync always` answer "durable" for a write in
    /// neither file. From the rename on, the new file holds them; a failure
    /// after it leaves the sink failed, so nothing waiting is told otherwise.
    fn rewrite_with(
        &mut self,
        image: &mut dyn FnMut(&mut dyn ImageOut) -> Result<()>,
    ) -> Result<()> {
        let mut disk = lock(&self.disk);
        if let Some(e) = &disk.failed {
            return Err(e.clone());
        }
        let tmp = beside(&self.path, "compacting");
        let written = (|| -> Result<File> {
            let mut out = FileImage {
                w: BufWriter::with_capacity(WRITE_BUF, create_private(&tmp)?),
                at: 0,
            };
            image(&mut out)?;
            let file = out
                .w
                .into_inner()
                .map_err(|e| crate::error::Error::Io(e.to_string()))?;
            file.sync_all()?;
            Ok(file)
        })();
        let written = match written {
            Ok(file) => file,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
        {
            self.mapping = None;
        }
        // Renamed in through the handle that wrote it, never opened again
        // by its name (`swap_in`).
        #[cfg(not(windows))]
        self.swap_in(&mut disk, written, &tmp)?;
        // Windows' swap opens it again itself, once this handle is closed.
        #[cfg(windows)]
        {
            drop(written);
            self.swap_in(&mut disk, &tmp)?;
        }
        drop(disk);
        self.rewritten();
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn side(&self) -> Option<PathBuf> {
        Some(beside(&self.path, "beside"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn adopt(&mut self, side: &Path) -> Result<()> {
        let mut disk = lock(&self.disk);
        if let Some(e) = &disk.failed {
            return Err(e.clone());
        }
        // Opened where it is no link (`O_NOFOLLOW`): the side file's
        // handle stays with its writer, and a link put at `.beside` since
        // is refused here, the database as it was, rather than followed.
        #[cfg(not(windows))]
        let image = private_beside(OpenOptions::new().read(true).write(true)).open(side)?;
        #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
        {
            self.mapping = None;
        }
        #[cfg(not(windows))]
        self.swap_in(&mut disk, image, side)?;
        #[cfg(windows)]
        self.swap_in(&mut disk, side)?;
        drop(disk);
        self.rewritten();
        Ok(())
    }
    /// The file as it stands, mapped read-only, with room past its end:
    /// what a mapped database points its stores at after a rewrite. The
    /// mapping it had covers the file the rename replaced, which is
    /// unlinked and goes when the last location pointing into it does.
    #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
    fn remapped(&mut self) -> Option<crate::store::Base> {
        let disk = lock(&self.disk);
        let len = disk.file.metadata().ok()?.len() as usize;
        if len == 0 {
            return None;
        }
        let m = Arc::new(Mapping::with_room(&disk.file, len).ok()?);
        self.mapping = Some(m.clone());
        Some(m)
    }

    /// Writes what is pending -- under the database's lock, where the
    /// appends leave it to the durability, but only once a handover is due
    /// -- and hands back the file mapped as far as it now goes: the mapping
    /// the stores read from, grown over the appends, or where they outgrew
    /// its room a new one.
    #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
    fn written_through(&mut self) -> Result<Option<crate::store::Base>> {
        let mut disk = lock(&self.disk);
        disk.write_pending(&self.pending)?;
        let len = disk.file.metadata()?.len() as usize;
        if let Some(m) = self.mapping.as_ref().filter(|m| m.cover(len)) {
            return Ok(Some(m.clone()));
        }
        let m = Arc::new(Mapping::with_room(&disk.file, len)?);
        self.mapping = Some(m.clone());
        Ok(Some(m))
    }

    /// The disk is held by whoever writes or fsyncs the file, and nothing
    /// but a durability takes it without the database's lock: held, one is
    /// under way.
    #[cfg(not(target_arch = "wasm32"))]
    fn syncing(&self) -> bool {
        matches!(
            self.disk.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }

    /// Pushes the whole file to disk, the bytes an earlier process wrote
    /// and never synced included. After a crash of the process alone they
    /// are in the file but may be only in the kernel's cache: a primary
    /// calls this before it tells a replica they exist.
    /// The pending appends written, and no fsync: past the process's end,
    /// in the kernel's cache.
    #[cfg(not(target_arch = "wasm32"))]
    fn write_out(&mut self) -> Result<()> {
        lock(&self.disk).write_pending(&self.pending)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_behind(&mut self, all: bool) -> Option<Durability> {
        self.behind = true;
        let held = lock(&self.pending).len();
        if held == 0 || (held < BEHIND_AT && !all) {
            return None;
        }
        let (disk, pending) = (Arc::clone(&self.disk), Arc::clone(&self.pending));
        // Not while a durability fsyncs the file: it writes them itself, and
        // the request this runs for, a buffered write, would wait out the
        // fsync -- a ledger's transfer beside the syncer's waited 126 ms.
        // Left, they are the next write behind's.
        Some(Box::new(move || match disk.try_lock() {
            Ok(mut disk) => disk.write_pending(&pending),
            Err(std::sync::TryLockError::WouldBlock) => Ok(()),
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().write_pending(&pending),
        }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn written_all(&self) -> bool {
        lock(&self.pending).is_empty()
    }

    fn sync_existing(&mut self) -> Result<()> {
        let mut disk = lock(&self.disk);
        disk.file.sync_data()?;
        disk.inherited = false;
        self.durable.store(disk.end, Ordering::Release);
        Ok(())
    }

    /// Writes the pending appends and pushes them to disk. Because of the
    /// buffer, the last writes can be lost if the process dies before `sync`
    /// is called; fennecdb never fsyncs every write anyway -- this buffer
    /// extends that model.
    fn sync(&mut self) -> Result<()> {
        lock(&self.disk).sync(&self.pending, self.appended, false)
    }
    /// Touches no file: the write and the fsync are both the durability's,
    /// which runs without the database's lock. It covers what an earlier
    /// process left in the file as well (`Disk::inherited`): what an
    /// answer waits for covers every byte a request may have been shown.
    fn flush(&mut self) -> Result<Option<Durability>> {
        let (disk, pending, upto) = (
            Arc::clone(&self.disk),
            Arc::clone(&self.pending),
            self.appended,
        );
        Ok(Some(Box::new(move || {
            lock(&disk).sync(&pending, upto, true)
        })))
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn close_log(&mut self) {
        #[cfg(unix)]
        self.let_log_go(&mut lock(&self.disk));
    }
}

/// The image written straight into the side file a rewrite renames over the
/// database: `at` counts what went in, and a patch seeks back to it.
struct FileImage {
    w: BufWriter<File>,
    at: u64,
}

impl ImageOut for FileImage {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.w.write_all(bytes)?;
        self.at += bytes.len() as u64;
        Ok(())
    }

    fn at(&self) -> u64 {
        self.at
    }

    fn patch(&mut self, at: u64, bytes: &[u8]) -> Result<()> {
        // What is buffered has to be in the file before the seek moves off
        // its end.
        self.w.flush()?;
        let f = self.w.get_mut();
        f.seek(SeekFrom::Start(at))?;
        f.write_all(bytes)?;
        f.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        // Do not let buffered leftovers vanish silently.
        let mut disk = lock(&self.disk);
        let written = disk.write_pending(&self.pending);
        #[cfg(unix)]
        if written.is_ok() {
            self.let_log_go(&mut disk);
        }
        // Everything written is durable, and more than the open found: a
        // mark says so, and the next open checks nothing behind it -- a
        // bulk load synced once at its end left every record claiming the
        // open's point, and each open after checked them all. No fsync of
        // its own: lost, the claims before it stand.
        let durable = self.durable.load(Ordering::Acquire);
        if written.is_ok()
            && disk.failed.is_none()
            && durable == disk.end
            && self.marks_past.is_some_and(|past| durable > past)
        {
            let _ = disk.file.write_all(&crate::engine::synced_mark());
        }
    }
}

impl FileSink {
    /// The file synced and its sync log removed, as a close does, and none
    /// begun after ([`Tail::close`]): the drop's, and a server's before it
    /// exits ([`Sink::close_log`]). Nothing after a storage error, whose
    /// log may hold what the file lacks.
    #[cfg(unix)]
    fn let_log_go(&self, disk: &mut Disk) {
        if disk.failed.is_some() {
            return;
        }
        let Disk {
            tail,
            file,
            end,
            written,
            synced,
            inherited,
            ..
        } = disk;
        if tail.close(file) {
            // That fsync covered every byte written: a durability handed
            // out before the close and run after it finds its bytes synced.
            *synced = *written;
            *inherited = false;
            self.durable.store(*end, Ordering::Release);
        }
    }
}

/// A file mapped read-only into the address space: its pages come in as
/// they are read and can be dropped again by the operating system, which
/// writes nothing back -- they are the file's. `mmap` is declared here
/// rather than taken from a crate, as `signal` is in fennec-server.
///
/// What a mapping covers is never cut or written in place: writes append
/// past the mapped length, a rewrite (`compact`, `checkpoint`) renames a
/// new file over it, so the mapping keeps the old one's pages until it is
/// dropped, and the one cut, a torn tail at the open, takes the bytes past
/// it out of the mapping first ([`FileSink::cut`]) -- a reader's that takes
/// no lock reaches no page it cuts ([`Mapping::reading`]). A compact beside
/// the database patches its side file's head in place, before that file's
/// stores are the database's (`Database::finish_beside`).
///
/// A database's own file is mapped with room past its end (`with_room`),
/// which the appends grow into: the pages a write adds to the file are the
/// mapping's once the write is made, on Linux and on macOS alike, so the
/// records written since the open are taken in where they lie
/// ([`Database::hand_over`]) rather than through a new mapping, whose pages
/// every reader would fault in again -- and the old one's torn down under
/// the write lock.
#[cfg(all(unix, target_pointer_width = "64"))]
pub struct Mapping {
    ptr: *mut u8,
    /// What is mapped: the file's length when it was, and the room.
    reserved: usize,
    /// How much of it the file holds, as far as a reader looks.
    len: std::sync::atomic::AtomicUsize,
}

/// The room a database's file is mapped with past its end: the file's own
/// length, and a gigabyte at the least. It is address space alone, of which
/// a 64-bit process has terabytes, and it is taken again, twice the file's
/// length, only when the file outgrows it.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
const ROOM: usize = 1 << 30;

// SAFETY: a `Mapping` owns its pages, which no other value points at, and
// `munmap` lets them go from whichever thread drops it.
#[cfg(all(unix, target_pointer_width = "64"))]
unsafe impl Send for Mapping {}
// SAFETY: through `&Mapping` the pages are only read -- they are mapped
// `PROT_READ` and nothing writes through `ptr` -- and the length a reader
// takes is an atomic (`cover`).
#[cfg(all(unix, target_pointer_width = "64"))]
unsafe impl Sync for Mapping {}

#[cfg(all(unix, target_pointer_width = "64"))]
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
}

/// `mmap`'s memory of no file, zeros until written: Linux's and Android's
/// value, illumos' and every BSD's, macOS among them.
#[cfg(all(unix, target_pointer_width = "64"))]
const MAP_ANON: i32 = if cfg!(any(target_os = "linux", target_os = "android")) {
    0x20
} else if cfg!(any(target_os = "illumos", target_os = "solaris")) {
    0x100
} else {
    0x1000
};

/// The largest page a system this maps on has (arm64 Linux's 64 KiB; 16
/// KiB on Apple's, 4 KiB on x86_64): where a mapping's pages may be laid
/// over by others, a multiple of every one.
#[cfg(all(unix, target_pointer_width = "64"))]
const PAGE_MAX: usize = 64 << 10;

/// `madvise`'s "the pages will be needed": the kernel reads them in ahead
/// of the faults. 3 on Linux and on macOS.
#[cfg(all(unix, target_pointer_width = "64"))]
const MADV_WILLNEED: i32 = 3;

/// How much of a file [`touch`] asks for at a time.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
const TOUCH_STEP: usize = 16 << 20;

#[cfg(all(unix, target_pointer_width = "64"))]
impl Mapping {
    /// The first `len` bytes of `file`. A mapping cannot be empty; a file
    /// always holds at least the magic by then.
    fn of(file: &File, len: usize) -> Result<Mapping> {
        Mapping::reserving(file, len, len)
    }

    /// [`Self::of`], with room past the file's end for what is appended to
    /// it ([`ROOM`]); the file alone where the system will not map that
    /// much -- an address space limit -- and a handover then maps it anew.
    fn with_room(file: &File, len: usize) -> Result<Mapping> {
        Mapping::reserving(file, len, len + len.max(ROOM)).or_else(|_| Mapping::of(file, len))
    }

    /// The first `len` bytes of `file` for a reader that takes no lock
    /// ([`open_read_only`]), and the survey its load takes: mapped up to
    /// where no open to write the file cuts it ([`crate::engine::surveyed`],
    /// found by reading it, never through a mapping), and from the page
    /// holding that point on read into memory of its own -- none of it where
    /// the point is the file's end, as a file with nothing past its image
    /// has it. A server opening the file after a crash cuts its torn tail
    /// while the reader walks it, and a page of a mapping wholly past a
    /// file's end is the process's end to read (`SIGBUS`); bytes read are
    /// the reader's whatever happens to the file after.
    fn reading(file: &File, len: usize) -> Result<(Mapping, crate::engine::Survey)> {
        use std::os::unix::fs::FileExt;
        let (survey, uncut) = crate::engine::surveyed(&mut |buf, at| file.read_at(buf, at), len)?;
        let m = Mapping::of(file, len)?;
        // Nothing an open cuts: no page of the mapping ends up past the
        // file's end, the last one's part past it zeros as ever, and the
        // file is mapped whole -- a page rounded down to was up to 64 KiB
        // read into memory for nothing.
        let at = match uncut >= len {
            true => len,
            false => uncut / PAGE_MAX * PAGE_MAX,
        };
        if at >= len {
            return Ok((m, survey));
        }
        const PROT_READ: i32 = 1;
        const PROT_WRITE: i32 = 2;
        const MAP_PRIVATE: i32 = 2;
        const MAP_FIXED: i32 = 0x10;
        let tail = len - at;
        // SAFETY: `at < len = reserved`, a multiple of every page, so the
        // range from `ptr + at` to the mapping's end is pages of `m` alone,
        // which `MAP_FIXED` lays memory of no file over -- nothing has read
        // them, nor holds a pointer into them, `m` being this function's
        // until it returns. A failure is `MAP_FAILED`, checked below, and
        // `m`'s drop unmaps the range whatever lies there.
        let laid = unsafe {
            mmap(
                m.ptr.add(at),
                tail,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_FIXED | MAP_ANON,
                -1,
                0,
            )
        };
        if laid as isize == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: those `tail` bytes from `ptr + at` are the memory just
        // laid there, readable and writable, and this is the one reference
        // to them until the `mprotect` below; they are zeros, so every one
        // is initialized.
        let into = unsafe { std::slice::from_raw_parts_mut(m.ptr.add(at), tail) };
        let mut got = 0;
        while got < tail {
            match file.read_at(&mut into[got..], (at + got) as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Read-only, as the rest: nothing writes through a `&Mapping`.
        // SAFETY: the same pages, `into`'s last use above; a failure leaves
        // them writable, which nothing does, and is said.
        if unsafe { mprotect(m.ptr.add(at), tail, PROT_READ) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // A file cut meanwhile ended sooner: covered as far as it was read.
        m.cover(at + got);
        Ok((m, survey))
    }

    fn reserving(file: &File, len: usize, reserved: usize) -> Result<Mapping> {
        use std::os::fd::AsRawFd;
        const PROT_READ: i32 = 1;
        const MAP_SHARED: i32 = 1;
        // SAFETY: no address is asked for, so the system picks one that
        // overlaps nothing of the process's; the descriptor is `file`'s,
        // open across the call (the mapping keeps the file's pages after
        // it, not the descriptor). A failure is `MAP_FAILED`, checked below.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                reserved,
                PROT_READ,
                MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr as isize == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Mapping {
            ptr,
            reserved,
            len: std::sync::atomic::AtomicUsize::new(len),
        })
    }

    /// Takes the file as `len` bytes long, where the mapping reaches that
    /// far. A reader never looks past what the file holds: a page past its
    /// end is the process's end (`SIGBUS`).
    fn cover(&self, len: usize) -> bool {
        if len > self.reserved {
            return false;
        }
        // Grown under the database's write lock, which is what orders it
        // before the reads of what it covers.
        self.len.store(len, std::sync::atomic::Ordering::Relaxed);
        true
    }
}

/// Brings every page of `m` into memory before anything waits on one: a
/// step of it at a time asked for ahead ([`MADV_WILLNEED`]), then a byte of
/// each of its pages read. Read a fault at a time, the pages came in a few
/// at a time, and on this 8 GB Mac a compact's new 1.2 GB file took 3.0 to
/// 3.3 s at about 400 MB/s -- the writer beside it at 11-15k updates a
/// second, the file's old pages it read pushed out meanwhile, and the new
/// ones gone again by the swap: the 2 s after it ran at 21-39k. Asked for
/// first, the kernel reads a step in large requests: 0.6 to 0.8 s, and
/// 90-150k a second after the swap (`make compact-bench`).
#[cfg(all(unix, target_pointer_width = "64"))]
pub fn touch(m: &crate::store::Base) {
    let bytes: &[u8] = (**m).as_ref();
    let mut sum = 0u8;
    // A step starts a whole number of steps into the mapping, which starts
    // on a page: what `madvise` asks of an address.
    for from in (0..bytes.len()).step_by(TOUCH_STEP) {
        let to = (from + TOUCH_STEP).min(bytes.len());
        // Advice: a refusal leaves the reads below to fault the pages in.
        // SAFETY: `from` is inside `bytes`, so the pointer is in bounds, and
        // `to - from` bytes from it are `bytes`' own. `MADV_WILLNEED` reads
        // pages ahead and changes no byte of them.
        unsafe {
            madvise(
                bytes.as_ptr().add(from) as *mut u8,
                to - from,
                MADV_WILLNEED,
            )
        };
        for at in (from..to).step_by(4096) {
            // Volatile, or the compiler drops reads whose result is unused.
            // SAFETY: `at < to <= bytes.len()`: a byte of `bytes`, which `m`
            // keeps mapped across the call.
            sum ^= unsafe { std::ptr::read_volatile(bytes.as_ptr().add(at)) };
        }
    }
    std::hint::black_box(sum);
}

#[cfg(all(unix, target_pointer_width = "64"))]
impl AsRef<[u8]> for Mapping {
    fn as_ref(&self) -> &[u8] {
        let len = self.len.load(std::sync::atomic::Ordering::Relaxed);
        // SAFETY: `ptr` starts a mapping of `reserved` bytes, live until
        // `drop` and so for as long as `&self`, and `len <= reserved`
        // (`cover` takes no more). `len` is never past what the file
        // holds, so no byte of the slice is on a page past its end, which
        // would fault: a mapping is made with the file's length, `cover`
        // grows it only over bytes written (`written_through`), and the one
        // cut, a torn tail at the open, takes the bytes past it out first
        // (`FileSink::cut`). Nor does a byte of it change while the slice
        // lives: appends land past `len`, a rewrite renames a new file over
        // this one (root CLAUDE.md, *A storage error stops writes*), and
        // the one write in place, the counter's head a compact beside the
        // database patches into its side file (`finish_beside`), lands
        // before that file's stores are the database's, on the one thread
        // holding them, no slice of its mapping alive across it. Another
        // process writing the file is what the `<file>.lock` every opener
        // of a file to write holds keeps out (*Single writer*). A reader
        // taking none (`open_read_only`) maps the file only up to where no
        // open to write it cuts it, found by reading it, and reads the rest
        // into memory of its own laid over the mapping (`Mapping::reading`),
        // so a server's open cutting a torn tail under it, during its walk
        // or after, takes no page of the file a slice of it spans
        // (docs/design/conventions.md#unsafe-code).
        unsafe { std::slice::from_raw_parts(self.ptr, len) }
    }
}

#[cfg(all(unix, target_pointer_width = "64"))]
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `reserved` are what `mmap` made, unmapped once,
        // here; a slice of it borrows `self`, so none outlives the drop.
        unsafe { munmap(self.ptr, self.reserved) };
    }
}

/// The file a rewrite beside the database writes (`compact` on a server):
/// the image with no lock held, and then, under the write lock, the writes
/// made meanwhile, before the sink takes it in place of the database's file
/// ([`Sink::adopt`]). Removed when dropped unless it was adopted.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
pub struct SideFile {
    path: PathBuf,
    out: FileImage,
    adopted: bool,
    /// On Linux, whether the file is written past the page cache
    /// ([`Self::past_cache`]), and how far it has been synced and its
    /// pages let go of.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    past: bool,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    dropped: u64,
}

/// How much of a side file written past the cache is written before it is
/// synced and its pages let go of, on Linux: the pages of a written file
/// stay in the cache until they are on disk, so they go a stretch at a
/// time rather than all at the end.
#[cfg(all(
    unix,
    target_pointer_width = "64",
    any(target_os = "linux", target_os = "android")
))]
const PAST_STEP: u64 = 64 << 20;

#[cfg(all(unix, target_pointer_width = "64"))]
extern "C" {
    #[cfg(target_vendor = "apple")]
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
}

#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
impl SideFile {
    pub fn create(path: &Path) -> Result<SideFile> {
        // One a failed rewrite left, which a view may still map: Windows
        // refuses its truncation (`ERROR_USER_MAPPED_FILE`), not its delete.
        #[cfg(windows)]
        let _ = std::fs::remove_file(path);
        let file = create_private(path)?;
        Ok(SideFile {
            path: path.to_path_buf(),
            out: FileImage {
                w: BufWriter::with_capacity(WRITE_BUF, file),
                at: 0,
            },
            adopted: false,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            past: false,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            dropped: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Has what is written from here on kept out of the page cache, where
    /// the system lets a file ask: a compact whose new file would not fit
    /// in memory beside the old one ([`crate::cache::room`]). Written
    /// through the cache, its pages pushed out the old file's, which every
    /// read and update needs until the swap: a container held to 2 GB
    /// compacting a 1.5 GB file into 1.1 ran its writer at 5 to 6k updates
    /// a second through the compact, 8k with the pages dropped once the
    /// image was written, and 18 to 25k with them dropped as it went.
    /// macOS's `F_NOCACHE`; on Linux the file synced and its pages dropped
    /// (`POSIX_FADV_DONTNEED`) every [`PAST_STEP`].
    pub fn past_cache(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            self.past = true;
        }
        #[cfg(target_vendor = "apple")]
        {
            use std::os::fd::AsRawFd;
            const F_NOCACHE: i32 = 48;
            // Advice: refused, the file is written through the cache.
            // SAFETY: the descriptor is the image's, open across the call;
            // `F_NOCACHE` takes an `int` and touches no memory of ours.
            unsafe { fcntl(self.out.w.get_ref().as_raw_fd(), F_NOCACHE, 1i32) };
        }
    }

    /// Pushes what is written to disk: the image, before the lock is taken
    /// for the rest, so that the fsync under it covers only that.
    pub fn sync(&mut self) -> Result<()> {
        self.out.w.flush()?;
        self.out.w.get_ref().sync_data()?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if self.past {
            use std::os::fd::AsRawFd;
            const POSIX_FADV_DONTNEED: i32 = 4;
            // The pages are on disk now, and dropping them is advice too.
            // SAFETY: the descriptor is the image's, open across the call;
            // the advice is about the file's cached pages, none of ours.
            unsafe { posix_fadvise(self.out.w.get_ref().as_raw_fd(), 0, 0, POSIX_FADV_DONTNEED) };
            self.dropped = self.out.at;
        }
        Ok(())
    }

    /// The file as written so far, mapped: where the stores of the image
    /// read from once it is the database's file. Appends after it land past
    /// the mapping, as a database's own do.
    pub fn map(&mut self) -> Result<crate::store::Base> {
        self.out.w.flush()?;
        let m = Mapping::of(self.out.w.get_ref(), self.out.at as usize)?;
        Ok(Arc::new(m))
    }

    /// Taken in place of the database's file: not to be removed.
    pub fn adopted(&mut self) {
        self.adopted = true;
    }
}

#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
impl ImageOut for SideFile {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write(bytes)?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if self.past && self.out.at - self.dropped >= PAST_STEP {
            self.sync()?;
        }
        Ok(())
    }
    fn at(&self) -> u64 {
        self.out.at()
    }
    fn patch(&mut self, at: u64, bytes: &[u8]) -> Result<()> {
        self.out.patch(at, bytes)
    }
}

#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
impl Drop for SideFile {
    fn drop(&mut self) {
        if !self.adopted {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Makes a rename or a removal in `path`'s directory durable. The new name
/// is an entry in the directory, and until the directory is synced a power
/// loss can bring back the old one: the file from before a `compact`,
/// without the writes the image had taken in. Public for the files a server
/// keeps beside a database (fennec-http's `.follows`). On Windows the
/// renamed file is flushed and then the directory, NTFS's log written
/// lazily (`windows::sync_dir`); nothing elsewhere.
#[cfg(unix)]
pub fn sync_dir(path: &Path) -> Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
pub fn sync_dir(path: &Path) -> Result<()> {
    windows::sync_dir(path)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
mod windows;
#[cfg(all(windows, target_pointer_width = "64"))]
pub use windows::{assume_room, placeholders, touch, Mapping};
#[cfg(windows)]
pub use windows::{mappable, sync_dir_alone, volume, Volume};

/// What wraps a file's sink before the database takes it: a primary's
/// `Tee` (fennec-http) goes around it here, fsyncing what the file already
/// holds before a replica can be sent any of it.
pub type Wrap<'a> = dyn FnOnce(Box<dyn Sink>) -> Result<Box<dyn Sink>> + 'a;

/// Opens a fennecdb file (creating it when missing): mapped where the target
/// maps files, read into memory where it does not. A 1 GB file of 2.3
/// million rows with a hash and an ordered index opened this way holds 188
/// MB rather than 1 095, and its `compact` peaks at 236 MB rather than
/// 2 012.
pub fn open(path: impl AsRef<Path>) -> Result<Database> {
    open_with(path, true, Box::new(Ok))
}

/// [`open`] -- or [`open_in_memory`] when `mapped` is false -- with the
/// file's sink wrapped before the database takes it. The flag reaches
/// every server path (`fennec-server --no-mmap`), replicated files and tenant
/// directories included.
pub fn open_with(path: impl AsRef<Path>, mapped: bool, wrap: Box<Wrap<'_>>) -> Result<Database> {
    open_into(Database::new(), path, mapped, wrap)
}

/// [`open_with`] as a server opens its file: the vectors the open would
/// link into a graph -- the writes after the last checkpoint, every vector
/// of a graph it cannot restore -- are left for a thread beside the queries
/// to link ([`Database::defer_linking`]), and the file opens in the time
/// its documents take to read.
pub fn open_serving(path: impl AsRef<Path>, mapped: bool, wrap: Box<Wrap<'_>>) -> Result<Database> {
    let mut db = Database::new();
    db.defer_linking();
    open_into(db, path, mapped, wrap)
}

fn open_into(
    db: Database,
    path: impl AsRef<Path>,
    mapped: bool,
    wrap: Box<Wrap<'_>>,
) -> Result<Database> {
    #[cfg(all(unix, target_pointer_width = "64"))]
    if mapped {
        return open_mapped_into(db, path, wrap);
    }
    // A volume whose renames another handle refuses, or a share whose read
    // errors a view would make the process's end, opens read into memory.
    #[cfg(all(windows, target_pointer_width = "64"))]
    if mapped && windows::mappable(path.as_ref()) {
        return open_mapped_into(db, path, wrap);
    }
    let _ = mapped;
    open_in_memory_into(db, path, wrap)
}

/// Opens a fennecdb file (creating it when missing) and reads it into memory,
/// records and all; a last record a crash cut short is cut off the file.
/// What a network file system wants, whose read errors a mapping would turn
/// into the process's death, and what has `--max-memory` count the data.
pub fn open_in_memory(path: impl AsRef<Path>) -> Result<Database> {
    open_in_memory_into(Database::new(), path, Box::new(Ok))
}

fn open_in_memory_into(
    mut db: Database,
    path: impl AsRef<Path>,
    wrap: Box<Wrap<'_>>,
) -> Result<Database> {
    let (mut sink, existing) = FileSink::open(path)?;
    // A new file is loaded too, as a mapped one is: it holds the magic by
    // now, which the bytes the database counts as its file's must include.
    if existing.len() >= MAGIC.len() {
        let whole = db.load(&existing)?;
        if whole < existing.len() {
            let end = sink.cut(whole, db.marks_cut())?;
            db.cut_to(end);
        }
    }
    db.set_sink(wrap(Box::new(sink))?);
    Ok(db)
}

/// [`open_in_memory`], with the documents left in the file: it is mapped
/// rather than read, and a document is decoded from its pages. What the
/// process holds is what is derived from the documents -- the offset index,
/// the hash, ordered and text indexes, the graph -- and the documents
/// written since the open until they are handed over to the file
/// ([`Database::hand_over`]). A rewrite (`checkpoint`, `compact`) writes the
/// new file and the stores are pointed at it, so the old one is let go of.
///
/// This is what [`open`] does where the target maps files, which is every
/// one fennecdb serves from.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
pub fn open_mapped(path: impl AsRef<Path>) -> Result<Database> {
    open_mapped_into(Database::new(), path, Box::new(Ok))
}

#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
fn open_mapped_into(
    mut db: Database,
    path: impl AsRef<Path>,
    wrap: Box<Wrap<'_>>,
) -> Result<Database> {
    let (mut file, path) = FileSink::create(path)?;
    let len = file.seek(SeekFrom::End(0))? as usize;
    let mapping = Arc::new(Mapping::with_room(&file, len)?);
    let mut sink = FileSink::over(file, path);
    sink.mapping = Some(mapping.clone());
    // A new file is loaded this way too -- it holds the magic by now -- so
    // the database is a mapped one from the start, and its first rewrite
    // points the stores at the file it wrote. Left out, a new file, a new
    // tenant and a replica taking its first image kept everything in
    // memory until the process restarted.
    // Windows cuts no file a view maps (`ERROR_USER_MAPPED_FILE`): the
    // database and its views go, the file is cut, and it opens again, as
    // this one was set up -- which the load forgets.
    #[cfg(windows)]
    let deferred = db.defer_links;
    let whole = db.load_mapped(mapping)?;
    #[cfg(windows)]
    if whole < len {
        return windows::cut_and_open(db, deferred, sink, whole, wrap);
    }
    if whole < len {
        // The pages past the cut stay mapped, past what the mapping covers
        // ([`FileSink::cut`]): no record points there.
        let end = sink.cut(whole, db.marks_cut())?;
        db.cut_to(end);
    }
    db.set_sink(wrap(Box::new(sink))?);
    Ok(db)
}

/// Opens a file to read it, and leaves it as it is: nothing is created,
/// cut, synced or written, and every write is refused. What a tool that
/// only looks -- `fennec types` -- opens a file a server may be writing
/// with: from outside, a record the server is in the middle of appending
/// looks torn, and cutting it there destroyed that write, and the file with
/// it once the server's next append landed past the cut.
///
/// Nor is the sync log beside it written back ([`crate::synclog`]): that
/// is a write. While the process that keeps the log runs, or after it
/// ended on its own, the file holds those writes already; after the machine
/// went down, they are in the log until the file is opened to be written,
/// and the file is read into memory with them put back
/// ([`read_with_sync_log`]) -- read as it was, the last durable writes were
/// damage their sums refused. A raw copy of the file taken then, before a
/// server or the library opens it again, may lack them.
pub fn open_read_only(path: impl AsRef<Path>) -> Result<Database> {
    let read = |bytes: &[u8]| -> Result<Database> {
        let mut db = Database::new();
        db.load(bytes)?;
        db.set_sink(Box::new(ReadOnly));
        Ok(db)
    };
    if let Some(bytes) = read_with_sync_log(path.as_ref())? {
        return read(&bytes);
    }
    // Where a server would not map the file, neither does a reader: a
    // share's read error would end it, and on FAT32 its view refuses the
    // server's rename.
    #[cfg(all(windows, target_pointer_width = "64"))]
    if !windows::mappable(path.as_ref()) {
        return read(&std::fs::read(path)?);
    }
    #[cfg(all(any(unix, windows), target_pointer_width = "64"))]
    return read_looked(looked(&File::open(path)?)?);
    #[cfg(not(all(any(unix, windows), target_pointer_width = "64")))]
    read(&std::fs::read(path)?)
}

/// The first of a read-only open's two steps: the file looked at -- on a
/// Unix surveyed by reading and mapped no further than an open to write it
/// could cut ([`Mapping::reading`]), the rest read into memory. On Windows
/// it is mapped at its length, and the reader's view refuses a cut
/// (`ERROR_USER_MAPPED_FILE`): a server's open after a crash fails by name
/// until the reader is gone. A test cuts the file between the two.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
fn looked(file: &File) -> Result<Looked> {
    let len = file.metadata()?.len() as usize;
    if len < MAGIC.len() {
        return Err(crate::error::Error::Corrupt(
            "invalid fennecdb signature".into(),
        ));
    }
    #[cfg(unix)]
    let (mapping, survey) = Mapping::reading(file, len)?;
    #[cfg(unix)]
    return Ok((Arc::new(mapping), Some(survey)));
    #[cfg(windows)]
    Ok((Arc::new(Mapping::of(file, len)?), None))
}

/// What [`looked`] made: the mapping, and on a Unix the survey the load
/// takes in the place of a walk of the mapping.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
type Looked = (Arc<Mapping>, Option<crate::engine::Survey>);

/// The second: the database read off what [`looked`] mapped, which it
/// covers no further than the records it took -- a torn tail past them is
/// nothing a slice of it hands out.
#[cfg(all(any(unix, windows), target_pointer_width = "64"))]
fn read_looked((mapping, survey): Looked) -> Result<Database> {
    let mut db = Database::new();
    #[cfg_attr(not(unix), allow(unused_variables))]
    let whole = match survey {
        Some(s) => db.load_surveyed(mapping.clone(), s)?,
        None => db.load_mapped(mapping.clone())?,
    };
    #[cfg(unix)]
    mapping.cover(whole);
    db.set_sink(Box::new(ReadOnly));
    Ok(db)
}

/// The file at `path` with what the sync log beside it holds and the file
/// lost put back, writing neither, where a log names it and puts anything
/// back: what a tool that only looks -- [`open_read_only`], `fennec verify`
/// -- reads after the machine went down. `None` otherwise: the file is to
/// be read as it is.
pub fn read_with_sync_log(path: impl AsRef<Path>) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        let path = resolved(path.as_ref());
        let file = File::open(&path)?;
        Ok(crate::synclog::applied(
            &file,
            &crate::synclog::path_for(&path),
        )?)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

/// The sink of a database [`open_read_only`] opened.
struct ReadOnly;

impl Sink for ReadOnly {
    fn append(&mut self, _bytes: &[u8]) -> Result<()> {
        Err(crate::error::Error::ReadOnly(
            "the file was opened to be read".into(),
        ))
    }
    fn rewrite(&mut self, _bytes: &[u8]) -> Result<()> {
        self.append(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record appended in parts -- a replica's, its claim made this
    /// file's -- is never split by a durability, which a syncer runs after
    /// the database's lock is let go, beside the next append: every point
    /// it stores as durable is where a record ends. Appended a part at a
    /// time, one that ran between them stored a point inside the record,
    /// which every record after it claimed; a power cut that then took its
    /// trailer alone refused the open where it cuts. Large records too,
    /// written past the buffer under the disk's lock.
    #[test]
    fn a_record_in_parts_is_never_split_by_a_sync() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-parts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("parts.fennec");
        let (mut sink, _) = FileSink::open(&path).unwrap();
        let durable = sink.durable().unwrap();
        let (disk, pending) = (Arc::clone(&sink.disk), Arc::clone(&sink.pending));
        let mut ends = vec![lock(&disk).end];
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let syncer = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while !stop.load(Ordering::Acquire) {
                    lock(&disk).sync(&pending, u64::MAX, false).unwrap();
                    seen.push(durable.load(Ordering::Acquire));
                }
                seen
            })
        };
        let big = vec![7u8; WRITE_BUF];
        let trailer = [9u8; 5];
        for i in 0..20_000 {
            let body = match i % 500 {
                0 => &big[..],
                k => &big[..100 + k],
            };
            sink.record_parts(0, &[], &[body, &trailer]).unwrap();
            ends.push(ends[ends.len() - 1] + (body.len() + trailer.len()) as u64);
        }
        stop.store(true, Ordering::Release);
        let seen = syncer.join().unwrap();
        assert!(!seen.is_empty());
        for d in seen {
            assert!(
                ends.binary_search(&d).is_ok(),
                "a sync stored {d} as durable, inside a record"
            );
        }
        drop(sink);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A torn tail cut at the open (`open_mapped_into`) is cut out of the
    /// mapping too. Left covered, a slice of the mapping held pages wholly
    /// past the file's new end, which are the process's end to read
    /// (`SIGBUS`), and the first appends after the cut landed in its bytes
    /// in place, under every slice a reader took: what `Mapping::as_ref`
    /// says never happens. The cut's mark goes past what it covers, and the
    /// appends after the mark.
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn a_cut_takes_the_bytes_past_it_out_of_the_mapping() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.fennec");
        // Pages past the cut whatever the system's page: 4 KB, 16 KB, 64 KB.
        let len = 3 << 16;
        std::fs::write(&path, vec![7u8; len]).unwrap();
        let (mut sink, _) = FileSink::open(&path).unwrap();
        let mapping = Arc::new(Mapping::with_room(&lock(&sink.disk).file, len).unwrap());
        sink.mapping = Some(mapping.clone());
        let mark = crate::engine::synced_mark();
        assert_eq!(sink.cut(100, true).unwrap(), 100 + mark.len() as u64);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 108);
        assert_eq!(std::fs::read(&path).unwrap()[100..], mark[..]);
        assert_eq!((*mapping).as_ref().len(), 100);
        // Every page of what it covers read: one past the file's end would
        // end this process.
        touch(&(mapping.clone() as crate::store::Base));
        sink.append(&[1, 2, 3]).unwrap();
        sink.sync().unwrap();
        assert_eq!((*mapping).as_ref(), &[7u8; 100][..]);
        assert_eq!(std::fs::read(&path).unwrap()[108..], [1, 2, 3]);
        drop(sink);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a power cut leaves of a file at every step of an open's cut
    /// ([`FileSink::cut`]), built from the steps the cutting open took: each
    /// write and length after the last fsync lost or kept -- a write whole,
    /// or the length it grew the file to alone -- and, while no fsync of
    /// the cut has run, bytes the earlier process never synced lost
    /// besides. Every such file, opened by every way a file opens, reads as
    /// the cutting open read it, cutting nothing it kept, and every way
    /// that writes leaves it as the cut does. Three files: one torn as
    /// v0.2.0's `torn-wide` is -- the records claiming the last fsync go
    /// with the tail, and a synced row the disk flipped a bit of lies below
    /// the point they name, read unchecked -- and one whose rows after the
    /// last fsync are kept, its last torn long and short: the earlier
    /// process's pages, durable only by the cut's own fsync. With the mark
    /// written under the cut's one fsync, a power cut kept the mark and
    /// lost bytes of those rows, which the next open read unchecked as the
    /// zeros it held, or kept the cut and lost the mark, and the next open
    /// cut the flipped row and the synced row after it.
    ///
    /// The mark's write torn, half of it either way -- eight bytes across
    /// two sectors -- reads as the cutting open read it too, but where the
    /// point the cutting open read to came from a record past the cut and a
    /// row below it fails its sum (`torn-wide`): there, with no frame at
    /// the cut, the next open finds the point from the records kept alone,
    /// and the records past the cut, still there, refuse the open naming
    /// the row -- never a read of it otherwise, nor a cut of anything kept.
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn a_cut_a_power_cut_stops_at_any_step_opens_as_the_cutting_open_read_it() {
        use crate::query::{Expr, Statement};
        use crate::schema::{Field, Schema};
        use crate::value::{DataType, Value};
        use std::ops::Range;
        /// Where each record of `file` starts and ends, by its head.
        fn heads(file: &[u8]) -> Vec<(usize, usize)> {
            let varint = |p: &mut usize| {
                let (mut v, mut shift) = (0usize, 0);
                loop {
                    let b = file[*p];
                    *p += 1;
                    v |= usize::from(b & 0x7f) << shift;
                    if b < 0x80 {
                        return v;
                    }
                    shift += 7;
                }
            };
            let (mut out, mut at) = (Vec::new(), MAGIC.len());
            while at < file.len() {
                let mut p = at + 1;
                varint(&mut p);
                let len = varint(&mut p);
                out.push((at, p + len));
                at = p + len;
            }
            out
        }
        /// `step` as a power cut left it, `how` saying what of it: nothing
        /// (0), all (1), the length a write grew the file to alone (2), or
        /// the first (3) or last (4) half of its bytes; whether any of it.
        fn kept(file: &mut Vec<u8>, step: &CutStep, how: usize) -> bool {
            match (step, how) {
                (CutStep::Sync, _) | (_, 0) => false,
                (CutStep::SetLen(n), _) => {
                    file.resize(*n as usize, 0);
                    true
                }
                (CutStep::Write(at, b), how) => {
                    let (at, end, half) = (*at as usize, *at as usize + b.len(), b.len() / 2);
                    if how == 2 && end <= file.len() {
                        return false;
                    }
                    if file.len() < end {
                        file.resize(end, 0);
                    }
                    match how {
                        1 => file[at..end].copy_from_slice(b),
                        2 => {}
                        3 => file[at..at + half].copy_from_slice(&b[..half]),
                        _ => file[at + half..end].copy_from_slice(&b[half..]),
                    }
                    true
                }
            }
        }
        /// Every file a power cut after each of `steps` may leave of
        /// `torn`, each once, but those holding nothing of the cut: the
        /// earlier crash's own, `lost` -- bytes no fsync covered -- zeros.
        /// Each says what it holds, and whether a write in it is torn.
        fn stopped(
            torn: &[u8],
            lost: Range<usize>,
            steps: &[CutStep],
        ) -> Vec<(String, Vec<u8>, bool)> {
            let mut out: Vec<(String, Vec<u8>, bool)> = Vec::new();
            for k in 0..=steps.len() {
                let done = &steps[..k];
                let synced = done.iter().rposition(|s| *s == CutStep::Sync);
                let (on_disk, after) = done.split_at(synced.map_or(0, |i| i + 1));
                let hows: Vec<usize> = after
                    .iter()
                    .map(|s| match s {
                        CutStep::Write(..) => 5,
                        _ => 2,
                    })
                    .collect();
                for pick in 0..hows.iter().product::<usize>() {
                    for lose in [false, true] {
                        if lose && synced.is_some() {
                            continue;
                        }
                        let mut file = torn.to_vec();
                        if lose {
                            file[lost.clone()].fill(0);
                        }
                        let mut any = false;
                        for s in on_disk {
                            any |= kept(&mut file, s, 1);
                        }
                        let (mut p, mut said, mut half) = (pick, Vec::new(), false);
                        for (s, n) in after.iter().zip(&hows) {
                            let how = p % n;
                            p /= n;
                            any |= kept(&mut file, s, how);
                            half |= how > 2;
                            said.push(match (s, how) {
                                (CutStep::Write(..), 1) => "the mark",
                                (CutStep::Write(..), 2) => "the mark's length",
                                (CutStep::Write(..), 3) => "the mark's first half",
                                (CutStep::Write(..), 4) => "the mark's last half",
                                (CutStep::SetLen(_), 1) => "the length",
                                (_, _) => "nothing",
                            });
                        }
                        if lose && !any {
                            continue;
                        }
                        if let Some(o) = out.iter_mut().find(|(_, f, _)| *f == file) {
                            o.2 &= half;
                            continue;
                        }
                        let what = format!(
                            "after {k} of {} steps, of the unsynced ones {}{}",
                            steps.len(),
                            match said.is_empty() {
                                true => "none".to_string(),
                                false => said.join(" and "),
                            },
                            if lose {
                                ", a page no fsync covered lost"
                            } else {
                                ""
                            }
                        );
                        out.push((what, file, half));
                    }
                }
            }
            out
        }
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stopped.fennec");
        // The file once each step ran -- a row put, or `None` an fsync --
        // written and not closed.
        let written = |steps: &[Option<String>]| {
            let _ = std::fs::remove_file(&path);
            let mut db = open(&path).unwrap();
            let schema = Schema::new("t", vec![Field::new("a", DataType::Text)]).unwrap();
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .unwrap();
            for step in steps {
                match step {
                    None => db.sync().unwrap(),
                    Some(a) => {
                        db.execute(&Statement::Put {
                            collection: "t".into(),
                            docs: vec![vec![("a".into(), Expr::Lit(Value::Text(a.clone())))]],
                            docs_param: None,
                            insert: false,
                            if_absent: false,
                            else_set: None,
                            require: None,
                            pick: None,
                        })
                        .unwrap();
                    }
                }
            }
            db.write_out().unwrap();
            std::fs::read(&path).unwrap()
        };
        let texts = |db: &Database| -> Vec<(u64, String)> {
            let c = db.collection("t").unwrap();
            let mut ids: Vec<u64> = c.store.iter_ids().collect();
            ids.sort_unstable();
            ids.into_iter()
                .map(|id| match c.store.read(&c.schema, id) {
                    Ok(Some(doc)) => match doc.fields.iter().find(|(f, _)| f == "a") {
                        Some((_, Value::Text(a))) => (id, a.clone()),
                        other => (id, format!("{other:?}")),
                    },
                    other => (id, format!("{other:?}")),
                })
                .collect()
        };
        let opened = |way: &str| match way {
            "mapped" => open(&path),
            "in-memory" => open_in_memory(&path),
            "serving" => open_serving(&path, true, Box::new(Ok)),
            "serving in memory" => open_serving(&path, false, Box::new(Ok)),
            _ => open_read_only(&path),
        };
        let row = |s: &str| Some(s.to_string());
        let synced = "a synced row after it ".repeat(240);
        let mut torn_wide = vec![row("first"), None, row("the row a disk flipped a bit of")];
        // More than a page: the claims after the next fsync, rounded down a
        // page from their records, name a point past the row before it.
        torn_wide.extend([
            row(&synced),
            None,
            row("the row a power cut lost a page of"),
        ]);
        torn_wide.extend((0..20).map(|n| row(&format!("after the fsync, {n}"))));
        let mut unsynced = vec![row("first"), None];
        unsynced.extend((0..3).map(|n| row(&format!("a row no fsync covered, {n} ").repeat(60))));
        unsynced.push(row(&"the row a crash tore ".repeat(20)));
        let find = |file: &[u8], what: &str| {
            let what = what.as_bytes();
            file.windows(what.len()).position(|w| w == what).unwrap()
        };
        let mark = crate::engine::synced_mark();
        let (mut fails, mut orders, mut torn_refused) = (Vec::new(), Vec::new(), Vec::new());
        for name in [
            "torn-wide",
            "kept unsynced, torn long",
            "kept unsynced, torn short",
        ] {
            let (torn, lost) = match name {
                "torn-wide" => {
                    let mut bad = written(&torn_wide);
                    let flipped = find(&bad, "the row a disk flipped") + 15;
                    let lost = find(&bad, "the row a power cut lost") + 25;
                    bad[flipped] ^= 0x20;
                    bad[lost] ^= 0x20;
                    let at = find(&bad, "after the fsync, 5");
                    (bad, at..at + 18)
                }
                _ => {
                    let good = written(&unsynced);
                    let last = *heads(&good).last().unwrap();
                    let tail = match name {
                        "kept unsynced, torn long" => (last.1 - last.0) / 2,
                        _ => 3,
                    };
                    // 1 200 bytes of the second row no fsync covered.
                    let at = find(&good, "a row no fsync covered, 1");
                    (good[..last.0 + tail].to_vec(), at + 100..at + 1300)
                }
            };
            // The open that cuts it, every way an open writes a file: the
            // rows it reads, where it cuts and the steps it took.
            let mut cutting = None;
            for way in ["mapped", "in-memory", "serving", "serving in memory"] {
                std::fs::write(&path, &torn).unwrap();
                CUT_STEPS.with(|s| s.borrow_mut().clear());
                let db = opened(way).unwrap_or_else(|e| panic!("{name}, {way}: {e}"));
                let read = (
                    texts(&db),
                    db.cut_on_open().expect("the open cut nothing").at as usize,
                    CUT_STEPS.with(|s| std::mem::take(&mut *s.borrow_mut())),
                );
                drop(db);
                match &cutting {
                    None => cutting = Some(read),
                    Some(first) => assert!(*first == read, "{name}, {way}: another cut"),
                }
            }
            let (want, whole, steps) = cutting.unwrap();
            if name == "torn-wide" {
                assert!(
                    want[1].1.contains("Flipped") && want.len() == 3,
                    "the flipped row read unchecked: {want:?}"
                );
            } else {
                assert_eq!(want.len(), 4, "{name}");
            }
            let left = [&torn[..whole], &mark[..]].concat();
            for (what, file, half) in stopped(&torn, lost, &steps) {
                for way in [
                    "mapped",
                    "in-memory",
                    "serving",
                    "serving in memory",
                    "read-only",
                ] {
                    let at = format!("{name}, {what}, {way}");
                    std::fs::write(&path, &file).unwrap();
                    let db = match opened(way) {
                        Ok(db) => db,
                        // A mark torn across two sectors leaves no frame
                        // at the cut: the durable point is the kept
                        // records' own, and a row below the cutting open's
                        // failing its sum is refused by the records after
                        // it, still there -- never read otherwise, nor cut.
                        Err(crate::error::Error::Corrupt(e)) if half && name == "torn-wide" => {
                            torn_refused.push(format!("{at}: {e}"));
                            continue;
                        }
                        Err(e) => {
                            fails.push(format!("{at}: refused: {e}"));
                            continue;
                        }
                    };
                    if texts(&db) != want {
                        fails.push(format!("{at}: read {:?}", texts(&db)));
                    }
                    if let Some(c) = db.cut_on_open().filter(|c| c.at < whole as u64) {
                        fails.push(format!("{at}: cut what the cut kept: {c:?}"));
                    }
                    drop(db);
                    let now = std::fs::read(&path).unwrap();
                    if way == "read-only" {
                        assert!(now == file, "{at}: the look wrote");
                        continue;
                    }
                    if now != left {
                        fails.push(format!(
                            "{at}: left {} bytes, not {}",
                            now.len(),
                            left.len()
                        ));
                    }
                    let db = opened("read-only").unwrap();
                    if db.cut_on_open().is_some() || texts(&db) != want {
                        fails.push(format!("{at}, then read-only: {:?}", db.cut_on_open()));
                    }
                }
            }
            orders.push((name, whole, steps));
        }
        assert!(fails.is_empty(), "{}:\n{}", fails.len(), fails.join("\n"));
        eprintln!("refused, a mark torn:\n{}", torn_refused.join("\n"));
        // The order each cut took: the file synced, the mark over the torn
        // record's first bytes synced, the cut synced.
        for (name, whole, steps) in orders {
            let mark_at = CutStep::Write(whole as u64, mark.clone());
            let cut_at = CutStep::SetLen((whole + mark.len()) as u64);
            let sync = CutStep::Sync;
            let order = [sync.clone(), mark_at, sync.clone(), cut_at, sync];
            assert_eq!(steps, order, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reader that takes no lock (`open_read_only`) over a file a crash
    /// left a torn record at the end of is covered to the records its open
    /// took: a server opening the file then cuts the tail under it, and no
    /// slice the reader takes after reaches past the file's new end. Before,
    /// it kept the length the file had at its open: after the cut its slice
    /// was 131 139 bytes over a file of 40, the pages past the end a read of
    /// which ends the process (`SIGBUS`).
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn a_reader_is_covered_to_the_records_its_open_took() {
        use crate::query::{Expr, Statement};
        use crate::schema::{Field, Schema};
        use crate::value::{DataType, Value};
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-reader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reader.fennec");
        let put = |a: String| Statement::Put {
            collection: "t".into(),
            docs: vec![vec![("a".into(), Expr::Lit(Value::Text(a)))]],
            docs_param: None,
            insert: false,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        };
        {
            let mut db = open(&path).unwrap();
            let schema = Schema::new("t", vec![Field::new("a", DataType::Text)]).unwrap();
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .unwrap();
            db.execute(&put("one".into())).unwrap();
            // Pages past the cut whatever the system's page: 4 KB, 16 KB, 64 KB.
            db.execute(&put("x".repeat(3 << 16))).unwrap();
            db.sync().unwrap();
        }
        // The last record torn: the crash came in the middle of its append.
        let torn = std::fs::metadata(&path).unwrap().len() - (1 << 16);
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(torn).unwrap();
        drop(f);
        let reader = open_read_only(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            torn,
            "the reader cut"
        );
        // A server's open after the crash, under the reader: the file cut,
        // and its close's mark after the cut (`FileSink::cut`).
        drop(open(&path).unwrap());
        let mark = crate::engine::synced_mark();
        let file = std::fs::read(&path).unwrap();
        assert!(file.ends_with(&mark), "the cut unmarked");
        let cut = (file.len() - mark.len()) as u64;
        assert!(cut < torn, "the open cut nothing: {cut} of {torn}");
        let t = reader.collection("t").unwrap();
        let base = t
            .store
            .mapped_base()
            .expect("the reader's records are mapped");
        assert_eq!((**base).as_ref().len() as u64, cut);
        // Every page of what it covers read: one past the file's end would
        // end this process.
        touch(base);
        assert_eq!(t.stats().documents, 1);
        drop(reader);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reader that takes no lock (`open_read_only`) stopped between its
    /// look at the file and its walk of it, a server's open cutting the
    /// torn tail in between: what it walks past the point no open cuts it
    /// read into memory at its look, so no page it reads is past the file's
    /// end, and it reads the whole records. Mapped at its length, as the
    /// reader was before, its walk summed the torn record's pages past the
    /// cut and the process ended (`SIGBUS`).
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn a_reader_that_looked_before_a_cut_walks_what_it_read() {
        use crate::query::{Expr, Statement};
        use crate::schema::{Field, Schema};
        use crate::value::{DataType, Value};
        use std::os::unix::fs::FileExt;
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-looked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("looked.fennec");
        let put = |a: String| Statement::Put {
            collection: "t".into(),
            docs: vec![vec![("a".into(), Expr::Lit(Value::Text(a)))]],
            docs_param: None,
            insert: false,
            if_absent: false,
            else_set: None,
            require: None,
            pick: None,
        };
        let synced = {
            let mut db = open(&path).unwrap();
            let schema = Schema::new("t", vec![Field::new("a", DataType::Text)]).unwrap();
            db.execute(&Statement::CreateCollection {
                schema,
                if_not_exists: false,
            })
            .unwrap();
            // Pages before the point no open cuts, whatever the system's
            // page, so that the reader maps some of the file.
            db.execute(&put("y".repeat(3 << 16))).unwrap();
            db.sync().unwrap();
            db.execute(&put("one".into())).unwrap();
            db.sync().unwrap();
            db.execute(&put("x".repeat(3 << 16))).unwrap();
            db.sync().unwrap();
            std::fs::metadata(&path).unwrap().len()
        };
        // As a crash leaves it: no close's mark, and the last record's last
        // 64 KiB zeros, its length on the disk before its bytes.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(synced - (1 << 16)).unwrap();
        f.set_len(synced).unwrap();
        drop(f);
        let _ = std::fs::remove_file(crate::synclog::path_for(&path));

        let file = File::open(&path).unwrap();
        let (_, floor) =
            crate::engine::surveyed(&mut |b, at| file.read_at(b, at), synced as usize).unwrap();
        assert!(floor >= PAGE_MAX, "the reader maps nothing: {floor}");
        let looked = looked(&file).unwrap();
        // A server's open after the crash, under the reader: the file cut,
        // and its close's mark after the cut (`FileSink::cut`).
        drop(open(&path).unwrap());
        let mark = crate::engine::synced_mark().len();
        let cut = std::fs::metadata(&path).unwrap().len() as usize - mark;
        assert!(
            cut < synced as usize,
            "the open cut nothing: {cut} of {synced}"
        );
        assert!(floor <= cut, "{floor} mapped past the cut at {cut}");
        let reader = read_looked(looked).unwrap();
        let t = reader.collection("t").unwrap();
        let base = t
            .store
            .mapped_base()
            .expect("the reader's records are mapped");
        // Every page of what it covers read: one past the file's end would
        // end this process.
        touch(base);
        assert!((**base).as_ref().len() <= cut);
        let mut texts: Vec<String> = t
            .store
            .iter_ids()
            .map(
                |id| match t.store.read(&t.schema, id).unwrap().unwrap().get("a") {
                    Some(Value::Text(a)) => a.chars().take(3).collect(),
                    other => panic!("{other:?}"),
                },
            )
            .collect();
        texts.sort();
        assert_eq!(texts, ["one", "yyy"]);
        drop(reader);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A durability finds its bytes on disk when a later one got there
    /// first, and runs no fsync of its own; what it wrote is the file's
    /// tail, in the order it was appended, rewrite or not.
    #[test]
    fn durabilities_share_an_fsync_and_keep_the_order() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("share.fennec");
        let _ = std::fs::remove_file(&path);
        let (mut sink, _) = FileSink::open(&path).unwrap();

        sink.append(b"one ").unwrap();
        let first = sink.flush().unwrap().unwrap();
        sink.append(b"two ").unwrap();
        let second = sink.flush().unwrap().unwrap();
        // The later one writes both and fsyncs once; the earlier one then
        // has nothing left to do.
        second().unwrap();
        first().unwrap();
        assert_eq!(lock(&sink.disk).fsyncs, 1);

        sink.append(b"three").unwrap();
        sink.sync().unwrap();
        assert_eq!(lock(&sink.disk).fsyncs, 2);
        let on_disk = std::fs::read(&path).unwrap();
        assert!(on_disk.ends_with(b"one two three"), "{on_disk:?}");

        // A rewrite replaces the file with an image that already holds
        // everything appended: what was pending is not written after it.
        sink.append(b" lost").unwrap();
        let before = sink.flush().unwrap().unwrap();
        sink.rewrite(b"IMAGE").unwrap();
        before().unwrap();
        sink.append(b" after").unwrap();
        sink.sync().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"IMAGE after");

        drop(sink);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// What an earlier process left in the file -- in the page cache alone,
    /// had it crashed -- a durability fsyncs, once, though it covers nothing
    /// appended: a replayed `Idempotency-Key` after a restart answers a
    /// write that process landed. A close's sync leaves it, as it did.
    #[test]
    fn a_durability_covers_what_an_earlier_process_left() {
        let dir =
            std::env::temp_dir().join(format!("fennecdb-fs-inherited-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("left.fennec");
        let (mut earlier, _) = FileSink::open(&path).unwrap();
        earlier.append(b"landed").unwrap();
        earlier.write_out().unwrap();
        drop(earlier);

        let (mut sink, held) = FileSink::open(&path).unwrap();
        assert!(held.ends_with(b"landed"), "{held:?}");
        sink.sync().unwrap();
        assert_eq!(lock(&sink.disk).fsyncs, 0, "a close's sync fsynced");
        sink.flush().unwrap().unwrap()().unwrap();
        assert_eq!(
            lock(&sink.disk).fsyncs,
            1,
            "the inherited bytes went unsynced"
        );
        sink.flush().unwrap().unwrap()().unwrap();
        assert_eq!(
            lock(&sink.disk).fsyncs,
            1,
            "inherited bytes are fsynced once"
        );
        drop(sink);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// As a server opens its file (`open_serving`) after a process that
    /// landed a write and was killed before its fsync: the write is in the
    /// file, in the page cache alone for all anyone knows. The first
    /// durability fsyncs it though it covers nothing appended -- a replayed
    /// `Idempotency-Key` waits for that fsync before it answers the write
    /// as made -- the file itself, with no sync log begun (kept here as
    /// Linux keeps one); a plain sync before it fsyncs nothing, and a
    /// durability after it none.
    #[cfg(unix)]
    #[test]
    fn a_servers_first_durability_fsyncs_what_a_killed_process_left() {
        let dir = std::env::temp_dir().join(format!(
            "fennecdb-fs-killed-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("killed.fennec");
        let mut earlier = open(&path).unwrap();
        earlier.fork(7).unwrap();
        earlier.write_out().unwrap();
        // Killed: nothing of its own runs after the write, no drop.
        std::mem::forget(earlier);

        keep_sync_log(true);
        let mut db = open_serving(&path, true, Box::new(Ok)).unwrap();
        keep_sync_log(false);
        assert!(
            db.history().lineage.iter().any(|&(id, _)| id == 7),
            "the killed process's write is not in the file"
        );
        let fsyncs = || FSYNCS_HERE.with(|n| n.get());
        let before = fsyncs();
        db.sync().unwrap();
        assert_eq!(fsyncs(), before, "a plain sync fsynced");
        db.durability().unwrap().expect("a durability")().unwrap();
        assert_eq!(fsyncs(), before + 1, "what the process left went unsynced");
        let log = crate::synclog::path_for(&resolved(&path));
        assert!(!log.exists(), "an fsync of what it left began a sync log");
        db.durability().unwrap().expect("a durability")().unwrap();
        assert_eq!(fsyncs(), before + 1, "what it left was fsynced twice");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With a sync log, durabilities handed out together share one entry
    /// as they share an fsync; and a log that cannot be written fails the
    /// sink as a file that cannot be synced does -- nothing after it is
    /// told it is durable.
    #[cfg(unix)]
    #[test]
    fn durabilities_share_a_log_entry_and_a_failed_one_stops_the_sink() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("share.fennec");
        keep_sync_log(true);
        let (mut sink, _) = FileSink::open(&path).unwrap();
        keep_sync_log(false);
        let log = crate::synclog::path_for(&path);

        sink.append(b"zero ").unwrap();
        sink.sync().unwrap();
        sink.append(b"one ").unwrap();
        sink.sync().unwrap();
        assert_eq!(crate::synclog::SyncLog::entries(&log), 1);
        sink.append(b"two ").unwrap();
        let first = sink.flush().unwrap().unwrap();
        sink.append(b"three ").unwrap();
        let second = sink.flush().unwrap().unwrap();
        second().unwrap();
        first().unwrap();
        assert_eq!(crate::synclog::SyncLog::entries(&log), 2);
        assert_eq!(lock(&sink.disk).fsyncs, 3);

        if let Some(l) = lock(&sink.disk).tail.log.as_mut() {
            l.break_writes(&log);
        }
        sink.append(b"four ").unwrap();
        assert!(sink.sync().is_err());
        sink.append(b"five ").unwrap();
        assert!(sink.sync().is_err(), "the sink stays failed");
        drop(sink);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A record appended for its durability waits for it, however far past
    /// the buffer it goes, and the durability puts it on disk in its place;
    /// a plain append that far past is written there and then.
    #[test]
    fn a_deferred_append_waits_for_its_durability() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-defer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("defer.fennec");
        let _ = std::fs::remove_file(&path);
        let (mut sink, _) = FileSink::open(&path).unwrap();
        let len = |p: &Path| std::fs::metadata(p).unwrap().len() as usize;

        sink.append(b"head ").unwrap();
        let before = len(&path);
        let big = vec![7u8; 2 * WRITE_BUF];
        sink.append_deferred(&big).unwrap();
        assert_eq!(len(&path), before);
        let durable = sink.flush().unwrap().unwrap();
        durable().unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        assert!(on_disk.ends_with(&big));
        assert!(on_disk[..on_disk.len() - big.len()].ends_with(b"head "));

        sink.append(&big).unwrap();
        assert_eq!(len(&path), on_disk.len() + big.len());

        drop(sink);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// An append past the buffer does not wait for a durability that holds
    /// the disk -- it runs under the database's lock, and the fsync would
    /// hold every request -- but keeps the bytes until it is done, up to
    /// `WRITE_HELD`, past which it waits; the bytes reach the file in the
    /// order they came. The disk is held here as an fsync holds it, and let
    /// go once the buffer is past `WRITE_HELD`, an event, not a time.
    #[test]
    fn an_append_past_the_buffer_does_not_wait_out_an_fsync() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-busy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("busy.fennec");
        let _ = std::fs::remove_file(&path);
        let (mut sink, _) = FileSink::open(&path).unwrap();
        let len = |p: &Path| std::fs::metadata(p).unwrap().len() as usize;
        let start = len(&path);
        assert!(!sink.syncing());

        let (disk, pending) = (Arc::clone(&sink.disk), Arc::clone(&sink.pending));
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let guard = lock(&disk);
            held_tx.send(()).unwrap();
            while lock(&pending).len() < WRITE_HELD {
                std::thread::yield_now();
            }
            drop(guard);
        });
        held_rx.recv().unwrap();
        assert!(sink.syncing());
        let chunk = vec![1u8; WRITE_BUF / 4];
        let mut sent = Vec::new();
        for i in 0..8 {
            let chunk: Vec<u8> = chunk.iter().map(|b| b + i).collect();
            sink.append(&chunk).unwrap();
            sent.extend_from_slice(&chunk);
        }
        // Twice the buffer, and nothing written while the disk is held.
        assert_eq!(len(&path), start);
        while sent.len() < WRITE_HELD {
            sink.append(&chunk).unwrap();
            sent.extend_from_slice(&chunk);
        }
        // Past `WRITE_HELD` the append waited for the disk and wrote.
        holder.join().unwrap();
        assert_eq!(len(&path), start + sent.len());
        assert!(!sink.syncing());
        sink.append(b"tail").unwrap();
        sink.sync().unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(&on_disk[start..start + sent.len()], &sent[..]);
        assert!(on_disk.ends_with(b"tail"));

        drop(sink);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// An image that cannot be written leaves the old file standing, and the
    /// appends pending when it began still go to it: a durability handed out
    /// before the rewrite finds its bytes on disk rather than being told
    /// they are there when they are in neither file.
    #[test]
    fn a_failed_rewrite_leaves_the_pending_appends_to_the_old_file() {
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fail.fennec");
        let _ = std::fs::remove_file(&path);
        let (mut sink, _) = FileSink::open(&path).unwrap();

        sink.append(b"kept").unwrap();
        let durable = sink.flush().unwrap().unwrap();
        let err = sink.rewrite_with(&mut |out| {
            out.write(b"half an ima")?;
            Err(crate::error::Error::Io("no space left on device".into()))
        });
        assert!(err.is_err());
        durable().unwrap();
        assert!(std::fs::read(&path).unwrap().ends_with(b"kept"));
        assert!(!beside(&path, "compacting").exists());
        // The sink is not failed: the next append and sync go on.
        sink.append(b" more").unwrap();
        sink.sync().unwrap();
        assert!(std::fs::read(&path).unwrap().ends_with(b"kept more"));

        drop(sink);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// The umask a child of [`under_umasks`] runs under, in octal; unset in
    /// the test run itself.
    #[cfg(unix)]
    const UMASK: &str = "FENNEC_TEST_UMASK";

    /// Runs this binary's test `name` again in a child under a umask of
    /// 022, the usual one, and of 000, the widest -- set by `sh` before it
    /// execs the binary -- and the umask when this is that child. The umask
    /// is the process's: set in a test, it would change every file the
    /// tests beside it on cargo's threads make, and theirs this one's. The
    /// child runs the one test (`--exact`) and nothing beside it.
    #[cfg(unix)]
    fn under_umasks(name: &str) -> Option<u32> {
        if let Ok(mask) = std::env::var(UMASK) {
            return Some(u32::from_str_radix(&mask, 8).unwrap());
        }
        for mask in ["022", "000"] {
            let child = std::process::Command::new("/bin/sh")
                .args(["-c", "umask \"$0\" && exec \"$@\"", mask])
                .arg(std::env::current_exe().unwrap())
                .args([name, "--exact", "--test-threads=1"])
                .env(UMASK, mask)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let out = bounded(child, std::time::Duration::from_secs(120));
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && said.contains("1 passed"),
                "under umask {mask}:\n{said}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        None
    }

    /// `child`'s output once it exits, or once `within` has passed, killed:
    /// a child that hangs fails the test with what it printed rather than
    /// hold it, and the run, for ever.
    #[cfg(unix)]
    fn bounded(child: std::process::Child, within: std::time::Duration) -> std::process::Output {
        let pid = child.id().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
        match rx.recv_timeout(within) {
            Ok(out) => out,
            Err(_) => {
                // Not reaped until its output is read, so the id is still its.
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", &pid])
                    .status();
                let out = rx.recv().unwrap();
                panic!(
                    "the child ran past {within:?} and was killed:\n{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn inode(path: &Path) -> u64 {
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap())
    }

    /// Every file and directory fennec-core makes is its owner's alone
    /// whatever the umask -- the database, its sync log, a rewrite's and a
    /// compact beside's side files, a side file a crash left wider -- and
    /// a file there keeps its mode: a database made 0640 for a backup group
    /// is 0640 after a checkpoint, a compact, a compact beside the database
    /// (the auto-compact's) and its sync log's making, and one an earlier
    /// release made 0644 is opened as it is. Up to 0.2.0 each file took the
    /// umask's mode, 0644 under 022 and 0666 under 000, and a rewrite gave
    /// the database the umask's mode in place of its own.
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn files_are_made_their_owners_alone_and_keep_their_mode() {
        use std::os::unix::fs::PermissionsExt;
        let Some(mask) =
            under_umasks("fs::tests::files_are_made_their_owners_alone_and_keep_their_mode")
        else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-modes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The umask is the one asked: a plain create takes it.
        let probe = dir.join("probe");
        File::create(&probe).unwrap();
        assert_eq!(mode(&probe), 0o666 & !mask, "the child's umask");

        // A database made, and its sync log.
        keep_sync_log(true);
        let path = dir.join("new.fennec");
        let mut db = open(&path).unwrap();
        assert_eq!(mode(&path), 0o600, "a database made");
        db.fork(7).unwrap();
        db.sync().unwrap();
        let log = crate::synclog::path_for(&path);
        assert!(log.exists(), "the sync log is made at the first sync");
        // As its open made it, before it took its database's mode: made
        // without the mode on the open, it was the umask's until then.
        assert_eq!(
            crate::synclog::MADE.with(|m| m.take()),
            Some(0o600),
            "a sync log as its open made it"
        );
        assert_eq!(mode(&log), 0o600, "the sync log of a database its owner's");
        drop(db);

        // A rewrite's side file, as it is written.
        let (mut sink, _) = FileSink::open(&path).unwrap();
        let mut seen = None;
        sink.rewrite_with(&mut |out| {
            seen = Some(mode(&beside(&path, "compacting")));
            out.write(&MAGIC[..])
        })
        .unwrap();
        assert_eq!(seen, Some(0o600), "a rewrite's side file");
        assert_eq!(mode(&path), 0o600, "a database rewritten");
        drop(sink);

        // A compact beside's side file, and one a crash left wider: made
        // anew rather than truncated, so no descriptor opened on it before
        // reads what is written now.
        // Held while it is looked at: a side file not adopted removes
        // itself as it goes.
        let side = beside(&path, "beside");
        let made = SideFile::create(&side).unwrap();
        assert_eq!(mode(&side), 0o600, "a compact beside's side file");
        drop(made);
        std::fs::write(&side, b"").unwrap();
        std::fs::set_permissions(&side, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Another user's descriptor, opened while it was wide: it keeps
        // the inode it opened alive, and reads none of the new file.
        let mut held = File::open(&side).unwrap();
        create_private(&side).unwrap().write_all(b"rows").unwrap();
        assert_eq!(mode(&side), 0o600, "a side file left wider");
        let mut read = Vec::new();
        held.read_to_end(&mut read).unwrap();
        assert!(
            read.is_empty(),
            "a descriptor on the side file left reads the new one"
        );
        let held_inode = std::os::unix::fs::MetadataExt::ino(&held.metadata().unwrap());
        assert_ne!(
            inode(&side),
            held_inode,
            "a side file left wider is made anew"
        );
        drop(held);
        std::fs::remove_file(&side).unwrap();

        // A directory made, and one there.
        let made = dir.join("made").join("deeper");
        create_dir_private(&made).unwrap();
        assert_eq!(mode(&made), 0o700, "a directory made");
        assert_eq!(mode(made.parent().unwrap()), 0o700, "a directory above");
        std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o750)).unwrap();
        create_dir_private(&made).unwrap();
        assert_eq!(mode(&made), 0o750, "a directory there keeps its mode");

        // A database an operator gave a backup group: every way it is
        // replaced keeps the mode.
        let path = dir.join("kept.fennec");
        drop(open(&path).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut db = open(&path).unwrap();
        db.fork(7).unwrap();
        db.sync().unwrap();
        let log = crate::synclog::path_for(&path);
        assert_eq!(
            crate::synclog::MADE.with(|m| m.take()),
            Some(0o600),
            "a sync log beside a database made 0640, as its open made it"
        );
        assert_eq!(mode(&log), 0o640, "a sync log takes its database's mode");
        let before = inode(&path);
        db.checkpoint().unwrap();
        assert_ne!(inode(&path), before, "a checkpoint renames an image over");
        assert_eq!(mode(&path), 0o640, "a checkpoint");
        db.fork(8).unwrap();
        db.execute(&crate::query::Statement::Compact(None)).unwrap();
        assert_eq!(mode(&path), 0o640, "a compact");
        // The rewrite let the log go; the first sync after it fsyncs the
        // file and makes another.
        db.fork(9).unwrap();
        db.sync().unwrap();
        assert_eq!(mode(&log), 0o640, "a sync log made after a compact");
        let db = std::sync::RwLock::new(db);
        let mut beside_mode = None;
        let before = inode(&path);
        // Called once the graphs are copied, and again once the side file is
        // made, before its image is written.
        Database::maintain_with(&db, &crate::query::Statement::Compact(None), &mut || {
            if beside(&path, "beside").exists() {
                beside_mode = Some(mode(&beside(&path, "beside")));
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(beside_mode, Some(0o600), "a compact beside's side file");
        assert_ne!(
            inode(&path),
            before,
            "a compact beside renames its image over"
        );
        assert_eq!(mode(&path), 0o640, "a compact beside the database");
        drop(db);

        // One an earlier release made with the umask's mode is opened as
        // it is: never narrowed.
        let path = dir.join("wide.fennec");
        drop(open(&path).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(open(&path).unwrap());
        assert_eq!(mode(&path), 0o644, "a database there keeps its mode");

        keep_sync_log(false);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A group of this process's user other than `made`, the one its files
    /// in that directory are made with, that `path` could be given
    /// (`chgrp`): none where the user is in no other.
    #[cfg(unix)]
    fn another_group(path: &Path, made: u32) -> Option<u32> {
        let out = std::process::Command::new("id").arg("-G").output().ok()?;
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|g| g.parse::<u32>().ok())
            .filter(|&g| g != made)
            .find(|&g| std::os::unix::fs::chown(path, None, Some(g)).is_ok())
    }

    /// A database an operator gave a group's read (`chgrp backup` and
    /// `chmod 640`) is never readable by another group after a rewrite: an
    /// image is made with the process's group (Linux) or its directory's
    /// (macOS), so a checkpoint, a compact and a compact beside the
    /// database each leave it in that group with the group's bits, or with
    /// none. Its sync log too. Up to this fix every rewrite copied the
    /// bits whatever the new file's group: the backup group lost its read
    /// on Linux, and on macOS every user of `staff` gained it. A setgid
    /// directory -- the docs' way to keep a group across rewrites -- makes
    /// every new file of its group, and the database keeps both.
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn a_groups_read_goes_to_no_other_group() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-groups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gid = |p: &Path| std::fs::metadata(p).unwrap().gid();
        let path = dir.join("db.fennec");
        drop(open(&path).unwrap());
        let made = gid(&path);
        let Some(backup) = another_group(&path, made) else {
            eprintln!(
                "skipped a_groups_read_goes_to_no_other_group: the user running the tests is in \
                 no group but {made} (`id -G`), so no file can be given another's"
            );
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        };
        let held = |p: &Path, what: &str| {
            let (m, g) = (mode(p), gid(p));
            match g == backup {
                true => assert_eq!(m, 0o640, "{what}: the group's read is gone"),
                false => assert_eq!(
                    m & 0o070,
                    0,
                    "{what}: mode {m:o} gives group {g} the read given group {backup}"
                ),
            }
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        keep_sync_log(true);
        let mut db = open(&path).unwrap();
        db.fork(7).unwrap();
        db.sync().unwrap();
        held(&crate::synclog::path_for(&path), "a sync log made");
        db.checkpoint().unwrap();
        held(&path, "a checkpoint");
        db.fork(8).unwrap();
        db.execute(&crate::query::Statement::Compact(None)).unwrap();
        held(&path, "a compact");
        let db = std::sync::RwLock::new(db);
        Database::maintain_with(&db, &crate::query::Statement::Compact(None), &mut || {})
            .unwrap()
            .unwrap();
        held(&path, "a compact beside the database");
        drop(db);

        // The recipe: a directory of the group, setgid, keeps the group on
        // every file made in it, and the database the group's read.
        let shared = dir.join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::os::unix::fs::chown(&shared, None, Some(backup)).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o2750)).unwrap();
        let path = shared.join("db.fennec");
        drop(open(&path).unwrap());
        assert_eq!(gid(&path), backup, "a file made in a setgid directory");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut db = open(&path).unwrap();
        db.fork(7).unwrap();
        db.sync().unwrap();
        db.checkpoint().unwrap();
        db.fork(8).unwrap();
        db.execute(&crate::query::Statement::Compact(None)).unwrap();
        drop(db);
        assert_eq!(
            (gid(&path), mode(&path)),
            (backup, 0o640),
            "a database in a setgid directory, compacted"
        );
        keep_sync_log(false);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A link put at the name of a file a database keeps beside it -- a
    /// rewrite's `.compacting`, a compact beside's `.beside`, the sync log
    /// -- is never opened through: the file it names keeps its bytes and
    /// its mode, and the database is renamed over by its image, never by
    /// the link. Up to this fix a side file was opened through the link,
    /// which wrote the image into the file the link named, set its mode
    /// there and renamed the link over the database; the sync log wrote
    /// its zeros into it. A link put there while the image is written --
    /// after the side file was made, before its rename -- is refused, the
    /// database as it was: the image was opened again by its name to be
    /// renamed in, through the link, until `swap_in` took the writer's
    /// handle (`.compacting`) or an open that follows none (`.beside`).
    /// fennec-http's `a_link_at_the_sidecars_name_is_never_followed` holds
    /// a tenant's `.instance` alike.
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn a_link_at_a_side_files_name_is_never_followed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.fennec");
        let elsewhere = |name: &str| {
            let p = dir.join(name);
            std::fs::write(&p, b"another file").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            p
        };
        let untouched = |p: &Path, what: &str| {
            assert_eq!(std::fs::read(p).unwrap(), b"another file", "{what}");
            assert_eq!(mode(p), 0o644, "{what}: its mode");
        };
        keep_sync_log(true);
        let mut db = open(&path).unwrap();
        db.fork(7).unwrap();

        let log = crate::synclog::path_for(&path);
        let target = elsewhere("log-target");
        std::os::unix::fs::symlink(&target, &log).unwrap();
        db.sync().unwrap();
        untouched(&target, "a link at the sync log's name");
        std::fs::remove_file(&log).unwrap();

        let target = elsewhere("compacting-target");
        std::os::unix::fs::symlink(&target, beside(&path, "compacting")).unwrap();
        db.checkpoint().unwrap();
        untouched(&target, "a link at a rewrite's side file");
        assert!(
            std::fs::symlink_metadata(&path).unwrap().is_file(),
            "the database renamed over by a link"
        );
        assert_eq!(mode(&path), 0o600, "the database rewritten");

        let target = elsewhere("beside-target");
        std::os::unix::fs::symlink(&target, beside(&path, "beside")).unwrap();
        db.fork(8).unwrap();
        let db = std::sync::RwLock::new(db);
        Database::maintain_with(&db, &crate::query::Statement::Compact(None), &mut || {})
            .unwrap()
            .unwrap();
        untouched(&target, "a link at a compact beside's side file");
        assert!(std::fs::symlink_metadata(&path).unwrap().is_file());

        // Put at `.beside` once the compact made it, before its image is
        // renamed in: the compact fails, and the database is as it was.
        let target = elsewhere("beside-meanwhile");
        db.write().unwrap().fork(9).unwrap();
        let before = inode(&path);
        let mut planted = false;
        let compacted =
            Database::maintain_with(&db, &crate::query::Statement::Compact(None), &mut || {
                let side = beside(&path, "beside");
                if !planted && side.exists() {
                    std::fs::remove_file(&side).unwrap();
                    std::os::unix::fs::symlink(&target, &side).unwrap();
                    planted = true;
                }
            })
            .unwrap();
        assert!(planted, "the compact made its side file");
        assert!(
            compacted.is_err(),
            "a compact renamed in a link put at its side file's name"
        );
        untouched(
            &target,
            "a link put at a compact beside's side file meanwhile",
        );
        assert!(
            std::fs::symlink_metadata(&path).unwrap().is_file(),
            "the database renamed over by a link"
        );
        assert_eq!(inode(&path), before, "the database as it was");
        drop(db);

        // Put at `.compacting` while a rewrite writes its image.
        let other = dir.join("other.fennec");
        drop(open(&other).unwrap());
        let before = (std::fs::read(&other).unwrap(), inode(&other));
        let (mut sink, _) = FileSink::open(&other).unwrap();
        let target = elsewhere("compacting-meanwhile");
        let rewritten = sink.rewrite_with(&mut |out| {
            let side = beside(&other, "compacting");
            std::fs::remove_file(&side).unwrap();
            std::os::unix::fs::symlink(&target, &side).unwrap();
            out.write(&MAGIC[..])
        });
        assert!(
            rewritten.is_err(),
            "a rewrite renamed in a link put at its side file's name"
        );
        untouched(&target, "a link put at a rewrite's side file meanwhile");
        assert!(
            std::fs::symlink_metadata(&other).unwrap().is_file(),
            "the database renamed over by a link"
        );
        assert_eq!(
            (std::fs::read(&other).unwrap(), inode(&other)),
            before,
            "the database as it was"
        );
        drop(sink);
        keep_sync_log(false);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An image copied for a restore is its owner's alone, follows no link
    /// at its name -- one naming a file, which keeps its bytes and mode,
    /// or one naming none, which stays none -- and comes back open at its
    /// end, the file at the name, whatever was there. On Apple's systems an
    /// image its owner's alone is cloned, and one another user may read,
    /// or its owner not write, copied: a clone has its image's mode from
    /// its making. Without `CLONE_NOFOLLOW` the clone made the file a link
    /// naming none pointed at, as `fs::copy` did.
    #[cfg(unix)]
    #[test]
    fn a_copy_of_an_image_is_cloned_where_it_is_its_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fennecdb-fs-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("image"), dir.join("restoring"));
        let target = dir.join("elsewhere");
        let nowhere = dir.join("nowhere");
        std::fs::write(&target, b"another file").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        for image in [0o600, 0o700, 0o644, 0o400] {
            let _ = std::fs::remove_file(&from);
            std::fs::write(&from, b"image").unwrap();
            std::fs::set_permissions(&from, std::fs::Permissions::from_mode(image)).unwrap();
            for there in ["nothing", "a link to a file", "a link to none", "a file"] {
                let _ = std::fs::remove_file(&to);
                match there {
                    "a link to a file" => std::os::unix::fs::symlink(&target, &to).unwrap(),
                    "a link to none" => std::os::unix::fs::symlink(&nowhere, &to).unwrap(),
                    "a file" => {
                        std::fs::write(&to, b"left by a crash").unwrap();
                        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o644))
                            .unwrap();
                    }
                    _ => {}
                }
                let what = format!("an image {image:o}, {there} at its copy's name");
                #[cfg(target_vendor = "apple")]
                let before = CLONED_HERE.with(|c| c.get());
                let made = copy_private(&from, &to);
                assert!(!nowhere.exists(), "made where a link pointed: {what}");
                let mut f = made.unwrap();
                #[cfg(target_vendor = "apple")]
                assert_eq!(
                    CLONED_HERE.with(|c| c.get()) - before,
                    usize::from(image & 0o077 == 0 && image & 0o600 == 0o600),
                    "cloned: {what}"
                );
                assert_eq!(std::fs::read(&target).unwrap(), b"another file", "{what}");
                assert_eq!(mode(&target), 0o644, "{what}");
                assert!(std::fs::symlink_metadata(&to).unwrap().is_file(), "{what}");
                assert_eq!(mode(&to), 0o600, "{what}");
                assert_eq!(
                    std::os::unix::fs::MetadataExt::ino(&f.metadata().unwrap()),
                    inode(&to),
                    "the handle is the file at the name: {what}"
                );
                f.write_all(b" and its writes").unwrap();
                f.seek(SeekFrom::Start(0)).unwrap();
                let mut read = Vec::new();
                f.read_to_end(&mut read).unwrap();
                assert_eq!(read, b"image and its writes", "{what}");
                assert_eq!(std::fs::read(&from).unwrap(), b"image", "{what}");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
