//! FennecQL lexer. Allocation is kept to a minimum: every token looks into the
//! source slice, and a String is only produced for strings/identifiers.

use fennec_core::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    /// `"..."`: text. SQL reads one as a column's name, which is why the
    /// parser is told it apart from a [`Tok::Single`].
    Str(String),
    /// `'...'`: text, as SQL writes it, `''` a quote within it.
    Single(String),
    Int(i64),
    Float(f64),
    /// `$1` -> Param(0)
    Param(usize),
    /// `$from`, `$jwt.sub`: a named statement's parameter, or a claim it
    /// declares, by name -- ASCII letters, digits, `_` and a dot between
    /// two names -- which its definition's parser numbers
    /// (`Parser::named`) and refuses outside one. `$` and anything but a
    /// digit was refused before, so no text that lexed reads otherwise.
    /// Without named statements (the browser module) `$` and a letter is
    /// refused as before.
    #[cfg(feature = "statements")]
    Named(String),
    /// Numbers alone between brackets, read as the parser reads such a
    /// list: into `f32`s, a vector ([`tokenize_vectors`]).
    Vector(Vec<f32>),
    /// Numbers alone between brackets, as written: what [`tokenize_values`]
    /// makes of the list [`Tok::Vector`] stands for. Natively alone: the
    /// browser module reads them a token a number, and a token holding
    /// `Value`s took their comparison into it with the tokens', 0.4 KB.
    #[cfg(not(target_arch = "wasm32"))]
    Values(Vec<fennec_core::value::Value>),
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    At,
    Star,
    Plus,
    Minus,
    Slash,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    Tilde,
    /// pgvector's distance, `<->` L2, `<=>` cosine and `<#>` the negative
    /// inner product: what SQL's `order by emb <-> $1` ranks by. No text
    /// that lexed before reads otherwise -- a `<` cannot precede `->` or
    /// `=>` in a statement that parsed -- but for `<#>`, which was a `<`
    /// and a comment.
    Distance(&'static str),
    /// `;`, which ends the statement before it: dropped as white space,
    /// it let a clause go on past it -- `del notes; where id = 3` was one
    /// statement to the parser, which ran it as `-c`, and two to a shell
    /// that ran each statement at its `;`.
    Semi,
    Eof,
    /// `event.metadata."order-id"`: a path with a key FennecQL does not
    /// write as a name, quoted as a policy quotes a claim
    /// (`$jwt."cognito:groups"`) -- its names and keys as written, cut by
    /// dots, which no key holds (`event.metadata.order-id`). A webhook
    /// consumer reads one where it reads its event (a parameter's `from`,
    /// its condition) and every other place refuses it: a dot before a
    /// `"` was no token at all, so no text that lexed reads otherwise.
    /// Without named statements it is that error still. A `String`, as
    /// [`Tok::Ident`]'s: a list of keys made every token's copy and drop
    /// an arm longer, the lexer's loop 0.9 KB. The last variant, so no
    /// other token's discriminant moves.
    #[cfg(feature = "statements")]
    Keys(String),
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Str(s) => format!("\"{s}\""),
            Tok::Single(s) => format!("'{s}'"),
            Tok::Int(i) => i.to_string(),
            Tok::Float(f) => {
                let mut text = String::new();
                fennec_core::num::f64_into(&mut text, *f);
                text
            }
            Tok::Param(i) => format!("${}", i + 1),
            #[cfg(feature = "statements")]
            Tok::Named(n) => format!("`${n}`"),
            #[cfg(feature = "statements")]
            Tok::Keys(_) => "a path with a quoted key".into(),
            Tok::Vector(v) => format!("a vector of {}", v.len()),
            #[cfg(not(target_arch = "wasm32"))]
            Tok::Values(v) => format!("a list of {}", v.len()),
            Tok::LBrace => "`{`".into(),
            Tok::RBrace => "`}`".into(),
            Tok::LParen => "`(`".into(),
            Tok::RParen => "`)`".into(),
            Tok::LBracket => "`[`".into(),
            Tok::RBracket => "`]`".into(),
            Tok::Comma => "`,`".into(),
            Tok::Colon => "`:`".into(),
            Tok::At => "`@`".into(),
            Tok::Star => "`*`".into(),
            Tok::Plus => "`+`".into(),
            Tok::Minus => "`-`".into(),
            Tok::Slash => "`/`".into(),
            Tok::Lt => "`<`".into(),
            Tok::Le => "`<=`".into(),
            Tok::Gt => "`>`".into(),
            Tok::Ge => "`>=`".into(),
            Tok::Eq => "`=`".into(),
            Tok::Ne => "`!=`".into(),
            Tok::Tilde => "`~`".into(),
            Tok::Distance(op) => format!("`{op}`"),
            Tok::Semi => "`;`".into(),
            Tok::Eof => "end of input".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub pos: usize,
}

/// Whether a `-` after `last` takes something away (`n - 1`, `n-1`)
/// rather than starting a negative number (`= -1`, `[1, -2]`): it does
/// after what ends a value -- a name, a literal, a parameter, a closing
/// bracket. After a keyword, which is a name too, `-1` is a `-` and a `1`,
/// which the parser folds back into the number (`where x > 0 and -1 < y`).
/// `$from`, `$jwt.sub` from `s`, past its `$`, and where it ends: out of
/// the lexer's loop, which every text runs. With this and the parser's
/// paths only a definition takes inline -- in `primary`, `put`'s documents
/// and a `get`'s collection -- the repository's texts, none a definition,
/// parsed 1.6% slower than on main; out of line, as fast
/// (`docs/design/named-statements.md`, *What is built*).
#[cfg(feature = "statements")]
#[cold]
#[inline(never)]
fn named_at(src: &str, s: usize) -> (Tok, usize) {
    let b = src.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = s;
    loop {
        while b.get(i).is_some_and(|&c| word(c)) {
            i += 1;
        }
        match (b.get(i), b.get(i + 1)) {
            (Some(b'.'), Some(&c)) if c.is_ascii_alphabetic() || c == b'_' => i += 1,
            _ => break,
        }
    }
    (Tok::Named(src.get(s..i).unwrap_or_default().to_string()), i)
}

/// What a quoted key is refused with, whatever it holds that no key may:
/// one message, which says what a key holds.
#[cfg(feature = "statements")]
pub(crate) const KEY_HOLDS: &str = "a quoted key is the JSON's as written: a character at least, \
                                    and no `\\`, no control character and no `.`, which cuts a \
                                    path's keys";

/// At a `.` at byte `i`: where it stands right after a name, the last of
/// `out`, and before a `"` (`event.metadata."order-id"`), the path read on
/// into [`Tok::Keys`] in that name's place -- each key in quotes, each name
/// after a dot -- and where it ends; `None` for every other dot, the error
/// it was, but a dot after a name and before a digit (`items.0`), refused
/// naming the key quoted. A quoted key is taken as written, the JSON's
/// own: a character at least, with no `\`, no control character -- a line
/// ends none, so the shell's gate closes one at a line's end
/// ([`Unclosed`]) -- and no `.`, which cuts a path's keys ([`KEY_HOLDS`]).
/// Out of the lexer's loop, every check with it, reached where a dot alone
/// was refused: no text that lexed pays for it.
#[cfg(feature = "statements")]
#[cold]
#[inline(never)]
fn quoted_keys(src: &str, out: &mut [Token], mut i: usize, start: usize) -> Result<Option<usize>> {
    let b = src.as_bytes();
    let after_name = (src.get(..i).and_then(|s| s.chars().next_back()))
        .is_some_and(|c| c.is_alphanumeric() || c == '_');
    let Some(Token { tok: last, .. }) = out.last_mut() else {
        return Ok(None);
    };
    let Tok::Ident(first) = last else {
        return Ok(None);
    };
    if !after_name || !matches!(b.get(i + 1), Some(b'"' | b'0'..=b'9')) {
        return Ok(None);
    }
    let mut path = std::mem::take(first);
    loop {
        // At a `.` before a `"`, a digit or a name's first character.
        path.push('.');
        let s = i + 1;
        let mut j = s;
        let why = match b.get(s) {
            Some(b'"') => loop {
                j += 1;
                match b.get(j) {
                    None => break Some(UNTERMINATED),
                    Some(b'"') if j > s + 1 => break None,
                    Some(&(b'"' | b'\\' | b'.' | ..=0x1f | 0x7f)) => break Some(KEY_HOLDS),
                    Some(_) => {}
                }
            },
            // `items.0`: a key no name reads as, refused naming it quoted.
            Some(b'0'..=b'9') => {
                j += (b[s..].iter())
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                    .count();
                return Err(Error::Query(format!(
                    "position {start}: unexpected character `.`: a key that starts with a digit \
                     is quoted, `.\"{}\"`",
                    src.get(s..j).unwrap_or_default()
                )));
            }
            _ => {
                while let Some(c) = src.get(j..).and_then(|t| t.chars().next()) {
                    if !(c.is_alphanumeric() || c == '_') {
                        break;
                    }
                    j += c.len_utf8();
                }
                None
            }
        };
        if let Some(why) = why {
            return Err(Error::Query(format!("position {start}: {why}")));
        }
        i = match b[s] {
            b'"' => {
                path.push_str(src.get(s + 1..j).unwrap_or_default());
                j + 1
            }
            _ => {
                path.push_str(src.get(s..j).unwrap_or_default());
                j
            }
        };
        match (b.get(i), src.get(i + 1..).and_then(|t| t.chars().next())) {
            (Some(b'.'), Some(c))
                if c == '"' || c.is_ascii_digit() || c.is_alphabetic() || c == '_' => {}
            _ => break,
        }
    }
    *last = Tok::Keys(path);
    Ok(Some(i))
}

/// At a character at byte `i` that begins no token: where it ends after
/// a path with a quoted key ([`quoted_keys`]), else its refusal -- the
/// character as the text holds it, a `char` formatted being 274 bytes of
/// the browser module. The lexer's loop holds a call alone for it: with
/// the quoted key's call and the refusal inline, the loop the texts that
/// are no definition run was laid out anew, its registers spilled,
/// though no text that lexed reached the arm.
#[cfg(feature = "statements")]
#[cold]
#[inline(never)]
fn unlexed(
    src: &str,
    out: &mut [Token],
    i: usize,
    start: usize,
    counted: usize,
    other: char,
) -> Result<usize> {
    if other == '.' {
        if let Some(end) = quoted_keys(src, out, i, start)? {
            return Ok(end);
        }
    }
    let other = src
        .get(counted..counted + other.len_utf8())
        .unwrap_or_default();
    Err(Error::Query(format!(
        "position {start}: unexpected character `{other}`"
    )))
}

fn subtracts(last: Option<&Token>) -> bool {
    match last.map(|t| &t.tok) {
        Some(
            Tok::Ident(_)
            | Tok::Str(_)
            | Tok::Single(_)
            | Tok::Int(_)
            | Tok::Float(_)
            | Tok::Param(_)
            | Tok::Vector(_)
            | Tok::RParen
            | Tok::RBracket,
        ) => true,
        #[cfg(not(target_arch = "wasm32"))]
        Some(Tok::Values(_)) => true,
        #[cfg(feature = "statements")]
        Some(Tok::Named(_) | Tok::Keys(_)) => true,
        _ => false,
    }
}

pub fn tokenize(src: &str) -> Result<Vec<Token>> {
    lex::<false>(src, false)
}

/// [`tokenize`], with numbers alone between brackets read at once, as
/// written, into the list the parser makes of their tokens (`Tok::Values`)
/// where [`tokenize_vectors`] reads a vector: what `parse_exact` reads,
/// natively. The browser module reads them a token a number, as it did.
pub(crate) fn tokenize_values(src: &str) -> Result<Vec<Token>> {
    lex::<false>(src, cfg!(not(target_arch = "wasm32")))
}

/// [`tokenize`], with numbers alone between brackets read at once into
/// the vector the parser makes of them (`Parser::numbers_in_brackets`): a
/// token and a pass over the text for the whole list, where each number and
/// each comma was a token of its own, and each number's text read three
/// times -- for its end, for a `_`, for its value. A `put` of 1 000 rows
/// holding a 128-dim vector each took 5.4 ms to parse, 4.6 of it the lexer.
/// Not after `in`, whose list keeps its numbers as they are written: an
/// integer an integer and a decimal an `f64`. The browser module too, for
/// its memory rather than the time: a token a number, a `put` of 1 000
/// 768-dim rows as text took it from 58 to 130 MB, which it never gives
/// back, for 0.3 KB brotli.
pub fn tokenize_vectors(src: &str) -> Result<Vec<Token>> {
    lex::<true>(src, false)
}

/// The words a text that ends inside a string is refused with, by which
/// `parser::reading` knows its end is what refused it.
pub(crate) const UNTERMINATED: &str = "unterminated string";

/// What a text leaves open, read as the lexer reads its strings, quoted
/// keys, comments and brackets but a piece at a time and with no token
/// made: a string or a key not closed, or more brackets opened than
/// closed. No statement is whole while either holds, so a reader taking a
/// text a line at a time -- the shell -- asks this before it reads the
/// text again: a document written over 10 000 lines is read once, at its
/// last line, where reading it at each was the text lexed and parsed
/// 10 000 times.
///
/// It is the lexer's reading but for one text the lexer refuses anyway: a
/// number whose exponent's sign is followed by a `-` (`1e--2`), where it
/// sees a comment the lexer does not reach. `the_gate_reads_as_the_lexer`
/// holds it to the lexer over generated texts.
#[derive(Clone, Debug, Default)]
pub struct Unclosed {
    /// Brackets opened and not closed: `(`, `[` and `{`.
    depth: i64,
    /// The quote of the string the text is inside, 0 outside one.
    quote: u8,
    /// Inside a string, just after a backslash: the next byte is escaped.
    escape: bool,
    /// Inside a comment, which the line's end closes.
    comment: bool,
    /// Outside a string and a comment, the byte before was a `-`: one more
    /// starts a comment.
    dash: bool,
    /// The byte before was a `<`: a `#` after it may be pgvector's `<#>`.
    lt: bool,
    /// The bytes before were `<#`: a `>` makes them `<#>`, an operator,
    /// anything else a `<` and a comment.
    lt_hash: bool,
    /// The byte before ends a name -- a letter, a digit, `_`, a byte of a
    /// character outside ASCII -- or a quoted key: a `.` after it may lead
    /// a path's quoted key.
    name: bool,
    /// The byte before was a `.` after a name: a `"` after it opens a
    /// quoted key (`event.metadata."order-id"`), which no `\` escapes and
    /// a control character -- a line's end -- closes, as the lexer refuses
    /// it there: a key open at a line's end swallows no line after it.
    dot: bool,
}

/// [`Unclosed::quote`] inside a quoted key.
const KEY: u8 = b'.';

impl Unclosed {
    /// The next piece of the text.
    pub fn feed(&mut self, piece: &str) {
        for &b in piece.as_bytes() {
            if self.comment {
                self.comment = b != b'\n';
                continue;
            }
            if self.quote == KEY {
                if b == b'"' || b < 0x20 || b == 0x7f {
                    self.quote = 0;
                    self.name = b == b'"';
                }
                continue;
            }
            if self.quote != 0 {
                match (self.escape, b) {
                    (true, _) => self.escape = false,
                    (false, b'\\') => self.escape = true,
                    (false, q) if q == self.quote => self.quote = 0,
                    _ => {}
                }
                continue;
            }
            if std::mem::take(&mut self.lt_hash) {
                if b != b'>' {
                    self.comment = b != b'\n';
                }
                continue;
            }
            let dash = std::mem::take(&mut self.dash);
            let lt = std::mem::replace(&mut self.lt, b == b'<');
            let name = std::mem::replace(
                &mut self.name,
                b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80,
            );
            let dot = std::mem::replace(&mut self.dot, b == b'.' && name);
            match b {
                b'"' if dot => self.quote = KEY,
                b'"' | b'\'' => self.quote = b,
                b'#' if lt => self.lt_hash = true,
                b'#' => self.comment = true,
                b'-' => match dash {
                    true => self.comment = true,
                    false => self.dash = true,
                },
                b'(' | b'[' | b'{' => self.depth += 1,
                b')' | b']' | b'}' => self.depth -= 1,
                _ => {}
            }
        }
    }

    /// Whether the text so far ends inside a string or with a bracket
    /// open.
    pub fn is_open(&self) -> bool {
        self.quote != 0 || self.depth > 0
    }
}

/// At a `[` at byte `i`: the numbers alone up to the `]` that closes it,
/// each read as a token of its own reads it and made an `f32` as the parser
/// makes it, and where the `]` ends. `None` for anything else -- an empty
/// list, a trailing comma, a comment, a `_` in a number, one that does not
/// read -- which the tokens then read as they always did. And `None` for a
/// list whose `f32`s would pass for the whole numbers written where one was
/// another (`[1.00000001]`), which the parser reads as written: a list of
/// numbers takes a vector of whole `f32`s as the integers written
/// (`fennec_core::value::whole_f32s`), so the quick read makes no other.
fn numbers_at(src: &str, b: &[u8], mut i: usize) -> Option<(Vec<f32>, usize)> {
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let space = |i: usize| matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r'));
    let mut v = Vec::new();
    let start = i;
    // A number written with a fraction or an exponent: only a list holding
    // one can hide a rounding in a whole `f32`.
    let mut floats = false;
    let closed = |v: Vec<f32>, end: usize, floats: bool| {
        let hides = floats
            && fennec_core::value::whole_f32s(&v)
            && fennec_core::num::hides_rounding(src.get(start..end)?, &v);
        (!hides).then_some((v, end))
    };
    i += 1;
    loop {
        while space(i) {
            i += 1;
        }
        // Natively a number is read in the one pass that finds its end,
        // where Clinger's path takes it (`num::clinger`, the JSON reader's):
        // 3.5 -> 2.3 ms of the 1 000 rows' parse. The browser module goes
        // the general way, and carries no second reader.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(x) = fennec_core::num::clinger(b, &mut i, &mut floats) {
            v.push(x as f32);
            while space(i) {
                i += 1;
            }
            match b.get(i) {
                Some(b',') => {
                    i += 1;
                    continue;
                }
                Some(b']') => return closed(v, i + 1, floats),
                _ => return None,
            }
        }
        let s = i;
        i += (b.get(i) == Some(&b'-')) as usize;
        if !digit(i) {
            return None;
        }
        while digit(i) {
            i += 1;
        }
        let mut float = false;
        if b.get(i) == Some(&b'.') && digit(i + 1) {
            float = true;
            i += 1;
            while digit(i) {
                i += 1;
            }
        }
        if let Some(b'e' | b'E') = b.get(i) {
            float = true;
            i += 1;
            i += matches!(b.get(i), Some(b'+' | b'-')) as usize;
            while digit(i) {
                i += 1;
            }
        }
        let text = src.get(s..i)?;
        floats |= float;
        v.push(match float {
            true => fennec_core::num::parse_f64(text)? as f32,
            false => fennec_core::num::parse_i64(text)? as f32,
        });
        while space(i) {
            i += 1;
        }
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b']') => return closed(v, i + 1, floats),
            _ => return None,
        }
    }
}

/// [`numbers_at`] as written, for [`tokenize`]: each number the value its
/// token would be -- an integer an `Int`, one with a fraction or an exponent
/// the `f64` it says -- in one pass, natively. `None` for anything else,
/// which the tokens read as they always did. A number a token at a time
/// was most of what reading a text again as written cost: a `put` of 1 000
/// rows, each an `[int]` and a `[float]` of 16, sent as text and read again
/// for its `[float]`s, took 93.9 M instructions so and 80.6 M this way.
#[cfg(not(target_arch = "wasm32"))]
fn values_at(src: &str, b: &[u8], mut i: usize) -> Option<(Vec<fennec_core::value::Value>, usize)> {
    use fennec_core::value::Value;
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let space = |i: usize| matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r'));
    let mut v = Vec::new();
    i += 1;
    loop {
        while space(i) {
            i += 1;
        }
        let mut float = false;
        let x = match fennec_core::num::clinger(b, &mut i, &mut float) {
            // Clinger's path takes an integer of no more than 2^53.
            Some(x) if !float => Value::Int(x as i64),
            Some(x) => Value::Float(x),
            None => {
                let s = i;
                i += (b.get(i) == Some(&b'-')) as usize;
                if !digit(i) {
                    return None;
                }
                while digit(i) {
                    i += 1;
                }
                if b.get(i) == Some(&b'.') && digit(i + 1) {
                    float = true;
                    i += 1;
                    while digit(i) {
                        i += 1;
                    }
                }
                if let Some(b'e' | b'E') = b.get(i) {
                    float = true;
                    i += 1;
                    i += matches!(b.get(i), Some(b'+' | b'-')) as usize;
                    while digit(i) {
                        i += 1;
                    }
                }
                let text = src.get(s..i)?;
                match float {
                    true => Value::Float(fennec_core::num::parse_f64(text)?),
                    false => Value::Int(fennec_core::num::parse_i64(text)?),
                }
            }
        };
        v.push(x);
        while space(i) {
            i += 1;
        }
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b']') => return Some((v, i + 1)),
            _ => return None,
        }
    }
}

/// One body natively for each way, the flag a constant in each; the browser
/// module keeps one, the flag read at run time: [`tokenize`] is there only
/// for a list a json field is given (`fennec_ql::parse_exact`), and a copy
/// of its own was 2.9 KB of the module.
fn lex<const VECTORS: bool>(src: &str, values: bool) -> Result<Vec<Token>> {
    lex_with(src, VECTORS, values)
}

#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
fn lex_with(src: &str, vectors: bool, values: bool) -> Result<Vec<Token>> {
    // The browser module reads a list as written a token a number, and
    // carries no code for the other way (`tokenize_values`).
    #[cfg(target_arch = "wasm32")]
    let values = {
        let _ = values;
        false
    };
    // Walked a byte at a time, a character read whole only where one
    // outside ASCII stands. Collected into a `Vec<char>` first, and each
    // number copied into a `String` of its own to parse, a query holding a
    // 128-dim vector took 16.9 us to parse. The text is sliced with `get`,
    // always at a character's edge: an index that can panic kept its
    // panic's formatting of a `char` in the browser module, 2.7 KB brotli.
    let b = src.as_bytes();
    let mut i = 0usize;
    // A token's position is its character's, as the errors count it: the
    // bytes stepped past are counted as they are left, each but a UTF-8
    // continuation byte.
    let (mut chars, mut counted) = (0usize, 0usize);
    // A token about every four bytes, but no more than 65 536 ahead: a
    // list of numbers is one token, and a `put` of 1 000 768-dim rows had
    // 39 MB set aside for 7 000 -- in the browser module, whose memory is
    // never given back, for good.
    let mut out = Vec::with_capacity((b.len() / 4 + 2).min(1 << 16));
    // The character at byte `i`, and its length in bytes.
    let at = |i: usize| -> (char, usize) {
        match b[i] {
            c if c < 0x80 => (c as char, 1),
            _ => {
                let c = src.get(i..).and_then(|t| t.chars().next()).unwrap_or('\0');
                (c, c.len_utf8())
            }
        }
    };
    // Braces open: a document's are the first, an object literal's inside.
    let mut braces = 0usize;
    let next_is = |i: usize, want: u8| b.get(i) == Some(&want);
    let digit_at = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);

    while i < b.len() {
        let (c, len) = at(i);
        // whitespace
        if c.is_whitespace() {
            i += len;
            continue;
        }
        // comment: -- to end of line, # is accepted too
        if c == '#' || (c == '-' && next_is(i + 1, b'-')) {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        chars += b[counted..i].iter().filter(|&&x| x & 0xC0 != 0x80).count();
        counted = i;
        let start = chars;
        let tok = match c {
            '{' => {
                i += 1;
                braces += 1;
                Tok::LBrace
            }
            '}' => {
                i += 1;
                braces = braces.saturating_sub(1);
                Tok::RBrace
            }
            '(' => {
                i += 1;
                Tok::LParen
            }
            ')' => {
                i += 1;
                Tok::RParen
            }
            // Not inside an object literal, two braces down: only a json
            // field holds one, and its numbers stay as they are written.
            '[' if braces < 2 && (vectors || values) => {
                // `in [..]` and `facet f ranges [..]` keep their integers and
                // `f64`s: a bound past 2^24 is not an `f32`.
                let after_in = matches!(out.last(), Some(Token { tok: Tok::Ident(w), .. }) if w.eq_ignore_ascii_case("in") || w.eq_ignore_ascii_case("ranges"));
                match !after_in {
                    true if vectors => match numbers_at(src, b, i) {
                        Some((v, end)) => {
                            i = end;
                            Tok::Vector(v)
                        }
                        None => {
                            i += 1;
                            Tok::LBracket
                        }
                    },
                    #[cfg(not(target_arch = "wasm32"))]
                    true => match values_at(src, b, i) {
                        Some((v, end)) => {
                            i = end;
                            Tok::Values(v)
                        }
                        None => {
                            i += 1;
                            Tok::LBracket
                        }
                    },
                    _ => {
                        i += 1;
                        Tok::LBracket
                    }
                }
            }
            '[' => {
                i += 1;
                Tok::LBracket
            }
            ']' => {
                i += 1;
                Tok::RBracket
            }
            ',' => {
                i += 1;
                Tok::Comma
            }
            ':' => {
                i += 1;
                Tok::Colon
            }
            '@' => {
                i += 1;
                Tok::At
            }
            '*' => {
                i += 1;
                Tok::Star
            }
            '~' => {
                i += 1;
                Tok::Tilde
            }
            '+' => {
                i += 1;
                Tok::Plus
            }
            '/' => {
                i += 1;
                Tok::Slash
            }
            // SQL's `||` joins two texts, as FennecQL's `+` does: the same
            // operator, a text and a number refused alike.
            '|' if next_is(i + 1, b'|') => {
                i += 2;
                Tok::Plus
            }
            '-' if !digit_at(i + 1) || subtracts(out.last()) => {
                i += 1;
                Tok::Minus
            }
            ';' => {
                i += 1;
                Tok::Semi
            }
            '=' => {
                i += 1;
                if next_is(i, b'=') {
                    i += 1;
                }
                Tok::Eq
            }
            '!' => {
                i += 1;
                if next_is(i, b'=') {
                    i += 1;
                    Tok::Ne
                } else {
                    return Err(Error::Query(format!(
                        "position {start}: `!` alone is invalid"
                    )));
                }
            }
            // pgvector's `<->`, `<=>` and `<#>`, before `<=` and the `#`
            // that begins a comment.
            '<' if matches!(b.get(i + 1), Some(b'-' | b'=' | b'#')) && next_is(i + 2, b'>') => {
                let op = match b[i + 1] {
                    b'-' => "<->",
                    b'=' => "<=>",
                    _ => "<#>",
                };
                i += 3;
                Tok::Distance(op)
            }
            '<' => {
                i += 1;
                if next_is(i, b'=') {
                    i += 1;
                    Tok::Le
                } else if next_is(i, b'>') {
                    i += 1;
                    Tok::Ne
                } else {
                    Tok::Lt
                }
            }
            '>' => {
                i += 1;
                if next_is(i, b'=') {
                    i += 1;
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            // `$from`, `$jwt.sub`: in ASCII alone, as a named statement's
            // names are, a dot between two names.
            #[cfg(feature = "statements")]
            '$' if b.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
                let (tok, end) = named_at(src, i + 1);
                i = end;
                tok
            }
            '$' => {
                i += 1;
                let s = i;
                while digit_at(i) {
                    i += 1;
                }
                // `$from` in the module, which reads no definition: its
                // refusal is the module's, not a number's.
                #[cfg(not(feature = "statements"))]
                if s == i && b.get(i).is_some_and(u8::is_ascii_alphabetic) {
                    return Err(Error::Query(format!(
                        "position {start}: {}",
                        crate::parser::UNBUILT
                    )));
                }
                if s == i {
                    return Err(Error::Query(format!(
                        "position {start}: expected a number after `$`"
                    )));
                }
                // Unwrapped, a number past `usize` was a panic.
                let n = fennec_core::num::parse_i64(src.get(s..i).unwrap_or_default())
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| {
                        Error::Query(format!(
                            "position {start}: no parameter is numbered that high"
                        ))
                    })?;
                if n == 0 {
                    return Err(Error::Query("parameters start at $1".into()));
                }
                Tok::Param(n - 1)
            }
            '"' | '\'' => {
                let quote = b[i];
                i += 1;
                let mut s = String::new();
                loop {
                    if i >= b.len() {
                        return Err(Error::Query(format!("position {start}: {UNTERMINATED}")));
                    }
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        let (e, elen) = at(i);
                        s.push(match e {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '0' => '\0',
                            other => other,
                        });
                        i += elen;
                        continue;
                    }
                    if b[i] == quote {
                        i += 1;
                        // SQL's `'it''s'`: two quotes in a single-quoted
                        // text are one. Two texts side by side were never
                        // FennecQL, so no text that parsed reads otherwise.
                        if quote == b'\'' && next_is(i, b'\'') {
                            s.push('\'');
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    // A run of plain characters at once, a backslash that
                    // ends the text among them.
                    let run = i;
                    while i < b.len() && b[i] != quote && !(b[i] == b'\\' && i + 1 < b.len()) {
                        i += 1;
                    }
                    s.push_str(src.get(run..i).unwrap_or_default());
                }
                match quote {
                    b'"' => Tok::Str(s),
                    _ => Tok::Single(s),
                }
            }
            c if c.is_ascii_digit() || (c == '-' && digit_at(i + 1)) => {
                let s = i;
                if b[i] == b'-' {
                    i += 1;
                }
                let mut is_float = false;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
                if next_is(i, b'.') && digit_at(i + 1) {
                    is_float = true;
                    i += 1;
                    while digit_at(i) {
                        i += 1;
                    }
                }
                if next_is(i, b'e') || next_is(i, b'E') {
                    is_float = true;
                    i += 1;
                    if next_is(i, b'+') || next_is(i, b'-') {
                        i += 1;
                    }
                    while digit_at(i) {
                        i += 1;
                    }
                }
                // Read where it stands: a copy only to drop `_`s.
                let raw = src.get(s..i).unwrap_or_default();
                let owned;
                let text = match raw.contains('_') {
                    true => {
                        owned = raw.replace('_', "");
                        owned.as_str()
                    }
                    false => raw,
                };
                if is_float {
                    Tok::Float(fennec_core::num::parse_f64(text).ok_or_else(|| {
                        Error::Query(format!("position {start}: invalid decimal number `{text}`"))
                    })?)
                } else {
                    Tok::Int(fennec_core::num::parse_i64(text).ok_or_else(|| {
                        Error::Query(format!("position {start}: invalid integer `{text}`"))
                    })?)
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let s = i;
                i += len;
                loop {
                    while i < b.len() {
                        let (c, l) = at(i);
                        if !(c.is_alphanumeric() || c == '_') {
                            break;
                        }
                        i += l;
                    }
                    // `meta.source.rank`: a dot between two names is a path,
                    // one token, which the parser takes where a field goes.
                    // A dot was read only inside a number before, so no text
                    // that parsed reads differently.
                    let next = match b.get(i) {
                        Some(b'.') if i + 1 < b.len() => at(i + 1).0,
                        _ => break,
                    };
                    // SQL's `o.*`, every column of the table `o` names: a
                    // dot before a `*` was no token at all.
                    if next == '*' {
                        i += 2;
                        break;
                    }
                    if !(next.is_alphabetic() || next == '_') {
                        break;
                    }
                    i += 1;
                }
                Tok::Ident(src.get(s..i).unwrap_or_default().to_string())
            }
            // A character no token begins, refused out of line -- but a
            // dot right after a name before a `"`, a path with a quoted key
            // (`event.metadata."order-id"`), read on into `Tok::Keys`.
            #[cfg(feature = "statements")]
            other => {
                i = unlexed(src, &mut out, i, start, counted, other)?;
                continue;
            }
            #[cfg(not(feature = "statements"))]
            other => {
                // The character as the text holds it: a `char` formatted
                // was 274 bytes of the browser module for this alone.
                let other = src
                    .get(counted..counted + other.len_utf8())
                    .unwrap_or_default();
                return Err(Error::Query(format!(
                    "position {start}: unexpected character `{other}`"
                )));
            }
        };
        out.push(Token { tok, pos: start });
    }
    chars += b[counted..].iter().filter(|&&x| x & 0xC0 != 0x80).count();
    out.push(Token {
        tok: Tok::Eof,
        pos: chars,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let t = tokenize(r#"get docs where year >= 2020 and title ~ "rust" limit 10"#).unwrap();
        assert!(matches!(t[0].tok, Tok::Ident(ref s) if s == "get"));
        assert!(t.iter().any(|t| t.tok == Tok::Ge));
        assert!(t.iter().any(|t| t.tok == Tok::Tilde));
    }

    #[test]
    fn a_dot_between_names_is_one_path() {
        let t = tokenize("where meta.source.rank >= 2.5 and x.y_1 = a.").unwrap_err();
        assert!(t.to_string().contains('.'), "{t}");
        let t = tokenize("where meta.source.rank >= 2.5 and x.y_1 = a").unwrap();
        assert_eq!(t[1].tok, Tok::Ident("meta.source.rank".into()));
        assert_eq!(t[3].tok, Tok::Float(2.5));
        assert_eq!(t[5].tok, Tok::Ident("x.y_1".into()));
        // Not before a digit: `a.5` is no path.
        assert!(tokenize("a.5").is_err());
    }

    /// A `-` after what ends a value takes away; anywhere else it is a
    /// number's sign, as it always was.
    #[test]
    fn a_minus_after_a_value_subtracts() {
        let toks =
            |s: &str| -> Vec<Tok> { tokenize(s).unwrap().into_iter().map(|t| t.tok).collect() };
        let n = || Tok::Ident("n".into());
        assert_eq!(toks("n-1"), [n(), Tok::Minus, Tok::Int(1), Tok::Eof]);
        assert_eq!(toks("n - 1"), [n(), Tok::Minus, Tok::Int(1), Tok::Eof]);
        assert_eq!(
            toks("$1-2"),
            [Tok::Param(0), Tok::Minus, Tok::Int(2), Tok::Eof]
        );
        assert_eq!(
            toks("(n)-2.5")[3..],
            [Tok::Minus, Tok::Float(2.5), Tok::Eof]
        );
        assert_eq!(toks("n = -1"), [n(), Tok::Eq, Tok::Int(-1), Tok::Eof]);
        assert_eq!(
            toks("[1, -2]")[3..],
            [Tok::Int(-2), Tok::RBracket, Tok::Eof]
        );
        assert_eq!(toks("-n"), [Tok::Minus, n(), Tok::Eof]);
        assert_eq!(
            toks("n+1*2/3")[1..6],
            [Tok::Plus, Tok::Int(1), Tok::Star, Tok::Int(2), Tok::Slash]
        );
        // A comment is still a comment.
        assert_eq!(toks("n -- 1"), [n(), Tok::Eof]);
    }

    /// `$` and a letter is a name, a path of names among them; `$` and a
    /// digit a number, as it was; `$` and anything else refused, as it was.
    #[cfg(feature = "statements")]
    #[test]
    fn a_named_parameter_is_one_token() {
        let toks =
            |s: &str| -> Vec<Tok> { tokenize(s).unwrap().into_iter().map(|t| t.tok).collect() };
        let named = |n: &str| Tok::Named(n.into());
        assert_eq!(toks("$from"), [named("from"), Tok::Eof]);
        assert_eq!(
            toks("$jwt.sub=$a_1"),
            [named("jwt.sub"), Tok::Eq, named("a_1"), Tok::Eof]
        );
        // A path of names is one; a dot before no name is the lexer's
        // refusal, as after a field's name.
        assert_eq!(toks("$a.b.c_2")[..1], [named("a.b.c_2")]);
        assert!(tokenize("$a.5").is_err() && tokenize("a.5").is_err());
        assert_eq!(toks("$a-1")[..3], [named("a"), Tok::Minus, Tok::Int(1)]);
        assert_eq!(toks("$1")[..1], [Tok::Param(0)]);
        assert!(tokenize("$_a").is_err() && tokenize("$").is_err() && tokenize("$ a").is_err());
        assert_eq!(tokenize("$ş").unwrap_err(), tokenize("$_").unwrap_err());
    }

    /// A dot right after a name and before a `"` reads the name on as a
    /// path whose keys may be quoted (a webhook consumer's), names and
    /// quoted keys after it; every other dot is the error it was.
    #[cfg(feature = "statements")]
    #[test]
    fn a_quoted_key_reads_a_path_on() {
        let toks =
            |s: &str| -> Vec<Tok> { tokenize(s).unwrap().into_iter().map(|t| t.tok).collect() };
        let keys = |k: &[&str]| Tok::Keys(k.join("."));
        assert_eq!(
            toks("event.metadata.\"order-id\""),
            [keys(&["event", "metadata", "order-id"]), Tok::Eof]
        );
        assert_eq!(
            toks("event.\"a b\".c.\"0\".şehir=1"),
            [
                keys(&["event", "a b", "c", "0", "şehir"]),
                Tok::Eq,
                Tok::Int(1),
                Tok::Eof
            ]
        );
        assert_eq!(
            toks("x.\"k\"-1")[..3],
            [keys(&["x", "k"]), Tok::Minus, Tok::Int(1)]
        );
        // Placed where the name began, as every token is.
        let t = tokenize("get e where EVENT.\"x-y\" = 1").unwrap();
        assert_eq!((t[3].pos, &t[3].tok), (12, &keys(&["EVENT", "x-y"])));
        // Unclosed is a string unclosed, which the shell reads on past.
        let e = tokenize("event.\"order").unwrap_err().to_string();
        assert!(e.ends_with(UNTERMINATED), "{e}");
        for (text, words) in [
            // What no key holds, one message, at the path's first dot.
            (
                "event.\"\"",
                "position 5: a quoted key is the JSON's as written",
            ),
            ("event.\"a.b\"", KEY_HOLDS),
            ("event.\"a\\\"b\"", KEY_HOLDS),
            ("x.y.\"a\nb\" = 1", "position 3: a quoted key"),
            ("event.\"a\tb\"", KEY_HOLDS),
            ("event.\"a\u{7f}\"", KEY_HOLDS),
            ("event.\"ok\".\"\r\"", KEY_HOLDS),
            // A dot after a space, a `*` or a text is no path's.
            ("event .\"a\"", "unexpected character `.`"),
            ("o.*.\"a\"", "unexpected character `.`"),
            ("\"a\".\"b\"", "unexpected character `.`"),
            // A key no name reads as, after a name or a key, quoted.
            (
                "event.items.0.price",
                "position 11: unexpected character `.`: a key that starts with a digit is \
                 quoted, `.\"0\"`",
            ),
            (
                "event.\"a\".5x = 1",
                "starts with a digit is quoted, `.\"5x\"`",
            ),
            ("x.ş.2", "`.\"2\"`"),
        ] {
            let e = tokenize(text).unwrap_err().to_string();
            assert!(e.contains(words), "{text}: {e}");
        }
        // A control character never reaches the words.
        let e = tokenize("event.\"a\nb\"").unwrap_err().to_string();
        assert!(!e.contains('\n'), "{e}");
        // `1.5`, `$a.5`, `a .5` and `x.*.5` stay what they were.
        assert_eq!(toks("1.5")[..1], [Tok::Float(1.5)]);
        for text in ["$a.5", "a .5", "x.*.5"] {
            let e = tokenize(text).unwrap_err().to_string();
            assert!(!e.contains("digit"), "{text}: {e}");
        }
    }

    /// The shell's gate closes a quoted key where the lexer does: at its
    /// `"`, and at a line's end, where the lexer refuses it -- an open key
    /// leaves the text open only at the text's end -- while a `"` after
    /// no name's dot is a string still, which a line's end leaves open.
    #[test]
    fn the_gate_closes_a_key_at_a_line_s_end() {
        let open = |t: &str| {
            let mut g = Unclosed::default();
            g.feed(t);
            g.is_open()
        };
        assert!(open("get t where event.\"order"));
        assert!(!open("get t where event.\"order\n"));
        assert!(!open("get t where event.\"order-id\" = 1"));
        assert!(!open("get t where event.\"a\".\"b\".c.\"d\" = 'x'"));
        assert!(open("get t where event.\"a\".\"b"));
        assert!(
            !open("get t where event.\"a\\\" = 1"),
            "no `\\` escapes a key"
        );
        assert!(open("get t where x = .\"a\nb"), "a string after a lone dot");
        assert!(
            open("get t where x = \"a\".\"b\nc"),
            "a string after a text's dot"
        );
        assert!(open("get t where ş.\"a"));
    }

    #[test]
    fn numbers_and_params() {
        let t = tokenize("[-0.5, 1e-3, 42] $2").unwrap();
        assert_eq!(t[1].tok, Tok::Float(-0.5));
        assert_eq!(t[3].tok, Tok::Float(1e-3));
        assert_eq!(t[5].tok, Tok::Int(42));
        assert_eq!(t[7].tok, Tok::Param(1));
    }

    /// The tokenizer as it was, over a `Vec<char>`: what the byte walk is
    /// held to.
    fn tokenize_chars(src: &str) -> Result<Vec<Token>> {
        let b: Vec<char> = src.chars().collect();
        let mut i = 0usize;
        let mut out = Vec::new();

        while i < b.len() {
            let c = b[i];
            // whitespace
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            // comment: -- to end of line, # is accepted too
            if c == '#' || (c == '-' && i + 1 < b.len() && b[i + 1] == '-') {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            let start = i;
            let tok = match c {
                '{' => {
                    i += 1;
                    Tok::LBrace
                }
                '}' => {
                    i += 1;
                    Tok::RBrace
                }
                '(' => {
                    i += 1;
                    Tok::LParen
                }
                ')' => {
                    i += 1;
                    Tok::RParen
                }
                '[' => {
                    i += 1;
                    Tok::LBracket
                }
                ']' => {
                    i += 1;
                    Tok::RBracket
                }
                ',' => {
                    i += 1;
                    Tok::Comma
                }
                ':' => {
                    i += 1;
                    Tok::Colon
                }
                '@' => {
                    i += 1;
                    Tok::At
                }
                '*' => {
                    i += 1;
                    Tok::Star
                }
                '~' => {
                    i += 1;
                    Tok::Tilde
                }
                '+' => {
                    i += 1;
                    Tok::Plus
                }
                '/' => {
                    i += 1;
                    Tok::Slash
                }
                '|' if i + 1 < b.len() && b[i + 1] == '|' => {
                    i += 2;
                    Tok::Plus
                }
                '-' if !(i + 1 < b.len() && b[i + 1].is_ascii_digit()) || subtracts(out.last()) => {
                    i += 1;
                    Tok::Minus
                }
                ';' => {
                    i += 1;
                    Tok::Semi
                }
                '=' => {
                    i += 1;
                    if i < b.len() && b[i] == '=' {
                        i += 1;
                    }
                    Tok::Eq
                }
                '!' => {
                    i += 1;
                    if i < b.len() && b[i] == '=' {
                        i += 1;
                        Tok::Ne
                    } else {
                        return Err(Error::Query(format!(
                            "position {start}: `!` alone is invalid"
                        )));
                    }
                }
                // pgvector's distances, read since SQL's spellings were.
                '<' if matches!(b.get(i + 1), Some('-' | '=' | '#'))
                    && b.get(i + 2) == Some(&'>') =>
                {
                    let op = match b[i + 1] {
                        '-' => "<->",
                        '=' => "<=>",
                        _ => "<#>",
                    };
                    i += 3;
                    Tok::Distance(op)
                }
                '<' => {
                    i += 1;
                    if i < b.len() && b[i] == '=' {
                        i += 1;
                        Tok::Le
                    } else if i < b.len() && b[i] == '>' {
                        i += 1;
                        Tok::Ne
                    } else {
                        Tok::Lt
                    }
                }
                '>' => {
                    i += 1;
                    if i < b.len() && b[i] == '=' {
                        i += 1;
                        Tok::Ge
                    } else {
                        Tok::Gt
                    }
                }
                // A named statement's `$from`, since named statements.
                #[cfg(feature = "statements")]
                '$' if b.get(i + 1).is_some_and(char::is_ascii_alphabetic) => {
                    i += 1;
                    let s = i;
                    let word = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
                    loop {
                        while b.get(i).is_some_and(word) {
                            i += 1;
                        }
                        match (b.get(i), b.get(i + 1)) {
                            (Some('.'), Some(c)) if c.is_ascii_alphabetic() || *c == '_' => i += 1,
                            _ => break,
                        }
                    }
                    Tok::Named(b[s..i].iter().collect())
                }
                '$' => {
                    i += 1;
                    let s = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if s == i {
                        return Err(Error::Query(format!(
                            "position {start}: expected a number after `$`"
                        )));
                    }
                    let n: usize = b[s..i].iter().collect::<String>().parse().unwrap();
                    if n == 0 {
                        return Err(Error::Query("parameters start at $1".into()));
                    }
                    Tok::Param(n - 1)
                }
                '"' | '\'' => {
                    let quote = c;
                    i += 1;
                    let mut s = String::new();
                    loop {
                        if i >= b.len() {
                            return Err(Error::Query(format!(
                                "position {start}: unterminated string"
                            )));
                        }
                        if b[i] == '\\' && i + 1 < b.len() {
                            i += 1;
                            s.push(match b[i] {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                '0' => '\0',
                                other => other,
                            });
                            i += 1;
                            continue;
                        }
                        if b[i] == quote {
                            i += 1;
                            if quote == '\'' && i < b.len() && b[i] == '\'' {
                                s.push('\'');
                                i += 1;
                                continue;
                            }
                            break;
                        }
                        s.push(b[i]);
                        i += 1;
                    }
                    match quote {
                        '"' => Tok::Str(s),
                        _ => Tok::Single(s),
                    }
                }
                c if c.is_ascii_digit()
                    || (c == '-' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) =>
                {
                    let s = i;
                    if b[i] == '-' {
                        i += 1;
                    }
                    let mut is_float = false;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == '_') {
                        i += 1;
                    }
                    if i < b.len() && b[i] == '.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                        is_float = true;
                        i += 1;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
                        is_float = true;
                        i += 1;
                        if i < b.len() && (b[i] == '+' || b[i] == '-') {
                            i += 1;
                        }
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    let text: String = b[s..i].iter().filter(|c| **c != '_').collect();
                    if is_float {
                        Tok::Float(fennec_core::num::parse_f64(&text).ok_or_else(|| {
                            Error::Query(format!("position {s}: invalid decimal number `{text}`"))
                        })?)
                    } else {
                        Tok::Int(text.parse().map_err(|_| {
                            Error::Query(format!("position {s}: invalid integer `{text}`"))
                        })?)
                    }
                }
                c if c.is_alphabetic() || c == '_' => {
                    let s = i;
                    loop {
                        while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                            i += 1;
                        }
                        // A path, `meta.lang`, since paths were read, and
                        // SQL's `o.*` since its spellings were.
                        match (b.get(i), b.get(i + 1)) {
                            (Some('.'), Some('*')) => {
                                i += 2;
                                break;
                            }
                            (Some('.'), Some(&n)) if n.is_alphabetic() || n == '_' => i += 1,
                            _ => break,
                        }
                    }
                    // A quoted key, since a webhook consumer's paths were
                    // read: the path read on, names and quoted keys; and a
                    // key no name reads as, `items.0`, refused naming it
                    // quoted.
                    #[cfg(feature = "statements")]
                    if b.get(i) == Some(&'.') && b[i - 1] != '*' {
                        let dot = i;
                        let refused =
                            |why: String| Err(Error::Query(format!("position {dot}: {why}")));
                        let digit = |i: usize| {
                            let key: String = b[i + 1..]
                                .iter()
                                .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                                .collect();
                            format!(
                                "unexpected character `.`: a key that starts with a digit is \
                                 quoted, `.\"{key}\"`"
                            )
                        };
                        if b.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
                            return refused(digit(i));
                        }
                        if b.get(i + 1) == Some(&'"') {
                            let mut path: String = b[s..i].iter().collect();
                            loop {
                                path.push('.');
                                if b.get(i + 1) == Some(&'"') {
                                    let mut j = i + 2;
                                    loop {
                                        match b.get(j) {
                                            None => return refused(UNTERMINATED.into()),
                                            Some('"') if j > i + 2 => break,
                                            Some(&c)
                                                if matches!(c, '"' | '\\' | '.' | '\u{7f}')
                                                    || (c as u32) < 0x20 =>
                                            {
                                                return refused(KEY_HOLDS.into())
                                            }
                                            Some(_) => j += 1,
                                        }
                                    }
                                    path.extend(&b[i + 2..j]);
                                    i = j + 1;
                                } else {
                                    let k = i + 1;
                                    let mut j = k;
                                    while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                                        j += 1;
                                    }
                                    path.extend(&b[k..j]);
                                    i = j;
                                }
                                match (b.get(i), b.get(i + 1)) {
                                    (Some('.'), Some(&n))
                                        if n == '"' || n.is_alphabetic() || n == '_' => {}
                                    (Some('.'), Some(n)) if n.is_ascii_digit() => {
                                        return refused(digit(i))
                                    }
                                    _ => break,
                                }
                            }
                            out.push(Token {
                                tok: Tok::Keys(path),
                                pos: start,
                            });
                            continue;
                        }
                    }
                    Tok::Ident(b[s..i].iter().collect())
                }
                other => {
                    return Err(Error::Query(format!(
                        "position {start}: unexpected character `{other}`"
                    )))
                }
            };
            out.push(Token { tok, pos: start });
        }
        out.push(Token {
            tok: Tok::Eof,
            pos: b.len(),
        });
        Ok(out)
    }

    /// Generated texts of every kind of token, whitespace, comment and
    /// mistake, ASCII and not: the byte walk gives the tokens, positions
    /// and errors the character walk gave.
    #[test]
    fn the_byte_walk_reads_as_the_char_walk_did() {
        let pieces = [
            "get",
            "docs",
            "şehir",
            "名前",
            "_x1",
            "a_b",
            "Ω",
            " ",
            "  ",
            "\t",
            "\n",
            "\u{0B}",
            "\u{A0}",
            "\u{3000}",
            "\u{2028}",
            "{",
            "}",
            "(",
            ")",
            "[",
            "]",
            ",",
            ":",
            "@",
            "*",
            "~",
            ";",
            "=",
            "==",
            "!=",
            "!",
            "<",
            "<=",
            "<>",
            ">",
            ">=",
            "$1",
            "$23",
            "$",
            "$0",
            "12",
            "-3",
            "-3.5",
            "1_000",
            "1e5",
            "2.5E-3",
            "7e",
            "1.",
            "-",
            "+",
            "/",
            "n-1",
            "-x",
            "--c\n",
            "-- c",
            "#c\n",
            "#",
            "\"s\"",
            "'q'",
            "''",
            "'a''b'",
            "||",
            "\"a\\\"b\"",
            "'x\\n'",
            "\"ü\\ğ\"",
            "\"unterminated",
            "'",
            "\\",
            "\"a\\",
            "é",
            "\u{301}",
            "𝔸",
            "%",
            "^",
            "&",
            "|",
            "+",
            "/",
            "?",
            ".",
            "0.5",
            "-0",
            "99999999999999999999",
            "1e400",
            "_",
            "x",
            "\u{0}",
        ];
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..40_000 {
            let mut text = String::new();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            for _ in 0..(x % 12) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                text.push_str(pieces[(x % pieces.len() as u64) as usize]);
            }
            // The character walk unwrapped a parameter's number, and a
            // number past `usize` was a panic; the byte walk refuses it.
            let b = match std::panic::catch_unwind(|| tokenize_chars(&text)) {
                Ok(b) => b,
                Err(_) => {
                    assert!(tokenize(&text).is_err(), "{text:?}");
                    continue;
                }
            };
            let a = tokenize(&text);
            match (&a, &b) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{text:?}"),
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{text:?}"),
                _ => panic!("{text:?}: {a:?} against {b:?}"),
            }
        }
    }

    /// [`Unclosed`] fed a generated text in pieces, cut anywhere: where
    /// the lexer reads the text it agrees on the brackets left open and on
    /// the string, and where the lexer finds a string unterminated it is in
    /// one -- over quotes, escapes, comments and brackets, ASCII and not.
    #[test]
    fn the_gate_reads_as_the_lexer() {
        let pieces = [
            "get",
            " ",
            "\n",
            "\t",
            "(",
            ")",
            "[",
            "]",
            "{",
            "}",
            "\"",
            "'",
            "\\",
            "\\\"",
            "--",
            "-",
            "#",
            ";",
            "a",
            "1",
            "1e5",
            "7e",
            "-3",
            "x - 1",
            "[1, 2]",
            "\"a;b\"",
            "'it''s'",
            "-- it's\n",
            "# \"\n",
            "ş",
            "名",
            "\u{A0}",
            ",",
            ":",
            "\r\n",
            // pgvector's `<#>`, no comment, beside the `#` that begins one.
            "<",
            ">",
            "<#>",
            "<#",
            "<->",
            // A quoted key, after a name's dot, and a dot after anything.
            ".",
            "\u{1}",
        ];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        for _ in 0..50_000 {
            let mut text = String::new();
            for _ in 0..rnd(14) {
                text.push_str(pieces[rnd(pieces.len())]);
            }
            let mut gate = Unclosed::default();
            let mut at = 0;
            while at < text.len() {
                let mut to = (at + 1 + rnd(6)).min(text.len());
                while !text.is_char_boundary(to) {
                    to += 1;
                }
                gate.feed(&text[at..to]);
                at = to;
            }
            match tokenize(&text) {
                Ok(toks) => {
                    let depth: i64 = toks
                        .iter()
                        .map(|t| match t.tok {
                            Tok::LParen | Tok::LBracket | Tok::LBrace => 1,
                            Tok::RParen | Tok::RBracket | Tok::RBrace => -1,
                            _ => 0,
                        })
                        .sum();
                    assert_eq!(gate.quote, 0, "{text:?}");
                    assert_eq!(gate.depth, depth, "{text:?}");
                }
                Err(e) if e.to_string().ends_with(UNTERMINATED) => {
                    assert!(gate.quote != 0, "{text:?}")
                }
                Err(_) => {}
            }
        }
    }

    #[test]
    fn comments_skipped() {
        let t = tokenize("get docs -- comment\n# another comment\nlimit 1").unwrap();
        let idents: Vec<String> = t
            .iter()
            .filter_map(|x| match &x.tok {
                Tok::Ident(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(idents, vec!["get", "docs", "limit"]);
    }
}
