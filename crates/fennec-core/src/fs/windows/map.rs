//! The file mapped on Windows (docs/design/windows.md, decision 1): views
//! of it laid end to end in a placeholder, one at the open and one each
//! handover.
//!
//! A section -- Windows' file mapping object -- is made at a size and keeps
//! it, and a read-only one cannot be larger than its file, so the room Unix
//! maps past a file's end, which the appends grow into, has no counterpart
//! in one view. A placeholder (`VirtualAlloc2`) reserves the file's length
//! and its room as one stretch of addresses; the file's pages replace its
//! start as a view (`MapViewOfFile3`), and a handover past the views splits
//! the placeholder where they end and maps there the file's pages from that
//! page to its end, from a section made then. The stores' addresses stay,
//! a reader keeps the pages it touched, and the write lock pays a split, a
//! section and a view. Where the system refuses placeholders -- a Windows
//! before 1803 -- the file is mapped whole, with no room, and each handover
//! maps it anew, as a Unix file outgrowing its room is.
//!
//! `VirtualAlloc2` and `MapViewOfFile3` are looked up at run time rather
//! than linked: the import library the toolchain links does not name them
//! (onecore.lib does), and an import of either from a system without them
//! would keep the program from starting at all, where it should start and
//! map each handover anew.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

const PAGE_NOACCESS: u32 = 0x01;
const PAGE_READONLY: u32 = 0x02;
const FILE_MAP_READ: u32 = 0x04;
const MEM_RESERVE: u32 = 0x2000;
const MEM_REPLACE_PLACEHOLDER: u32 = 0x4000;
const MEM_RESERVE_PLACEHOLDER: u32 = 0x4_0000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_PRESERVE_PLACEHOLDER: u32 = 0x2;

/// `SYSTEM_INFO`, for the page size and the allocation granularity.
#[repr(C)]
struct SystemInfo {
    architecture: u16,
    reserved: u16,
    page_size: u32,
    lowest: *mut c_void,
    highest: *mut c_void,
    processors_mask: usize,
    processors: u32,
    processor_type: u32,
    granularity: u32,
    level: u16,
    revision: u16,
}

/// `WIN32_MEMORY_RANGE_ENTRY`: what `PrefetchVirtualMemory` asks for.
#[repr(C)]
struct Range {
    at: *mut c_void,
    len: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetSystemInfo(info: *mut SystemInfo);
    fn GetCurrentProcess() -> *mut c_void;
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
    fn VirtualFree(at: *mut c_void, len: usize, kind: u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn PrefetchVirtualMemory(
        process: *mut c_void,
        count: usize,
        ranges: *const Range,
        flags: u32,
    ) -> i32;
}

type VirtualAlloc2 = unsafe extern "system" fn(
    process: *mut c_void,
    at: *mut c_void,
    len: usize,
    kind: u32,
    protect: u32,
    params: *mut c_void,
    count: u32,
) -> *mut c_void;

type MapViewOfFile3 = unsafe extern "system" fn(
    section: *mut c_void,
    process: *mut c_void,
    at: *mut c_void,
    offset: u64,
    len: usize,
    kind: u32,
    protect: u32,
    params: *mut c_void,
    count: u32,
) -> *mut c_void;

/// The two calls placeholders need, and the module that exported them.
#[derive(Clone, Copy)]
pub(in crate::fs) struct Calls {
    alloc: VirtualAlloc2,
    map: MapViewOfFile3,
    /// What CI reports (`the_placeholder_calls_are_found`).
    #[cfg(test)]
    from: &'static str,
}

/// The placeholder calls, looked up once: kernel32's, as their pages name
/// the DLL, else kernelbase's, which implements them. `None` where neither
/// exports them.
pub(in crate::fs) fn calls() -> Option<Calls> {
    static CALLS: OnceLock<Option<Calls>> = OnceLock::new();
    *CALLS.get_or_init(|| {
        for from in ["kernel32.dll", "kernelbase.dll"] {
            let name: Vec<u16> = from.encode_utf16().chain([0]).collect();
            // SAFETY: both names are NUL-ended; a module handle from
            // `GetModuleHandleW` needs no closing, and both modules stay
            // loaded for the process's life.
            unsafe {
                let module = GetModuleHandleW(name.as_ptr());
                if module.is_null() {
                    continue;
                }
                let alloc = GetProcAddress(module, c"VirtualAlloc2".as_ptr().cast());
                let map = GetProcAddress(module, c"MapViewOfFile3".as_ptr().cast());
                if !alloc.is_null() && !map.is_null() {
                    // The exports are these functions, as their pages
                    // declare them.
                    return Some(Calls {
                        alloc: std::mem::transmute::<*mut c_void, VirtualAlloc2>(alloc),
                        map: std::mem::transmute::<*mut c_void, MapViewOfFile3>(map),
                        #[cfg(test)]
                        from,
                    });
                }
            }
        }
        None
    })
}

/// Whether a file is mapped in views laid end to end in a placeholder,
/// rather than whole and anew at each handover: what a Windows server says
/// at its start.
pub fn placeholders() -> bool {
    calls().is_some()
}

/// The page size and the allocation granularity, asked once.
fn system() -> (usize, usize) {
    static SYSTEM: OnceLock<(usize, usize)> = OnceLock::new();
    *SYSTEM.get_or_init(|| {
        let mut info = SystemInfo {
            architecture: 0,
            reserved: 0,
            page_size: 0,
            lowest: std::ptr::null_mut(),
            highest: std::ptr::null_mut(),
            processors_mask: 0,
            processors: 0,
            processor_type: 0,
            granularity: 0,
            level: 0,
            revision: 0,
        };
        // SAFETY: the call fills the struct it is handed.
        unsafe { GetSystemInfo(&mut info) };
        (
            (info.page_size as usize).max(4096),
            (info.granularity as usize).max(1 << 16),
        )
    })
}

fn round_up(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// A section of `file` at the length it has now, read-only: what each view
/// is made from, and closed once it is, since a view holds its section.
struct Section(*mut c_void);

impl Section {
    fn of(file: &File) -> io::Result<Section> {
        // SAFETY: the handle is the file's, open across the call; no name,
        // no attributes, and a size of 0 for the file's own.
        let s = unsafe {
            CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        if s.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Section(s))
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: the handle is the section's own, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The file mapped read-only: what `crate::fs::Mapping` is on a Unix --
/// `of` maps its length, `with_room` its length and room past it, `cover`
/// takes it as grown -- in views of the file laid end to end.
pub struct Mapping {
    at: *mut u8,
    /// The addresses reserved: the file's length and the room, a whole
    /// number of pages; the views' alone where there is no room.
    reserved: usize,
    /// How much of it the file holds, as far as a reader looks.
    len: AtomicUsize,
    views: Mutex<Views>,
}

/// What a handover grows, under the database's write lock.
struct Views {
    /// A handle of the file to make each view's section from: a handle of
    /// its own, since the sink's goes with a rewrite while a pinned read
    /// may still grow nothing but read this. `None` with no room.
    file: Option<File>,
    /// Each view's address, laid end to end from `at`.
    at: Vec<usize>,
    /// Where the views end, a whole number of pages from `at`: the
    /// placeholder holds the rest of `reserved`.
    mapped: usize,
    /// A split placeholder whose view could not be made: two placeholders
    /// past the views, and no handover maps into them again.
    stuck: Option<usize>,
}

// The pages are read-only and shared by nothing but readers; the views
// grow under a mutex.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

thread_local! {
    /// The room a mapping on this thread takes ([`assume_room`]).
    static ROOM: std::cell::Cell<Option<Option<usize>>> = const { std::cell::Cell::new(None) };
}

/// Has a mapping made on this thread take `room` -- `Some(None)` no
/// placeholder at all, as a system refusing them maps each handover anew,
/// `Some(Some(n))` a room of `n` bytes past the file -- or, `None`, the
/// room a mapping takes: how a test reaches a file outgrowing its room,
/// and a Windows before 1803, on a machine that has neither.
#[doc(hidden)]
pub fn assume_room(room: Option<Option<usize>>) {
    ROOM.with(|r| r.set(room));
}

impl Mapping {
    /// The first `len` bytes of `file`, one view and no room. A view
    /// cannot be empty; a file holds the magic at least by then.
    pub(in crate::fs) fn of(file: &File, len: usize) -> crate::error::Result<Mapping> {
        let section = Section::of(file)?;
        // SAFETY: the section is the file's, at least `len` long; the view
        // is unmapped in `drop`.
        let at = unsafe { MapViewOfFile(section.0, FILE_MAP_READ, 0, 0, len) };
        if at.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Mapping {
            at: at.cast(),
            reserved: len,
            len: AtomicUsize::new(len),
            views: Mutex::new(Views {
                file: None,
                at: vec![at as usize],
                mapped: round_up(len, system().0),
                stuck: None,
            }),
        })
    }

    /// [`Self::of`], with room past the file's end for what is appended to
    /// it (`ROOM`, as on a Unix): a placeholder of both, the file's view
    /// at its start. The file alone where the system refuses placeholders
    /// -- and a handover then maps it anew.
    pub(in crate::fs) fn with_room(file: &File, len: usize) -> crate::error::Result<Mapping> {
        let room = len.max(super::super::ROOM);
        let room = match ROOM.with(|r| r.get()) {
            Some(None) => return Mapping::of(file, len),
            Some(Some(n)) => n,
            None => room,
        };
        match calls() {
            Some(calls) => {
                Mapping::laid(calls, file, len, len + room).or_else(|_| Mapping::of(file, len))
            }
            None => Mapping::of(file, len),
        }
    }

    fn laid(calls: Calls, file: &File, len: usize, want: usize) -> io::Result<Mapping> {
        let reserved = round_up(want, system().1);
        let file = file.try_clone()?;
        // SAFETY: a reservation of addresses alone, of a whole number of
        // allocation units, freed in `drop`.
        let at = unsafe {
            (calls.alloc)(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                reserved,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            )
        };
        if at.is_null() {
            return Err(io::Error::last_os_error());
        }
        let m = Mapping {
            at: at.cast(),
            reserved,
            len: AtomicUsize::new(0),
            views: Mutex::new(Views {
                file: Some(file),
                at: Vec::new(),
                mapped: 0,
                stuck: None,
            }),
        };
        m.grow(calls, len)?;
        Ok(m)
    }

    /// Takes the file as `len` bytes long, where the views reach that far
    /// or the room lets a view more be laid after them. A reader never
    /// looks past what the file holds.
    pub(in crate::fs) fn cover(&self, len: usize) -> bool {
        if len > self.reserved {
            return false;
        }
        match calls() {
            Some(calls) => self.grow(calls, len).is_ok(),
            None => len <= self.len.load(Ordering::Acquire),
        }
    }

    /// The views laid as far as `len`: past the last one's last page, the
    /// placeholder split there and the file's pages from that page on
    /// mapped in its place. The last page of a view holds the bytes later
    /// writes put past its section's end -- the file's own page, which the
    /// next view does not map again (`a_view_shows_what_is_written_into_its_last_page`).
    fn grow(&self, calls: Calls, len: usize) -> io::Result<()> {
        let mut v = self.views.lock().unwrap_or_else(|e| e.into_inner());
        if len > v.mapped {
            let (page, _) = system();
            let Some(file) = v.file.as_ref().filter(|_| v.stuck.is_none()) else {
                return Err(io::Error::other("no view can be laid past this one"));
            };
            let end = round_up(len, page);
            if end > self.reserved {
                return Err(io::Error::other("past the room"));
            }
            let section = Section::of(file)?;
            let at = self.at.wrapping_add(v.mapped) as *mut c_void;
            let piece = end - v.mapped;
            if end < self.reserved {
                // SAFETY: `at` starts the placeholder past the views, and
                // `piece` is a whole number of pages short of its end: the
                // placeholder splits in two there.
                if unsafe { VirtualFree(at, piece, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) } == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            // SAFETY: the placeholder at `at` is `piece` bytes, which the
            // view of the file from `mapped` to its end, `len`, fills to
            // the page, replacing it; it is unmapped in `drop`. Its size is
            // the file's bytes, not the page-rounded `piece` the page asks
            // for: a view past a section's end is refused, and the system
            // rounds it to the page and matches the placeholder by its
            // pages (`a_view_replaces_a_placeholder_at_the_file_s_length`).
            let view = unsafe {
                (calls.map)(
                    section.0,
                    GetCurrentProcess(),
                    at,
                    v.mapped as u64,
                    len - v.mapped,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READONLY,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if view.is_null() {
                let e = io::Error::last_os_error();
                if end < self.reserved {
                    v.stuck = Some(v.mapped + piece);
                }
                return Err(e);
            }
            // A view replacing a placeholder is where the placeholder was.
            debug_assert_eq!(view, at);
            v.at.push(view as usize);
            v.mapped = end;
        }
        // Grown under the database's write lock; released, so a reader
        // that sees the length sees the view laid before it.
        self.len.fetch_max(len, Ordering::Release);
        Ok(())
    }

    /// The views this mapping holds.
    #[cfg(test)]
    pub(in crate::fs) fn views(&self) -> usize {
        self.views.lock().unwrap().at.len()
    }
}

impl AsRef<[u8]> for Mapping {
    fn as_ref(&self) -> &[u8] {
        let len = self.len.load(Ordering::Acquire);
        // SAFETY: the views map the first `len` bytes from `at`, laid end
        // to end, until `drop`.
        unsafe { std::slice::from_raw_parts(self.at, len) }
    }
}

/// A mapping of more views than this lets them go on a thread of its own
/// ([`Teardown`]). Each view is an `UnmapViewOfFile`, where a Unix mapping
/// is one `munmap`, and the drop that lets a mapping go is a handover's
/// past its room or a rewrite's, under the write lock: single small writes
/// hand over every 16 384 (`HANDOVER_DOCS`), 2 MB of 128-byte rows, so
/// 100 GB written into a mapping's room are 50 000 views. Fewer go in
/// place -- an open's one among them -- as a Unix mapping's do.
const IN_PLACE: usize = 16;

/// The teardowns on threads not done yet, and the wait for them
/// ([`unmapped`]).
static UNMAPPING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// What a dropped mapping lets go of -- its views, the placeholders past
/// them and its handle of the file -- in place, or on a thread of its own.
struct Teardown {
    at: usize,
    reserved: usize,
    views: Views,
    /// Counted in [`UNMAPPING`] until done.
    counted: bool,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        let v = &self.views;
        for &view in &v.at {
            // SAFETY: each a view the mapping made, unmapped once: its
            // addresses are freed with it.
            unsafe { UnmapViewOfFile(view as *const c_void) };
        }
        if v.file.is_some() {
            // The placeholder past the views, and the second half of one
            // split for a view that could not be made.
            let rest = [Some(v.mapped), v.stuck];
            for from in rest.into_iter().flatten().filter(|&f| f < self.reserved) {
                // SAFETY: a placeholder the mapping reserved, freed once.
                unsafe { VirtualFree((self.at + from) as *mut c_void, 0, MEM_RELEASE) };
            }
        }
        if self.counted {
            let mut n = UNMAPPING.0.lock().unwrap_or_else(|e| e.into_inner());
            *n -= 1;
            if *n == 0 {
                UNMAPPING.1.notify_all();
            }
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let v = self.views.get_mut().unwrap_or_else(|e| e.into_inner());
        let views = Views {
            file: v.file.take(),
            at: std::mem::take(&mut v.at),
            mapped: v.mapped,
            stuck: v.stuck,
        };
        let mut t = Teardown {
            at: self.at as usize,
            reserved: self.reserved,
            views,
            counted: false,
        };
        if t.views.at.len() <= IN_PLACE {
            return;
        }
        *UNMAPPING.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        t.counted = true;
        #[cfg(test)]
        tests::SENT.with(|n| n.set(n.get() + 1));
        // Nothing reads the views now: the last holder of the mapping let
        // it go. A thread that cannot be made drops the teardown with the
        // closure, which lets the views go in place.
        let _ = std::thread::Builder::new().spawn(move || drop(t));
    }
}

/// Waits for the teardowns on threads ([`IN_PLACE`]) to be done: a cut,
/// which Windows refuses while any view maps the file, waits for the
/// mappings this process dropped before it.
pub(in crate::fs) fn unmapped() {
    let mut n = UNMAPPING.0.lock().unwrap_or_else(|e| e.into_inner());
    while *n > 0 {
        n = UNMAPPING.1.wait(n).unwrap_or_else(|e| e.into_inner());
    }
}

/// [`crate::fs::touch`] on Windows: `PrefetchVirtualMemory` (Windows 8)
/// asks a step's pages in ahead of the reads, as `MADV_WILLNEED` does.
pub fn touch(m: &crate::store::Base) {
    let bytes: &[u8] = (**m).as_ref();
    let mut sum = 0u8;
    for from in (0..bytes.len()).step_by(super::super::TOUCH_STEP) {
        let to = (from + super::super::TOUCH_STEP).min(bytes.len());
        let range = Range {
            at: bytes.as_ptr().wrapping_add(from) as *mut c_void,
            len: to - from,
        };
        // SAFETY: the range is within the mapping, which `m` holds across
        // the call. Advice: refused, the reads below fault the pages in.
        unsafe { PrefetchVirtualMemory(GetCurrentProcess(), 1, &range, 0) };
        for at in (from..to).step_by(4096) {
            // Volatile, or the compiler drops reads whose result is unused.
            sum ^= unsafe { std::ptr::read_volatile(bytes.as_ptr().add(at)) };
        }
    }
    std::hint::black_box(sum);
}

#[cfg(test)]
pub(in crate::fs) mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    thread_local! {
        /// Teardowns this thread's drops sent to a thread of their own.
        pub(super) static SENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fennec-map-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn appending(path: &std::path::Path) -> File {
        std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(path)
            .unwrap()
    }

    /// Bytes `n` of a file these tests write: a byte of its offset's.
    fn byte(n: usize) -> u8 {
        (n % 251) as u8
    }

    fn bytes(from: usize, to: usize) -> Vec<u8> {
        (from..to).map(byte).collect()
    }

    /// `VirtualAlloc2` and `MapViewOfFile3` are found, and from which
    /// module: on `windows-latest` the placeholders are what a file is
    /// mapped in (docs/design/windows.md, "What only Windows settles").
    #[test]
    fn the_placeholder_calls_are_found() {
        let calls = calls().expect("VirtualAlloc2 and MapViewOfFile3 exported");
        println!(
            "VirtualAlloc2 and MapViewOfFile3 from {}; page {} bytes, granularity {}",
            calls.from,
            system().0,
            system().1
        );
    }

    /// A read-only section is no larger than its file: asked for more, it is
    /// refused, where a writable one would grow the file -- why the room
    /// past the file is a placeholder rather than a larger section.
    #[test]
    fn a_read_only_section_does_not_grow_its_file() {
        let dir = scratch("section");
        let path = dir.join("short.fennec");
        std::fs::write(&path, bytes(0, 5000)).unwrap();
        let file = File::open(&path).unwrap();
        // SAFETY: the handle is the file's; a section made is closed.
        let s = unsafe {
            CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                PAGE_READONLY,
                0,
                1 << 20,
                std::ptr::null(),
            )
        };
        let e = io::Error::last_os_error();
        if !s.is_null() {
            unsafe { CloseHandle(s) };
        }
        assert!(
            s.is_null(),
            "a read-only section larger than its file was made"
        );
        println!("a read-only section past its file: {e}");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 5000);
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The question decision 1 left to CI: a view made before a write into
    /// its last page -- past its section's end, within the page -- shows
    /// it, so a handover lays its next view from the following page and
    /// the records across the page are read through the two. Then a
    /// thousand handovers' views, end to end, each record read back.
    #[test]
    fn a_view_shows_what_is_written_into_its_last_page() {
        let dir = scratch("last-page");
        let path = dir.join("pages.fennec");
        // Not a whole page: the view's last page is part the file's.
        std::fs::write(&path, bytes(0, 5000)).unwrap();
        let mut file = appending(&path);
        let m = Mapping::with_room(&file, 5000).unwrap();
        assert_eq!(m.views(), 1);
        file.write_all(&bytes(5000, 7000)).unwrap();
        assert!(m.cover(7000));
        assert_eq!(m.views(), 1, "a view laid inside the last one's page");
        let seen = &m.as_ref()[5000..7000];
        let shown = seen == &bytes(5000, 7000)[..];
        println!("a view shows what is written into its last page after it: {shown}");
        assert!(shown, "the view's last page reads {:?}", &seen[..16]);

        // Across the page: the rest of it through the first view, the
        // next page on through the second.
        file.write_all(&bytes(7000, 13000)).unwrap();
        assert!(m.cover(13000));
        assert_eq!(m.views(), 2);
        assert_eq!(m.as_ref(), &bytes(0, 13000)[..]);

        let mut len = 13000;
        for i in 0..1000 {
            let more = 3000 + 997 * (i % 7);
            file.write_all(&bytes(len, len + more)).unwrap();
            len += more;
            assert!(m.cover(len));
        }
        println!("a thousand handovers: {} views", m.views());
        assert!(m.views() > 900 && m.views() <= 1002, "{}", m.views());
        assert!(m.as_ref() == &bytes(0, len)[..], "the views read otherwise");
        drop(m);
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mapping of a few views lets them go in its drop, and one of many
    /// on a thread of its own, the drop -- a handover's or a rewrite's,
    /// under the write lock -- paying no `UnmapViewOfFile` a view; a cut,
    /// which Windows refuses while a view maps the file, is taken once
    /// [`unmapped`] answers.
    #[test]
    fn many_views_are_let_go_of_on_a_thread() {
        let dir = scratch("teardown");
        let path = dir.join("views.fennec");
        std::fs::write(&path, bytes(0, 5000)).unwrap();
        let mut file = appending(&path);
        let sent = || SENT.with(|n| n.get());
        // `appending`'s handle may append, not cut.
        let cut = |len| {
            let f = std::fs::OpenOptions::new().write(true).open(&path);
            f.and_then(|f| f.set_len(len))
        };

        let few = Mapping::with_room(&file, 5000).unwrap();
        file.write_all(&bytes(5000, 20_000)).unwrap();
        assert!(few.cover(20_000));
        assert_eq!(few.views(), 2);
        let before = sent();
        drop(few);
        assert_eq!(sent(), before, "a mapping of two views sent away");
        cut(19_000).expect("a cut after the views went in place");

        let many = Mapping::with_room(&file, 19_000).unwrap();
        let mut len = 19_000;
        for _ in 0..200 {
            file.write_all(&bytes(len, len + 5000)).unwrap();
            len += 5000;
            assert!(many.cover(len));
        }
        assert!(many.views() > IN_PLACE, "{}", many.views());
        drop(many);
        assert_eq!(
            sent(),
            before + 1,
            "a mapping of many views let go in place"
        );
        unmapped();
        cut(5000).expect("a cut once the thread let the views go");
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), bytes(0, 5000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mapping refuses to cover past its room, and one with no room past
    /// its length, so the sink maps the file anew; a mapping lets its views
    /// and its placeholders go when dropped.
    #[test]
    fn a_file_past_its_room_is_not_covered() {
        let dir = scratch("room");
        let path = dir.join("room.fennec");
        std::fs::write(&path, bytes(0, 5000)).unwrap();
        let mut file = appending(&path);
        assume_room(Some(Some(1 << 16)));
        let m = Mapping::with_room(&file, 5000).unwrap();
        assume_room(Some(None));
        let whole = Mapping::with_room(&file, 5000).unwrap();
        assume_room(None);
        file.write_all(&bytes(5000, 200_000)).unwrap();
        assert!(m.cover(60_000));
        assert!(!m.cover(200_000), "covered past the room");
        assert_eq!(m.as_ref(), &bytes(0, 60_000)[..]);
        assert!(!whole.cover(6000), "a mapping with no room covered more");
        assert!(whole.cover(5000));
        let anew = Mapping::with_room(&file, 200_000).unwrap();
        assert_eq!(anew.as_ref(), &bytes(0, 200_000)[..]);
        drop((m, whole, anew));
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Which `ViewSize` replaces a placeholder over a file not a whole
    /// number of pages long: the page asks for the placeholder's own,
    /// page-rounded, which `windows-latest` refuses past the section's end
    /// (5, access denied), as it refuses 0 (87); the file's length is what
    /// it takes -- what [`Mapping::grow`] passes. Each printed.
    #[test]
    fn a_view_replaces_a_placeholder_at_the_file_s_length() {
        let calls = calls().unwrap();
        let dir = scratch("view-size");
        let path = dir.join("odd.fennec");
        std::fs::write(&path, bytes(0, 5000)).unwrap();
        let file = File::open(&path).unwrap();
        let mut said = Vec::new();
        let mut exact = false;
        for (name, size) in [("page-rounded", 8192), ("exact", 5000), ("zero", 0)] {
            // SAFETY: a placeholder of two pages, replaced by a view or
            // freed, the view unmapped.
            unsafe {
                let at = (calls.alloc)(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    8192,
                    MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                    PAGE_NOACCESS,
                    std::ptr::null_mut(),
                    0,
                );
                assert!(!at.is_null(), "{}", io::Error::last_os_error());
                let section = Section::of(&file).unwrap();
                let view = (calls.map)(
                    section.0,
                    GetCurrentProcess(),
                    at,
                    0,
                    size,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READONLY,
                    std::ptr::null_mut(),
                    0,
                );
                if view.is_null() {
                    said.push(format!("{name}: {}", io::Error::last_os_error()));
                    VirtualFree(at, 0, MEM_RELEASE);
                } else {
                    let read = std::slice::from_raw_parts(view as *const u8, 5000);
                    assert_eq!(read, &bytes(0, 5000)[..]);
                    said.push(format!("{name}: mapped"));
                    exact |= name == "exact";
                    UnmapViewOfFile(view);
                }
            }
        }
        println!("a view over a placeholder, ViewSize {}", said.join("; "));
        assert!(exact, "{said:?}");
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
