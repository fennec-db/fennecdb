//! What a file's sink does differently on Windows (docs/design/windows.md,
//! decisions 1, 2, 3 and 7e): the file mapped in views laid end to end
//! ([`map`]), a torn tail cut once its views are gone, a rename made
//! durable -- the renamed file flushed, then its directory -- a rename
//! another handle refuses tried again for a bounded time, a file fennec
//! makes kept out of the search indexer, and the volume a file is on named,
//! which says whether it is mapped. kernel32's calls, and ntdll's one,
//! are declared here, as `fennec_abi::lock` declares its own: the crates
//! take no `windows-sys`.
//! Every call takes a handle std opened, never a path, so a long path is
//! std's `\\?\` one wherever it is used.

#[cfg(target_pointer_width = "64")]
mod map;
#[cfg(target_pointer_width = "64")]
pub use map::{assume_room, placeholders, touch, Mapping};

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, Instant};

/// Lets `CreateFileW` open a directory, which it refuses otherwise.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_ATTRIBUTE_NOT_CONTENT_INDEXED: u32 = 0x2000;
/// winnt.h's: the volume renames over, and deletes, a file other handles
/// hold open, the name gone at once. `GetVolumeInformationByHandleW`'s page
/// does not list it; NTFS reports it (`the_experiment_on_each_volume`).
const FILE_SUPPORTS_POSIX_UNLINK_RENAME: u32 = 0x400;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// How long one swap, or one `sync_dir`, tries again what another handle
/// refuses -- an indexer's, a backup agent's, an antivirus's, a cloud
/// folder's sync, opened without `FILE_SHARE_DELETE` or `FILE_SHARE_WRITE`
/// -- before a swap fails the sink as any failed rename does: one deadline
/// for every try of the call, the rename's, the open's after it and the
/// flushes' opens, so a write waiting on a checkpoint waits about a second
/// at most, never a second a try. Such a handle is held while a file is
/// read.
pub(crate) const REFUSED_FOR: Duration = Duration::from_secs(1);

/// The longest wait between two tries.
const RETRY_AT_MOST: Duration = Duration::from_millis(50);

#[link(name = "kernel32")]
extern "system" {
    fn GetVolumeInformationByHandleW(
        file: *mut c_void,
        volume_name: *mut u16,
        volume_name_len: u32,
        serial: *mut u32,
        max_component: *mut u32,
        flags: *mut u32,
        file_system: *mut u16,
        file_system_len: u32,
    ) -> i32;
    fn SetFileInformationByHandle(
        file: *mut c_void,
        class: i32,
        info: *const c_void,
        len: u32,
    ) -> i32;
    fn GetFileInformationByHandleEx(
        file: *mut c_void,
        class: i32,
        info: *mut c_void,
        len: u32,
    ) -> i32;
}

/// Refused by another handle's share mode, or by a delete pending on the
/// name, and so worth a second try: a refused rename has written nothing,
/// where a failed flush may have dropped what it could not write and is
/// never tried again (docs/design/durability.md#storage-errors).
fn refused(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED)
    )
}

/// `op` tried again while another handle refuses it, until `until`, waiting
/// a millisecond and then twice as long each time, up to [`RETRY_AT_MOST`]
/// and never past `until`: the error once the time is out. A caller making
/// several calls passes each the one deadline, so they share
/// [`REFUSED_FOR`] rather than take it each.
fn tried_again<T>(until: Instant, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(test)]
    tests::DEADLINES.with(|d| d.borrow_mut().push(until));
    let mut wait = Duration::from_millis(1);
    loop {
        match op() {
            Err(e) if refused(&e) => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(e);
                }
                #[cfg(test)]
                tests::REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(wait.min(left));
                wait = (wait * 2).min(RETRY_AT_MOST);
            }
            done => return done,
        }
    }
}

/// What a flush did: flushed, or not run because its open, or a
/// directory's flush, was refused -- taken as nothing to flush.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum Flushed {
    Done,
    Refused,
}

/// Opens `path` -- a file or a directory -- to flush it: write access,
/// which `FlushFileBuffers` asks, every share mode, as std's opens keep;
/// tried again while another handle refuses it for sharing, until `until`.
/// `None` where it is refused all the same: for access -- a directory some
/// systems will not open for write, a read-only file -- or for sharing by
/// a handle that does not share write, as many a reader opens one. Neither
/// is a storage error: nothing was written, nothing dropped, and before
/// slice 1 nothing was flushed there at all. So the flush is skipped, as
/// a refusal for access is, rather than another process's reader failing
/// a tenant's create or a sidecar's write.
fn flushable(path: &Path, until: Instant) -> io::Result<Option<File>> {
    let opened = tried_again(until, || {
        match OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
        {
            // Not worth a second try: a handle or an ACL, not a moment.
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(None),
            opened => opened.map(Some),
        }
    });
    match opened {
        Err(e) if refused(&e) => Ok(None),
        opened => opened,
    }
}

/// `FlushFileBuffers` (std's `sync_all`). A refusal for access is taken as
/// nothing to flush where `f` is a directory, as Lucene.NET and OpenJDK
/// take one (decision 3); a file, which the handle may write, has no such
/// leave: there it is the file's own failure, which the sink's next flush
/// through it would meet, and the storage error it is.
fn flush(f: &File) -> io::Result<Flushed> {
    match f.sync_all() {
        Ok(()) => Ok(Flushed::Done),
        Err(e)
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED)
                && f.metadata().is_ok_and(|m| m.is_dir()) =>
        {
            Ok(Flushed::Refused)
        }
        Err(e) => Err(e),
    }
}

/// The directory `path` is in, flushed: where NTFS keeps the name a rename
/// gave, which its lazily written log could otherwise lose to a power cut.
fn flush_parent(path: &Path, until: Instant) -> io::Result<Flushed> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let flushed = match flushable(dir, until)? {
        Some(d) => flush(&d)?,
        None => Flushed::Refused,
    };
    #[cfg(test)]
    tests::FLUSHES.with(|n| n.set((n.get().0 + 1, n.get().1)));
    Ok(flushed)
}

/// The renamed file flushed through a handle open for write, which
/// `MOVEFILE_WRITE_THROUGH` does once it has moved it. No page says which
/// of the two flushes carries NTFS's log for a rename, so both run -- once
/// a rename, never a write.
fn flush_renamed(f: &File) -> io::Result<Flushed> {
    let flushed = flush(f)?;
    #[cfg(test)]
    tests::FLUSHES.with(|n| n.set((n.get().0, n.get().1 + 1)));
    Ok(flushed)
}

/// `fs::sync_dir` on Windows: the renamed file at `path` flushed, where it
/// is there -- a removal's is not -- and its open is not refused
/// ([`flushable`]), and then its directory, both opens held to one
/// deadline.
pub(super) fn sync_dir(path: &Path) -> io::Result<()> {
    let until = Instant::now() + REFUSED_FOR;
    let opened = match flushable(path, until) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        opened => opened?,
    };
    if let Some(f) = opened {
        flush_renamed(&f)?;
    }
    flush_parent(path, until)?;
    Ok(())
}

/// The directory `path` is in flushed, and not `path`: `fs::sync_dir` for
/// a caller that flushed each file it renamed there through the handle it
/// wrote it with -- fennec-http's archive, whose renames one flush of the
/// directory covers before what stands on them.
pub fn sync_dir_alone(path: &Path) -> io::Result<()> {
    flush_parent(path, Instant::now() + REFUSED_FOR)?;
    Ok(())
}

/// A rewrite's image renamed over the database's file -- std's rename,
/// which replaces it with POSIX semantics where the volume has them --
/// then opened to be appended to, flushed through that handle, and its
/// directory flushed: what `swap_in` does on a Unix with `rename`,
/// `sync_dir` and an open. The rename, the open after it and the
/// directory's open are tried again while another handle refuses them,
/// within one [`REFUSED_FOR`] for all three.
pub(super) fn swap(from: &Path, to: &Path) -> io::Result<File> {
    if let Ok(image) = OpenOptions::new()
        .access_mode(FILE_RW_ATTRIBUTES)
        .open(from)
    {
        mark_unindexed(&image);
    }
    let until = Instant::now() + REFUSED_FOR;
    tried_again(until, || std::fs::rename(from, to))?;
    let f = tried_again(until, || OpenOptions::new().read(true).write(true).open(to))?;
    flush_renamed(&f)?;
    flush_parent(to, until)?;
    Ok(f)
}

/// `FILE_BASIC_INFO`: the times (0 leaves one as it is) and the attributes.
#[repr(C)]
struct BasicInfo {
    times: [i64; 4],
    attributes: u32,
}

/// `SetFileInformationByHandle`'s class for [`BasicInfo`].
const FILE_BASIC_INFO: i32 = 0;
/// Access to read and to write a file's attributes, and nothing else.
const FILE_RW_ATTRIBUTES: u32 = 0x0080 | 0x0100;
/// Valid only alone, so never kept beside another attribute.
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

/// The file `f` is open on kept out of Windows Search's content index --
/// one handle fewer that may refuse a rename over it -- through a handle
/// that may read and write its attributes: a database's file as its open
/// makes it, a rewrite's image before it is renamed into the database's
/// place. `CreateFileW` takes no such attribute at the making (CI found
/// it unset), so it is set after. Advice, as `madvise` is on a Unix:
/// refused, the file is indexed as before.
pub(super) fn mark_unindexed(f: &File) {
    use std::os::windows::fs::MetadataExt;
    let Ok(was) = f.metadata().map(|m| m.file_attributes()) else {
        return;
    };
    let info = BasicInfo {
        times: [0; 4],
        attributes: (was & !FILE_ATTRIBUTE_NORMAL) | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED,
    };
    // SAFETY: the handle is open across the call, `info` is the struct the
    // class names, and its size is the one passed.
    unsafe {
        SetFileInformationByHandle(
            f.as_raw_handle(),
            FILE_BASIC_INFO,
            &info as *const BasicInfo as *const c_void,
            std::mem::size_of::<BasicInfo>() as u32,
        )
    };
}

/// The file system a database's file is on, as `GetVolumeInformationByHandleW`
/// names it: what a Windows server says at its start (docs/design/windows.md,
/// decision 3).
#[derive(Debug, Clone)]
pub struct Volume {
    /// `NTFS`, `ReFS`, `FAT32`, `exFAT`, ...
    pub file_system: String,
    /// The volume's flags, `FILE_SUPPORTS_*`.
    pub flags: u32,
    /// Reached through a network redirector -- an SMB share, a mapped
    /// drive -- which names the server's file system and flags as its own.
    pub remote: bool,
}

impl Volume {
    /// Whether a rename replaces a file other handles hold open, and a
    /// delete takes its name at once. Where not -- FAT32, exFAT, many a
    /// network share -- std's rename falls back to the classic one, which
    /// another handle on the file refuses: a checkpoint or a compact under
    /// the database's own handle fails there.
    pub fn posix_renames(&self) -> bool {
        self.flags & FILE_SUPPORTS_POSIX_UNLINK_RENAME != 0
    }
}

/// The volume `path` is on: the file's, or, where it is not made yet, the
/// directory's it will be made in.
pub fn volume(path: &Path) -> io::Result<Volume> {
    // For no access at all, as std's `metadata` opens: the query asks none.
    let open = |p: &Path| {
        OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
    };
    let f = match open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => open(d)?,
            _ => open(Path::new("."))?,
        },
        Err(e) => return Err(e),
    };
    let mut flags = 0u32;
    // MAX_PATH + 1, the length the call's page asks of the buffer.
    let mut name = [0u16; 261];
    // SAFETY: the handle is the file's, open across the call; the pointers
    // the call fills are `flags` and `name`, whose length it is told, and
    // the ones it is passed null it skips, as its page says.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            f.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Ok(Volume {
        file_system: String::from_utf16_lossy(&name[..len]),
        flags,
        remote: remote(&f),
    })
}

/// `GetFileInformationByHandleEx`'s class for `FILE_REMOTE_PROTOCOL_INFO`.
const FILE_REMOTE_PROTOCOL_INFO: i32 = 13;
/// `NtQueryVolumeInformationFile`'s class for `FILE_FS_DEVICE_INFORMATION`.
const FILE_FS_DEVICE_INFORMATION: i32 = 4;
/// A device's characteristic: a network file system's, the redirector's.
const FILE_REMOTE_DEVICE: u32 = 0x10;

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryVolumeInformationFile(
        file: *mut c_void,
        status: *mut [usize; 2],
        info: *mut c_void,
        len: u32,
        class: i32,
    ) -> i32;
}

/// Whether `f` is reached through a network redirector -- an SMB share,
/// a mapped drive: where the device its volume is on says it is remote,
/// or the redirector answers `FILE_REMOTE_PROTOCOL_INFO`, a local file
/// system refusing the class. The device first: on `windows-latest` the
/// handle `volume` opened through `\\localhost\C$` was refused the class
/// in one run, and every handle answered it in the next, where the device
/// said remote each time (`a_file_is_mapped_where_its_volume_allows`).
fn remote(f: &File) -> bool {
    remote_device(f) || remote_protocol(f).is_ok()
}

/// Whether the device `f`'s volume is on is a network file system's
/// (`FILE_FS_DEVICE_INFORMATION`'s `FILE_REMOTE_DEVICE`): the I/O manager
/// answers it from the device, asking no server.
fn remote_device(f: &File) -> bool {
    let mut status = [0usize; 2];
    let mut info = [0u32; 2];
    // SAFETY: the handle is open across the call; `status` is an
    // `IO_STATUS_BLOCK`, two pointers wide, and `info` the class's struct,
    // a device type and its characteristics.
    let done = unsafe {
        NtQueryVolumeInformationFile(
            f.as_raw_handle(),
            &mut status,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as u32,
            FILE_FS_DEVICE_INFORMATION,
        )
    };
    done >= 0 && info[1] & FILE_REMOTE_DEVICE != 0
}

/// `FILE_REMOTE_PROTOCOL_INFO`, 116 bytes, where the redirector answers it.
fn remote_protocol(f: &File) -> io::Result<[u32; 29]> {
    let mut info = [0u32; 29];
    // SAFETY: the handle is open across the call, and `info` is the
    // struct's 116 bytes, 4-aligned, as the class asks.
    let done = unsafe {
        GetFileInformationByHandleEx(
            f.as_raw_handle(),
            FILE_REMOTE_PROTOCOL_INFO,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// Whether a database's file at `path` is mapped (decision 1): where its
/// volume renames over a file other handles hold open, NTFS and ReFS, and
/// is not a share. Elsewhere it is read into memory, as `--no-mmap` reads
/// it: on FAT32 or exFAT a view would refuse a rename over its file as a
/// handle does, and a share's read error is the process's end through a
/// view (`EXCEPTION_IN_PAGE_ERROR`) where a read answers it as an error. A
/// volume that cannot be asked is not mapped either.
pub fn mappable(path: &Path) -> bool {
    volume(path).is_ok_and(|v| v.posix_renames() && !v.remote)
}

/// A mapped open that found a torn tail: Windows cuts no file a view of it
/// maps (`SetEndOfFile`'s `ERROR_USER_MAPPED_FILE`), and the open maps
/// before it reads where the records end, so the database and its views
/// go, the file is cut to the `whole` bytes its records fill, and it opens
/// again -- a crash's tail costs a second open -- as the first was set up:
/// `deferred`, a server's, its vectors left to link beside the queries
/// (`defer_linking`, which the first load cleared). Another view of the
/// file refuses the cut, and the open fails with it, saying so, until the
/// view goes: another process's -- a reader held over a file whose writer
/// crashed -- or this one's, a stream or a pinned read still reading the
/// database a storage error stopped, a `--dir` tenant opened again. Once
/// an open: the second open finding a tail to cut fails rather than cut
/// again.
#[cfg(target_pointer_width = "64")]
pub(super) fn cut_and_open(
    db: crate::engine::Database,
    deferred: bool,
    mut sink: super::FileSink,
    whole: usize,
    wrap: Box<super::Wrap<'_>>,
) -> crate::error::Result<crate::engine::Database> {
    // What the load cut, said by the open that cut it: the open after the
    // cut finds nothing to cut, and the server's log and a test's
    // `cut_on_open` read None on Windows alone.
    let said = db.cut_on_open().cloned();
    drop(db);
    sink.mapping = None;
    if CUTTING.with(|c| c.get()) {
        return Err(crate::error::Error::Io(
            "the file's last record was cut short again after its torn tail was cut".into(),
        ));
    }
    // The mappings this process let go of before, whose views a thread
    // may still be letting go ([`map::unmapped`]).
    map::unmapped();
    // Through the sink's handle, as its `cut` does after, for the error's
    // number: the system's words for it name no view.
    let cut = super::lock(&sink.disk).file.set_len(whole as u64);
    if let Err(e) = cut {
        return Err(match e.raw_os_error() {
            Some(ERROR_USER_MAPPED_FILE) => crate::error::Error::Io(format!(
                "the file's last record was cut short, and Windows cuts no file a view \
                 maps: another process maps it, or a stream or a pinned read of this one \
                 still reads it, and it opens once they let it go ({e})"
            )),
            _ => e.into(),
        });
    }
    sink.cut(whole)?;
    let path = sink.path().to_path_buf();
    drop(sink);
    let mut db = crate::engine::Database::new();
    if deferred {
        db.defer_linking();
    }
    // Set across the second open, whatever it ends in.
    struct Cutting;
    impl Drop for Cutting {
        fn drop(&mut self) {
            CUTTING.with(|c| c.set(false));
        }
    }
    CUTTING.with(|c| c.set(true));
    let _cutting = Cutting;
    let mut db = super::open_mapped_into(db, path, wrap)?;
    db.said_cut(said);
    Ok(db)
}

/// `SetEndOfFile`'s refusal of a file a view maps.
#[cfg(target_pointer_width = "64")]
const ERROR_USER_MAPPED_FILE: i32 = 1224;

#[cfg(target_pointer_width = "64")]
thread_local! {
    /// Set while [`cut_and_open`] opens the file it cut.
    static CUTTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::engine::Sink;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::path::PathBuf;
    use std::process::{Child, ChildStdout, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Renames and opens [`tried_again`] took a refusal for, process-wide:
    /// what a test letting a handle go once one was refused waits for.
    pub(in crate::fs) static REFUSALS: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        /// The deadline of each [`tried_again`] this thread ran, in turn.
        pub(in crate::fs) static DEADLINES: std::cell::RefCell<Vec<Instant>> =
            const { std::cell::RefCell::new(Vec::new()) };
        /// Flushes this thread ran: of a renamed file's directory, and of
        /// the renamed file.
        pub(in crate::fs) static FLUSHES: std::cell::Cell<(usize, usize)> =
            const { std::cell::Cell::new((0, 0)) };
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileMappingW(
            file: *mut c_void,
            attributes: *mut c_void,
            protect: u32,
            size_high: u32,
            size_low: u32,
            name: *const u16,
        ) -> *mut c_void;
        fn MapViewOfFile(
            section: *mut c_void,
            access: u32,
            offset_high: u32,
            offset_low: u32,
            len: usize,
        ) -> *mut c_void;
        fn UnmapViewOfFile(at: *const c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }

    const PAGE_READONLY: u32 = 0x02;
    const FILE_MAP_READ: u32 = 0x04;
    const DELETE: u32 = 0x0001_0000;
    const FILE_DISPOSITION_INFO_EX: i32 = 21;
    const FILE_RENAME_INFO_EX: i32 = 22;
    const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;
    const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x1;
    const FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: u32 = 0x2;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: std's.
    const SHARE_ALL: u32 = 0x7;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE`: an opener that refuses others
    /// a delete or a rename.
    const SHARE_NO_DELETE: u32 = 0x3;
    /// `FILE_SHARE_READ` alone: a reader that refuses others a write too.
    const SHARE_READ: u32 = 0x1;

    /// Roots of other volumes to run [`the_experiment_on_each_volume`] on as
    /// well, `;`-separated: CI's ReFS and FAT32 VHDs.
    const VOLUMES: &str = "FENNEC_TEST_VOLUMES";
    const MAP_FOR: &str = "FENNEC_TEST_MAP_FOR";
    const MAP_SHARE: &str = "FENNEC_TEST_MAP_SHARE";
    const MAPPED: &str = "mapped, until stdin ends";

    const OLD: &[u8] = b"the file a view maps, as a database's own";
    const NEW: &[u8] = b"the image a rewrite renames over it";

    /// A read-only view of a whole file, as slice 2 maps one: a section
    /// made from the file's handle at its length, closed once the view is
    /// made, since a view holds its section.
    struct View {
        at: *mut c_void,
        len: usize,
    }

    impl View {
        fn of(file: &File) -> View {
            let len = file.metadata().unwrap().len() as usize;
            // SAFETY: the handle is the file's, open across both calls; the
            // section's handle is closed once the view holds it, and the
            // view is unmapped in `drop`.
            unsafe {
                let (none, unnamed) = (std::ptr::null_mut(), std::ptr::null());
                let section =
                    CreateFileMappingW(file.as_raw_handle(), none, PAGE_READONLY, 0, 0, unnamed);
                assert!(!section.is_null(), "{}", io::Error::last_os_error());
                let at = MapViewOfFile(section, FILE_MAP_READ, 0, 0, 0);
                let e = io::Error::last_os_error();
                CloseHandle(section);
                assert!(!at.is_null(), "{e}");
                View { at, len }
            }
        }

        fn bytes(&self) -> &[u8] {
            // SAFETY: the view maps `len` bytes from `at` until `drop`.
            unsafe { std::slice::from_raw_parts(self.at as *const u8, self.len) }
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            // SAFETY: `at` is the view `of` mapped, unmapped once.
            unsafe { UnmapViewOfFile(self.at) };
        }
    }

    fn opened(path: &Path, share: u32) -> File {
        OpenOptions::new()
            .read(true)
            .share_mode(share)
            .open(path)
            .unwrap()
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().collect()
    }

    /// `from` renamed to `to` through `SetFileInformationByHandle`'s
    /// `FileRenameInfoEx`, `flags` its `FILE_RENAME_FLAG_*`: the call std's
    /// rename makes first.
    fn renamed_by_handle(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
        let f = OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(SHARE_ALL)
            .open(from)?;
        let name = wide(to);
        // FILE_RENAME_INFO: the flags, 4 bytes of padding, the root
        // directory's handle, the name's length in bytes, and the name from
        // byte 20, ended by a NUL the length leaves out.
        let len = 20 + 2 * (name.len() + 1);
        let mut info = vec![0u64; len.div_ceil(8)];
        let p = info.as_mut_ptr() as *mut u8;
        // SAFETY: `info` holds `len` bytes, 8-aligned; each write is within
        // it at its field's offset and alignment.
        unsafe {
            (p as *mut u32).write(flags);
            (p.add(16) as *mut u32).write((name.len() * 2) as u32);
            std::ptr::copy_nonoverlapping(name.as_ptr(), p.add(20) as *mut u16, name.len());
        }
        // SAFETY: the handle is open across the call, `info` is the struct
        // the class names, `len` bytes long.
        let ok = unsafe {
            SetFileInformationByHandle(
                f.as_raw_handle(),
                FILE_RENAME_INFO_EX,
                p as *const c_void,
                len as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `path` deleted through `FileDispositionInfoEx`, `flags` its
    /// `FILE_DISPOSITION_FLAG_*`.
    fn deleted_by_handle(path: &Path, flags: u32) -> io::Result<()> {
        let f = OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(SHARE_ALL)
            .open(path)?;
        // SAFETY: the handle is open across the call; the struct is one
        // `u32`, its flags.
        let ok = unsafe {
            SetFileInformationByHandle(
                f.as_raw_handle(),
                FILE_DISPOSITION_INFO_EX,
                &flags as *const u32 as *const c_void,
                4,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The classic rename over: `MoveFileExW` with
    /// `MOVEFILE_REPLACE_EXISTING`, the one #453 found refused by any
    /// other handle on the target.
    fn moved(from: &Path, to: &Path) -> io::Result<()> {
        let (mut from, mut to) = (wide(from), wide(to));
        from.push(0);
        to.push(0);
        // SAFETY: both names are NUL-ended and outlive the call.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum Holder {
        Nothing,
        /// A handle open on the file, opened with this share mode.
        Handle(u32),
        /// A view of the file in this process, its file opened with this
        /// share mode, the handle kept or closed once the view is made.
        View(u32, bool),
        /// A view in another process, its handle closed.
        Process(u32),
    }

    const HOLDERS: [(&str, Holder); 9] = [
        ("nothing", Holder::Nothing),
        ("a handle", Holder::Handle(SHARE_ALL)),
        (
            "a handle without FILE_SHARE_DELETE",
            Holder::Handle(SHARE_NO_DELETE),
        ),
        ("a view, its handle closed", Holder::View(SHARE_ALL, false)),
        (
            "a view, its handle closed, opened without FILE_SHARE_DELETE",
            Holder::View(SHARE_NO_DELETE, false),
        ),
        ("a view and its handle", Holder::View(SHARE_ALL, true)),
        (
            "a view and its handle without FILE_SHARE_DELETE",
            Holder::View(SHARE_NO_DELETE, true),
        ),
        ("a view in another process", Holder::Process(SHARE_ALL)),
        (
            "a view in another process, opened without FILE_SHARE_DELETE",
            Holder::Process(SHARE_NO_DELETE),
        ),
    ];

    #[derive(Clone, Copy, PartialEq)]
    enum Op {
        /// The image renamed over the file, `FileRenameInfoEx` with POSIX
        /// semantics and replace-if-exists: what decision 2 asks.
        PosixOver,
        /// std's rename over it, as `swap_in` makes it.
        StdOver,
        /// `MoveFileExW` over it.
        ClassicOver,
        /// The file deleted, `FileDispositionInfoEx` with POSIX semantics.
        PosixDelete,
        /// std's `remove_file`.
        StdRemove,
        /// The file renamed away from its name, POSIX semantics: the
        /// retired swap's first rename.
        PosixAway,
        /// std's rename of the file away.
        StdAway,
    }

    const OPS: [(&str, Op); 7] = [
        ("POSIX rename over", Op::PosixOver),
        ("std rename over", Op::StdOver),
        ("MoveFileEx over", Op::ClassicOver),
        ("POSIX delete", Op::PosixDelete),
        ("std remove", Op::StdRemove),
        ("POSIX rename away", Op::PosixAway),
        ("std rename away", Op::StdAway),
    ];

    /// What Windows did, a row a holder in [`HOLDERS`]' order, a column an
    /// operation in [`OPS`]': `ok`; `ok, pending` for a delete whose name
    /// stays until the last handle goes; `e<n>` for a refusal, its Win32
    /// error (5 access denied, 32 sharing violation, 87 invalid
    /// parameter). Asserted, so a Windows update that changes one fails CI
    /// before a server meets it (docs/design/windows.md, decision 2, which
    /// holds the tables as `windows-latest` answered them).
    type Table = [[&'static str; 7]; 9];

    /// A read-only view refuses nothing, in this process or another,
    /// whatever its opener shared: neither a rename over the file, POSIX or
    /// classic, nor a delete, nor a rename away. Only a handle does -- the
    /// classic rename over any, everything over one not sharing delete.
    const NTFS: Table = [
        ["ok", "ok", "ok", "ok", "ok", "ok", "ok"],
        ["ok", "ok", "e5", "ok", "ok", "ok", "ok"],
        ["e32", "e5", "e5", "e32", "e32", "e32", "e32"],
        ["ok", "ok", "ok", "ok", "ok", "ok", "ok"],
        ["ok", "ok", "ok", "ok", "ok", "ok", "ok"],
        ["ok", "ok", "e5", "ok", "ok", "ok", "ok"],
        ["e32", "e5", "e5", "e32", "e32", "e32", "e32"],
        ["ok", "ok", "ok", "ok", "ok", "ok", "ok"],
        ["ok", "ok", "ok", "ok", "ok", "ok", "ok"],
    ];

    /// ReFS answers as NTFS, cell for cell.
    const REFS: Table = NTFS;

    /// No POSIX semantics (87); std's rename and delete fall back to the
    /// classic ones, which a view refuses as a handle does, but for a
    /// rename of the file away.
    const FAT32: Table = [
        ["e87", "ok", "ok", "e87", "ok", "e87", "ok"],
        ["e87", "e5", "e5", "e87", "ok, pending", "e87", "ok"],
        ["e87", "e5", "e5", "e32", "e32", "e32", "e32"],
        ["e87", "e5", "e5", "e87", "e5", "e87", "ok"],
        ["e87", "e5", "e5", "e87", "e5", "e87", "ok"],
        ["e87", "e5", "e5", "e87", "e5", "e87", "ok"],
        ["e87", "e5", "e5", "e32", "e32", "e32", "e32"],
        ["e87", "e5", "e5", "e87", "e5", "e87", "ok"],
        ["e87", "e5", "e5", "e87", "e5", "e87", "ok"],
    ];

    /// What each file system answers: its table, whether it reports POSIX
    /// renames, and what a flush of a directory does there.
    fn expected(file_system: &str) -> Option<(&'static Table, bool, &'static str)> {
        match file_system {
            "NTFS" => Some((&NTFS, true, "Done")),
            "ReFS" => Some((&REFS, true, "Done")),
            "FAT32" => Some((&FAT32, false, "Done")),
            _ => None,
        }
    }

    /// What holds a row's files, let go of when dropped.
    #[derive(Default)]
    struct Held {
        views: Vec<Option<View>>,
        handles: Vec<File>,
        other: Option<(Child, BufReader<ChildStdout>)>,
    }

    impl Drop for Held {
        fn drop(&mut self) {
            self.views.clear();
            self.handles.clear();
            if let Some((mut child, _)) = self.other.take() {
                drop(child.stdin.take());
                let _ = child.wait();
            }
        }
    }

    fn hold(holder: Holder, files: &[PathBuf]) -> Held {
        let mut held = Held::default();
        match holder {
            Holder::Nothing => {}
            Holder::Handle(share) => {
                held.handles = files.iter().map(|f| opened(f, share)).collect()
            }
            Holder::View(share, keep) => {
                for f in files {
                    let file = opened(f, share);
                    held.views.push(Some(View::of(&file)));
                    if keep {
                        held.handles.push(file);
                    }
                }
            }
            Holder::Process(share) => held.other = Some(mapped_elsewhere(files, share)),
        }
        held
    }

    /// Another process -- this test binary again -- mapping each of
    /// `files`, opened with `share`, its handles closed, until its stdin
    /// ends.
    fn mapped_elsewhere(files: &[PathBuf], share: u32) -> (Child, BufReader<ChildStdout>) {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fs::windows::tests::maps_for_another_process"])
            .args(["--nocapture", "--test-threads=1"])
            .env(MAP_FOR, std::env::join_paths(files).unwrap())
            .env(MAP_SHARE, share.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut said = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains(MAPPED) {
            line.clear();
            let read = said.read_line(&mut line).unwrap();
            assert!(read > 0, "the other process ended before it mapped");
        }
        (child, said)
    }

    /// The other process [`mapped_elsewhere`] starts; nothing in a test run
    /// of its own.
    #[test]
    fn maps_for_another_process() {
        let Some(files) = std::env::var_os(MAP_FOR) else {
            return;
        };
        let share = std::env::var(MAP_SHARE).unwrap().parse().unwrap();
        let views: Vec<View> = std::env::split_paths(&files)
            .map(|f| View::of(&opened(&f, share)))
            .collect();
        println!("{MAPPED}");
        std::io::stdout().flush().unwrap();
        let mut rest = String::new();
        let _ = std::io::stdin().read_line(&mut rest);
        drop(views);
    }

    /// The names `dir` lists: a file whose delete is pending is among them.
    fn listed(dir: &Path) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect()
    }

    fn run(op: Op, file: &Path, image: &Path, away: &Path) -> io::Result<()> {
        let posix = FILE_RENAME_FLAG_POSIX_SEMANTICS | FILE_RENAME_FLAG_REPLACE_IF_EXISTS;
        match op {
            Op::PosixOver => renamed_by_handle(image, file, posix),
            Op::StdOver => std::fs::rename(image, file),
            Op::ClassicOver => moved(image, file),
            Op::PosixDelete => deleted_by_handle(
                file,
                FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            ),
            Op::StdRemove => std::fs::remove_file(file),
            Op::PosixAway => renamed_by_handle(file, away, posix),
            Op::StdAway => std::fs::rename(file, away),
        }
    }

    /// What `op` did to `file`, told as [`Table`] tells it; `ok?` where it
    /// answered success and the names say otherwise, `view changed` where
    /// a view in this process no longer reads the file it mapped.
    fn outcome(
        op: Op,
        done: io::Result<()>,
        file: &Path,
        away: &Path,
        view: Option<&View>,
    ) -> String {
        if let Err(e) = done {
            return format!("e{}", e.raw_os_error().unwrap_or(-1));
        }
        let names = listed(file.parent().unwrap());
        let named = |p: &Path| names.iter().any(|n| Some(n.as_os_str()) == p.file_name());
        let mut said = match op {
            Op::PosixOver | Op::StdOver | Op::ClassicOver => {
                if std::fs::read(file).ok().as_deref() == Some(NEW) {
                    "ok"
                } else {
                    "ok?"
                }
            }
            Op::PosixDelete | Op::StdRemove => {
                if named(file) {
                    "ok, pending"
                } else {
                    "ok"
                }
            }
            Op::PosixAway | Op::StdAway => {
                if named(away) && !named(file) {
                    "ok"
                } else {
                    "ok?"
                }
            }
        }
        .to_string();
        if view.is_some_and(|v| v.bytes() != OLD) {
            said.push_str(", view changed");
        }
        said
    }

    /// Every operation against every holder, in `dir`.
    fn matrix(dir: &Path) -> Vec<Vec<String>> {
        let mut rows = Vec::new();
        for (h, &(_, holder)) in HOLDERS.iter().enumerate() {
            let named = |o: usize, ext: &str| dir.join(format!("{h}-{o}.{ext}"));
            let files: Vec<PathBuf> = (0..OPS.len()).map(|o| named(o, "fennec")).collect();
            for (o, file) in files.iter().enumerate() {
                std::fs::write(file, OLD).unwrap();
                std::fs::write(named(o, "image"), NEW).unwrap();
            }
            let held = hold(holder, &files);
            let row = OPS
                .iter()
                .enumerate()
                .map(|(o, &(_, op))| {
                    let away = named(o, "retired");
                    let done = run(op, &files[o], &named(o, "image"), &away);
                    let view = held.views.get(o).and_then(Option::as_ref);
                    outcome(op, done, &files[o], &away, view)
                })
                .collect();
            drop(held);
            rows.push(row);
        }
        rows
    }

    /// Decision 2's experiment (docs/design/windows.md): whether a
    /// read-only view of a file refuses a rename over it -- in this
    /// process and another, its opener sharing delete or not -- beside
    /// std's rename, the classic one, a delete and a rename of the file
    /// away, each against no holder, a handle and a view; and on each
    /// volume, what it names itself, whether it reports POSIX renames and
    /// what a directory's flush does. On the temporary directory's volume,
    /// and on each of [`VOLUMES`] -- CI's ReFS and FAT32 VHDs where the job
    /// could make them. Every answer is printed, and held to what Windows
    /// did when this was written.
    #[test]
    fn the_experiment_on_each_volume() {
        let mut roots = vec![std::env::temp_dir()];
        if let Some(more) = std::env::var_os(VOLUMES) {
            roots.extend(std::env::split_paths(&more).filter(|p| !p.as_os_str().is_empty()));
        }
        let mut wrong = Vec::new();
        for root in roots {
            let dir = root.join(format!("fennec-experiment-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = std::fs::canonicalize(&dir).unwrap();
            let vol = volume(&dir).unwrap();
            let dir_flush = match flush_parent(&dir.join("x"), Instant::now() + REFUSED_FOR) {
                Ok(f) => format!("{f:?}"),
                Err(e) => format!("e{}", e.raw_os_error().unwrap_or(-1)),
            };
            let seen = matrix(&dir);
            // One print a volume, which the other tests' output cannot cut.
            let mut said = format!(
                "{}: {} (flags {:#010x}), POSIX renames: {}, a directory's flush: {dir_flush}\n",
                root.display(),
                vol.file_system,
                vol.flags,
                vol.posix_renames(),
            );
            said.push_str(&format!("    columns: {:?}\n", OPS.map(|o| o.0)));
            for (row, (holder, _)) in seen.iter().zip(HOLDERS) {
                said.push_str(&format!("    {row:?}, // {holder}\n"));
            }
            println!("{said}");
            match expected(&vol.file_system) {
                None => println!("nothing held for {}", vol.file_system),
                Some((table, posix, flushed)) => {
                    if vol.posix_renames() != posix {
                        wrong.push(format!(
                            "{}: POSIX renames {}",
                            vol.file_system,
                            vol.posix_renames()
                        ));
                    }
                    if dir_flush != flushed {
                        wrong.push(format!(
                            "{}: a directory's flush {dir_flush}",
                            vol.file_system
                        ));
                    }
                    for (h, (row, (holder, _))) in seen.iter().zip(HOLDERS).enumerate() {
                        for (o, (got, (op, _))) in row.iter().zip(OPS).enumerate() {
                            if got != table[h][o] {
                                wrong.push(format!(
                                    "{}: {op} against {holder}: {got}, not {}",
                                    vol.file_system, table[h][o]
                                ));
                            }
                        }
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
        assert!(
            wrong.is_empty(),
            "Windows answered otherwise:\n{}",
            wrong.join("\n")
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fennec-fs-windows-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A rename another handle refuses -- one opened without
    /// `FILE_SHARE_DELETE`, an indexer's or a backup agent's -- is tried
    /// again, and taken once the handle goes; held past [`REFUSED_FOR`],
    /// it fails, the file as it was. Through a sink, the rewrite fails and
    /// the sink with it, as any failed rename does.
    #[test]
    fn a_rename_another_handle_refuses_is_tried_again() {
        let dir = scratch("refused");
        let (file, image) = (dir.join("db.fennec"), dir.join("db.fennec.compacting"));
        std::fs::write(&file, OLD).unwrap();

        // Let go once refused: the rename waits for it and is taken.
        std::fs::write(&image, NEW).unwrap();
        let holder = opened(&file, SHARE_NO_DELETE);
        let before = REFUSALS.load(Ordering::Relaxed);
        let letting_go = std::thread::spawn(move || {
            while REFUSALS.load(Ordering::Relaxed) == before {
                std::thread::yield_now();
            }
            drop(holder);
        });
        swap(&image, &file).unwrap();
        letting_go.join().unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), NEW);
        assert!(!image.exists());

        // Held throughout: refused once the time is out, nothing changed.
        std::fs::write(&image, OLD).unwrap();
        let holder = opened(&file, SHARE_NO_DELETE);
        let start = Instant::now();
        let e = swap(&image, &file).unwrap_err();
        assert!(start.elapsed() >= REFUSED_FOR, "{:?}", start.elapsed());
        assert!(refused(&e), "{e}");
        println!("a rename over a handle without FILE_SHARE_DELETE: {e}");
        assert_eq!(std::fs::read(&file).unwrap(), NEW);
        assert_eq!(std::fs::read(&image).unwrap(), OLD);
        drop(holder);

        // Through a sink: the rewrite fails, and every sync after it.
        let path = dir.join("sink.fennec");
        let (mut sink, _) = super::super::FileSink::open(&path).unwrap();
        sink.append(b"kept").unwrap();
        sink.sync().unwrap();
        let holder = opened(&path, SHARE_NO_DELETE);
        assert!(sink.rewrite(b"IMAGE").is_err());
        sink.append(b" more").unwrap();
        assert!(
            sink.sync().is_err(),
            "a failed rewrite's rename left the sink taking writes"
        );
        drop(holder);
        assert!(std::fs::read(&path).unwrap().ends_with(b"kept"));
        drop(sink);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A checkpoint's and a compact's rename is followed by both flushes,
    /// the renamed file's and its directory's; `sync_dir` flushes both
    /// where the file is there, the directory alone where it is not, and
    /// answers `Ok` for a directory -- flushed, or refused and taken as
    /// nothing to flush.
    #[test]
    fn a_rename_is_followed_by_both_flushes() {
        let dir = scratch("flushes");
        let path = dir.join("flushed.fennec");
        let mut db = super::super::open(&path).unwrap();
        db.fork(7).unwrap();
        let flushes = || FLUSHES.with(|n| n.get());
        let before = flushes();
        db.checkpoint().unwrap();
        assert_eq!(
            flushes(),
            (before.0 + 1, before.1 + 1),
            "a checkpoint's rename"
        );
        db.fork(8).unwrap();
        db.execute(&crate::query::Statement::Compact(None)).unwrap();
        assert_eq!(
            flushes(),
            (before.0 + 2, before.1 + 2),
            "a compact's rename"
        );
        drop(db);

        let before = flushes();
        super::super::sync_dir(&path).unwrap();
        assert_eq!(flushes(), (before.0 + 1, before.1 + 1), "a file's sync_dir");
        super::super::sync_dir(&dir.join("removed")).unwrap();
        assert_eq!(
            flushes(),
            (before.0 + 2, before.1 + 1),
            "a removal's sync_dir"
        );
        super::super::sync_dir(&dir).unwrap();
        assert_eq!(
            flushes(),
            (before.0 + 3, before.1 + 2),
            "a directory's sync_dir"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The database's file and a rewrite's image are made kept out of the
    /// search indexer, and the file keeps it across a checkpoint.
    #[test]
    fn a_file_fennec_makes_is_not_content_indexed() {
        let dir = scratch("unindexed");
        let path = dir.join("unindexed.fennec");
        let unindexed = |p: &Path| {
            std::fs::metadata(p).unwrap().file_attributes() & FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
                != 0
        };
        let mut db = super::super::open(&path).unwrap();
        assert!(unindexed(&path), "a new database's file");
        db.fork(7).unwrap();
        db.checkpoint().unwrap();
        assert!(unindexed(&path), "a checkpoint's image");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One swap holds its rename, the open after it and its directory's
    /// open to one deadline, and one `sync_dir` its two opens: a stall
    /// under the write lock is about [`REFUSED_FOR`] whatever refuses what,
    /// never that a try. A deadline already out is tried once, and no more.
    #[test]
    fn one_deadline_holds_every_try_of_a_call() {
        let dir = scratch("deadline");
        let (file, image) = (dir.join("db.fennec"), dir.join("db.fennec.compacting"));
        std::fs::write(&file, OLD).unwrap();
        std::fs::write(&image, NEW).unwrap();
        let taken = || DEADLINES.with(|d| d.take());
        taken();
        let start = Instant::now();
        drop(swap(&image, &file).unwrap());
        let seen = taken();
        assert_eq!(seen.len(), 3, "the rename, the open, the directory's open");
        assert!(seen.iter().all(|&u| u == seen[0]), "{seen:?}");
        assert!(seen[0] >= start + REFUSED_FOR);
        super::super::sync_dir(&file).unwrap();
        let seen = taken();
        assert_eq!(seen.len(), 2, "the file's open, the directory's");
        assert_eq!(seen[0], seen[1]);

        let mut tries = 0;
        let e = tried_again(Instant::now(), || {
            tries += 1;
            Err::<(), _>(io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION))
        })
        .unwrap_err();
        assert_eq!(
            (tries, e.raw_os_error()),
            (1, Some(ERROR_SHARING_VIOLATION))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A flush's open another handle refuses for sharing -- a reader not
    /// sharing write -- is tried again until the deadline and then taken as
    /// nothing to flush, as a refusal for access is: `sync_dir` answers
    /// `Ok`, the directory still flushed where the file was held, and the
    /// file still flushed where the directory was.
    #[test]
    fn a_flush_another_handle_refuses_is_nothing_to_flush() {
        let dir = scratch("unshared");
        let path = dir.join("read.fennec");
        std::fs::write(&path, OLD).unwrap();
        let flushes = || FLUSHES.with(|n| n.get());

        let reader = opened(&path, SHARE_READ);
        let (before, start) = (flushes(), Instant::now());
        super::super::sync_dir(&path).unwrap();
        assert!(start.elapsed() >= REFUSED_FOR, "{:?}", start.elapsed());
        assert_eq!(flushes(), (before.0 + 1, before.1), "the directory alone");
        drop(reader);

        let held = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&dir)
            .unwrap();
        let refused = flush_parent(&path, Instant::now()).unwrap();
        assert_eq!(refused, Flushed::Refused, "the directory held");
        let before = flushes();
        super::super::sync_dir(&path).unwrap();
        assert_eq!(flushes(), (before.0 + 1, before.1 + 1), "the file's flush");
        drop(held);
        let done = flush_parent(&path, Instant::now() + REFUSED_FOR).unwrap();
        assert_eq!(done, Flushed::Done, "the directory let go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An open that finds a torn tail cuts it by a second open, every
    /// record before it read (a server's, whose vectors the second leaves
    /// to the linker too, is `tests/windows.rs`').
    #[test]
    fn a_torn_tail_is_cut_by_a_second_open() {
        let dir = scratch("torn");
        let path = dir.join("torn.fennec");
        let mut db = super::super::open(&path).unwrap();
        db.fork(7).unwrap();
        drop(db);
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[3, 1, 100, 0, 1, 2, 3]).unwrap();
        drop(f);
        let db = super::super::open_serving(&path, true, Box::new(Ok)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), whole);
        assert!(db.history().lineage.iter().any(|&(id, _)| id == 7));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `path` through its drive's administrative share,
    /// `\\localhost\C$\...`, which the runner's account, an
    /// administrator's, reaches: the file as a share serves it.
    fn shared(path: &Path) -> PathBuf {
        use std::path::{Component, Prefix};
        let mut out = PathBuf::new();
        for c in std::fs::canonicalize(path).unwrap().components() {
            match c {
                Component::Prefix(p) => match p.kind() {
                    Prefix::VerbatimDisk(d) | Prefix::Disk(d) => {
                        out.push(format!(r"\\localhost\{}$\", d as char))
                    }
                    other => panic!("{other:?}: not on a drive"),
                },
                Component::RootDir => {}
                c => out.push(c),
            }
        }
        out
    }

    /// `mappable`, which every open of a server's file asks on Windows:
    /// true on the runner's NTFS and the job's ReFS, false on its FAT32 and
    /// through a share -- the temporary directory through its drive's
    /// administrative share, the one place the network redirector answers
    /// `FILE_REMOTE_PROTOCOL_INFO`. The mapped path's other tests open
    /// through `open_mapped`, which asks nothing: had `remote` said yes on
    /// NTFS, every Windows server would read its file into memory with them
    /// green. Each answer printed.
    #[test]
    fn a_file_is_mapped_where_its_volume_allows() {
        let dir = scratch("mappable");
        let file = dir.join("new.fennec");
        let local = volume(&file).unwrap();
        println!(
            "{}: {}, remote {}, mappable {}",
            dir.display(),
            local.file_system,
            local.remote,
            mappable(&file)
        );
        assert!(
            !local.remote,
            "the runner's {} taken for a share",
            local.file_system
        );
        assert!(
            mappable(&file),
            "a file on the runner's {}",
            local.file_system
        );

        if let Some(more) = std::env::var_os(VOLUMES) {
            let mut seen = Vec::new();
            for root in std::env::split_paths(&more).filter(|p| !p.as_os_str().is_empty()) {
                let v = volume(&root).unwrap();
                let m = mappable(&root.join("new.fennec"));
                println!(
                    "{}: {}, remote {}, mappable {m}",
                    root.display(),
                    v.file_system,
                    v.remote
                );
                assert!(!v.remote, "{} taken for a share", v.file_system);
                assert_eq!(m, v.posix_renames(), "{}", v.file_system);
                seen.push((v.file_system, m));
            }
            assert!(seen.contains(&("ReFS".into(), true)), "{seen:?}");
            assert!(seen.contains(&("FAT32".into(), false)), "{seen:?}");
        }

        // Each way `remote` asks, on the share's first handle -- the one a
        // server's open makes -- and on later ones, each answer printed.
        let share = shared(&dir);
        let mut said = format!("{}:", share.display());
        for _ in 0..3 {
            let open = OpenOptions::new()
                .access_mode(0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(&share)
                .unwrap();
            let device = remote_device(&open);
            let protocol = match remote_protocol(&open) {
                Ok(info) => format!(
                    "version {}, {} bytes, protocol {:#x}",
                    info[0] & 0xffff,
                    info[0] >> 16,
                    info[1]
                ),
                Err(e) => format!("refused: {e}"),
            };
            said.push_str(&format!(
                "\n    a remote device {device}; FILE_REMOTE_PROTOCOL_INFO {protocol}"
            ));
            assert!(device, "{said}");
        }
        let v = volume(&share.join("new.fennec")).unwrap();
        said.push_str(&format!(
            "\n    {}, remote {}, mappable {}",
            v.file_system,
            v.remote,
            mappable(&share.join("new.fennec"))
        ));
        println!("{said}");
        assert!(v.remote, "{} not taken for a share", share.display());
        assert!(!mappable(&share.join("new.fennec")));
        // Neither way says the runner's own NTFS is a share.
        let open = OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&dir)
            .unwrap();
        assert!(!remote_device(&open), "{}", dir.display());
        assert!(remote_protocol(&open).is_err(), "{}", dir.display());
        drop(open);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A thousand handovers lay a thousand views end to end in the one
    /// placeholder, each record read back where it was written; a rewrite
    /// maps the file it renamed into place again in one.
    #[test]
    fn a_rewrite_maps_the_file_again_in_one_view() {
        let dir = scratch("views");
        let path = dir.join("views.fennec");
        let (mut sink, _) = super::super::FileSink::open(&path).unwrap();
        let first = sink.remapped().unwrap();
        let mut written = super::super::MAGIC.to_vec();
        for i in 0..1000u32 {
            let record = vec![(i % 251) as u8; 4096 + 7 * i as usize % 4096];
            sink.append(&record).unwrap();
            written.extend_from_slice(&record);
            let base = sink.written_through().unwrap().unwrap();
            assert!(std::sync::Arc::ptr_eq(&base, &first), "mapped anew");
        }
        let views = sink.mapping.as_ref().unwrap().views();
        println!("a thousand handovers: {views} views");
        assert!(views > 1000, "{views}");
        assert!(
            (*first).as_ref() == &written[..],
            "the views read otherwise"
        );
        drop(first);

        sink.rewrite(&written).unwrap();
        let again = sink.remapped().unwrap();
        assert_eq!(sink.mapping.as_ref().unwrap().views(), 1);
        assert!((*again).as_ref() == &written[..]);
        drop((again, sink));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
