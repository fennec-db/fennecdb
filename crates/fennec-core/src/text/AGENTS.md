# AGENTS.md

fennec-core's text index's words -- the tokenizer's fold and the table of words -- for code in `crates/fennec-core/src/text/`, which binds `text.rs`'s tokenizer and its reading of a query (`text::grams`, `indexed_terms`, `read_words`) too. The rest of fennec-core's rules, *Text*'s among them, are in [its AGENTS.md](../../AGENTS.md), the rules every change meets in the root's [AGENTS.md](../../../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Text

**[`@text` matches whole words unless told
otherwise.](../../../../docs/design/text.md#tokenizer)** It folds `I`, `İ`, `ı`, `i`
together and a Latin or Greek letter to its base letter (`sutlac` is
`Sütlaç`) at index and query time, through `text/fold.rs`,
`tools/fold/gen.py`'s alone (`--check`); unspaced scripts go by grams
(`text::grams`). `@text(accents)` (kind 11, a byte of flags) also indexes
a word its accents make another as written (`text::WRITTEN`), by
`indexed_terms` around a tokenizer that tests no flag, case deciding it no
more than the fold. Ranking, `facet`, `highlight()` and `fuzzy` read a
query through `read_words` (`query_terms`): a word typed with accents its
spelling only where a row the statement reads holds it
(`TextIndex::holds`, a scoped token's `within`), never a row it cannot,
else its fold. Nothing else folds: `=`, `like`, `nocase`, collations and
the other indexes compare as they did.

**[A text index's words are a table of their own
(`text/terms.rs`).](../../../../docs/design/text.md#term-table)** `memory_bytes`
counts each list's allocation past its bytes (`ALLOC`).
