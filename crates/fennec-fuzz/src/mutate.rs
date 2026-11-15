//! The mutator: an input changed by a few moves at a time -- a bit flipped,
//! a byte or a word set to a value that sits on an edge, bytes inserted,
//! dropped, copied or repeated, a length or a varint made huge or nothing,
//! a token of the target's put in, a run of digits made another number, two
//! inputs spliced -- the moves libFuzzer and AFL make, each a few lines
//! here, so the fuzzer takes no crate (docs/design/conventions.md#fuzzing).

/// xorshift64*: a run is its seed's, so an input a run made is made again.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // splitmix64 of the seed, never zero, which xorshift keeps at zero.
        let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Rng((z ^ (z >> 31)) | 1)
    }

    pub fn draw(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number from 0 to `n - 1`; 0 where `n` is.
    pub fn below(&mut self, n: usize) -> usize {
        match n {
            0 => 0,
            n => (self.draw() % n as u64) as usize,
        }
    }

    pub fn one_in(&mut self, n: u64) -> bool {
        self.draw().is_multiple_of(n)
    }

    pub fn pick<'a, T>(&mut self, of: &'a [T]) -> &'a T {
        &of[self.below(of.len())]
    }
}

/// Integers that sit on an edge: a type's ends and their neighbours, the
/// sizes a reader rounds to, a page's, a power of two either side.
const EDGES: &[u64] = &[
    0,
    1,
    2,
    7,
    8,
    16,
    32,
    63,
    64,
    65,
    100,
    127,
    128,
    255,
    256,
    511,
    512,
    1000,
    1024,
    4096,
    32_767,
    32_768,
    65_535,
    65_536,
    1 << 20,
    16 << 20,
    i32::MAX as u64,
    1 << 31,
    u32::MAX as u64,
    1 << 32,
    (1 << 32) + 1,
    1 << 53,
    (1 << 53) + 1,
    1 << 62,
    i64::MAX as u64,
    1 << 63,
    u64::MAX - 1,
    u64::MAX,
];

/// Numbers as text that sit on an edge of a reader of numbers: the ints'
/// ends, past them, the doubles' and the floats', and spellings no number
/// takes.
const NUMBERS: &[&str] = &[
    "0",
    "-0",
    "1",
    "-1",
    "0.1",
    "1e5",
    "255",
    "256",
    "65536",
    "16777217",
    "2147483648",
    "4294967296",
    "9007199254740993",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "99999999999999999999999999999999999999",
    "1e308",
    "1.7976931348623157e308",
    "1e309",
    "-1e309",
    "4.9e-324",
    "1e-400",
    "0x10",
    "1_000",
    "NaN",
    "Infinity",
    "1e",
    "-",
    ".5",
];

/// An input's moves, and what they draw on.
pub struct Mutator {
    pub rng: Rng,
    /// Tokens the target's text is made of, its own and its seeds': put in
    /// whole, where a byte at a time would take long to spell one.
    pub dictionary: Vec<Vec<u8>>,
    /// The longest input the target is handed.
    pub max_len: usize,
}

impl Mutator {
    pub fn new(seed: u64, dictionary: Vec<Vec<u8>>, max_len: usize) -> Mutator {
        Mutator {
            rng: Rng::new(seed),
            dictionary,
            max_len,
        }
    }

    /// `input` changed by one move or several -- 1, 2, 4 or 8, fewer more
    /// often -- `others` the inputs a splice may take from.
    pub fn mutate(&mut self, input: &mut Vec<u8>, others: &[Vec<u8>]) {
        let moves = 1 << self.rng.below(4);
        for _ in 0..moves {
            self.one(input, others);
            if input.len() > self.max_len {
                input.truncate(self.max_len);
            }
        }
    }

    fn one(&mut self, b: &mut Vec<u8>, others: &[Vec<u8>]) {
        let r = &mut self.rng;
        if b.is_empty() {
            return self.insert(b);
        }
        match r.below(17) {
            0 => {
                let at = r.below(b.len());
                b[at] ^= 1 << r.below(8);
            }
            1 => {
                let at = r.below(b.len());
                b[at] = r.draw() as u8;
            }
            2 => {
                // An edge as a word of 1, 2, 4 or 8 bytes, either end first.
                let width = 1 << r.below(4);
                let v = *r.pick(EDGES);
                let bytes = match r.one_in(2) {
                    true => v.to_le_bytes(),
                    false => v.to_be_bytes(),
                };
                let bytes = match r.one_in(2) {
                    true => &bytes[..width],
                    false => &bytes[8 - width..],
                };
                self.put(b, bytes.to_vec());
            }
            3 => {
                // A small step either way, a byte's or a little-endian word's.
                let at = r.below(b.len());
                let delta = 1 + r.below(35) as u8;
                b[at] = match r.one_in(2) {
                    true => b[at].wrapping_add(delta),
                    false => b[at].wrapping_sub(delta),
                };
            }
            4 => self.insert(b),
            5 => {
                // Bytes dropped: a few, or a long run.
                let at = r.below(b.len());
                let n = 1 + match r.one_in(4) {
                    true => r.below(b.len() - at),
                    false => r.below(8.min(b.len() - at)),
                };
                b.drain(at..at + n.min(b.len() - at));
            }
            6 => {
                // A run copied to another place, over what is there or in.
                let from = r.below(b.len());
                let n = 1 + r.below((b.len() - from).min(64));
                let run = b[from..from + n].to_vec();
                self.put(b, run);
            }
            7 => {
                // A run repeated: a list inside a list inside a list, a
                // header said a thousand times, a bracket ten thousand.
                let from = r.below(b.len());
                let n = 1 + r.below((b.len() - from).min(16));
                let run = b[from..from + n].to_vec();
                let times = match r.below(3) {
                    0 => 2 + r.below(8),
                    1 => 10 + r.below(100),
                    _ => 100 + r.below(10_000),
                };
                let mut many = Vec::with_capacity(run.len() * times);
                for _ in 0..times {
                    many.extend_from_slice(&run);
                    if many.len() > self.max_len {
                        break;
                    }
                }
                let at = r.below(b.len() + 1);
                b.splice(at..at, many);
            }
            8 => {
                // A length as fennecdb writes one (LEB128), on an edge, over
                // the bytes there or in among them -- or a varint that never
                // ends.
                let v = *r.pick(EDGES);
                let mut out = Vec::with_capacity(10);
                match r.one_in(16) {
                    true => out.extend_from_slice(&[0xff; 10]),
                    false => leb128(&mut out, v),
                }
                self.put(b, out);
            }
            9 => {
                // A length as SQLite writes one: big-endian 7 bits a byte,
                // the ninth all 8.
                let v = *r.pick(EDGES);
                self.put(b, sqlite_varint(v));
            }
            10 | 11 if !self.dictionary.is_empty() => {
                let token = r.pick(&self.dictionary).clone();
                match r.one_in(2) {
                    true => {
                        let at = r.below(b.len() + 1);
                        b.splice(at..at, token);
                    }
                    false => self.put(b, token),
                }
            }
            12 if !others.is_empty() => {
                // A splice: this one's start, another's end.
                let other = r.pick(others);
                if !other.is_empty() {
                    let cut = r.below(b.len() + 1);
                    let from = r.below(other.len());
                    b.truncate(cut);
                    b.extend_from_slice(&other[from..]);
                }
            }
            13 => {
                let keep = r.below(b.len());
                b.truncate(keep);
            }
            14 => {
                // A run of digits made another number.
                let at = r.below(b.len());
                let start = (0..=at)
                    .rev()
                    .find(|&i| !b[i].is_ascii_digit())
                    .map_or(0, |i| i + 1);
                let end = (at..b.len())
                    .find(|&i| !b[i].is_ascii_digit())
                    .unwrap_or(b.len());
                let n = r.pick(NUMBERS).as_bytes().to_vec();
                b.splice(start..end.max(start), n);
            }
            15 => {
                // Two runs swapped.
                let n = 1 + r.below(b.len().min(32));
                if b.len() >= 2 * n {
                    let x = r.below(b.len() - 2 * n + 1);
                    let y = x + n + r.below(b.len() - x - 2 * n + 1);
                    for i in 0..n {
                        b.swap(x + i, y + i);
                    }
                }
            }
            _ => {
                // The input's end cut or grown: a body short of its length, a
                // file short of its page.
                match r.one_in(2) {
                    true => {
                        let n = 1 + r.below(b.len());
                        b.truncate(b.len() - n);
                    }
                    false => {
                        let n = 1 + r.below(512);
                        let fill = match r.one_in(2) {
                            true => 0,
                            false => r.draw() as u8,
                        };
                        b.resize(b.len() + n, fill);
                    }
                }
            }
        }
    }

    /// A few bytes in, at random or one byte many times.
    fn insert(&mut self, b: &mut Vec<u8>) {
        let r = &mut self.rng;
        let at = r.below(b.len() + 1);
        let n = 1 + r.below(16);
        let bytes: Vec<u8> = match r.one_in(2) {
            true => (0..n).map(|_| r.draw() as u8).collect(),
            false => vec![r.draw() as u8; n],
        };
        b.splice(at..at, bytes);
    }

    /// `bytes` over those at a place of `b`, or in among them.
    fn put(&mut self, b: &mut Vec<u8>, bytes: Vec<u8>) {
        let r = &mut self.rng;
        let at = r.below(b.len() + 1);
        match r.one_in(2) {
            true => {
                let end = (at + bytes.len()).min(b.len());
                b.splice(at..end, bytes);
            }
            false => {
                b.splice(at..at, bytes);
            }
        }
    }
}

/// `v` as fennecdb's codec writes a length (`codec::put_uvarint`).
pub fn leb128(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// `v` as SQLite writes a varint: big-endian, 7 bits a byte and the
/// continuation in the top bit, the ninth byte all 8.
pub fn sqlite_varint(v: u64) -> Vec<u8> {
    if v >> 56 != 0 {
        let mut out: Vec<u8> = (0..8).map(|i| 0x80 | (v >> (57 - 7 * i)) as u8).collect();
        out.push(v as u8);
        return out;
    }
    let mut out = vec![(v & 0x7f) as u8];
    let mut rest = v >> 7;
    while rest > 0 {
        out.push(0x80 | (rest & 0x7f) as u8);
        rest >>= 7;
    }
    out.reverse();
    out
}

/// The tokens of seeds: their words, numbers and runs of punctuation as
/// text holds them -- what a text target's input is spelled with -- and
/// the printable runs of a binary seed (a table's name, its SQL). At most
/// `most`, the most frequent first.
pub fn tokens_of(seeds: &[Vec<u8>], most: usize) -> Vec<Vec<u8>> {
    let mut counts: std::collections::HashMap<&[u8], usize> = std::collections::HashMap::new();
    for s in seeds {
        let mut i = 0;
        while i < s.len() {
            let c = s[i];
            let kind = |c: u8| match c {
                b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'0'..=b'9' => 1,
                b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~' => 2,
                _ => 0,
            };
            let k = kind(c);
            let start = i;
            while i < s.len() && kind(s[i]) == k && k != 0 {
                i += 1;
            }
            if k == 0 {
                i += 1;
                continue;
            }
            let t = &s[start..i];
            if (2..=32).contains(&t.len()) {
                *counts.entry(t).or_default() += 1;
            }
        }
    }
    let mut all: Vec<(&[u8], usize)> = counts.into_iter().collect();
    all.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    all.into_iter()
        .take(most)
        .map(|(t, _)| t.to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_its_seeds() {
        let make = |seed| {
            let mut m = Mutator::new(seed, vec![b"get".to_vec()], 4096);
            let mut b = b"get docs where n = 1".to_vec();
            for _ in 0..1000 {
                m.mutate(&mut b, &[b"put x {a: 1}".to_vec()]);
                assert!(b.len() <= 4096);
            }
            b
        };
        assert_eq!(make(7), make(7));
        assert_ne!(make(7), make(8));
    }

    #[test]
    fn varints_are_written_as_their_readers_read_them() {
        for v in EDGES {
            let mut b = Vec::new();
            leb128(&mut b, *v);
            assert_eq!(fennec_core::codec::get_uvarint(&b, &mut 0).unwrap(), *v);
        }
        assert_eq!(sqlite_varint(303), [0x82, 0x2f]);
        assert_eq!(sqlite_varint(u64::MAX), [0xff; 9]);
        assert_eq!(sqlite_varint(1 << 56).len(), 9);
    }

    #[test]
    fn tokens_are_the_seeds_words_most_frequent_first() {
        let t = tokens_of(&[b"get a where a = 1; get b".to_vec()], 3);
        assert_eq!(t[0], b"get");
    }
}
