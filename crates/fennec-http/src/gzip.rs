//! Answers in gzip (RFC 1952) over a deflate of our own (RFC 1951), where
//! the request's `Accept-Encoding` takes it ([`accepts`]), its client is
//! not on the server's host or private network ([`local`]) and the answer
//! is one worth it ([`encode`]): JSON, NDJSON or text, past `--http-gzip`'s
//! bytes, carrying no tag. A subscription's stream is one gzip member
//! ([`Stream`]), each event deflated alone and ended on a byte boundary, so
//! its client reads it whole as it comes and the stream holds nothing
//! between events but a CRC and a count
//! (docs/design/http.md#compressed-answers).
//!
//! The compressor is fennecdb's own, as every crate under the server is
//! (zero dependencies): LZ77 over 4-byte hash chains, greedy with a short
//! chain, and a dynamic Huffman block a 16 384 symbols, written stored where
//! that is smaller. Its tables are sized to the input and let go of when it
//! returns: nothing of it is kept by a connection or a stream.

use crate::http::{Request, Response};
use std::net::IpAddr;

/// `--http-gzip`'s default: an answer this long or longer is compressed
/// for a client that takes gzip. Under it an answer and its head are one
/// TCP segment either way (1 460 bytes of payload at Ethernet's MTU), so
/// it saves no packet, and its compression's fixed cost -- some 5 us,
/// the tables and the block's codes -- weighs most against what it saves
/// (docs/design/http.md#compressed-answers).
pub const MIN_BYTES: usize = 1400;

// ------------------------------------------------------------- the choice

/// Whether `Accept-Encoding` takes gzip: `gzip` or `x-gzip` (RFC 9110
/// 8.4.1.3) with a weight above zero, or `*` so where gzip is not named.
/// Every line of the header is one list.
pub fn accepts(req: &Request) -> bool {
    let mut named = None;
    let mut star = None;
    for (k, v) in &req.headers {
        if !k.eq_ignore_ascii_case("accept-encoding") {
            continue;
        }
        for item in v.split(',') {
            let mut parts = item.split(';');
            let coding = parts.next().unwrap_or("").trim();
            let weight = parts
                .filter_map(|p| p.trim().split_once('='))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("q"))
                .is_none_or(|(_, q)| above_zero(q.trim()));
            if coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip") {
                named = Some(named.unwrap_or(false) || weight);
            } else if coding == "*" {
                star = Some(weight);
            }
        }
    }
    named.or(star).unwrap_or(false)
}

/// A weight (`qvalue`, RFC 9110 12.4.2) above zero: anything but `0`,
/// `0.`, `0.0`, `0.00` and `0.000`. One that does not parse is taken as
/// given, as no weight at all is.
fn above_zero(q: &str) -> bool {
    !(q.starts_with('0') && q.bytes().all(|b| b == b'0' || b == b'.'))
}

/// Whether an answer of this type is worth compressing: text, which JSON
/// and NDJSON are. An archive or a backup (`application/octet-stream`) is
/// a file's pages, sent server to server or to `fennec backup`, and
/// never is.
fn compressible(content_type: &str) -> bool {
    content_type.starts_with("application/json")
        || content_type.starts_with("application/x-ndjson")
        || content_type.starts_with("text/")
        || content_type.starts_with("application/javascript")
}

/// Whether a client at `ip` is on this host or a private network:
/// loopback, RFC 1918, link-local or IPv6's unique local addresses. Its
/// link is the host's own or a LAN's, 1 Gbit and up, where compressing
/// broke even at 1 Gbit and took twice as long on loopback as sending the
/// bytes as they are (docs/design/http.md#compressed-answers). Not CGNAT's
/// 100.64/10, which a carrier or a VPN over the internet gives out.
pub fn local(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Whether `req`'s answer goes as gzip, where it is worth it: its
/// `Accept-Encoding` takes gzip, and its client -- the one a router
/// forwarded it for or the global entry named, or the connection's peer
/// ([`crate::audit::client_ip`]) -- is not [`local`], unless
/// `--http-gzip-local` says to compress for those too.
pub fn wanted(req: &Request, cfg: &crate::Config) -> bool {
    accepts(req) && (cfg.gzip_local || !crate::audit::client_ip().is_some_and(local))
}

/// `resp` as `req` takes it, where `--http-gzip` is on: an answer of its
/// bytes or more, of a type that compresses, says `Vary: Accept-Encoding`,
/// and goes as gzip where [`wanted`] and the compressed body is the
/// shorter. Never an answer carrying a tag -- a row's `ETag` and its
/// `no-transform`, a read's poll, a studio file's -- which names one body,
/// compared strongly (RFC 9110 8.8.3: a tag is the representation's, its
/// coding with it), nor a `HEAD`, whose length is the identity's. An answer
/// under the bytes costs one compare.
pub fn encode(resp: &mut Response, req: &Request, cfg: &crate::Config) {
    let Some(min) = cfg.gzip else {
        return;
    };
    if resp.body.len() < min {
        return;
    }
    encode_cold(resp, req, cfg);
}

#[cold]
#[inline(never)]
fn encode_cold(resp: &mut Response, req: &Request, cfg: &crate::Config) {
    if req.method == crate::http::Method::Head
        || resp.no_transform
        || !compressible(resp.content_type)
        || resp.extra.iter().any(|(k, _)| {
            k.eq_ignore_ascii_case("etag")
                || k.eq_ignore_ascii_case("content-encoding")
                || k.eq_ignore_ascii_case("content-range")
        })
    {
        return;
    }
    resp.extra.push(("Vary".into(), "Accept-Encoding".into()));
    if !wanted(req, cfg) {
        return;
    }
    let mut out = Vec::with_capacity(resp.body.len() / 4 + 64);
    compress(&resp.body, &mut out);
    if out.len() < resp.body.len() {
        resp.body = out;
        resp.extra.push(("Content-Encoding".into(), "gzip".into()));
    }
}

// ------------------------------------------------------------------- gzip

/// A member's header: no name, no time (0: none given), the fast setting
/// (XFL 4) and no system named (255).
const HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 4, 255];

/// `input` as one gzip member, appended to `out`.
pub fn compress(input: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&HEADER);
    let mut bits = Bits::new(out);
    deflate(input, &mut bits, true);
    bits.align();
    out.extend_from_slice(&crc32(0, input).to_le_bytes());
    out.extend_from_slice(&(input.len() as u32).to_le_bytes());
}

/// One gzip member sent a piece at a time, each piece readable whole on its
/// own arrival: a subscription's events. Each is deflated alone -- no
/// window carried from one to the next, so a stream holds its CRC and its
/// count and nothing else between events -- and ends with the empty stored
/// block zlib's `Z_SYNC_FLUSH` writes, after a stored block too: Go's
/// inflater hands a stored block's bytes on only at the next flush, and a
/// change sent stored alone waited in it for the event after.
pub struct Stream {
    crc: u32,
    size: u32,
}

impl Stream {
    /// A stream whose member's header goes at the end of `head`, sent with
    /// the answer's head.
    pub fn start(head: &mut Vec<u8>) -> Stream {
        head.extend_from_slice(&HEADER);
        Stream { crc: 0, size: 0 }
    }

    /// `data` deflated onto `out`: compressed where it is `min` bytes or
    /// more, stored blocks otherwise, then the sync mark.
    pub fn piece(&mut self, data: &[u8], min: usize, out: &mut Vec<u8>) {
        self.crc = crc32(self.crc, data);
        self.size = self.size.wrapping_add(data.len() as u32);
        let mut bits = Bits::new(out);
        if data.len() >= min {
            deflate(data, &mut bits, false);
        } else if !data.is_empty() {
            stored(&mut bits, data, false);
        }
        stored(&mut bits, &[], false);
    }

    /// The member's end: an empty last block, its CRC and its length.
    pub fn finish(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[0x03, 0x00]);
        out.extend_from_slice(&self.crc.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
    }
}

// ----------------------------------------------------------------- CRC-32

const fn crc_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut s = 1;
    while s < 8 {
        let mut i = 0;
        while i < 256 {
            let p = t[s - 1][i];
            t[s][i] = (p >> 8) ^ t[0][(p & 0xff) as usize];
            i += 1;
        }
        s += 1;
    }
    t
}

/// Slice-by-8's tables, made at compile time.
static CRC: [[u32; 256]; 8] = crc_tables();

/// gzip's CRC-32 (ISO 3309, reflected `0xEDB88320`) of `data`, carried on
/// from `crc`: slice-by-8, eight bytes a step.
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    let (words, rest) = data.as_chunks::<8>();
    for w in words {
        let a = u32::from_le_bytes([w[0], w[1], w[2], w[3]]) ^ c;
        let b = u32::from_le_bytes([w[4], w[5], w[6], w[7]]);
        c = CRC[7][(a & 0xff) as usize]
            ^ CRC[6][((a >> 8) & 0xff) as usize]
            ^ CRC[5][((a >> 16) & 0xff) as usize]
            ^ CRC[4][(a >> 24) as usize]
            ^ CRC[3][(b & 0xff) as usize]
            ^ CRC[2][((b >> 8) & 0xff) as usize]
            ^ CRC[1][((b >> 16) & 0xff) as usize]
            ^ CRC[0][(b >> 24) as usize];
    }
    for &x in rest {
        c = CRC[0][((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

// ------------------------------------------------------------------- bits

/// Bits out, least significant first, as deflate packs them.
struct Bits<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(out: &'a mut Vec<u8>) -> Bits<'a> {
        Bits { out, acc: 0, n: 0 }
    }

    /// `len` bits of `v`, at most 32.
    #[inline(always)]
    fn put(&mut self, v: u32, len: u32) {
        debug_assert!(len <= 32 && (len == 32 || v >> len == 0));
        self.acc |= (v as u64) << self.n;
        self.n += len;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// Out to the next byte, zeros after.
    fn align(&mut self) {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.acc = 0;
    }
}

// ----------------------------------------------------------------- tables

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The code-length alphabet's lengths go in this order (RFC 1951 3.2.7).
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// A length less 3 to its code less 257. 258 is code 285 alone: 284's
/// extra bits reach 257 at most.
const LEN_CODE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut code = 0;
    while code < 29 {
        let base = LEN_BASE[code] as usize - 3;
        let span = 1usize << LEN_EXTRA[code];
        let mut j = 0;
        while j < span && base + j < 256 {
            t[base + j] = code as u8;
            j += 1;
        }
        code += 1;
    }
    t
};

/// A distance less 1 to its code: under 256 by itself, past it by its
/// 128s (zlib's `_dist_code`), every code from 16 on spanning whole 128s.
const DIST_CODE: [u8; 512] = {
    let mut t = [0u8; 512];
    let mut code = 0;
    while code < 30 {
        let base = DIST_BASE[code] as usize - 1;
        let span = 1usize << DIST_EXTRA[code];
        let mut j = 0;
        while j < span {
            let d = base + j;
            if d < 256 {
                t[d] = code as u8;
            } else {
                t[256 + (d >> 7)] = code as u8;
            }
            j += 1;
        }
        code += 1;
    }
    t
};

#[inline(always)]
fn dist_code(d1: usize) -> usize {
    if d1 < 256 {
        DIST_CODE[d1] as usize
    } else {
        DIST_CODE[256 + (d1 >> 7)] as usize
    }
}

/// The fixed code's lengths (RFC 1951 3.2.6).
const FIXED_LIT: [u8; 288] = {
    let mut t = [0u8; 288];
    let mut i = 0;
    while i < 288 {
        t[i] = if i < 144 {
            8
        } else if i < 256 {
            9
        } else if i < 280 {
            7
        } else {
            8
        };
        i += 1;
    }
    t
};

// ---------------------------------------------------------------- Huffman

/// Code lengths for `freq` into `lens`, none past `max`, the code complete
/// and at least two symbols in it (zlib's rule: a decoder may refuse a
/// code of one). Moffat and Katajainen's in-place lengths over the used
/// symbols sorted by count, then the longest folded down to `max` until
/// Kraft's sum is one again (miniz's `enforce_max_code_size`).
fn lengths(freq: &[u32], max: u32, lens: &mut [u8]) {
    let n = freq.len();
    lens[..n].fill(0);
    // (count, symbol): the count is overwritten with parents, then depths.
    let mut a = [(0u32, 0u16); 288];
    let mut used = 0;
    for (s, &f) in freq.iter().enumerate() {
        if f > 0 {
            a[used] = (f, s as u16);
            used += 1;
        }
    }
    if used < 2 {
        let only = if used == 1 { a[0].1 as usize } else { 0 };
        lens[only] = 1;
        lens[if only == 0 { 1 } else { 0 }] = 1;
        return;
    }
    let a = &mut a[..used];
    a.sort_unstable();
    // Moffat-Katajainen: weights of internal nodes in place, then each
    // node's parent, then depths.
    a[0].0 += a[1].0;
    let (mut root, mut leaf) = (0usize, 2usize);
    for next in 1..used - 1 {
        if leaf >= used || a[root].0 < a[leaf].0 {
            a[next].0 = a[root].0;
            a[root].0 = next as u32;
            root += 1;
        } else {
            a[next].0 = a[leaf].0;
            leaf += 1;
        }
        if leaf >= used || (root < next && a[root].0 < a[leaf].0) {
            a[next].0 += a[root].0;
            a[root].0 = next as u32;
            root += 1;
        } else {
            a[next].0 += a[leaf].0;
            leaf += 1;
        }
    }
    a[used - 2].0 = 0;
    for next in (0..used.saturating_sub(2)).rev() {
        a[next].0 = a[a[next].0 as usize].0 + 1;
    }
    let (mut avail, mut taken, mut depth) = (1i64, 0i64, 0u32);
    let mut root = used as i64 - 2;
    let mut next = used as i64 - 1;
    while avail > 0 {
        while root >= 0 && a[root as usize].0 == depth {
            taken += 1;
            root -= 1;
        }
        while avail > taken {
            a[next as usize].0 = depth;
            next -= 1;
            avail -= 1;
        }
        avail = 2 * taken;
        depth += 1;
        taken = 0;
    }
    // How many codes of each length, the longest folded down to `max`.
    let mut count = [0u32; 64];
    for &(d, _) in a.iter() {
        count[(d as usize).min(63)] += 1;
    }
    let max = max as usize;
    for l in max + 1..64 {
        count[max] += count[l];
        count[l] = 0;
    }
    let mut total: u64 = (1..=max).map(|l| (count[l] as u64) << (max - l)).sum();
    while total != 1u64 << max {
        count[max] -= 1;
        for l in (1..max).rev() {
            if count[l] != 0 {
                count[l] -= 1;
                count[l + 1] += 2;
                break;
            }
        }
        total -= 1;
    }
    // The shortest codes to the most frequent: `a` is sorted by count
    // ascending.
    let mut j = used;
    for (l, &c) in count.iter().enumerate().take(max + 1).skip(1) {
        for _ in 0..c {
            j -= 1;
            lens[a[j].1 as usize] = l as u8;
        }
    }
}

/// Canonical codes for `lens` (RFC 1951 3.2.2), bit-reversed for writing
/// least significant first.
fn codes(lens: &[u8], codes: &mut [u16]) {
    let mut count = [0u16; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    for (s, &l) in lens.iter().enumerate() {
        if l != 0 {
            let c = next[l as usize];
            next[l as usize] += 1;
            codes[s] = c.reverse_bits() >> (16 - l as u32);
        } else {
            codes[s] = 0;
        }
    }
}

// ----------------------------------------------------------------- blocks

/// The symbols a block holds before it is written: zlib's default.
const BLOCK: usize = 16_384;
/// A symbol: a literal byte, or `MATCH | (length - 3) << 15 | (distance -
/// 1)`.
const MATCH: u32 = 1 << 31;

/// A block's symbols and their counts, the raw bytes they stand for kept
/// for a stored block where that is the shorter.
struct Symbols {
    syms: Vec<u32>,
    lit: [u32; 288],
    dist: [u32; 32],
}

impl Symbols {
    fn new(cap: usize) -> Symbols {
        Symbols {
            syms: Vec::with_capacity(cap),
            lit: [0; 288],
            dist: [0; 32],
        }
    }

    #[inline(always)]
    fn literal(&mut self, b: u8) {
        self.syms.push(b as u32);
        self.lit[b as usize] += 1;
    }

    #[inline(always)]
    fn matched(&mut self, len: usize, dist: usize) {
        self.syms
            .push(MATCH | ((len as u32 - 3) << 15) | (dist as u32 - 1));
        self.lit[257 + LEN_CODE[len - 3] as usize] += 1;
        self.dist[dist_code(dist - 1)] += 1;
    }

    fn clear(&mut self) {
        self.syms.clear();
        self.lit = [0; 288];
        self.dist = [0; 32];
    }
}

/// The bits `syms` take under these lengths, extra bits with them.
fn data_bits(s: &Symbols, lit: &[u8], dist: &[u8]) -> u64 {
    let mut bits = 0u64;
    for (&n, &l) in s.lit[..286].iter().zip(&lit[..286]) {
        bits += n as u64 * l as u64;
    }
    for (&n, &e) in s.lit[257..286].iter().zip(&LEN_EXTRA) {
        bits += n as u64 * e as u64;
    }
    for ((&n, &l), &e) in s.dist[..30].iter().zip(&dist[..30]).zip(&DIST_EXTRA) {
        bits += n as u64 * (l as u64 + e as u64);
    }
    bits
}

/// The code lengths of a dynamic block's two codes, run-length coded (RFC
/// 1951 3.2.7): `(symbol, extra bits' value)`.
fn runs(all: &[u8], out: &mut Vec<(u8, u8)>) {
    let mut i = 0;
    while i < all.len() {
        let l = all[i];
        let mut run = 1;
        while i + run < all.len() && all[i + run] == l {
            run += 1;
        }
        let mut r = run;
        if l == 0 {
            while r >= 11 {
                let k = r.min(138);
                out.push((18, (k - 11) as u8));
                r -= k;
            }
            if r >= 3 {
                out.push((17, (r - 3) as u8));
                r = 0;
            }
        } else {
            out.push((l, 0));
            r -= 1;
            while r >= 3 {
                let k = r.min(6);
                out.push((16, (k - 3) as u8));
                r -= k;
            }
        }
        for _ in 0..r {
            out.push((l, 0));
        }
        i += run;
    }
}

/// Writes `s`'s block, whichever of dynamic, fixed and stored is the
/// shortest; `raw` the bytes it stands for. Leaves `s` empty.
fn write_block(bits: &mut Bits, s: &mut Symbols, raw: &[u8], last: bool) {
    s.lit[256] = 1;
    let mut lit_len = [0u8; 288];
    let mut dist_len = [0u8; 32];
    lengths(&s.lit[..286], 15, &mut lit_len);
    lengths(&s.dist[..30], 15, &mut dist_len);
    let hlit = (257..=286)
        .rev()
        .find(|&i| lit_len[i - 1] != 0)
        .unwrap_or(257);
    let hdist = (1..=30).rev().find(|&i| dist_len[i - 1] != 0).unwrap_or(1);
    let mut all = [0u8; 316];
    all[..hlit].copy_from_slice(&lit_len[..hlit]);
    all[hlit..hlit + hdist].copy_from_slice(&dist_len[..hdist]);
    let mut rle = Vec::with_capacity(hlit + hdist);
    runs(&all[..hlit + hdist], &mut rle);
    let mut cl_freq = [0u32; 19];
    for &(c, _) in &rle {
        cl_freq[c as usize] += 1;
    }
    let mut cl_len = [0u8; 19];
    lengths(&cl_freq, 7, &mut cl_len);
    let hclen = (4..=19)
        .rev()
        .find(|&i| cl_len[CL_ORDER[i - 1]] != 0)
        .unwrap_or(4);
    let mut header = 14 + 3 * hclen as u64;
    for c in 0..19 {
        let extra = match c {
            16 => 2,
            17 => 3,
            18 => 7,
            _ => 0,
        };
        header += cl_freq[c] as u64 * (cl_len[c] as u64 + extra);
    }
    let dynamic = header + data_bits(s, &lit_len, &dist_len);
    let fixed = data_bits(s, &FIXED_LIT, &[5u8; 32]);
    let stored_bits = (raw.len() as u64 + 5 * raw.len().div_ceil(65_535).max(1) as u64) * 8;
    if stored_bits <= dynamic.min(fixed) {
        stored(bits, raw, last);
    } else if fixed <= dynamic {
        bits.put(last as u32 | 1 << 1, 3);
        let mut lc = [0u16; 288];
        let mut dc = [0u16; 32];
        codes(&FIXED_LIT, &mut lc);
        codes(&[5u8; 32], &mut dc);
        symbols(bits, &s.syms, &FIXED_LIT, &lc, &[5u8; 32], &dc);
    } else {
        bits.put(last as u32 | 2 << 1, 3);
        bits.put((hlit - 257) as u32, 5);
        bits.put((hdist - 1) as u32, 5);
        bits.put((hclen - 4) as u32, 4);
        for &c in &CL_ORDER[..hclen] {
            bits.put(cl_len[c] as u32, 3);
        }
        let mut clc = [0u16; 19];
        codes(&cl_len, &mut clc);
        for &(c, extra) in &rle {
            bits.put(clc[c as usize] as u32, cl_len[c as usize] as u32);
            match c {
                16 => bits.put(extra as u32, 2),
                17 => bits.put(extra as u32, 3),
                18 => bits.put(extra as u32, 7),
                _ => {}
            }
        }
        let mut lc = [0u16; 288];
        let mut dc = [0u16; 32];
        codes(&lit_len, &mut lc);
        codes(&dist_len, &mut dc);
        symbols(bits, &s.syms, &lit_len, &lc, &dist_len, &dc);
    }
    s.clear();
}

/// A block's symbols and its end, under these codes.
fn symbols(bits: &mut Bits, syms: &[u32], ll: &[u8], lc: &[u16], dl: &[u8], dc: &[u16]) {
    for &sym in syms {
        if sym & MATCH == 0 {
            bits.put(lc[sym as usize] as u32, ll[sym as usize] as u32);
        } else {
            let len3 = ((sym >> 15) & 0xff) as usize;
            let d1 = (sym & 0x7fff) as usize;
            let code = LEN_CODE[len3] as usize;
            let extra = LEN_EXTRA[code] as u32;
            let s = 257 + code;
            bits.put(
                lc[s] as u32 | ((len3 as u32 + 3 - LEN_BASE[code] as u32) << ll[s]),
                ll[s] as u32 + extra,
            );
            let dcode = dist_code(d1);
            let dextra = DIST_EXTRA[dcode] as u32;
            bits.put(dc[dcode] as u32, dl[dcode] as u32);
            if dextra > 0 {
                bits.put(d1 as u32 + 1 - DIST_BASE[dcode] as u32, dextra);
            }
        }
    }
    bits.put(lc[256] as u32, ll[256] as u32);
}

/// `raw` in stored blocks of 65 535 bytes at most, the last one last where
/// `last` says so; an empty `raw` is one empty block, zlib's sync mark.
fn stored(bits: &mut Bits, raw: &[u8], last: bool) {
    let mut chunks = raw.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        bits.put(last as u32, 3);
        bits.align();
        bits.out.extend_from_slice(&[0, 0, 0xff, 0xff]);
        return;
    }
    while let Some(c) = chunks.next() {
        let end = last && chunks.peek().is_none();
        bits.put(end as u32, 3);
        bits.align();
        let len = c.len() as u16;
        bits.out.extend_from_slice(&len.to_le_bytes());
        bits.out.extend_from_slice(&(!len).to_le_bytes());
        bits.out.extend_from_slice(c);
    }
}

// ------------------------------------------------------------------ LZ77

/// deflate's window.
const WINDOW: usize = 32_768;
/// Candidates a position looks at: 4 were 11% faster over JSON rows for 3%
/// more bytes (docs/design/http.md#the-compressor).
const CHAIN: u32 = 8;
/// A match this long is taken at once: from 64, 4% slower and no smaller.
const NICE: usize = 32;
/// A match this long or shorter has its positions hashed too; past it they
/// are skipped, zlib's fast levels' trade: up to 16, 13% slower for 0.1%
/// fewer bytes.
const INSERT: usize = 6;

#[inline(always)]
fn read32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
}

/// How many bytes `a` and `b` share from the start, up to `max`.
#[inline(always)]
fn common(a: &[u8], b: &[u8], max: usize) -> usize {
    let (a, b) = (&a[..max], &b[..max]);
    let mut i = 0;
    while i + 8 <= max {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().unwrap())
            ^ u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < max && a[i] == b[i] {
        i += 1;
    }
    i
}

/// `input` deflated onto `bits`, the last block final where `last` says so;
/// otherwise left where it ends, for the stream's sync mark after.
fn deflate(input: &[u8], bits: &mut Bits, last: bool) {
    let n = input.len();
    // Sized to the input: a 2 KB event takes 24 KB of tables, 320 KB the
    // most, and zlib's streaming deflate keeps 256 KB for as long as it
    // lives.
    let hbits = (usize::BITS - n.max(256).saturating_sub(1).leading_zeros()).clamp(8, 15);
    let shift = 32 - hbits;
    let mut head = vec![0u32; 1 << hbits];
    let wsize = n.next_power_of_two().clamp(1, WINDOW);
    let wmask = wsize - 1;
    let mut prev = vec![0u32; wsize];
    let mut s = Symbols::new(BLOCK.min(n + 1));
    let hash = |p: usize| (read32(input, p).wrapping_mul(0x9E37_79B1) >> shift) as usize;
    // Positions hashed: four bytes from each.
    let end = n.saturating_sub(3);
    let mut p = 0;
    let mut start = 0;
    let mut misses = 0usize;
    while p < end {
        let h = hash(p);
        let mut cand = head[h] as usize;
        prev[p & wmask] = cand as u32;
        head[h] = p as u32 + 1;
        let max = (n - p).min(258);
        let here = &input[p..p + max];
        let first = read32(here, 0);
        let (mut best, mut best_dist) = (3usize, 0usize);
        let mut chain = CHAIN;
        while cand != 0 && chain > 0 {
            let c = cand - 1;
            let dist = p - c;
            if dist > WINDOW {
                break;
            }
            let there = &input[c..c + max];
            if there[best] == here[best] && read32(there, 0) == first {
                let l = common(there, here, max);
                if l > best {
                    best = l;
                    best_dist = dist;
                    if l >= NICE || l == max {
                        break;
                    }
                }
            }
            let next = prev[c & wmask] as usize;
            if next >= cand {
                break;
            }
            cand = next;
            chain -= 1;
        }
        if best >= 4 {
            s.matched(best, best_dist);
            if best <= INSERT {
                for q in p + 1..(p + best).min(end) {
                    let h = hash(q);
                    prev[q & wmask] = head[h];
                    head[h] = q as u32 + 1;
                }
            }
            p += best;
            misses = 0;
        } else {
            // A run with nothing found steps on faster, as LZ4 and Snappy
            // do: a long incompressible stretch -- a base64 field -- is
            // stored, not searched byte by byte.
            misses += 1;
            let step = 1 + (misses >> 6);
            let to = (p + step).min(end);
            for &b in &input[p..to] {
                s.literal(b);
            }
            p = to;
        }
        if s.syms.len() >= BLOCK - 1 {
            write_block(bits, &mut s, &input[start..p], false);
            start = p;
        }
    }
    for &b in &input[p..] {
        s.literal(b);
    }
    // The last block says so even where it holds nothing but its end: the
    // input may have ended a full block exactly.
    if last || !s.syms.is_empty() {
        write_block(bits, &mut s, &input[start..], last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic stream of bytes (xorshift64*).
    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
            })
            .collect()
    }

    /// Rows as a `get` answers them: the seed and `/query`'s JSON.
    fn rows(n: usize) -> Vec<u8> {
        let mut s = String::from("{\"rows\":[");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"_id\":{i},\"name\":\"user {}\",\"email\":\"u{}@example.com\",\
                 \"age\":{},\"score\":{:.3},\"tags\":[\"t{}\",\"t{}\"]}}",
                i * 7919 % 10007,
                i * 31,
                18 + i % 60,
                (i as f64 * 0.618).fract() * 100.0,
                i % 13,
                i % 7
            ));
        }
        s.push_str("]}");
        s.into_bytes()
    }

    /// Python's zlib, an inflater not ours, run once over `script` with
    /// `input`; `None` where no `python3` runs here.
    fn python(script: &str, input: &[u8]) -> Option<Vec<u8>> {
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};
        let mut child = match Command::new("python3")
            .args(["-I", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("no python3 to inflate with ({e}): skipped");
                return None;
            }
        };
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
        writer.join().unwrap().unwrap();
        assert!(
            child.wait().unwrap().success(),
            "python's zlib refused a member"
        );
        Some(out)
    }

    /// Each member, length-prefixed, inflated whole by Python's zlib: one
    /// whole member each, nothing after it.
    fn inflated(members: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
        const SCRIPT: &str = "
import sys, zlib, struct
data = sys.stdin.buffer.read()
i = 0
while i < len(data):
    (n,) = struct.unpack_from('<I', data, i)
    i += 4
    d = zlib.decompressobj(31)
    raw = d.decompress(data[i:i + n]) + d.flush()
    i += n
    if not d.eof or d.unused_data:
        sys.exit('not one whole member')
    sys.stdout.buffer.write(struct.pack('<I', len(raw)) + raw)
";
        let mut framed = Vec::new();
        for m in members {
            framed.extend_from_slice(&(m.len() as u32).to_le_bytes());
            framed.extend_from_slice(m);
        }
        let out = python(SCRIPT, &framed)?;
        let mut back = Vec::new();
        let mut i = 0;
        while i < out.len() {
            let n = u32::from_le_bytes(out[i..i + 4].try_into().unwrap()) as usize;
            back.push(out[i + 4..i + 4 + n].to_vec());
            i += 4 + n;
        }
        Some(back)
    }

    #[test]
    fn python_inflates_every_kind_of_input() {
        let mut inputs: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"hello, hello, hello".to_vec(),
            vec![0u8; 1 << 20],
            b"abc".repeat(100_000),
            (0..=255u8).cycle().take(70_000).collect(),
            rows(3_000),
            noise(1, 200_000),
        ];
        // Block boundaries: noise is a literal a byte, so these end a
        // block of symbols one short, on and past it, and a stored block's
        // 65 535 bytes.
        for n in [
            BLOCK - 2,
            BLOCK - 1,
            BLOCK,
            BLOCK + 1,
            2 * BLOCK - 1,
            2 * BLOCK,
            65_534,
            65_535,
            65_536,
            131_071,
        ] {
            inputs.push(noise(n as u64, n));
        }
        // Text with noise between: matches and literals, stored and
        // compressed blocks in one member; a repeat at the window's very
        // edge and past it.
        let mut mixed = Vec::new();
        for i in 0..40 {
            mixed.extend_from_slice(&rows(40 + i));
            mixed.extend_from_slice(&noise(i as u64 + 99, 3_000 + 997 * i));
        }
        inputs.push(mixed);
        // A full block ending the input: literals, then a match to its end.
        let mut full = noise(11, BLOCK - 2);
        full.extend_from_within(..100);
        inputs.push(full);
        let mut edge = noise(7, WINDOW);
        edge.extend_from_within(..300);
        edge.extend_from_slice(&noise(8, 5));
        edge.extend_from_within(..300);
        inputs.push(edge);
        let members: Vec<Vec<u8>> = inputs
            .iter()
            .map(|i| {
                let mut out = Vec::new();
                compress(i, &mut out);
                out
            })
            .collect();
        let Some(back) = inflated(&members) else {
            return;
        };
        assert_eq!(back.len(), inputs.len());
        for (k, (b, i)) in back.iter().zip(&inputs).enumerate() {
            assert!(b == i, "input {k} ({} bytes) came back otherwise", i.len());
        }
        // Text shrinks as deflate's does; noise grows by its stored
        // blocks' headers alone, 5 bytes a block of symbols.
        assert!(
            members[6].len() * 4 < inputs[6].len(),
            "{}",
            members[6].len()
        );
        let noisy = members[7].len() - inputs[7].len();
        assert!(
            noisy <= 18 + 5 * (inputs[7].len() / (BLOCK - 1) + 1),
            "{noisy}"
        );
    }

    /// A stream's every piece is read whole once its bytes are in, before
    /// the next piece and before the member's end.
    #[test]
    fn a_stream_is_read_a_piece_at_a_time() {
        const SCRIPT: &str = "
import sys, zlib, struct
data = sys.stdin.buffer.read()
(k,) = struct.unpack_from('<I', data, 0)
ends = struct.unpack_from('<%dI' % k, data, 4)
body = data[4 + 4 * k:]
d = zlib.decompressobj(31)
at, got = 0, 0
for e in ends:
    got += len(d.decompress(body[at:e]))
    at = e
    sys.stdout.buffer.write(struct.pack('<I', got))
got += len(d.decompress(body[at:]))
sys.stdout.buffer.write(struct.pack('<IB', got, 1 if d.eof and not d.unused_data else 0))
";
        let pieces: Vec<Vec<u8>> = vec![
            rows(2_000),
            b": keepalive\n\n".to_vec(),
            rows(3),
            noise(5, 2_000),
            Vec::new(),
            rows(400),
            b"event: change\ndata: {}\n\n".to_vec(),
        ];
        let mut head = Vec::new();
        let mut stream = Stream::start(&mut head);
        let mut body = head;
        let mut ends = Vec::new();
        for p in &pieces {
            stream.piece(p, MIN_BYTES, &mut body);
            ends.push(body.len() as u32);
            // Every piece ends with the sync mark, a stored one too: Go's
            // inflater hands a stored block on only at the next flush,
            // where Python's and Node's hand it on at once.
            assert_eq!(
                body[body.len() - 4..],
                [0, 0, 0xff, 0xff],
                "{} bytes",
                p.len()
            );
        }
        stream.finish(&mut body);
        let mut framed = (ends.len() as u32).to_le_bytes().to_vec();
        for e in &ends {
            framed.extend_from_slice(&e.to_le_bytes());
        }
        framed.extend_from_slice(&body);
        let Some(out) = python(SCRIPT, &framed) else {
            return;
        };
        let mut want = 0;
        for (k, p) in pieces.iter().enumerate() {
            want += p.len();
            let got = u32::from_le_bytes(out[4 * k..4 * k + 4].try_into().unwrap()) as usize;
            assert_eq!(got, want, "piece {k} not readable on its arrival");
        }
        let tail = 4 * pieces.len();
        let got = u32::from_le_bytes(out[tail..tail + 4].try_into().unwrap()) as usize;
        assert_eq!((got, out[tail + 4]), (want, 1), "the member's end");
        // The compressed seed is the stream's bulk, far under its rows.
        assert!(ends[0] as usize * 4 < pieces[0].len(), "{}", ends[0]);
    }

    #[test]
    fn crc_is_iso_3309s() {
        assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(0, b""), 0);
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let whole = crc32(0, &data);
        let (a, b) = data.split_at(377);
        assert_eq!(crc32(crc32(0, a), b), whole);
    }

    #[test]
    fn tables_hold_deflates_codes() {
        for len in 3..=258usize {
            let c = LEN_CODE[len - 3] as usize;
            let base = LEN_BASE[c] as usize;
            assert!(len >= base && len - base < 1 << LEN_EXTRA[c], "{len}");
        }
        assert_eq!(LEN_CODE[255], 28);
        for d in 1..=32_768usize {
            let c = dist_code(d - 1);
            let base = DIST_BASE[c] as usize;
            assert!(d >= base && d - base < 1 << DIST_EXTRA[c], "{d}");
        }
    }

    /// Kraft's sum of `lens` is one: the code is complete.
    fn complete(lens: &[u8]) -> bool {
        let sum: u64 = lens
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1u64 << (32 - l))
            .sum();
        sum == 1u64 << 32
    }

    #[test]
    fn lengths_are_complete_and_bounded() {
        let mut lens = [0u8; 288];
        // Fibonacci counts, which a plain Huffman code makes as deep as
        // there are symbols.
        let mut fib = vec![1u32, 1];
        while fib.len() < 40 {
            let k = fib.len();
            fib.push(fib[k - 1].saturating_add(fib[k - 2]));
        }
        lengths(&fib, 15, &mut lens);
        assert!(lens[..40].iter().all(|&l| (1..=15).contains(&l)));
        assert!(complete(&lens[..40]));
        lengths(&fib[..19], 7, &mut lens);
        assert!(lens[..19].iter().all(|&l| (1..=7).contains(&l)));
        assert!(complete(&lens[..19]));
        // One symbol, and none: two codes of one bit.
        let mut one = [0u32; 30];
        one[7] = 9;
        lengths(&one, 15, &mut lens);
        assert_eq!((lens[7], lens[0]), (1, 1));
        lengths(&[0u32; 30], 15, &mut lens);
        assert_eq!((lens[0], lens[1]), (1, 1));
        // A spread of counts.
        let counts: Vec<u32> = (0..286u32)
            .map(|i| i.wrapping_mul(2_654_435_761) % 1000)
            .collect();
        lengths(&counts, 15, &mut lens);
        assert!(complete(&lens[..286]));
        for (i, &c) in counts.iter().enumerate() {
            assert_eq!(c == 0, lens[i] == 0, "{i}");
        }
    }

    fn request(accept: &[&str]) -> Request {
        Request {
            method: crate::http::Method::Get,
            target: "/".into(),
            path: "/".into(),
            query: Vec::new(),
            headers: accept
                .iter()
                .map(|v| ("Accept-Encoding".to_string(), v.to_string()))
                .collect(),
            body: Vec::new(),
            keep_alive: true,
        }
    }

    #[test]
    fn accept_encoding_is_read_as_rfc_9110_says() {
        for (values, takes) in [
            (&["gzip"][..], true),
            (&["gzip, deflate, br, zstd"][..], true),
            (&["br;q=1.0, GZIP;q=0.5"][..], true),
            (&["x-gzip"][..], true),
            (&["*"][..], true),
            (&["deflate", "gzip"][..], true),
            (&["gzip;q=0"][..], false),
            (&["gzip; q=0.000"][..], false),
            (&["*;q=0.1, gzip;q=0"][..], false),
            (&["identity"][..], false),
            (&["br, deflate"][..], false),
            (&["gzipped"][..], false),
            (&[][..], false),
            (&["*;q=0"][..], false),
            (&["gzip;q=0.001"][..], true),
        ] {
            assert_eq!(accepts(&request(values)), takes, "{values:?}");
        }
    }

    #[test]
    fn only_what_is_worth_it_is_compressed() {
        let body = br#"{"rows":[{"id":1,"name":"a"},{"id":2,"name":"a"}]}"#.repeat(100);
        let gz = request(&["gzip"]);
        let answer = |b: &[u8]| Response::json(200, b.to_vec());
        let on = crate::Config::default();
        let off_cfg = crate::Config {
            gzip: None,
            ..crate::Config::default()
        };
        // A client across the internet.
        crate::audit::connection("http", Some("203.0.113.9:4000".parse().unwrap()));
        let mut r = answer(&body);
        encode(&mut r, &gz, &on);
        assert!(r.body.len() < body.len() / 10, "{}", r.body.len());
        assert!(r
            .extra
            .contains(&("Content-Encoding".into(), "gzip".into())));
        assert!(r.extra.contains(&("Vary".into(), "Accept-Encoding".into())));
        // Small, off, unasked: as it was; unasked still says what it
        // varies by.
        let mut small = answer(&body[..MIN_BYTES - 1]);
        encode(&mut small, &gz, &on);
        assert!(small.extra.is_empty());
        let mut off = answer(&body);
        encode(&mut off, &gz, &off_cfg);
        assert!(off.extra.is_empty() && off.body == body);
        let mut unasked = answer(&body);
        encode(&mut unasked, &request(&[]), &on);
        assert_eq!(unasked.body, body);
        assert_eq!(
            unasked.extra,
            vec![("Vary".into(), "Accept-Encoding".into())]
        );
        // A tag, its no-transform, a HEAD, a file's pages: never.
        let mut tagged = answer(&body).header("ETag", "\"1\"");
        encode(&mut tagged, &gz, &on);
        assert_eq!(tagged.body, body);
        let mut row = answer(&body);
        row.no_transform = true;
        encode(&mut row, &gz, &on);
        assert_eq!(row.body, body);
        let mut head = request(&["gzip"]);
        head.method = crate::http::Method::Head;
        let mut headed = answer(&body);
        encode(&mut headed, &head, &on);
        assert_eq!(headed.body, body);
        let mut pages = answer(&body);
        pages.content_type = "application/octet-stream";
        encode(&mut pages, &gz, &on);
        assert_eq!(pages.body, body);
        // Incompressible: identity, the shorter.
        let noise: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mut n = answer(&noise);
        encode(&mut n, &gz, &on);
        assert!(n.body.len() <= noise.len());
        // A client on this host or its network: as it is, unless told.
        for peer in [
            "127.0.0.1:4000",
            "10.1.2.3:4000",
            "[::1]:4000",
            "[fd00::5]:4000",
        ] {
            crate::audit::connection("http", Some(peer.parse().unwrap()));
            let mut near = answer(&body);
            encode(&mut near, &gz, &on);
            assert_eq!(near.body, body, "{peer}");
            assert_eq!(near.extra, vec![("Vary".into(), "Accept-Encoding".into())]);
            let mut told = answer(&body);
            let local = crate::Config {
                gzip_local: true,
                ..crate::Config::default()
            };
            encode(&mut told, &gz, &local);
            assert!(told.body.len() < body.len() / 10, "{peer}");
        }
    }

    #[test]
    fn local_is_this_host_and_private_networks() {
        for (ip, near) in [
            ("127.0.0.1", true),
            ("10.0.0.7", true),
            ("172.16.4.1", true),
            ("192.168.1.20", true),
            ("169.254.1.1", true),
            ("::1", true),
            ("fd12::1", true),
            ("fe80::1", true),
            ("::ffff:192.168.0.2", true),
            ("100.64.0.1", false),
            ("203.0.113.9", false),
            ("8.8.8.8", false),
            ("2001:db8::1", false),
        ] {
            assert_eq!(local(ip.parse().unwrap()), near, "{ip}");
        }
    }
}
