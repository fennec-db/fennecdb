//! The boot clock on Windows (docs/design/windows.md, 7i): interrupt time,
//! read by `QueryInterruptTimePrecise`, which counts from the system's
//! start through every sleep and hibernation -- where
//! `QueryUnbiasedInterruptTime`, the same clock with them left out, would
//! have a node woken past its lease believe it still ran, as `Instant`'s
//! clock had Linux's and macOS's ([`super::Moment`]). Before it, Windows
//! measured the lease on `Instant` ([`super::boot_fallback`]).
//!
//! The call's page names kernel32.dll and Mincore.lib, an import library
//! the toolchain does not link, and slice 2 found kernelbase.dll exporting
//! calls such pages name kernel32's (`fs::windows::map`): so it is looked up
//! at run time, in either, as the mapping's calls are. A system without it,
//! before Windows 10, measures the lease on `Instant` throughout, as a Linux
//! kernel without a boot clock does; one that has it is held to it.

use std::ffi::c_void;
use std::sync::OnceLock;

/// `QueryInterruptTimePrecise`: the interrupt-time count, in 100 ns units.
type Query = unsafe extern "system" fn(count: *mut u64);

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

/// The call, and the module that exports it, looked up once.
fn query() -> Option<(Query, &'static str)> {
    static QUERY: OnceLock<Option<(Query, &'static str)>> = OnceLock::new();
    *QUERY.get_or_init(|| {
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
                let call = GetProcAddress(module, c"QueryInterruptTimePrecise".as_ptr().cast());
                if !call.is_null() {
                    // The export is the function its page declares.
                    return Some((std::mem::transmute::<*mut c_void, Query>(call), from));
                }
            }
        }
        None
    })
}

/// Now, in nanoseconds since the system started, the time it slept among
/// them.
pub fn now() -> u64 {
    match query() {
        Some((read, _)) => {
            let mut count = 0u64;
            // SAFETY: the call writes one `u64`, which outlives it.
            unsafe { read(&mut count) };
            count.saturating_mul(100)
        }
        None => super::boot_fallback(),
    }
}

/// The module the clock was found in, `None` where it was not: what the
/// clock's test prints.
#[cfg(test)]
pub fn from() -> Option<&'static str> {
    query().map(|(_, from)| from)
}
