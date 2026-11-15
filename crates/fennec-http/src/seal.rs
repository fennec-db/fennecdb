//! Sealed files: an archive's images, segments and history, and a backup,
//! encrypted and authenticated with ChaCha20-Poly1305 (`crypto.rs`) under a
//! key from a file. A backup leaves the machine -- a bucket, another disk --
//! where the disk's own encryption no longer covers it; the live database
//! stays plain, since a mapped file is read where it lies, and is for the
//! disk to encrypt.
//!
//! ```text
//! "FENECSL\x02", the file's id (16 bytes), then frames:
//! [plaintext length u32][nonce 12][ciphertext][tag 16]
//! ```
//!
//! Each frame's nonce is the system's randomness -- a seal is refused
//! without it, never given the clock's -- and so is the file's id. A
//! frame's tag covers the id, its place in the file and whether it is the
//! last: a frame moved, dropped, repeated or taken from another file
//! sealed under the same key does not open, nor does one byte changed, the
//! id's among them, nor a file cut short where a whole one was sealed
//! (`open_whole`). A segment is appended a frame at a time, each holding
//! whole records, so a crash leaves at most its last frame cut short,
//! which is cut off as a torn record is in a plain one (`open_appended`).
//!
//! Version 1 (`FENECSL\x01`, sealed by 0.1.7 to 0.2.0) has no id: its
//! tags cover a frame's place alone, so a frame of another version 1 file
//! under the same key, at the same place, opens in it. Such a file still
//! opens, and a segment of one goes on in its own version
//! ([`FileId::V1`]); every file begun now is version 2. A later version is
//! refused by its number, never read as a plain file.

use crate::crypto;
use std::io;
use std::path::Path;

/// The magic a file is sealed under now: its id follows it.
pub const MAGIC: &[u8; 8] = b"FENECSL\x02";
/// Version 1's: no id after it.
pub const MAGIC_V1: &[u8; 8] = b"FENECSL\x01";
/// The magic's bytes before its version.
const NAME: &[u8; 7] = b"FENECSL";
/// A file's id: 16 bytes of the system's randomness.
const ID: usize = 16;
/// Where a version 2 file's first frame begins: past its magic and id.
pub const START: usize = MAGIC.len() + ID;
const HEAD: usize = 4 + 12;
const TAG: usize = 16;
/// A whole file is sealed a mebibyte a frame.
const FRAME: usize = 1 << 20;

fn corrupt(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// A 256-bit key.
#[derive(Clone)]
pub struct Key([u8; 32]);

impl Key {
    /// The key in `path`: 64 hexadecimal digits, as `fennec key` writes it.
    pub fn from_file(path: &Path) -> io::Result<Key> {
        let text = std::fs::read_to_string(path)?;
        Key::from_hex(text.trim()).ok_or_else(|| {
            corrupt(&format!(
                "{}: a key is 64 hexadecimal digits (`fennec key` writes one)",
                path.display()
            ))
        })
    }

    pub fn from_hex(text: &str) -> Option<Key> {
        if text.len() != 64 {
            return None;
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Key(k))
    }

    /// A new key from the system's randomness, as hexadecimal; `None`
    /// without it.
    pub fn generate() -> Option<String> {
        let k = crypto::system_random(32)?;
        Some(k.iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// The file a frame is sealed into: the id its head holds, which every
/// frame's tag covers, or none in a version 1 file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId(Option<[u8; ID]>);

impl FileId {
    /// A version 1 file's: its frames' tags cover their place alone. Only
    /// a file sealed so goes on in it -- a segment an older archiver began.
    pub const V1: FileId = FileId(None);

    /// A new file's head -- the magic and a fresh id -- appended to `out`.
    /// The id is the system's randomness, as a nonce is, and refused
    /// without it, `out` as it was: two files of one id under one key
    /// would open each other's frames again.
    pub fn begin(out: &mut Vec<u8>) -> io::Result<FileId> {
        let id: [u8; ID] = crypto::system_random(ID)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| {
                io::Error::other("no randomness from the system for a sealed file's id")
            })?;
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&id);
        Ok(FileId(Some(id)))
    }

    /// What frame `index`'s tag covers beside its bytes: the file's id, its
    /// place and whether it is the last; version 1's the last two alone.
    /// Poly1305 takes the length in, so the two never stand for each other.
    fn aad<'a>(&self, index: u64, last: bool, buf: &'a mut [u8; ID + 9]) -> &'a [u8] {
        let at = match &self.0 {
            Some(id) => {
                buf[..ID].copy_from_slice(id);
                ID
            }
            None => 0,
        };
        buf[at..at + 8].copy_from_slice(&index.to_le_bytes());
        buf[at + 8] = last as u8;
        &buf[..at + 9]
    }
}

/// Whether `bytes` are a sealed file's, of any version: one this binary
/// does not open is refused by its version, never read as a plain file.
pub fn is_sealed(bytes: &[u8]) -> bool {
    bytes.len() >= MAGIC.len() && bytes.starts_with(NAME)
}

/// Whether `bytes` are a version 2 file's head cut short -- its magic or
/// its id not whole, as a crash leaves a file it was making -- which holds
/// no frame.
pub fn cut_in_head(bytes: &[u8]) -> bool {
    let magic = bytes.len().min(MAGIC.len());
    bytes.len() < START && bytes[..magic] == MAGIC[..magic]
}

/// The file `bytes`' head names, and where its frames begin; a version
/// this binary seals none of refused by its number.
fn head(bytes: &[u8]) -> io::Result<(FileId, usize)> {
    if !is_sealed(bytes) {
        return Err(corrupt("not a sealed file"));
    }
    match bytes[NAME.len()] {
        1 => Ok((FileId::V1, MAGIC_V1.len())),
        2 => match bytes.get(MAGIC.len()..START).map(<[u8; ID]>::try_from) {
            Some(Ok(id)) => Ok((FileId(Some(id)), START)),
            _ => Err(corrupt("a sealed file cut short inside its id")),
        },
        v => Err(corrupt(&format!(
            "sealed as version {v}, which this binary does not open (it opens 1 and 2): \
             a later fennecdb's"
        ))),
    }
}

/// Frame `index` of `plain` in `file`, appended to `out`. Its nonce is
/// the system's randomness and nothing else: a nonce used twice under a
/// key gives away both frames' keystream and lets their tags be forged, so
/// without the randomness the seal is refused, `out` as it was.
pub fn seal_frame(
    key: &Key,
    file: &FileId,
    index: u64,
    last: bool,
    plain: &[u8],
    out: &mut Vec<u8>,
) -> io::Result<()> {
    let nonce: [u8; 12] = crypto::system_random(12)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| io::Error::other("no randomness from the system for a seal's nonce"))?;
    out.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    out.extend_from_slice(&nonce);
    let at = out.len();
    out.extend_from_slice(plain);
    let mut aad = [0; ID + 9];
    let aad = file.aad(index, last, &mut aad);
    let tag = crypto::seal_in_place(&key.0, &nonce, aad, &mut out[at..]);
    out.extend_from_slice(&tag);
    Ok(())
}

/// `plain` sealed whole: the magic and a new id, then frames of a
/// mebibyte, the last flagged as the last.
pub fn seal_whole(key: &Key, plain: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(plain.len() + plain.len() / FRAME * 32 + 64 + START);
    let file = FileId::begin(&mut out)?;
    let mut chunks = plain.chunks(FRAME).peekable();
    if chunks.peek().is_none() {
        seal_frame(key, &file, 0, true, &[], &mut out)?;
    }
    let mut index = 0;
    while let Some(c) = chunks.next() {
        seal_frame(key, &file, index, chunks.peek().is_none(), c, &mut out)?;
        index += 1;
    }
    Ok(out)
}

/// What [`open_frames`] found: the plaintext, where the last whole frame
/// ends, the frames opened, whether the last was flagged as the last, and
/// the file they are bound to.
struct Opened {
    plain: Vec<u8>,
    end: usize,
    frames: u64,
    last: bool,
    file: FileId,
}

/// The frames of `bytes` past the head: each whole one opened, until one
/// is cut short.
fn open_frames(key: &Key, bytes: &[u8]) -> io::Result<Opened> {
    let (file, start) = head(bytes)?;
    let mut out = Vec::with_capacity(bytes.len());
    let (mut pos, mut index, mut last) = (start, 0u64, false);
    let mut aad = [0; ID + 9];
    while let Some(head) = bytes.get(pos..pos + HEAD) {
        // Zeros from a frame's head to the end are a frame cut short: an
        // append whose length reached the disk before its bytes, as a
        // database's open reads them. A frame's head holds a random nonce,
        // never zeros.
        if head == [0; HEAD] && bytes[pos..].iter().all(|&b| b == 0) {
            break;
        }
        let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(frame) = bytes.get(pos + HEAD..pos + HEAD + len + TAG) else {
            break;
        };
        if last {
            return Err(corrupt("a sealed file goes on past its last frame"));
        }
        let nonce: [u8; 12] = head[4..].try_into().unwrap_or([0; 12]);
        let at = out.len();
        out.extend_from_slice(&frame[..len]);
        // A tag that does not match leaves the bytes as they were, so the
        // frame is tried as the last only after it is not a middle one.
        let tag = &frame[len..];
        let opened = [false, true].into_iter().find(|&l| {
            let aad = file.aad(index, l, &mut aad);
            crypto::open_in_place(&key.0, &nonce, aad, &mut out[at..], tag)
        });
        let Some(l) = opened else {
            return Err(corrupt(&format!(
                "frame {index} does not open: another key, or bytes changed"
            )));
        };
        last = l;
        pos += HEAD + len + TAG;
        index += 1;
    }
    Ok(Opened {
        plain: out,
        end: pos,
        frames: index,
        last,
        file,
    })
}

/// A file sealed whole: every frame opened, the last flagged as the last,
/// nothing after it. Cut short anywhere, it is refused.
pub fn open_whole(key: &Key, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let o = open_frames(key, bytes)?;
    if !o.last || o.end != bytes.len() {
        return Err(corrupt("a sealed file cut short"));
    }
    Ok(o.plain)
}

/// A file appended a frame at a time: its whole frames opened, where they
/// end -- a frame after it is cut short, as a crash leaves one -- how
/// many there are, the next frame's index, and the file the next is
/// sealed into.
pub fn open_appended(key: &Key, bytes: &[u8]) -> io::Result<(Vec<u8>, usize, u64, FileId)> {
    let o = open_frames(key, bytes)?;
    if o.last {
        return Err(corrupt(
            "an appended file holds a frame sealed as a whole file's last",
        ));
    }
    Ok((o.plain, o.end, o.frames, o.file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> Key {
        Key([b; 32])
    }

    #[test]
    fn a_whole_file_opens_as_it_was_and_not_otherwise() {
        let k = key(7);
        for len in [0, 1, FRAME - 1, FRAME, FRAME + 1, 3 * FRAME + 5] {
            let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let sealed = seal_whole(&k, &plain).unwrap();
            assert!(sealed.starts_with(MAGIC));
            assert_eq!(open_whole(&k, &sealed).unwrap(), plain, "{len}");
            assert!(open_whole(&key(8), &sealed).is_err(), "another key");
            let mut flipped = sealed.clone();
            let at = flipped.len() / 2;
            flipped[at] ^= 1;
            assert!(open_whole(&k, &flipped).is_err(), "a byte changed");
            // Cut short at every frame's end but the last, and inside one.
            assert!(open_whole(&k, &sealed[..sealed.len() - 1]).is_err());
            if len > FRAME {
                let first = START + HEAD + FRAME + TAG;
                assert!(open_whole(&k, &sealed[..first]).is_err(), "a frame dropped");
            }
        }
    }

    /// Frames swapped, repeated or one dropped from the middle: each is at
    /// a place its tag does not cover.
    #[test]
    fn frames_out_of_place_do_not_open() {
        let k = key(1);
        let plain = vec![9u8; 3 * FRAME + 10];
        let sealed = seal_whole(&k, &plain).unwrap();
        let frame = HEAD + FRAME + TAG;
        let at = |i: usize| START + i * frame;
        let mut swapped = sealed.clone();
        swapped[at(0)..at(1)].copy_from_slice(&sealed[at(1)..at(2)]);
        swapped[at(1)..at(2)].copy_from_slice(&sealed[at(0)..at(1)]);
        assert!(open_whole(&k, &swapped).is_err(), "two swapped");
        let mut repeated = sealed.clone();
        repeated[at(2)..at(3)].copy_from_slice(&sealed[at(1)..at(2)]);
        assert!(open_whole(&k, &repeated).is_err(), "one repeated");
        let dropped = [&sealed[..at(1)], &sealed[at(2)..]].concat();
        assert!(open_whole(&k, &dropped).is_err(), "one dropped");
    }

    /// Two files sealed under one key, a frame of one put in the other at
    /// its own place: its tag covers the other file's id, and it does not
    /// open -- whole or appended. In version 1 it did, which the id closes.
    #[test]
    fn a_frame_from_another_file_does_not_open() {
        let k = key(2);
        let (a, b) = (vec![1u8; 3 * FRAME], vec![2u8; 3 * FRAME]);
        let (sa, sb) = (seal_whole(&k, &a).unwrap(), seal_whole(&k, &b).unwrap());
        let frame = HEAD + FRAME + TAG;
        let at = |i: usize| START + i * frame;
        for i in 0..3 {
            let mut taken = sa.clone();
            taken[at(i)..at(i + 1)].copy_from_slice(&sb[at(i)..at(i + 1)]);
            let e = open_whole(&k, &taken).unwrap_err().to_string();
            assert!(e.contains(&format!("frame {i} does not open")), "{e}");
        }
        // Appended, as two segments are: a frame of the one going on in
        // the other.
        let segment = |n: u8| {
            let mut out = Vec::new();
            let file = FileId::begin(&mut out).unwrap();
            for i in 0..4u64 {
                seal_frame(&k, &file, i, false, &[n; 40], &mut out).unwrap();
            }
            out
        };
        let (ga, gb) = (segment(1), segment(2));
        let frame = HEAD + 40 + TAG;
        let mut taken = ga.clone();
        taken[START + 2 * frame..START + 3 * frame]
            .copy_from_slice(&gb[START + 2 * frame..START + 3 * frame]);
        let e = open_appended(&k, &taken).unwrap_err().to_string();
        assert!(e.contains("frame 2 does not open"), "{e}");
        // The same taking between two version 1 files opens: their frames
        // are bound to their places alone.
        let v1 = |n: u8| {
            let mut out = MAGIC_V1.to_vec();
            for i in 0..4u64 {
                seal_frame(&k, &FileId::V1, i, false, &[n; 40], &mut out).unwrap();
            }
            out
        };
        let (va, vb) = (v1(1), v1(2));
        let mut taken = va.clone();
        let m = MAGIC_V1.len();
        taken[m + 2 * frame..m + 3 * frame].copy_from_slice(&vb[m + 2 * frame..m + 3 * frame]);
        let (plain, ..) = open_appended(&k, &taken).unwrap();
        assert_eq!(plain[80..120], [2; 40]);
    }

    /// The id is the tags' as much as the bytes are: one altered, every
    /// frame fails; cut short, the file is refused, whole or appended; and
    /// a file's head is held to its version.
    #[test]
    fn an_id_cut_short_or_changed_does_not_open() {
        let k = key(6);
        let sealed = seal_whole(&k, &vec![3u8; FRAME + 7]).unwrap();
        for byte in [MAGIC.len(), START - 1] {
            let mut changed = sealed.clone();
            changed[byte] ^= 0x80;
            let e = open_whole(&k, &changed).unwrap_err().to_string();
            assert!(e.contains("frame 0 does not open"), "{e}");
        }
        for cut in [MAGIC.len(), MAGIC.len() + 5, START - 1] {
            assert!(cut_in_head(&sealed[..cut]), "{cut}");
            let e = open_whole(&k, &sealed[..cut]).unwrap_err().to_string();
            assert!(e.contains("inside its id"), "{e}");
            assert!(open_appended(&k, &sealed[..cut]).is_err());
        }
        // Cut inside the magic: a start of a head. Whole, a head is no cut.
        assert!(cut_in_head(&sealed[..3]) && cut_in_head(&[]));
        assert!(!cut_in_head(&sealed[..START]));
        assert!(!cut_in_head(MAGIC_V1), "version 1's magic is whole");
        // The id dropped and the magic made version 1's: the tags cover an
        // id, and do not open as version 1's.
        let mut downgraded = MAGIC_V1.to_vec();
        downgraded.extend_from_slice(&sealed[START..]);
        assert!(open_whole(&k, &downgraded).is_err());
        // A later version is refused by its number, never read as plain.
        let mut later = sealed.clone();
        later[NAME.len()] = 3;
        assert!(is_sealed(&later));
        let e = open_whole(&k, &later).unwrap_err().to_string();
        assert!(e.contains("version 3"), "{e}");
    }

    /// A version 1 file, as 0.2.0 sealed one: it opens whole, and appended
    /// it says so, and goes on in its own version.
    #[test]
    fn a_version_1_file_opens_and_goes_on_as_one() {
        let k = key(9);
        let mut whole = MAGIC_V1.to_vec();
        seal_frame(&k, &FileId::V1, 0, false, &[4; FRAME], &mut whole).unwrap();
        seal_frame(&k, &FileId::V1, 1, true, b"end", &mut whole).unwrap();
        let plain = open_whole(&k, &whole).unwrap();
        assert_eq!((plain.len(), &plain[FRAME..]), (FRAME + 3, &b"end"[..]));
        let mut appended = MAGIC_V1.to_vec();
        seal_frame(&k, &FileId::V1, 0, false, b"one", &mut appended).unwrap();
        let (plain, end, frames, file) = open_appended(&k, &appended).unwrap();
        assert_eq!(
            (&plain[..], end, frames, file),
            (&b"one"[..], appended.len(), 1, FileId::V1)
        );
        seal_frame(&k, &file, frames, false, b"two", &mut appended).unwrap();
        assert_eq!(open_appended(&k, &appended).unwrap().0, b"onetwo");
        // A frame sealed with an id does not go on in it.
        let mut other = Vec::new();
        let id = FileId::begin(&mut other).unwrap();
        seal_frame(&k, &id, 2, false, b"three", &mut appended).unwrap();
        assert!(open_appended(&k, &appended).is_err());
    }

    #[test]
    fn an_appended_file_loses_only_a_frame_cut_short() {
        let k = key(3);
        let mut file = Vec::new();
        let id = FileId::begin(&mut file).unwrap();
        for i in 0..5u64 {
            seal_frame(&k, &id, i, false, &[i as u8; 40], &mut file).unwrap();
        }
        let whole = file.len();
        seal_frame(&k, &id, 5, false, &[5; 40], &mut file).unwrap();
        let cut = &file[..file.len() - 3];
        let (plain, end, frames, at) = open_appended(&k, cut).unwrap();
        assert_eq!((plain.len(), end, frames, at), (200, whole, 5, id));
        // Zeros to the end, as an append whose length reached the disk
        // before its bytes leaves them, are a frame cut short; zeros with a
        // byte after them are a frame that does not open.
        let mut zeros = file[..whole].to_vec();
        zeros.resize(whole + 4096, 0);
        let (plain, end, frames, _) = open_appended(&k, &zeros).unwrap();
        assert_eq!((plain.len(), end, frames), (200, whole, 5));
        zeros.push(1);
        assert!(open_appended(&k, &zeros).is_err());
        // Sealed whole, a file is no appended one.
        assert!(open_appended(&k, &seal_whole(&k, b"x").unwrap()).is_err());
    }

    fn nonce_of(frame: &[u8]) -> [u8; 12] {
        frame[4..HEAD].try_into().unwrap()
    }

    /// Without the system's randomness a seal is refused -- nothing
    /// appended, no frame under a nonce of the clock's, no file under an
    /// id of it -- and a key is made by none.
    #[test]
    fn no_randomness_refuses_a_seal() {
        let k = key(4);
        crypto::without_randomness(|| {
            let mut out = MAGIC_V1.to_vec();
            assert!(seal_frame(&k, &FileId::V1, 0, false, b"secret", &mut out).is_err());
            assert_eq!(out, MAGIC_V1, "a refused seal appends nothing");
            let mut head = Vec::new();
            assert!(FileId::begin(&mut head).is_err());
            assert!(head.is_empty(), "a refused id appends nothing");
            assert!(seal_whole(&k, b"secret").is_err());
            assert!(seal_whole(&k, b"").is_err());
            assert!(Key::generate().is_none());
        });
        assert!(seal_whole(&k, b"secret").is_ok());
    }

    /// Frames sealed back to back, in one tick of the clock, never share a
    /// nonce, nor files an id: a smoke test of the source, which a clock's
    /// fallback failed.
    #[test]
    fn frames_sealed_back_to_back_take_their_own_nonces() {
        let k = key(5);
        let mut seen = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for i in 0..10_000u64 {
            let mut frame = Vec::new();
            let id = FileId::begin(&mut frame).unwrap();
            assert!(ids.insert(id), "file {i}'s id came again");
            seal_frame(&k, &id, i, false, b"", &mut frame).unwrap();
            assert!(
                seen.insert(nonce_of(&frame[START..])),
                "frame {i}'s nonce came again"
            );
        }
        let sealed = seal_whole(&k, &vec![1u8; 3 * FRAME]).unwrap();
        let frame = HEAD + FRAME + TAG;
        let nonces: std::collections::HashSet<_> = (0..3)
            .map(|i| nonce_of(&sealed[START + i * frame..]))
            .collect();
        assert_eq!(nonces.len(), 3);
    }
}
