//! Every function of the C ABI called with what its contract allows at its
//! edges (fennec.h, *Pointers and lengths*): a length of 0, a NULL with the
//! largest length a `size_t` holds, and a buffer exactly as long as it says
//! -- a heap block of that length alone, the bytes after it another
//! allocation's, so a read past its end is one AddressSanitizer reports
//! (CI's sanitize job runs these under it, docs/design/conventions.md).

use fennec_ffi::*;
use std::ffi::{c_char, CStr};

/// A call's code and the text it wrote, freed; the length it wrote held to
/// the text's.
fn taken(code: i32, out: *mut c_char, len: usize) -> (i32, String) {
    if out.is_null() {
        assert_eq!(len, 0, "a length with no text");
        return (code, String::new());
    }
    // SAFETY: not NULL: the NUL-ended text the call wrote through `out`,
    // read once, then freed once, as fennec.h says.
    let text = unsafe {
        let s = CStr::from_ptr(out).to_str().unwrap().to_string();
        fennec_free_string(out);
        s
    };
    assert_eq!(len, text.len(), "the length written is the text's");
    (code, text)
}

/// `text`'s bytes in a heap block of exactly their length, nothing after
/// them that is theirs.
fn exact(text: &[u8]) -> Box<[u8]> {
    text.to_vec().into_boxed_slice()
}

/// `text` followed by `tail`, in one block: a call handed the length of
/// `text` alone must read nothing of `tail`.
fn before(text: &str, tail: &str) -> Box<[u8]> {
    [text.as_bytes(), tail.as_bytes()]
        .concat()
        .into_boxed_slice()
}

fn memory() -> u64 {
    let (mut h, mut out, mut len) = (0u64, std::ptr::null_mut(), 0usize);
    // SAFETY: the handle and both out pointers are locals', alive across the
    // call.
    let code = unsafe { fennec_open_memory(&mut h, &mut out, &mut len) };
    assert_eq!(taken(code, out, len).0, FENNEC_OK);
    h
}

/// `fennec_query` over raw pointers and lengths.
fn query_raw(
    h: u64,
    text: (*const u8, usize),
    params: (*const u8, usize),
    vectors: (*const u8, usize),
) -> (i32, String) {
    let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
    // SAFETY: each pair is a live buffer of that length or NULL, as the
    // callers below make them and fennec.h allows; the out pointers are
    // locals'.
    let code = unsafe {
        fennec_query(
            h, text.0, text.1, params.0, params.1, vectors.0, vectors.1, &mut out, &mut len,
        )
    };
    taken(code, out, len)
}

fn query(h: u64, text: &str) -> String {
    let b = exact(text.as_bytes());
    let none = (std::ptr::null(), 0);
    let (code, r) = query_raw(h, (b.as_ptr(), b.len()), none, none);
    assert_eq!(code, FENNEC_OK, "{text}: {r}");
    r
}

fn by_handle(
    f: unsafe extern "C" fn(u64, *mut *mut c_char, *mut usize) -> i32,
    h: u64,
) -> (i32, String) {
    let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
    // SAFETY: `f` is one of the C ABI's calls taking a handle and the out
    // pointers, both locals'.
    let code = unsafe { f(h, &mut out, &mut len) };
    taken(code, out, len)
}

const NONE: (*const u8, usize) = (std::ptr::null(), 0);
/// The largest length a `size_t` holds, with no buffer behind it.
const NULL_MAX: (*const u8, usize) = (std::ptr::null(), usize::MAX);

#[test]
fn the_version_is_static_and_ended() {
    let (a, b) = (fennec_version(), fennec_version());
    assert_eq!(a, b, "one static string");
    // SAFETY: `fennec_version` answers a static NUL-ended string.
    let v = unsafe { CStr::from_ptr(a) };
    assert_eq!(v.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn an_open_reads_its_path_to_its_length() {
    let dir = std::env::temp_dir().join(format!("fennec-ffi-bounds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("app.fennec");
    let path = path.to_str().unwrap();
    let open = |bytes: (*const u8, usize), handle: *mut u64| {
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: `bytes` is a live buffer of its length or NULL, `handle` a
        // local's or NULL, the out pointers locals'.
        let code = unsafe { fennec_open(bytes.0, bytes.1, 0, handle, &mut out, &mut len) };
        taken(code, out, len)
    };
    let mut h = 0u64;
    // No path, and a NULL with the largest length: refused, nothing read.
    assert_eq!(open(NONE, &mut h).0, FENNEC_MISUSE);
    assert_eq!(open(NULL_MAX, &mut h).0, FENNEC_MISUSE);
    // A path, and a handle to write it to: none, refused.
    let p = exact(path.as_bytes());
    assert_eq!(
        open((p.as_ptr(), p.len()), std::ptr::null_mut()).0,
        FENNEC_MISUSE
    );
    // A path that is not UTF-8.
    let bad = exact(&[0xff, 0xfe]);
    assert_eq!(open((bad.as_ptr(), bad.len()), &mut h).0, FENNEC_MISUSE);
    // The path read to its length and no further: the bytes after it in
    // the same block name another file, which is never made.
    let b = before(path, ".not-this-one");
    let (code, r) = open((b.as_ptr(), path.len()), &mut h);
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(std::path::Path::new(path).exists());
    assert!(!std::path::Path::new(&format!("{path}.not-this-one")).exists());
    // Every call that takes no buffer, on a file's handle, its out pointers
    // NULL: the answer let go of.
    for f in [fennec_sync, fennec_flush, fennec_checkpoint] {
        // SAFETY: a handle, and both out pointers NULL, as fennec.h allows.
        let code = unsafe { f(h, std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(code, FENNEC_OK);
    }
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
    // Open again from a block exactly the path's length.
    let (code, r) = open((p.as_ptr(), p.len()), &mut h);
    assert_eq!(code, FENNEC_OK, "{r}");
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_handle_no_database_holds_is_refused_by_every_call() {
    for h in [0, u64::MAX] {
        for f in [
            fennec_close,
            fennec_sync,
            fennec_flush,
            fennec_checkpoint,
            fennec_sync_status,
        ] {
            assert_eq!(by_handle(f, h).0, FENNEC_MISUSE);
        }
        let (code, _) = query_raw(h, NONE, NONE, NONE);
        assert_eq!(code, FENNEC_MISUSE);
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: a handle, a number and the out pointers, locals'.
        let code = unsafe { fennec_changes(h, u64::MAX, &mut out, &mut len) };
        assert_eq!(taken(code, out, len).0, FENNEC_MISUSE);
    }
    // SAFETY: NULL for the handle, which is refused, and for the out
    // pointers.
    let code = unsafe {
        fennec_open_memory(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(code, FENNEC_MISUSE);
}

#[test]
fn a_query_reads_each_buffer_to_its_length() {
    let h = memory();
    query(h, "create collection t (a int, e vector<2> @hnsw(l2))");
    query(
        h,
        "put t [{a: 1, e: [1, 0]}, {a: 2, e: [0, 1]}, {a: 3, e: [1, 1]}]",
    );
    // No text: nothing to run, and no read.
    let (code, r) = query_raw(h, NONE, NONE, NONE);
    assert!(code == FENNEC_OK || code == FENNEC_QUERY, "{code} {r}");
    // A NULL with the largest length, where text goes: refused.
    assert_eq!(query_raw(h, NULL_MAX, NONE, NONE).0, FENNEC_MISUSE);
    let t = exact(b"get t select a order a limit 1");
    assert_eq!(
        query_raw(h, (t.as_ptr(), t.len()), NULL_MAX, NONE).0,
        FENNEC_MISUSE
    );
    // Where vectors go a NULL is none, whatever its length.
    let (code, r) = query_raw(h, (t.as_ptr(), t.len()), NONE, NULL_MAX);
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(r.contains(r#""rows":[{"a":1}]"#), "{r}");
    // The text read to its length: `limit 1` followed by a `0` that would
    // make it `limit 10`, and the parameters by a `]` that would close
    // another array.
    let t = before("get t select a order a limit 1", "0");
    let (code, r) = query_raw(h, (t.as_ptr(), 30), NONE, NONE);
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(r.contains(r#""rows":[{"a":1}]"#), "{r}");
    let t = exact(b"get t select a where a = $1");
    let p = before("[2]", ",3]");
    let (code, r) = query_raw(h, (t.as_ptr(), t.len()), (p.as_ptr(), 3), NONE);
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(r.contains(r#""rows":[{"a":2}]"#), "{r}");
    // A vector apart, in a block exactly its place, its length and its two
    // `f32`s long.
    let near = exact(b"get t select a near e $1 limit 1");
    let one = exact(b"[null]");
    let v: Vec<u8> = [0u32.to_le_bytes(), 2u32.to_le_bytes()]
        .concat()
        .into_iter()
        .chain([1.0f32, 0.0].iter().flat_map(|x| x.to_le_bytes()))
        .collect();
    let v = exact(&v);
    let (code, r) = query_raw(
        h,
        (near.as_ptr(), near.len()),
        (one.as_ptr(), one.len()),
        (v.as_ptr(), v.len()),
    );
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(r.contains(r#""rows":[{"a":1"#), "{r}");
    // The same vector one byte short, and one claiming the largest length
    // a `u32` holds over its two values: each refused, nothing read past
    // the block.
    let short = exact(&v[..v.len() - 1]);
    let (code, r) = query_raw(
        h,
        (near.as_ptr(), near.len()),
        (one.as_ptr(), one.len()),
        (short.as_ptr(), short.len()),
    );
    assert_eq!(code, FENNEC_QUERY, "{r}");
    let mut long = v.to_vec();
    long[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let long = exact(&long);
    let (code, r) = query_raw(
        h,
        (near.as_ptr(), near.len()),
        (one.as_ptr(), one.len()),
        (long.as_ptr(), long.len()),
    );
    assert_eq!(code, FENNEC_QUERY, "{r}");
    // A place past the parameters, its top bit (exact) set or not.
    for place in [1u32, u32::MAX, !FENNEC_VECTOR_EXACT] {
        let mut far = v.to_vec();
        far[0..4].copy_from_slice(&place.to_le_bytes());
        let far = exact(&far);
        let (code, r) = query_raw(
            h,
            (near.as_ptr(), near.len()),
            (one.as_ptr(), one.len()),
            (far.as_ptr(), far.len()),
        );
        assert_eq!(code, FENNEC_QUERY, "{place}: {r}");
    }
    // Text that is not UTF-8, cut inside a character at the block's end.
    let cut = exact("get t where a = 'é".as_bytes().split_last().unwrap().1);
    assert_eq!(
        query_raw(h, (cut.as_ptr(), cut.len()), NONE, NONE).0,
        FENNEC_MISUSE
    );
    // Its answer let go of where no out pointer is given.
    let t = exact(b"get t");
    // SAFETY: a live block of its length, NULL for the rest, as fennec.h
    // allows.
    let code = unsafe {
        fennec_query(
            h,
            t.as_ptr(),
            t.len(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(code, FENNEC_OK);
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
}

#[test]
fn changes_take_any_counter() {
    let h = memory();
    query(h, "create collection t (a int)");
    for since in [0, 1, u64::MAX] {
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: a handle, a number and the out pointers, locals'.
        let code = unsafe { fennec_changes(h, since, &mut out, &mut len) };
        let (code, r) = taken(code, out, len);
        assert_eq!(code, FENNEC_OK, "{since}: {r}");
        assert!(r.starts_with(r#"{"seq":"#), "{r}");
    }
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
}

#[test]
fn a_schema_and_a_warm_read_their_text_to_its_length() {
    let h = memory();
    query(h, "create collection t (a int @hash, b text @text)");
    let schema = |text: (*const u8, usize), mode: u32| {
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: a live block of its length or NULL, the out pointers
        // locals'.
        let code = unsafe { fennec_schema(h, text.0, text.1, mode, &mut out, &mut len) };
        taken(code, out, len)
    };
    // Describe reads no text, whatever it is handed.
    let (code, described) = schema(NULL_MAX, FENNEC_SCHEMA_DESCRIBE);
    assert_eq!(code, FENNEC_OK, "{described}");
    assert!(described.contains(r#""t""#), "{described}");
    assert_eq!(schema(NULL_MAX, FENNEC_SCHEMA_PLAN).0, FENNEC_MISUSE);
    assert_eq!(schema(NONE, u32::MAX).0, FENNEC_MISUSE);
    // The description read to its length: the database's own, planned,
    // asks for nothing, the bytes after it in its block a brace that would
    // make it no JSON.
    let d = before(&described, "}");
    let (code, r) = schema((d.as_ptr(), described.len()), FENNEC_SCHEMA_PLAN);
    assert_eq!(code, FENNEC_OK, "{r}");
    let warm = |only: (*const u8, usize)| {
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: a live block of its length or NULL, the out pointers
        // locals'.
        let code = unsafe { fennec_warm(h, only.0, only.1, &mut out, &mut len) };
        taken(code, out, len)
    };
    assert_eq!(warm(NULL_MAX).0, FENNEC_MISUSE);
    let (code, r) = warm(NONE);
    assert_eq!(code, FENNEC_OK, "{r}");
    // `t.a`, followed in its block by `,t.b` it must not read: one index
    // named, at most one built.
    let only = before("t.a", ",t.b");
    let (code, r) = warm((only.as_ptr(), 3));
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(r == r#"{"built":0}"# || r == r#"{"built":1}"#, "{r}");
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
}

#[test]
fn a_sync_reads_its_config_and_events_to_their_length() {
    let h = memory();
    query(h, "create collection tasks (key text @hash, title text)");
    let feed = |kind: u32, status: i32, bytes: (*const u8, usize)| {
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: a live block of its length or NULL, the out pointers
        // locals'.
        let code = unsafe {
            fennec_sync_feed(h, kind, 1, status, 0, bytes.0, bytes.1, &mut out, &mut len)
        };
        taken(code, out, len)
    };
    // Before a start: no sync to feed or ask.
    assert_eq!(feed(FENNEC_SYNC_POLL, 0, NONE).0, FENNEC_MISUSE);
    assert_eq!(by_handle(fennec_sync_status, h).0, FENNEC_MISUSE);
    let start = |config: (*const u8, usize)| {
        let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
        // SAFETY: a live block of its length or NULL, the out pointers
        // locals'.
        let code = unsafe { fennec_sync_start(h, config.0, config.1, &mut out, &mut len) };
        taken(code, out, len)
    };
    assert_eq!(start(NULL_MAX).0, FENNEC_MISUSE);
    assert_ne!(start(NONE).0, FENNEC_OK);
    // The config read to its length, a brace after it in its block.
    let config = r#"{"url":"https://example.test","token":"tk","seed":"ab","shapes":[{"collection":"tasks","key":"key"}]}"#;
    let c = before(config, "}");
    let (code, r) = start((c.as_ptr(), config.len()));
    assert_eq!(code, FENNEC_OK, "{r}");
    assert_eq!(by_handle(fennec_sync_status, h).0, FENNEC_OK);
    // An event's bytes: NULL with the largest length is none, a kind past
    // the last refused, a status past either end of an HTTP one held to it.
    assert_eq!(feed(FENNEC_SYNC_POLL, 0, NULL_MAX).0, FENNEC_OK);
    assert_eq!(feed(u32::MAX, 0, NONE).0, FENNEC_MISUSE);
    for status in [i32::MIN, -1, 0, 200, i32::MAX] {
        let body = exact(b"");
        assert_eq!(
            feed(FENNEC_SYNC_OPENED, status, (body.as_ptr(), 0)).0,
            FENNEC_OK
        );
    }
    // A piece of a stream exactly as long as it says, and a signal read to
    // its length, a brace after it in its block.
    let piece = exact(b"event: ping\ndata: {}\n\n");
    assert_eq!(
        feed(FENNEC_SYNC_BYTES, 0, (piece.as_ptr(), piece.len())).0,
        FENNEC_OK
    );
    let signal = r#"{"online":false}"#;
    let s = before(signal, "}");
    let (code, r) = feed(FENNEC_SYNC_SIGNAL, 0, (s.as_ptr(), signal.len()));
    assert_eq!(code, FENNEC_OK, "{r}");
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
}

/// `FENNEC_SCHEMA_DESCRIBE` leaves its text unread, as fennec.h says it
/// does: handed a pointer to nothing and the largest length, it describes.
/// It read the text as UTF-8 before it looked at the mode, a slice made of
/// whatever the caller passed, a pointer the header told it it could leave
/// as anything.
#[test]
fn a_description_reads_no_text() {
    let h = memory();
    query(h, "create collection t (a int)");
    let nothing = std::ptr::NonNull::<u8>::dangling().as_ptr();
    let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
    // SAFETY: fennec.h says this mode leaves the text unread; the out
    // pointers are locals'.
    let code = unsafe {
        fennec_schema(
            h,
            nothing,
            usize::MAX,
            FENNEC_SCHEMA_DESCRIBE,
            &mut out,
            &mut len,
        )
    };
    let (code, r) = taken(code, out, len);
    assert_eq!(code, FENNEC_OK, "{r}");
    assert!(r.contains(r#""t""#), "{r}");
    assert_eq!(by_handle(fennec_close, h).0, FENNEC_OK);
}

#[test]
fn freeing_null_is_nothing() {
    // SAFETY: NULL, which fennec.h's free takes as nothing.
    unsafe { fennec_free_string(std::ptr::null_mut()) };
}
