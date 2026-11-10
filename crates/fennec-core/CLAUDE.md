# CLAUDE.md

fennec-core's invariants -- storage, writes, indexes, the HNSW graph, vector and text search, `lookup` and aggregates, and the browser module's builds -- for code in `crates/fennec-core/`. The engine's lock and blocks -- prepared blocks and pinned reads -- are in [src/engine/CLAUDE.md](src/engine/CLAUDE.md), beside their code. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### Storage

**[No buffer pool of its own, and the file is mapped.](../../docs/design/storage.md#the-mapped-file)**
`fs::open` maps it, a read decoding over its bytes; a rewrite points the
stores at the new file (`Database::repoint`). The mapped path is
`std-fs` on a 64-bit Unix or Windows, every condition naming it so
(`any(unix, windows)`). On Windows the file is views laid end to end in a
placeholder, one at the open and one each handover (`Mapping::cover`),
and a volume without POSIX renames or a share is read into memory
(`fs::mappable`). In code, docs and site, "page cache" is the operating
system's, a "buffer pool" a database's own, which fennecdb has none of.

**[Writes are handed over to the mapped file.](../../docs/design/storage.md#handover)**
Past `HANDOVER_AT` or `HANDOVER_DOCS`, `Database::hand_over` points the
stores at it, never with a block open, the file the same byte for byte; on
Windows at a view laid past the last, a mapping of many views let go of
on a thread, not under the write lock (`map::Teardown`).

**[A block past 16 MB spills into the file before it lands.](../../docs/design/storage.md#spills)**
A spill (kind 10) counts only where a land (kind 11) names it; the feed
sends the block as one record (`landed_block`).

**[A lone `create index` or `compact` runs beside the database](../../docs/design/storage.md#maintenance-beside-the-database)**
(`Database::maintain`): every write must pass `Database::note`; a schema
change meanwhile fails it, and an image adopted meanwhile marks every tail
changed (`Tails::all_changed`).

**[A file compacts itself once half of it is dead](../../docs/design/storage.md#compaction)**
and 64 MB: `CompactPolicy` is the one rule, run beside the database by
`Compactor` or `fennec_http::link` (`--auto-compact`), on replicas too.

**[A file is compacted before it outgrows the page cache](../../docs/design/storage.md#compaction-and-the-page-cache)**
(`CompactPolicy::memory`) so its live records fit the cache's room
(`cache::room`): once a third of what it writes is dead and the file
reached `ROOM_SHARE` of that room or the process faults
(`cache::faulting`), its own faults forgotten (`cache::forget_faults`).

**[File format](../../docs/design/storage.md#file-format):** a record is
`[kind][collection-id][length][body]`, its length read even when empty;
one this version writes ends in its CRC32C (`crc.rs`), its kind's
`CHECKED` bit set, the length counting the sum -- inner records and the
counter's head carry none, nor one from before, read as it is.
`head` writes a head (the counter's `image_head` aside), `record_at` reads
it, `Walk` walks them. An open holds each record past the image to its
sum, a graph a later one replaces aside: one failing is a torn tail, cut,
unless a whole record follows it, refused then naming its byte, as
`apply`, an archive and `fennec verify` hold them.
A torn tail is cut on open (on Windows by a second open, the first's
views gone: `fs::windows::cut_and_open`), zeros to the end one too where
they start at a head -- after a byte that is not zero or a record whose
sum holds, or at the image's end (`zero_tail`: a length the disk took
before the bytes) -- any other zero head refused, a torn image refused,
and an unknown kind's refusal names its byte; the file only grows; a tool
that only looks opens with `fs::open_read_only`, which cuts, creates and
writes nothing. A new record kind, index kind, type tag or graph version
is a number a binary from before refuses (a graph's, rebuilds from), never
one it would misread.

### Durability

**[Buffered writes are written behind the lock](../../docs/design/durability.md#writes-behind-the-lock)**
(`Database::write_behind`), asked after every write no fsync follows.

**[A durable write syncs a log beside the file, not the file](../../docs/design/durability.md#the-sync-log)**
(Linux and Android, `synclog.rs`). Whatever renames a file into a
database's place removes the log first, durably (`fs::forget_sync_log`),
and syncs the directory after (`fs::sync_dir`, which on Windows flushes
the renamed file, then its directory).

### The HNSW graph

**[The HNSW graph is derived data, not a
cache.](../../docs/design/hnsw.md#the-graph-is-derived)** Never written on the
write path; an open that cannot validate it rebuilds it silently. A
tombstone carries its vector, a rewrite touches only indexes whose field
changed, to the bit (`unindex_doc`), and an unfiltered `near` tombstones cut
short walks again wider or searches exactly (`past_tombstones`).

**[A restore gives back the arena it had, to the
bit.](../../docs/design/hnsw.md#restored-arena)** Each vector is normalised in
the write path's order (`flat_sq`, `flat_sqs8`).

**[A graph record is laid out flat.](../../docs/design/hnsw.md#flat-graph-records)**
Versions 9 (images) and 10 (tails) are written; every layout reads into
one set of arrays (`Read`), built in one place (`build_restored`).

**[A batch links in parallel, into the graph it would link in
turn.](../../docs/design/hnsw.md#parallel-linking)** A pruning reads only the list
it prunes and the vectors, and each list's additions go in node order
(`link_batch`).

**[The upper layers are walked with a
beam.](../../docs/design/hnsw.md#upper-layer-beam)** A search and an insert walk
each layer above 0 with `UPPER_BEAM` (4).

**[A block's `put`s link their vectors
together.](../../docs/design/hnsw.md#block-linking)** Natively they wait
(`defer_batch`), linked before the block's record is written; a search in
the block measures them exactly, and a rollback drops those it made
tombstones (`forget_waiting`).

**[A server answers before its graph is
linked.](../../docs/design/hnsw.md#deferred-linking)** `fs::open_serving`
leaves the tail's vectors for `link::beside` to link, a search measuring
each beside its walk.

**[A server keeps its graphs in its
file.](../../docs/design/hnsw.md#graphs-in-the-tail)** `link::keep` appends a
changed graph (kind 4) under the read lock (`Database::save_graphs`),
moving no counter, sent to no replica.

**[A vector written again is one node.](../../docs/design/hnsw.md#aliases)**
Every insert, batch and deferral goes through `VectorIndex::place`: a
vector a live node holds to the bit makes its document an alias.

**[Natively the copies table grows from its own
slots.](../../docs/design/hnsw.md#the-copies-table)** `Same` grows reading no
vector, in 256 shards past `SAME_SPLIT`.

**[A graph record carries the copies table's
halves.](../../docs/design/hnsw.md#halves)** Every writer writes them
(`HALVES`), so a record is the same however its index was made.

**[The browser module hashes as a native build
does.](../../docs/design/hnsw.md#halves-in-the-browser)** It takes and writes the
halves.

**[An open copies a graph's vectors into the
arena.](../../docs/design/hnsw.md#opening-a-graph)** It restores and validates
the graph then.

### Writes

**[`insert` never writes over; `put`
does.](../../docs/design/writes.md#insert-and-put)** A held id is
`Error::Duplicate` (409); a scoped rewrite keeps `insert`, and the
builders' `.insert()` sends `put`, which the sync's undo writes rows back
through, `.insertNew()` `insert`.

**[A `set` reads the row it writes, and every write says what it
did.](../../docs/design/writes.md#arithmetic-in-set)** `query::arith` never
wraps past 64 bits; the record holds the result, never the expression.

**[`require <n>` makes a write's count a
condition.](../../docs/design/writes.md#require)** Any other count is
`Error::Unmet` (412), the block put back; `n` is an integer, never a
parameter. A `get` takes it too, counting after `offset` and `limit`
(`read_required`); a sync replica refuses one reaching an unanswered
insert's row (`REQUIRE_UNANSWERED`) or a `get`'s beside a synced write
(`GUARDED`). `else '<reason>'` after it is a text of the statement, 1 to
200 bytes, no backtick and never a parameter, leading the refusal's
message, where `Error::reason` reads it back up to the first `` (` `` --
never a field beside it -- for every answer's `reason`.

**[`if absent else set` is an upsert; `put <c> $n` takes a parameter's
documents.](../../docs/design/writes.md#upserts)** Scoped, the row it sets
passes the update filter (`Check::before_overwrite`) and `$n`'s documents
go in first (`Statement::with_documents`); a sync replica refuses a
document naming neither id nor key (`UPSERT_KEY`).

**[A `set` or a `del` picks its rows with `order` and `limit`, and
`returning` answers them.](../../docs/design/writes.md#picked-writes)**
`page_ids` picks them under one lock; `returning` is held to the read
rules (`Hook::answering`).

**[A key of `order` may be an
expression](../../docs/design/writes.md#order-by-expression)** (`Sort::expr`,
`order_ids_by`), ties in id order; `check_order` refuses an inner `get`,
an aggregate or `expired()` in one.

**[A `put` answers the rows it wrote with
`returning`.](../../docs/design/writes.md#put-returning)** Each row reaches
`Hook::answering` as the write it was. A write clause goes into every
builder, held to the golden file, and the sync refuses one a replica
cannot answer (`PICKED`), as a scenario holds.

### Indexes

**[`@unique` is a `@hash` that asks its bucket before a
write.](../../docs/design/indexes.md#unique)** `Collection::unique_clash` runs
after the hooks, before any write; a scoped token is told the field alone
(`access::told`).

**[A field keeps one index, and a text field `@text` beside a `@hash`,
`@unique` or `@sorted`.](../../docs/design/indexes.md#two-indexes-on-a-field)**
`Field::add_index` is the one rule wherever a field is indexed; any other
second index is refused where it is declared, never dropped. The text one
is `Field::beside`, looked for through `Field::text_index`, written behind
`TAG_BESIDE`.

**[A `@hash` bucket holds its ids ascending, in
runs](../../docs/design/indexes.md#hash-buckets)** (`engine/bucket.rs`), read in
order through `Bucket::iter`.

**[A hash index's table grows a 256th at a time, and a text index's terms
too](../../docs/design/indexes.md#sharded-tables)** (`maps::Sharded`): natively,
past `SPLIT_AT`, 256 maps, each key kept beside its hash; the browser
module keeps one map.

**[`@ttl` is an ordered index whose rows expire, and every read leaves them
out at once.](../../docs/design/indexes.md#ttl)** Every filter must have
`Database::alive` ANDed in; a server sweeps (`fennec_http::sweep`) a row a
pass past its time (`sweep::GRACE`), never a replica, a following tenant
or the browser, and a scoped token reads `expired()` only by the `expired`
grant.

**[Only an `and` chain reaches an index.](../../docs/design/indexes.md#index-reach)**
An `in` takes buckets only when every element resolves, else the scan.

**[A filter is bound to its collection once a
query.](../../docs/design/indexes.md#bound-filters)** What `Filter` cannot bind
stays `eval`: an error at the first row, never over none. A stream's look
binds a filter holding a long `in` list once a look, its lists kept for
the next (`Database::changes_held`, `Lists`, their sets alone), where it
reads another collection.

**[`@sorted` must give the scan's answer, row for
row.](../../docs/design/indexes.md#sorted)** Keys order as `Value::cmp_value`,
which compares an int, a float and a time by their true values
(`value::tie`), never as two `f64`s alone, every index's keys and
`Value::key_as` with it, and any other two kinds by kind (`type_rank`), a
time below every text; a comparison, `greatest` and `least` read a text
beside a time as the time it names, either side (`Value::cmp_read`), never
the order, an index stepping aside where its keys cannot say it
(`key_as`, `lookup_key`). A
`NaN` equals a `NaN` and is above every other number, `+inf` included --
PostgreSQL's order (`value::floats`), never equal to everything -- every
`NaN` one key whatever its bits, `-0.0` that of `0.0`, in a list or a
vector too (`float_key`, `codec::encode_key`); a key is equal exactly
where `cmp_value` is, a whole float and a time filed as their int; `null`
apart, ties in id order; a literal of another kind than a
typed field's -- or part's -- is no bound or none (`SortedIndex::by_kind`),
and a time claim's `or` edge is folded by the field's type (`fold_kinds`).

**[A whole float goes out as every digit of the int it
is.](../../docs/design/indexes.md#sorted)** JSON writes one short of 2^63
either side so (`json::num_into`), never its shortest digits, which past
2^53 read back as another int: a value read back finds its row
(`a_number_read_back_through_json_finds_its_row`), the router's order check
with it. The browser module reads an int no double holds in a parameter as
the double JavaScript wrote, where its reader met one (`json::wide`,
`fennec-wasm`'s `as_double`).

**[An index over several fields is its parts' bytes laid end to
end](../../docs/design/indexes.md#composite-indexes)** (`composite.rs`), a part's
bytes ordering as `cmp_value` orders its values or held apart.

**[An open builds no hash, text, ordered or sparse
index.](../../docs/design/indexes.md#lazy-indexes)** The first read fills its
`Derived`; a write skips an unbuilt one, and `stats` and `memory_bytes`
count what is built, never building to count.

**[A server warms what an open left for the first
read](../../docs/design/indexes.md#warm)** (`--warm`, `fennec_http::warm`), an
index at a time under the read lock; a bench's warm-up reads every index
its writes keep up.

**[A point is two `f64`s on Redis's sphere; `@geo` is Morton
keys](../../docs/design/indexes.md#geo)**: `distance` uses `geo.rs`'s own
series, never libm; a list bound for a point is read as written
(`DataType::keeps_numbers`).

### The browser module

**[Threads are `cfg`'d out of WASM.](../../docs/design/browser-module.md#no-threads-in-wasm)**
Nor has it a clock: `now()` reads the time a statement is handed
(`Database::set_clock`), or errors.

**[Indexes are features; a build without one opens a file declaring
it.](../../docs/design/browser-module.md#index-features)** A missing one is a type
of no value (`off.rs`), its checks behind `EVERY_INDEX`, and a replayed
`create index` of one is taken unbuilt, never refused; a crate using
`fennec-core` names `full`, and `fennec-ql` and `fennec-abi` name it in their
dev-dependencies alone.

**[The rest of the language is features
too.](../../docs/design/browser-module.md#language-features)** Its checks ask
`crate::FULL` first; a build without one opens every file, refusing what
it cannot answer -- a function by name (`missing_function`), every
statement over a `nocase` or collated field's collection
(`refuse_unbuilt`). `FENNEC_WRITE_FIXTURE=1` rewrites `tests/language.fennec`.

**[What is core, and why one module
ships.](../../docs/design/browser-module.md#what-is-core)** What any app expects
is core, a domain's extra a feature.

**[The module holds one copy of a
generic.](../../docs/design/browser-module.md#one-copy-of-a-generic)** Sort
through `sort_unstable`, `text::best_first` or `order_rows`; slice a `str`
with `get`, never `s[a..b]`.

**[The core lost what it held twice or for one
use.](../../docs/design/browser-module.md#size-cuts)** What std brings in for one
call is the crate's own (`num::parse_i64`).

**[The module grows nothing by doubling past a
mebibyte.](../../docs/design/browser-module.md#chunked-memory)** Its memory never
goes back, so vectors are 1 MiB chunks (`ROWS_CHUNK`) and a segment
doubles only to its sealing record.

### Vector search

**[wasm32's SIMD kernels match the scalar ones bit for
bit.](../../docs/design/vector-search.md#simd)** `vector::simd` keeps the
scalar loop's order (`web/fennec.test.js`).

**[aarch64's strips are NEON, to the same
bits.](../../docs/design/vector-search.md#neon)** `vector::neon` multiplies and
adds unfused in `strip8!`'s order, held by
`f32_and_f16_kernels_match_the_scalar_strips`.

**[A vector goes out as its
`f32`s.](../../docs/design/vector-search.md#vectors-out)** JSON writes the
shortest text reading back as it (`json::num32_into`), but for
`json::TIE`.

**[A number is read exactly.](../../docs/design/vector-search.md#numbers-in)**
`num::parse_f64` reads one, and natively `num::clinger` a list of numbers
alone into `f32`s. An `[int]`, `[float]` or `[timestamp]` keeps a list as
written, as a json field does (`DataType::keeps_numbers`): a vector of
whole `f32`s short of 2^24 is the integers written, no quick read making
one of a list hiding a rounding (`value::quick_vector`), and any other is
read again where `Database::exactly` names it. A filter compares a vector
field with a list of numbers by its `f32`s (`value::compared_vector`). A
stored statement runs its text as written, held to its quick read's digest
(`fennec_ql::stored_definition`).

**[A vector parameter goes into the module as
`f32`s.](../../docs/design/vector-search.md#vector-parameters)** `run` hands it
beside the JSON (`vectorsApart`), `-0` as `0`.

**[A quantized index holds codes, and `near` orders by the
vectors.](../../docs/design/vector-search.md#quantized-indexes)** `Space` orders
the beam by the stored vectors, so every score is exact.

**[A bit code is a residual's signs from a learned
centre.](../../docs/design/vector-search.md#bit-codes)** Its centres are
learned alike on every target and travel in the graph record
(`BIT_CENTRED`).

**[Filtered `near` needs its
fallback.](../../docs/design/vector-search.md#filtered-near)** A filtered ANN walk
short of the limit must search the whole set.

**[`sparse<N>` is pgvector's `sparsevec`, and `@inverted` answers
exactly.](../../docs/design/vector-search.md#sparse-vectors)** Every way in goes
through `sparse::normalise`.

### Text

**[`collate und` and `collate tr` are ICU's
orders.](../../docs/design/text.md#collations)**
`crates/fennec-core/tools/collate/gen.py` writes the tables, and equality
stays the bytes'. In the browser a statement meeting a chunk it lacks is
refused (`collate::refuse`), a write before it changes anything, and
`Fennec.query`, the builder, `restore` and the sync fetch it and run it
again.

**[`nocase` text compares by its case fold wherever it
compares.](../../docs/design/text.md#nocase)** Every such place goes through
`Field::text_order`; the fold is `str.casefold()`'s, Turkish's under
`collate tr`, and a field made `nocase` or no longer is a `rebuild`.

**[`match` prunes with MaxScore.](../../docs/design/text.md#maxscore)** Scores
add in `f64`; `pruning_never_changes_the_answer` holds it to an
exhaustive merge.

**[Beside `order`, `count` or an aggregate, `match` is a filter: the rows
its ranking scores above zero.](../../docs/design/text.md#match-as-a-filter)**
`Select::match_ranks` tells the two apart. The set is `TextIndex::scored`,
made from the ranking's own cursors and `within` and never holding more
than twice the index's documents, then the statement's filter -- a token's
rules, `@ttl`'s `alive` -- as a `where`'s rows are
(`Database::matched_rows`, which a `facet` beside any `match` counts).
`tests/match_filter.rs` holds it to the ranking's ids, and every form to
`id in (get ... match ... select id)` where that list, the ranking's first
10 000 rows, holds them all. It keeps no row ceiling (a ranking keeps
`MAX_MATCH_ROWS`), its rows charged to a call, and a scoped token's
`explain` tells the rows it kept alone; `rerank`, `order _score`, marks
beside `count` and an ungrouped aggregate's `limit` without `order` are
refused.

**[`@text` matches whole words unless told
otherwise.](../../docs/design/text.md#tokenizer)** It folds `I`, `İ`, `ı`, `i`
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

**[The text index is derived and never
persisted.](../../docs/design/text.md#text-index-is-derived)**

**[A text index's words are a table of their own
(`text/terms.rs`).](../../docs/design/text.md#term-table)** `memory_bytes`
counts each list's allocation past its bytes (`ALLOC`).

**[`fuzzy` and `prefix` walk a sorted copy of the
words.](../../docs/design/text.md#fuzzy-and-prefix)** Every write keeps it up
once built, never in the file, no chunk of it empty -- through its last word
out and the next in (`Dictionary::sound`) -- and a panic's put-back lets it
go (`forget_words`); a word reaching more than `MAX_EXPANSIONS` (10 000)
words is refused, never cut.

**[`rerank` deliberately uses no index.](../../docs/design/text.md#rerank)** It
reorders `match`'s candidates by exact distance over stored vectors.

**[`fuse` adds ranks, not scores.](../../docs/design/text.md#fuse)** Each search
runs `candidates` deep, filtered; a row scores `1 / (k + rank)` from
each, `k` 60, at most `MAX_FUSE_K`, well short of where `f32` weighs
neighbouring ranks alike.

**[`highlight()` re-reads the text; `facet` counts beside the
rows.](../../docs/design/text.md#highlight-and-facet)** Marks are the searched
index's tokens; a facet past `MAX_FACET_VALUES` (10 000) without `top` is
refused, never cut.

**[A disjunctive facet's filter is split as
written.](../../docs/design/text.md#disjunctive-facets)** `scoped()`, `@ttl`'s
`alive` and anything else ANDed in after must reach every split filter
through `each_filter_mut`.

### lookup and aggregates

**[`lookup` chains, and the chain is still
positional.](../../docs/design/lookup-and-aggregates.md#lookup-chains)** `on
child = parent` names the level just above, and a collection appears once
a query.

**[`lookup` is one bucket probe per parent, not a
join.](../../docs/design/lookup-and-aggregates.md#lookup)** An unindexed child
key is refused; nesting is never a `Value`.

**[`required` is the other half, and it is a
pass.](../../docs/design/lookup-and-aggregates.md#required)** It drops a parent
no child matches before `offset` and `limit`.

**[A `lookup` over a key of several fields is a run of an index over
several fields.](../../docs/design/lookup-and-aggregates.md#lookup-over-several-fields)**
`tests/composite_lookup.rs` holds it to plain `get`s.

**[A `lookup` level may fold its children, and a `lookup` hangs off
groups.](../../docs/design/lookup-and-aggregates.md#folded-levels)** A level is
the `get` its children would be; `tests/lookup_aggregate.rs` holds it.

**[An item of a select list is an expression, and an aggregate folds
one](../../docs/design/lookup-and-aggregates.md#select-expressions)**: an
aggregate is a call, never an `Expr` variant, and `count(distinct)` past
`MAX_DISTINCT_VALUES` is refused.

**[`approx_count_distinct` is HyperLogLog, and `having` keeps
groups](../../docs/design/lookup-and-aggregates.md#hyperloglog-and-having)**
(`hll.rs`): its own hash, the same on every target.

**[`bucket(at, 1d, 'Europe/Berlin')` reads a zone's
clock](../../docs/design/lookup-and-aggregates.md#time-zones)** (`tz.rs`):
`tools/tz/gen.py` writes `tz/zones.bin`; before 2000 a named zone is
refused.
