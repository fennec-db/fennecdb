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

use std::ffi::{c_char, c_void, CStr};

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

/// A new `byte[]`: the code, then `payload`.
///
/// # Safety
/// `env` is the call's `JNIEnv`.
unsafe fn answer(env: Env, code: i32, payload: &[u8]) -> JArray {
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
    // SAFETY: `NewByteArray` takes the call's `env` and any length; a
    // null is its refusal, looked at below.
    let array = unsafe { new(env, bytes.len() as i32) };
    // An allocation the VM refused leaves an OutOfMemoryError pending,
    // which Kotlin throws as the call returns.
    if !array.is_null() {
        // SAFETY: `array` is the new `byte[]` of `bytes.len()` bytes, the
        // region from 0 all of it, copied from `bytes`, which holds that
        // many.
        unsafe {
            set(
                env,
                array,
                0,
                bytes.len() as i32,
                bytes.as_ptr() as *const i8,
            )
        };
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
    // SAFETY: `fennec_version` answers a static NUL-terminated text, never
    // freed; `env` is the one the VM handed this call.
    unsafe { answer(env, 0, CStr::from_ptr(crate::fennec_version()).to_bytes()) }
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_open(
    env: Env,
    _class: *mut c_void,
    path: JArray,
    flags: i32,
) -> JArray {
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
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_openMemory(
    env: Env,
    _class: *mut c_void,
) -> JArray {
    let (mut handle, mut out) = (0u64, std::ptr::null_mut());
    // SAFETY: fennec.h's contract: `handle` and `out` are places to write,
    // `out_len` null.
    let code = unsafe { crate::fennec_open_memory(&mut handle, &mut out, std::ptr::null_mut()) };
    // SAFETY: `env` is the call's, and `out` what the C ABI wrote, unfreed.
    unsafe {
        match code {
            0 => answer(env, 0, handle.to_string().as_bytes()),
            _ => taken(env, code, out),
        }
    }
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
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_changes(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    since: i64,
) -> JArray {
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
}

#[no_mangle]
pub unsafe extern "system" fn Java_com_fennecdb_FennecNative_syncStart(
    env: Env,
    _class: *mut c_void,
    handle: i64,
    config: JArray,
) -> JArray {
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
}

/// The calls that take a handle alone.
macro_rules! by_handle {
    ($($java:ident => $call:ident),* $(,)?) => {$(
        #[no_mangle]
        pub unsafe extern "system" fn $java(env: Env, _class: *mut c_void, handle: i64) -> JArray {
            let mut out = std::ptr::null_mut();
            // SAFETY: fennec.h's contract: `out` a place to write, `out_len`
            // null.
            let code = unsafe { crate::$call(handle as u64, &mut out, std::ptr::null_mut()) };
            // SAFETY: `env` is the call's, and `out` what the C ABI wrote,
            // unfreed.
            unsafe { taken(env, code, out) }
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
