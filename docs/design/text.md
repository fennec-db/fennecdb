# Text

The long form of the invariants [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) lists under *Text*: collations, `nocase`, `match` and `match` as a filter, the text index, `fuzzy` and `prefix`, `rerank`, `fuse`, `highlight()` and facets. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Collations

**`collate und` and `collate tr` are ICU's orders, a query's or a field's.**
The weights are ICU's own for every assigned code point --
`crates/fennec-core/tools/collate/gen.py` reads ICU 76.1 out of macOS's libicucore into
`collate/`: the root cut into twelve chunks by script and each tailoring as
what it changes, with ICU's own contractions (a letter and a mark it
composes with, a two-part vowel sign, a Thai or Lao vowel written before its
consonant; up to three characters) and Hangul syllables decomposed into
jamo. ICU gives 133 535 primary weights, which no packed entry has the bits
for, so consecutive code points that alone give consecutive primaries share
a rank and are told apart by their code points (`BY_CODE_POINT`), a run of
them one range (`UNIFORM`), and a table's words are written as differences
in LEB128: 24 931 ranks, 154 KB for every script (324 KB plain), 161 KB of
`make small`'s 1331. A comparison walks ICU's three levels, letters then
accents then case, over the whole string before it falls back to the
bytes, so the order is total.
It starts at the first byte the two strings do not share, stepped back past
any character that can continue a contraction (`CONTINUES`; a fixed step is
wrong, Gurung Khema's overlap), and a Latin letter's element is made once
per collation (`Collation::latin`): 68 -> 30 ns before the root, and 33.5 ns
for `tr` and 35.2 for `und` now over a million Turkish names -- 41 while
every letter went through the tailoring's table and the root's chunk. Held
to ICU itself over 1 250 000 random pairs, five apart -- marks that compose
with their letter across another mark or out of canonical order, which
ICU's normalisation and discontiguous contractions find and fennecdb does
not -- and to `Intl.Collator` in `web/fennec.test.js`.
`order name collate und` names it for one key; a field declared `name text
collate und` has it wherever its text compares -- `order` naming none, `<`
and `>` in `where` (`RowAccess::text_order`), `min` and `max`, and a
`@sorted` index over it, whose chunks order through `sorted::Entry` with the
collation passed in. Equality stays the bytes' (a `nocase` field's is its
fold's, below). The schema writes a collated field behind type tag 11 (`TAG_COLLATED`) and the collation's code (1 `tr`,
2 `und`), which an older binary refuses rather than read the field in byte
order. A native build carries every
chunk. The browser module carries `latin` (9 KB) and is handed the rest
(`fennec_add_chunk`), which `make wasm` puts in `web/collate/`: a comparison
that meets a script it has not got notes the chunk (`collate::take_missing`)
and the statement is refused (`collate::refuse`) -- a read after it ran, a
write before it changes anything (`put`, `update`'s pass of its own,
`delete`'s filter), a load after it (`fennec_load`: its `@sorted` indexes
were built without the chunk) -- and `Fennec.query`, the builder, `restore`
and the sync layer fetch the chunks and run it again. So every stored
collated value's chunks are there, which keeps a `@sorted` index in order.
A chunk carries the tables' stamp, and another version's is refused. The
root cost the browser module 17.5 KB, 8.2 KB gzip and 6.8 KB brotli,
over the 10.4 KB `tr` and the field had; the other chunks are 145 KB, 67
KB gzip, 61 KB brotli, where plain words were 102 and 90.

## nocase

**`nocase` is a text compared by its case fold wherever it is compared.**
`email text nocase` (`Field::nocase`, schema tag 17 before the type,
`TAG_NOCASE`, so a binary from before refuses the file rather than take a
second `Alice@x.com` its `@unique` refuses): `=`, `!=`, `in`, `<` and
`>`, `~`, the keys of `@hash`, `@unique`, `@sorted` and a composite's part,
`order`, `group`, `count(distinct)`, `approx_count_distinct`, `facet`,
`min`, `max`, `first ... by`, a `lookup`'s child key and a scoped token's
rules all go through one rule (`Field::text_order`, a `collate::TextOrder`
of a collation and a `case::CaseFold`) -- an index option alone would have
left `=` comparing bytes beside a `@unique` that folds, and a `lookup` on
it finding what `where` did not. The text is kept as written; a group's
and a facet's value is the first row holding its fold, in id order (the
bucket's first member, read). The fold is Unicode's full case folding,
CaseFolding.txt's C and F -- Python's `str.casefold()`, which
`tests/nocase.rs` holds it to at every code point (`tests/casefold.txt`,
`tools/casefold.py`) -- with Turkish's T mappings under `collate tr`
(`ISTANBUL` is `ıstanbul`, `İSTANBUL` is `istanbul`). Full rather than
simple: simple folding keeps `ß` apart from `SS`, and would be a table of
its own, where the full one is the standard library's lowercase mapping,
already in the browser module -- and for the 297 code points whose fold is
not their lowercase (`UNLIKE_LOWER`, 41 runs, which a test derives from
the standard library again), its lowercase, uppercase and lowercase again,
`ı` left alone and Cherokee's small letters made its capitals. Nothing is
normalised: `é` and `e` with a combining accent stay apart, and a
full-width letter is not ASCII. A collated `nocase` field orders by the
collation over the folds, which ties exactly the texts that fold to one,
so its order and its equality agree. The indexes hold folds
(`HashIndex::key`, `folded_key`, `encode_folded` writing an ASCII text's
lowercased as it copies it; `sorted::Ordered`'s `fold`; `composite::Part`'s);
the bound `Filter` folds a value once a query and a row as it compares it
(`Test::Folded`, `case::cmp_to_folded`: the ASCII both start with byte by
byte, the rest a character at a time until the two part): folded whole as
a row was read, a scan for one name over 100 000 rows, a third past ASCII,
went 5.4 -> 17.0 ms, and goes to 6.8. An order folds its `nocase` keys
once a row in a pass after the reading (`folded_keys`, so an order over
none reads as it did), 8.5 -> 16.2 ms, where folding both sides at every
comparison was 17.4; a `@sorted` one is walked. A collection without a
`nocase` field reads and writes as it did (`make nocase-bench`'s plain
column against main's, in turns). Made `nocase` or no longer, a field is
rebuilt -- the declared schema's `nocase_changed` refusal names
`rebuild`, which a `@unique` refuses where two values become one. A
text's alone: a list's elements meet `has`, an index each and a facet a
value at a time. `fennec import` takes PostgreSQL's `citext` and SQLite's
`COLLATE NOCASE` as it. The browser module grew 11.5 KB, 3.7 KB brotli
(the engine's paths 6.4 KB, `case` 2.1); its scans of a plain field moved
within 2% of main's under load, once a `nocase` field's tests shared a
point's out-of-line arm of `Filter::test` and `compare` stopped asking the
order of a text `=` (an arm of their own and that question were 5 and 7%
of a count by numbers and by a text over 20 000 rows).

## MaxScore

**`match` prunes with MaxScore.** The exhaustive merge is not selective --
on BEIR FiQA the average query reaches 86% of the corpus -- so terms whose
remaining ceiling cannot beat the worst kept score stop driving the frontier.
Measured, with identical output: SciFact 179 -> 47 us, FiQA 1918 -> 311 us,
Turkish WebFAQ 401 -> 82 us. `pruning_never_changes_the_answer` compares it
against an exhaustive reference over 25 000 generated cases; scores accumulate
in `f64` so the two orders of summation agree once rounded back to `f32`.

## match as a filter

**Beside `order`, `count` or an aggregate, `match` is a filter: the rows
its ranking scores above zero.** A search page asks for "the matches,
newest first" and "N results", and both were refused -- `match` decided the
order, and a `count` or an aggregate beside it was refused as beside `near`
-- so a caller wrote `get r where id in (get r match title $1 select id)
order at desc limit 20` and the same list's `count`: two statements' work,
the list the ranking's first 10 000 rows (its default page: past them a
count was a count of a page, believed whole) and refused past an `in`
list's 100 000 values. `Select::match_ranks` says which a `match` is: with
`where`, `limit`, `offset`, `facet` and marks alone it ranks, down the code
it took; with `order`, `count` or an aggregate list, grouped or not, it
selects, and the statement's clause decides the rest as it does a
`where`'s rows.

The set is the ranking's own (`TextIndex::scored`): the cursors a ranking
walks, made by the functions that make them for it (`query_terms`,
`reach`, `loose_cursors`, `cursor`) with its `within` -- each word a
cursor, a word's `fuzzy` variants and `prefix` completions one a distance
-- and a document is in it where a cursor weighing more than nothing holds
it: a ranking keeps a document whose summed contributions are above zero,
each its cursor's weight times a factor above zero -- so a word so
common its idf rounds to 0 in `f32`, every one of 2^23 documents or more
holding it, adds none, as it adds none to a ranking. A `facet` counts over
the same set beside a ranking too (`Database::matched_rows`, handed the
filter's rows the ranking read, so a scoped token's are read once), where
it counted `TextIndex::matching`'s -- every document holding a term --
which took that word's list: the one place the two sets parted, gone with
`matching` and `matching_loose`. The filter is the statement's,
`scoped()`'s rules and `@ttl`'s `alive` ANDed in before it reaches
`Database::matched_rows`: a scoped token's set is the one BM25 over its own
rows scores above zero (`Match::within`, the filter's rows read first, as
the ranking reads them), then held to those rows; any other filter's index
candidates are intersected with the set and the rest of it tested a row at
a time, the plan `id in <set> and <filter>` takes. A page is put in order
as the list's would be (`order_ids`, `order_ids_by`), ties in id order, no
ordered index walked, as none is beside an `id in` list. Every row the
words reach is charged to a call (`--call-max-rows`), as a ranking's are.
`tests/match_filter.rs` holds each form to its workaround's answer row for
row over 3 000 rows -- eight queries, plain, `fuzzy` and `prefix`, by four
filters (none, a hash equality, a scan, both), five orders, six counts and
aggregates, each also scored over the filter's rows -- and the set to the
ranking's ids; `tests/access.rs` holds a scoped token's page, count and
groups to its own rows, and `tests/match_filter.rs` a row past its `@ttl`
to a twin whose expiry is written out.

Bounds. A count or an aggregate answers no rows, and an ordered page reads
every match as an ordered `get` reads every row its filter passes, so
neither keeps the ranking's `MAX_MATCH_ROWS`: a `count` over 12 500
matched rows answers 12 500. The set is a `Vec<DocId>`, eight bytes a
row, as `matching_ids` holds a `where`'s, and bounded by the index's
documents, never by its lists' sum (`text::ids_of`). Lists holding more
ids than their span has 64-id words, and no more words than there are
documents, are marked in a bitmap of the span and read out in id order: a
pass over the postings and one over the words, no sort. Others -- few ids
over a wide span, or ids far apart -- are laid end to end and put
together (`sort_unstable`, `dedup`) whenever they pass the count of
documents, the one sort they always had where they hold fewer; one list
alone is copied. Either way it holds twice the documents' ids at most
while it is made, the bitmap's words and the set or the lists laid end to
end -- where each list copied and then sorted once held a query of 200
words, each in half of a million documents, as 100 million ids, 800 MB.
The sort alone was the first cut: a facet beside a ranking of the 100
commonest words of 200 000 titles, whose lists hold 262 786 ids over
149 669 rows, sorted once its ids passed 200 000 and again at the end,
where main had sorted all 262 786 once, and took 5% longer than main
(22.6 -> 23.9 ms). Marked, the set costs a tenth of the sort's
instructions: that facet retires 303 million instructions against main's
337 (18.8 ms against 22.8 on a loaded machine), `get ... match ... count`
over those words 4.9 million against the sort's 58 (0.54 ms against 5.5),
the ordered page 93 million against 147 and the groups 128 against 182; a
query of one or two words, under the bitmap's bar, retires what it did.
What bounded it before bounds it and errors as it did: a word `fuzzy` or
`prefix` reads as more than `MAX_EXPANSIONS` words, `--call-max-rows`,
and the ranking's 10 000 rows wherever `match` still ranks. `explain`
says how many rows the words scored and how many the filter kept; to a
scoped token it says the kept alone, since the words reach rows it cannot
read.

A plain ordered `get`, which runs none of this code, ran 4% slower in the
first timing run (4.88 -> 5.08 us, in every pair), and the branch's binary
did retire 1.4 thousand more instructions in it -- but not of its code:
the functions it spends its time in are main's instruction for
instruction, and the branch's path retires some 200 fewer. On macOS the
default allocator's work follows the executable's identity: the same
binary with its UUID alone rewritten, and signed again, ran the statement
in 78.6 to 82.1 thousand instructions and 16.1 to 16.8 thousand cycles,
main's and the branch's alike; over seven identities each, main and this
branch ran it in 16.55 and 16.56 thousand cycles, and with
`MallocNanoZone=0` the two retire the same and run in the same cycles. Two
builds a few percent apart in a statement of microseconds are compared
over several UUIDs, or with that variable set.

What answers beside it. `highlight()` and `snippet()` mark a row's words
whatever orders it, the marks its ranking gives it, so they stay beside
`order`; beside `count` there is no row to mark, refused. A row carries no
`_score` under `order`: the order is the statement's, and a score would be
BM25 worked out again over each row of the page for a column nothing
ordered by -- a caller after relevance leaves `order` out, and `order
_score`, which named no field ("not found"), is refused saying so. `rerank`
reorders the rows `match` ranks, and here it ranks none: refused, as is an
ungrouped aggregate's `limit` or `offset` with no `order`, which would fold
the first matches by id and read as "the best 50". `facet` beside a
filtering `match` counts the rows it selects, what a `count` beside it
counts. `near` and `fuse` are as they were; `fuse` was refused beside all
three already.

The browser module grew 1.9 KB raw, 1.1 KB gzip and 0.8 KB brotli
(`matched_rows` 1.4 KB, `scored` 1.1 -- 0.65 KB of it the bitmap's, 0.3
gzip -- less the facets' own path, 1.8 KB: `matched_set`, `matching` and
`matching_loose`): kept in it, as a page's search box asks "how many" and
"newest first" as a server's does, and the bitmap with it, since a page
counts a facet over common words as a server does. The first cut grew it
3.1 KB raw, 1.0 brotli, before the facets shared the set.
A build without the text index refuses `match` in `text_query`, in line
with each caller (`no_text_index`), so neither the ranking's path nor the
filter's after it is code of that module, nor `rerank`'s, and the checks
only a built index would make (`order _score`, `rerank` or an aggregate's
`limit` beside a filter) are asked where one is: the module without the
indexes and the one with no feature lost 4.3 and 4.1 KB raw, 1.4 and 1.2
brotli, against main.

Rejected: putting the set into the filter as an `id in [...]` -- each id an
`Expr`, then a `Value` encoded into the bound filter's `InSet` table, as a
written list is -- for eight bytes a row and its bound kept; the rows
holding a term (`matching`), which leaves the ranking's set at the `f32`
edge above; a ranking with no `k`, read off for its positive scores,
which walks every list a document at a time where the set is the lists
put together; merging each list into the set as it comes, a walk of the
set a list, which costs the set's length a list where the bitmap costs
its words once; and the bitmap for every query of two lists or more,
which reads and clears a word for each 64 ids of the span however few
the lists hold -- the bar keeps it to lists that hold more ids than that.

## Tokenizer

**`@text` is word-boundary matching unless told otherwise.** `prefix=N` also
indexes each word's prefixes, which is a dictionary-free stemmer for inflected
languages: on Turkish WebFAQ it is worth +14.6% nDCG@10 for 3.4x the postings
(58 MB -> 182 MB, 82 -> 485 us per query). Off by default — the corpus
decides. The tokenizer also folds `I`, `İ`, `ı`, `i` onto one term, because the
locale-blind Unicode mapping turns `İ` into two code points nobody can type and
would hide every capitalised Turkish word. A script written without spaces
is split into runs of characters rather than words (`text::grams`): pairs of
Han, kana and Hangul, triples of Thai, Lao, Khmer and Myanmar letters --
pairs of Thai letters were too common to tell documents apart, 0.495 against
0.547 at a query three times as slow. nDCG@10 went 0.006 -> 0.439 on
C-MTEB's Chinese EcomRetrieval, 0.162 -> 0.868 on its CovidRetrieval, 0.119
-> 0.582 on the Japanese JaGovFaqs and 0.537 -> 0.682 on WebFAQ's Korean;
English scores to the fourth decimal what it did. `@text(chars)` adds each
character of Han, kana and Hangul, worth 0.439 -> 0.528 on product titles
and a little less on longer text, for 1.6 to 1.9x the postings: an index
kind of its own (7), as a quantized graph is, so a binary that knows no
`chars` refuses the file. The tokenizer takes its callback as `&mut dyn
FnMut`: generic, it was compiled six times, 8 KB of the browser module.

**A Latin or Greek letter folds to its base letter; `@text(accents)` keeps
a word its accents make another as written beside its fold.** The table is
`text/fold.rs`, 1 056 bytes over the Latin and Greek blocks, written by
`tools/fold/gen.py` from `UnicodeData.txt` -- through rustfmt, so that what
it writes is the file as committed, which CI's fmt job holds it to
(`--check`): a canonical decomposition's first letter, a compatibility
decomposition into letters of the script, the letters with a stroke or a
middle dot by what they are (`ø`, `ł`, `đ`, `ħ`), `æ`, `œ` and `ß` as two
letters and the final sigma as `σ`; a letter folds as its lowercase does.
A letter that folds to nothing is read in one lookup too: 0, one that is
its own lowercase, is kept as it is, and a capital of one that stays is
its lowercase where a byte holds it (`Α` is `α`), so every letter Greek
text writes is one lookup; `LOWER`, lowercased and looked up again, is
left to a capital whose lowercase no byte holds (`Þ`, `Ŋ`) and to one
whose lowercase is outside the table. With 0 read as `LOWER`, as the
table first had it, each of Greek's unaccented letters was looked up,
lowercased and looked up again; the table is 400 bytes brotli, where it
was 327 (414 with a byte for a letter kept and 0 for one lowered). Where a code point is in the table is a match of its three ranges, each
range's offset a constant of its arm (`fold::at`): a loop summing the
ranges' lengths walked them for every Cyrillic and Greek letter. A
combining mark stays in its word; after a Latin or Greek letter, or ASCII,
it is the accent the fold drops. ASCII never reaches the table; Cyrillic
and every other script stay as written, and a mark after one of their
letters stays with it: `е` and U+0308 is `ё` written decomposed, no more
`е` than `ё` is. Dropped after any letter, as the option's first build
had it, a decomposed `ё` found every `е` and a decomposed `й` every `и`.
Measured by BM25 over WebFAQ (`make beir`'s
pipeline; Turkish 144 846 documents, Vietnamese 123 972, Greek 38 516,
French 569 505), each set's queries as typed and the same queries typed
without accents (NFD, the marks taken off):

| nDCG@10 | main | `@text`, folded | `@text(accents)`, kept as written |
|---|---|---|---|
| Turkish, as typed | 0.4841 | 0.4835 | 0.4839 |
| Turkish, without accents | 0.4095 | 0.4835 | 0.4835 |
| Vietnamese, as typed | 0.6471 | 0.6006 | 0.6453 |
| Vietnamese, without accents | 0.2113 | 0.6005 | 0.6005 |
| Greek, as typed | 0.5956 | 0.6002 | 0.5971 |
| Greek, without accents | 0.3119 | 0.6002 | 0.6002 |
| French, as typed | 0.4932 | 0.4943 | 0.4942 |
| French, without accents | 0.4105 | 0.4943 | 0.4943 |
| SciFact (English) | 0.6618 | 0.6618 | 0.6618 |

Folded, a word is one term however it is accented, at index and query time
alike: a word typed without accents finds every spelling, and so does one
typed with them. That lost where accents tell words apart: Vietnamese's
tones tell syllables apart (`ma`, `má`, `mà`, `mả`, `mã` and `mạ` are six
words), and its queries typed with them lost 0.047, a folded syllable's
list all of them -- a query took 1.75x as long (818 -> 1 432 us). Kept as
written as well (`text::WRITTEN`, `~`), as Elasticsearch's
`preserve_original` keeps the original beside its fold, a query's word
typed without accents reads the fold, every spelling of it, and one typed
with them its term as written where the index holds it and the fold where
it does not -- typed with an accent no document writes it with, it finds
every spelling (`TextIndex::read_words`, which ranking, `facet`,
`highlight()` and `fuzzy` all read a query through, `query_terms` counting
it). A written term is no word of the
document's length, so BM25 scores a document as main did, and it has no
prefixes and no place in the `fuzzy` dictionary: a loose `match` reads a
word typed with accents as written and its fold among its variants, at a
prefix's weight, below it.

**Only a row the statement reads decides a spelling.** Whether a word
typed with accents reads its term as written or its fold is asked of the
rows the statement reads (`TextIndex::holds`, handed them as `text::Reads`):
every row for most statements, and for a scoped token's `match` the rows
its filter selects, the rows its BM25 counts (`Match::within`). Asked of
the index, alice's own `sutlac tarifi` was found by `sütlaç` -- no row of
hers writes it so, and the word read its fold -- until bob wrote a row of
`sütlaç` she cannot read; then the spelling was read, which only his row
held, and her empty answer told her a row she could not read held the
word. The ranking (`search_within`, `search_loose`), the set a facet counts
and a `match` beside `order` or `count` selects (`scored`, handed the rows
the ranking reads as its `within`) and the marks of `highlight()`
(`read_terms`, `loose_words`) all read the query through `read_words`, so
a statement reads a word one way wherever it reads it: `tests/within.rs`
holds a scoped statement's rows, scores, facets and marks to be the same
with bob's rows as without them, and
`a_row_a_statement_does_not_read_decides_no_word` each path. The fallback
is "no row this statement reads writes it so", which for a scoped token is
the access rule's to decide; for any other `match` the rows are the
collection's, as its BM25's are, whatever its `where` keeps -- a `where`
narrows the rows ranked and nothing counted. A filter that fails to
evaluate fails the ranking and the set, which read its rows
(`matching_ids`); the marks, asked of each row by the filter's test, count
such a row as one the statement does not read. The browser holds no scoped
token, and asks the index.

**Case decides a word as written no more than its fold.** A term as
written is the word lowercased, and a letter whose uppercase lowercases to
another is that other, as the fold reads it (`as_written`): the final `ς`
is `σ`, which `Σ` lowercases to wherever it stands, `ſ` is `s`, `ϐ ϑ ϕ ϖ ϰ
ϱ ϵ` are `β θ φ π κ ρ ε`, `ẛ` is `ṡ` and the prosgegrammeni `ι`. Lowercased
alone, every Greek word ending in `ς` had a term as written with no
accent to it, and case decided the word: of `ΟΔΟΣ`, `οδος` and `Οδος`,
`οδος` found the last two and `ΟΔΟΣ` all three.
`under_accents_case_decides_nothing_the_fold_does_not` holds it over every
code point, each beside its lowercase, and beside its uppercase wherever
the fold reads the two alike. The figures first measured for Greek kept as
written held that sigma's terms: 2 685 177 postings (+70% over main's
1 583 771, 44.5 MB) and 0.5975 as typed, where Greek now holds 2 625 766
(+66%, 43.8 MB) and scores 0.5971 -- little, since Greek's monotonic
spelling accents nearly every word of more than one syllable, and the
words ending in `ς` without one are a few short ones (`της`, `τους`,
`τις`) in most documents. Vietnamese, which has no sigma, moved by 3
postings of 10 061 133.

**The fold is the default and keeping words as written an option, because
the second helps Vietnamese alone.** On Turkish, French and Greek it scores
as the fold does or worse (Greek 0.5971 against 0.6002), and every
accented corpus pays its second term a word -- postings and
`memory_bytes`, main / folded / kept: Turkish 4 129 247 / 4 128 332 /
5 278 601 (+28%, 64.6 / 64.2 / 82.9 MB), French 24.20 / 24.11 / 28.04
million (+16%, 311.9 / 310.2 / 360.5 MB), Greek 1.58 / 1.58 / 2.63 million
(+66%, 25.8 / 25.5 / 43.8 MB) and Vietnamese 5.71 / 5.47 / 10.06 million
(+76%, 73.1 / 69.9 / 125.5 MB), nearly every Greek and Vietnamese word
bearing one; SciFact 633 949 / 633 936 / 634 010. `stats()` reads them:
`TextIndexStats::terms`, `postings` and `bytes` count the `~` terms and
their documents beside the folds, as they count a prefix index's `^`
terms. Single runs under a load
of 2 to 3, the index's build per document went 4.97 -> 6.19 us over
Turkish, 8.69 -> 10.37 over Vietnamese and 17.8 -> 27.4 over Greek kept as
written; a `match`, main / folded / kept, Turkish 87.0 / 88.2 / 86.7 us,
Vietnamese 818 / 1 432 / 976, French 1 001 / 990 / 1 035, Greek 85 / 87 /
99, SciFact 52.6 / 52.8 / 49.3 -- typed without accents, a query reads the
fold either way. So `@text(accents)` is what a language whose accents make
different words asks for, and the rest pay nothing for it: an index
without it hands out no `~` term, its query reads a document's terms in
one pass (`query_terms`), and no word is looked up as written nor tested
for a mark. Under `prefix=6`
Turkish scores 0.5532 kept as written as typed (main 0.5547) and 0.5528
without accents (main 0.4681), its postings 14.07 -> 15.21 million: a
word's prefixes are its fold's, so `prefix`, `chars`, `k1` and `b` all
combine with it.

**The tokenizer every index runs knows nothing of `accents`.** `terms`
takes no flag. Under the option `indexed_terms` hands it one closure, which
asks `as_written` of each word the tokenizer did not hand out as a slice of
the text itself -- a word of lowercase ASCII and every gram of Han, kana,
Hangul or Thai are their own spellings -- and makes a word's prefixes
beside; without the option or prefixes it hands the tokenizer `f` as it
is, and a query or an index tests the flag once a text. The option's
first build tested it in the tokenizer, a word at a time, and an index
without it paid: in turns of `for_each_term` over 20 000 WebFAQ documents
(SciFact's 5 183 for English) under a load of 2 to 3, main / that build /
this one, ns a byte, Russian 21.84 / 22.68 / 22.18, Greek 21.25 / 21.78 /
21.71, English 3.80 / 3.41 / 3.55 and Turkish 8.13 / 6.51 / 6.62 (an
uppercase ASCII word lowercased on the ASCII path, where main took the
long one); `TextIndex::insert`, us a document, Russian 14.86 / 15.61 /
15.06, Greek 16.79 / 18.01 / 16.83, English 15.23 / 15.57 / 14.59, Turkish
4.06 / 3.62 / 3.41 -- rough figures, a few percent of noise, the formal
timing in turns apart. A letter of a script neither the table nor its
lowercase reaches -- Cyrillic and every block up to the Latin and Greek
extensions, and every one past them but the Angström sign, whose lowercase
is `å` -- is lowercased as before anything folded, its two lookups of the
table spared; `every_character_folds_as_its_lowercase_and_once` holds those
ranges to the table. Under the option the closure is a call a word the
first build did not make: `insert` with `accents`, that build / this one,
English 14.60 / 16.03 us, Russian 18.87 / 19.58, Greek 25.08 / 25.94,
Turkish 4.85 / 4.96.

**A script the fold never reaches retires no more instructions than main
did.** The formal timing of that build against main (7 pairs in turns,
each started below a load of 1.5) still had Russian slower in every pair:
the tokenizer 21.11 -> 21.73 ns a byte (+3.0%), the index's build
15.17 -> 15.54 us a document, a `put` 18.03 -> 18.47 us. Counted in
instructions retired (`/usr/bin/time -l`, the reading of the documents
taken off), it was the code, not where it landed: Russian's tokenizing
retired 2.5% more than main's as an index reaches it and 3.7% more as
`foldtime.rs` calls it, and with main's loop put back in the branch's
tokenizer 0.6% fewer. Two calls of `char::to_lowercase` in the
loop -- Cyrillic's arm and the fold's, for a letter the table did not
hold -- left the standard library's lowercase lookup out of line, a call a
letter, where main's one call had it inlined; and a `matches!` of the four
ranges a Cyrillic letter is tested against was compiled into vector
instructions, a dozen a letter. So the loop calls the lowercase once, for
the scripts that stay (`stays`, a `match` of its ranges), and the fold's
arm reads the table alone, a letter's lowercase among its bytes, the rare
one no byte holds lowercased out of the loop (`lower_folded`, cold);
`fold_into` is inlined into the loop, natively -- the browser module,
built for size, keeps it a function. Latin-1's signs, IPA and the
modifier letters, outside the table and each its own lowercase, are kept
as written by an arm of their own (`KEPT`): sent down the cold path,
Azerbaijani's `ə` and Uzbek's `ʻ` were a call a letter, where main
lowercased them inline. Its terms are the same: the tokenizer,
`@text(accents)` and a prefix index hand out what that build did for
every code point in nine contexts and over 12 000 Russian and Greek,
5 183 English, 30 000 Turkish and 4 869 Azerbaijani documents.
Instructions retired, millions, main / that build / this one, five passes
of the tokenizer as an index reaches it (`for_each_indexed_term`), three
of `TextIndex::insert` and two of `create index` through the database (a
`put` of the documents taken off):

| | tokenizer | `insert` | `create index` |
|---|---:|---:|---:|
| Russian | 8 256 / 8 464 / 8 025 | 5 811 / 5 941 / 5 679 | 4 050 / 4 135 / 3 964 |
| Greek | 9 542 / 9 843 / 7 672 | 6 721 / 6 902 / 5 600 | 4 679 / 4 801 / 3 931 |
| English | 1 970 / 1 607 / 1 657 | 2 645 / 2 441 / 2 474 | 1 807 / 1 673 / 1 691 |
| Turkish | 4 265 / 3 209 / 3 235 | 4 111 / 3 483 / 3 498 | 2 944 / 2 513 / 2 517 |
| Azerbaijani | 886 / 729 / 650 | 808 / 714 / 668 | 577 / 516 / 483 |

Russian is 2 to 3% under main, and its cycles with it: the tokenizer as
`foldtime.rs` calls it, 2.177 / 2.248 / 2.145 billion, in turns. Greek is
a fifth under main, where it was 3% over: its unaccented letters, most of
its text, were looked up, lowercased and looked up again. English and
Turkish keep what the fold's ASCII path won, within 3% of that build. Tried and dropped: the test for
ASCII after the one for the scripts that stay (Turkish, mostly ASCII
letters, 7% over that build), and the words that are not ASCII handed to
a function of their own, which spared English's loop its registers but
cost Russian a call a word (8 348, over main).

**A word written decomposed is another spelling under `accents`.** A
term as written keeps a word's letters as written: `sütlaç` written whole
(NFC, as nearly every text and keyboard writes it) and `u` with U+0308,
`c` with U+0327 (NFD, as macOS's file names and some Vietnamese input
methods write it) are two terms as written of one fold. A query typed one
way reads the rows written that way where the statement reads one, and
the fold, every spelling, where it reads none; typed without accents it
reads the fold. Without the option the two are one word. Making them one
term takes each composed letter's marks: 746 of the table's code points
decompose canonically, into up to three marks -- 3.2 KB raw and 452 bytes
brotli of data alone (`tools/fold/gen.py` could emit it), before the code
putting a decomposed word's marks in Unicode's order -- where the option
whole costs the module about 200 bytes brotli, for text nearly never
written so. So it is said here and on the site, not built.

**`accents` is index kind 11, its options a byte after it.** `chars` took
kind 7 beside the plain 3, as a quantized graph took 5 beside 2: a binary
from before refuses a kind it does not know (`unknown index kind`) rather
than build the index without the option -- here, read a word typed with
accents as every spelling. Two more kinds for `accents` and `accents` with
`chars` would have doubled with each option after it; kind 11 is followed
by a byte of flags (`TEXT_CHARS` 1, `TEXT_ACCENTS` 2), and a bit this
binary does not know is refused (`unknown text index options`), so the
next option is a bit a binary from before refuses in turn. An index
without `accents` is written as kind 3 or 7, as it always was, so a file
that declares none stays one an older binary opens. `TextIndexSpec`'s
`Debug` leaves `accents` out where it is unset, as `Field`'s leaves out
`beside`: `make fennecql-corpus` compares trees by that form, and every
`@text` of every text would have read as another.

Weights tried for the fold beside the word as written, accented queries,
Turkish / Vietnamese / Greek / French (Greek's before its final sigma was
read as `σ`, above): none, as built, 0.4839 / 0.6453 / 0.5975 / 0.4942;
`fuzzy`'s -- the whole weight, scaled below the least the written term
adds to a document -- 0.4829 / 0.6400 / 0.5964 / 0.4945; a
tenth 0.4816 / 0.6409 / 0.5943 / 0.4946; a quarter 0.4766 / 0.6339 /
0.5871 / 0.4936; and the whole, unscaled -- both terms a query's, as
Elasticsearch with `preserve_original` on both sides reads them -- 0.5384
on Greek. Each weight counts an accented word twice against the words of
the query typed without accents, and it walked one more list a word:
Turkish's `match` 87 -> 101 us at a tenth, Vietnamese's 976 -> 1 241.
Rejected as well, the layout the other way round: the word as written the
plain term and its fold behind a mark, a word typed without accents
reading its own term whole and the marked fold at less. The rare documents
written without accents then ranked first: Greek's queries typed without
accents scored 0.4310 at a quarter, 0.5246 at half and 0.5630 at
`fuzzy`'s weight, against the fold's 0.6002, and reading the two whole is
the fold again for a merge of two lists a word at query time.

## Text index is derived

**The text index is derived data as well, but it is not persisted.** `@text`
builds an inverted index from the documents the first time a statement reads it
after an open — 16 µs per document against the HNSW graph's ~34 µs. Nothing
about it reaches the file, so there is no validation path and no stale-index
case to handle. It is shrunk to fit where it is known complete (its build,
`create index`); live ingest keeps `Vec` growth slack. Its terms grow a
256th at a time, as a hash index's keys do (below).

## Term table

**A text index's word is 9 bytes of a table, 16 of a term, and its bytes**
(`text/terms.rs`). A map of `String` to two `Vec`s, its hash beside it, was
a slot of 88 bytes and three allocations a word: a `--dir` node's tenant of
200 notes of 30 words held 565 KB of anonymous memory for its text index
over a Zipf vocabulary and 1 135 over made-up words (musl, in Docker, 500
tenants) -- 11 to 22 times the tenant's 51 KB, its hash and ordered indexes
4-5 KB each, the map's table 729 KB of the second -- and `memory_bytes`
counted 28% of the tenant, no table and no allocator. A slot is now a
word's place under half its hash, with a control byte; its bytes are in an
arena after their length; a word one document holds (three in four over a
Zipf vocabulary) is a `One` of 16 bytes, the document and its count, no
allocation; a word of more a `List` the slot names directly, as the map's
slot held it. The same tenants' indexes hold 192 and 225 KB, a tenant 594
-> 221 and 1 164 -> 255 KB, and 500 of them building their indexes as they
were written 375 -> 173 and 703 -> 172 MB; in musl's slots (`make
text-memory`) SciFact's index 15.0 -> 10.4 MB and FiQA's 75.8 -> 66.5.
`memory_bytes` counts the table, the arrays and what each list's allocation
takes past its bytes (`ALLOC`, 12): within a tenth of musl's slots where it
was a third (a tenant's notes) to four fifths (FiQA), and 79 and 86% of a
tenant's anonymous memory. A word that goes leaves its place to the next
(`free`, and `spare` for a list, its arrays kept to 16 documents: moved
into by the last, its slot found by its hash again, a browser's update of a
note took 12% longer) and its bytes dead in the arena, written again once
half of it is; a slot that goes moves the ones after it back, no tombstone;
a build closes the places up and shrinks every array. A probe reads a group
of 8 slots by their control bytes, as hashbrown does without SIMD
(`GROUP`): a slot at a time, a word past its own slot cost a mispredicted
exit and `make search-bench`'s build took 149 ms against the map's 142 --
though FiQA, its common words in their own slots, built in 505 against 536
by groups. A word is compared by its length and first seven bytes as one
masked word (`Key`): through `memcmp` a length's branch mispredicted at
each. Past 98 304 words natively the table, arrays and arena are 256 shards
by the hash's top byte, as `maps::Sharded` splits a map: `make growth-bench
GROWTH=text`'s put p50 1.21-1.25 -> 0.83-0.88 us, p99 2.9-7.1 -> 1.6-2.0,
the split's 5.2-6.3 ms -> 3.4-4.7, the index built after an open 1.8-2.6 s
-> 0.46-0.58. In ABBA turns against main, load 2-5: search-bench's build
140-146 -> 139-142 ms and `match` 0.054 either way, SciFact's build 90-91
-> 87 ms (one run of six 94) and FiQA's 559-565 -> 535-539, `match` over
them 4-6% quicker, 400 000 words each in one document 113-178 -> 38-47 ms,
BEIR's nDCG@10 the same to the fourth digit and its loose `match`es as
quick or quicker, an update of a FiQA document 1% slower on macOS (306-309
-> 310-312 us, though its own instructions only +0.23%: the same binary
search, out of line in both, 5-8% apart in samples, and `#[inline(never)]`
on main's own `drop_doc` moved main's up to 0.8%, its removals 0.4-1.2%; a
list's record a cache line, its word's head in it, or its drop out of line,
no nearer) and the same on Linux in Docker's VM, in turns (musl 1 505-1 521
against 1 509-1 514 us, glibc 2 100-2 160 against 2 105-2 161), and of a
SciFact one the same; the browser module's build of SciFact 151-154 ->
145-146 ms (one run of six 155) and of 20 000 notes of made-up words 75-77
-> 32, `match` 3-7% quicker, `make wasm-speed` as it was, for 2.2 KB brotli
of the module, 3.1 KB raw (hashbrown's copy for the map, 3.0 KB raw, gone).
Rejected: control bytes in each group's cache line beside its slots, FiQA
533 ms and search-bench 147; a group's tags compared one by one, 171; the
first two slots picked without a branch, 155.

## fuzzy and prefix

**`match ... fuzzy` and `prefix` walk a sorted copy of the index's words**
(`fuzzy.rs`, `TextIndex::search_loose`). `fuzzy [N]` reads a query's word
within the edits its length allows -- none under 5 characters, one up to 8,
two from 9, Meilisearch's bounds, `N` lowering them -- in restricted
Damerau distance (a swap one edit, as Lucene, Elasticsearch and Meilisearch
count it), an edit touching a word's first character counting two
(Meilisearch's rule: allowed at one, the walk took 4.6x the nodes for 9%
more words, mostly words no one meant); `prefix` reads the last word as the
start of one too. The words are a `Dictionary`: the index's terms but the
`^` ones, in byte order, in chunks of up to 256 a byte arena and their ends
each (1.3 MB for 60 000 words, where a `Box<str>` a word was twice that and
25% slower to walk), sorted by the first loose `match` -- a `OnceLock`, a
`OnceCell` in the browser, as `Derived` is -- and kept up by every write
after it, never in the file. No chunk of it is empty, since finding a
word's chunk, a range's ends and the walk each read a chunk's first word: a
chunk its last word leaves goes, and a dictionary holding none -- sorted
over no words, or every word deleted since -- takes its next word as a
chunk of one. Pushed empty first, as it was, that chunk was asked its first
word and the write panicked: a loose `match`, a `del` of the last row and
an `insert` closed the connection, every later word new to the field
panicking the same way. `a_dictionary_kept_up_is_the_one_built` holds its
shape after every add and remove, through runs that empty chunks first,
last, between and all of them and split full ones, and a walk over it to
one over a fresh build; `TextIndex`'s
`the_words_kept_up_through_an_empty_index_are_a_fresh_build` holds the copy
to a build from the index's words after every write. A prefix is a range
of it; the words within
`k` edits are found walking it as the trie it is, a row of the table a
character banded to `2k + 1` cells, a node's children by binary search --
only the query's characters near the depth where any other would end the
row -- and a branch with no edit left looked up as the rest of the query
rather than walked (a swap, counted from the row above, keeps it walking
one row more). Over FiQA's 135 500 words, 2 000 typo'd words a bound: 14 us
a word at one edit, 120 at two, 4 a prefix; SymSpell's deletions 9 us but
304 MB and 2 s to build, an index of trigrams 8.4 MB and 316 us and no
prefixes, the table over every word 4.3 ms; comparing through `memcmp` was
a third of the walk. Each word's variants of one distance are one more
term -- their lists merged two at a time (`union`), a document's counts
summed, so BM25 saturates over them and their idf is theirs together, a
rare misspelling ranking no higher than its word -- at the word's weight
one edit away or begun, a quarter two away (`NEAR_WEIGHT`: on BEIR with a
typo in each query, the whole weight for one edit took SciFact 0.636 ->
0.663 where half took it to 0.657, a quarter for two took FiQA 0.213 ->
0.229 where the whole took it to 0.226). Where the index holds the word as
typed, its variants together are scaled below the least it adds to a
document -- once, in the longest document there is (`longest`, only
raised) -- so a one-word query ranks every document holding it first, and
queries written whole lose nothing (0.662 -> 0.665 and 0.232 -> 0.233;
unscaled, at half the weight a word one edit away, 0.656 and 0.229). They
are cursors as any term's, so MaxScore stays rank-safe
(`pruning_never_changes_a_loose_answer`). A word reaching
more than `MAX_EXPANSIONS` (10 000) words is refused, never cut: FiQA's
80 426 words hold no one-letter prefix past 7 020. `highlight()` marks the
variants (`Terms::add`), `facet` counts their rows (`TextIndex::scored`). A
plain `match` takes the path it did, `search_within` split into
`query_terms`, `cursor` and `ranked`, the last in line natively: FiQA's
0.261 ms p50 on main, 0.257 here (0.269 called), SciFact's 0.042 either
way, `make search-bench`'s 0.056 ms and `make wasm-speed`'s `match` 0.81 ms
either way. Core, in `text` and no feature of its own -- a search box is no
domain's extra: the browser module grew 17.1 KB, 6.0 KB brotli, the
standard library's sort for `&[u8]` (3.7 KB) a merge of its own there.

## rerank

**`rerank` deliberately uses no index.** `match ... rerank` takes candidates
from the inverted index and reorders them by exact distance over vectors read
straight out of the store — so a collection can do vector retrieval with no
HNSW graph to build, hold, validate or rebuild. Measured on BEIR it matches or
beats a full dense scan while scoring under 2% of the corpus. It requires
`match`: without candidates there is nothing to reorder.

## fuse

**`fuse` adds ranks, not scores.** `match ... near ... fuse` runs both searches
to their own depth -- `candidates`, 20 unless given, never under the page --
with the filter applied to each, and a document scores `1 / (k + rank)` from
each list it is on (`k` 60). A BM25 score and a cosine distance share no
scale, and a weight between them would need retuning per corpus. Measured
with `make beir` (nDCG@10): SciFact 0.700 against 0.662 for `match` and 0.645
for `near`; FiQA 0.366 against 0.232 and 0.366 -- the one path near the top
of both. The depth is the knob that matters: up to 61 a side a document both
searches found outranks every document only one found, and at 100 a side
both scores fall (0.688, 0.359). It is built from what the engine already
had -- both searches, the `HashMap<DocId, u32>` a `DocMap` keeps sparse ids
in, the text index's `best_first` sort -- because in types of its own it was 11 KB of the
browser module; this way it is 2.

**`k` is 0 to `MAX_FUSE_K`, 1 000 000.** A weight is `1 / (k + rank)` in
`f32` (`query::fuse_weight`): while `k + rank` stays under 2^23 two
neighbouring ranks' weights are more than an `f32` apart, so each of a
side's 10 000 ranks weighs more than the next; past it neighbours may weigh
the same, and past 2^24 `k` itself rounds. The parser clamped `k
99999999999` to `u32::MAX`, where a side's first 256 ranks weigh alike;
it refuses it now, naming the range (`fuse_k_keeps_every_rank_apart`),
and natively `Select::check` holds every `Select` to it -- the `/near`
body's `fuse_k`, read as written, and one built in Rust. `candidates` is 1 to
10 000, refused as it is read.

**`fuse` cannot tell a list that found something from one that found
chance**, so a caller whose words match nothing but prefixes leaves them out.
`prefix=12` makes every word of a query reach the three letters it begins
with; for a question in a language the text is not in, that is a list of
`tell` and `version`, and fused it voted as much as the meaning. In the docs
site's search the right page stood 1st to 6th as the prose around it changed
and 1st every time with the list left out. A weight was measured and refused
(at a fifth of a vote the page stood 1st or 2nd, at 0.3 2nd to 5th, a number
fitted to two questions, and `fuse` adds ranks so as to have none to tune);
so was a longer `prefix_min`, which costs the typist (`stud` finds nothing at
4, `compa` at 5). The caller asks instead whether a best hit of the words
holds a word of the query whole or to a five-letter stem, which
`highlight()` marks, and fuses only if so (`site/search-query.js`,
`site/README.md`).

## highlight and facet

**`highlight()` reads the text again; `facet` counts beside the rows.**
`highlight(body)` and `snippet(body, N)` are select-list items (`Mark`,
its arguments one `Vec<Expr>`: Select's clone of them as fields was 432
bytes of the browser module), answered under their label, after the fields
or after `*`; the same one twice is refused, since a JSON row holds a name
once. They need `match`, and go with `fuse` and `rerank` -- a row only
`near` found has no marks. Nothing is kept in the index for them: the
row's text is split again by the tokenizer of the `@text` index the
`match` searched (`text::terms` hands each term its byte span, which the
index passes over: the build and `match` measured as before),
`highlight::Terms` holds the query's terms in the text index's own map (a
sorted `Vec<String>` was a sort of its own, 3.5 KB), and a span is marked
where a term the text yields is one of them -- so a mark is what the index
matched: a word whatever its case, the whole word a `prefix=N` term was cut
from, the characters of a Han or Thai run's matched pairs or triples,
overlapping ones merged. A span is widened over the marks of the characters
at its ends (`highlight::extends`, the blocks a text puts combining marks,
joiners, selectors and Hangul jamo in, not Unicode's whole table), and the
offsets go out as UTF-16 code units, what every client but Go's and
Python's indexes a string by. Without tags a column is `[start, end]`
pairs, a snippet's `{marks, text}`; with them the marked text, not escaped.
A snippet is the window of `N` words (a Han character a word) with the most
marks, centred on them. `facet f [top N]` counts over the rows the query
selects before `offset` and `limit` -- the filter's, `match`'s
(`Database::matched_rows`, every row its ranking scores above zero, the
filter's among them: [match as a filter](#match-as-a-filter)), a required
`lookup`'s -- and is refused beside `near`, which ranks
every row the filter passes rather than selecting some. A `@hash` field
of text, int, bool or timestamp, whose bucket key is the value's encoding,
counts from the buckets (a bitset of the set when there is a filter); the
rest reads the field of every row, a list once a row for each value, null
a value, counted under `codec::encode_value` as `group` keys its rows, so
`tests/facets.rs` holds a scalar facet to `group ... count` over the same
filter. Ordered most first, then by value in the field's collation,
through `order_rows`; past `MAX_FACET_VALUES` (10 000) without `top`
refused, never cut. The counts live in `ResultSet::facets`, beside `nested`,
never in a `Value`, and a JSON answer carries them as `"facets"` after the
rows -- over HTTP the bare array becomes `{"rows", "facets"}` only when a
query asked for them. Over 100 000 documents a row's marks cost 1 to 3 us,
a snippet's 2 to 4; a facet over every row 0.01 ms through `@hash` and 6.1 by
the scan (`make search-bench`). The browser module grew 20.2 KB, 7.0 KB
brotli; the one without indexes 3.9 KB brotli, which counts facets and
refuses a `match` and its marks.

## Disjunctive facets

**A disjunctive facet's filter is split where the filter is written.**
`facet brand disjunctive` counts over the rows the query selects with the
`and` chain's terms that read the field alone left out (`Facet::rest`,
`Select::split_facets`); a term reading it beside another field is
refused, not guessed at. The split is the parser's and a REST `facet=`'s,
from the filter as written: `scoped()` and `@ttl`'s `alive` reach the
split filters through `each_filter_mut`, so a token's rules and an expiry
are never what is left out -- split after them, `facet owner disjunctive`
counted every user's rows (`tests/access.rs`). A select built in code is
split in `answered_select`, before the expiry; one that leaves nothing
out is a plain facet. Each disjunctive facet finds its own rows
(`facet_set`: `matching_ids`, `match`'s `matched_rows`, a required
`lookup`) in the same statement under the same lock. `facet price ranges
[0, 2500, 5000]` counts a number by range, every range in order, value
`[from, to]` (`facet_ranges`): through a `@sorted` field's index when every
bound is a key it holds exactly (`SortedIndex::range_counts`: over every
row two searches and the chunks' lengths between, `Chunked::count`; under a
filter its entries walked against the set's bits) and by reading the field
otherwise; the lexer keeps `ranges [..]` as written, as it keeps `in
[..]`, an `f32` vector holding no bound past 2^24 exactly. Over 100 000
rows: six ranges 0.004 ms through the index against 4.7 read, 0.15
against 0.19 under a filter keeping 3%; a page with two disjunctive facets
0.75 ms in process as its three statements were, over HTTP 0.95 against
1.19 in turn. The value-count scan keeps its own loop (`each_value` serves
the ranges): through a shared one, inlined and all, it went 4.8 -> 5.2
ms, and with `facet` out of line 4.8 -> 5.6 (`#[inline(always)]`). The
two cost the browser module 2.9 KB brotli -- the split, the ranges'
read and index paths, the parser -- kept in it, so a replica or a page
answers a shop's sidebar as its server does.
