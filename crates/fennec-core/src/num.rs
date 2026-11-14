//! Decimal text to `f64`, without the twelve-kilobyte table -- and floats
//! back to text without the standard library's twenty (`f64_into`, below).
//!
//! `str::parse::<f64>()` is Eisel-Lemire, and Eisel-Lemire is fast because it
//! reads a 751-entry table of 128-bit powers of five. Measured in the wasm
//! build that table is 12 628 bytes -- a twelfth of the whole module, and
//! random enough that neither gzip nor brotli takes anything off it. The
//! algorithm around it is only 469 bytes; the table *is* the cost.
//!
//! So the table goes, and four paths read a number with what the module
//! holds already:
//!
//! * **Fast.** When the mantissa fits in 53 bits and the decimal exponent is
//!   within ±22, both `m` and `10^e` are exact `f64`s, so one IEEE multiply
//!   or divide is already correctly rounded (Clinger). That is nearly every
//!   number a person writes.
//! * **Quotient.** Up to 19 digits over `10^1` to `10^25` are a whole number
//!   divided by `5^n`, to a quotient of 55 bits and a remainder that round
//!   exactly (`divided`). That is what JavaScript writes: seventeen digits
//!   for most floats, every `f32` it widens among them, past Clinger's 53.
//! * **Multiplied.** Any other number's first 19 digits times its power of
//!   five to 125 bits -- Ryu's multipliers, which `f64_into` below writes
//!   with (`pow5`, `inv_pow5`), every bit of the product kept -- give a span
//!   the number lies in, and where both its ends round to one `f64`, that is
//!   the number's (`near`): Eisel and Lemire's algorithm, on the table the
//!   module holds instead of theirs. A span is never wider than 2^-59 of the
//!   number, so only one within that of a midpoint between two `f64`s is
//!   left over: one written with more digits, or now and then a 19-digit one.
//! * **Exact.** That number's every digit, as one integer, is compared with
//!   the midpoint's bits, both brought to one power of ten and of two
//!   (`exact`, `Big`): a few thousand instructions at most.
//!
//! The digits were shifted as a decimal string before, 27 bits a pass, for
//! every number the first two paths did not take: `1e255` cost 137 000
//! instructions where `1e25` cost 211, so a 64 MiB body of them was minutes
//! of a core; and a number past an `f64`'s range read as an infinity, which
//! JSON writes as `null`, or a number not 0 as 0. One is refused now
//! ([`read_f64`]), as an int past its range is.
//!
//! Every path is correctly rounded, and the tests check exactly that against
//! `str::parse::<f64>()`: on the host the reference costs nothing, and only
//! the wasm build pays for the table it replaces.

use std::cmp::Ordering;

/// `10^0` through `10^22` -- every power of ten that is exact in an `f64`.
/// `10^23` is not, which is where the fast path stops.
pub(crate) const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// Digits [`exact`] compares. A midpoint between two `f64`s has at most 768
/// significant digits -- `(2m + 1) × 2^-1075` -- so past these a digit
/// cannot move the comparison but where it is equal, which whether any
/// digit past them is not 0 settles.
const MAX_DIGITS: usize = 800;

/// `s` as an `i64`, as `str::parse` reads one: a sign, then a digit or
/// more, and `None` for anything else or a number past the type. The
/// standard library's was inlined where it was called, a copy of itself at
/// each -- the JSON reader's, the lexer's, a date's -- 1.2 KB of the browser
/// module in the one place it stayed whole; this is one function.
pub fn parse_i64(s: &str) -> Option<i64> {
    // The magnitude of `i64::MIN`; past a tenth of it another digit would
    // take any number out of the type, so nothing below can overflow.
    const MAX: u64 = 1 << 63;
    let (neg, digits) = match s.as_bytes() {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        all => (false, all),
    };
    if digits.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for &c in digits {
        let d = c.wrapping_sub(b'0');
        if d > 9 || n > MAX / 10 {
            return None;
        }
        n = n * 10 + d as u64;
    }
    match neg {
        true if n <= MAX => Some((n as i64).wrapping_neg()),
        false if n < MAX => Some(n as i64),
        _ => None,
    }
}

/// Parses a decimal number. `None` for anything that is not one -- the JSON
/// scanner hands over whatever run of `[0-9.eE+-]` it found, so `1.2.3` and
/// `--1` do arrive here and have to be rejected rather than guessed at --
/// and for a number past an `f64`'s range, which [`read_f64`] tells apart.
/// Inlined, as `read_f64` is: a call of its own around the reader cost a
/// number a dozen instructions.
#[inline]
pub fn parse_f64(s: &str) -> Option<f64> {
    match read(s) {
        (x, None) => Some(x),
        _ => None,
    }
}

/// Why a text reads as no `f64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// Not a decimal number: `1.2.3`, `--1`, `1e`.
    Malformed,
    /// Past the largest `f64`, 1.7976931348623157e308 either side of 0:
    /// `1e400`, which would round to an infinity -- a value JSON writes as
    /// `null`.
    TooLarge,
    /// Not 0, but nearer it than the smallest `f64`, 5e-324: `1e-400`,
    /// which would round to 0.
    TooSmall,
}

/// What a refusal says after a number past the largest `f64`.
const TOO_LARGE: &str =
    "is past a float's range: a float is at most 1.7976931348623157e308 either side of 0";

/// What a refusal says after a number nearer 0 than the smallest `f64`.
const TOO_SMALL: &str = "is past a float's range: a float is 0 or at least 5e-324 either side of 0";

impl Unread {
    /// What a refusal says after the number, for one past a float's range;
    /// `None` for a malformed one, which each reader words its own way.
    pub fn range(self) -> Option<&'static str> {
        match self {
            Unread::Malformed => None,
            Unread::TooLarge => Some(TOO_LARGE),
            Unread::TooSmall => Some(TOO_SMALL),
        }
    }
}

/// Whether a refusal's words `m` are those of a number past a float's
/// range, whichever reader wrote them: they end in the range's
/// ([`Unread::range`]). What a reader of a text handed in passes on as
/// said -- `near`'s text of a vector, an imported json cell -- where its
/// own words for a text it cannot read ("unparseable", "expected JSON")
/// would call a number past the range a malformed text. The end's bytes
/// compared, never the words searched for: a search for a pattern of
/// several bytes brings the string searcher into the browser module (7.8
/// KB, `Error::reason`). The two texts compared as constants: asked of
/// [`Unread::range`] in a closure, it was 142 bytes of the module.
pub fn past_range(m: &str) -> bool {
    m.ends_with(TOO_LARGE) || m.ends_with(TOO_SMALL)
}

/// A number's text as a refusal names it: whole up to 40 characters, and
/// past them its first 40 and its length -- a body of one 64 MiB number is
/// not told back whole.
pub fn shown(text: &str) -> String {
    match text.get(..40) {
        Some(head) if text.len() > 40 => format!("{head}... ({} characters)", text.len()),
        _ => text.to_string(),
    }
}

/// A decimal number as the `f64` nearest it, or why it is none: a text that
/// is no number, or a number past the range -- read as an infinity or as 0
/// it would be another number than the one written, and a limit errors, it
/// does not truncate. A number past the largest `f64` but nearer it than the
/// next power of two's midpoint rounds to it (1.7976931348623158e308), and
/// one nearer 0 than the smallest `f64` but past half of it rounds up to it
/// (3e-324), as rounding to the nearest says.
#[inline]
pub fn read_f64(s: &str) -> Result<f64, Unread> {
    match read(s) {
        (x, None) => Ok(x),
        (_, Some(why)) => Err(why),
    }
}

/// The reading [`read_f64`] and [`parse_f64`] wrap: `(x, None)` for a
/// number, `(0, Some(why))` for none. A pair of two scalars comes back in
/// two registers, where a `Result<f64, Unread>` came back through memory,
/// its tag and number stored and loaded again -- some 5 instructions a
/// JSON document's number, read as main's `Option<f64>` was.
fn read(s: &str) -> (f64, Option<Unread>) {
    let malformed = (0.0, Some(Unread::Malformed));
    let b = s.as_bytes();
    let mut i = 0;

    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };

    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int = &b[int_start..i];

    let frac = if i < b.len() && b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        &b[start..i]
    } else {
        &b[..0]
    };

    // A lone `.`, or `-`, is not a number.
    if int.is_empty() && frac.is_empty() {
        return malformed;
    }

    let mut exp: i64 = 0;
    if i < b.len() && (b[i] | 0x20) == b'e' {
        i += 1;
        let eneg = match b.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return malformed; // `1e`, `1e+`
        }
        // Cut short past 2^40: an exponent that far out decides zero or
        // infinity whatever digits a text in memory holds, settled below
        // before a digit is read. Cut at 100 000 before, `0.` and a million
        // zeros then `1e1000001` -- 1 -- read as an infinity.
        let mut e: i64 = 0;
        for &d in &b[start..i] {
            e = e * 10 + (d - b'0') as i64;
            if e > 1 << 40 {
                break;
            }
        }
        exp = if eneg { -e } else { e };
    }

    if i != b.len() {
        return malformed; // trailing junk: `1.2.3`, `1x`
    }

    match convert(neg, int, frac, exp) {
        Ok(x) => (x, None),
        Err(why) => (0.0, Some(why)),
    }
}

fn convert(neg: bool, int: &[u8], frac: &[u8], exp: i64) -> Result<f64, Unread> {
    // Leading zeros are not significant and would otherwise eat into the
    // 19-digit budget of the fast path: `0.00000001` must still take it.
    let int = {
        let mut k = 0;
        while k < int.len() && int[k] == b'0' {
            k += 1;
        }
        &int[k..]
    };

    // Fast path: accumulate while the mantissa is certainly exact.
    let mut mant: u64 = 0;
    let mut digits: i64 = 0;
    let mut truncated = false;
    let mut e10 = exp;
    for &d in int {
        if digits < 19 {
            mant = mant * 10 + (d - b'0') as u64;
            digits += 1;
        } else {
            truncated = true;
            e10 += 1; // the digit still counts, its value no longer fits
        }
    }
    for &d in frac {
        if digits < 19 {
            // Leading zeros of a pure fraction are not significant either.
            if mant == 0 && d == b'0' {
                e10 -= 1;
                continue;
            }
            mant = mant * 10 + (d - b'0') as u64;
            digits += 1;
            e10 -= 1;
        } else {
            truncated = true;
        }
    }

    if mant == 0 {
        return Ok(if neg { -0.0 } else { 0.0 });
    }

    if !truncated && mant <= (1u64 << 53) {
        let m = mant as f64;
        if (0..=22).contains(&e10) {
            return Ok(sign(neg, m * POW10[e10 as usize]));
        }
        if (-22..0).contains(&e10) {
            return Ok(sign(neg, m / POW10[(-e10) as usize]));
        }
        // A little further up: if the surplus above 10^22 still fits in the
        // mantissa, absorbing it there keeps both operands exact and the
        // result to a single rounding. There is no mirror of this below
        // zero -- two divisions would round twice.
        if (23..=22 + 15).contains(&e10) {
            let spare = POW10[(e10 - 22) as usize] as u64;
            if let Some(scaled) = mant.checked_mul(spare) {
                if scaled <= 1u64 << 53 {
                    return Ok(sign(neg, (scaled as f64) * POW10[22]));
                }
            }
        }
    }
    // Seventeen digits, as JavaScript writes most floats and every `f32`
    // it widens, are past the 53 bits Clinger's path takes, and went the
    // decimal way: a page of 200 768-dim vectors took 74.6 ms to read in the
    // browser module, and takes 51.9 this way.
    if !truncated && (1..STEP as i64).contains(&-e10) {
        return Ok(sign(neg, divided(mant, (-e10) as usize)));
    }

    // The number is from 10^lead up to 10^(lead + 1): from 10^309 it is
    // past the largest `f64`, and under 10^-324 -- half the smallest is
    // 2.47e-324 -- it rounds to 0, both settled before a multiplier is
    // looked up, which leaves `near` the powers from 10^-342 to 10^308.
    let lead = e10 + digits - 1;
    if lead > 308 {
        return Err(Unread::TooLarge);
    }
    if lead < -324 {
        return Err(Unread::TooSmall);
    }
    let bits = match near(mant, e10 as i32, truncated) {
        Ok(bits) => bits,
        Err(below) => exact(int, frac, exp, below),
    };
    // Both readings round the magnitude, so 0 is all bits clear.
    let x = f64::from_bits(bits);
    if bits == 0 {
        Err(Unread::TooSmall)
    } else if x.is_infinite() {
        Err(Unread::TooLarge)
    } else {
        Ok(sign(neg, x))
    }
}

/// `w / 10^n`, correctly rounded, where `5^n` fits in 64 bits: `w × 2^s`
/// divided by `5^n` to a quotient of 55 bits or more, and its remainder,
/// are the value's bits past the 53 kept and whether any below them is set.
fn divided(w: u64, n: usize) -> f64 {
    let d = SMALL[n];
    let bits = |x: u64| 64 - x.leading_zeros() as i32;
    let s = (55 + bits(d) - bits(w)).max(0);
    let num = (w as u128) << s;
    let (q, r) = div128((num >> 64) as u64, num as u64, d);
    let cut = bits(q) - 53;
    let mant = q >> cut;
    let half = 1u64 << (cut - 1);
    let rest = q & (2 * half - 1);
    // To the nearest, and from an exact half to the even one.
    let up = rest > half || (rest == half && (r != 0 || mant & 1 == 1));
    assemble(mant + up as u64, cut - s - n as i32)
}

/// `(hi × 2^64 + lo) / d` and its remainder, where the quotient fits in 64
/// bits (`hi < d`): long division in two 32-bit digits, each guessed from
/// the divisor's top half and brought down at most twice (Knuth's algorithm
/// D, as Hacker's Delight writes it for 128 by 64). Divided as a `u128`, it
/// took the compiler's own 128-bit division into the browser module, 1.4 KB
/// of it; wasm divides 64 bits itself.
fn div128(hi: u64, lo: u64, d: u64) -> (u64, u64) {
    const B: u64 = 1 << 32;
    let k = d.leading_zeros();
    let d = d << k;
    let (d1, d0) = (d >> 32, d & (B - 1));
    let top = match k {
        0 => hi,
        _ => hi << k | lo >> (64 - k),
    };
    let lo = lo << k;
    let digit = |top: u64, next: u64| {
        let (mut q, mut rem) = (top / d1, top % d1);
        while q >= B || q * d0 > (rem << 32 | next) {
            q -= 1;
            rem += d1;
            if rem >= B {
                break;
            }
        }
        q
    };
    let q1 = digit(top, lo >> 32);
    let mid = (top << 32 | lo >> 32).wrapping_sub(q1.wrapping_mul(d));
    let q0 = digit(mid, lo & (B - 1));
    let rem = (mid << 32 | lo & (B - 1)).wrapping_sub(q0.wrapping_mul(d));
    (q1 << 32 | q0, rem >> k)
}

fn sign(neg: bool, v: f64) -> f64 {
    if neg {
        -v
    } else {
        v
    }
}

/// `w × 10^q` rounded to the nearest `f64`, its bits -- an infinity's past
/// the largest, 0's under half the smallest -- where the multipliers tell
/// it: `Err` with the lower of the two it falls between where they cannot,
/// for [`exact`] to choose. `wide` where `w` is the first 19 of more
/// digits, the number above `w × 10^q` and under `(w + 1) × 10^q`; `q`
/// from -342 to 308, which `convert` holds it to.
///
/// The multiplier is Ryu's: `5^q` cut to 125 bits for `q` from 0, all of it
/// to `5^53`, and one more than `2^k / 5^-q` cut below 0. Times `w`, every
/// bit of the product kept, it puts the number in a span of `w` -- of
/// `w + t` where digits were cut -- in the product's last place, never
/// wider than 2^-59 of it: where both ends round to one `f64`, so does the
/// number, and only one within that of a midpoint is left to `exact`.
/// Eisel and Lemire read the same product from a 128-bit table of every
/// power -- 12 KB the module does not hold -- and argue where its error
/// could cross a rounding; this table is the 0.8 KB `f64_into` holds
/// anyway, and the span says itself whether it crosses one.
///
/// Never inlined into the reader (`read`): there its registers were the digit
/// loops', which then spilled -- a number Clinger's path takes cost up to
/// 20 instructions more.
#[inline(never)]
fn near(w: u64, q: i32, wide: bool) -> Result<u64, u64> {
    let (t, e2) = match q >= 0 {
        true => (pow5(q as u32), q + pow5_bits(q) - BITS),
        false => (inv_pow5(-q as u32), q - pow5_bits(-q) + 1 - BITS),
    };
    let (a, b) = (w as u128 * (t >> 64), w as u128 * (t as u64) as u128);
    let p = (a + (b >> 64), b as u64);
    let add = |(hi, lo): (u128, u64), x: u128| {
        let (lo, carry) = lo.overflowing_add(x as u64);
        (hi + (x >> 64) + carry as u128, lo)
    };
    let (lo, hi) = match q >= 0 {
        true if q <= 53 && !wide => (p, p),
        true => (p, add(p, w as u128 + if wide { t + 1 } else { 0 })),
        false => {
            let (low, borrow) = p.1.overflowing_sub(w);
            (
                (p.0 - borrow as u128, low),
                if wide { add(p, t) } else { p },
            )
        }
    };
    let below = rounded(lo, e2);
    match hi == lo || rounded(hi, e2) == below {
        true => Ok(below),
        false => Err(below),
    }
}

/// `(hi × 2^64 + lo) × 2^e2` rounded to the nearest `f64`, an exact half to
/// the even one: its bits, an infinity's past the largest. `hi` is at least
/// 2^59, so every bit rounding cuts above `lo` is in `hi`.
fn rounded((hi, lo): (u128, u64), e2: i32) -> u64 {
    let n = 192 - hi.leading_zeros() as i32;
    // 53 bits kept, fewer under 2^-1022, where each is worth 2^-1074.
    let keep = (n + e2 + 1074).min(53);
    if keep < 1 {
        // Under 2^-1074: past half of it rounds up to it, half of it to 0.
        return (keep == 0 && (lo != 0 || !hi.is_power_of_two())) as u64;
    }
    let cut = (n - 64 - keep) as u32;
    let mant = (hi >> cut) as u64;
    let rest = hi & ((1u128 << cut) - 1);
    let half = 1u128 << (cut - 1);
    let up = rest > half || rest == half && (lo != 0 || mant & 1 == 1);
    assemble(mant + up as u64, n - keep + e2).to_bits()
}

/// Where [`near`]'s span holds the midpoint between `below` and the `f64`
/// after it, which of the two the number written rounds to -- the even one
/// where it is the midpoint: its digits as one integer, `x × 10^q`, against
/// the midpoint's `(2m + 1) × 2^(e - 1)`, each multiplied by the power of
/// five and of two the other holds, and compared whole: 800 digits, or a
/// midpoint by 5^1123, some 2 660 bits at most -- some thousands of
/// instructions, where shifting the digits took up to 183 000. Cold: only
/// a number within 2^-59 of a midpoint comes here.
#[cold]
#[inline(never)]
fn exact(int: &[u8], frac: &[u8], exp: i64, below: u64) -> u64 {
    #[cfg(test)]
    tests::EXACT.with(|n| n.set(n.get() + 1));
    // Past `MAX_DIGITS`, a digit only says whether the number is above the
    // integer the ones before make (`tail`). Nine digits a word.
    let mut x = Big::of(0);
    let (mut word, mut len, mut kept) = (0u32, 0u32, 0usize);
    let (mut dropped, mut tail) = (0i64, false);
    for &c in int.iter().chain(frac) {
        let d = (c - b'0') as u32;
        if kept == 0 && d == 0 {
            continue;
        }
        if kept == MAX_DIGITS {
            dropped += 1;
            tail |= d != 0;
            continue;
        }
        (word, len, kept) = (word * 10 + d, len + 1, kept + 1);
        if len == 9 {
            x.mul_add(1_000_000_000, word);
            (word, len) = (0, 0);
        }
    }
    x.mul_add(10u32.pow(len), word);
    let q = exp - frac.len() as i64 + dropped;
    let field = (below >> 52) as i64;
    let m = below & ((1 << 52) - 1) | ((field > 0) as u64) << 52;
    let e = field.max(1) - 1075;
    let mut y = Big::of(2 * m + 1);
    match q >= 0 {
        true => x.mul_pow5(q as u64),
        false => y.mul_pow5(-q as u64),
    }
    match e - 1 - q {
        s if s >= 0 => y.shl(s as u64),
        s => x.shl(-s as u64),
    }
    let tail = match tail {
        true => Ordering::Greater,
        false => Ordering::Equal,
    };
    match x.against(&y).then(tail) {
        Ordering::Less => below,
        Ordering::Greater => below + 1,
        Ordering::Equal => below + (below & 1),
    }
}

/// The words a [`Big`] holds: 2 816 bits, where [`exact`]'s widest number
/// takes some 2 660 -- 800 digits, or a midpoint times `5^1123`.
const WORDS: usize = 88;

/// A whole number for [`exact`], in 32-bit words, the lowest first, `n` of
/// them used and the last of those not 0. Of 32 bits, so the browser
/// module multiplies them itself: a 64-bit word's product is a call to its
/// 128-bit multiply there.
struct Big {
    w: [u32; WORDS],
    n: usize,
}

impl Big {
    fn of(v: u64) -> Big {
        let mut b = Big {
            w: [0; WORDS],
            n: 0,
        };
        b.mul_add(1, v as u32);
        if v >> 32 != 0 {
            b.w[1] = (v >> 32) as u32;
            b.n = 2;
        }
        b
    }

    /// `self × m + a`.
    fn mul_add(&mut self, m: u32, a: u32) {
        let mut carry = a as u64;
        for x in &mut self.w[..self.n] {
            let t = *x as u64 * m as u64 + carry;
            *x = t as u32;
            carry = t >> 32;
        }
        if carry != 0 {
            self.w[self.n] = carry as u32;
            self.n += 1;
        }
    }

    /// `self × 5^k`, `5^13` a step: the most a word holds.
    fn mul_pow5(&mut self, mut k: u64) {
        while k >= 13 {
            self.mul_add(SMALL[13] as u32, 0);
            k -= 13;
        }
        self.mul_add(SMALL[k as usize] as u32, 0);
    }

    /// `self × 2^k`.
    fn shl(&mut self, k: u64) {
        self.mul_add(1 << (k % 32), 0);
        let words = (k / 32) as usize;
        self.w.copy_within(..self.n, words);
        self.w[..words].fill(0);
        self.n += words;
    }

    /// A loop, not `iter().rev().cmp(..)`: the iterators' copies were 278
    /// bytes of the browser module.
    fn against(&self, other: &Big) -> Ordering {
        if self.n != other.n {
            return self.n.cmp(&other.n);
        }
        let mut i = self.n;
        while i > 0 {
            i -= 1;
            if self.w[i] != other.w[i] {
                return self.w[i].cmp(&other.w[i]);
            }
        }
        Ordering::Equal
    }
}

/// `mant * 2^e2` as an `f64`, built from the bits so no second rounding can
/// creep in on the way.
fn assemble(mant: u64, e2: i32) -> f64 {
    let (mut mant, mut e2) = (mant, e2);
    if mant == 0 {
        return 0.0;
    }
    // Rounding can carry out of 53 bits, and only from an even mantissa.
    if mant >= 1u64 << 53 {
        mant >>= 1;
        e2 += 1;
    }
    if mant >= 1u64 << 52 {
        let field = e2 + 52 + 1023;
        if field >= 0x7ff {
            return f64::INFINITY;
        }
        debug_assert!(field > 0);
        f64::from_bits(((field as u64) << 52) | (mant & ((1u64 << 52) - 1)))
    } else {
        // Subnormal: the exponent field is zero and the value is `mant`
        // scaled by 2^-1074, which is exactly what these bits mean.
        debug_assert_eq!(e2, -1074);
        f64::from_bits(mant)
    }
}

// ------------------------------------------------------------ and back to text

/// `x` as `{}` writes it -- the shortest digits that read back as `x`, in
/// plain decimal, `-0`, `NaN`, `inf` -- without the standard library's
/// formatting: Grisu with Dragon behind it, and Dragon's bignum, were 20.8 KB
/// of the browser module (`make size-report`), 4.6% of its code.
///
/// The digits are Ryu's (Adams, PLDI 2018) with one change. An exact half
/// -- `1125899906842624.25`, whose neighbours `.2` and `.3` are equally near
/// and both read back as it -- Ryu rounds to even and `{}` rounds up, and
/// what a query answered before has to be what it answers now, byte for byte.
pub fn f64_into(out: &mut String, x: f64) {
    float_into(out, x.to_bits(), 52, 11);
}

/// `x` as `{}` writes an `f32`: the shortest digits that read back as the
/// `f32`, not as the `f64` it widens to (`0.1`, not `0.10000000149011612`).
/// It goes through the same code as `f64_into`, with the interval an `f32`
/// stands for: the `f64` multipliers carry more precision than it needs, and
/// the tests hold every one of the 2^32 to `{}`.
pub fn f32_into(out: &mut String, x: f32) {
    float_into(out, x.to_bits() as u64, 23, 8);
}

fn float_into(out: &mut String, bits: u64, mbits: u32, ebits: u32) {
    let mantissa = bits & ((1 << mbits) - 1);
    let exponent = (bits >> mbits) as u32 & ((1 << ebits) - 1);
    let negative = bits >> (mbits + ebits) != 0;
    if exponent == (1 << ebits) - 1 {
        out.push_str(match (mantissa != 0, negative) {
            (true, _) => "NaN",
            (false, true) => "-inf",
            (false, false) => "inf",
        });
        return;
    }
    if negative {
        out.push('-');
    }
    if exponent == 0 && mantissa == 0 {
        out.push('0');
        return;
    }
    let (digits, exp) = shortest(mantissa, exponent, mbits, ebits);
    let mut text = [0u8; 20];
    let mut n = text.len();
    let mut d = digits;
    while d > 0 {
        n -= 1;
        text[n] = b'0' + (d % 10) as u8;
        d /= 10;
    }
    let text = &text[n..];
    // `{}` never writes an exponent: the digits before the point, then the
    // rest, with zeros wherever the exponent reaches past them.
    let point = text.len() as i32 + exp;
    // Pushed a `char` at a time, encoding each was 8% of the time a page of
    // vectors took to write, and checking the digits were UTF-8 another 6%.
    let push = |out: &mut String, digits: &[u8]| {
        // SAFETY: every byte is `b'0'` and a digit, ASCII.
        out.push_str(unsafe { std::str::from_utf8_unchecked(digits) })
    };
    if point <= 0 {
        out.push_str("0.");
        (point..0).for_each(|_| out.push('0'));
        push(out, text);
    } else if (point as usize) < text.len() {
        push(out, &text[..point as usize]);
        out.push('.');
        push(out, &text[point as usize..]);
    } else {
        push(out, text);
        (text.len() as i32..point).for_each(|_| out.push('0'));
    }
}

/// The multipliers carry this many bits of `5^i` and of `2^k / 5^i`.
const BITS: i32 = 125;

/// `(digits, e)`: the shortest decimal `digits × 10^e` inside the interval of
/// values that round to this float, the nearest to it when there are several
/// -- Ryu's d2d, for any width of float up to an `f64`.
fn shortest(mantissa: u64, exponent: u32, mbits: u32, ebits: u32) -> (u64, i32) {
    let bias = (1 << (ebits - 1)) - 1;
    // Two more bits, so the interval's bounds are whole numbers too.
    let (e2, m2) = match exponent {
        0 => (1 - bias - mbits as i32 - 2, mantissa),
        _ => (
            exponent as i32 - bias - mbits as i32 - 2,
            (1 << mbits) | mantissa,
        ),
    };
    // An even mantissa rounds to itself from a tie, so its bounds belong to it.
    let accept = m2 & 1 == 0;
    let mv = 4 * m2;
    // The interval is narrower below a power of two, whose lower neighbour
    // is half as far away.
    let mm_shift = (mantissa != 0 || exponent <= 1) as u64;
    let (mm, mp) = (mv - 1 - mm_shift, mv + 2);

    let (mut vr, mut vp, mut vm, e10);
    let mut vm_zeros = false;
    if e2 >= 0 {
        let q = log10_pow2(e2) - (e2 > 3) as i32;
        e10 = q;
        let mul = inv_pow5(q as u32);
        let j = -e2 + q + BITS + pow5_bits(q) - 1;
        (vr, vp, vm) = (
            mul_shift(mv, mul, j),
            mul_shift(mp, mul, j),
            mul_shift(mm, mul, j),
        );
        // Whether the digits cut from the lower bound are all zeros, which
        // decides if that bound is reachable, and an exact upper bound the
        // float does not own comes in by one. One at most of `mm`, `mv` and
        // `mp` is a multiple of 5; Ryu asks only up to q = 21.
        if q <= 21 && !mv.is_multiple_of(5) {
            if accept {
                vm_zeros = pow5_factor(mm) >= q as u32;
            } else {
                vp -= (pow5_factor(mp) >= q as u32) as u64;
            }
        }
    } else {
        let q = log10_pow5(-e2) - (-e2 > 1) as i32;
        e10 = q + e2;
        let i = -e2 - q;
        // `m × 5^i / 2^q`, which the multipliers only approximate -- closely
        // enough that the quotients are exact. Where `mp × 5^i` fits in 64
        // bits they are taken exactly instead: every f32 from 2^-24 to 2^25,
        // an embedding's components, whose 128-bit products wasm emulates.
        if i < STEP as i32 && 64 - mp.leading_zeros() as i32 + pow5_bits(i) <= 64 {
            let p = SMALL[i as usize];
            (vr, vp, vm) = ((mv * p) >> q, (mp * p) >> q, (mm * p) >> q);
        } else {
            let mul = pow5(i as u32);
            let j = q - (pow5_bits(i) - BITS);
            (vr, vp, vm) = (
                mul_shift(mv, mul, j),
                mul_shift(mp, mul, j),
                mul_shift(mm, mul, j),
            );
        }
        if q <= 1 {
            if accept {
                vm_zeros = mm_shift == 1;
            } else {
                vp -= 1;
            }
        }
    }

    // Digits go while the bounds still differ above them; the last one cut
    // from `vr` rounds it.
    let (mut removed, mut last) = (0, 0);
    while vp / 10 > vm / 10 {
        vm_zeros &= vm.is_multiple_of(10);
        last = vr % 10;
        (vr, vp, vm) = (vr / 10, vp / 10, vm / 10);
        removed += 1;
    }
    if vm_zeros {
        while vm.is_multiple_of(10) {
            last = vr % 10;
            (vr, vp, vm) = (vr / 10, vp / 10, vm / 10);
            removed += 1;
        }
    }
    // One up when `vr` is a bound the float does not own, or from a half or
    // more: an exact half too, where Ryu would go to the even neighbour.
    let up = (vr == vm && (!accept || !vm_zeros)) || last >= 5;
    (vr + up as u64, e10 + removed)
}

/// `⌊m × mul / 2^j⌋`, `j` at least 64: the product's low 64 bits never
/// reach it.
fn mul_shift(m: u64, mul: u128, j: i32) -> u64 {
    let low = m as u128 * (mul as u64) as u128;
    let high = m as u128 * (mul >> 64);
    (((low >> 64) + high) >> (j - 64)) as u64
}

/// `⌈log2 5^e⌉`, 1 for `e = 0`.
fn pow5_bits(e: i32) -> i32 {
    ((e as u32 * 1_217_359) >> 19) as i32 + 1
}

/// `⌊log10 2^e⌋`.
fn log10_pow2(e: i32) -> i32 {
    ((e as u32 * 78_913) >> 18) as i32
}

/// `⌊log10 5^e⌋`.
fn log10_pow5(e: i32) -> i32 {
    ((e as u32 * 732_923) >> 20) as i32
}

fn pow5_factor(mut v: u64) -> u32 {
    let mut n = 0;
    while v.is_multiple_of(5) {
        v /= 5;
        n += 1;
    }
    n
}

/// Every 26th multiplier is kept whole, and the rest are one of those times
/// `5^1` to `5^25`, plus a correction of 0 to 3 in the last place: two bits a
/// power, sixteen a word. The full tables are 10.4 KB; these are 0.8.
const STEP: u32 = 26;

/// `5^i` for `i` below `STEP`.
const SMALL: [u64; STEP as usize] = {
    let mut t = [1; STEP as usize];
    let mut i = 1;
    while i < t.len() {
        t[i] = t[i - 1] * 5;
        i += 1;
    }
    t
};

/// `5^(26b)`, its top 125 bits: `5^i >> (⌈log2 5^i⌉ - 125)`.
const POW5: [u128; 13] = [
    0x10000000000000000000000000000000,
    0x14adf4b7320334b90000000000000000,
    0x1aba4714957d300d0e549208b31adb10,
    0x1145b7e285bf98f56dc6ad264d8f0866,
    0x1652efdc6018a1fceb1dbd923d8596ca,
    0x1cda62055b2d9d83b4c1b80b22ae923c,
    0x12a5568b9f52f4165bb28b4e8f7e4c30,
    0x1819651531f9e78ff08aed437682d4fb,
    0x1f25c186a6f04c28b4ee134ad99bf150,
    0x1420eb449c8842e616499ecb70c25f03,
    0x1a03fde214caf08585a56ead360865b0,
    0x10cfeb353a97dad8093db1d57999890b,
    0x15baaf44fa52673ecf38bb735e3f36ac,
];

const POW5_FIX: [u32; 21] = [
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x40000000, 0x59695995, 0x55545555, 0x56555515,
    0x41150504, 0x40555410, 0x44555145, 0x44504540, 0x45555550, 0x40004000, 0x96440440, 0x55565565,
    0x54454045, 0x40154151, 0x55559155, 0x51405555, 0x00000105,
];

/// `5^-(26b)` as Ryu multiplies by it: `⌊2^(⌈log2 5^i⌉ - 1 + 125) / 5^i⌋ + 1`.
/// To `5^-364`: Ryu reaches `5^-290`, an `f64`'s largest, and reading a
/// number `5^-342`, its smallest subnormal's under 19 digits (`near`).
const POW5_INV: [u128; 15] = [
    0x20000000000000000000000000000001,
    0x18c240c4aecb13bb52a6c95fc0655034,
    0x1327fc58da0f6ff57ca8d50071dfc806,
    0x1da48ce468e7c7026520247d3556476e,
    0x16ef5b40c2fc77796139cdd76802e6e9,
    0x11bebdf578b2f391f951a7ff43de8c79,
    0x1b758d848fac54b07be8bee8d6e957e8,
    0x153eda614071a3b78bd3f9e999a423ea,
    0x10701bd527b4978c0848f973cb3ee3ce,
    0x196fbb9bb44db44d153285ebb9efbfa2,
    0x13ae3591f5b4d936adeee7f86c07b696,
    0x1e74404f3daada914d686a4eaf182222,
    0x17900ea4fda7c25798c0a106e09ebd9f,
    0x123b140576d820b28f20e37371497d0e,
    0x1c35f4275f7a29adb043138134743d85,
];

const POW5_INV_FIX: [u32; 22] = [
    0x54544554, 0x04055545, 0x10041000, 0x00400414, 0x40010000, 0x41155555, 0x00000454, 0x00010044,
    0x40000000, 0x44000041, 0x50454450, 0x55550054, 0x51655554, 0x40004000, 0x01000001, 0x00010500,
    0x51515411, 0x05555554, 0x50411500, 0x40040000, 0x05040110, 0x00000000,
];

fn fix(table: &[u32], i: u32) -> u128 {
    ((table[i as usize / 16] >> (i % 16 * 2)) & 3) as u128
}

/// The multiplier for `5^i`, `i` up to 325 (an `f64`'s smallest subnormal).
fn pow5(i: u32) -> u128 {
    let base = i / STEP;
    let (mul, off) = (POW5[base as usize], i - base * STEP);
    if off == 0 {
        return mul;
    }
    let m = SMALL[off as usize] as u128;
    let shift = pow5_bits(i as i32) - pow5_bits((base * STEP) as i32);
    let (low, high) = (m * (mul as u64) as u128, m * (mul >> 64));
    (low >> shift) + (high << (64 - shift)) + fix(&POW5_FIX, i)
}

/// The multiplier for `5^-i`, `i` up to 342: 290 for an `f64`'s largest
/// written, 342 for the smallest read (`near`).
fn inv_pow5(i: u32) -> u128 {
    let base = i.div_ceil(STEP);
    let (mul, off) = (POW5_INV[base as usize], base * STEP - i);
    if off == 0 {
        return mul;
    }
    let m = SMALL[off as usize] as u128;
    let shift = pow5_bits((base * STEP) as i32) - pow5_bits(i as i32);
    let (low, high) = (m * (mul as u64 - 1) as u128, m * (mul >> 64));
    (low >> shift) + (high << (64 - shift)) + 1 + fix(&POW5_INV_FIX, i)
}

/// The number at `j` as the general path reads it -- a float through
/// `num::parse_f64`, an integer through `i64` -- when it reads in the one
/// pass that finds its end: `-`, digits, a fraction, an exponent, and up to
/// 19 significant digits of no more than 2^53 over a power of ten an `f64`
/// holds exactly, which Clinger's path rounds with one multiply or divide,
/// as `parse_f64` does -- or of more, over a power short of 10^26, which
/// [`divided`] rounds as `parse_f64` does. That is every component of a
/// vector's text but the widest. `None`, and `j` where it was, for anything
/// else. The JSON reader
/// reads a vector's numbers with it, and FennecQL's lexer a list of them.
/// `float` is set where the number is written with a fraction or an
/// exponent, as its loop asks of a list of whole `f32`s ([`hides_rounding`]):
/// the flag the parse already keeps, where a test of each `f64` read cost a
/// number its instructions.
#[cfg(not(target_arch = "wasm32"))]
#[inline(always)]
pub fn clinger(b: &[u8], j: &mut usize, float_seen: &mut bool) -> Option<f64> {
    let mut k = *j;
    let neg = b.get(k) == Some(&b'-');
    k += neg as usize;
    // Leading zeros are not significant, and add nothing to `m`.
    let (mut m, mut sig, mut e10) = (0u64, 0u32, 0i32);
    let mut digits = |k: &mut usize, frac: bool| {
        let start = *k;
        while let Some(&d @ b'0'..=b'9') = b.get(*k) {
            sig += (m != 0 || d != b'0') as u32;
            m = m.wrapping_mul(10).wrapping_add((d - b'0') as u64);
            e10 -= frac as i32;
            *k += 1;
        }
        *k > start
    };
    if !digits(&mut k, false) {
        return None;
    }
    let mut float = false;
    if b.get(k) == Some(&b'.') {
        k += 1;
        if !digits(&mut k, true) {
            return None;
        }
        float = true;
    }
    if let Some(b'e' | b'E') = b.get(k) {
        k += 1;
        let eneg = b.get(k) == Some(&b'-');
        k += matches!(b.get(k), Some(b'-' | b'+')) as usize;
        let start = k;
        let mut e = 0i32;
        while let Some(&d @ b'0'..=b'9') = b.get(k) {
            e = e * 10 + (d - b'0') as i32;
            k += 1;
            if k - start > 3 {
                return None;
            }
        }
        if k == start {
            return None;
        }
        e10 += if eneg { -e } else { e };
        float = true;
    }
    // Past 19 digits `m` may have wrapped; and where the general path's
    // scan would go on, it is the one to read what follows.
    if sig > 19
        || matches!(
            b.get(k),
            Some(b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E')
        )
    {
        return None;
    }
    let x = if m == 0 {
        0.0
    } else if m > 1 << 53 {
        // Seventeen digits, as JavaScript writes most floats, are past
        // Clinger's 53 bits: a fraction over a power short of 10^26 is
        // `divided` here, as `parse_f64` divides it, rather than its text
        // scanned twice more on the general path to get there -- 250
        // instructions a number of a `[float]`, read into a vector or as
        // written.
        match float && (1..STEP as i32).contains(&-e10) {
            true => divided(m, (-e10) as usize),
            false => return None,
        }
    } else if (0..=22).contains(&e10) {
        m as f64 * POW10[e10 as usize]
    } else if (-22..0).contains(&e10) {
        m as f64 / POW10[-e10 as usize]
    } else {
        return None;
    };
    *j = k;
    *float_seen |= float;
    // An integer is an `i64`, which has no -0.
    Some(if neg && (float || m != 0) { -x } else { x })
}

/// Whether `list`, the text of a list of numbers alone from its `[` to its
/// `]`, holds one written with a fraction or an exponent that the `f32` it
/// was read into, `v` -- each whole and short of 2^24
/// ([`crate::value::whole_f32s`]) -- is not: `[1.00000001]` read as `[1]`,
/// which a list of numbers would take as the integer written. Each number
/// read again exactly; asked only of a list so read that held such a
/// number, which no embedding is (its first `f32` is not whole), so it
/// costs the quick read nothing. A list that does not split as it read is
/// taken to hide one.
pub fn hides_rounding(list: &str, v: &[f32]) -> bool {
    // Its brackets cut by place: `trim_matches` was 324 bytes of the
    // browser module.
    let inner = list
        .get(1..list.len().saturating_sub(1))
        .unwrap_or_default();
    let mut n = 0;
    for part in inner.split(',') {
        let x = parse_f64(part.trim());
        match (x, v.get(n)) {
            (Some(x), Some(&y)) if y as f64 == x => n += 1,
            _ => return true,
        }
    }
    n != v.len()
}

#[cfg(test)]
mod tests {
    /// Clinger's path, and the division it takes past 53 bits, reads a
    /// number as `parse_f64` does, to the bit: up to 19 digits each side of
    /// the point, every exponent near the division's 10^-26 and Clinger's
    /// 10^±22, signed and not, and the integers it leaves to the general
    /// path.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn clinger_reads_a_number_as_parse_f64_does() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        let (mut read, mut divided) = (0, 0);
        for _ in 0..300_000 {
            let mut t = String::new();
            if rnd(2) == 0 {
                t.push('-');
            }
            // A leading zero now and then, as JSON refuses and FennecQL reads.
            let int = rnd(20);
            for _ in 0..int.max(1) {
                t.push(char::from(b'0' + rnd(10) as u8));
            }
            if rnd(5) > 0 {
                t.push('.');
                for _ in 0..1 + rnd(20) {
                    t.push(char::from(b'0' + rnd(10) as u8));
                }
            }
            if rnd(4) == 0 {
                t.push('e');
                t.push_str(&format!("{}", rnd(60) as i64 - 30));
            }
            let mut j = 0;
            let mut float = false;
            if let Some(v) = super::clinger(t.as_bytes(), &mut j, &mut float) {
                assert_eq!(j, t.len(), "{t}");
                let want = match float {
                    true => super::parse_f64(&t).unwrap(),
                    false => super::parse_i64(&t).unwrap() as f64,
                };
                assert_eq!(v.to_bits(), want.to_bits(), "{t}");
                read += 1;
                // Past 2^53, which only a 16-digit mantissa or longer is.
                let mant = t
                    .split('e')
                    .next()
                    .unwrap_or_default()
                    .replace(['-', '.'], "");
                divided +=
                    (mant.trim_start_matches('0').parse::<u128>().unwrap_or(0) > 1 << 53) as u32;
            }
        }
        // Both ways were taken, many times over.
        assert!(read > 100_000 && divided > 10_000, "{read} {divided}");
    }

    #[test]
    fn an_i64_reads_as_str_parse_reads_it() {
        let mut cases: Vec<String> = [
            "",
            "+",
            "-",
            "0",
            "-0",
            "+0",
            "007",
            "-007",
            "1_0",
            " 1",
            "1 ",
            "1.0",
            "1e3",
            "9223372036854775807",
            "9223372036854775808",
            "-9223372036854775808",
            "-9223372036854775809",
            "922337203685477580",
            "9223372036854775799",
            "00000000000000000000000000000001",
            "-00000000000000000000009223372036854775808",
            "18446744073709551615",
            "18446744073709551616",
            "99999999999999999999",
            "--1",
            "+-1",
            "٣",
            "1a",
            "a1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let n = x as i64 >> (x % 64);
            cases.push(n.to_string());
            cases.push(format!("+{}", n.unsigned_abs()));
            cases.push((x >> (x % 64)).to_string());
        }
        for c in &cases {
            assert_eq!(parse_i64(c), c.parse::<i64>().ok(), "{c:?}");
        }
    }

    use super::*;

    thread_local! {
        /// How many numbers this thread's reading took to [`exact`]: a test
        /// holds itself to reaching it.
        pub(super) static EXACT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn exacts() -> usize {
        EXACT.with(|n| n.get())
    }

    /// Bit-for-bit, so a mismatched sign on a zero or a one-ulp slip in the
    /// last place both fail rather than comparing equal -- and refused where
    /// the reference reads an infinity, or 0 of a number that is not.
    #[track_caller]
    fn same(text: &str) {
        let want: f64 = text.parse().expect("the reference should parse this");
        let mantissa = text.split(['e', 'E']).next().unwrap_or_default();
        let zero = !mantissa.bytes().any(|c| (b'1'..=b'9').contains(&c));
        let got = read_f64(text);
        let range = match want {
            w if w.is_infinite() => Some(Unread::TooLarge),
            w if w == 0.0 && !zero => Some(Unread::TooSmall),
            _ => None,
        };
        if let Some(range) = range {
            assert_eq!(got, Err(range), "{text}");
            assert_eq!(parse_f64(text), None, "{text}");
            return;
        }
        let got = got.expect("read_f64 should read this");
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "{text}: got {got:e} ({:#x}), want {want:e} ({:#x})",
            got.to_bits(),
            want.to_bits()
        );
    }

    /// `m × 2^e` written out whole -- every digit, `5^-e` times `m` over
    /// `10^-e` below 1 -- as a midpoint between two `f64`s is written.
    fn written(m: u64, e: i32) -> String {
        let mut d: Vec<u8> = m.to_string().bytes().rev().map(|c| c - b'0').collect();
        let (by, times) = if e >= 0 { (2, e) } else { (5, -e) };
        for _ in 0..times {
            let mut carry = 0;
            for x in d.iter_mut() {
                let t = *x * by + carry;
                (*x, carry) = (t % 10, t / 10);
            }
            if carry > 0 {
                d.push(carry);
            }
        }
        let mut s: String = d.iter().rev().map(|&x| (b'0' + x) as char).collect();
        if e < 0 {
            s.push_str(&format!("e{e}"));
        }
        s
    }

    /// The midpoint between `x`, positive and finite, and the `f64` above
    /// it, as `(2m + 1) × 2^(e - 1)`.
    fn midpoint(x: f64) -> (u64, i32) {
        let bits = x.to_bits();
        let field = (bits >> 52) as i32;
        let m = bits & ((1 << 52) - 1) | ((field > 0) as u64) << 52;
        (2 * m + 1, field.max(1) - 1075 - 1)
    }

    /// Both fuzz tests below run this many rounds, so 200 000 comparisons
    /// against `str::parse` on every `make test`. Raising it is the way to
    /// search harder; it was run at 2 000 000 while this was written, and the
    /// seeds are fixed so a failure is reproducible.
    const SAMPLES: usize = 100_000;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // xorshift64*, enough to spray the input space and reproducible
            // from a fixed seed when something does fail.
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
    }

    #[test]
    fn the_fast_path_is_exact() {
        for text in [
            "0",
            "-0",
            "1",
            "-1",
            "1.5",
            "0.1",
            "0.2",
            "0.3",
            "3.14159265358979",
            "1e10",
            "1e-10",
            "1e22",
            "1e-22",
            "123456789.123456789",
            "-0.04729",
            "9007199254740992",
            "0.00000001",
            "0.000000000000000000001",
            "2",
            "1024",
            "6.02e23",
        ] {
            same(text);
        }
    }

    #[test]
    fn the_slow_path_is_exact() {
        for text in [
            // Boundaries and the classic torture cases.
            "2.2250738585072011e-308", // the PHP hang
            "2.2250738585072014e-308", // smallest normal
            "4.9406564584124654e-324", // smallest subnormal
            "1e-323",
            "1.7976931348623157e308", // largest finite
            "1.7976931348623159e308", // just over -> infinity
            "1e309",
            "1e-400",
            "9007199254740993", // 2^53 + 1, not representable
            "9007199254740995",
            "1e23", // first power of ten off the fast path
            "1e-23",
            "8.98846567431158e307",
            "0.500000000000000166533453693773481063544750213623046875",
            "3.518437208883201171875e13",
            "1.0000000000000000000000000000000000000000001",
            "123456789012345678901234567890e-15",
        ] {
            same(text);
        }
    }

    #[test]
    fn every_float_survives_a_round_trip() {
        // `{}` and `{:e}` are shortest-round-trip, so re-parsing has to give
        // back the very same bits. Random bit patterns cover subnormals, the
        // exponent extremes and everything between far better than a list.
        let mut rng = Rng(0x5eed_1234_9abc_def0);
        let mut checked = 0;
        for _ in 0..SAMPLES {
            let bits = rng.next();
            let f = f64::from_bits(bits);
            if !f.is_finite() {
                continue;
            }
            same(&format!("{f}"));
            same(&format!("{f:e}"));
            checked += 1;
        }
        assert!(
            checked * 4 > SAMPLES * 3,
            "too few finite samples: {checked}"
        );
    }

    #[test]
    fn random_decimal_text_agrees_with_the_reference() {
        // Built from digits rather than from floats, so the input is not
        // biased towards numbers that happen to be representable.
        let mut rng = Rng(0xfeed_face_0bad_c0de);
        for _ in 0..SAMPLES {
            let ndigits = 1 + (rng.next() % 25) as usize;
            let mut s = String::new();
            if rng.next() & 1 == 0 {
                s.push('-');
            }
            for k in 0..ndigits {
                let d = (rng.next() % 10) as u8;
                // No leading zero, so the digit count means what it says.
                s.push(if k == 0 && d == 0 {
                    '1'
                } else {
                    (b'0' + d) as char
                });
            }
            if rng.next() & 1 == 0 {
                s.push('.');
                for _ in 0..(1 + rng.next() % 25) {
                    s.push((b'0' + (rng.next() % 10) as u8) as char);
                }
            }
            if rng.next() & 1 == 0 {
                let e = (rng.next() % 700) as i64 - 350;
                s.push('e');
                s.push_str(&e.to_string());
            }
            same(&s);
        }
    }

    #[test]
    fn a_division_of_128_bits_by_64_is_exact() {
        let check = |hi: u64, lo: u64, d: u64| {
            let num = (hi as u128) << 64 | lo as u128;
            let want = ((num / d as u128) as u64, (num % d as u128) as u64);
            assert_eq!(div128(hi, lo, d), want, "{hi:#x}:{lo:#x} / {d:#x}");
        };
        let mut rng = Rng(0xd1d0_d1d0_5eed_0128);
        for _ in 0..SAMPLES {
            // Every size of divisor, and a dividend whose quotient fits.
            let d = (rng.next() >> (rng.next() % 64)).max(1);
            check(rng.next() % d, rng.next(), d);
            check(d - 1, u64::MAX, d);
            check(0, rng.next(), d);
        }
        for d in [
            1,
            2,
            3,
            5,
            1 << 32,
            (1 << 32) - 1,
            (1 << 32) + 1,
            u64::MAX,
            1 << 63,
        ] {
            check(d - 1, u64::MAX, d);
            check(d - 1, 0, d);
            check(0, 0, d);
        }
    }

    /// Up to 19 digits over 10^1 to 10^25, the quotient path: as many as
    /// JavaScript writes, and past the 53 bits Clinger's path takes.
    #[test]
    fn nineteen_digits_divide_exactly() {
        let mut rng = Rng(0x5eed_1e55_d1ce_f00d);
        for _ in 0..SAMPLES {
            let w = rng.next() % 10u64.pow(1 + (rng.next() % 19) as u32);
            let n = 1 + rng.next() % 25;
            same(&format!("{w}e-{n}"));
            same(&format!("-{w}e-{n}"));
        }
        // What an `f32` widens to, as JavaScript hands it over.
        for _ in 0..SAMPLES {
            let x = f32::from_bits(rng.next() as u32);
            if x.is_finite() {
                same(&format!("{}", x as f64));
            }
        }
        // Exactly between two `f64`s, which goes to the even one, and a hair
        // either side of it.
        for text in [
            "9007199254740992.5",
            "9007199254740993.5",
            "9007199254740992.50000001",
            "9007199254740992.49999999",
            "18014398509481985e0",
            "1801439850948198.5",
            "4503599627370496.25",
            "4503599627370496.75",
            "9999999999999999999e-1",
            "1e-25",
            "1234567890123456789e-25",
        ] {
            same(text);
        }
    }

    #[test]
    fn long_digit_strings_are_exact() {
        // Past the 19 digits the fast path can hold, and past the 800 the
        // slow path keeps -- the tail must not move a rounding decision.
        let mut rng = Rng(0x0123_4567_89ab_cdef);
        for _ in 0..2_000 {
            let n = 20 + (rng.next() % 900) as usize;
            let mut s = String::from("0.");
            for _ in 0..n {
                s.push((b'0' + (rng.next() % 10) as u8) as char);
            }
            same(&s);
        }
        let mut nines = String::from("0.");
        nines.push_str(&"9".repeat(1200));
        same(&nines);
    }

    #[test]
    fn malformed_text_is_rejected() {
        // The JSON scanner hands over any run of `[0-9.eE+-]`, so these do
        // arrive here. Rejecting beats guessing.
        for text in [
            "", "-", "+", ".", "-.", "e5", "1e", "1e+", "1e-", "1.2.3", "--1", "1-2", "1e5e5",
            "1..2", "1x", " 1", "1 ", "inf", "nan", "0x10",
        ] {
            assert!(parse_f64(text).is_none(), "{text:?} should be rejected");
            assert_eq!(read_f64(text), Err(Unread::Malformed), "{text:?}");
        }
    }

    /// The ends of the range, read or refused: the largest `f64` and the
    /// decimals rounding to it, the midpoint past it (a tie, to the even
    /// infinity) and a hair either side; the smallest subnormal, the
    /// midpoint under it (a tie, to the even 0) and a hair either side.
    #[test]
    fn a_number_past_a_float_s_range_is_refused() {
        use Unread::{TooLarge, TooSmall};
        let max = f64::MAX;
        let tiny = f64::from_bits(1);
        // The midpoint between the largest `f64` and 2^1024, and under the
        // smallest subnormal, written whole.
        let over = written((1 << 54) - 1, 970);
        let under = written(1, -1075);
        let (under_digits, _) = under.split_once('e').unwrap();
        // Each less one in its last digit, a 2 and a 5.
        let mut below_over = over.clone().into_bytes();
        *below_over.last_mut().unwrap() -= 1;
        let below_over = String::from_utf8(below_over).unwrap();
        let mut below_under = under_digits.to_string().into_bytes();
        *below_under.last_mut().unwrap() -= 1;
        let below_under = format!("{}e-1075", String::from_utf8(below_under).unwrap());
        let cases: Vec<(String, Result<f64, Unread>)> = vec![
            ("1.7976931348623157e308".into(), Ok(max)),
            ("1.7976931348623158e308".into(), Ok(max)),
            ("-1.7976931348623157e308".into(), Ok(-max)),
            (below_over, Ok(max)),
            (over.clone(), Err(TooLarge)),
            (format!("{over}.000000000000000000000000001"), Err(TooLarge)),
            ("1.7976931348623159e308".into(), Err(TooLarge)),
            ("1e309".into(), Err(TooLarge)),
            ("1e400".into(), Err(TooLarge)),
            ("-1e400".into(), Err(TooLarge)),
            (format!("1{}", "0".repeat(309)), Err(TooLarge)),
            ("1e99999999999999999999".into(), Err(TooLarge)),
            ("4.9406564584124654e-324".into(), Ok(tiny)),
            ("5e-324".into(), Ok(tiny)),
            ("-5e-324".into(), Ok(-tiny)),
            ("3e-324".into(), Ok(tiny)),
            ("2.4703282292062328e-324".into(), Ok(tiny)),
            (format!("{under_digits}1e-1076"), Ok(tiny)),
            (under.clone(), Err(TooSmall)),
            (below_under, Err(TooSmall)),
            ("2.4703282292062327e-324".into(), Err(TooSmall)),
            ("1e-324".into(), Err(TooSmall)),
            ("1e-400".into(), Err(TooSmall)),
            ("-1e-400".into(), Err(TooSmall)),
            ("1e-99999999999999999999".into(), Err(TooSmall)),
            // 0 written any way is 0, its sign kept.
            ("0e-400".into(), Ok(0.0)),
            ("-0.0e999999".into(), Ok(-0.0)),
            ("0.000e+99999999999999999999".into(), Ok(0.0)),
            // An exponent past what was read whole before, and the digits
            // that bring the number back: 1, read as an infinity and 0.
            (format!("0.{}1e1000001", "0".repeat(1_000_000)), Ok(1.0)),
            (format!("1{}e-1000000", "0".repeat(1_000_000)), Ok(1.0)),
        ];
        for (text, want) in cases {
            let got = read_f64(&text);
            match (got, want) {
                (Ok(g), Ok(w)) => assert_eq!(g.to_bits(), w.to_bits(), "{text:.60}"),
                (g, w) => assert_eq!(g, w, "{text:.60}"),
            }
            assert_eq!(parse_f64(&text).is_some(), want.is_ok(), "{text:.60}");
            if text.len() < 2_000 {
                same(&text);
            }
        }
        assert!(TooLarge.range().unwrap().contains("1.7976931348623157e308"));
        assert!(TooSmall.range().unwrap().contains("5e-324"));
        assert_eq!(Unread::Malformed.range(), None);
    }

    /// A midpoint between two `f64`s is a tie, to the even one; a hair
    /// above it rounds up, and its first 25 digits, under it, down. Every
    /// binade's, written whole: the numbers the multiplied path leaves to
    /// the exact one, which these hold to the reference.
    #[test]
    fn a_midpoint_and_a_hair_either_side_round_as_the_reference_does() {
        let mut rng = Rng(0x0dd5_eed5_4a1f_0001);
        let before = exacts();
        for k in 0..400 {
            let x = match k {
                0 => f64::MAX,
                1 => f64::from_bits(1),
                2 => f64::MIN_POSITIVE,
                3 => 1.0,
                4 => 9007199254740992.0,
                _ => f64::from_bits(rng.next() % 0x7ff0_0000_0000_0000),
            };
            let (m, e) = midpoint(x);
            let text = written(m, e);
            same(&text);
            let (digits, exp) = match text.split_once('e') {
                Some((d, x)) => (d.to_string(), x.parse::<i64>().unwrap()),
                None => (text.clone(), 0),
            };
            same(&format!("{digits}1e{}", exp - 1));
            if digits.len() > 25 {
                let cut = (digits.len() - 25) as i64;
                same(&format!("{}e{}", &digits[..25], exp + cut));
            }
        }
        // Most of them reached the exact path: the multiplied one cannot
        // tell a midpoint from a hair beside it.
        assert!(exacts() - before > 600, "{}", exacts() - before);
    }

    /// Past the 800 digits compared, a digit only tells whether the number
    /// is above the midpoint its first 800 write: zeros keep the tie, a 1
    /// a thousand digits on breaks it upwards.
    #[test]
    fn a_digit_past_those_compared_still_breaks_a_tie() {
        for x in [
            f64::from_bits(3),
            f64::from_bits(0x000f_ffff_ffff_fffe),
            0.1,
            1e300,
        ] {
            let (m, e) = midpoint(x);
            let text = written(m, e);
            let (digits, exp) = match text.split_once('e') {
                Some((d, x)) => (d.to_string(), x.parse::<i64>().unwrap()),
                None => (text.clone(), 0),
            };
            let pad = 1_000 - digits.len().min(999);
            let zeros = format!("{digits}{}e{}", "0".repeat(pad), exp - pad as i64);
            same(&zeros);
            let one = format!("{digits}{}1e{}", "0".repeat(pad), exp - pad as i64 - 1);
            same(&one);
            let even = x.to_bits() & 1 == 0;
            let tie = read_f64(&zeros).unwrap().to_bits();
            assert_eq!(tie, x.to_bits() + !even as u64, "{x:e}");
            assert_eq!(read_f64(&one).unwrap().to_bits(), x.to_bits() + 1, "{x:e}");
        }
    }

    /// The numbers JSON brings -- 19 digits or fewer, over any exponent --
    /// reach the exact path only within 2^-70 of a midpoint, which none of
    /// these is: `1e255` read the decimal way took 138 000 instructions.
    #[test]
    fn a_short_number_is_read_without_its_digits_compared() {
        let before = exacts();
        let mut rng = Rng(0x05ee_d0f5_ca1e_0019);
        for e in -342..=308 {
            same(&format!("1e{e}"));
            same(&format!(
                "{}e{e}",
                rng.next() % 10u64.pow(1 + (rng.next() % 19) as u32)
            ));
        }
        for _ in 0..SAMPLES {
            let x = f64::from_bits(rng.next() % 0x7ff0_0000_0000_0000);
            same(&format!("{x:e}"));
        }
        assert_eq!(exacts(), before);
    }

    /// A little-endian bignum of 32-bit words, only to hold the multipliers
    /// to the powers of five they are cut from.
    fn big(mut v: u128) -> Vec<u64> {
        let mut out = vec![];
        while v > 0 {
            out.push((v & 0xffff_ffff) as u64);
            v >>= 32;
        }
        out
    }

    fn times(a: &[u64], b: &[u64]) -> Vec<u64> {
        let mut out = vec![0u64; a.len() + b.len() + 1];
        for (i, &x) in a.iter().enumerate() {
            let mut carry = 0;
            for (j, &y) in b.iter().enumerate() {
                let t = out[i + j] + x * y + carry;
                out[i + j] = t & 0xffff_ffff;
                carry = t >> 32;
            }
            let mut k = i + b.len();
            while carry > 0 {
                let t = out[k] + carry;
                out[k] = t & 0xffff_ffff;
                carry = t >> 32;
                k += 1;
            }
        }
        while out.last() == Some(&0) {
            out.pop();
        }
        out
    }

    fn two_to(k: u32) -> Vec<u64> {
        let mut out = vec![0; k as usize / 32];
        out.push(1 << (k % 32));
        out
    }

    fn cmp(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
        a.len()
            .cmp(&b.len())
            .then_with(|| a.iter().rev().cmp(b.iter().rev()))
    }

    fn bits(a: &[u64]) -> i32 {
        (a.len() as i32 - 1) * 32 + 64 - a.last().unwrap().leading_zeros() as i32
    }

    #[test]
    fn the_multipliers_are_the_powers_of_five_they_claim() {
        use std::cmp::Ordering::*;
        let mut p = big(1);
        for i in 0..=342u32 {
            assert_eq!(bits(&p), pow5_bits(i as i32), "5^{i}");
            // `pow5(i)` is the top 125 bits of 5^i, cut, not rounded, to
            // 5^325.
            let shift = bits(&p) - BITS;
            if i <= 325 {
                let v = pow5(i);
                if shift >= 0 {
                    let unit = two_to(shift as u32);
                    assert_ne!(cmp(&times(&big(v), &unit), &p), Greater, "5^{i}");
                    assert_eq!(cmp(&times(&big(v + 1), &unit), &p), Greater, "5^{i}");
                } else {
                    let whole = times(&p, &two_to(-shift as u32));
                    assert_eq!(cmp(&big(v), &whole), Equal, "5^{i}");
                }
            }
            // `inv_pow5(i)` is one more than 2^k / 5^i, cut, to 5^-342: the
            // smallest power a number is read with (`near`).
            {
                let whole = two_to((pow5_bits(i as i32) - 1 + BITS) as u32);
                let v = inv_pow5(i);
                assert_ne!(cmp(&times(&big(v - 1), &p), &whole), Greater, "5^-{i}");
                assert_eq!(cmp(&times(&big(v), &p), &whole), Greater, "5^-{i}");
            }
            p = times(&p, &[5]);
        }
    }

    #[track_caller]
    fn writes_as_display_f64(x: f64) {
        let mut got = String::new();
        f64_into(&mut got, x);
        assert_eq!(got, format!("{x}"), "{:#x}", x.to_bits());
    }

    #[track_caller]
    fn writes_as_display_f32(x: f32) {
        let mut got = String::new();
        f32_into(&mut got, x);
        assert_eq!(got, format!("{x}"), "{:#x}", x.to_bits());
    }

    #[test]
    fn floats_are_written_as_display_writes_them() {
        for x in [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.1,
            0.2,
            0.3,
            1.0 / 3.0,
            2.0 / 3.0,
            123456.789,
            1e15,
            1e16,
            1e21,
            1e22,
            1e23,
            1e-5,
            1e-7,
            5e-324,
            2.2250738585072014e-308,
            f64::from_bits(0x000f_ffff_ffff_ffff), // the largest subnormal
            1.7976931348623157e308,
            f64::EPSILON,
            9007199254740991.0,
            9007199254740992.0,
            f64::NAN,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            writes_as_display_f64(x);
        }
        // Every power of two and of ten, whole numbers, and the smallest and
        // largest of each binade.
        for e in -1074..=1023 {
            writes_as_display_f64(2f64.powi(e));
        }
        for e in -323..=308 {
            writes_as_display_f64(format!("1e{e}").parse().unwrap());
        }
        for n in 0..100_000 {
            writes_as_display_f64(n as f64);
            writes_as_display_f32(n as f32);
        }
        for e in 1..0x7ff_u64 {
            writes_as_display_f64(f64::from_bits(e << 52));
            writes_as_display_f64(f64::from_bits(e << 52 | ((1 << 52) - 1)));
        }
        // Random bits are mostly astronomically large or small; what a
        // document holds is short decimals, so those too.
        let mut rng = Rng(0x0dd_ba11_c0ff_ee00);
        for _ in 0..SAMPLES * 2 {
            writes_as_display_f64(f64::from_bits(rng.next()));
            let digits = rng.next() % 10u64.pow(1 + (rng.next() % 17) as u32);
            let text = format!("{digits}e{}", (rng.next() % 61) as i32 - 30);
            writes_as_display_f64(text.parse().unwrap());
            writes_as_display_f32(text.parse().unwrap());
        }
    }

    #[test]
    fn an_exact_half_rounds_up_as_display_rounds_it() {
        // Where a float's last bit is worth 2^-1 to 2^-8, `x.25`, `x.125`
        // and the like sit exactly between two shortest candidates: Ryu
        // would take the even one, `{}` takes the one above.
        for e in -8..=-1 {
            let start = 2f64.powi(52 + e);
            for j in 0..4096 {
                writes_as_display_f64(start + j as f64 * 2f64.powi(e));
                writes_as_display_f64(2.0 * start - (j + 1) as f64 * 2f64.powi(e));
            }
            let start = 2f32.powi(23 + e);
            for j in 0..4096 {
                writes_as_display_f32(start + j as f32 * 2f32.powi(e));
            }
        }
        let mut got = String::new();
        f64_into(&mut got, f64::from_bits(0x4310_0000_0000_0001)); // 2^50 + 0.25
        assert_eq!(got, "1125899906842624.3");
    }

    #[test]
    fn f32s_are_written_as_display_writes_them() {
        // One in 4 099 of the 2^32, a spread over every exponent; the test
        // below takes all of them.
        for bits in (0..=u32::MAX).step_by(4099) {
            writes_as_display_f32(f32::from_bits(bits));
        }
        for x in [
            0.1f32,
            0.3,
            1.0 / 3.0,
            16777216.0,
            16777217.0,
            f32::MAX,
            f32::MIN_POSITIVE,
            1e-45,
        ] {
            writes_as_display_f32(x);
        }
    }

    /// Every `f32` against `{}`: `cargo test -p fennec-core --release --lib
    /// every_f32 -- --ignored`, 4 294 967 296 of them on every core.
    #[test]
    #[ignore]
    fn every_f32_is_written_as_display_writes_it() {
        let threads = std::thread::available_parallelism().map_or(8, |n| n.get()) as u64;
        std::thread::scope(|scope| {
            for t in 0..threads {
                scope.spawn(move || {
                    let (mut got, mut want) = (String::new(), String::new());
                    let span = (1u64 << 32) / threads;
                    let end = if t == threads - 1 {
                        1 << 32
                    } else {
                        (t + 1) * span
                    };
                    for bits in t * span..end {
                        let x = f32::from_bits(bits as u32);
                        got.clear();
                        want.clear();
                        f32_into(&mut got, x);
                        std::fmt::Write::write_fmt(&mut want, format_args!("{x}")).unwrap();
                        assert_eq!(got, want, "{bits:#x}");
                    }
                });
            }
        });
    }

    /// As many random `f64`s as asked for: `FENNEC_FLOATS=1000000000 cargo
    /// test -p fennec-core --release --lib many_f64 -- --ignored`.
    #[test]
    #[ignore]
    fn many_f64s_are_written_as_display_writes_them() {
        let n: u64 = std::env::var("FENNEC_FLOATS").map_or(100_000_000, |v| v.parse().unwrap());
        let threads = std::thread::available_parallelism().map_or(8, |n| n.get()) as u64;
        std::thread::scope(|scope| {
            for t in 0..threads {
                scope.spawn(move || {
                    let mut rng =
                        Rng(0x9e37_79b9_7f4a_7c15 ^ (t + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9));
                    let (mut got, mut want) = (String::new(), String::new());
                    for _ in 0..n / threads {
                        let x = f64::from_bits(rng.next());
                        got.clear();
                        want.clear();
                        f64_into(&mut got, x);
                        std::fmt::Write::write_fmt(&mut want, format_args!("{x}")).unwrap();
                        assert_eq!(got, want, "{:#x}", x.to_bits());
                    }
                });
            }
        });
    }

    #[test]
    fn accepted_shapes_match_the_reference_on_signs_and_zeros() {
        for text in [
            "0",
            "-0",
            "0.0",
            "-0.0",
            "0e0",
            "-0e100",
            "0.000e-99",
            "+1.5",
            ".5",
            "5.",
        ] {
            same(text);
        }
    }
}
