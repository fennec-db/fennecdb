//! The system's randomness on Windows: `BCryptGenRandom` with the system's
//! preferred generator, bcrypt.dll's, declared here as `fennec_abi::lock`
//! declares kernel32's calls -- the crates take no `windows-sys`.
//!
//! Windows has no `/dev/urandom`, which `system_random` read on every
//! system: there it answered nothing, so a `--dir` node and a hooks file --
//! whose stand-in key it makes -- refused to start, `fennec backup` made no
//! key, and a sealed archive's nonce came from the clock. fennec-server's
//! suite found it on Windows CI (docs/design/windows.md, slice 3). std's
//! own randomness on Windows is another call, `ProcessPrng`
//! (bcryptprimitives.dll), since Rust 1.78.

use std::ffi::c_void;

/// bcrypt.h's: the system's preferred generator, no algorithm handle.
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(algorithm: *mut c_void, buf: *mut u8, len: u32, flags: u32) -> i32;
}

/// Fills `buf` with the system's randomness; `false` where the system
/// refused (an `NTSTATUS` other than success), which its page names for a
/// bad argument alone. The call takes a length of 32 bits, so a longer
/// buffer goes in parts.
pub(super) fn fill(buf: &mut [u8]) -> bool {
    buf.chunks_mut(u32::MAX as usize).all(|part| {
        // SAFETY: a buffer of `part.len()` bytes, which fits a `u32`; no
        // algorithm handle with the system's preferred generator.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                part.as_mut_ptr(),
                part.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        status == 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::system_random;

    /// Each call's bytes are the system's: as long as asked, never the same
    /// twice, and not all zero.
    #[test]
    fn the_systems_randomness_is_read() {
        let a = system_random(32).expect("BCryptGenRandom");
        let b = system_random(32).expect("BCryptGenRandom");
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.iter().any(|&x| x != 0));
        assert_eq!(system_random(0), Some(Vec::new()));
        let mut long = vec![0u8; 1000];
        assert!(fill(&mut long));
        assert!(long[256..].iter().any(|&x| x != 0));
    }
}
