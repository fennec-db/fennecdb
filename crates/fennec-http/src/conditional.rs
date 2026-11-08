//! Optimistic concurrency on a row's REST routes, by HTTP's own
//! conditional requests (RFC 9110, section 13): `GET /<name>/<id>` answers
//! the row's tag in `ETag`, and a `PATCH`, `PUT` or `DELETE` of
//! `/<name>/<id>` sent with `If-Match` or `If-None-Match` is written only
//! where the row, as the server holds it under the write lock that then
//! writes it, meets them -- else 412 and nothing written
//! (docs/design/http.md, *Row tags*).
//!
//! A tag is a hash of the row's JSON as its reader is shown it, the very
//! bytes `GET /<name>/<id>` answers: it changes whenever those do, it is
//! the same on a replica and after a restart for the same row, and it says
//! nothing of a field a read rule withholds, which those bytes hold as
//! `null`.

use crate::http::{Request, Response};

/// A row's tag as `ETag` writes it: `"` and the 128 bits of SipHash-2-4
/// over `row`, the JSON object its reader is shown, in 32 hex digits.
///
/// Not SHA-256, which `content_tag` takes for a poll's answer: in software
/// it took 7 430 instructions a row of 66 bytes, a third of the read of
/// that row by id (23 200), and 50 300 a row of 1 KB; this takes about
/// four an input byte (`examples/rows.rs`'s `tag`). Its key is fixed, so
/// the same row has the same tag on every server and after every restart;
/// it needs no secret, since the bytes it hashes are the ones its reader
/// is answered beside it. What a key-less hash gives up is its being no
/// signature: no shortcut to a second row of the same tag is known for
/// SipHash with its key known, and one would let only a writer of the row
/// pass its own write off as unchanged -- as writing the old value back
/// does.
pub fn tag(row: &[u8]) -> Tag {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let (a, b) = siphash128(KEY, row);
    let mut t = [b'"'; 34];
    for (i, half) in [a, b].into_iter().enumerate() {
        for j in 0..16 {
            t[1 + i * 16 + j] = HEX[(half >> (60 - 4 * j)) as usize & 15];
        }
    }
    Tag(t)
}

/// A tag as `ETag` writes it, quotes and all, on the stack: written a
/// character at a time into a `String`, a digit through `char::from_digit`,
/// it took some 1 900 instructions of the 2 200 a tag of a row of 66 bytes
/// cost.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Tag([u8; 34]);

impl Tag {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl std::fmt::Debug for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The fixed key: the reference implementation's test key, `00 01 .. 0f`,
/// so the tag can be worked out anywhere from its row's JSON alone.
const KEY: (u64, u64) = (0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908);

/// SipHash-2-4 with a 128-bit output (Aumasson and Bernstein's
/// `siphash.c` with `cROUNDS` 2, `dROUNDS` 4 and an output of 16 bytes):
/// its two little-endian halves.
fn siphash128((k0, k1): (u64, u64), m: &[u8]) -> (u64, u64) {
    let mut v = [
        k0 ^ 0x736f_6d65_7073_6575,
        k1 ^ 0x646f_7261_6e64_6f6d ^ 0xee,
        k0 ^ 0x6c79_6765_6e65_7261,
        k1 ^ 0x7465_6462_7974_6573,
    ];
    let mut rest = m;
    while let Some((w, r)) = rest.split_first_chunk::<8>() {
        compress(&mut v, u64::from_le_bytes(*w), 2);
        rest = r;
    }
    let mut last = [0u8; 8];
    last[..rest.len()].copy_from_slice(rest);
    last[7] = m.len() as u8;
    compress(&mut v, u64::from_le_bytes(last), 2);
    v[2] ^= 0xee;
    rounds(&mut v, 4);
    let a = v[0] ^ v[1] ^ v[2] ^ v[3];
    v[1] ^= 0xdd;
    rounds(&mut v, 4);
    (a, v[0] ^ v[1] ^ v[2] ^ v[3])
}

#[inline(always)]
fn compress(v: &mut [u64; 4], m: u64, n: usize) {
    v[3] ^= m;
    rounds(v, n);
    v[0] ^= m;
}

#[inline(always)]
fn rounds(v: &mut [u64; 4], n: usize) {
    for _ in 0..n {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
}

/// What a request asks of the row it names before it is answered or
/// written: `If-Match` and `If-None-Match`, the standard's names for the
/// two, read in one pass over its headers.
#[derive(Clone, Copy, Default)]
pub struct Preconditions<'a> {
    pub if_match: Option<&'a str>,
    pub if_none_match: Option<&'a str>,
}

impl<'a> Preconditions<'a> {
    pub fn of(req: &'a Request) -> Preconditions<'a> {
        let mut p = Preconditions::default();
        for (k, v) in &req.headers {
            // Both names start `if-`: one look at a byte passes over the rest.
            if !matches!(k.as_bytes().first(), Some(b'i' | b'I')) {
                continue;
            }
            if k.eq_ignore_ascii_case("if-match") {
                p.if_match = Some(v);
            } else if k.eq_ignore_ascii_case("if-none-match") {
                p.if_none_match = Some(v);
            }
        }
        p
    }

    pub fn any(&self) -> bool {
        self.if_match.is_some() || self.if_none_match.is_some()
    }

    /// Whether `If-None-Match` is `*`: a `PUT` made only where there is no
    /// such row, which the route sends as an `insert`.
    pub fn only_if_absent(&self) -> bool {
        self.if_none_match.is_some_and(|v| v.trim() == "*")
    }

    /// The refusal of a write over a row whose tag is `now` -- `None`
    /// where its writer is shown no such row -- in the standard's order:
    /// `If-Match` first, compared strongly, then `If-None-Match`, weakly
    /// (RFC 9110, 13.2.2). 412 where either fails.
    pub fn refused(&self, now: Option<&str>) -> Option<Response> {
        if let Some(asked) = self.if_match {
            if !listed(asked, now, false) {
                return Some(failed(match now {
                    None => "If-Match: there is no such row",
                    Some(_) => "If-Match: the row is not the one tagged, it changed since",
                }));
            }
        }
        if let Some(asked) = self.if_none_match {
            if listed(asked, now, true) {
                return Some(failed(match asked.trim() {
                    "*" => "If-None-Match: * -- the row is there",
                    _ => "If-None-Match: the row is the one tagged",
                }));
            }
        }
        None
    }

    /// A read's answer to its preconditions over a row tagged `now`: 412
    /// where `If-Match` fails, 304 where `If-None-Match` names it.
    pub fn read(&self, now: Option<&str>) -> Option<Response> {
        if let Some(asked) = self.if_match {
            if !listed(asked, now, false) {
                return Some(failed("If-Match: the row is not the one tagged"));
            }
        }
        match (self.if_none_match, now) {
            (Some(asked), Some(tag)) if listed(asked, now, true) => {
                Some(Response::json(304, Vec::new()).header("ETag", tag))
            }
            _ => None,
        }
    }
}

/// 412 Precondition Failed, saying which precondition it was.
fn failed(why: &str) -> Response {
    Response::error(412, why)
}

/// Whether a header's list -- `*`, or tags apart by commas, each `"..."`
/// or a weak `W/"..."` -- holds `now`: `*` any row, a tag that one alone.
/// `weak` compares as `If-None-Match` does, a weak tag naming the strong
/// one of its bytes; else as `If-Match` does, a weak tag naming none. A
/// list that is no list names nothing.
fn listed(header: &str, now: Option<&str>, weak: bool) -> bool {
    let Some(now) = now else {
        return false;
    };
    let mut rest = header.trim();
    if rest == "*" {
        return true;
    }
    while !rest.is_empty() {
        rest = rest.trim_start_matches([' ', '\t', ',']);
        let (is_weak, after) = match rest.strip_prefix("W/") {
            Some(after) => (true, after),
            None => (false, rest),
        };
        let Some(inner) = after.strip_prefix('"') else {
            return false;
        };
        let Some(end) = inner.find('"') else {
            return false;
        };
        let quoted = &after[..end + 2];
        if quoted == now && (weak || !is_weak) {
            return true;
        }
        rest = &after[end + 2..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference implementation's vectors (`vectors.h`,
    /// `vectors_sip128`): key `00 .. 0f`, the message `00 01 .. n-1`.
    #[test]
    fn siphash128_holds_to_the_reference_vectors() {
        let want: [[u8; 16]; 3] = [
            [
                0xa3, 0x81, 0x7f, 0x04, 0xba, 0x25, 0xa8, 0xe6, 0x6d, 0xf6, 0x72, 0x14, 0xc7, 0x55,
                0x02, 0x93,
            ],
            [
                0xda, 0x87, 0xc1, 0xd8, 0x6b, 0x99, 0xaf, 0x44, 0x34, 0x76, 0x59, 0x11, 0x9b, 0x22,
                0xfc, 0x45,
            ],
            [
                0x81, 0x77, 0x22, 0x8d, 0xa4, 0xa4, 0x5d, 0xc7, 0xfc, 0xa3, 0x8b, 0xde, 0xf6, 0x0a,
                0xff, 0xe4,
            ],
        ];
        let msg: Vec<u8> = (0..64).collect();
        for (n, want) in want.iter().enumerate() {
            let (a, b) = siphash128(KEY, &msg[..n]);
            let mut got = [0u8; 16];
            got[..8].copy_from_slice(&a.to_le_bytes());
            got[8..].copy_from_slice(&b.to_le_bytes());
            assert_eq!(&got, want, "a message of {n} bytes");
        }
    }

    /// Its rounds are std's SipHash-2-4's, whose 64-bit output differs
    /// from this one by the two constants of the 128-bit variant alone:
    /// with them taken out, every length from 0 to 40 hashes as std does.
    #[test]
    #[allow(deprecated)]
    fn its_rounds_are_siphash24s() {
        use std::hash::Hasher;
        let msg: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(37) ^ 0x5a).collect();
        for n in 0..=msg.len() {
            let mut std = std::hash::SipHasher::new_with_keys(KEY.0, KEY.1);
            std.write(&msg[..n]);
            assert_eq!(siphash64(KEY, &msg[..n]), std.finish(), "{n} bytes");
        }
    }

    /// SipHash-2-4's 64-bit output, from the same rounds.
    fn siphash64((k0, k1): (u64, u64), m: &[u8]) -> u64 {
        let mut v = [
            k0 ^ 0x736f_6d65_7073_6575,
            k1 ^ 0x646f_7261_6e64_6f6d,
            k0 ^ 0x6c79_6765_6e65_7261,
            k1 ^ 0x7465_6462_7974_6573,
        ];
        let mut rest = m;
        while let Some((w, r)) = rest.split_first_chunk::<8>() {
            compress(&mut v, u64::from_le_bytes(*w), 2);
            rest = r;
        }
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        last[7] = m.len() as u8;
        compress(&mut v, u64::from_le_bytes(last), 2);
        v[2] ^= 0xff;
        rounds(&mut v, 4);
        v[0] ^ v[1] ^ v[2] ^ v[3]
    }

    #[test]
    fn a_tag_is_quoted_hex_and_moves_with_every_byte() {
        let row = br#"{"id":1,"n":2}"#;
        let t = tag(row);
        let s = t.as_str();
        assert_eq!(s.len(), 34);
        assert!(s.starts_with('"') && s.ends_with('"'));
        assert!(s[1..33].bytes().all(|b| b.is_ascii_hexdigit()));
        // The hash's two halves, high digit first.
        let (a, b) = siphash128(KEY, row);
        assert_eq!(s, format!("\"{a:016x}{b:016x}\""));
        assert_ne!(t, tag(br#"{"id":1,"n":3}"#));
        assert_eq!(t, tag(row));
    }

    #[test]
    fn lists_compare_as_the_standard_says() {
        let now = Some("\"abc\"");
        assert!(listed("*", now, false));
        assert!(!listed("*", None, false));
        assert!(listed("\"abc\"", now, false));
        assert!(listed(" \"x\", \"abc\" ", now, false));
        assert!(listed("\"x\",\"abc\"", now, false));
        // A weak tag names the row for `If-None-Match` alone.
        assert!(!listed("W/\"abc\"", now, false));
        assert!(listed("W/\"abc\"", now, true));
        // What is no list names nothing.
        assert!(!listed("abc", now, false));
        assert!(!listed("\"abc", now, true));
        assert!(!listed("", now, true));
        assert!(!listed("\"abc\"", None, true));
    }
}
