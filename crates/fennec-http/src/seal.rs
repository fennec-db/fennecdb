//! Sealed files: an archive's images, segments, history and a tenant's
//! presence, and a backup, encrypted and authenticated with
//! ChaCha20-Poly1305 (`crypto.rs`) under a key from a file. A backup leaves
//! the machine -- a bucket, another disk -- where the disk's own encryption
//! no longer covers it; the live database stays plain, since a mapped file
//! is read where it lies, and is for the disk to encrypt.
//!
//! ```text
//! "FENECSL\x03", the file's id (16 bytes), what the file is:
//!   [role u8][the archive's id 16][change u64][time u64][began u64]
//!   [the tenant's name: its length u8, its bytes]
//! then frames:
//!   [plaintext length u32][nonce 12][ciphertext][tag 16]
//! ```
//!
//! Each file is sealed under a key of its own, HChaCha20 (as XChaCha20
//! derives its subkey) of the file's id under a key made from the key for
//! that alone ([`FILE_KEYS`]), never under the key itself, which seals
//! versions 1 and 2's frames; and each frame's nonce is the system's
//! randomness -- a seal is refused without it, never given the clock's --
//! and so is the file's id. A frame's tag covers the tag of
//! the frame before it -- the first frame's, the file's whole head -- its
//! place in the file and whether it is the last: a frame moved, dropped,
//! repeated or taken from another file does not open, nor two runs of one
//! file's frames spliced where a crash cut it and it went on, nor one byte
//! changed, the head's among them, nor a file cut short where a whole one
//! was sealed (`open_whole`). What remains is a file cut short at a frame's
//! edge, which is what a crash leaves of a file appended a frame at a time
//! (`open_appended`): the archive's own order of changes notices it in any
//! but its last segment.
//!
//! The head says what the file is (`Binding`), under the first frame's
//! tag: a segment, an image, an archive's history, a tenant's presence or a
//! backup; the archive it belongs to, by an id it made as it began, and the
//! tenant whose it is; a segment's first change, an image's change and
//! time, and when its archive took its first image. A reader holds each
//! file it reads to them (`archive.rs`).
//!
//! Version 2 (`FENECSL\x02`, main's builds before 1.0 alone) has the id and
//! no more: its frames are sealed under the key, their tags cover the id,
//! their place and their last-ness. Version 1 (`FENECSL\x01`, 0.1.7 to
//! 0.2.0) has no id: its tags cover a frame's place alone. Both still open,
//! and nothing new is sealed in either; a later version is refused by its
//! number, never read as a plain file.

use crate::crypto;
use std::io;
use std::path::Path;

/// The magic a file is sealed under now: its id and what it is follow it.
pub const MAGIC: &[u8; 8] = b"FENECSL\x03";
/// Version 2's: its id alone after it.
pub const MAGIC_V2: &[u8; 8] = b"FENECSL\x02";
/// Version 1's: nothing after it.
pub const MAGIC_V1: &[u8; 8] = b"FENECSL\x01";
/// The magic's bytes before its version.
const NAME: &[u8; 7] = b"FENECSL";
/// A file's id: 16 bytes of the system's randomness.
const ID: usize = 16;
/// A version 3 head before the tenant's name: the magic, the id, the role,
/// the archive's id, three numbers and the name's length.
const BOUND: usize = 8 + ID + 1 + ID + 24 + 1;
/// The longest tenant's name a head holds.
const TENANT: usize = u8::MAX as usize;
/// Where a version 3 file naming no tenant begins its first frame.
pub const START: usize = BOUND;
/// What a frame's tag covers beside its bytes, at most: a version 3 head
/// naming the longest tenant, its place and its last-ness.
const AAD: usize = BOUND + TENANT + 9;
const HEAD: usize = 4 + 12;
const TAG: usize = 16;
/// A frame's bytes at most, and a whole file is sealed so many a frame: a
/// longer batch of writes goes out as several.
pub const FRAME: usize = 1 << 20;

fn corrupt(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// What a version 3 file's key is made under: HKDF-Expand (RFC 5869, 2.3)
/// of the key with this info, 32 bytes -- HMAC-SHA256(key, info || 0x01),
/// the key a uniformly random one, so no Extract -- and the file's key is
/// HChaCha20 of that and the file's id ([`file_key`]).
///
/// Never HChaCha20 of the key itself, as version 3 was first made: the key
/// seals versions 1 and 2's frames with ChaCha20, and a ChaCha20 block at
/// counter c and nonce N is HChaCha20's rounds over the same state with the
/// state added back, so 32 known bytes of a block's keystream give
/// HChaCha20(key, c || N): the key of a version 3 file whose id, read from
/// the file, is c || N (1.0's review,
/// `an_older_files_keystream_gives_no_file_key`). Those blocks are at
/// counter 0 (Poly1305's key) and from 1 on (the keystream) under a frame's
/// random nonce, wrapping at 2^32 blocks, and nothing in those versions
/// bounded a frame but memory: a constant input with a counter of 2^32 - 1
/// is outside them only for frames under 256 GiB and by the nonce's
/// randomness. HMAC-SHA256 shares no rounds with ChaCha20, so nothing
/// ChaCha20 under the key gives away tells anything of it, at any frame's
/// length or nonce, and it is held to RFC 4231's vectors and Wycheproof's
/// already. Made once a key ([`Key::of`]): a file pays the one
/// HChaCha20 it did. That key is used by HChaCha20 alone, never by
/// ChaCha20, so no keystream gives HChaCha20 under it away; a file's key is
/// used by ChaCha20 alone, and what its keystream gives away, HChaCha20
/// under the file's key, makes no key.
pub const FILE_KEYS: &[u8] = b"fennecdb sealed file keys, version 3";

/// A 256-bit key, and what version 3 files' keys are made under.
#[derive(Clone)]
pub struct Key {
    /// The key itself: versions 1 and 2 sealed their frames under it.
    raw: [u8; 32],
    /// What version 3 files' keys are made under ([`FILE_KEYS`]).
    files: [u8; 32],
}

/// A version 3 file's own key, from its id.
fn file_key(key: &Key, id: &[u8; ID]) -> [u8; 32] {
    crypto::hchacha20(&key.files, id)
}

impl Key {
    /// `raw`, and the key its version 3 files' keys are made under.
    fn of(raw: [u8; 32]) -> Key {
        let files = crypto::HmacKey::new(&raw).mac(&[FILE_KEYS, &[1]]);
        Key { raw, files }
    }

    /// The key in `path`: 64 hexadecimal digits, as `fennec key` writes it.
    /// A file others than its owner may read is warned of, never refused:
    /// whoever reads the key opens every archive and backup it sealed, and
    /// seals files that open as theirs.
    pub fn from_file(path: &Path) -> io::Result<Key> {
        let text = std::fs::read_to_string(path)?;
        let key = Key::from_hex(text.trim()).ok_or_else(|| {
            corrupt(&format!(
                "{}: a key is 64 hexadecimal digits (`fennec key` writes one)",
                path.display()
            ))
        })?;
        if let Some(warning) = open_key(path) {
            crate::log!("{warning}");
        }
        Ok(key)
    }

    pub fn from_hex(text: &str) -> Option<Key> {
        if text.len() != 64 {
            return None;
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Key::of(k))
    }

    /// A new key from the system's randomness, as hexadecimal; `None`
    /// without it.
    pub fn generate() -> Option<String> {
        let k = crypto::system_random(32)?;
        Some(k.iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// What to say of a key file open to more than its owner, by the rule
/// `--tls-key`'s is held to (`conn::open_to`), naming the file and its mode
/// and never a byte of it; `None` where it is not, or off Unix.
pub fn open_key(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        let who = crate::conn::open_to(m.mode(), m.uid())?;
        Some(format!(
            "warning: --key-file {} is open to {who} (mode {:04o}): whoever reads it opens \
             every archive and backup sealed with it, and seals files that open as theirs. \
             chmod 600 it, or 640 where root owns it and the archiver reads it as its group",
            path.display(),
            m.mode() & 0o7777
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// What a version 3 file is, which its head names under its first frame's
/// tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// An archive's segment of writes, `writes-<first>.log`.
    Segment,
    /// An archive's base image, `image-<change>-<time>.fennec`.
    Image,
    /// The history an archive is on, `history`.
    History,
    /// What a node said of a tenant's presence on it, `presence`.
    Presence,
    /// A backup file, `fennec backup`'s.
    Backup,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Role::Segment => 1,
            Role::Image => 2,
            Role::History => 3,
            Role::Presence => 4,
            Role::Backup => 5,
        }
    }

    fn of(b: u8) -> Option<Role> {
        Some(match b {
            1 => Role::Segment,
            2 => Role::Image,
            3 => Role::History,
            4 => Role::Presence,
            5 => Role::Backup,
            _ => return None,
        })
    }

    /// The role in words, as a refusal names it.
    pub fn name(self) -> &'static str {
        match self {
            Role::Segment => "an archive's segment of writes",
            Role::Image => "an archive's image",
            Role::History => "an archive's history",
            Role::Presence => "a tenant's presence",
            Role::Backup => "a backup",
        }
    }
}

/// What a version 3 file is bound to beside its own id: its head, which
/// its first frame's tag covers, and every frame's after it through theirs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub role: Role,
    /// The archive's id, made once as it began; zeros for a backup.
    pub archive: [u8; ID],
    /// A segment's first change, an image's change; 0 otherwise.
    pub change: u64,
    /// An image's time, ms since the epoch; 0 otherwise.
    pub time: u64,
    /// An image's: when its archive took its first image, ms since the
    /// epoch; 0 otherwise.
    pub began: u64,
    /// The tenant whose archive it is; empty for one database's, and a
    /// backup's.
    pub tenant: String,
}

impl Binding {
    /// A backup file's: no archive, no tenant.
    pub fn backup() -> Binding {
        Binding {
            role: Role::Backup,
            archive: [0; ID],
            change: 0,
            time: 0,
            began: 0,
            tenant: String::new(),
        }
    }

    /// Its bytes after the file's id; a tenant's name longer than a head
    /// holds is refused, nothing appended.
    fn put(&self, out: &mut Vec<u8>) -> io::Result<()> {
        if self.tenant.len() > TENANT {
            return Err(io::Error::other(format!(
                "a tenant's name of {} bytes, past the {TENANT} a sealed file's head holds",
                self.tenant.len()
            )));
        }
        out.push(self.role.byte());
        out.extend_from_slice(&self.archive);
        for n in [self.change, self.time, self.began] {
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.push(self.tenant.len() as u8);
        out.extend_from_slice(self.tenant.as_bytes());
        Ok(())
    }

    /// The binding `bytes` begin with -- a version 3 head past its id --
    /// and its length; `Ok(None)` where they are cut short inside it.
    fn read(bytes: &[u8]) -> io::Result<Option<(Binding, usize)>> {
        let fixed = BOUND - MAGIC.len() - ID;
        if bytes.len() < fixed {
            return Ok(None);
        }
        let role = Role::of(bytes[0]).ok_or_else(|| {
            corrupt(&format!(
                "a sealed file of a kind this binary does not know ({})",
                bytes[0]
            ))
        })?;
        let n = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or([0; 8]));
        let len = bytes[fixed - 1] as usize;
        let Some(name) = bytes.get(fixed..fixed + len) else {
            return Ok(None);
        };
        let tenant = std::str::from_utf8(name)
            .map_err(|_| corrupt("a sealed file's head names a tenant in bytes no name is"))?;
        Ok(Some((
            Binding {
                role,
                archive: bytes[1..1 + ID].try_into().unwrap_or([0; ID]),
                change: n(1 + ID),
                time: n(1 + ID + 8),
                began: n(1 + ID + 16),
                tenant: tenant.to_string(),
            },
            fixed + len,
        )))
    }
}

/// What the next frame of a file is sealed with: the key its version seals
/// under -- a version 3 file's own -- what its tag covers before its place,
/// and its place.
#[derive(Clone)]
pub struct Frames {
    key: [u8; 32],
    kind: Kind,
    index: u64,
}

#[derive(Clone)]
enum Kind {
    V1,
    V2([u8; ID]),
    /// The file's head, for its first frame; the tag of the frame before,
    /// for every frame after.
    V3(Chain),
}

#[derive(Clone)]
enum Chain {
    Head(Vec<u8>),
    Tag([u8; TAG]),
}

impl Frames {
    /// A version 1 file's, as 0.2.0 sealed one: its frames bound to their
    /// places alone. Nothing seals so but a test making such a file.
    #[doc(hidden)]
    pub fn v1(key: &Key) -> Frames {
        Frames {
            key: key.raw,
            kind: Kind::V1,
            index: 0,
        }
    }

    /// A version 2 file's of id `id`, as main sealed one before 1.0: its
    /// frames under the key itself, bound to the id and their places.
    /// Nothing seals so but a test making such a file.
    #[doc(hidden)]
    pub fn v2(key: &Key, id: [u8; ID]) -> Frames {
        Frames {
            key: key.raw,
            kind: Kind::V2(id),
            index: 0,
        }
    }

    /// The version of the file it seals frames of.
    pub fn version(&self) -> u8 {
        match self.kind {
            Kind::V1 => 1,
            Kind::V2(_) => 2,
            Kind::V3(_) => 3,
        }
    }

    /// The frames sealed so far: the next one's place.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// What the next frame's tag covers beside its bytes: in version 3 the
    /// head or the tag before, in version 2 the id, then its place and
    /// whether it is the last. Poly1305 takes the length in, so none of
    /// them stands for another.
    fn aad<'a>(&self, last: bool, buf: &'a mut [u8; AAD]) -> &'a [u8] {
        let before: &[u8] = match &self.kind {
            Kind::V1 => &[],
            Kind::V2(id) => id,
            Kind::V3(Chain::Head(head)) => head,
            Kind::V3(Chain::Tag(tag)) => tag,
        };
        let at = before.len();
        buf[..at].copy_from_slice(before);
        buf[at..at + 8].copy_from_slice(&self.index.to_le_bytes());
        buf[at + 8] = last as u8;
        &buf[..at + 9]
    }

    /// On to the next frame, the one at `index` having taken `tag`.
    fn advance(&mut self, tag: [u8; TAG]) {
        if let Kind::V3(chain) = &mut self.kind {
            *chain = Chain::Tag(tag);
        }
        self.index += 1;
    }

    /// `plain` sealed as the next frame and appended to `out`. Its nonce
    /// is the system's randomness and nothing else: a nonce used twice
    /// under a key gives away both frames' keystream and lets their tags be
    /// forged, so without the randomness the seal is refused, `out` as it
    /// was. A frame holds [`FRAME`] bytes at most: its length is a `u32`,
    /// written as it is, never cut to one.
    pub fn seal(&mut self, last: bool, plain: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        let len = u32::try_from(plain.len())
            .ok()
            .filter(|&n| n as usize <= FRAME)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "a frame of {} bytes, past the {FRAME} a sealed frame holds",
                    plain.len()
                ))
            })?;
        let nonce: [u8; 12] = crypto::system_random(12)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| io::Error::other("no randomness from the system for a seal's nonce"))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nonce);
        let at = out.len();
        out.extend_from_slice(plain);
        let mut aad = [0; AAD];
        let aad = self.aad(last, &mut aad);
        let tag = crypto::seal_in_place(&self.key, &nonce, aad, &mut out[at..]);
        out.extend_from_slice(&tag);
        self.advance(tag);
        Ok(())
    }
}

/// A new version 3 file's head -- the magic, a fresh id and what the file
/// is -- appended to `out`, and what its frames are sealed with. The id
/// is the system's randomness, as a nonce is, and refused without it, `out`
/// as it was: two files of one id under one key would share their key.
pub fn begin(key: &Key, binding: &Binding, out: &mut Vec<u8>) -> io::Result<Frames> {
    let mut head = Vec::with_capacity(BOUND + binding.tenant.len());
    let id: [u8; ID] = crypto::system_random(ID)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| io::Error::other("no randomness from the system for a sealed file's id"))?;
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&id);
    binding.put(&mut head)?;
    out.extend_from_slice(&head);
    Ok(Frames {
        key: file_key(key, &id),
        kind: Kind::V3(Chain::Head(head)),
        index: 0,
    })
}

/// Whether `bytes` are a sealed file's, of any version: one this binary
/// does not open is refused by its version, never read as a plain file.
pub fn is_sealed(bytes: &[u8]) -> bool {
    bytes.len() >= MAGIC.len() && bytes.starts_with(NAME)
}

/// Whether `bytes` are a version 2 or 3 file's head cut short -- its magic,
/// its id or what it is not whole, as a crash leaves a file it was making
/// -- which holds no frame.
pub fn cut_in_head(bytes: &[u8]) -> bool {
    if bytes.len() < MAGIC.len() {
        return bytes == &NAME[..bytes.len().min(NAME.len())];
    }
    if bytes[..MAGIC.len()] == MAGIC_V2[..] {
        return bytes.len() < MAGIC.len() + ID;
    }
    bytes[..MAGIC.len()] == MAGIC[..]
        && (bytes.len() < MAGIC.len() + ID
            || matches!(Binding::read(&bytes[MAGIC.len() + ID..]), Ok(None)))
}

/// What a version 3 file's head says it is, read without opening it --
/// to learn which archive a directory's files say they are of before any
/// is read. Nothing vouches for it until the file is opened, which holds
/// its frames to it. `None` for an older version, a plain file or a head
/// cut short or unknown.
pub fn bound_in(bytes: &[u8]) -> Option<Binding> {
    if !bytes.starts_with(MAGIC) {
        return None;
    }
    Binding::read(bytes.get(MAGIC.len() + ID..)?)
        .ok()
        .flatten()
        .map(|b| b.0)
}

/// The bytes a version 3 head may take at most: what [`bound_in`] needs.
pub const HEAD_MAX: usize = BOUND + TENANT;

/// The bytes [`vouched_in`] reads at most: a head, and a first frame of a
/// mebibyte.
pub const FIRST_MAX: usize = HEAD_MAX + HEAD + FRAME + TAG;

/// What a version 3 file's head says it is, vouched for by its first
/// frame's tag, which covers the head: read from the file's first
/// [`FIRST_MAX`] bytes, or all it has. A choice made from what a file is
/// before it is read -- which archive a directory is, when it began -- is
/// made from this, never from [`bound_in`]'s, which anyone who can write the
/// file can change. `None` for an older version or a plain file, and for a
/// head or a first frame cut short, which a crash leaves of a file just
/// begun and which says nothing; refused where the first frame does not
/// open.
pub fn vouched_in(key: &Key, bytes: &[u8]) -> io::Result<Option<Binding>> {
    if !bytes.starts_with(MAGIC) || cut_in_head(bytes) {
        return Ok(None);
    }
    let (frames, binding, start) = head(key, bytes)?;
    let Some(h) = bytes.get(start..start + HEAD) else {
        return Ok(None);
    };
    // Zeros to the end: an append whose length reached the disk before its
    // bytes, as `open_frames` reads them.
    if h == [0; HEAD] && bytes[start..].iter().all(|&b| b == 0) {
        return Ok(None);
    }
    let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
    let end = len.checked_add(start + HEAD + TAG);
    let Some(frame) = end.and_then(|end| bytes.get(start + HEAD..end)) else {
        return Ok(None);
    };
    let nonce: [u8; 12] = h[4..].try_into().unwrap_or([0; 12]);
    let tag: [u8; TAG] = frame[len..].try_into().unwrap_or([0; TAG]);
    let mut plain = frame[..len].to_vec();
    let mut aad = [0; AAD];
    let opened = [false, true].into_iter().any(|l| {
        let aad = frames.aad(l, &mut aad);
        crypto::open_in_place(&frames.key, &nonce, aad, &mut plain, &tag)
    });
    if !opened {
        return Err(corrupt(
            "frame 0 does not open: another key, or bytes changed",
        ));
    }
    Ok(binding)
}

/// The file `bytes`' head names: what its frames are sealed with, what it
/// is (version 3), and where its frames begin; a version this binary seals
/// none of refused by its number.
fn head(key: &Key, bytes: &[u8]) -> io::Result<(Frames, Option<Binding>, usize)> {
    if !is_sealed(bytes) {
        return Err(corrupt("not a sealed file"));
    }
    let id = || -> io::Result<[u8; ID]> {
        match bytes
            .get(MAGIC.len()..MAGIC.len() + ID)
            .map(<[u8; ID]>::try_from)
        {
            Some(Ok(id)) => Ok(id),
            _ => Err(corrupt("a sealed file cut short inside its id")),
        }
    };
    let frames = |kind, key| Frames {
        key,
        kind,
        index: 0,
    };
    match bytes[NAME.len()] {
        1 => Ok((frames(Kind::V1, key.raw), None, MAGIC_V1.len())),
        2 => Ok((frames(Kind::V2(id()?), key.raw), None, MAGIC.len() + ID)),
        3 => {
            let id = id()?;
            let Some((binding, len)) = Binding::read(&bytes[MAGIC.len() + ID..])? else {
                return Err(corrupt("a sealed file cut short inside its head"));
            };
            let start = MAGIC.len() + ID + len;
            let kind = Kind::V3(Chain::Head(bytes[..start].to_vec()));
            let key = file_key(key, &id);
            Ok((frames(kind, key), Some(binding), start))
        }
        v => Err(corrupt(&format!(
            "sealed as version {v}, which this binary does not open (it opens 1 to 3): \
             a later fennecdb's"
        ))),
    }
}

/// `plain` sealed whole, as `binding` says it is: the magic, a new id and
/// the binding, then frames of a mebibyte, the last flagged as the last.
pub fn seal_whole(key: &Key, binding: &Binding, plain: &[u8]) -> io::Result<Vec<u8>> {
    seal_whole_by(key, binding, plain, FRAME)
}

/// [`seal_whole`] in frames of `frame` bytes: what a test drives at a
/// frame's edge without a mebibyte of bytes.
pub fn seal_whole_by(
    key: &Key,
    binding: &Binding,
    plain: &[u8],
    frame: usize,
) -> io::Result<Vec<u8>> {
    let frame = frame.clamp(1, FRAME);
    let mut out = Vec::with_capacity(plain.len() + plain.len() / frame * 32 + 64 + HEAD_MAX);
    let mut frames = begin(key, binding, &mut out)?;
    let mut chunks = plain.chunks(frame).peekable();
    if chunks.peek().is_none() {
        frames.seal(true, &[], &mut out)?;
    }
    while let Some(c) = chunks.next() {
        frames.seal(chunks.peek().is_none(), c, &mut out)?;
    }
    Ok(out)
}

/// What [`open_frames`] found: the plaintext, where the last whole frame
/// ends, the frames to go on with after it, whether the last was flagged
/// as the last, and what the file says it is.
struct Opened {
    plain: Vec<u8>,
    end: usize,
    next: Frames,
    last: bool,
    binding: Option<Binding>,
}

/// The frames of `bytes` past the head: each whole one opened, until one
/// is cut short -- or, given `stop`, until those opened hold that many
/// bytes, refused where no frame ends there.
fn open_frames(key: &Key, bytes: &[u8], stop: Option<usize>) -> io::Result<Opened> {
    let (mut frames, binding, start) = head(key, bytes)?;
    let mut out = Vec::with_capacity(bytes.len());
    let (mut pos, mut last) = (start, false);
    let mut aad = [0; AAD];
    while let Some(head) = bytes.get(pos..pos + HEAD) {
        if stop.is_some_and(|n| out.len() >= n) {
            break;
        }
        // Zeros from a frame's head to the end are a frame cut short: an
        // append whose length reached the disk before its bytes, as a
        // database's open reads them. A frame's head holds a random nonce,
        // never zeros.
        if head == [0; HEAD] && bytes[pos..].iter().all(|&b| b == 0) {
            break;
        }
        let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        // Checked: on a 32-bit target a length near 4 GiB wraps the sum.
        let end = len.checked_add(pos + HEAD + TAG);
        let Some(frame) = end.and_then(|end| bytes.get(pos + HEAD..end)) else {
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
        let tag: [u8; TAG] = frame[len..].try_into().unwrap_or([0; TAG]);
        let opened = [false, true].into_iter().find(|&l| {
            let aad = frames.aad(l, &mut aad);
            crypto::open_in_place(&frames.key, &nonce, aad, &mut out[at..], &tag)
        });
        let Some(l) = opened else {
            return Err(corrupt(&format!(
                "frame {} does not open: another key, or bytes changed",
                frames.index
            )));
        };
        last = l;
        frames.advance(tag);
        pos += HEAD + len + TAG;
    }
    if let Some(n) = stop.filter(|&n| n != out.len()) {
        return Err(corrupt(&format!(
            "no frame ends at byte {n} of what it holds: a record cut short inside a whole frame"
        )));
    }
    Ok(Opened {
        plain: out,
        end: pos,
        next: frames,
        last,
        binding,
    })
}

/// A file sealed whole: every frame opened, the last flagged as the last,
/// nothing after it, and what it says it is (version 3). Cut short
/// anywhere, it is refused.
pub fn open_whole(key: &Key, bytes: &[u8]) -> io::Result<(Vec<u8>, Option<Binding>)> {
    let o = open_frames(key, bytes, None)?;
    if !o.last || o.end != bytes.len() {
        return Err(corrupt("a sealed file cut short"));
    }
    Ok((o.plain, o.binding))
}

/// A file appended a frame at a time, opened.
pub struct Appended {
    /// What its whole frames hold.
    pub plain: Vec<u8>,
    /// Where they end: a frame after it is cut short, as a crash leaves one.
    pub end: usize,
    /// What the frame after them is sealed with.
    pub next: Frames,
    /// What it says it is (version 3).
    pub binding: Option<Binding>,
}

/// A file appended a frame at a time: its whole frames opened.
pub fn open_appended(key: &Key, bytes: &[u8]) -> io::Result<Appended> {
    appended(open_frames(key, bytes, None)?)
}

/// [`open_appended`], its frames opened as far as the one ending at byte
/// `plain` of what they hold, refused where none does: a record cut short
/// after it, split across frames, is cut there.
pub fn open_appended_to(key: &Key, bytes: &[u8], plain: usize) -> io::Result<Appended> {
    appended(open_frames(key, bytes, Some(plain))?)
}

fn appended(o: Opened) -> io::Result<Appended> {
    if o.last {
        return Err(corrupt(
            "an appended file holds a frame sealed as a whole file's last",
        ));
    }
    Ok(Appended {
        plain: o.plain,
        end: o.end,
        next: o.next,
        binding: o.binding,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> Key {
        Key::of([b; 32])
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// An image's binding, tenant and all: what a whole file is sealed as
    /// in these tests.
    fn image() -> Binding {
        Binding {
            role: Role::Image,
            archive: [7; ID],
            change: 41,
            time: 1_700_000_000_000,
            began: 1_600_000_000_000,
            tenant: "acme".into(),
        }
    }

    fn segment(first: u64) -> Binding {
        Binding {
            role: Role::Segment,
            archive: [7; ID],
            change: first,
            time: 0,
            began: 0,
            tenant: String::new(),
        }
    }

    /// A segment of `n` frames of 40 bytes each, `i`'s bytes `i`: the file
    /// and where each frame ends.
    fn appended(k: &Key, n: u64) -> (Vec<u8>, Vec<usize>, Frames) {
        let mut file = Vec::new();
        let mut frames = begin(k, &segment(2), &mut file).unwrap();
        let mut ends = Vec::new();
        for i in 0..n {
            frames.seal(false, &[i as u8; 40], &mut file).unwrap();
            ends.push(file.len());
        }
        (file, ends, frames)
    }

    #[test]
    fn a_whole_file_opens_as_it_was_and_not_otherwise() {
        let k = key(7);
        for len in [0, 1, FRAME - 1, FRAME, FRAME + 1, 3 * FRAME + 5] {
            let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let sealed = seal_whole(&k, &image(), &plain).unwrap();
            assert!(sealed.starts_with(MAGIC));
            assert_eq!(
                open_whole(&k, &sealed).unwrap(),
                (plain, Some(image())),
                "{len}"
            );
            assert!(open_whole(&key(8), &sealed).is_err(), "another key");
            let mut flipped = sealed.clone();
            let at = flipped.len() / 2;
            flipped[at] ^= 1;
            assert!(open_whole(&k, &flipped).is_err(), "a byte changed");
            // Cut short at every frame's end but the last, and inside one.
            assert!(open_whole(&k, &sealed[..sealed.len() - 1]).is_err());
            if len > FRAME {
                let first = START + 4 + HEAD + FRAME + TAG;
                assert!(open_whole(&k, &sealed[..first]).is_err(), "a frame dropped");
            }
        }
    }

    /// Frames swapped, repeated or one dropped from the middle: each is at
    /// a place its tag does not cover, after a frame whose tag it does not.
    #[test]
    fn frames_out_of_place_do_not_open() {
        let k = key(1);
        let plain = vec![9u8; 3 * FRAME + 10];
        let sealed = seal_whole(&k, &segment(2), &plain).unwrap();
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
    /// its own place: it was sealed under the other file's key, after
    /// another tag, and does not open -- whole or appended. In version 1
    /// it did.
    #[test]
    fn a_frame_from_another_file_does_not_open() {
        let k = key(2);
        let (a, b) = (vec![1u8; 3 * FRAME], vec![2u8; 3 * FRAME]);
        let (sa, sb) = (
            seal_whole(&k, &segment(2), &a).unwrap(),
            seal_whole(&k, &segment(2), &b).unwrap(),
        );
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
        let ((ga, ..), (gb, ..)) = (appended(&k, 4), appended(&k, 4));
        let frame = HEAD + 40 + TAG;
        let mut taken = ga.clone();
        taken[START + 2 * frame..START + 3 * frame]
            .copy_from_slice(&gb[START + 2 * frame..START + 3 * frame]);
        let e = open_appended(&k, &taken).err().unwrap().to_string();
        assert!(e.contains("frame 2 does not open"), "{e}");
        // The same taking between two version 1 files opens: their frames
        // are bound to their places alone.
        let v1 = |n: u8| {
            let mut out = MAGIC_V1.to_vec();
            let mut frames = Frames::v1(&k);
            for _ in 0..4 {
                frames.seal(false, &[n; 40], &mut out).unwrap();
            }
            out
        };
        let (va, vb) = (v1(1), v1(2));
        let mut taken = va.clone();
        let m = MAGIC_V1.len();
        taken[m + 2 * frame..m + 3 * frame].copy_from_slice(&vb[m + 2 * frame..m + 3 * frame]);
        assert_eq!(open_appended(&k, &taken).unwrap().plain[80..120], [2; 40]);
    }

    /// The finding of 1.0's review: a segment's last frames cut, the
    /// archiver going on from the frame before them seals new frames at
    /// the places the cut ones had. Each frame's tag covers the tag before
    /// it, so an old frame 2 then a new frame 3 does not open -- it did,
    /// in version 2, dropping or doubling writes in a segment's middle --
    /// and only a run of whole frames from the start does: the old ones,
    /// or the new.
    #[test]
    fn frames_sealed_again_after_a_cut_do_not_splice_with_those_cut() {
        let k = key(10);
        let (file, ends, _) = appended(&k, 4);
        let reopened = open_appended(&k, &file[..ends[1]]).unwrap();
        assert_eq!((reopened.end, reopened.next.index()), (ends[1], 2));
        let mut again = file[..reopened.end].to_vec();
        let mut next = reopened.next;
        next.seal(false, &[7; 30], &mut again).unwrap();
        let new3 = again.len();
        next.seal(false, &[8; 30], &mut again).unwrap();
        let spliced = [&file[..ends[2]], &again[new3..]].concat();
        let e = open_appended(&k, &spliced).err().unwrap().to_string();
        assert!(e.contains("frame 3 does not open"), "{e}");
        let new_then_old = [&again[..new3], &file[ends[2]..]].concat();
        assert!(open_appended(&k, &new_then_old).is_err());
        // Either whole run opens: the one cut, or the one gone on with.
        assert_eq!(open_appended(&k, &file).unwrap().plain.len(), 160);
        assert_eq!(open_appended(&k, &again).unwrap().plain.len(), 80 + 60);
    }

    /// What the head says the file is, under the first frame's tag: one
    /// changed byte of it, anywhere, opens nothing, and read back it is
    /// what was sealed. An id cut short or changed opens nothing; a head
    /// is held to its version, and a later one is refused by its number.
    #[test]
    fn the_head_is_the_first_frames_and_a_version_is_named() {
        let k = key(6);
        let sealed = seal_whole(&k, &image(), &vec![3u8; FRAME + 7]).unwrap();
        let start = START + "acme".len();
        assert_eq!(bound_in(&sealed), Some(image()));
        for byte in MAGIC.len()..start {
            let mut changed = sealed.clone();
            changed[byte] ^= 0x80;
            assert!(open_whole(&k, &changed).is_err(), "byte {byte} of the head");
        }
        for cut in [MAGIC.len(), MAGIC.len() + 5, MAGIC.len() + ID, start - 1] {
            assert!(cut_in_head(&sealed[..cut]), "{cut}");
            assert!(open_whole(&k, &sealed[..cut]).is_err());
            assert!(open_appended(&k, &sealed[..cut]).is_err());
            assert_eq!(bound_in(&sealed[..cut]), None);
        }
        // Cut inside the magic: a start of a head. Whole, a head is no cut.
        assert!(cut_in_head(&sealed[..3]) && cut_in_head(&[]));
        assert!(!cut_in_head(&sealed[..start]));
        assert!(!cut_in_head(MAGIC_V1), "version 1's magic is whole");
        assert!(cut_in_head(&MAGIC_V2[..]) && !cut_in_head(&[&MAGIC_V2[..], &[0; ID]].concat()));
        // The binding dropped and the magic made version 2's, or the id too
        // and version 1's: the tags were made under the file's own key,
        // after its head, and open as neither.
        let mut v2 = MAGIC_V2.to_vec();
        v2.extend_from_slice(&sealed[MAGIC.len()..MAGIC.len() + ID]);
        v2.extend_from_slice(&sealed[start..]);
        assert!(open_whole(&k, &v2).is_err());
        let mut v1 = MAGIC_V1.to_vec();
        v1.extend_from_slice(&sealed[start..]);
        assert!(open_whole(&k, &v1).is_err());
        // A later version is refused by its number, never read as plain.
        let mut later = sealed.clone();
        later[NAME.len()] = 4;
        assert!(is_sealed(&later));
        let e = open_whole(&k, &later).unwrap_err().to_string();
        assert!(e.contains("version 4") && e.contains("1 to 3"), "{e}");
        // A role this binary does not know is named, never read as one.
        let mut unknown = sealed.clone();
        unknown[MAGIC.len() + ID] = 9;
        let e = open_whole(&k, &unknown).unwrap_err().to_string();
        assert!(e.contains("does not know (9)"), "{e}");
        // A name longer than a head holds is refused, nothing appended.
        let mut long = image();
        long.tenant = "t".repeat(TENANT + 1);
        let mut out = vec![1];
        assert!(begin(&k, &long, &mut out).is_err());
        assert_eq!(out, [1]);
    }

    /// Each file seals under its own key, HChaCha20 of its id under the key
    /// HKDF-Expand makes of the key ([`FILE_KEYS`]): a frame opens under
    /// that, never under the key itself, so a nonce two files' frames happen
    /// to share gives nothing away, nor under HChaCha20 of the key and the
    /// id, which an older file's keystream gave away.
    #[test]
    fn each_file_seals_under_its_own_key() {
        let k = key(11);
        let (file, ends, _) = appended(&k, 1);
        let id: [u8; ID] = file[MAGIC.len()..MAGIC.len() + ID].try_into().unwrap();
        let frame = &file[START..ends[0]];
        let nonce: [u8; 12] = frame[4..HEAD].try_into().unwrap();
        let (body, tag) = frame[HEAD..].split_at(40);
        let mut aad = file[..START].to_vec();
        aad.extend_from_slice(&0u64.to_le_bytes());
        aad.push(0);
        let open = |key: &[u8; 32]| {
            let mut b = body.to_vec();
            crypto::open_in_place(key, &nonce, &aad, &mut b, tag).then_some(b)
        };
        let files = crypto::hmac_sha256(&k.raw, &[FILE_KEYS, &[1]].concat());
        assert_eq!(open(&crypto::hchacha20(&files, &id)), Some(vec![0; 40]));
        assert_eq!(open(&k.raw), None, "the key itself");
        assert_eq!(
            open(&crypto::hchacha20(&k.raw, &id)),
            None,
            "HChaCha20 of the key itself"
        );
    }

    /// A file's key as another implementation makes it, so a change of the
    /// derivation fails here first: computed apart from this code, in
    /// Python -- HMAC-SHA256 and HKDF from its standard library, HChaCha20
    /// written from draft-irtf-cfrg-xchacha 2.2 and checked against its
    /// 2.2.1 vector. The key 00..1f, the id f0..ff.
    #[test]
    fn a_file_key_is_the_one_another_reader_makes() {
        let k = Key::of(std::array::from_fn(|i| i as u8));
        let id: [u8; ID] = std::array::from_fn(|i| 0xf0 + i as u8);
        assert_eq!(
            k.files[..],
            hex("b334afb4e085eabd2c6cd511a3ee509a414695c8e2fd8d1f50845a65eb2e47da")[..]
        );
        assert_eq!(
            file_key(&k, &id)[..],
            hex("862970b95cd16be8c85c3e3f58f648a77eafd77a3a3a96a3aa0de9fdf0421c09")[..]
        );
        // What version 3 first made, the key itself under HChaCha20.
        assert_eq!(
            crypto::hchacha20(&k.raw, &id)[..],
            hex("969e1d9115842722d5eae8d284f3b3df60f137195872dc2cfb786bf75a22054d")[..]
        );
    }

    /// The key files' keys are made under is HKDF-Expand's (RFC 5869, 2.3)
    /// first block, HMAC-SHA256 of the key and the info then 0x01, so a
    /// reader with an HKDF of its own makes it: RFC 5869's SHA-256 cases,
    /// A.1 to A.3, their PRK and info, and the first 32 bytes of each OKM.
    #[test]
    fn the_file_keys_key_is_hkdf_expand() {
        let info_a2: Vec<u8> = (0xb0..=0xff).collect();
        let cases: [(&str, Vec<u8>, &str); 3] = [
            (
                "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5",
                (0xf0..=0xf9).collect(),
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf",
            ),
            (
                "06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244",
                info_a2,
                "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c",
            ),
            (
                "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04",
                Vec::new(),
                "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d",
            ),
        ];
        for (prk, info, okm) in cases {
            let t1 = crypto::HmacKey::new(&hex(prk)).mac(&[&info, &[1]]);
            assert_eq!(t1[..], hex(okm)[..], "{prk}");
        }
    }

    /// The finding that held version 3 back: a file's key was HChaCha20 of
    /// the key and the file's id, the id read from the file, while the key
    /// itself seals version 1 and 2 frames with ChaCha20. A ChaCha20 block
    /// at counter c and nonce N is HChaCha20's rounds over the same state
    /// with that state added back, so a block's keystream less the
    /// constants (words 0-3) and less (c, N) (words 12-15) is HChaCha20 of
    /// the key and c || N: 32 bytes of plaintext known, bytes 0-15 and
    /// 48-63 of a block, and anyone holding an older file had the key of a
    /// version 3 file of id c || N, and sealed one -- any role, archive,
    /// tenant and contents -- that opened. The same bytes now give nothing.
    #[test]
    fn an_older_files_keystream_gives_no_file_key() {
        let k = key(14);
        // Frame 0 of a version 2 file, 64 bytes the forger knows: block 1
        // of its keystream is the ciphertext xored with them.
        let known: Vec<u8> = (0..64).map(|i| i as u8 ^ 0x5c).collect();
        let mut v2 = [&MAGIC_V2[..], &[3; ID]].concat();
        Frames::v2(&k, [3; ID])
            .seal(false, &known, &mut v2)
            .unwrap();
        let frame = &v2[MAGIC.len() + ID..];
        let stream: Vec<u8> = frame[HEAD..HEAD + 64]
            .iter()
            .zip(&known)
            .map(|(c, p)| c ^ p)
            .collect();
        let mut input = [0u8; 16];
        input[..4].copy_from_slice(&1u32.to_le_bytes());
        input[4..].copy_from_slice(&frame[4..HEAD]);
        let word = |b: &[u8], i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        let sigma = [0x61707865u32, 0x3320646e, 0x79622d32, 0x6b206574];
        let mut had = [0u8; 32];
        for i in 0..4 {
            let first = word(&stream, i).wrapping_sub(sigma[i]);
            let last = word(&stream, 12 + i).wrapping_sub(word(&input, i));
            had[i * 4..i * 4 + 4].copy_from_slice(&first.to_le_bytes());
            had[16 + i * 4..16 + i * 4 + 4].copy_from_slice(&last.to_le_bytes());
        }
        // The arithmetic holds: it is HChaCha20 of the key and c || N.
        assert_eq!(had, crypto::hchacha20(&k.raw, &input));
        // A version 3 file of id c || N, sealed whole under it: an image
        // of tenant xeno's at change 99, holding what the forger chose.
        let mut forged = MAGIC.to_vec();
        forged.extend_from_slice(&input);
        let mut bound = image();
        bound.tenant = "xeno".into();
        bound.change = 99;
        bound.put(&mut forged).unwrap();
        let mut frames = Frames {
            key: had,
            kind: Kind::V3(Chain::Head(forged.clone())),
            index: 0,
        };
        frames
            .seal(true, b"rows of the forger's", &mut forged)
            .unwrap();
        let opened = open_whole(&k, &forged);
        assert!(
            opened.is_err(),
            "a file sealed under an older file's keystream opened: {opened:?}"
        );
    }

    /// A head read before its file is opened is vouched for by its first
    /// frame alone: what it says once that frame opens, from the file's
    /// start or the whole of it; refused where a byte of it changed or the
    /// key is another; nothing where it or the frame is cut short, or the
    /// file is of an older version or plain.
    #[test]
    fn a_head_is_vouched_for_by_its_first_frame() {
        let k = key(8);
        let sealed = seal_whole_by(&k, &image(), &[5u8; 300], 100).unwrap();
        let start = START + "acme".len();
        let first = &sealed[..start + HEAD + 100 + TAG];
        assert_eq!(vouched_in(&k, first).unwrap(), Some(image()));
        assert_eq!(vouched_in(&k, &sealed).unwrap(), Some(image()));
        for byte in 0..start {
            let mut changed = first.to_vec();
            changed[byte] ^= 0x80;
            match vouched_in(&k, &changed) {
                // The name's length grown: its first frame read from
                // further on, past the end of what was read.
                Ok(None) if byte == START - 1 => {}
                // The magic changed: no file of this version.
                Ok(None) if byte < MAGIC.len() => {}
                Ok(b) => panic!("byte {byte} of the head changed, vouched for: {b:?}"),
                Err(_) => {}
            }
        }
        assert!(vouched_in(&key(9), first).is_err(), "another key");
        assert_eq!(vouched_in(&k, &sealed[..start + HEAD + 10]).unwrap(), None);
        assert_eq!(vouched_in(&k, &sealed[..start - 1]).unwrap(), None);
        let mut old = [&MAGIC_V2[..], &[1; ID]].concat();
        let mut v2 = Frames {
            key: k.raw,
            kind: Kind::V2([1; ID]),
            index: 0,
        };
        v2.seal(true, b"x", &mut old).unwrap();
        assert_eq!(vouched_in(&k, &old).unwrap(), None);
        assert_eq!(vouched_in(&k, b"plain bytes").unwrap(), None);
    }

    /// A frame holds a mebibyte at most, and its length is written as it
    /// is: one byte past refused, nothing appended. Driven at a frame's
    /// edge with a smaller one, a whole file goes out in as many frames as
    /// it takes, each opened back to the bytes.
    #[test]
    fn a_frame_holds_a_mebibyte_at_most() {
        let k = key(12);
        let mut out = Vec::new();
        let mut frames = begin(&k, &segment(2), &mut out).unwrap();
        let head = out.len();
        let e = frames
            .seal(false, &vec![0; FRAME + 1], &mut out)
            .unwrap_err();
        assert!(e.to_string().contains("past the 1048576"), "{e}");
        assert_eq!((out.len(), frames.index()), (head, 0));
        frames.seal(false, &vec![1; FRAME], &mut out).unwrap();
        assert_eq!(&out[head..head + 4], &(FRAME as u32).to_le_bytes());
        for (len, n) in [
            (0, 1),
            (1, 1),
            (63, 1),
            (64, 1),
            (65, 2),
            (128, 2),
            (129, 3),
        ] {
            let plain: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let sealed = seal_whole_by(&k, &segment(2), &plain, 64).unwrap();
            assert_eq!(sealed.len(), START + n * (HEAD + TAG) + len, "{len}");
            assert_eq!(open_whole(&k, &sealed).unwrap().0, plain);
        }
    }

    /// A version 1 file, as 0.2.0 sealed one, and a version 2 file, as
    /// main sealed one before 1.0: each opens whole and appended, saying
    /// what it is not, and goes on in its own version.
    #[test]
    fn older_versions_open() {
        let k = key(9);
        let mut whole = MAGIC_V1.to_vec();
        let mut frames = Frames::v1(&k);
        frames.seal(false, &[4; FRAME], &mut whole).unwrap();
        frames.seal(true, b"end", &mut whole).unwrap();
        let (plain, bound) = open_whole(&k, &whole).unwrap();
        assert_eq!(
            (plain.len(), &plain[FRAME..], bound),
            (FRAME + 3, &b"end"[..], None)
        );
        // Version 2: the id after the magic, every tag over it, the frames
        // under the key itself -- sealed here as main's code sealed them.
        let id = [5u8; ID];
        let mut v2 = MAGIC_V2.to_vec();
        v2.extend_from_slice(&id);
        let mut frames = Frames {
            key: k.raw,
            kind: Kind::V2(id),
            index: 0,
        };
        frames.seal(false, b"one", &mut v2).unwrap();
        let o = open_appended(&k, &v2).unwrap();
        assert_eq!(
            (&o.plain[..], o.end, o.next.version(), o.binding),
            (&b"one"[..], v2.len(), 2, None)
        );
        let mut next = o.next;
        next.seal(false, b"two", &mut v2).unwrap();
        assert_eq!(open_appended(&k, &v2).unwrap().plain, b"onetwo");
    }

    #[test]
    fn an_appended_file_loses_only_a_frame_cut_short() {
        let k = key(3);
        let (mut file, ends, mut frames) = appended(&k, 5);
        let whole = file.len();
        frames.seal(false, &[5; 40], &mut file).unwrap();
        let cut = &file[..file.len() - 3];
        let o = open_appended(&k, cut).unwrap();
        assert_eq!((o.plain.len(), o.end, o.next.index()), (200, whole, 5));
        // Zeros to the end, as an append whose length reached the disk
        // before its bytes leaves them, are a frame cut short; zeros with a
        // byte after them are a frame that does not open.
        let mut zeros = file[..whole].to_vec();
        zeros.resize(whole + 4096, 0);
        let o = open_appended(&k, &zeros).unwrap();
        assert_eq!((o.plain.len(), o.end, o.next.index()), (200, whole, 5));
        zeros.push(1);
        assert!(open_appended(&k, &zeros).is_err());
        // Sealed whole, a file is no appended one.
        assert!(open_appended(&k, &seal_whole(&k, &image(), b"x").unwrap()).is_err());
        // Opened to a frame's end: there and no further, the next frame
        // sealed after it; between two, refused.
        let o = open_appended_to(&k, &file, 120).unwrap();
        assert_eq!((o.plain.len(), o.end, o.next.index()), (120, ends[2], 3));
        let e = open_appended_to(&k, &file, 100).err().unwrap().to_string();
        assert!(e.contains("no frame ends at byte 100"), "{e}");
    }

    /// One bit changed, each bit of an appended file in turn. The head --
    /// the magic, the id and what the file is -- a frame's nonce, its
    /// sealed bytes and its tag are refused. A frame's length is under no
    /// tag: changed so the frame still ends inside the file, the bytes read
    /// as its tag are not it and the frame is refused; changed so the frame
    /// runs past the file's end, it is a frame cut short there, which
    /// nothing tells from the one a crash leaves, and the file opens to the
    /// frame before it -- what whoever can write the file does by cutting
    /// it. So with the magic's version set back to an older one, or the
    /// tenant's name made longer: the first frame is read from further on,
    /// its length past the end, and the file opens holding nothing, as one
    /// cut after its head does -- never a byte of what was sealed, and its
    /// head vouched for by no tag (a reader holds a file to its head only
    /// once a frame opened). The archive's sealed test changed its segment's middle byte and
    /// looked for a refusal, which the third or fourth byte of a length did
    /// not give it.
    #[test]
    fn a_bit_changed_is_refused_unless_a_length_then_runs_past_the_end() {
        let k = key(4);
        let mut whole = Vec::new();
        let mut frames = begin(&k, &segment(2), &mut whole).unwrap();
        let mut starts = Vec::new();
        for (i, len) in [8usize, 40, 0, 300, 17].into_iter().enumerate() {
            starts.push(whole.len());
            frames.seal(false, &vec![i as u8; len], &mut whole).unwrap();
        }
        let o = open_appended(&k, &whole).unwrap();
        assert_eq!((o.end, o.next.index()), (whole.len(), 5));
        let mut cut_short = 0;
        for at in 0..whole.len() {
            for bit in 0..8 {
                let mut changed = whole.clone();
                changed[at] ^= 1 << bit;
                let Ok(o) = open_appended(&k, &changed) else {
                    continue;
                };
                // Opened: as far as the frame the bit is in and no
                // further, the bit one of its length, the length now
                // past the end.
                let Some(f) = starts.iter().rposition(|&s| s <= at) else {
                    // The version set back to 2 or 1, or the name's length
                    // grown, moves where the first frame is read from, its
                    // length now past the end: a file of no frame, what
                    // cutting it after its head leaves, its head vouched for
                    // by no tag.
                    assert!(
                        o.plain.is_empty() && o.next.index() == 0,
                        "bit {bit} of byte {at}, in the head: the file opened"
                    );
                    assert!(at == NAME.len() || at == START - 1, "{at}");
                    continue;
                };
                assert!(
                    at < starts[f] + 4,
                    "bit {bit} of byte {at} is in no length, and the file opened"
                );
                let len: [u8; 4] = changed[starts[f]..starts[f] + 4].try_into().unwrap();
                assert!(
                    starts[f] + HEAD + u32::from_le_bytes(len) as usize + TAG > whole.len(),
                    "bit {bit} of byte {at}: a frame that ends inside the file opened"
                );
                assert_eq!(
                    (o.end, o.next.index()),
                    (starts[f], f as u64),
                    "bit {bit} of byte {at}"
                );
                cut_short += 1;
            }
        }
        // Every frame has such bits: the high ones of its length.
        assert!(cut_short >= 5 * 8, "{cut_short}");
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
        let mut out = Vec::new();
        let mut frames = begin(&k, &segment(2), &mut out).unwrap();
        crypto::without_randomness(|| {
            let before = out.clone();
            assert!(frames.seal(false, b"secret", &mut out).is_err());
            assert_eq!(out, before, "a refused seal appends nothing");
            assert_eq!(frames.index(), 0);
            let mut head = Vec::new();
            assert!(begin(&k, &segment(2), &mut head).is_err());
            assert!(head.is_empty(), "a refused id appends nothing");
            assert!(seal_whole(&k, &Binding::backup(), b"secret").is_err());
            assert!(seal_whole(&k, &Binding::backup(), b"").is_err());
            assert!(Key::generate().is_none());
        });
        assert!(seal_whole(&k, &Binding::backup(), b"secret").is_ok());
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
            let mut frames = begin(&k, &segment(2), &mut frame).unwrap();
            assert!(
                ids.insert(frame[MAGIC.len()..MAGIC.len() + ID].to_vec()),
                "file {i}'s id came again"
            );
            frames.seal(false, b"", &mut frame).unwrap();
            assert!(
                seen.insert(nonce_of(&frame[START..])),
                "frame {i}'s nonce came again"
            );
        }
        let sealed = seal_whole(&k, &segment(2), &vec![1u8; 3 * FRAME]).unwrap();
        let frame = HEAD + FRAME + TAG;
        let nonces: std::collections::HashSet<_> = (0..3)
            .map(|i| nonce_of(&sealed[START + i * frame..]))
            .collect();
        assert_eq!(nonces.len(), 3);
    }

    /// What a seal costs, version 2's way -- the key itself, the id in each
    /// frame's AAD -- against version 3's -- a file's own key made as it
    /// begins, the tag before in each frame's AAD: frames of 1 KiB and of
    /// 1 MiB, a thousand a file, and frames of 64 bytes a file each, the
    /// best of rounds taken in turn in one process, as a ratio. Run by
    /// hand: `cargo test --release -p fennec-http --lib seal_cost --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn seal_cost() {
        let k = key(13);
        for (size, frames, per_file) in [
            (1 << 10, 10_000, 1_000),
            (1 << 20, 10_000, 1_000),
            (64, 10_000, 1),
        ] {
            let plain = vec![7u8; size];
            let mut out = Vec::with_capacity(size + 64 + HEAD_MAX);
            let mut best = [f64::MAX; 2];
            for _ in 0..3 {
                for (v, b) in best.iter_mut().enumerate() {
                    let t = std::time::Instant::now();
                    let mut f = None;
                    for i in 0..frames {
                        out.clear();
                        if i % per_file == 0 {
                            f = Some(match v {
                                0 => Frames {
                                    key: k.raw,
                                    kind: Kind::V2([i as u8; ID]),
                                    index: 0,
                                },
                                _ => begin(&k, &segment(2), &mut out).unwrap(),
                            });
                        }
                        f.as_mut().unwrap().seal(false, &plain, &mut out).unwrap();
                    }
                    std::hint::black_box(&out);
                    *b = b.min(t.elapsed().as_secs_f64());
                }
            }
            eprintln!(
                "{frames} frames of {size} bytes, {per_file} a file: version 2 {:.4} s, \
                 version 3 {:.4} s, 3/2 {:.4}",
                best[0],
                best[1],
                best[1] / best[0]
            );
        }
    }

    /// A key file another user, or its group, can read is warned of by
    /// name and mode, as `--tls-key` is; one its owner alone reads is not.
    /// The words alone: that `Key::from_file` logs them, on the way every
    /// `--key-file` takes, is held through the command (fennec-cli's
    /// `tokens.rs`).
    #[cfg(unix)]
    #[test]
    fn a_key_file_others_can_read_is_warned_of() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "fennec-seal-key-{}-{}",
            std::process::id(),
            crypto::id_u64()
        ));
        std::fs::write(&path, "ab".repeat(32)).unwrap();
        let mode = |m: u32| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m));
        mode(0o600).unwrap();
        assert_eq!(open_key(&path), None);
        assert!(Key::from_file(&path).is_ok());
        mode(0o644).unwrap();
        let w = open_key(&path).unwrap();
        assert!(w.contains("is open to every user (mode 0644)"), "{w}");
        assert!(w.contains(&path.display().to_string()), "{w}");
        assert!(Key::from_file(&path).is_ok(), "warned, never refused");
        let _ = std::fs::remove_file(&path);
    }
}
