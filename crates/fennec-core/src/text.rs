//! Full-text index: a tokenizer, an inverted index and BM25.
//!
//! **Why a sparse structure instead of reusing the vector index.** Measured on
//! BEIR SciFact (5 183 documents, 300 queries): this index scores nDCG@10
//! 0.662 against 0.645 for a 90 MB transformer, a gap a paired bootstrap
//! cannot separate from zero (p = 0.18). It gets there out of 9.1 MB of
//! postings over 7.8 MB of documents. The same corpus encoded as
//! 4096-dimensional hashed term vectors reaches the same 0.662 and costs
//! 85 MB of arena, or 21 MB quantised to int8 -- and in a database whose
//! whole file is resident that is the whole argument. Lexical retrieval is a
//! sparse problem and wants a sparse structure.
//!
//! Queries are answered with MaxScore pruning, so a common term with a long
//! postings list costs its walk only while it can still change the answer.
//!
//! **What it is not: semantic.** On BEIR FiQA, where questions share little
//! vocabulary with the answers that resolve them, this index scores 0.232
//! against the same transformer's 0.368 -- a gap that *is* significant. No
//! amount of tuning closes it, because no amount of counting words recovers a
//! meaning the words do not carry. That is what `rerank` is for: this index
//! chooses the candidates, stored vectors order them.

// Without the `text` feature the index's code is here but unused: the index
// is a type of no value (`off.rs`), and the compiler drops the rest. The
// tokenizer stays, for what else splits text.
#![cfg_attr(not(feature = "text"), allow(dead_code, unused_imports))]

use crate::error::{Error, Result};
#[cfg(feature = "text")]
use crate::fuzzy::{typos, Dictionary};
use crate::maps::Map;
use crate::schema::TextIndexSpec;
#[cfg(feature = "text")]
use crate::store::DocMap;
use crate::value::DocId;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
#[cfg(feature = "text")]
use terms::{List, Postings, Terms};

#[cfg(feature = "text")]
mod terms;

// Written by `tools/fold/gen.py`: what a Latin or Greek letter folds to.
mod fold;

/// How loosely a `match` reads its query's words: `match body $1 fuzzy`,
/// `fuzzy 1`, `prefix`. Plain -- no edits, no prefix -- is a `match` as it
/// always was, down the same code.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Loose {
    /// The most edits a word may take, 0 to 2, each word held besides to
    /// what its length allows ([`crate::fuzzy::typos`]): `fuzzy` alone is
    /// 2, so the length decides.
    pub fuzzy: u8,
    /// The query's last word also matches the words it begins: what a
    /// search box sends as it is typed into.
    pub prefix: bool,
}

impl Loose {
    pub fn is_plain(&self) -> bool {
        self.fuzzy == 0 && !self.prefix
    }
}

/// The rows a statement reads, where it reads fewer than the index holds,
/// as a test of a row: a scoped token's `match`, the rows its filter
/// selects ([`crate::query::Match::within`]). `None`: every row. Under
/// `@text(accents)` it decides whether a word typed with accents reads its
/// spelling, which only a row the statement reads may decide
/// (`TextIndex::holds`).
pub type Reads<'a> = Option<&'a dyn Fn(DocId) -> bool>;

/// The most words of an index one word of a query may reach through
/// `fuzzy` or `prefix`, itself among them. Past it the query is refused,
/// never cut: a word read as the first 10 000 of its 30 000 completions is
/// a wrong answer believed right. 10 000 as `near` and `match` answer at
/// most 10 000 rows: FiQA's 80 426 words hold no prefix of one letter
/// reaching more (`s` reaches 7 020, `co` 2 245, `con` 794), so a search
/// box is refused only at a first letter over a larger vocabulary. Each
/// word reached is a list merged into its word's: over 100 000 documents of
/// 60 000 made-up words a word's first two letters took 3.0 ms, its first
/// three 0.44 (`make fuzzy-bench`).
pub const MAX_EXPANSIONS: usize = 10_000;

/// What a word found near a query's word counts for, by its distance: one
/// it begins (`prefix`), one edit away, two. Of the weight the word's
/// variants of that distance carry together, as one term (`Reach`).
/// Measured on BEIR with a typo in each query (`make beir`, nDCG@10), a
/// word one edit away is the word meant -- at the whole weight SciFact's
/// typed queries went 0.636 -> 0.663, at half 0.657 -- and two away is as
/// often another word: at the whole weight FiQA's went 0.2125 -> 0.2261, at
/// a quarter 0.2292.
#[cfg(feature = "text")]
const NEAR_WEIGHT: [f32; 3] = [1.0, 1.0, 0.25];

/// What a derived index of the text index's is held in: a lock natively,
/// a second reader waiting for the first one's build, a cell in the
/// browser, which has one thread -- as `engine::Derived` holds an index.
#[cfg(all(feature = "text", not(target_arch = "wasm32")))]
type Lazy<T> = std::sync::OnceLock<T>;
#[cfg(all(feature = "text", target_arch = "wasm32"))]
type Lazy<T> = std::cell::OnceCell<T>;

/// Calls `f` with every term in `text`, lowercased, I-folded and its
/// accents folded.
///
/// The ASCII path hands out a borrowed slice and allocates nothing; anything
/// else folds through the Unicode default case mapping into a reused buffer.
/// That is the same trade `query::like_match` makes and for the same reason --
/// dropping the folding path would lose `ÇALIŞMA` matching `çalışma`, which
/// every non-English corpus depends on.
///
/// **The dotted and dotless I are folded together**, and that is not what the
/// default mapping does. Unicode is locale-blind, so `İ` (U+0130) lowercases
/// to `i` followed by a COMBINING DOT ABOVE -- a two-code-point term nobody
/// can type, so `İstanbul` was unreachable from `istanbul`. `I` went to `i`
/// while `ı` stayed, so `IŞIK` missed `ışık` as well.
///
/// `I`, `İ`, `ı` and `i` therefore all index as `i`, and a stray combining dot
/// is dropped. The cost is the `kız`/`kiz` distinction, a real Turkish minimal
/// pair; the gain is that case stops deciding whether a word is findable.
///
/// Measured on the Turkish WebFAQ retrieval set (144 846 documents, 10 000
/// queries), nDCG@10 moves 0.4823 -> 0.4841 when the queries are capitalised
/// the way the corpus is, and 0.4729 -> 0.4841 when they are typed in
/// lowercase the way a person actually types them. Small both times, because
/// in that corpus query and document mostly break the same way and so still
/// meet; the point is the second column, where after the fold the two are the
/// same number. Case no longer decides.
///
/// **A Latin or Greek letter folds to its base letter** (`fold.rs`, written
/// by `tools/fold/gen.py`): `Sütlaç`, `sütlaç` and `sutlac` are one term,
/// so a word typed on a keyboard without the letters finds the word
/// written with them. nDCG@10 by BM25 over WebFAQ's queries typed without
/// accents went 0.4095 -> 0.4835 in Turkish, 0.2113 -> 0.6005 in
/// Vietnamese and 0.3119 -> 0.6002 in Greek. A combining mark after such a
/// letter, or after ASCII, is the accent the fold drops; after a letter of
/// a script that stays as written it stays in the word with it. Under
/// `@text(accents)` a word whose accents make it another is indexed as
/// written as well (`WRITTEN`, [`indexed_terms`]), which a query typed with
/// accents reads (`TextIndex::read_words`).
///
/// **A script written without spaces is indexed in runs of characters.** Han,
/// kana and Hangul, and Thai, Lao, Khmer and Myanmar, run their words
/// together or hang particles on them, so a run of them is indexed as its
/// overlapping pairs of characters -- `東京都に` is `東京`, `京都`, `都に` --
/// or triples for the four whose characters are letters rather than
/// syllables, and a query's find the documents that hold them anywhere.
/// Whole, a run was one term no query repeated. nDCG@10 by BM25, before and
/// after: C-MTEB's EcomRetrieval 0.006 -> 0.439 and CovidRetrieval 0.162 ->
/// 0.868, WebFAQ's Chinese 0.146 -> 0.686, JaGovFaqs 0.119 -> 0.582, WebFAQ's
/// Korean 0.537 -> 0.682 and its Thai 0.448 -> 0.547 -- pairs of Thai
/// letters were 0.495 at a query three times as slow, too common to tell
/// documents apart.
pub fn for_each_term(text: &str, mut f: impl FnMut(&str)) {
    terms(text, false, &mut |t, _, _| f(t));
}

/// What a word's term as written starts with, beside its fold, where its
/// accents make it another word and the index keeps them
/// (`TextIndexSpec::accents`): `Sütlaç` is `sutlac` and `~sütlaç`. A term
/// is letters and digits by construction, so the mark is free, as `^` is
/// for a prefix.
pub(crate) const WRITTEN: char = '~';

/// [`for_each_term`], each character of a run of Han, kana or Hangul handed
/// out as well as each pair when `chars` is set (`TextIndexSpec::chars`).
///
/// `f` is a trait object: generic over it, the tokenizer was compiled once
/// for every caller -- insert, remove and search, with prefixes and without
/// -- six copies and 8 KB of the browser module.
///
/// Each term comes with where it stands in `text`, its first byte and the
/// one past its last: what highlighting marks (`highlight.rs`), so that a
/// marked span is exactly the text a term the index matched was read from.
/// The index passes them over.
///
/// It knows no `accents`: a word as written is handed out by
/// [`indexed_terms`] around it, so the tokenizer every index runs tests no
/// flag for it. With the flag tested here a word at a time, an index
/// without the option paid for it: Russian 21.84 -> 22.68 ns a byte against
/// main, and 22.18 without it (docs/design/text.md#tokenizer).
pub(crate) fn terms(text: &str, chars: bool, f: &mut dyn FnMut(&str, usize, usize)) {
    let mut buf = String::new();
    // A combining mark is not alphanumeric, so a plain `is_alphanumeric`
    // split would cut `I\u{307}stanbul` -- the decomposed spelling of
    // `İstanbul`, and what the default mapping leaves behind -- or a
    // decomposed `re\u{301}sume\u{301}` into two terms. Keep it inside the
    // word; it is dropped below. So are a Thai or a Khmer word's marks, some
    // of which are not letters: split at them, `ไม่` lost its tone mark.
    for raw in text.split(splits_words) {
        if raw.is_empty() || raw.chars().all(combining) {
            continue;
        }
        // `split` hands out slices of `text`, so where one starts is how far
        // its pointer is past the text's.
        let base = raw.as_ptr() as usize - text.as_ptr() as usize;
        // ASCII is its own answer lowercased -- `I` folds to `i` -- and never
        // reaches the fold's table, so English pays nothing for it.
        if raw.is_ascii() {
            if raw.bytes().any(|b| b.is_ascii_uppercase()) {
                buf.clear();
                buf.push_str(raw);
                buf.make_ascii_lowercase();
                f(&buf, base, base + raw.len());
            } else {
                f(raw, base, base + raw.len());
            }
            continue;
        }
        // A run of a script written without spaces is its runs of characters
        // (`grams`), what is between them a word as any other.
        let mut rest = raw;
        while let Some(first) = rest.chars().next() {
            let n = gram(first);
            let end = rest
                .char_indices()
                .find(|&(_, c)| gram(c) != n)
                .map_or(rest.len(), |(i, _)| i);
            // Checked, though `end` is a boundary: `split_at`'s panic formats
            // the character it would cut into.
            let (run, after) = rest.split_at_checked(end).unwrap_or((rest, ""));
            let at = base + (raw.len() - rest.len());
            rest = after;
            if n > 0 {
                grams(run, at, n, chars && n == 2, f);
                continue;
            }
            buf.clear();
            // Whether a combining mark here is an accent the fold drops: one
            // after ASCII or a Latin or Greek letter is (`e\u{301}` is `é`,
            // so `e`; U+0307 is what the default mapping leaves behind for
            // `İ`); one after a letter of a script that stays as written
            // stays with it -- `е\u{308}` is `ё` written decomposed, which
            // folds to `е` no more than `ё` does.
            let mut drops = true;
            for c in run.chars() {
                if c.is_ascii() {
                    buf.push(c.to_ascii_lowercase());
                    drops = true;
                    continue;
                }
                match c {
                    '\u{0300}'..='\u{036F}' => {
                        if !drops {
                            buf.push(c);
                        }
                    }
                    'İ' | 'ı' => {
                        buf.push('i');
                        drops = true;
                    }
                    // A full-width Latin letter or digit, as Japanese and
                    // Chinese text writes them (`ＩＴ`, `２０２３`), is the
                    // ASCII one.
                    '\u{FF01}'..='\u{FF5E}' => {
                        buf.push(full_width(c));
                        drops = true;
                    }
                    // A letter neither the table nor its lowercase reaches --
                    // Cyrillic and every script past Greek but the Latin and
                    // Greek extensions, the Angström sign aside (its
                    // lowercase is `å`) -- is lowercased as it was before
                    // anything folded, its two lookups of the table spared.
                    // `every_character_folds_as_its_lowercase_and_once` holds
                    // the ranges to the table.
                    '\u{0400}'..='\u{1DFF}' | '\u{2000}'..='\u{212A}' | '\u{212C}'.. => {
                        buf.extend(c.to_lowercase());
                        drops = false;
                    }
                    // A letter the table folds is lowercase folded; any other
                    // is lowercased, and what that gives folded in turn (the
                    // Angström sign is `å`, so `a`). Latin and Greek drop the
                    // marks after them, folded or not.
                    _ => {
                        if !fold_into(&mut buf, c) {
                            for l in c.to_lowercase() {
                                if !fold_into(&mut buf, l) {
                                    buf.push(l);
                                }
                            }
                        }
                        drops = true;
                    }
                }
            }
            // A run of marks alone is no word.
            if buf.is_empty() {
                continue;
            }
            f(&buf, at, at + run.len());
        }
    }
}

/// Every overlapping run of `n` characters in `run` -- the whole of it when it
/// is shorter -- and each character as well with `chars`. The starts of the
/// last `n` characters are held in a ring, so a run allocates nothing.
/// `at` is where the run starts in the text the spans are of.
fn grams(run: &str, at: usize, n: usize, chars: bool, f: &mut dyn FnMut(&str, usize, usize)) {
    let mut starts = [0usize; 3];
    let mut seen = 0;
    for (i, c) in run.char_indices() {
        let end = i + c.len_utf8();
        // `get`: the ends are the characters' own, and an index would keep
        // its panic's formatting of a `char` in the browser module.
        if chars {
            f(run.get(i..end).unwrap_or(""), at + i, at + end);
        }
        starts[seen % n] = i;
        seen += 1;
        if seen >= n {
            // The oldest of the last `n` characters: the one `n` back.
            let from = starts[seen % n];
            f(run.get(from..end).unwrap_or(""), at + from, at + end);
        }
    }
    if seen < n && !(chars && seen == 1) {
        f(run, at, at + run.len());
    }
}

/// A full-width Latin letter or digit (`ＩＴ`, `２０２３`, U+FF01 to U+FF5E)
/// as the ASCII one, lowercase.
fn full_width(c: char) -> char {
    char::from_u32(c as u32 - 0xFEE0)
        .unwrap_or(c)
        .to_ascii_lowercase()
}

/// `run`, a word `terms` folded to `fold`, as written into `out`, behind
/// [`WRITTEN`] -- lowercased, the `I` forms folded and a full-width letter
/// its ASCII one, as the index read every word before it folded accents, a
/// combining accent kept -- and whether its accents make it another word
/// than its fold. ASCII is its fold already. A run of a script written
/// without spaces never reaches it: its terms are slices of the text,
/// which [`indexed_terms`] asks nothing of.
///
/// Case decides no more than it does for the fold: a letter whose
/// uppercase lowercases to another letter is that letter, as the fold
/// reads it -- the final `ς` is `σ`, which `Σ` lowercases to wherever it
/// stands, `ſ` is `s` and the Greek symbol forms their letters. Kept as it
/// is, `ς` made every Greek word ending in it a term as written with no
/// accent to it, and `οδος` found what `ΟΔΟΣ` did not.
///
/// A second pass over a word the first folded, made only under
/// `@text(accents)`: in the one pass, beside the fold, every index paid
/// for a second buffer it never handed out.
fn as_written(run: &str, fold: &str, out: &mut String) -> bool {
    if run.is_ascii() {
        return false;
    }
    out.clear();
    out.push(WRITTEN);
    for c in run.chars() {
        match c {
            _ if c.is_ascii() => out.push(c.to_ascii_lowercase()),
            'İ' | 'ı' => out.push('i'),
            // What the default mapping leaves behind for `İ`.
            '\u{0307}' => {}
            '\u{FF01}'..='\u{FF5E}' => out.push(full_width(c)),
            'ς' => out.push('σ'),
            'ſ' => out.push('s'),
            'ϐ' => out.push('β'),
            'ϑ' => out.push('θ'),
            'ϕ' => out.push('φ'),
            'ϖ' => out.push('π'),
            'ϰ' => out.push('κ'),
            'ϱ' => out.push('ρ'),
            'ϵ' => out.push('ε'),
            // A long s with a dot above, and the prosgegrammeni, `ι` apart.
            'ẛ' => out.push('ṡ'),
            '\u{1FBE}' => out.push('ι'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out.len() != fold.len() + 1 || !out.ends_with(fold)
}

/// Where text is cut into words: at what is neither a letter nor a digit,
/// a combining mark or a mark a word of a script holds.
pub(crate) fn splits_words(c: char) -> bool {
    !c.is_alphanumeric() && !combining(c) && !marked(c)
}

/// A combining diacritical mark (U+0300 to U+036F): the accents a letter
/// written decomposed carries after it, kept inside its word and dropped
/// from its term after a Latin or Greek letter.
pub(crate) fn combining(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}')
}

/// Pushes what the table folds `c` to (`fold.rs`, written by
/// `tools/fold/gen.py`), its base letter or letters lowercase -- `Ş` and `ş`
/// are `s`, `ǽ` is `ae`, `ά` is `α` -- and says whether it held one.
fn fold_into(buf: &mut String, c: char) -> bool {
    let byte = fold::at(c as u32)
        .and_then(|at| fold::TABLE.get(at))
        .copied()
        .unwrap_or(0);
    match byte {
        0 => return false,
        1..=0x1F => buf.push_str(fold::PAIRS.get(byte as usize - 1).copied().unwrap_or("")),
        0x20..=0x7F => buf.push(byte as char),
        _ => buf.push(char::from_u32(0x300 + byte as u32).unwrap_or(c)),
    }
    true
}

/// How a run of `c`'s script is indexed: in overlapping runs of 2
/// characters for Han, kana and Hangul, whose characters are syllables or
/// words, and of 3 for Thai, Lao, Khmer and Myanmar, whose are letters; 0
/// for a script that spaces its words, whose words are the terms.
pub(crate) fn gram(c: char) -> usize {
    match c as u32 {
        0x0E00..=0x0EFF      // Thai, Lao
        | 0x1000..=0x109F    // Myanmar
        | 0x1780..=0x17FF => 3, // Khmer
        0x1100..=0x11FF      // Hangul jamo
        | 0x3005..=0x3007    // 々, 〆, 〇
        | 0x3041..=0x30FF    // Hiragana, Katakana
        | 0x3131..=0x318E    // Hangul compatibility jamo
        | 0x31F0..=0x31FF    // Katakana phonetic extensions
        | 0x3400..=0x4DBF    // Han, extension A
        | 0x4E00..=0x9FFF    // Han
        | 0xA960..=0xA97F    // Hangul jamo extended-A
        | 0xAC00..=0xD7FF    // Hangul syllables, jamo extended-B
        | 0xF900..=0xFAFF    // Han compatibility
        | 0xFF66..=0xFFDC    // half-width katakana and Hangul
        | 0x20000..=0x3FFFF => 2, // Han of the other planes
        _ => 0,
    }
}

/// A mark of a script whose words hold marks that are not letters -- a Thai
/// tone mark, a Khmer or Myanmar sign -- kept inside the word.
fn marked(c: char) -> bool {
    matches!(c as u32, 0x0E31..=0x0E4E | 0x0EB1..=0x0ECD | 0x102B..=0x103E | 0x17B4..=0x17D3)
}

/// `for_each_term` collected. For queries and tests, where the allocation is
/// noise next to the postings walk that follows.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for_each_term(text, |t| out.push(t.to_string()));
    out
}

/// Every term a document contributes under `spec`: the words, and their
/// prefixes when the index asks for them.
///
/// The prefixes are marked with a leading `^` so they cannot collide with a
/// real word of the same length -- a term is alphanumeric by construction, so
/// the marker is free. They count towards the document length as well, which
/// is what keeps BM25's normalisation meaningful: every document grows by
/// roughly the same factor, so `dl / avgdl` is where it was.
pub fn for_each_indexed_term(text: &str, spec: &TextIndexSpec, mut f: impl FnMut(&str)) {
    indexed_terms(text, spec, &mut |t, _, _| f(t));
}

/// [`for_each_indexed_term`], each term with the span of `text` it was read
/// from: a prefix the span of its whole word, which is what it matched.
///
/// With `accents` (`TextIndexSpec::accents`), a word whose accents make it
/// another word is handed out twice, on the same span: its fold, then its
/// form as written behind [`WRITTEN`] ([`as_written`]), its prefixes after
/// them -- a word's prefixes are its fold's. Around the tokenizer, so that
/// an index without the option, or prefixes, hands it `f` as it is, and
/// one with either pays one call a word for both: nested, one closure for
/// each, `@text(accents)` paid two.
pub(crate) fn indexed_terms(
    text: &str,
    spec: &TextIndexSpec,
    f: &mut dyn FnMut(&str, usize, usize),
) {
    let (prefixes, accents) = (spec.prefixes(), spec.accents);
    if prefixes.is_none() && !accents {
        return terms(text, spec.chars, f);
    }
    let (mut buf, mut written) = (String::new(), String::new());
    let start = text.as_ptr() as usize;
    terms(text, spec.chars, &mut |w, from, to| {
        f(w, from, to);
        // A term handed out as the text itself -- a word of lowercase
        // ASCII, a run of Han or Thai, every gram of which is a slice of
        // it -- is its own spelling: asked no more, as the tokenizer's
        // ASCII path asked nothing before the second pass moved out of it.
        if accents
            && w.as_ptr() as usize != start + from
            && as_written(text.get(from..to).unwrap_or(""), w, &mut written)
        {
            f(&written, from, to);
        }
        let Some(prefixes) = &prefixes else {
            return;
        };
        // `chars`, not bytes: a Turkish word is not one byte per letter, and
        // slicing it as if it were would panic on a boundary.
        let n = w.chars().count();
        for k in prefixes.clone() {
            if n <= k {
                break; // the whole word is already indexed above
            }
            buf.clear();
            buf.push('^');
            buf.extend(w.chars().take(k));
            f(&buf, from, to);
        }
    });
}

/// An inverted index with BM25 scoring.
///
/// Like the HNSW graph this is derived data -- it is built from the
/// documents by the first statement that reads it after an open. Unlike the
/// graph it is *not* persisted: building it measures 16 us per document
/// (SciFact, 85 ms for 5 183) against the graph's ~44 us. A second record
/// kind, and the validation path that would have to come with it, was not
/// worth ~2 s per 100 000 documents.
#[cfg(feature = "text")]
pub struct TextIndex {
    pub spec: TextIndexSpec,
    /// term -> postings, kept ascending by document id so `search` can merge
    /// them without sorting ([`terms`]). Its table grows a shard at a time:
    /// a field whose rows bring terms of their own -- an id, a code, a name
    /// -- took its doublings whole under the write lock, 125-132 ms at 1.8
    /// million terms and 276-324 at 3.7 million.
    postings: Terms,
    /// document -> term count, for the length normalisation.
    /// Each document's number of terms: none is indexed without one, so
    /// 0 is none.
    lengths: DocMap,
    total_terms: u64,
    /// The words in order, for `fuzzy` and `prefix`: sorted by the first
    /// `match` that asks either, kept up by the writes from then on, as a
    /// derived index is ([`crate::fuzzy`]). An index no `match` reads
    /// loosely never makes it.
    words: Lazy<Dictionary>,
    /// The most terms a document has held: the longest a document can be,
    /// below which a word's own term outscores its variants (`Reach`).
    /// Only ever raised, as `Postings::max_tf` is.
    longest: u32,
}

#[cfg(feature = "text")]
impl TextIndex {
    pub fn new(spec: TextIndexSpec) -> TextIndex {
        TextIndex {
            spec,
            postings: Terms::default(),
            lengths: DocMap::default(),
            total_terms: 0,
            words: Lazy::new(),
            longest: 0,
        }
    }

    /// The words in order, sorted the first time they are asked for.
    fn dictionary(&self) -> &Dictionary {
        self.words
            .get_or_init(|| Dictionary::of_bytes(self.postings.words()))
    }

    pub fn len(&self) -> usize {
        self.lengths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lengths.len() == 0
    }

    /// The terms it keeps: its words, and their prefixes (`^`) and, under
    /// `accents`, their forms as written (`~`) where it keeps them.
    pub fn terms(&self) -> usize {
        self.postings.len()
    }

    pub fn postings_count(&self) -> usize {
        self.postings.postings()
    }

    /// Postings, the term dictionary and the length table, as they sit in
    /// memory -- each list's arrays with what an allocator takes past
    /// them -- and the sorted words once a `match` has asked for them. Not
    /// RSS, the same caveat as `Database::memory_bytes`, but within a tenth
    /// of what musl's allocator holds for them (`make text-memory`), where
    /// the count of a map, its table and its allocations left out, was a
    /// third of it for a tenant's notes and four fifths for FiQA.
    pub fn memory_bytes(&self) -> usize {
        self.postings.bytes() + self.lengths.bytes() + self.words.get().map_or(0, Dictionary::bytes)
    }

    /// Each of the document's terms goes straight into its list, a word at
    /// a time. Counted first in a map of the document's own, with a `String`
    /// for each term, SciFact's 5 183 documents took 160 ms to index against
    /// 85, and 20 000 short texts 21.9 ms in the browser module against 8.0.
    pub fn insert(&mut self, doc: DocId, text: &str) {
        let spec = self.spec;
        let mut n = 0u32;
        for_each_indexed_term(text, &spec, |t| {
            // A word as written (`accents`) is the word again: its fold is
            // counted, so a document is as long as it is without them.
            n += u32::from(!t.starts_with(WRITTEN));
            if self.postings.bump(t, doc) {
                if let Some(words) = self.words.get_mut() {
                    words.add(t);
                }
            }
        });
        if n == 0 {
            return;
        }
        self.longest = self.longest.max(n);
        if let Some(old) = self.lengths.insert(doc, n) {
            self.total_terms -= old as u64;
        }
        self.total_terms += n as u64;
    }

    /// Removes a document. `text` must be what was indexed -- every caller
    /// reads the stored document first, so the terms are the right ones.
    ///
    /// A list the document leaves empty goes as the document leaves it:
    /// `retain` over the dictionary afterwards walked every term, 0.38 ms a
    /// delete -- or an update of any field -- at 200 000 documents.
    pub fn remove(&mut self, doc: DocId, text: &str) {
        let spec = self.spec;
        for_each_indexed_term(text, &spec, |t| {
            if self.postings.drop_doc(t, doc) {
                if let Some(words) = self.words.get_mut() {
                    words.remove(t);
                }
            }
        });
        if let Some(n) = self.lengths.remove(doc) {
            self.total_terms -= n as u64;
        }
    }

    /// Gives back the growth slack of every postings list, of the table
    /// and of the arrays the terms are in.
    ///
    /// `Vec` grows by doubling, so a list built one document at a time sits
    /// at ~1.5x the bytes it needs -- 3.8 MB of the 13.0 MB SciFact index.
    /// That slack is the price of cheap appends and it is worth paying while
    /// documents are still arriving; it is not worth paying afterwards.
    /// Called where the index is known to be complete: its build from the
    /// documents, and `create index`.
    pub fn shrink_to_fit(&mut self) {
        self.postings.shrink_to_fit();
        self.lengths.shrink_to_fit();
    }

    pub fn clear(&mut self) {
        self.postings = Terms::default();
        self.lengths.clear();
        self.total_terms = 0;
        self.words = Lazy::new();
        self.longest = 0;
    }

    fn avgdl(&self) -> f32 {
        if self.lengths.len() == 0 {
            return 1.0;
        }
        (self.total_terms as f32 / self.lengths.len() as f32).max(1.0)
    }

    /// How many documents BM25 counts and their mean length: the index's,
    /// or those of `within` (ascending), each length summed as the index
    /// sums its own, so a set scores as an index holding it alone would.
    fn counted(&self, within: Option<&[DocId]>) -> (usize, f32) {
        let Some(w) = within else {
            return (self.lengths.len(), self.avgdl());
        };
        let total: u64 = w.iter().map(|&d| self.lengths.get(d) as u64).sum();
        let mean = match w.len() {
            0 => 1.0,
            n => (total as f32 / n as f32).max(1.0),
        };
        (w.len(), mean)
    }

    /// Every document `query` scores above zero, ascending: the rows a
    /// `match` beside `order`, `count` or an aggregate selects, a filter
    /// there rather than a ranking, and the rows `facet` counts beside
    /// either. Its terms are the cursors a ranking walks, weighed as
    /// [`Self::search_within`] and [`Self::search_loose`] weigh them --
    /// BM25's statistics over `within`, a scoped token's rows, where it is
    /// given -- and a document is in the set where a term weighing more
    /// than nothing holds it: a ranking keeps a document whose score is
    /// above zero, and its score is the sum of its terms' contributions,
    /// each its weight times a factor above zero. So the set is the
    /// ranking's to the document, a term so common its idf rounds to 0 in
    /// `f32` -- every one of 2^23 documents or more holding it -- adding
    /// none, as it adds none to a ranking.
    ///
    /// Its memory is bounded by the documents the index holds, never by
    /// the lists' sum (`ids_of`): never more than twice their count in
    /// ids -- where copying every list first held a query of 200 words,
    /// each in half of a million documents, as 100 million ids: 800 MB.
    pub fn scored(
        &self,
        query: &str,
        loose: Loose,
        within: Option<&[DocId]>,
    ) -> Result<Vec<DocId>> {
        let most = self.lengths.len();
        if most == 0 {
            return Ok(Vec::new());
        }
        let within = within.filter(|_| cfg!(not(target_arch = "wasm32")));
        let test = |d: DocId| within.is_some_and(|w| w.binary_search(&d).is_ok());
        let reads = within.map(|_| &test as &dyn Fn(DocId) -> bool);
        let qtf = self.query_terms(query, reads);
        let (n, avgdl) = self.counted(within);
        // A plain query's cursors are its terms' alone, as `search_within`
        // makes them; `loose_cursors` makes those first.
        let reach = match loose.is_plain() {
            true => Vec::new(),
            false => self.reach(query, loose, &qtf, reads)?,
        };
        let unions = unions(&reach);
        let cursors = self.loose_cursors(&qtf, &reach, &unions, n, avgdl, within);
        Ok(ids_of(&cursors, most))
    }

    /// Top `k` documents for `query`, best first.
    ///
    /// `accept` is the filter membership test; it is applied while the
    /// postings are merged, so a `where` clause never costs a second pass.
    pub fn search<F>(&self, query: &str, k: usize, accept: F) -> Vec<(DocId, f32)>
    where
        F: Fn(DocId) -> bool,
    {
        self.search_within(query, k, accept, None)
    }

    /// [`Self::search`], BM25's statistics taken over `within` (ascending)
    /// when it is given: its documents counted, their mean length, and each
    /// term's documents among them. A scoped token's `match` is scored so
    /// ([`crate::query::Match::within`]): over the collection a score said
    /// how many rows the token could not read held a term. A term none of
    /// them holds is passed over, its list unwalked; and a word typed with
    /// accents reads as `within` decides ([`Self::holds`]).
    pub fn search_within<F>(
        &self,
        query: &str,
        k: usize,
        accept: F,
        within: Option<&[DocId]>,
    ) -> Vec<(DocId, f32)>
    where
        F: Fn(DocId) -> bool,
    {
        if k == 0 || self.lengths.len() == 0 {
            return Vec::new();
        }
        // The browser holds no scoped token to ask for it: left in, the
        // counting was 151 bytes brotli of its module.
        let within = within.filter(|_| cfg!(not(target_arch = "wasm32")));
        let test = |d: DocId| within.is_some_and(|w| w.binary_search(&d).is_ok());
        let reads = within.map(|_| &test as &dyn Fn(DocId) -> bool);
        let qtf = self.query_terms(query, reads);
        let (n, avgdl) = self.counted(within);
        let mut cursors: Vec<Cursor> = Vec::with_capacity(qtf.len());
        for (term, count) in &qtf {
            if let Some(list) = self.postings.get(term) {
                cursors.extend(self.cursor(list, *count as f32, n, within));
            }
        }
        self.ranked(cursors, k, accept, avgdl)
    }

    /// The terms of `query` as the index reads them ([`Self::read_words`]),
    /// each with how often the query holds it. Repeated query terms fold
    /// into a weight: scoring the same postings list twice would give the
    /// same answer for twice the walk. One pass over the query, its
    /// prefixes among its terms.
    fn query_terms(&self, query: &str, reads: Reads) -> Map<String, u32> {
        let mut qtf: Map<String, u32> = Map::default();
        self.read_words(query, reads, &mut |t, _| match qtf.get_mut(t) {
            Some(c) => *c += 1,
            None => {
                qtf.insert(t.to_string(), 1);
            }
        });
        qtf
    }

    /// Each term of `query` as the index reads it, with its word's fold:
    /// the index's terms -- a word's fold, its prefixes -- but, under
    /// `@text(accents)`, a word written with accents as written where a row
    /// the statement reads holds it so ([`Self::holds`]). A word typed
    /// without them finds every spelling of it, and one typed with them
    /// finds what was written as it was typed, as the index found it before
    /// accents folded -- on Vietnamese, whose tones tell its syllables
    /// apart, the fold took nDCG@10 0.6471 -> 0.6006 -- unless no row the
    /// statement reads was: typed with an accent no such row writes it
    /// with, it finds every spelling.
    ///
    /// Ranking, `facet`, `highlight()` and `fuzzy` all read a query here,
    /// so a statement reads a word one way wherever it reads it.
    fn read_words(&self, query: &str, reads: Reads, f: &mut dyn FnMut(&str, &str)) {
        if !self.spec.accents {
            indexed_terms(query, &self.spec, &mut |t, _, _| f(t, t));
            return;
        }
        // A word's fold comes first, its form as written after it where it
        // has one, then the fold's prefixes.
        let mut fold = String::new();
        indexed_terms(query, &self.spec, &mut |t, _, _| {
            if t.starts_with('^') {
                f(t, t);
                return;
            }
            if t.starts_with(WRITTEN) {
                match self.holds(t, reads) {
                    true => f(t, &fold),
                    false => f(&fold, &fold),
                }
                fold.clear();
                return;
            }
            if !fold.is_empty() {
                f(&fold, &fold);
            }
            fold.clear();
            fold.push_str(t);
        });
        if !fold.is_empty() {
            f(&fold, &fold);
        }
    }

    /// Whether a row `reads` passes -- any row, without it -- holds the
    /// term `t`: what decides whether a word typed with accents reads its
    /// spelling or its fold. Asked of the rows a statement reads alone: a
    /// scoped token's `match` that read a spelling because a row it cannot
    /// read held it told it so -- its own `sutlac tarifi` found by
    /// `sütlaç` once bob wrote a row of `sütlaç`, and found no more once
    /// he did.
    fn holds(&self, t: &str, reads: Reads) -> bool {
        self.postings
            .get(t)
            .is_some_and(|l| reads.is_none_or(|r| l.docs.iter().any(|&d| r(d))))
    }

    /// The terms `query` reads (`query_terms`), what `highlight()` marks.
    pub fn read_terms(&self, query: &str, reads: Reads) -> Map<String, u32> {
        self.query_terms(query, reads)
    }

    /// A walk through `list` weighted `times` its idf, or none where
    /// `within` holds none of its documents and the term is passed over,
    /// its list unwalked.
    fn cursor<'a>(
        &self,
        list: Postings<'a>,
        times: f32,
        n: usize,
        within: Option<&[DocId]>,
    ) -> Option<Cursor<'a>> {
        let df = match within {
            None => list.len(),
            Some(w) => held_by(list.docs, w),
        };
        if df == 0 {
            return None;
        }
        let (k1, b) = (self.spec.k1(), self.spec.b());
        let weight = times * idf(n, df);
        // The most this term can ever add to a document: its largest `tf`,
        // against the most generous length normalisation there is (`dl` ->
        // 0). An over-estimate is what makes it safe.
        let tf = list.max_tf as f32;
        Some(Cursor {
            at: 0,
            list,
            weight,
            ceiling: weight * (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b)),
        })
    }

    /// [`Self::search_within`] with the query's words read loosely
    /// ([`Loose`]): each word's variants of one distance -- the words
    /// `fuzzy` finds within its edits, the words `prefix` finds the last
    /// word begins -- ranked as one more term, a list of the documents any
    /// of them holds, each document's counts summed ([`union`]). A
    /// document holding three spellings of a word scores as one holding
    /// the word three times, BM25's saturation and all, not three words
    /// over; and the term's idf is the union's, so a rare misspelling the
    /// corpus holds twice ranks no higher than the word it misspells.
    ///
    /// **A word's own term outranks its variants.** Their weights are
    /// scaled, where they would reach it, below the least the word itself
    /// adds to a document -- one occurrence in the longest document there
    /// is -- so of two documents, one holding a word of the query as typed
    /// and one only a variant of it, the first scores more from that word:
    /// a one-word query ranks every document holding it above every
    /// document holding only what is near it. A word no document holds
    /// as typed -- the typo, the half-typed word -- ranks by its variants
    /// alone: a completion of a prefix and a word one edit away at the
    /// whole weight, two edits away at a quarter (`NEAR_WEIGHT`).
    ///
    /// The variants are cursors as any term's, their ceilings their own,
    /// so MaxScore is as safe over them as over a plain query, which
    /// `pruning_never_changes_a_loose_answer` holds it to.
    pub fn search_loose<F>(
        &self,
        query: &str,
        k: usize,
        accept: F,
        within: Option<&[DocId]>,
        loose: Loose,
    ) -> Result<Vec<(DocId, f32)>>
    where
        F: Fn(DocId) -> bool,
    {
        if loose.is_plain() {
            return Ok(self.search_within(query, k, accept, within));
        }
        if k == 0 || self.lengths.len() == 0 {
            return Ok(Vec::new());
        }
        let within = within.filter(|_| cfg!(not(target_arch = "wasm32")));
        let test = |d: DocId| within.is_some_and(|w| w.binary_search(&d).is_ok());
        let reads = within.map(|_| &test as &dyn Fn(DocId) -> bool);
        let qtf = self.query_terms(query, reads);
        let reach = self.reach(query, loose, &qtf, reads)?;
        let (n, avgdl) = self.counted(within);
        let unions = unions(&reach);
        let cursors = self.loose_cursors(&qtf, &reach, &unions, n, avgdl, within);
        Ok(self.ranked(cursors, k, accept, avgdl))
    }

    /// What each word of `query` reaches read loosely, the words the query
    /// holds as terms of its own left out; a word whose variants pass
    /// [`MAX_EXPANSIONS`] refuses the query.
    fn reach<'a>(
        &'a self,
        query: &str,
        loose: Loose,
        qtf: &Map<String, u32>,
        reads: Reads,
    ) -> Result<Vec<Reach<'a>>> {
        // Each word's fold, what it reads and how often: its variants are
        // its fold's, scaled below what it reads. A prefix of an index of
        // them is no word.
        let mut words: Vec<(String, String, u32)> = Vec::new();
        let mut last = 0;
        self.read_words(query, reads, &mut |t, fold| {
            if t.starts_with('^') {
                return;
            }
            last = match words.iter().position(|w| w.1 == t) {
                Some(i) => {
                    words[i].2 += 1;
                    i
                }
                None => {
                    words.push((fold.to_string(), t.to_string(), 1));
                    words.len() - 1
                }
            };
        });
        let mut out = Vec::new();
        let mut near: Vec<(&str, u8)> = Vec::new();
        for (i, (w, read, count)) in words.iter().enumerate() {
            let k = typos(w.chars().count()).min(loose.fuzzy);
            let prefix = loose.prefix && i == last;
            if k == 0 && !prefix {
                continue;
            }
            near.clear();
            if !self
                .dictionary()
                .near(w, k, prefix, MAX_EXPANSIONS, &mut near)
            {
                let how = if prefix { "prefix" } else { "fuzzy" };
                return Err(Error::Query(format!(
                    "`match ... {how}` reads `{w}` as more than {MAX_EXPANSIONS} words of the \
                     index, and a word reaches {MAX_EXPANSIONS} at most: refused rather than cut"
                )));
            }
            let mut found = Vec::new();
            for &(t, d) in &near {
                // A word read as written has its fold, its other
                // spellings, among its variants, as near as a word it
                // begins.
                if (t == w && read == w) || qtf.contains_key(t) {
                    continue;
                }
                if let Some(list) = self.postings.get(t) {
                    found.push((t, d, list));
                }
            }
            out.push(Reach {
                count: *count,
                own: self.postings.get(read),
                found,
            });
        }
        Ok(out)
    }

    /// The query's own terms' cursors, then each word's variants', a
    /// distance a cursor, scaled below the word's own (`search_loose`).
    #[allow(clippy::too_many_arguments)]
    fn loose_cursors<'a>(
        &'a self,
        qtf: &Map<String, u32>,
        reach: &[Reach<'a>],
        unions: &'a [List],
        n: usize,
        avgdl: f32,
        within: Option<&[DocId]>,
    ) -> Vec<Cursor<'a>> {
        let mut cursors: Vec<Cursor> = Vec::with_capacity(qtf.len() + 3 * reach.len());
        for (term, count) in qtf {
            if let Some(list) = self.postings.get(term) {
                cursors.extend(self.cursor(list, *count as f32, n, within));
            }
        }
        let (k1, b) = (self.spec.k1(), self.spec.b());
        let mut next = unions.iter().map(List::view);
        for r in reach {
            let mark = cursors.len();
            for (d, scale) in NEAR_WEIGHT.iter().enumerate() {
                let lists = r.of(d as u8);
                let list = match lists.len() {
                    0 => continue,
                    1 => lists[0],
                    _ => match next.next() {
                        Some(u) => u,
                        None => continue,
                    },
                };
                cursors.extend(self.cursor(list, r.count as f32 * scale, n, within));
            }
            let Some(own) = r
                .own
                .and_then(|l| self.cursor(l, r.count as f32, n, within))
            else {
                continue;
            };
            // The least the word's own term adds to a document holding it:
            // once, in the longest document there is. A hundredth below it
            // survives the scores' rounding to `f32`.
            let longest = self.longest.max(1) as f32;
            let floor =
                0.99 * own.weight * (k1 + 1.0) / (1.0 + k1 * (1.0 - b + b * longest / avgdl));
            let most: f32 = cursors[mark..].iter().map(|c| c.ceiling).sum();
            if most > floor {
                for c in &mut cursors[mark..] {
                    c.weight *= floor / most;
                    c.ceiling *= floor / most;
                }
            }
        }
        cursors
    }

    /// The words `query` reaches read loosely besides its own terms: what
    /// `highlight()` marks beside them, its words read as `reads` decides.
    pub fn loose_words(&self, query: &str, loose: Loose, reads: Reads) -> Result<Vec<&str>> {
        if loose.is_plain() {
            return Ok(Vec::new());
        }
        let qtf = self.query_terms(query, reads);
        let reach = self.reach(query, loose, &qtf, reads)?;
        let mut out = Vec::new();
        for r in &reach {
            for f in &r.found {
                out.push(f.0);
            }
        }
        Ok(out)
    }

    /// The first `k` documents `cursors` reach, best first. Natively in
    /// line with each caller, as the walk was before a loose `match` shared
    /// it: called, FiQA's `match` took 0.261 -> 0.269 ms p50; in line, 0.257.
    /// The browser module's `match` moved by noise alone either way, so it
    /// keeps one copy.
    #[cfg_attr(not(target_arch = "wasm32"), inline(always))]
    fn ranked<F>(&self, cursors: Vec<Cursor>, k: usize, accept: F, avgdl: f32) -> Vec<(DocId, f32)>
    where
        F: Fn(DocId) -> bool,
    {
        if cursors.is_empty() {
            return Vec::new();
        }
        let (k1, b) = (self.spec.k1(), self.spec.b());

        // MaxScore (Turtle & Flood). Sorted by ceiling, `reach[i]` is the most
        // the first `i` terms can contribute together. Once the heap is full,
        // any term set whose reach falls short of the worst kept score can no
        // longer *start* a candidate: a document holding only those terms
        // cannot get in. Those terms stay scorable, they just stop driving the
        // frontier -- which is the whole saving, because the terms with the
        // lowest ceilings are the common ones with the longest postings.
        //
        // Worth doing because the exhaustive walk is not selective at all: on
        // BEIR FiQA the average query reaches 86% of the corpus, 50 000 of
        // 57 638 documents, almost all of it through terms that carry no
        // information. Measured, same answers throughout: SciFact 179 -> 47
        // us, FiQA 1918 -> 311 us, Turkish WebFAQ 401 -> 82 us. The rows it
        // returns are exactly the exhaustive ones, which is what
        // `pruning_never_changes_the_answer` holds it to.
        let mut cursors = by_ceiling(&cursors);
        let mut reach: Vec<f32> = Vec::with_capacity(cursors.len() + 1);
        reach.push(0.0);
        for c in &cursors {
            reach.push(reach[reach.len() - 1] + c.ceiling);
        }

        let mut heap: BinaryHeap<ByScore> = BinaryHeap::with_capacity(k + 1);
        // Nothing is pruned until the heap is full; `-inf` says so without a
        // second flag to keep in step.
        let mut worst_kept = f32::NEG_INFINITY;
        let mut pivot = 0usize;
        loop {
            // Strictly less, never "less or equal": a document that ties the
            // worst kept score still has to be compared against it, or the
            // tie-break on document id would depend on the pruning.
            while pivot < cursors.len() && reach[pivot + 1] < worst_kept {
                pivot += 1;
            }
            if pivot == cursors.len() {
                break; // nothing left that could beat what is already held
            }

            // The lists are ascending, so the frontier is the smallest head
            // among the terms that can still start a candidate. With a
            // handful of them a linear scan beats a heap over the cursors:
            // fewer branches, no allocation.
            let mut doc = DocId::MAX;
            for c in &cursors[pivot..] {
                if let Some(d) = c.list.docs.get(c.at) {
                    if *d < doc {
                        doc = *d;
                    }
                }
            }
            if doc == DocId::MAX {
                break;
            }
            let dl = self.lengths.get(doc).max(1) as f32;
            let denom_len = k1 * (1.0 - b + b * dl / avgdl);
            let mut score = 0.0f64;
            for c in cursors[pivot..].iter_mut() {
                if c.list.docs.get(c.at) == Some(&doc) {
                    score += c.contribution(c.at, k1, denom_len);
                    c.at += 1;
                }
            }
            // The rest can only add. Walk them from the largest ceiling down
            // and stop as soon as everything still unread cannot carry the
            // score to where it would matter.
            let mut gave_up = false;
            for j in (0..pivot).rev() {
                // What is still unread cannot carry this document to where it
                // would matter. Stop, and do not offer the partial score --
                // it would be popped straight back out, but saying so here is
                // cheaper than relying on that.
                if score + (reach[j + 1] as f64) < (worst_kept as f64) {
                    gave_up = true;
                    break;
                }
                let c = &mut cursors[j];
                // These are only ever asked about increasing document ids, so
                // the search starts where the last one left off.
                c.at += c.list.docs[c.at..].partition_point(|d| *d < doc);
                if c.list.docs.get(c.at) == Some(&doc) {
                    score += c.contribution(c.at, k1, denom_len);
                }
            }
            if !gave_up && score > 0.0 && accept(doc) {
                heap.push(ByScore(score as f32, doc));
                if heap.len() > k {
                    heap.pop();
                }
                if heap.len() == k {
                    worst_kept = heap.peek().map(|s| s.0).unwrap_or(f32::NEG_INFINITY);
                }
            }
        }

        let mut out: Vec<(DocId, f32)> = heap.into_iter().map(|s| (s.1, s.0)).collect();
        out.sort_by(best_first);
        out
    }
}

/// One word of a loose `match`'s query and what it reaches.
#[cfg(feature = "text")]
struct Reach<'a> {
    /// How often the query holds the word.
    count: u32,
    /// The word's own list, where the index holds it.
    own: Option<Postings<'a>>,
    /// Each variant, its distance and its list.
    found: Vec<(&'a str, u8, Postings<'a>)>,
}

#[cfg(feature = "text")]
impl<'a> Reach<'a> {
    /// The lists of its variants `d` edits away.
    fn of(&self, d: u8) -> Vec<Postings<'a>> {
        self.found
            .iter()
            .filter(|f| f.1 == d)
            .map(|f| f.2)
            .collect()
    }
}

/// The documents the cursors weighing more than nothing hold, ascending,
/// each once: [`TextIndex::scored`]'s set, `most` the documents the index
/// holds.
///
/// Lists that hold more ids than their span has 64-id words, and no more
/// words than there are documents, are marked in a bitmap of that span and
/// read out in id order: a pass over the postings and one over the words,
/// no sort. Other lists -- few ids over a wide span, or ids far apart --
/// are laid end to end and put together through the engine's one sort
/// whenever they pass `most`, the one sort they always had where they hold
/// fewer; one list alone is copied. Either holds twice `most` ids at most:
/// the bitmap's words and the set, or the lists laid end to end. The sort
/// alone put a facet beside the 100 commonest words of 200 000 titles, two
/// sorts of 200 000 ids and more, 5% behind the one sort main made of
/// every list; marked, `count` over them retires a twelfth of what it did.
#[cfg(feature = "text")]
fn ids_of(cursors: &[Cursor], most: usize) -> Vec<DocId> {
    let (mut count, mut held, mut lo, mut hi) = (0, 0, DocId::MAX, 0);
    let mut one: &[DocId] = &[];
    for c in cursors {
        let l = c.list.docs;
        if let (true, Some(&first), Some(&last)) = (c.weight > 0.0, l.first(), l.last()) {
            (count, held, one) = (count + 1, held + l.len(), l);
            (lo, hi) = (lo.min(first), hi.max(last));
        }
    }
    if count < 2 {
        return one.to_vec();
    }
    let words = (hi - lo) / 64 + 1;
    let mut out: Vec<DocId> = Vec::new();
    if words <= held.min(most) as u64 {
        let mut bits = vec![0u64; words as usize];
        for c in cursors.iter().filter(|c| c.weight > 0.0) {
            for &d in c.list.docs {
                let at = d - lo;
                bits[(at / 64) as usize] |= 1 << (at % 64);
            }
        }
        out.reserve_exact(bits.iter().map(|w| w.count_ones() as usize).sum());
        for (i, &w) in bits.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                out.push(lo + i as u64 * 64 + w.trailing_zeros() as u64);
                w &= w - 1;
            }
        }
        return out;
    }
    // How many ascending lists `out` holds laid end to end.
    let mut laid = 0;
    for c in cursors.iter().filter(|c| c.weight > 0.0) {
        out.extend_from_slice(c.list.docs);
        laid += 1;
        if laid > 1 && out.len() > most {
            out.sort_unstable();
            out.dedup();
            laid = 1;
        }
    }
    if laid > 1 {
        out.sort_unstable();
        out.dedup();
    }
    out
}

/// Each word's variants of one distance as one list where there are
/// several, in the order [`TextIndex::loose_cursors`] takes them.
#[cfg(feature = "text")]
fn unions(reach: &[Reach]) -> Vec<List> {
    let mut out = Vec::new();
    for r in reach {
        for d in 0..NEAR_WEIGHT.len() as u8 {
            let lists = r.of(d);
            if lists.len() > 1 {
                out.push(union(&lists));
            }
        }
    }
    out
}

/// The documents any of `lists` holds, ascending, each with its counts
/// there summed: merged two at a time, a level at a time, so a word
/// reaching `n` lists costs the postings `log n` times over.
#[cfg(feature = "text")]
fn union(lists: &[Postings]) -> List {
    let none = List::default();
    let mut level: Vec<List> = Vec::with_capacity(lists.len().div_ceil(2));
    for p in lists.chunks(2) {
        level.push(merged(p[0], p.get(1).copied().unwrap_or(none.view())));
    }
    while level.len() > 1 {
        let mut up = Vec::with_capacity(level.len().div_ceil(2));
        for p in level.chunks(2) {
            up.push(merged(p[0].view(), p.get(1).unwrap_or(&none).view()));
        }
        level = up;
    }
    level.pop().unwrap_or_default()
}

#[cfg(feature = "text")]
fn merged(a: Postings, b: Postings) -> List {
    let mut out = List::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    loop {
        let (doc, tf) = match (a.docs.get(i), b.docs.get(j)) {
            (Some(&x), Some(&y)) if x == y => {
                i += 1;
                j += 1;
                (x, a.tfs[i - 1] + b.tfs[j - 1])
            }
            (Some(&x), Some(&y)) if x < y => {
                i += 1;
                (x, a.tfs[i - 1])
            }
            (Some(&x), None) => {
                i += 1;
                (x, a.tfs[i - 1])
            }
            (_, Some(&y)) => {
                j += 1;
                (y, b.tfs[j - 1])
            }
            (None, None) => break,
        };
        out.docs.push(doc);
        out.tfs.push(tf);
        out.max_tf = out.max_tf.max(tf);
    }
    out
}

/// Robertson/Sparck-Jones idf of a term `df` of `n` documents hold, the
/// `ln(1 + ...)` form: always positive, so a term occurring in every
/// document contributes ~0 rather than pushing the score negative the way
/// the unsmoothed form does.
fn idf(n: usize, df: usize) -> f32 {
    let (n, df) = (n as f32, df as f32);
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

/// How many of `docs` are in `within`, both ascending: the shorter walked,
/// each looked for in the longer from where the last was found -- `|short|
/// log |long|`, which for a user's rows against a common term's list is a
/// few hundred steps.
fn held_by(docs: &[DocId], within: &[DocId]) -> usize {
    let (short, long) = if docs.len() <= within.len() {
        (docs, within)
    } else {
        (within, docs)
    };
    let mut at = 0;
    let mut n = 0;
    for d in short {
        at += long[at..].partition_point(|x| x < d);
        if long.get(at) == Some(d) {
            n += 1;
            at += 1;
        }
    }
    n
}

/// Highest score first, ties to the lower id. A function rather than a
/// closure so that every ranking sorted this way -- `fuse`'s too -- shares
/// one copy of the sort: a closure inside the generic `search` is a type of
/// its own for every `accept` it is given.
pub(crate) fn best_first(a: &(DocId, f32), b: &(DocId, f32)) -> Ordering {
    b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))
}

/// `cursors` by ceiling, lowest first, ties in the order they came -- put in
/// that order by `best_first` as `(position, -ceiling)`, since the browser
/// module holds that sort anyway: a sort of `Cursor`s of its own was 4.2 KB
/// of it.
///
/// The vector index's candidates keep theirs: this way a build of 30 000 x
/// 128 took 2.5% longer, for 0.4 KB brotli.
#[cfg(feature = "text")]
fn by_ceiling<'a>(cursors: &[Cursor<'a>]) -> Vec<Cursor<'a>> {
    let mut order: Vec<(DocId, f32)> = cursors
        .iter()
        .enumerate()
        .map(|(i, c)| (i as DocId, -c.ceiling))
        .collect();
    order.sort_by(best_first);
    order.iter().map(|&(i, _)| cursors[i as usize]).collect()
}

/// One query term's walk through its postings.
#[cfg(feature = "text")]
#[derive(Clone, Copy)]
struct Cursor<'a> {
    at: usize,
    list: Postings<'a>,
    /// idf, times how often the term occurs in the query.
    weight: f32,
    /// The most this term can contribute to any single document.
    ceiling: f32,
}

#[cfg(feature = "text")]
impl Cursor<'_> {
    /// One term's BM25 contribution, accumulated in `f64`.
    ///
    /// The terms are summed in whatever order the walk reaches them, and that
    /// order is not the same with pruning as without it. In `f32` the
    /// difference lands at ~1e-7, which is enough to reorder two documents
    /// that score exactly the same and so to make the tie-break depend on the
    /// plan rather than on the document id. Summing in `f64` puts the
    /// difference at ~1e-16, which the cast back to `f32` absorbs.
    #[inline]
    fn contribution(&self, at: usize, k1: f32, denom_len: f32) -> f64 {
        let tf = self.list.tfs[at] as f32;
        (self.weight * (tf * (k1 + 1.0)) / (tf + denom_len)) as f64
    }
}

/// Ordered so that the *worst* candidate sits at the top of the max-heap --
/// that is the one a better score evicts. Ties break on the document id so
/// the same query always returns the same rows in the same order. The
/// sparse index keeps its best the same way, through this one type.
#[derive(PartialEq)]
pub(crate) struct ByScore(pub(crate) f32, pub(crate) DocId);

impl Eq for ByScore {}

impl Ord for ByScore {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .total_cmp(&self.0)
            .then_with(|| self.1.cmp(&other.1))
    }
}

impl PartialOrd for ByScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(not(feature = "text"))]
pub use crate::off::TextIndex;

#[cfg(all(test, feature = "text"))]
impl TextIndex {
    /// The same walk with the pruning switched off: every term drives the
    /// frontier, every matching document is scored. Only `search`'s
    /// equivalence test uses it -- it is the thing MaxScore has to agree with.
    fn search_unpruned<F>(&self, query: &str, k: usize, accept: F) -> Vec<(DocId, f32)>
    where
        F: Fn(DocId) -> bool,
    {
        if k == 0 || self.lengths.len() == 0 {
            return Vec::new();
        }
        let qtf = self.query_terms(query, None);
        let (k1, b, avgdl) = (self.spec.k1(), self.spec.b(), self.avgdl());
        let mut cursors: Vec<Cursor> = Vec::new();
        for (term, count) in &qtf {
            if let Some(list) = self.postings.get(term) {
                let weight = *count as f32 * idf(self.lengths.len(), list.len());
                let tf = list.max_tf as f32;
                cursors.push(Cursor {
                    at: 0,
                    list,
                    weight,
                    ceiling: weight * (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b)),
                });
            }
        }
        self.ranked_unpruned(cursors, k, accept, avgdl)
    }

    /// [`Self::search_loose`] with the pruning switched off: the same
    /// cursors, variants and all, every one driving the frontier.
    fn search_loose_unpruned<F>(
        &self,
        query: &str,
        k: usize,
        accept: F,
        loose: Loose,
    ) -> Vec<(DocId, f32)>
    where
        F: Fn(DocId) -> bool,
    {
        let qtf = self.query_terms(query, None);
        let reach = self.reach(query, loose, &qtf, None).unwrap();
        let (n, avgdl) = self.counted(None);
        let unions = unions(&reach);
        let cursors = self.loose_cursors(&qtf, &reach, &unions, n, avgdl, None);
        self.ranked_unpruned(cursors, k, accept, avgdl)
    }

    fn ranked_unpruned<F>(
        &self,
        cursors: Vec<Cursor>,
        k: usize,
        accept: F,
        avgdl: f32,
    ) -> Vec<(DocId, f32)>
    where
        F: Fn(DocId) -> bool,
    {
        if cursors.is_empty() {
            return Vec::new();
        }
        let (k1, b) = (self.spec.k1(), self.spec.b());
        let mut cursors = by_ceiling(&cursors);
        let mut heap: BinaryHeap<ByScore> = BinaryHeap::with_capacity(k + 1);
        loop {
            let mut doc = DocId::MAX;
            for c in &cursors {
                if let Some(d) = c.list.docs.get(c.at) {
                    if *d < doc {
                        doc = *d;
                    }
                }
            }
            if doc == DocId::MAX {
                break;
            }
            let dl = self.lengths.get(doc).max(1) as f32;
            let denom_len = k1 * (1.0 - b + b * dl / avgdl);
            let mut score = 0.0f64;
            for c in cursors.iter_mut() {
                if c.list.docs.get(c.at) == Some(&doc) {
                    score += c.contribution(c.at, k1, denom_len);
                    c.at += 1;
                }
            }
            if score > 0.0 && accept(doc) {
                heap.push(ByScore(score as f32, doc));
                if heap.len() > k {
                    heap.pop();
                }
            }
        }
        let mut out: Vec<(DocId, f32)> = heap.into_iter().map(|s| (s.1, s.0)).collect();
        out.sort_by(best_first);
        out
    }
}

#[cfg(all(test, feature = "text"))]
mod tests {
    use super::*;

    fn spec() -> TextIndexSpec {
        TextIndexSpec::default()
    }

    /// The terms the index reads of `q` over every row, in order.
    fn read(ix: &TextIndex, q: &str) -> Vec<String> {
        let mut out: Vec<String> = ix.read_terms(q, None).into_keys().collect();
        out.sort_unstable();
        out
    }

    /// Every document a plain `query` scores above zero.
    fn matching(ix: &TextIndex, query: &str) -> Vec<DocId> {
        ix.scored(query, Loose::default(), None).unwrap()
    }

    #[test]
    fn tokenizes_and_folds_case() {
        assert_eq!(tokenize("Rust and WASM"), ["rust", "and", "wasm"]);
        assert_eq!(tokenize("a-b_c.d"), ["a", "b", "c", "d"]);
        assert_eq!(tokenize("  "), Vec::<String>::new());
        // Non-ASCII goes through the Unicode mapping and the fold, not the
        // ASCII path.
        assert_eq!(tokenize("Grüße"), ["grusse"]);
        // Case must not decide the term, whatever the alphabet. The Turkish
        // pair is spelled out in `the_turkish_i_folds_every_way_it_is_written`.
        for (a, b) in [("ÖDEV", "ödev"), ("ÇALIŞMA", "çalışma"), ("GRÜßE", "grüße")] {
            assert_eq!(tokenize(a), tokenize(b), "{a} vs {b}");
        }
    }

    /// Every way of writing the Turkish I has to land on the same term, or a
    /// capitalised word is unfindable. The default Unicode mapping does not
    /// do this on its own -- see `for_each_term`.
    #[test]
    fn the_turkish_i_folds_every_way_it_is_written() {
        for group in [
            ["İSTANBUL", "İstanbul", "istanbul", "ISTANBUL"],
            ["IŞIK", "ışık", "Işık", "işik"],
            ["KIZ", "kız", "Kız", "kiz"],
        ] {
            let first = tokenize(group[0]);
            for other in &group[1..] {
                assert_eq!(first, tokenize(other), "{:?} vs {other:?}", group[0]);
            }
            // And nothing combining survives into the term.
            assert!(!first[0].contains('\u{0307}'), "{first:?}");
        }
        // The decomposed spelling of `İ` (I + combining dot) folds too.
        assert_eq!(tokenize("I\u{307}stanbul"), tokenize("İstanbul"));
    }

    /// A Latin or Greek letter is its base letter, its accents dropped, so
    /// a word typed on a keyboard without them finds the word written with
    /// them, and the other way round.
    #[test]
    fn accents_fold_to_their_letters() {
        assert_eq!(tokenize("Sütlaç tarifi"), ["sutlac", "tarifi"]);
        assert_eq!(tokenize("sütlaç fırında"), ["sutlac", "firinda"]);
        for group in [
            ["sutlac", "Sütlaç", "SÜTLAÇ", "sütlaç"],
            ["kofte", "köfte", "KÖFTE", "Köfte"],
            ["cay", "çay", "ÇAY", "Çay"],
            ["dogru", "doğru", "DOĞRU", "Doğru"],
            ["sise", "şişe", "ŞİŞE", "Şişe"],
            // French, Spanish, Portuguese, Polish, Czech, Romanian.
            ["ecole", "école", "ÉCOLE", "École"],
            ["nino", "niño", "NIÑO", "Niño"],
            ["acao", "ação", "AÇÃO", "Ação"],
            ["lodz", "łódź", "ŁÓDŹ", "Łódź"],
            ["prilis", "příliš", "PŘÍLIŠ", "Příliš"],
            ["stiinta", "știință", "ȘTIINȚĂ", "Știință"],
            // Vietnamese stacks a tone on a vowel's own mark.
            ["pho", "phở", "PHỞ", "Phở"],
            ["nguyen", "nguyễn", "NGUYỄN", "Nguyễn"],
            ["duong", "đường", "ĐƯỜNG", "Đường"],
            // The letters Unicode gives no decomposition, by what they are.
            ["oresund", "øresund", "ØRESUND", "Øresund"],
            ["strasse", "straße", "STRASSE", "Straße"],
            ["aegir", "ægir", "ÆGIR", "Ægir"],
            ["oeuvre", "œuvre", "ŒUVRE", "Œuvre"],
            ["ijs", "ĳs", "ĲS", "Ĳs"],
            ["hobz", "ħobż", "ĦOBŻ", "Ħobż"],
            // Greek's tonos and dialytika, polytonic breathings, and the
            // final sigma, which `Σ` lowercases to `σ` wherever it stands.
            ["ελληνικα", "ελληνικά", "ΕΛΛΗΝΙΚΑ", "Ελληνικά"],
            ["οδος", "οδός", "ΟΔΟΣ", "Οδός"],
            ["προυποθεση", "προϋπόθεση", "ΠΡΟΫΠΟΘΕΣΗ", "Προϋπόθεση"],
            ["ανθρωπος", "ἄνθρωπος", "ἌΝΘΡΩΠΟΣ", "Ἄνθρωπος"],
        ] {
            let first = tokenize(group[0]);
            for other in &group[1..] {
                assert_eq!(first, tokenize(other), "{:?} vs {other:?}", group[0]);
            }
        }
        // Written decomposed, a letter and its marks fold as the letter
        // written whole does: macOS's file names, Vietnamese typed with
        // its tones apart.
        assert_eq!(tokenize("Su\u{308}tlac\u{327}"), ["sutlac"]);
        assert_eq!(tokenize("pho\u{31B}\u{309} bo\u{300}"), ["pho", "bo"]);
        assert_eq!(tokenize("re\u{301}sume\u{301}"), ["resume"]);
        // A mark alone is no word, and a mark keeps no word apart.
        assert_eq!(tokenize("a \u{301} b"), ["a", "b"]);
        // Cyrillic and every other script stay as written: `ё` is not `е`,
        // `й` not `и` -- nor written decomposed, a mark after a letter the
        // fold leaves staying with it.
        assert_eq!(tokenize("Ёлка и йогурт"), ["ёлка", "и", "йогурт"]);
        assert_ne!(tokenize("ёлка"), tokenize("елка"));
        assert_eq!(tokenize("Е\u{308}лка"), ["е\u{308}лка"]);
        assert_ne!(tokenize("е\u{308}лка"), tokenize("елка"));
        assert_eq!(tokenize("и\u{306}огурт"), ["и\u{306}огурт"]);
        // A mark after a Latin or Greek letter goes, folded by the table or
        // not: `α` is its own base letter.
        assert_eq!(tokenize("α\u{301}λφα"), ["αλφα"]);
        assert_eq!(tokenize("ä\u{301}"), ["a"]);
        assert_eq!(tokenize("مرحبا שלום"), ["مرحبا", "שלום"]);
        // English never reaches the table.
        assert_eq!(
            tokenize("The Quick brown FOX, naive cafe"),
            ["the", "quick", "brown", "fox", "naive", "cafe"]
        );
        assert_eq!(tokenize("naïve café"), ["naive", "cafe"]);
    }

    /// By default a word is its fold alone: typed with accents or without,
    /// a query finds every spelling, and nothing is kept as written.
    #[test]
    fn a_word_is_its_fold_however_it_is_typed() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "Sütlaç tarifi");
        ix.insert(2, "sutlac tarifi");
        ix.insert(3, "SÜTLAÇ");
        ix.insert(4, "köfte");
        let found = |ix: &TextIndex, q: &str| {
            let mut ids: Vec<DocId> = ix.search(q, 10, |_| true).iter().map(|h| h.0).collect();
            ids.sort_unstable();
            ids
        };
        for q in ["sutlac", "SUTLAC", "sütlaç", "SÜTLAÇ", "sütlac"] {
            assert_eq!(found(&ix, q), [1, 2, 3], "{q}");
        }
        assert_eq!(found(&ix, "kofte"), [4]);
        assert_eq!(found(&ix, "KÖFTE"), [4]);
        assert_eq!(matching(&ix, "sütlaç"), [1, 2, 3]);
        assert_eq!(read(&ix, "Sütlaç"), ["sutlac"]);
        assert_eq!(ix.terms(), 3, "sutlac, tarifi, kofte");
        assert_eq!(ix.lengths.get(1), 2);
        let mut prefixed = Vec::new();
        let p = TextIndexSpec {
            prefix_max: 4,
            ..spec()
        };
        for_each_indexed_term("Sütlaç", &p, |t| prefixed.push(t.to_string()));
        assert_eq!(prefixed, ["sutlac", "^sut", "^sutl"]);
    }

    /// Under `@text(accents)` a word is indexed by its fold and, where its
    /// accents make it another word, as written beside it: typed without
    /// accents a query finds every spelling, typed with them the spelling
    /// it typed -- or every spelling, where no document writes it so.
    #[test]
    fn a_word_typed_with_accents_reads_it_as_written() {
        let accents = || TextIndexSpec {
            accents: true,
            ..spec()
        };
        let mut ix = TextIndex::new(accents());
        ix.insert(1, "Sütlaç tarifi");
        ix.insert(2, "sutlac tarifi");
        ix.insert(3, "SÜTLAÇ");
        ix.insert(4, "köfte");
        let found = |ix: &TextIndex, q: &str| {
            let mut ids: Vec<DocId> = ix.search(q, 10, |_| true).iter().map(|h| h.0).collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(found(&ix, "sutlac"), [1, 2, 3]);
        assert_eq!(found(&ix, "SUTLAC"), [1, 2, 3]);
        assert_eq!(found(&ix, "sütlaç"), [1, 3]);
        assert_eq!(found(&ix, "SÜTLAÇ"), [1, 3]);
        // Spelt as no document spells it, it reads the fold.
        assert_eq!(found(&ix, "sütlac"), [1, 2, 3]);
        assert_eq!(found(&ix, "kofte"), [4]);
        assert_eq!(matching(&ix, "sütlaç"), [1, 3]);
        assert_eq!(matching(&ix, "sutlac tarifi"), [1, 2, 3]);
        assert_eq!(read(&ix, "Sütlaç"), ["~sütlaç"]);
        assert_eq!(read(&ix, "sutlac"), ["sutlac"]);
        // A document is as long as its words, the written forms aside:
        // BM25 scores it as it did before accents folded.
        assert_eq!(ix.lengths.get(1), 2);
        assert_eq!(ix.lengths.get(3), 1);
        // No written form is a word the dictionary holds, nor has prefixes.
        assert!(ix.postings.words().iter().all(|w| !w.starts_with(b"~")));
        let mut prefixed = Vec::new();
        let p = TextIndexSpec {
            prefix_max: 4,
            ..accents()
        };
        for_each_indexed_term("Sütlaç", &p, |t| prefixed.push(t.to_string()));
        assert_eq!(prefixed, ["sutlac", "~sütlaç", "^sut", "^sutl"]);
        assert_eq!(tokenize("Sütlaç"), ["sutlac"]);
        // Written decomposed, its marks stay in its term as written.
        let mut decomposed = Vec::new();
        for_each_indexed_term("Su\u{308}tlac\u{327}", &accents(), |t| {
            decomposed.push(t.to_string())
        });
        assert_eq!(decomposed, ["sutlac", "~su\u{308}tlac\u{327}"]);
        // The last document written so gone, the spelling reads the fold.
        ix.remove(1, "Sütlaç tarifi");
        ix.remove(3, "SÜTLAÇ");
        assert_eq!(found(&ix, "sütlaç"), [2]);
        assert_eq!(ix.terms(), 4, "sutlac, tarifi, kofte, ~köfte");
        // ASCII is never written apart from its fold, nor the I forms.
        let mut ix = TextIndex::new(accents());
        ix.insert(1, "The Quick brown FOX, İSTANBUL Istanbul");
        assert_eq!(ix.terms(), 5);
    }

    /// Case never decides a word: every character folds as its lowercase
    /// does, and a fold is folded already -- over every code point, so a
    /// table written from another Unicode than the standard library's
    /// lowercase mapping is caught here.
    #[test]
    fn every_character_folds_as_its_lowercase_and_once() {
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            if splits_words(c) || gram(c) > 0 || combining(c) || marked(c) {
                continue;
            }
            let one = tokenize(&format!("a{c}"));
            let lower = tokenize(&format!("a{}", c.to_lowercase()));
            assert_eq!(one, lower, "U+{:04X}", c as u32);
            let again: Vec<String> = one.iter().flat_map(|t| tokenize(t)).collect();
            assert_eq!(one, again, "U+{:04X} folds twice", c as u32);
        }
    }

    /// Under `@text(accents)` case decides no more than it does for the
    /// fold: over every code point, a character beside its lowercase, and
    /// beside its uppercase wherever the fold reads the two alike, gives
    /// the same terms, as written and all -- the final `ς` beside `Σ`,
    /// `ſ` beside `S`, `ϑ` beside `Θ`.
    #[test]
    fn under_accents_case_decides_nothing_the_fold_does_not() {
        let accents = TextIndexSpec {
            accents: true,
            ..spec()
        };
        let all = |t: &str| {
            let mut out = Vec::new();
            for_each_indexed_term(t, &accents, |w| out.push(w.to_string()));
            out
        };
        let mut checked = 0;
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            if splits_words(c) || gram(c) > 0 || combining(c) || marked(c) {
                continue;
            }
            let one = format!("a{c}");
            let lower = format!("a{}", c.to_lowercase());
            assert_eq!(all(&one), all(&lower), "U+{:04X}", c as u32);
            let mut upper = c.to_uppercase();
            if let (Some(u), None) = (upper.next(), upper.next()) {
                let upper = format!("a{u}");
                if tokenize(&one) == tokenize(&upper) {
                    assert_eq!(
                        all(&one),
                        all(&upper),
                        "U+{:04X} and U+{:04X}",
                        c as u32,
                        u as u32
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 2_000, "{checked}");
        // A Greek word with no accent is its fold alone, however it ends
        // and however it is cased; one with an accent is found by every
        // casing of it.
        let mut ix = TextIndex::new(accents);
        ix.insert(1, "ΟΔΟΣ");
        ix.insert(2, "οδος");
        ix.insert(3, "Οδος");
        ix.insert(4, "ΟΔΌΣ οδός");
        assert_eq!(ix.terms(), 2, "οδοσ and ~οδόσ");
        for q in ["οδος", "ΟΔΟΣ", "Οδος", "οδοσ"] {
            let mut ids: Vec<DocId> = ix.search(q, 10, |_| true).iter().map(|h| h.0).collect();
            ids.sort_unstable();
            assert_eq!(ids, [1, 2, 3, 4], "{q}");
        }
        for q in ["οδός", "ΟΔΌΣ", "Οδός"] {
            assert_eq!(matching(&ix, q), [4], "{q}");
        }
    }

    /// Whether a word typed with accents reads its spelling is decided by
    /// the rows a statement reads alone: alice's `sutlac tarifi`, the one
    /// row she reads, is found by `sütlaç` -- read as its fold, since none
    /// of her rows writes it so -- and answers the same whether or not bob
    /// wrote a row of `sütlaç` she cannot read. Every way a query's words
    /// are read: the ranking, a loose one, the set a facet counts and what
    /// `highlight()` marks.
    #[test]
    fn a_row_a_statement_does_not_read_decides_no_word() {
        let accents = TextIndexSpec {
            accents: true,
            ..spec()
        };
        let index = |bob: &str| {
            let mut ix = TextIndex::new(accents);
            ix.insert(1, "sutlac tarifi");
            ix.insert(2, bob);
            ix.insert(3, "sutlas firinda");
            ix
        };
        // Bob's row is as long either way, so BM25 counts it the same.
        let (without, with) = (index("köfte"), index("sütlaç"));
        let alice: &[DocId] = &[1, 3];
        let test = |d: DocId| alice.contains(&d);
        let reads: Reads = Some(&test);
        let fuzzy = Loose {
            fuzzy: 1,
            prefix: false,
        };
        let begun = Loose {
            fuzzy: 0,
            prefix: true,
        };
        for q in ["sütlaç", "SÜTLAÇ tarifi", "sütlaç fırında"] {
            // `accept` holds the ranking to her rows, as the engine's does.
            let a = without.search_within(q, 10, test, Some(alice));
            assert_eq!(a, with.search_within(q, 10, test, Some(alice)), "{q}");
            assert_eq!(a[0].0, 1, "{q}");
            for loose in [fuzzy, begun] {
                let a = without
                    .search_loose(q, 10, test, Some(alice), loose)
                    .unwrap();
                let b = with.search_loose(q, 10, test, Some(alice), loose).unwrap();
                assert_eq!(a, b, "{q} {loose:?}");
                let a = without.scored(q, loose, Some(alice)).unwrap();
                let b = with.scored(q, loose, Some(alice)).unwrap();
                let mine =
                    |ids: Vec<DocId>| ids.into_iter().filter(|d| test(*d)).collect::<Vec<_>>();
                assert_eq!(mine(a), mine(b), "{q} {loose:?}");
                let mut a = without.loose_words(q, loose, reads).unwrap();
                let mut b = with.loose_words(q, loose, reads).unwrap();
                a.sort_unstable();
                b.sort_unstable();
                assert_eq!(a, b, "{q} {loose:?}");
            }
            // The set a facet counts, held to her rows after, as the
            // engine holds it.
            let plain = Loose::default();
            let mine = |ids: Vec<DocId>| ids.into_iter().filter(|d| test(*d)).collect::<Vec<_>>();
            let a = mine(without.scored(q, plain, Some(alice)).unwrap());
            assert_eq!(
                a,
                mine(with.scored(q, plain, Some(alice)).unwrap()),
                "{q}"
            );
            assert!(a.contains(&1), "{q}");
            let keys = |ix: &TextIndex, reads: Reads| {
                let mut k: Vec<String> = ix.read_terms(q, reads).into_keys().collect();
                k.sort_unstable();
                k
            };
            assert_eq!(keys(&without, reads), keys(&with, reads), "{q}");
            // Over every row bob's spelling is read as it is written.
            assert!(
                keys(&with, None).iter().any(|t| t.starts_with(WRITTEN)),
                "{q}"
            );
        }
        // Once a row alice reads writes it so, she reads the spelling.
        let mut ix = index("sütlaç");
        ix.insert(4, "sütlaç tatlısı");
        let mine: &[DocId] = &[1, 3, 4];
        let hits = ix.search_within("sütlaç", 10, |d| mine.contains(&d), Some(mine));
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn prefixes_are_only_emitted_when_asked_for() {
        // `ı` folds to `i`, so the stored term is `kitaplarin`.
        let mut off = Vec::new();
        for_each_indexed_term("kitapların", &spec(), |t| off.push(t.to_string()));
        assert_eq!(off, ["kitaplarin"]);

        let on = TextIndexSpec {
            prefix_min: 3,
            prefix_max: 6,
            ..TextIndexSpec::default()
        };
        let mut got = Vec::new();
        for_each_indexed_term("kitapların", &on, |t| got.push(t.to_string()));
        assert_eq!(got, ["kitaplarin", "^kit", "^kita", "^kitap", "^kitapl"]);
        // A word no longer than the prefix is already its own term.
        let mut short = Vec::new();
        for_each_indexed_term("ev kar", &on, |t| short.push(t.to_string()));
        assert_eq!(short, ["ev", "kar"]);
    }

    /// A script written without spaces is indexed in overlapping runs of
    /// characters: pairs of Han, kana and Hangul, across the kana a Japanese
    /// word ends in, and triples of Thai letters, a tone mark among them.
    #[test]
    fn unspaced_scripts_are_indexed_in_runs_of_characters() {
        assert_eq!(tokenize("北京天安门"), ["北京", "京天", "天安", "安门"]);
        assert_eq!(
            tokenize("東京都に住む"),
            ["東京", "京都", "都に", "に住", "住む"]
        );
        assert_eq!(tokenize("학교에 간다"), ["학교", "교에", "간다"]);
        assert_eq!(
            tokenize("ไม่มีปัญหา"),
            ["ไม่", "ม่ม", "่มี", "มีป", "ีปั", "ปัญ", "ัญห", "ญหา"]
        );
        // A run shorter than its gram is one term; a script that spaces its
        // words keeps them whole beside one that does not.
        assert_eq!(tokenize("京 ไม่"), ["京", "ไม่"]);
        assert_eq!(
            tokenize("iPhone手机 2023年"),
            ["iphone", "手机", "2023", "年"]
        );
        // Full-width Latin and digits are the ASCII ones.
        assert_eq!(tokenize("ＩＴ企業 ２０２３"), ["it", "企業", "2023"]);
    }

    /// `chars` adds each character of Han, kana and Hangul, once for a run
    /// of one, and nothing to Thai, whose characters are letters.
    #[test]
    fn chars_add_each_character_to_the_pairs() {
        let on = TextIndexSpec {
            chars: true,
            ..TextIndexSpec::default()
        };
        let terms = |text: &str| {
            let mut got = Vec::new();
            for_each_indexed_term(text, &on, |t| got.push(t.to_string()));
            got.sort();
            got
        };
        assert_eq!(terms("北京市"), ["京", "京市", "北", "北京", "市"]);
        assert_eq!(terms("京"), ["京"]);
        assert_eq!(terms("ไม่มี"), ["ม่ม", "ไม่", "่มี"]);
        let mut ix = TextIndex::new(on);
        ix.insert(1, "北京市");
        ix.insert(2, "上海市");
        let ids = |q: &str| {
            ix.search(q, 10, |_| true)
                .iter()
                .map(|h| h.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("京"), [1], "one character finds the runs holding it");
        assert_eq!(ids("市").len(), 2);
    }

    /// The whole point of the option: an inflected form and its stem have to
    /// meet somewhere. Byte slicing would also panic here -- these are
    /// two-byte letters.
    #[test]
    fn prefixes_let_an_inflected_form_meet_its_stem() {
        let on = TextIndexSpec {
            prefix_min: 3,
            prefix_max: 6,
            ..TextIndexSpec::default()
        };
        let mut ix = TextIndex::new(on);
        ix.insert(1, "kitapların fiyatı");
        ix.insert(2, "araba kiralama");
        assert_eq!(ix.search("kitap", 10, |_| true)[0].0, 1);
        assert_eq!(ix.search("kitabı", 10, |_| true)[0].0, 1);

        // Without the option the same query finds nothing at all.
        let mut plain = TextIndex::new(spec());
        plain.insert(1, "kitapların fiyatı");
        assert!(plain.search("kitap", 10, |_| true).is_empty());
    }

    /// A small deterministic generator: no dev-dependency, and a failing
    /// seed can be replayed by hand.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn upto(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// MaxScore is only allowed to be faster. Whatever it prunes, the rows it
    /// returns -- ids, order and scores -- have to be the ones the exhaustive
    /// walk returns, including how ties break.
    ///
    /// The vocabulary is deliberately skewed: a few terms in almost every
    /// document (which is what makes pruning worth doing) and a long tail of
    /// rare ones (which is what it must not lose).
    #[test]
    fn pruning_never_changes_the_answer() {
        for seed in 1..=250u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let vocab: Vec<String> = (0..60).map(|i| format!("t{i}")).collect();
            let mut ix = TextIndex::new(spec());
            let n_docs = 40 + rng.upto(160);
            for id in 1..=n_docs as u64 {
                let len = 3 + rng.upto(25);
                let mut words = Vec::with_capacity(len);
                for _ in 0..len {
                    // 60% of the time from the first three terms, so they end
                    // up in nearly every document.
                    let w = if rng.upto(10) < 6 {
                        rng.upto(3)
                    } else {
                        rng.upto(vocab.len())
                    };
                    words.push(vocab[w].as_str());
                }
                ix.insert(id, &words.join(" "));
            }
            for _ in 0..25 {
                let qlen = 1 + rng.upto(6);
                let q: Vec<&str> = (0..qlen)
                    .map(|_| {
                        let w = if rng.upto(10) < 5 {
                            rng.upto(3)
                        } else {
                            rng.upto(vocab.len())
                        };
                        vocab[w].as_str()
                    })
                    .collect();
                let query = q.join(" ");
                for k in [1usize, 3, 10, 50] {
                    let odd = rng.upto(4) == 0;
                    let want = ix.search_unpruned(&query, k, |d| !odd || d % 2 == 0);
                    let got = ix.search(&query, k, |d| !odd || d % 2 == 0);
                    assert_eq!(
                        want.iter().map(|x| x.0).collect::<Vec<_>>(),
                        got.iter().map(|x| x.0).collect::<Vec<_>>(),
                        "seed {seed} k {k} query {query:?}"
                    );
                    for (w, g) in want.iter().zip(&got) {
                        assert!(
                            (w.1 - g.1).abs() <= 1e-5 * w.1.abs().max(1.0),
                            "seed {seed} score {} vs {}",
                            w.1,
                            g.1
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ranks_by_bm25() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "the rust programming language");
        ix.insert(2, "rust rust rust and more rust");
        ix.insert(3, "python programming");
        let hits = ix.search("rust", 10, |_| true);
        // Document 2 says it four times in six words; it has to come first.
        assert_eq!(hits[0].0, 2);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn a_term_in_every_document_separates_nothing() {
        let mut ix = TextIndex::new(spec());
        for id in 1..=5 {
            ix.insert(id, "common term here");
        }
        // idf is ~0 for a term with df == n, so no document stands out; the
        // unsmoothed form would have gone negative here instead.
        let hits = ix.search("common", 10, |_| true);
        assert_eq!(hits.len(), 5);
        for (_, s) in &hits {
            assert!(*s >= 0.0 && *s < 0.2, "score {s} should be near zero");
        }
    }

    #[test]
    fn removal_restores_the_empty_state() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "alpha beta");
        ix.insert(2, "beta gamma");
        ix.remove(1, "alpha beta");
        assert_eq!(ix.len(), 1);
        assert!(ix.search("alpha", 10, |_| true).is_empty());
        assert_eq!(ix.search("beta", 10, |_| true)[0].0, 2);
        ix.remove(2, "beta gamma");
        assert!(ix.is_empty());
        assert_eq!(ix.terms(), 0, "the dictionary must not keep empty lists");
        assert_eq!(ix.total_terms, 0);
    }

    /// A dictionary past the split -- its terms in shards -- answers as the
    /// one map did: each term's documents, a term gone with its last one,
    /// the same ranking through a shrink, and empty again once cleared.
    #[test]
    fn a_dictionary_in_shards_answers_as_one_did() {
        let text = |d: u64| format!("t{d}a t{d}b t{}c shared", d / 2);
        let mut ix = TextIndex::new(spec());
        let n = 60_000u64;
        for d in 0..n {
            ix.insert(d, &text(d));
        }
        assert_eq!(ix.terms(), 2 * n as usize + n as usize / 2 + 1);
        for d in (0..n).step_by(97) {
            assert_eq!(matching(&ix, &format!("t{d}a")), [d]);
            assert_eq!(matching(&ix, &format!("t{}c", d / 2)), [d & !1, d | 1]);
        }
        for d in (0..n).step_by(3) {
            ix.remove(d, &text(d));
        }
        assert_eq!(matching(&ix, "t3a t4a t6b"), [4]);
        let pair = ix.search("t5c t9b", 10, |_| true);
        ix.shrink_to_fit();
        assert_eq!(ix.search("t5c t9b", 10, |_| true), pair);
        assert_eq!(ix.postings_count(), 4 * ix.len());
        ix.clear();
        assert_eq!((ix.terms(), ix.len()), (0, 0));
        ix.insert(1, "alpha");
        assert_eq!(matching(&ix, "alpha"), [1]);
    }

    /// A query whose lists hold many times the documents there are: the
    /// set is put together as the lists pass that count, and is still
    /// each document a term holds, ascending, once.
    #[test]
    fn a_set_whose_lists_pass_the_documents_is_each_once() {
        let mut ix = TextIndex::new(spec());
        // Document `d` holds `w<i>` where `d + i` is no multiple of 3:
        // each word two thirds of the 300, and 50 more holding none.
        for d in 0..300u64 {
            let words: Vec<String> = (0..40)
                .filter(|i| (d + i) % 3 != 0)
                .map(|i| format!("w{i}"))
                .collect();
            ix.insert(d, &words.join(" "));
        }
        for d in 300..350u64 {
            ix.insert(d, "other");
        }
        let every: Vec<String> = (0..40).map(|i| format!("w{i}")).collect();
        assert_eq!(
            matching(&ix, &every.join(" ")),
            (0..300).collect::<Vec<_>>()
        );
        // `w0`, `w3`, ...: those whose `d` is no multiple of 3, 14 lists
        // of 200 over 350 documents.
        let thirds: Vec<String> = (0..40).step_by(3).map(|i| format!("w{i}")).collect();
        let want: Vec<DocId> = (0..300).filter(|d| d % 3 != 0).collect();
        assert_eq!(matching(&ix, &thirds.join(" ")), want);
    }

    /// The set is one whichever way `ids_of` puts it together: lists dense
    /// over their span marked in a bitmap, lists sparse over it -- ids far
    /// apart, or few against the span -- laid end to end and sorted, one
    /// list copied. Each is the documents a word of the query holds,
    /// ascending, once.
    #[test]
    fn the_bitmap_and_the_sort_put_together_one_set() {
        // Document `i` holds `w<k>` where `k + 2` divides `i + k`: `w0`
        // half of them, `w9` one in eleven, some none.
        let holds = |i: u64, k: u64| (i + k).is_multiple_of(k + 2);
        let queries: [&[u64]; 5] = [
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[9],
            &[8, 9],
            &[0, 9],
            &[3, 5, 7],
        ];
        // A gap of 1 or 3 puts every query of several words in the bitmap,
        // of 64 the ten words alone (400 words of bits, 800 ids), of 1 000
        // or 2^40 none.
        for gap in [1u64, 3, 64, 1_000, 1 << 40] {
            let mut ix = TextIndex::new(spec());
            for i in 0..400u64 {
                let words: Vec<String> = (0..10)
                    .filter(|&k| holds(i, k))
                    .map(|k| format!("w{k}"))
                    .collect();
                ix.insert(1 + i * gap, &format!("t {}", words.join(" ")));
            }
            for q in queries {
                let text: Vec<String> = q.iter().map(|k| format!("w{k}")).collect();
                let want: Vec<DocId> = (0..400u64)
                    .filter(|&i| q.iter().any(|&k| holds(i, k)))
                    .map(|i| 1 + i * gap)
                    .collect();
                assert_eq!(matching(&ix, &text.join(" ")), want, "gap {gap}, {q:?}");
            }
        }
    }

    /// The count `memory_bytes` reads is what the terms hold -- each
    /// shard's count of its lists their walk (`Terms::check`) -- through
    /// inserts, updates, removals and a shrink, and the index answers as
    /// one built afresh from the documents left.
    #[test]
    fn the_byte_count_is_the_walk_it_replaced() {
        let walked = |ix: &TextIndex| {
            ix.postings.check();
            ix.postings.bytes() + ix.lengths.bytes()
        };
        let mut ix = TextIndex::new(spec());
        let mut seed = 0x51ED_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut docs: Vec<(DocId, String)> = Vec::new();
        for round in 0..2000u64 {
            let id = 1 + next() % 300;
            if let Some(at) = docs.iter().position(|d| d.0 == id) {
                let (_, old) = docs.remove(at);
                ix.remove(id, &old);
            }
            if round % 3 != 0 {
                let words: Vec<String> = (0..1 + next() % 6)
                    .map(|_| format!("w{}", next() % 40))
                    .collect();
                let text = words.join(" ");
                ix.insert(id, &text);
                docs.push((id, text));
            }
            assert_eq!(ix.memory_bytes(), walked(&ix), "round {round}");
        }
        ix.shrink_to_fit();
        assert_eq!(ix.memory_bytes(), walked(&ix));
        let mut fresh = TextIndex::new(spec());
        for (id, text) in &docs {
            fresh.insert(*id, text);
        }
        for q in ["w1", "w2 w3", "w7 w7 w30", "w39 w0 w12 w5"] {
            assert_eq!(ix.search(q, 50, |_| true), fresh.search(q, 50, |_| true));
            assert_eq!(matching(&ix, q), matching(&fresh, q));
        }
        assert_eq!(ix.postings_count(), fresh.postings_count());
        for (id, text) in docs {
            ix.remove(id, &text);
        }
        assert_eq!(ix.terms(), 0);
        ix.shrink_to_fit();
        assert_eq!(ix.memory_bytes(), walked(&ix));
        assert_eq!(ix.postings.bytes(), 0);
    }

    #[test]
    fn reindexing_a_document_replaces_its_terms() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "alpha beta");
        ix.remove(1, "alpha beta");
        ix.insert(1, "gamma delta");
        assert!(ix.search("alpha", 10, |_| true).is_empty());
        assert_eq!(ix.search("gamma", 10, |_| true)[0].0, 1);
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn the_filter_is_applied_during_the_merge() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "rust");
        ix.insert(2, "rust");
        ix.insert(3, "rust");
        let hits = ix.search("rust", 10, |d| d != 2);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [1, 3]);
    }

    #[test]
    fn top_k_keeps_the_best_and_is_deterministic() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "rust rust rust");
        ix.insert(2, "rust rust");
        ix.insert(3, "rust");
        ix.insert(4, "rust");
        let hits = ix.search("rust", 2, |_| true);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [1, 2]);
        // Documents 3 and 4 score identically; ties must resolve on the id.
        let all = ix.search("rust", 4, |_| true);
        assert_eq!(all[2].0, 3);
        assert_eq!(all[3].0, 4);
    }

    #[test]
    fn out_of_order_inserts_keep_the_postings_sorted() {
        let mut ix = TextIndex::new(spec());
        for id in [9u64, 3, 7, 1, 5] {
            ix.insert(id, "term");
        }
        let list = ix.postings.get("term").unwrap();
        assert!(list.docs.windows(2).all(|w| w[0] < w[1]), "{:?}", list.docs);
    }

    #[test]
    fn repeated_query_terms_do_not_change_the_ranking() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "alpha beta beta");
        ix.insert(2, "alpha alpha beta");
        let once = ix.search("alpha beta", 10, |_| true);
        let twice = ix.search("alpha alpha beta beta", 10, |_| true);
        assert_eq!(
            once.iter().map(|h| h.0).collect::<Vec<_>>(),
            twice.iter().map(|h| h.0).collect::<Vec<_>>()
        );
    }

    fn fuzzy() -> Loose {
        Loose {
            fuzzy: 2,
            prefix: false,
        }
    }

    fn ids(hits: &[(DocId, f32)]) -> Vec<DocId> {
        hits.iter().map(|h| h.0).collect()
    }

    /// Words over five letters, five to eleven of them, each a third of the
    /// time a typo of one before it and a third that one with letters
    /// after: many are an edit or two from another, and many begin another.
    fn near_words(rng: &mut Rng, n: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(n);
        while out.len() < n {
            let w = match (out.is_empty(), rng.upto(3)) {
                (false, 0) => {
                    let w = out[rng.upto(out.len())].clone();
                    misspelt(rng, &w)
                }
                (false, 1) => {
                    let mut w = out[rng.upto(out.len())].clone();
                    w.extend((0..1 + rng.upto(3)).map(|_| b"abcde"[rng.upto(5)] as char));
                    w
                }
                _ => {
                    let len = 5 + rng.upto(7);
                    (0..len).map(|_| b"abcde"[rng.upto(5)] as char).collect()
                }
            };
            if !out.contains(&w) {
                out.push(w);
            }
        }
        out
    }

    /// A word with a typo at one place past the first: a letter dropped,
    /// doubled, changed, or swapped with the next.
    fn misspelt(rng: &mut Rng, w: &str) -> String {
        let mut c: Vec<char> = w.chars().collect();
        let at = 1 + rng.upto(c.len() - 1);
        match rng.upto(4) {
            0 => {
                c.remove(at);
            }
            1 => c.insert(at, c[at]),
            2 => c[at] = b"abcde"[rng.upto(5)] as char,
            _ if at + 1 < c.len() => c.swap(at, at + 1),
            _ => {}
        }
        c.into_iter().collect()
    }

    /// A word misspelt finds the documents holding it; a word begun finds
    /// the documents holding what it begins; plain, neither does.
    #[test]
    fn a_typo_and_a_word_begun_find_the_word() {
        let mut ix = TextIndex::new(spec());
        ix.insert(1, "a database of documents");
        ix.insert(2, "databases and their indexes");
        ix.insert(3, "nothing to see");
        let begun = Loose {
            fuzzy: 0,
            prefix: true,
        };
        let loose = |q: &str, l: Loose| ids(&ix.search_loose(q, 10, |_| true, None, l).unwrap());
        assert!(ix.search("databse", 10, |_| true).is_empty());
        // Seven letters take one edit: `databases` is two away.
        assert_eq!(loose("databse", fuzzy()), [1]);
        assert_eq!(loose("databasse", fuzzy()), [1, 2], "nine letters take two");
        assert_eq!(loose("datab", begun), [1, 2]);
        assert_eq!(loose("indexes datab", begun), [2, 1]);
        // Only the last word is a prefix.
        assert_eq!(loose("datab nothing", begun), [3]);
        // A word under five letters takes no edit; a typo in the first
        // letter counts two.
        assert!(loose("seee", fuzzy()).is_empty());
        let one = Loose {
            fuzzy: 1,
            prefix: false,
        };
        assert!(loose("tatabases", one).is_empty());
        assert_eq!(
            loose("tatabases", fuzzy()),
            [2],
            "two edits from nine letters"
        );
        // A misspelt word begun.
        let both = Loose {
            fuzzy: 2,
            prefix: true,
        };
        assert_eq!(loose("datbas", both), [1, 2]);
        assert_eq!(ix.scored("databasse", fuzzy(), None).unwrap(), [1, 2]);
        let mut words = ix.loose_words("databasse indexes", fuzzy(), None).unwrap();
        words.sort_unstable();
        assert_eq!(words, ["database", "databases"]);
    }

    /// For a one-word query, every document holding the word as typed
    /// ranks above every document holding only a variant of it, however
    /// often and in however short a document -- over generated corpora,
    /// with `fuzzy`, `prefix` and both.
    #[test]
    fn a_word_as_typed_outranks_its_variants() {
        let mut checked = 0;
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let n = 40 + rng.upto(80);
            let vocab = near_words(&mut rng, n);
            let mut ix = TextIndex::new(spec());
            let mut docs: Vec<Vec<&str>> = Vec::new();
            for id in 0..(30 + rng.upto(200)) as u64 {
                let len = 1 + rng.upto(40);
                let words: Vec<&str> = (0..len)
                    .map(|_| vocab[rng.upto(vocab.len())].as_str())
                    .collect();
                ix.insert(id, &words.join(" "));
                docs.push(words);
            }
            for _ in 0..20 {
                let w = &vocab[rng.upto(vocab.len())];
                let loose = Loose {
                    fuzzy: [0, 1, 2][rng.upto(3)],
                    prefix: rng.upto(2) == 0,
                };
                // Sometimes the word itself, sometimes a typo no document
                // may hold, sometimes a word begun.
                let q = match rng.upto(3) {
                    0 => w.clone(),
                    1 => misspelt(&mut rng, w),
                    _ => w.chars().take(2 + rng.upto(w.len() - 1)).collect(),
                };
                let hits = ix.search_loose(&q, 1_000, |_| true, None, loose).unwrap();
                let holds = |d: DocId| docs[d as usize].contains(&q.as_str());
                let first_variant = hits.iter().position(|h| !holds(h.0)).unwrap_or(hits.len());
                assert!(
                    hits[first_variant..].iter().all(|h| !holds(h.0)),
                    "seed {seed} {q:?} {loose:?}: {hits:?}"
                );
                if first_variant > 0 && first_variant < hits.len() {
                    assert!(hits[first_variant - 1].1 > hits[first_variant].1);
                    checked += 1;
                }
                // And every document holding it is found.
                let n = (0..docs.len() as DocId).filter(|&d| holds(d)).count();
                assert_eq!(first_variant, n, "seed {seed} {q:?}");
            }
        }
        assert!(checked > 100, "{checked}");
    }

    /// MaxScore over a loose query's cursors -- the variants' unions among
    /// them -- returns what the exhaustive walk over the same cursors does.
    #[test]
    fn pruning_never_changes_a_loose_answer() {
        let mut queries = 0;
        for seed in 1..=120u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let n = 30 + rng.upto(120);
            let vocab = near_words(&mut rng, n);
            let mut ix = TextIndex::new(spec());
            for id in 1..=(40 + rng.upto(160)) as u64 {
                let len = 3 + rng.upto(25);
                let words: Vec<&str> = (0..len)
                    .map(|_| match rng.upto(10) < 5 {
                        true => vocab[rng.upto(3)].as_str(),
                        false => vocab[rng.upto(vocab.len())].as_str(),
                    })
                    .collect();
                ix.insert(id, &words.join(" "));
            }
            for _ in 0..20 {
                let qlen = 1 + rng.upto(4);
                let q: Vec<String> = (0..qlen)
                    .map(|_| {
                        let w = &vocab[rng.upto(vocab.len())];
                        match rng.upto(3) {
                            0 => w.clone(),
                            1 => misspelt(&mut rng, w),
                            _ => w.chars().take(3).collect(),
                        }
                    })
                    .collect();
                let query = q.join(" ");
                let loose = Loose {
                    fuzzy: [0, 1, 2][rng.upto(3)],
                    prefix: rng.upto(2) == 0,
                };
                for k in [1usize, 3, 10, 50] {
                    let odd = rng.upto(4) == 0;
                    let accept = |d: DocId| !odd || d.is_multiple_of(2);
                    let want = ix.search_loose_unpruned(&query, k, accept, loose);
                    let got = ix.search_loose(&query, k, accept, None, loose).unwrap();
                    assert_eq!(
                        ids(&want),
                        ids(&got),
                        "seed {seed} k {k} {query:?} {loose:?}"
                    );
                    for (w, g) in want.iter().zip(&got) {
                        assert!((w.1 - g.1).abs() <= 1e-5 * w.1.abs().max(1.0));
                    }
                    queries += !want.is_empty() as usize;
                }
            }
        }
        assert!(queries > 5_000, "{queries}");
    }

    /// Past the bound a word is refused, never read as some of what it
    /// reaches.
    #[test]
    fn a_word_reaching_too_many_is_refused() {
        let mut ix = TextIndex::new(spec());
        for i in 0..MAX_EXPANSIONS as u64 {
            ix.insert(i, &format!("pre{i}"));
        }
        let prefix = Loose {
            fuzzy: 0,
            prefix: true,
        };
        let hits = ix.search_loose("pre", 5, |_| true, None, prefix).unwrap();
        assert_eq!(hits.len(), 5);
        ix.insert(MAX_EXPANSIONS as u64, "prez");
        let e = ix
            .search_loose("pre", 5, |_| true, None, prefix)
            .unwrap_err();
        assert!(e.to_string().contains("refused rather than cut"), "{e}");
        assert!(ix.scored("pre", prefix, None).is_err());
        assert!(ix.search_loose("pre1", 5, |_| true, None, prefix).is_ok());
    }

    /// The sorted words, once a loose `match` has made them, are kept up
    /// by the writes: an index written to after it answers as one built
    /// from the documents as they stand.
    #[test]
    fn the_words_are_kept_up_by_the_writes() {
        for seed in 1..=30u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let vocab = near_words(&mut rng, 200);
            let text = |rng: &mut Rng| {
                (0..1 + rng.upto(6))
                    .map(|_| vocab[rng.upto(vocab.len())].as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let mut ix = TextIndex::new(spec());
            let mut docs: Vec<(DocId, String)> = Vec::new();
            for id in 0..50 {
                let t = text(&mut rng);
                ix.insert(id, &t);
                docs.push((id, t));
            }
            ix.search_loose("abcde", 10, |_| true, None, fuzzy())
                .unwrap();
            for round in 0..300u64 {
                let at = rng.upto(docs.len());
                if rng.upto(3) == 0 {
                    let (id, old) = docs.remove(at);
                    ix.remove(id, &old);
                } else {
                    let t = text(&mut rng);
                    ix.insert(1_000 + round, &t);
                    docs.push((1_000 + round, t));
                }
            }
            let mut fresh = TextIndex::new(spec());
            for (id, t) in &docs {
                fresh.insert(*id, t);
            }
            assert_eq!(
                ix.dictionary().len(),
                fresh.dictionary().len(),
                "seed {seed}"
            );
            let both = Loose {
                fuzzy: 1,
                prefix: true,
            };
            for _ in 0..20 {
                let w = &vocab[rng.upto(vocab.len())];
                let q = misspelt(&mut rng, w);
                for loose in [fuzzy(), both] {
                    let mut a = ix.loose_words(&q, loose, None).unwrap();
                    let mut b = fresh.loose_words(&q, loose, None).unwrap();
                    a.sort_unstable();
                    b.sort_unstable();
                    assert_eq!(a, b, "seed {seed} {q:?}");
                }
            }
        }
    }
}
