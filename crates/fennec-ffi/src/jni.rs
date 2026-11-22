//! The JNI functions the Kotlin library calls (`integrations/kotlin`,
//! `com.fennecdb.FennecNative`), over the same calls as the C ABI.
//!
//! Written by hand over the JNI function table rather than through the
//! `jni` crate: four of its functions are used -- an array's length, a new
//! byte array, and copying bytes out of one and into one -- each found at
//! its fixed place in the table (`jni.h`'s `JNINativeInterface_`).
//!
//! Everything crosses as a `byte[]`. Text goes in as its UTF-8 bytes, which
//! Kotlin's `encodeToByteArray` makes: through a `jstring`, JNI's own
//! "modified UTF-8" writes a character outside the BMP -- an emoji in a
//! title -- as two three-byte halves, which is not UTF-8 and the parser
//! refuses. An answer comes back as one `byte[]`, its first byte the call's
//! code and the rest the answer's JSON (or, for an open, the handle as
//! decimal digits): no exception crosses, and a call costs one array.
//!
//! A Java array's length is an `int`: an answer it cannot hold is a query
//! error saying so ([`array_len`]), never a length cast to one. And every
//! function's own work -- reading its arrays, making its answer -- runs
//! under `catch_unwind` ([`guarded`]), as the C ABI's calls do under
//! `call`: a panic leaving an `extern "system"` function aborts the
//! process, the app with it.

use fennec_core::json;
use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

type Env = *mut *const *const c_void;
type JArray = *mut c_void;

const GET_ARRAY_LENGTH: usize = 171;
const NEW_BYTE_ARRAY: usize = 176;
const GET_BYTE_ARRAY_REGION: usize = 200;
const SET_BYTE_ARRAY_REGION: usize = 208;

/// The table's function at `at`.
///
/// # Safety
/// `env` is the `JNIEnv` the VM handed the native call running, and `at`
/// one of the table's places above.
unsafe fn function(env: Env, at: usize) -> *const c_void {
    // SAFETY: as the contract says, `env` points at the VM's pointer to
    // its function table, which JNI hands every native call, and `at` is a
    // fixed place inside it (`jni.h`'s `JNINativeInterface_`).
    unsafe { *(*env).add(at) }
}

/// A `byte[]`'s bytes, copied out; empty for a null.
///
/// # Safety
/// `env` is the call's `JNIEnv`, and `array` null or a `byte[]` the VM
/// handed the call.
unsafe fn read(env: Env, array: JArray) -> Vec<u8> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: the table's entry at `GET_ARRAY_LENGTH` is `GetArrayLength`,
    // of this signature in `jni.h`; `env` is the call's (`function`).
    let len: unsafe extern "system" fn(Env, JArray) -> i32 =
        unsafe { std::mem::transmute(function(env, GET_ARRAY_LENGTH)) };
    // SAFETY: the entry at `GET_BYTE_ARRAY_REGION` is
    // `GetByteArrayRegion`, of this signature in `jni.h`.
    let get: unsafe extern "system" fn(Env, JArray, i32, i32, *mut i8) =
        unsafe { std::mem::transmute(function(env, GET_BYTE_ARRAY_REGION)) };
    // SAFETY: `array` is a live `byte[]` of the call, not null (above).
    let n = unsafe { len(env, array) }.max(0);
    let mut out = vec![0u8; n as usize];
    // SAFETY: the region from 0 of `n` bytes is the whole array, copied
    // into `out`, which holds `n`.
    unsafe { get(env, array, 0, n, out.as_mut_ptr() as *mut i8) };
    out
}

/// The most bytes a `byte[]` holds: its length is a Java `int`.
const MOST: usize = i32::MAX as usize;

/// The length of the `byte[]` answering a call with `payload` after its
/// code, or the query error (its JSON) refusing an answer no `byte[]`
/// holds. Cast to the `int` `NewByteArray` takes, an answer of 2 GiB or
/// more went over as a negative length -- an exception the VM threw -- or,
/// from 4 GiB, as the length past a multiple of 4 GiB: an array that
/// short, the answer cut to it (1.0's review). A limit errors, never cuts
/// (the root CLAUDE.md's *Limits*), and what the statement wrote stands.
fn array_len(payload: usize) -> Result<i32, String> {
    if let Some(len) = payload.checked_add(1).and_then(|n| i32::try_from(n).ok()) {
        return Ok(len);
    }
    let mut text = String::from("{\"kind\":\"error\",\"message\":");
    let why = format!(
        "an answer of {payload} bytes is more than a JVM array holds ({} after its code): \
         ask for fewer rows or fields; what the statement wrote stands",
        MOST - 1
    );
    json::escape_into(&mut text, &why);
    text.push('}');
    Err(text)
}

/// `work`'s answer, a panic in it answered as the C ABI's `call` answers
/// one: `FENNEC_PANIC` and its message. The function's own work -- reading
/// its arrays, making its answer -- is outside `call`, and a panic leaving
/// an `extern "system"` function aborts the process, the app with it.
///
/// # Safety
/// `env` is the call's `JNIEnv`.
unsafe fn guarded(env: Env, work: impl FnOnce() -> JArray) -> JArray {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(array) => array,
        // SAFETY: `env` is the call's, as this function's contract says.
        Err(panic) => unsafe {
            answer(
                env,
                crate::FENNEC_PANIC,
                crate::panicked(&*panic).as_bytes(),
            )
        },
    }
}

/// A new `byte[]`: the code, then `payload`; or, where no `byte[]` holds
/// that, the query error saying so (`array_len`).
///
/// # Safety
/// `env` is the call's `JNIEnv`.
unsafe fn answer(env: Env, code: i32, payload: &[u8]) -> JArray {
    let len = match array_len(payload.len()) {
        Ok(len) => len,
        // SAFETY: `env` is the call's, as this function's contract says;
        // the refusal is a short text, which a `byte[]` holds.
        Err(refusal) => return unsafe { answer(env, crate::FENNEC_QUERY, refusal.as_bytes()) },
    };
    // SAFETY: the table's entry at `NEW_BYTE_ARRAY` is `NewByteArray`, of
    // this signature in `jni.h`; `env` is the call's (`function`).
    let new: unsafe extern "system" fn(Env, i32) -> JArray =
        unsafe { std::mem::transmute(function(env, NEW_BYTE_ARRAY)) };
    // SAFETY: the entry at `SET_BYTE_ARRAY_REGION` is
    // `SetByteArrayRegion`, of this signature in `jni.h`.
    let set: unsafe extern "system" fn(Env, JArray, i32, i32, *const i8) =
        unsafe { std::mem::transmute(function(env, SET_BYTE_ARRAY_REGION)) };
    let mut bytes = Vec::with_capacity(1 + payload.len());
    bytes.push(code as u8);
    bytes.extend_from_slice(payload);
    // SAFETY: `NewByteArray` takes the call's `env` and a length from 0;
    // a null is its refusal, looked at below.
    let array = unsafe { new(env, len) };
    // An allocation the VM refused leaves an OutOfMemoryError pending,
    // which Kotlin throws as the call returns.
    if !array.is_null() {
        // SAFETY: `array` is the new `byte[]` of `len` bytes, the region
        // from 0 all of it, copied from `bytes`, which holds that many.
        unsafe { set(env, array, 0, len, bytes.as_ptr() as *const i8) };
    }
    array
}

/// A call's code and the text it wrote, freed here.
///
/// # Safety
/// `env` is the call's `JNIEnv`, and `out` null or the text the C ABI
/// wrote, not yet freed.
unsafe fn taken(env: Env, code: i32, out: *mut c_char) -> JArray {
    if out.is_null() {
        // SAFETY: `env` is the call's, as `answer` asks.
        return unsafe { answer(env, code, &[]) };
    }
    // SAFETY: `out` is the C ABI's text, NUL-terminated and not null
    // (above), read whole before it is freed; `env` is the call's.
    let r = unsafe { answer(env, code, CStr::from_ptr(out).to_bytes()) };
    // SAFETY: `out` came from the C ABI, freed once, after its last read.
    unsafe { crate::fennec_free_string(out) };
    r
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_version(
    env: Env,
    _class: *mut c_void,
) -> JArray {
    let work = || {
        // SAFETY: `fennec_version` answers a static NUL-terminated text, never
        // freed; `env` is the one the VM handed this call.
        unsafe { answer(env, 0, CStr::from_ptr(crate::fennec_version()).to_bytes()) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_open(
    env: Env,
    _class: *mut c_void,
    path: JArray,
    flags: i32,
) -> JArray {
    let work = || {
        // SAFETY: `env` and `path` are the ones the VM handed this call.
        let path = unsafe { read(env, path) };
        let (mut handle, mut out) = (0u64, std::ptr::null_mut());
        // SAFETY: fennec.h's contract (*Pointers and lengths*): the path is a
        // `Vec`'s bytes and its length, alive across the call, `handle` and
        // `out` places to write, `out_len` null.
        let code = unsafe {
            crate::fennec_open(
                path.as_ptr(),
                path.len(),
                flags as u32,
                &mut handle,
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe {
            match code {
                0 => answer(env, 0, handle.to_string().as_bytes()),
                _ => taken(env, code, out),
            }
        }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_openMemory(
    env: Env,
    _class: *mut c_void,
) -> JArray {
    let work = || {
        let (mut handle, mut out) = (0u64, std::ptr::null_mut());
        // SAFETY: fennec.h's contract: `handle` and `out` are places to write,
        // `out_len` null.
        let code =
            unsafe { crate::fennec_open_memory(&mut handle, &mut out, std::ptr::null_mut()) };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe {
            match code {
                0 => answer(env, 0, handle.to_string().as_bytes()),
                _ => taken(env, code, out),
            }
        }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_query(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    text: JArray,
    params: JArray,
    vectors: JArray,
) -> JArray {
    let work = || {
        // SAFETY: `env` and the three arrays are the ones the VM handed this
        // call.
        let (text, params, vectors) =
            unsafe { (read(env, text), read(env, params), read(env, vectors)) };
        let mut out = std::ptr::null_mut();
        // SAFETY: fennec.h's contract (*Pointers and lengths*): each text is a
        // `Vec`'s bytes and its length, alive across the call, and `out` a
        // place to write, `out_len` null.
        let code = unsafe {
            crate::fennec_query(
                handle as u64,
                text.as_ptr(),
                text.len(),
                params.as_ptr(),
                params.len(),
                vectors.as_ptr(),
                vectors.len(),
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe { taken(env, code, out) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_changes(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    since: i64,
) -> JArray {
    let work = || {
        let mut out = std::ptr::null_mut();
        // SAFETY: fennec.h's contract: `out` is a place to write, `out_len`
        // null.
        let code = unsafe {
            crate::fennec_changes(
                handle as u64,
                since.max(0) as u64,
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe { taken(env, code, out) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

/// `fennec_schema`: a schema declared as FennecQL, planned or applied.
#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_schema(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    request: JArray,
    mode: i32,
) -> JArray {
    let work = || {
        // SAFETY: `env` and `request` are the ones the VM handed this call.
        let request = unsafe { read(env, request) };
        let mut out = std::ptr::null_mut();
        // SAFETY: fennec.h's contract (*Pointers and lengths*): each text is a
        // `Vec`'s bytes and its length, alive across the call, and `out` a
        // place to write, `out_len` null.
        let code = unsafe {
            crate::fennec_schema(
                handle as u64,
                request.as_ptr(),
                request.len(),
                mode as u32,
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe { taken(env, code, out) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

/// `fennec_warm`: the indexes an open left for their first read, built now;
/// `only` the names by commas, empty for every one.
#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_warm(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    only: JArray,
) -> JArray {
    let work = || {
        // SAFETY: `env` and `only` are the ones the VM handed this call.
        let only = unsafe { read(env, only) };
        let mut out = std::ptr::null_mut();
        // SAFETY: fennec.h's contract (*Pointers and lengths*): each text is a
        // `Vec`'s bytes and its length, alive across the call, and `out` a
        // place to write, `out_len` null.
        let code = unsafe {
            crate::fennec_warm(
                handle as u64,
                only.as_ptr(),
                only.len(),
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe { taken(env, code, out) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_syncStart(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    config: JArray,
) -> JArray {
    let work = || {
        // SAFETY: `env` and `config` are the ones the VM handed this call.
        let config = unsafe { read(env, config) };
        let mut out = std::ptr::null_mut();
        // SAFETY: fennec.h's contract (*Pointers and lengths*): each text is a
        // `Vec`'s bytes and its length, alive across the call, and `out` a
        // place to write, `out_len` null.
        let code = unsafe {
            crate::fennec_sync_start(
                handle as u64,
                config.as_ptr(),
                config.len(),
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe { taken(env, code, out) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_syncFeed(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    kind: i32,
    id: i64,
    status: i32,
    seq: i64,
    bytes: JArray,
) -> JArray {
    let work = || {
        // SAFETY: `env` and `bytes` are the ones the VM handed this call.
        let bytes = unsafe { read(env, bytes) };
        let mut out = std::ptr::null_mut();
        // SAFETY: fennec.h's contract (*Pointers and lengths*): each text is a
        // `Vec`'s bytes and its length, alive across the call, and `out` a
        // place to write, `out_len` null.
        let code = unsafe {
            crate::fennec_sync_feed(
                handle as u64,
                kind as u32,
                id as u64,
                status,
                seq.max(0) as u64,
                bytes.as_ptr(),
                bytes.len(),
                &mut out,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
        unsafe { taken(env, code, out) }
    };
    // SAFETY: `env` is the one the VM handed this call.
    unsafe { guarded(env, work) }
}

/// The calls that take a handle alone.
macro_rules! by_handle {
    ($($java:ident => $call:ident),* $(,)?) => {$(
        #[no_mangle]
        pub unsafe extern "system" fn $java(env: Env, _class: *mut c_void, handle: i64) -> JArray {
            let work = || {
                let mut out = std::ptr::null_mut();
                // SAFETY: fennec.h's contract: `out` a place to write,
                // `out_len` null.
                let code = unsafe { crate::$call(handle as u64, &mut out, std::ptr::null_mut()) };
                // SAFETY: `env` is the call's, and `out` what the C ABI
                // wrote, unfreed.
                unsafe { taken(env, code, out) }
            };
            // SAFETY: `env` is the one the VM handed this call.
            unsafe { guarded(env, work) }
        }
    )*};
}

by_handle! {
    Java_com_fennecdb_FennecNative_close => fennec_close,
    Java_com_fennecdb_FennecNative_sync => fennec_sync,
    Java_com_fennecdb_FennecNative_flush => fennec_flush,
    Java_com_fennecdb_FennecNative_checkpoint => fennec_checkpoint,
    Java_com_fennecdb_FennecNative_syncStatus => fennec_sync_status,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `byte[]` the stand-in VM made: its bytes.
    type Made = Vec<u8>;

    /// `NewByteArray`'s stand-in: a boxed `Vec` of `len` bytes, or null
    /// for a negative length, where the VM throws.
    extern "system" fn new_byte_array(_env: Env, len: i32) -> JArray {
        match usize::try_from(len) {
            Ok(n) => Box::into_raw(Box::new(vec![0u8; n])) as JArray,
            Err(_) => std::ptr::null_mut(),
        }
    }

    /// `SetByteArrayRegion`'s stand-in, held to the array's bounds as the
    /// VM holds it.
    ///
    /// # Safety
    /// `array` is `new_byte_array`'s, and `from` valid for `len` bytes.
    unsafe extern "system" fn set_byte_array_region(
        _env: Env,
        array: JArray,
        start: i32,
        len: i32,
        from: *const i8,
    ) {
        // SAFETY: `array` is a `Made` `new_byte_array` boxed, alive until
        // `answered` takes it back, and nothing else borrows it.
        let to = unsafe { &mut *(array as *mut Made) };
        let (start, len) = (start as usize, len as usize);
        // SAFETY: `from` holds `len` bytes, as this function's contract
        // says.
        let bytes = unsafe { std::slice::from_raw_parts(from as *const u8, len) };
        to[start..start + len].copy_from_slice(bytes);
    }

    /// The bytes of the `byte[]` `call` answers with, over a stand-in
    /// `JNIEnv` whose table holds the two functions `answer` calls.
    fn answered(call: impl FnOnce(Env) -> JArray) -> Made {
        let mut table = vec![std::ptr::null::<c_void>(); SET_BYTE_ARRAY_REGION + 1];
        table[NEW_BYTE_ARRAY] = new_byte_array as *const c_void;
        table[SET_BYTE_ARRAY_REGION] = set_byte_array_region as *const c_void;
        let mut head: *const *const c_void = table.as_ptr();
        let array = call(&mut head);
        assert!(!array.is_null(), "an answer");
        // SAFETY: `array` is a `Made` `new_byte_array` boxed, taken back
        // once.
        *unsafe { Box::from_raw(array as *mut Made) }
    }

    #[test]
    fn an_answer_is_its_code_then_its_bytes() {
        // SAFETY: the stand-in `JNIEnv` holds what `answer` calls.
        let bytes = answered(|env| unsafe { answer(env, crate::FENNEC_QUERY, b"{}") });
        assert_eq!(bytes, [crate::FENNEC_QUERY as u8, b'{', b'}']);
    }

    // An answer of 2 GiB or more had its length cast to the `int` JNI
    // takes: negative to 4 GiB, an exception the VM threw where Kotlin
    // reads a code, then the length past 4 GiB's multiple, the answer cut
    // to it. Held on the length, as no test allocates 2 GiB.
    #[test]
    fn an_answer_no_jvm_array_holds_is_a_query_error_never_cut() {
        assert_eq!(array_len(0), Ok(1));
        assert_eq!(array_len(MOST - 1), Ok(i32::MAX));
        let mut past = vec![MOST, MOST + 1, usize::MAX];
        past.extend(usize::try_from((1u64 << 32) + 7));
        for n in past {
            let refusal = array_len(n).expect_err("no byte[] holds it");
            let message = format!(
                "an answer of {n} bytes is more than a JVM array holds ({} after its code)",
                MOST - 1
            );
            assert!(
                refusal.starts_with("{\"kind\":\"error\",\"message\":")
                    && refusal.contains(&message),
                "{n}: {refusal}"
            );
        }
        // The refusal itself is answered as one, under its own code.
        let refusal = array_len(MOST).unwrap_err();
        // SAFETY: the stand-in `JNIEnv` holds what `answer` calls.
        let bytes = answered(|env| unsafe { answer(env, crate::FENNEC_QUERY, refusal.as_bytes()) });
        assert_eq!(bytes[0], crate::FENNEC_QUERY as u8);
        assert_eq!(&bytes[1..], refusal.as_bytes());
    }

    // A panic in a JNI function's own work left an `extern "system"`
    // function, which aborts the process: the app with it.
    #[test]
    fn a_panic_in_a_jni_functions_own_work_is_answered_never_unwound() {
        // SAFETY: the stand-in `JNIEnv` holds what `answer` calls.
        let bytes = answered(|env| unsafe { guarded(env, || panic!("the work broke")) });
        assert_eq!(bytes[0], crate::FENNEC_PANIC as u8);
        let text = std::str::from_utf8(&bytes[1..]).unwrap();
        assert_eq!(
            text,
            "{\"kind\":\"error\",\"message\":\"internal error: the work broke\"}"
        );
    }
}
