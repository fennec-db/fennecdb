//! The system's randomness on Unix, read with no file descriptor: Linux's
//! `getrandom` (glibc 2.25, musl 1.1.20, the kernel's 3.17) and macOS's
//! `getentropy` (10.12), declared here as `fennec_abi::lock` declares
//! `fcntl` -- the crates take no `libc`.
//!
//! Every key and nonce was read from `/dev/urandom`, whose open takes a
//! descriptor: a server at its limit (EMFILE) read none, and the nonces
//! fell back to the clock -- two frames of a sealed archive sealed in one
//! tick could share one. `/dev/urandom` is read only where the call is
//! missing: a Linux kernel before 3.17 (ENOSYS), and a Unix neither Linux
//! nor macOS, which no release is built for.

/// Fills `buf` with the system's randomness; `false` where it gave none.
#[cfg(target_os = "linux")]
pub(super) fn fill(buf: &mut [u8]) -> bool {
    use std::io::ErrorKind;
    extern "C" {
        fn getrandom(buf: *mut u8, len: usize, flags: u32) -> isize;
    }
    let mut at = 0;
    while at < buf.len() {
        let rest = &mut buf[at..];
        // SAFETY: `rest` is `rest.len()` bytes to write. No flags: the
        // urandom source, which waits only until it is first seeded at boot.
        let n = unsafe { getrandom(rest.as_mut_ptr(), rest.len(), 0) };
        if n > 0 {
            // A read past 256 bytes may come back short, or a signal cut it.
            at += n as usize;
            continue;
        }
        if n == 0 {
            return false;
        }
        match std::io::Error::last_os_error().kind() {
            ErrorKind::Interrupted => {}
            ErrorKind::Unsupported => return urandom(rest),
            _ => return false,
        }
    }
    true
}

/// Fills `buf` with the system's randomness; `false` where it gave none.
#[cfg(target_os = "macos")]
pub(super) fn fill(buf: &mut [u8]) -> bool {
    extern "C" {
        fn getentropy(buf: *mut u8, len: usize) -> i32;
    }
    // At most 256 bytes a call: EIO past them.
    buf.chunks_mut(256).all(|part| {
        // SAFETY: `part` is `part.len()` bytes to write, 256 at most.
        unsafe { getentropy(part.as_mut_ptr(), part.len()) == 0 }
    })
}

/// Fills `buf` with the system's randomness; `false` where it gave none.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn fill(buf: &mut [u8]) -> bool {
    urandom(buf)
}

/// `/dev/urandom`, where the system has no call for it.
#[cfg(not(target_os = "macos"))]
fn urandom(buf: &mut [u8]) -> bool {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .is_ok()
}
