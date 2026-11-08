//! The system's randomness on Windows: `BCryptGenRandom` with the system's
//! preferred generator, bcrypt.dll's, declared here as `fennec_abi::lock`
//! declares kernel32's calls -- the crates take no `windows-sys`.
//!
//! Windows has no `/dev/urandom`, which `system_random` read on every
//! system: there it answered nothing, so a `--dir` node and a hooks file --
//! whose stand-in key it makes -- refused to start, `fennec backup` made no
//! key, and `random_bytes` fell back to the clock for a sealed archive's
//! nonce. fennec-server's suite found it on Windows CI
//! (docs/design/windows.md, slice 3).

use std::ffi::c_void;

/// bcrypt.h's: the system's preferred generator, no algorithm handle.
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(algorithm: *mut c_void, buf: *mut u8, len: u32, flags: u32) -> i32;
}

/// `n` bytes of the system's randomness, `None` where the system refused.
/// The call takes a length of 32 bits, so a longer buffer goes in parts.
pub fn system_random(n: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; n];
    for part in buf.chunks_mut(u32::MAX as usize) {
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
        if status != 0 {
            return None;
        }
    }
    Some(buf)
}

/// Cryptographic randomness. The call fails only on a bad argument, its
/// page says, as std's own randomness on Windows takes it, so there is no
/// clock to fall back to here: a refusal is a bug, and says so.
pub fn random_bytes(n: usize) -> Vec<u8> {
    system_random(n).expect("BCryptGenRandom refused the system's preferred generator")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(random_bytes(17).len(), 17);
    }
}
