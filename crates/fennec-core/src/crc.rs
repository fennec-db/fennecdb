//! CRC32C (Castagnoli), the checksum each record of a file carries
//! (docs/design/storage.md#file-format): the CPU's instruction where it has
//! one -- aarch64's `crc32c`, found at build time where the target promises
//! it (every Apple chip) and at run time elsewhere, and x86_64's SSE4.2
//! `crc32`, found at run time -- and a table where it has none, the browser
//! module's wasm32 among them. Every way gives the same bits: a file written
//! on one opens on the others (`tests` holds each to the tables).
//!
//! CRC32C rather than another sum: it is what the instructions compute, so
//! a record's costs about a cycle per 8 bytes where they run, and the
//! tables it falls back on are made at run time; a 32-bit CRC finds every burst of up to 32
//! flipped bits and every odd count of them, and misses any other damage
//! once in 2^32 -- a torn record's zeros or stale bytes among it. The sync
//! log's 64-bit multiply hash (`synclog.rs`) promises none of that, and
//! xxHash or a 64-bit CRC would be code of our own on every target, never
//! an instruction.

/// The polynomial, reflected.
const POLY: u32 = 0x82F6_3B78;

/// `crc`, the checksum of the bytes before, carried over `bytes`: the
/// checksum of them all. A new one starts from 0.
pub fn crc32c(crc: u32, bytes: &[u8]) -> u32 {
    !raw(!crc, bytes)
}

/// The register carried over `bytes`, neither end inverted.
#[inline]
fn raw(state: u32, bytes: &[u8]) -> u32 {
    #[cfg(all(target_arch = "aarch64", target_feature = "crc"))]
    {
        // SAFETY: the target promises the instructions.
        unsafe { arm::raw(state, bytes) }
    }
    #[cfg(all(target_arch = "aarch64", not(target_feature = "crc")))]
    {
        if std::arch::is_aarch64_feature_detected!("crc") {
            // SAFETY: the CPU said it has them.
            return unsafe { arm::raw(state, bytes) };
        }
        table(state, bytes)
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("sse4.2") {
            // SAFETY: the CPU said it has them.
            return unsafe { x86::raw(state, bytes) };
        }
        table(state, bytes)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    table(state, bytes)
}

/// Eight bytes a step through eight tables (slicing-by-8): what a CPU
/// without the instruction runs, wasm32's among them. Each byte's lookup
/// stands apart from the others', where a byte a step waits on the one
/// before: in the browser module a 100-document `put` took 6 us more than
/// with no sum that way, about 7 cycles a byte, and 1.5 us so
/// (docs/design/storage.md#file-format).
#[cfg_attr(
    all(target_arch = "aarch64", target_feature = "crc", not(test)),
    allow(dead_code)
)]
fn table(mut state: u32, bytes: &[u8]) -> u32 {
    let t = tables();
    let (words, rest) = bytes.as_chunks::<8>();
    for w in words {
        let lo = state ^ u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        state = t[7][(lo & 0xff) as usize]
            ^ t[6][(lo >> 8 & 0xff) as usize]
            ^ t[5][(lo >> 16 & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][w[4] as usize]
            ^ t[2][w[5] as usize]
            ^ t[1][w[6] as usize]
            ^ t[0][w[7] as usize];
    }
    for &b in rest {
        state = t[0][((state ^ b as u32) & 0xff) as usize] ^ (state >> 8);
    }
    state
}

/// The register after each byte from a register of 0 (`[0]`), and after
/// it and 1 to 7 zero bytes more: made once, at the first use, rather than
/// carried as 8 KiB of the browser module's data.
#[cfg_attr(
    all(target_arch = "aarch64", target_feature = "crc", not(test)),
    allow(dead_code)
)]
fn tables() -> &'static [[u32; 256]; 8] {
    static TABLES: std::sync::OnceLock<[[u32; 256]; 8]> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        let mut t = [[0u32; 256]; 8];
        for (i, e) in t[0].iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            }
            *e = c;
        }
        for k in 1..8 {
            for i in 0..256 {
                let c = t[k - 1][i];
                t[k][i] = (c >> 8) ^ t[0][(c & 0xff) as usize];
            }
        }
        t
    })
}

/// The bytes a lane takes in a stretch of three: a long run is summed as
/// three lanes at once, each its own chain of instructions, so the CPU runs
/// them side by side -- one instruction waits three cycles for the one
/// before it, and starts one a cycle -- and the lanes are joined by two
/// multiplications in the polynomial's field. 16 KiB: the joins are then
/// about 5% of a stretch's time.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const LANE: usize = 16 << 10;

/// x^(8 LANE) modulo the polynomial: what a lane's register is multiplied
/// by to stand as if the next lane's bytes had followed it.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const LANE_SHIFT: u32 = {
    // x^0 is the top bit, reflected; each step multiplies by x.
    let mut p = 1u32 << 31;
    let mut n = 0;
    while n < 8 * LANE {
        p = if p & 1 != 0 { (p >> 1) ^ POLY } else { p >> 1 };
        n += 1;
    }
    p
};

/// The checksum of the last `back` bytes of a run, from the checksum of
/// the whole run and of the bytes before them, `front`: `crc32c(0, a ++ b)`
/// is `crc32c(0, a) · x^(8 |b|) ^ crc32c(0, b)` in the polynomial's field
/// (zlib's `crc32_combine`), so `b`'s is found without reading `b` again.
/// What a judgment of damage checks a long record it may have found by
/// chance with (`engine::judge_after`): summed again, the vectors' bytes
/// after a lost head -- a twentieth of which read as heads, some of
/// megabytes -- were 133 MB summed for a look over 4 MiB.
pub(crate) fn crc32c_after(whole: u32, front: u32, back: u64) -> u32 {
    // x^(8 back): x^8 squared once a bit of `back`, and taken where the
    // bit is set -- a table of the squares, made once, was 220 bytes more
    // of the browser module for its lock.
    let (mut shift, mut square, mut n) = (1u32 << 31, 1u32 << 23, back);
    while n != 0 {
        if n & 1 != 0 {
            shift = mul(square, shift);
        }
        square = mul(square, square);
        n >>= 1;
    }
    whole ^ mul(shift, front)
}

/// `a` times `b` modulo the polynomial, both reflected (zlib's `multmodp`).
fn mul(a: u32, mut b: u32) -> u32 {
    let mut m = 1u32 << 31;
    let mut p = 0;
    loop {
        if a & m != 0 {
            p ^= b;
            if a & (m - 1) == 0 {
                return p;
            }
        }
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
}

/// The register over `bytes` from `state`, an instruction a word: three
/// lanes at once over each stretch of `3 * LANE`, then one over the rest.
/// The register is linear in its bytes, so a stretch of lanes `a`, `b`, `c`
/// from `s` is `((R(s, a) · x^8L) ^ R(0, b)) · x^8L ^ R(0, c)`.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
macro_rules! lanes {
    ($state:expr, $bytes:expr, $word:expr, $word32:expr, $word16:expr, $byte:expr) => {{
        let mut s = $state;
        let mut bytes: &[u8] = $bytes;
        while bytes.len() >= 3 * LANE {
            let (a, rest) = bytes.split_at(LANE);
            let (b, rest) = rest.split_at(LANE);
            let (c, rest) = rest.split_at(LANE);
            let (a, _) = a.as_chunks::<8>();
            let (b, _) = b.as_chunks::<8>();
            let (c, _) = c.as_chunks::<8>();
            let (mut x, mut y, mut z) = (s, 0u32, 0u32);
            for ((p, q), r) in a.iter().zip(b).zip(c) {
                x = $word(x, u64::from_le_bytes(*p));
                y = $word(y, u64::from_le_bytes(*q));
                z = $word(z, u64::from_le_bytes(*r));
            }
            s = mul(LANE_SHIFT, mul(LANE_SHIFT, x) ^ y) ^ z;
            bytes = rest;
        }
        let (words, mut rest) = bytes.as_chunks::<8>();
        for w in words {
            s = $word(s, u64::from_le_bytes(*w));
        }
        // The last 7 bytes at most, in three instructions rather than seven:
        // a lone `put`'s record is a few dozen bytes.
        if let Some((w, r)) = rest.split_first_chunk::<4>() {
            s = $word32(s, u32::from_le_bytes(*w));
            rest = r;
        }
        if let Some((w, r)) = rest.split_first_chunk::<2>() {
            s = $word16(s, u16::from_le_bytes(*w));
            rest = r;
        }
        if let Some(&b) = rest.first() {
            s = $byte(s, b);
        }
        s
    }};
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::{mul, LANE, LANE_SHIFT};
    use core::arch::aarch64::{__crc32cb, __crc32cd, __crc32ch, __crc32cw};

    #[target_feature(enable = "crc")]
    #[inline]
    pub(super) fn raw(state: u32, bytes: &[u8]) -> u32 {
        lanes!(state, bytes, __crc32cd, __crc32cw, __crc32ch, __crc32cb)
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{mul, LANE, LANE_SHIFT};
    use core::arch::x86_64::{_mm_crc32_u16, _mm_crc32_u32, _mm_crc32_u64, _mm_crc32_u8};

    #[target_feature(enable = "sse4.2")]
    pub(super) fn raw(state: u32, bytes: &[u8]) -> u32 {
        lanes!(
            state,
            bytes,
            word,
            _mm_crc32_u32,
            _mm_crc32_u16,
            _mm_crc32_u8
        )
    }

    /// The register over a word: the instruction's is 64 bits wide.
    #[target_feature(enable = "sse4.2")]
    #[inline]
    fn word(state: u32, w: u64) -> u32 {
        _mm_crc32_u64(state as u64, w) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check value of the CRC catalogue and RFC 3720's vectors.
    #[test]
    fn the_published_values() {
        assert_eq!(crc32c(0, b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(0, &[0; 32]), 0x8A91_36AA);
        assert_eq!(crc32c(0, &[0xff; 32]), 0x62A8_AB43);
        let up: Vec<u8> = (0..32).collect();
        assert_eq!(crc32c(0, &up), 0x46DD_794E);
        assert_eq!(crc32c(0, b""), 0);
        assert_eq!(!table(!0, b"123456789"), 0xE306_9283);
        assert_eq!(!table(!0, &[0; 32]), 0x8A91_36AA);
    }

    /// The instruction, its lanes and the table give the same bits over
    /// every length around a word and a stretch, and a checksum carried on
    /// is the one of the bytes whole.
    #[test]
    fn every_way_gives_the_same_bits() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let bytes: Vec<u8> = (0..(3 * (16 << 10)) * 2 + 100)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let stretch = 3 * (16 << 10);
        let lens = (0..80).chain([stretch - 1, stretch, stretch + 9, 2 * stretch + 77]);
        for len in lens {
            let b = &bytes[..len];
            // A byte a step, the polynomial's own definition.
            let mut bitwise = !0u32;
            for &x in b {
                bitwise ^= x as u32;
                for _ in 0..8 {
                    bitwise = if bitwise & 1 != 0 {
                        (bitwise >> 1) ^ POLY
                    } else {
                        bitwise >> 1
                    };
                }
            }
            let want = !bitwise;
            assert_eq!(!table(!0, b), want, "the tables, {len}");
            assert_eq!(crc32c(0, b), want, "{len}");
            for cut in [0, 1, 7, 8, len / 3, len] {
                let cut = cut.min(len);
                assert_eq!(
                    crc32c(crc32c(0, &b[..cut]), &b[cut..]),
                    want,
                    "{len} at {cut}"
                );
                // And the bytes after the cut's, from the whole's and
                // those before.
                assert_eq!(
                    crc32c_after(want, crc32c(0, &b[..cut]), (len - cut) as u64),
                    crc32c(0, &b[cut..]),
                    "{len} after {cut}"
                );
            }
        }
    }
}
