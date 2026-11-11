# Indexes

The long form of the invariants [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) lists under *Indexes*: `@unique`, two indexes on a field, hash buckets and tables, `@ttl`, what reaches an index, bound filters, `@sorted`, composite indexes, lazy builds, `--warm` and `@geo`. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## @unique

**`@unique` is a `@hash` that asks its bucket before a write.**
`IndexKind::Hash { unique }`, written as index kind 8 so a binary from
before refuses the file rather than open it as a plain hash and take the
duplicates. `put`, `insert` and `set` ask `Collection::unique_clash` after
the hooks and before anything is written: the bucket the value would go in
holding another id is `Error::Duplicate` (409), the statement put
back whole -- and the id `put` handed out for the document handed out
again, since nothing of it reached the store for the block's mark. A block's
earlier writes are in the bucket as a write keeps a built index up, so it
may free a value and take it again; `null` is no value; equality is the
hash key's, so `-0.0` meets `0.0`. The index is a `Derived` like any hash,
and the first ask after an open builds it from the documents, which is what
makes the answer exact: an open costs nothing more (19 ms for an image of a
million rows either way), the first write 15 ms over 100 000 rows and 259
ms over a million, and a put after it 940 ns against 850 under `@hash`; a
collection with no unique field writes as before (848 against 852). `create
index ... @unique` over a value held twice is refused naming it and two of
its documents, under the lock or beside the database, where it is asked of
the index once the writes made meanwhile are in. `Database::apply` builds
it and asks nothing: the primary did. The clash names the value as the
row holding it stores it, read from that row (the refusal alone pays it):
under `nocase` the spelling written is not that one, and a sign-up as
`SAMI.lindqvist@...` was told the row held `SAMI...` where it held
`sami...`. A float JSON has no number for -- a `NaN`, an infinity -- is
named `NaN`, `inf` or `-inf` (`json::named`), in a list too: JSON's `null`
had said "document 1 holds null already" of a `NaN`, a value no `@unique`
refuses. A scoped token is told the field alone (`access::told`, in
`within`): the clash names the other row's id and echoes its value, which
told alice that bob's profile existed, where, and what it held. A
`@unique` beside a `@text` ([below](#two-indexes-on-a-field)) is this one:
the field's own index, its bucket asked by every write as above.

## Two indexes on a field

**A field keeps one index, and a text field `@text` beside a `@hash`,
`@unique` or `@sorted`.** `Field::add_index` is the one rule -- for
`create collection`, `alter collection ... add field`, `create index`
(under the lock and beside the database), a description's `"also"` and
`fennec_ql::schema_text` -- and any other second index is refused where it
is declared, never dropped. The text one is `Field::beside`, every place
that looks for a text index reading `Field::text_index`; the field's own
stays `Field::index`, so every check of a hash, an ordered, a unique or an
expiring index reads what it read. A schema writes the text one behind
`TAG_BESIDE` (18) before the field's type, which a binary before it meets
as an unknown type tag and refuses the file for.

The cause. `field_def` took each `@` as `field.indexed(kind)`, which sets
the field's one index: the last annotation won and the others were
dropped without a word. `email text @unique @text` was a text index alone
-- every write took a duplicate, `insert`, `put`, `set`, an upsert and a
`/batch` alike, with `nocase` or without -- and `@text @unique` a unique
index alone, whose `match` was refused for having no text index. Every
other way a field is indexed already refused a second: `create index` on
an indexed field was 409 (`check_index`), a description from statements
said "has two indexes: a field takes one", and the TypeScript declarations
`two indexes: fennecdb takes one a field`. The parser alone dropped one.

Which two, and why. An index answers a kind of question: `@hash` and
`@unique` equality, `@sorted` and `@ttl` equality, ranges and order,
`@text` words, `@hnsw`, `@inverted` and `@geo` each its own type's. Two
of the first four answer one question twice -- `@unique` is a `@hash`,
`@sorted` answers `=` too, `@ttl` is an ordered index -- and a vector, a
sparse vector or a point takes one index its type has. `@text` answers no
question the others do, and none of them answers its: so it is the one
index that stands beside another, and a text field is the one that wants
it -- an email unique and found by a word of it, a title ordered and
searched, a tag looked up and searched. `@unique @sorted` is refused with
the rest, though the two answer different questions: the one-field
unique-and-ordered key has no reader yet, and a field keeping three
indexes, or two of the four, would be a list where every check of the own
index looks today; `@unique(a, b)` over several fields is ordered and
unique already. A path into a json field keeps one, as before.

Rejected: `Field::index` made a list, which every one of some eighty
places that read a field's index would walk, and the schema answers'
`"index"` a list each client parses again, for pairs the rule refuses
anyway; `@unique` beside `@text` written as a one-field unique index over
several fields, which needs the `composite` feature for a check every
build makes, and shows the field as two declarations it was not written
as; and refusing `@unique @text` at definition, which leaves an email
that cannot be both unique and searched.

Kept up. The collection's indexes were already held by kind and field
name (`hashes`, `texts`, `sorted`, ...), and every write keeps each map up
by the name, so a field in two maps needed no write path of its own:
`Collection::new` and `reset_index_structures` put the text one in
`texts` beside the other, `build_index` builds both (`build_kind`, one
kind), and `create index` builds only the one it adds. A block put back
takes off only that one (`Undo::Indexed`'s flag), the field keeping what
it had; a replica applying the schema builds only what changed of the two
(`apply`), as a `create index` on an unindexed field always did. The
declared plan compares the own index and the text one each as an index
(`index_plan` twice), so a `@unique` declared beside a `@text` the
database has is its `create index ... @unique`, and one taken off a
refusal with the rebuild. The schema answers -- `/collections`, the
module's, a description's field -- write `"also"` beside `"index"` only
where there is one, so a schema without stays byte for byte; the sync's
replica DDL (`schema_ddl`, `schemaDDL`) makes the text one too, the
`@unique` a hash as ever, and the server refuses the duplicate.

A file made before. Its schema holds the one index the parser kept, and
nothing of the one it dropped: an open cannot know a `@unique` was meant,
so it serves as it did. The `@unique` is put back by `create index ...
@unique` -- or by a declared schema saying `@unique @text`, which plans
that statement -- and over documents already holding a value twice it is
refused naming the value and two of them, as `create index ... @unique`
over any duplicates is, nothing built until each value is held once:
taking the index and reporting the duplicates would have left a `@unique`
that did not hold, which a write after it could not tell from one that
did.

The corpus (`make fennecql-corpus`), 10 095 texts against main: 13 read
as another tree and 2 are refused that parsed, every one this change's
own `@unique @text`, `@hash @text` and `@unique @sorted` texts -- no text
main holds declares two indexes on a field. `Field`'s `Debug`, the form
the corpus compares, writes `beside` only where it holds one: derived, it
changed every field of 1 048 texts.

Size and speed. No write or read path changed: the index upkeep walks the
same maps, `unique_clash` asks the same bucket, and `text()` reads its
spec through `text_index`'s one match; the parser's only new work is in
`create collection`'s annotations. The browser module grew 2 666 bytes,
1 266 gzip, 1 269 brotli (809.4, 333.5, 276.2 KB; the test builds without
the indexes and with no feature 2.1 KB each): `build_kind` 172 net of the
`build_index` it came out of, `Schema::decode`'s tag 316, `undo` 282,
`add_index` and its refusal 265, the plan's second index 253, the rest a
line or two each. Taken back: the plan's two indexes walked in a loop were
224 bytes more than two calls, `undo` taking every index off and putting
the text one back through `Taken` 417 more than removing the one, the
refusal naming both annotations (`IndexKind::word`) 393, and a
normalising `add_index` in `Schema::new` 91.


**A `@hash` bucket holds its ids ascending, in runs** (`engine/bucket.rs`).
A bucket was a `Vec` in the order its ids came, and a row left it by
`retain`, a walk of the whole bucket: with a field of a few values -- a
country, a device, an event's name -- every delete walked a fifth of the
collection, and the `@ttl` sweep held the write lock 370 to 500 ms a
thousand rows of an analytics site's 594 000 events, every dashboard read
waiting behind it. Now an id is found by binary search, and past 512 ids a
bucket is runs of at most 512 (`Bucket::Runs`, behind a box so a bucket
is the size of a `Vec`), so a removal moves one run's tail -- the sweep
takes the oldest rows, which one ascending list holds first and would
move all of. A run left a quarter full joins the next where both fit; a
bucket of one run is a list again, so a `@unique` value's costs what it
did. Over 10 000 000 rows with a field of five values the sweep holds the
lock 0.79 ms a thousand rows against 825 (`make ttl-bench`'s `hash`); a
lone `del` among a million rows 1.09 us against 76 to 134, a lone `put`
699 to 708 ns against 708 to 754 (`writes`). Readers take a bucket's ids
in order through one iterator (`Bucket::iter`, `Ids`), so `lookup`'s
children no longer sort them, and the maps a group's or a facet value's
number is kept in are of the index's own type (`Bucket::one`): through
`flatten` and a second map type the change was 1.3 KB brotli of the
browser module. Its buckets stay one sorted list (the runs `cfg`'d out,
a removal a `memmove` of the rest): 390 bytes, 0.4 KB brotli.

## Sharded tables

**A hash index's table grows a 256th at a time, and a text index's
terms too** (`maps::Sharded`). A `HashMap` that is full moves every key
into a table twice its size in the insert that found it full -- under the
write lock, every request waiting -- and a `@unique` field holds a key a
row: `make recon-bench`'s journal passing 1 835 008 entries held one
transfer and everything behind it 120 to 137 ms in every run, and `make
growth-bench`'s 4 million puts one at a time took 7, 19, 49, 109 and
235-267 ms at each doubling from 229 376 keys. A `@text` index's terms were
the same map, and a field whose rows bring terms of their own -- an id, a
code, a name -- doubled it as often: 9-10, 23-25, 55-59, 125-132 and
276-324 ms over the same doublings (`growth-bench`'s `text`, four new terms
a row to 4 million). Past `SPLIT_AT` (114 688 keys, a table of 2^17
buckets' worth) the map is 256 maps, each growing on its own: the longest
put is the split's, 2.5-3.6 ms for a hash index's keys and 4.9-5.3 for a
text index's terms, against 6.2-8.2 and 8.5-10.6 while the split hashed
every key again. A text index's terms are a table of their own since,
split the same way (below). Each key is kept beside its hash (`Hashed`), which its
table takes as it is (`Pass`): a lookup is one SipHash under the map's own
random keys, and the shard is bits 49 to 56 of it -- below the seven a
table tags its buckets with: by the top byte, every key of a shard had one
tag and a probe read every key it passed. Picked by an `Fx` of the key, the
shard then hashing it again with keys of its own read from the shard, a
lookup in a loop over a million keys took 94-135 ns and over four million
121-144, against 68-87 and 83-93 now, and 33-42 -> 24-34 under the split;
an index built after an open is 15-25% faster, its tables growing with no
key hashed again. The cost is 8 bytes a slot: 48 -> 56 a hash index's, 80
-> 88 a text index's then. A put's p50 is as it was (1.08-1.29 us), and so is
`match` (a new term 1.05-1.29 us against 1.09-1.37, three 1.37-1.65 against
1.41-1.72). Rejected:
one table taken into a new one 4 096 keys at a time, looked up in both
meanwhile -- lookups 163 ns, since a map that stops taking keys never
finishes its steps, and builds a third slower. The browser module keeps
the one map and its `Fx` (`cfg`), byte for byte the size it was.

## @ttl

**`@ttl` is an ordered index whose rows expire, and every read leaves
them out at once.** `IndexKind::Sorted { ttl }`, written as index kind 9
and the milliseconds behind it, so a binary from before refuses the
file rather than hand out rows past their time; a timestamp field's
alone. Every filter over the collection -- each `lookup` level's, an
inner `get`'s, a `set`'s and a `del`'s, `changes_since`'s shape -- has
`not (field <= now - ttl)` ANDed in (`Database::alive`): a `not` so the
planner never takes it for a range, and a row whose field is null, with
no time to expire from, lives. `now` is the system's clock natively and
`Database::set_clock`'s where set -- the browser module's, handed
`Date.now()` (`db.now`) as `fennec_query`'s last argument, and refused
without one rather than answered at a time it was not. A server's
`fennec_http::sweep` looks at every database it serves once a minute and
deletes what is past its time, a range of the index found under the
read lock (`Database::expired`) and 1 000 rows deleted under the write
lock as a block of ordinary deletes (`Database::sweep`, the filter
written there, since a `del` leaves expired rows out): replicas,
`/_changes`, archives and subscriptions see deletes. A replica, a
following tenant and the browser never sweep. `expired()` in a filter
reads the rows past their time (`Expr::asks_expired`, answered in
`answer_filter` as `field <= now - ttl`, a range of the index, the alive
test left out; `not expired()` is the default read), in every place a
filter goes, `set` and `del` too: a reaper's, which gives back what a
lapsed hold reserved -- `del holds where expired() and id = $1 require 1`
beside the give-back, one block, the delete the once-only guard -- where
a ledger kept holds `@sorted` and reaped by hand, since a row the sweep
deleted unseen kept its money in `held` for good. The sweeper deletes a
row a sweep's period past its time (`sweep::GRACE`, `pass_after`), so a
reaper as frequent sees every one; at the first pass after its time a row
that lapsed a moment before went unseen whatever the reaper's period. A
scoped token reaches them only by the policy's `expired` grant
(`Scope::reaping`), never through `write` or `*`; a rule's own filter
takes none. A collection without `@ttl` refuses it by name (the registry
has no `expired`, and says so). 100 000 rows past their
time out of 200 000 went in 0.56 s, the lock held 1.06 ms a batch at the
median and 2.3 at most; a read tests each row's time, so a `count` over
a million rows half past their time is a scan, 68.8 ms against 1.1 with
no expiry, and a collection with none reads as it did (`make
ttl-bench`). `create index ... @ttl(d)` gives a timestamp field one, and
`alter collection c alter field f @ttl(d) | @sorted` sets or takes it
off (field change 4, the index kept). `_idempotency`'s `at` is the first
user: its purge on the write path went. A subscriber hears a row reach its
time at that time (`sse::Expiry`): told at the sweep, a minute and its
grace later, a courier whose phone went quiet stayed on a dispatcher's map
73 s after it had gone from every read. A stream of a collection with
`@ttl` works out when its next row expires (`wakes_at` with no filter, a
step of the index, 1.3 us over 100 000 rows, again only after a change to
the collection or a look), sleeps until then if no write comes first --
`Hub::wait`'s timeout, writing no keep-alive for it -- and reads the rows
that reached their time since it last looked: `expired()` and the field's
window, the shape ANDed in, every row of the collection for an unscoped
shape that holds nothing (`FennecHttp.live`'s, told of any write to it),
sent as deletions at the change the stream stands at -- a scoped stream's
held to the ids it was sent, a quiet one's emptied -- and a resumed stream
tells every row past its time not yet swept. The sweep's delete comes
after, a deletion of what is gone. About 10 ms after the row's time in the
test, which holds the row still in the file as `expired()` once heard, and
a scoped stream silent at another user's row's time. The two features cost
the browser module 16.4 KB, 5.7 KB brotli -- 18.7 KB more while a ttl printed back
through a float, which brought the standard library's float formatting.
`expired()` cost it 561 bytes, 98 brotli.

## Index reach

**Only an `and` chain reaches an index** -- equality (`=` or `in [..]`) over a
`@hash` field or over `id`, comparisons over a `@sorted` field, the leading
parts of an index over several fields and a radius or a box over `@geo`
(both below). `in` is a set
of equalities written short, so it is answered as the union of one bucket per
element, and only when *every* element resolves to something the field's type
can express: one it cannot sends the whole list back to the scan, because a
union missing an element's rows is a wrong answer rather than a slow one. `id`
has no `@hash` and cannot have one -- `Schema::new` reserves the name -- so the
store's id index answers it directly; before that it was a full scan, 383 us
against 0.50 us over 20 000 documents. Everything else -- `!=`, `~`, `has`, a
comparison on a field without `@sorted`, anything under `or` -- is a full scan.
`~` is unranked substring matching; ranked text retrieval is `match` over an
`@text` field, which does reach one. `order` reads every match's key but puts
only `offset + limit` rows in order -- unless it is one key over a `@sorted`
field with a `limit`, which walks the index and stops at the page; with no
`order`, the scan stops at `offset + limit` matches.

## Bound filters

**A filter is bound to its collection once a query.** A scan, the
probe of a filtered `near`, an ordered walk and each `lookup` level test
rows through `Filter` (`engine.rs`): each field compared with a value by
its position, the value worked out once, and a row decoded in one pass for
every field the filter reads (`Store::read_fields`). What it cannot bind --
a field it does not know, a parameter not given, a call, two fields
compared -- stays an `eval` node over the stored row, so every error comes
where and as it came before: at the first row tested, and never over no
rows. Both take their comparison from one `query::compare`. Looked up by
name and cloned value by value, a scan of 20 000 rows with two comparisons
and an order took 1.91 ms natively and 2.88 in the browser module; bound,
1.07 and 1.70, and a count over an `in` and an `or` 2.24 -> 0.90 ms. The
positions are kept sorted as they come: sorted after, `usize` was a sort of
its own, 3 KB of the browser module. A row's text lands in the text its
slot held (`Store::read_fields`), a malloc and a free a row less: a count
by text equality 1.49 -> 0.96 ms natively, 1.86 -> 1.42 in the browser;
resized rather than truncated, the row cost numeric filters 7 to 13%.
Reading an `order`'s keys with the filter's own pass, rather than finding
each matched row again (`order_ids`), took 6 to 9% off an ordered scan
natively but made the unordered ones 4 to 6% slower in the browser and the
module 1.9 KB larger, so it is not in.

**The rows an equality's bucket gave are tested without it**
(`Database::candidates_held`, `Filter::without`). The scan's path
(`matching_ids_to`: a `get`, its `count` and page, a `set`, a `del`) tested
the rows a `@hash` bucket or the id index gave with the whole filter, the
equality that chose them among its terms, though every one holds it: the
bucket is that equality's whole answer, as `is_bare_equality` already takes
it to be where the filter is that term alone. Natively the filter is now
bound without it, the other terms in their order, so each is asked of the
rows it was and fails where it failed; a union of buckets (`in`), a range, a
box and an index over several fields are no one term's answer and leave the
filter whole. A scoped token's rules made it a cost: fennec-http asks them
before the statement's own terms (access.md, *The rules are asked before
the caller's terms*), so `owner = $jwt.sub`, whose bucket is a scoped read's
usual rows, was asked again of each. Over 100 000 rows, five owners and the
rule's bucket of 20 000, main's engine with the rule last against this one
with it first (medians of three runs in turns, each of 15 rounds, load under
2 on 8 cores, `Database::query` alone): a count by a term matching one row
711 -> 595 us, where the rule asked first without it took 924; one matching
half 837 -> 617; `like` 1 229 -> 1 066; a page of 20 711 -> 602; where the
statement's own equality gave the rows 8.6 -> 7.1 us, its page 8.8 -> 7.3;
an `in` 20.7 -> 18.6, the rule refusing four rows in five before it; a row
by id the same, 4.84 -> 4.78. Most of it is the field no longer read: a
term left out that no other reads takes its field out of
`Store::read_fields`' pass. The browser module, which holds no token's
rules, binds the filter whole as before: 33 bytes more of it (852 824 ->
852 857, brotli 70), inside the layout's own noise.

A stream's look tests the rows written since its cursor through a bound
filter too, where the filter reads another collection and holds a long
`in` list (`Database::changes_held`): an inner `get` answered as 100 000
titles, compared a value at a time by `eval`, made a look over 1 000
changed rows no title named take 3.1 s. Bound once a look, each long
list's set is kept for the next (`Lists`, by the ordinal of its `in`
among the long ones and with the schema it was bound against): the
positions of its values in the filter's own list, by their keys' hashes
(`Positions`, 1.3 MB for 100 000 titles), so a write's look looks its row
up rather than copying and hashing the list again
([fennecql.md](fennecql.md#inner-gets)). Any other stream's filter is
evaluated a row at a time as before (`changes_since`), a long list
written in too, so a plain subscription's look runs what it ran: it
tests the one or two rows a write made.

## @sorted

**`@sorted` must give the scan's answer, row for row.** Its keys order exactly
as `Value::cmp_value` orders the field's values (ints and timestamps through a
sign-bit flip, floats through order-preserving bits with `-0.0` folded onto
`0.0` and every `NaN` onto the last key, past `+inf`'s -- below), and `null`
is held apart below every key. A literal the
key space cannot express exactly (`12.5` against an `int`) is left out of the
range and evaluated per row; an int no float holds against a `float` or a json
path (2^53 + 1) lies between two keys with none between them, and is said by
them (`SortedIndex::beside`, below). One of
another kind than a typed field's, which `cmp_value` orders the field's values
against by kind alone (`level > true`, `level < ""`, `t >= ""` where `""` reads
as no time), is true of every value or of none (`SortedIndex::by_kind`), so it
is no bound or an empty range (`Range::close`) and the range stays exact,
natively -- the browser module, where no scoped rule writes one, evaluates it
row by row as before, 611 bytes smaller: the
edges a scoped token's ordered rule is held to its claim's kind by (`level <= 5
and level > true`, access.md) were evaluated row by row, and a count over 20
001 of 200 000 rows in process went 44 us -> 790 us, +0.35 us with the verdict,
what any second comparison costs (`level > -1` the same; `make policy-bench
ARGS=ranges`). Its unit test holds each verdict to `compare` over values of
every kind, and `tests/sorted.rs` has filters of another kind's literals. The
edge such a rule writes below a text claim that reads as a time is `x >= ""
or x >= "0000-01-01"` (a time and an int order alike against every value
but that text, so no edge ANDed in takes every text and every time), which
`answer_filter` folds to the side the field's declared type reads
(`fold_kinds`, natively, asked only where `kind_splits` finds one): the
second for a `timestamp` field, the first for any other -- the same rows,
the `or`'s every row, and a comparison the range says as a key, where the
`or` left a count over the range to test every row of it. Ties come
out in ascending id both ways, since that is the order the scan leaves them in
-- a descending walk reverses each run of equal keys. A walk tests the rest of
the filter row by row and gives up past an eighth of the collection, so a filter
that matches almost nothing costs 1.25x the scan rather than 2x in random reads.
The structure is a sorted `Vec` of chunks of at most 256 entries, not a
`BTreeSet`, which made the browser module 75 KB larger, each chunk found among
`firsts`, the chunks' first entries side by side: searched over the chunks'
own, each probe was a cache miss into a buffer of its own, and a lone put
beside an ordered index over a random int took 1 324 ns, 1 098 now (512 to 256
moved what a write shifts; the reads did not move). `tests/sorted.rs` checks
every filter, order and page against a twin collection without the index. It is
derived data like the hash and text indexes: built when a statement first reads
it after an open (below), never in the file.

**Numbers of two kinds compare by their true values** (`Value::cmp_value`,
`value::tie`), and every index's keys with them. Met as two `f64`s alone, as
they were, the ints 2^60 and 2^60 + 1 each equalled the float 2^60 and not
each other: `n = 1152921504606846976.0` found one row through `@hash` and
`@unique`, which key the int exactly, and through the scan and `@sorted`
every one rounding to it, from 2^60 - 64 to 2^60 + 128; a sort over such
values had no order to keep, and the float 2^63 equalled `i64::MAX`. Rounding to the
nearest float is monotonic, so where two numbers' `f64`s differ they order
the numbers; where they are equal short of 2^53, where every int is a float,
the numbers are equal; and past it a tie of two kinds is told apart as two
ints, 2^63 past every int (`tie`, out of line). A timestamp is its
milliseconds' int, against an int too. So a whole float short of 2^63 either
side is the one int it is, and a key says only that: `Value::key_as` hands a
`@hash` lookup or a `lookup`'s probe the number itself or nothing (-2^63
finds `i64::MIN`, 2^63 nothing, 2^53 + 1 no `float`; a `NaN` its own key,
below), `hash_key` files a whole float under its int up to
2^63 (a path's `@unique` refuses 2^60.0 beside 2^60), `SortedIndex::bound`
and `CompositeIndex::literal` key an int against a `float` only where a
float holds it, and a json index keys every int a float holds as that
float, holding apart only those none holds. The indexes are derived, so no
file changes. Two floats have an arm of their own, and the mixed kinds go
through the last arm, as they did, the tie looked at again there: arms of
their own for the mixed kinds lengthened every comparison's dispatch, an
order of 100 000 rows by a float 2.0% more instructions and by an int 0.6%.
This way, against the build before (in-loop instructions, `proc_pid_rusage`,
seven alternating runs and fifteen more for the orders, the 1-minute load
under 6): an order by a float 1.5% fewer, by an int 0.6% fewer, a filter
scan by an int 0.2% fewer with an int parameter and 0.3% more with a float
one -- the mixed comparison -- a float field's scan 0.5% fewer with a float
parameter and 0.4% more with an int one, an `@sorted` range the same; the
orders' paired cycles moved +0.2%, inside their runs' spread. The browser
module grew 330 bytes, 141 gzip and 55 brotli at `-q 11` -- inside the 0.3
KB brotli moves by with the layout alone -- `tie` 317 of them. `tests/mixed_numbers.rs`
holds every index, `lookup`, `in`, `order`, `min`, `max`, `having` and a
json `@unique` to the scan near 2^53 to 2^63, random ints and literals among
them, and `value::tests` the comparison to one worked out in `i128`.

An int no float holds -- 2^53 + 1 -- has no key in a float's key space, so
the first version of this rule left `xs > 9007199254740993` to be tested row
by row over every row, where `as` had made it 2^53's key before. It lies
between two adjacent floats (`value::floats_beside`), and every comparison
with it is said by one of them as strict or inclusive as it was -- `>` and
`<=` by the one below, `>=` and `<` by the one above, `=` by both, a range
of no key (`Range::narrow_beside`) -- so a `NaN` row, past both, is taken or
left as the literal would take it. A float field's `@sorted`, a
json path's (whose numbers are keyed as floats) and an index over several
fields with a float or json part (`CompositeIndex::beside`, through its
`literal` of each float) answer it so, natively: the browser module tests
such a comparison row by row, as it did, as it does a literal of another
kind (`by_kind`) -- with the bounds it grew 960 bytes more. A facet's
ranges by such an int are counted row by row, as they were: the key above
it would have answered them, for 160 bytes of the module no one asked for.
**A `NaN` equals a `NaN` and is above every other number, `+inf`
included** -- PostgreSQL's order, what a user of a general database
expects. `cmp_value` found a `NaN` equal to every value, the one comparison
left that was no order after the true values above: `NaN = 3`, `NaN = 5` and
`3 < 5` at once. A sort over a float field holding one could panic in the
standard library (Rust 1.81 on: "user-provided comparison function does not
correctly implement a total order"), or put rows in no order; a float
field's `@hash` filed a `NaN` under its bits, where `xh = 5.0` did not look,
and answered 3 rows where the scan took 432; and since #511 a `lookup` by a
`NaN` key found no child, `key_as` making it no key. A `NaN` reaches a
field by the Rust API's values and parameters, the native library's vector
bytes (a `vector<N>` or a `[float]` it fills), a PostgreSQL import's
`'NaN'::float8` (text `COPY` and `--follow`) into a `float` or a `[float]`
-- never a vector field, where `load::adapt` refuses a non-finite
component of a list as of a vector, its row named -- a SQLite text `'NaN'`
read into a float column (`load::adapt` reads a text through
`parse::<f64>()`, which takes `nan`, `inf` and `infinity` in any case;
SQLite stores a `NaN` itself as `NULL`, so no `REAL` holds one), a
`vector` function over an infinity a text reader made of `1e39`
(`put c {x: dot([1e39], [0])}`, `set v = normalize(v)`); JSON cannot carry
one, and writes one as `null`, as it does an infinity. Two floats compare through `value::floats`, which asks the `NaN`
question only where `partial_cmp` finds no order, a mixed pair's last arm
the same, and a vector's or a sparse vector's `f32`s as the `f64`s they
widen to. Every key files every `NaN` as one, whatever its sign or payload
-- `0.0 / 0.0` is `-NaN` on x86, a signalling one reads as written: an
ordered index's and a composite part's `float_key` is `u64::MAX`, past
`+inf`'s (a positive `NaN`'s bits already ordered there, a negative one's
below `-inf`), and `hash_key` and the keys a group, a distinct count, a
facet and a sketch count by take `f64::NAN`'s bytes (`codec::key_number`
now, `-0.0` keyed as the int 0, below) -- inside a list, an object or a vector too
(`codec::encode`'s key copy, the `f32`s as `f32::NAN`'s, which `hash_key`
reaches through `encode_key`): filed by their floats' bits, a
`[float]` field's `@hash` answered `xs = [NaN]` with one of the two rows
the scan took, and `[0.0]` with one against `[-0.0]`, though its plan said
"which is the answer"; a group, a distinct count and a sketch split `[NaN]`
from `[-NaN]` and `[0.0]` from `[-0.0]`; and `@unique` held both. Every
index follows that key, since a rewrite leaves the entries of a value whose
hash key it keeps (`same`): an index over several fields keys a json part apart
so too, where by its bits a `[-0.0]` rewritten as `[0.0]` kept its entry,
the delete after it looked for the other, and the entry left refused a
later row as a duplicate of the row deleted
(`a_zero_rewritten_in_a_list_leaves_no_entry_behind`). So `Value::key_as`
takes a `NaN` as a `float`'s key and no `int`'s, `@unique` holds one
`NaN`, `max` is a `NaN` where one is held and `min` the least number,
`x > 5` takes the `NaN` rows as PostgreSQL's does -- through the ordered
index as a range of keys -- and an ordered index holding a `NaN` is walked
for `order`, where it was passed
over: its `nans` set, `Range::strict`, which decided whether they matched,
and `SortedIndex::has_nan` are gone, the browser module the smaller for it
(below). A facet's ranges count a `NaN` in none: a range ends short of its
bound, and none is past a `NaN`. Rejected: SQLite's way, turning a `NaN`
into `null` as it is written, loses the value a write was given, and every
reader of a file a `NaN` is in would need it; IEEE 754's `totalOrder`
(`f64::total_cmp`) puts `-NaN` below `-inf` and `-0.0` below `0.0`, which
`=` holds equal; refusing a `NaN` refuses PostgreSQL's data on import. A
router merging tenants' rows reads them from JSON, where a `NaN` is `null`
and orders first, so a tenant whose ordered rows end in one is named out of
order, never merged wrong -- as one ending in `+inf` was. A held write's
rank by an expression (`waits::rank`) leaves a non-finite key to the
arrival's order, as before. The `NaN` test is out of line and cold
(`value::nan_order`): inlined, it was folded into the `>` path, and an
order of 100 000 rows by a float took 1.8% more instructions. This way,
against the build before (`make instr-compare PKG=fennec-core
EXAMPLE=orders`, each build under 8 UUIDs, 7 rounds, the 1-minute load at
most 4.2): an order by an int +0.01%, by a float -0.54%, a float field's
scan -0.19%, a group by a float +0.03% -- the key's `NaN` test, under one
instruction a row. The browser module was 1 399 bytes smaller, 507 gzip and
274 brotli (inside the 0.3 KB brotli moves by), 1 081 of them the ordered
index's. The review's round folded a list's floats and named a `NaN` in a
refusal, and `codec::encode` became a copy for a record and one for a key
(`KEY`) -- a second copy kept for its speed, as the vector index's stable
sort is ([one copy of a
generic](browser-module.md#one-copy-of-a-generic)): a record is written by
the code it was, and a key of an int or a text reaches its arm as it did.
One copy cost every key, each measured
against the branch before: the kind tested ahead of the float's took a
group by a float 0.22% more instructions, after it a group by an int
0.30%, and a flag handed down 0.14% to the int's group and two registers
saved to every value a record writes. Against main, each build under 8
UUIDs, 7 rounds, every run started under a 1-minute load of 4 and ending
at most at 4.47 (`MODES=...,group-int,group-list,hash-list`): an order by
an int +0.01%, by a float -0.51%, the scan -0.19%, a group by a float
+0.04% and by an int 0.00% -- the branch before's each -- a group by a
`[float]` pair +0.17%, the fold of two floats a row, and 1 000 lookups in
its `@hash` +0.21%, inside 2 SE. The key's copy is 640 bytes of the
module, `key_f32s` 99 and `json::named` 257: against the branch before
+1 006 bytes, 343 gzip and 342 brotli, against main -393, -164 and +68
(noise). The test build without the indexes is 898 bytes larger than
main's, brotli inside its noise. `tests/nan.rs` holds every index -- `@hash`,
`@sorted`, an index over several fields either way, a json path's, an
int's and a timestamp's against a `NaN` literal -- to the scan and the
scan to PostgreSQL's order over random edges (`NaN`s of four bit patterns,
the infinities, both zeros, floats from 2^53 to 2^63), `between` and `in`
lists too, every `order` path, a page and a descending one, to that order
(`an_order_over_nan_is_a_total_order`; on main its sort put the rows in no
order), and groups, counts, sketches, facets, `max`, `having`, `greatest`,
`@unique` and `lookup` to one `NaN`, a list's and a vector's `NaN`s and
zeros through a `[float]` field's `@hash`, `group`, `count(distinct)`, the
sketches and `@unique` to one value
(`a_list_or_a_vector_holds_one_nan_and_one_zero`), and a refusal to name
a `NaN` as itself (`a_unique_refusal_names_a_nan_as_it_is`);
`value::tests` holds `cmp_value` antisymmetric and transitive over every
triple of 67 edge values of every kind (98 since, below).

*What a file written before meets.* No record changes -- every key is
derived, built at the first read -- but three answers move with the order,
and fennecdb is alpha, so they are said here, not migrated. A `@unique`
field holding a `NaN` and a `-NaN` side by side, or a `[float]` one
`[NaN]` beside `[-NaN]` or `[0.0]` beside `[-0.0]`, holds one value twice
now: every `set` of either row is refused `Duplicate` -- `unique_holders`
asks every `@unique` value of the document the `set` leaves, not only the
fields it changes -- until one of them is changed to another value or
deleted. A sketch `hll_accumulate` stored before hashed a `NaN` of other
bits by those bits, and a list's or a vector's floats unfolded: combined
with one made after, an approximate count may be one more. And a scoped
token's rule compares by the order too: `x <= $jwt.max` admitted a `NaN`
row, equal to every claim, and admits none now, where `x > $jwt.min`
admits every one (`http-access.html` says so).

**Values of two kinds order by kind, but for the numbers, and a text and
bytes; a comparison reads a text beside a time as the time it names**
(`Value::type_rank`, `Value::cmp_read`): `null`, a bool, a number -- an
int, a float and a time, its milliseconds' int, by their true values
(above) -- a text or bytes by their bytes, a vector, a list, a sparse
vector, an object. The last cross-kind equality that was no order went
with #520's review: `cmp_value` read a text beside a time as the time it
named, so `Timestamp(0)` equalled `"1970-01-01T00:00:00Z"`, which the int
0, equal to the time, ranked below as a text. No field holds the three --
a json field never holds a time, `json_value` writing it as ISO text --
but an expression does: `order coalesce(t, j)`, `t` a time field and `j` a
json one holding ints and ISO texts, panicked in the standard library's
sort (`smallsort.rs`, its total-order check) in 3 of 117 runs of 1 191 to
2 530 rows, and otherwise put the rows in no order. A server's connection
unwinds such a panic; the shell, `panic = abort`, died.

Now the order and the comparison are two. The order -- `cmp_value`, which
`order`, `min`, `max`, a sort's keys and every key a group, a distinct
count, a facet or an index files take -- puts a time below every text, as
any two kinds. A comparison -- `=`, `<`, `in`, `has`, a filter's, a
`case`'s, a `having`'s, an aggregate's `filter`, an access rule's, and
`greatest` and `least`'s choice -- reads a text beside a time as the time
it names, whichever side
either came from, as main's `cmp_value` did: `j.at > now() - 1d` over a
json field's ISO times compares two times. It is `cmp_read`, a copy of
`cmp_value`'s body (`cmp_kinds::<READ>`) whose arms where a time meets a
text parse the text, so no comparison of two other kinds pays for it; a
comparison needs no order, its values met two at a time and never sorted.

`greatest` and `least` choose as a comparison does (`plugin::extreme`):
they pick one of a row's values and sort none, and a statement keeps a
row's latest time with `set q {at: greatest(at, $2)}`, `$2` an ISO text --
`examples/analytics`'s quotes. Chosen by kind, as #529's first build did,
the text always won: `at` went back when a batch came out of order, and
the next batch's `case when at <= $2` took a stale price (CI's `examples
(analytics)`, `kinds.rs`'s `greatest_and_least_read_a_text_beside_a_time`).
Over a time, its ISO text and the int equal to it the choice follows the
arguments' order, as main's did; no sort meets it.

The first try read only a text *written* beside a time -- a literal, a
parameter -- as PostgreSQL types a literal of no type by what it meets,
and left a text read off a row beside a time a text. #529's review found
the answers it moved wrong and silent, through the scan and every index
alike: over a json field whose `at` held ISO times, `j.at > now() - 1d`
held of a week's rows too (`now() - 1d` is folded to a time), `j.at <
now() - 1d` of none, `j.at = $1` and `t = j.at` of none. Nor could `eval`
tell a statement's value from a row's: `aggregate.rs`'s `lift` makes a
group's fields parameters, a binding's `new.x` is one, so `where t = j.at`
answered none where `select t = j.at` answered true, and `count(*) filter
(where j.at > $1)` 2 where `filter (where $1 < j.at)` answered 1.

A `timestamp` field still has a text written beside it read once a query
(`Filter::bind`'s `as_field`: a `timestamp` field, an item of a
`[timestamp]` one), as every index reads it (`Value::key_as`,
`SortedIndex::bound`, `CompositeIndex::literal`): read at every row, as
main read it, a scan of 100 000 times against a text parameter took
172.5 M instructions, 45.1 M so. A text naming no time stays a text, above
every time, as `SortedIndex::by_kind` ranks it. An index answers a time
beside a text as the scan does or leaves it to the scan: a text field's
`@hash` takes no key for a time (`key_as`), whose ISO text found one
spelling where the scan, reading `2026-01-01 00:00:00+00` and `2026-01-01`
as the time too, found every one -- main's own mismatch, which a `lookup`
never meets, `check_key_types` refusing a time key beside a text one; a
json `@hash` and `@sorted` keep no key for a time (`lookup_key`,
`SortedIndex::bound`), nor an index over several fields for a text or
json part (`CompositeIndex::literal`).

What users meet: no answer of a comparison, `greatest` or `least` moved
from main. An `order`
and a `group` over an expression mixing a time with a text go by kind --
`order coalesce(t, j.at)` puts every time before every text, `group` keeps
a time and its ISO text apart -- and `order timestamp(j.at)` orders ISO
texts as the times they name, where every one names one. A keyset page
over such an expression (`where coalesce(t, j.at) > $last order
coalesce(t, j.at)`) reads the texts as times the order put after every
time, skipping or repeating rows: page by `timestamp(...)`. Rejected:
reading in `cmp_value` a text that names a time as that time against
every kind makes it a number -- `"2026-01-01"` below `"10"` and `"abc"`
above, two texts no longer in their bytes' order -- and breaks a text
field's `@sorted`; refusing a time beside a text, as PostgreSQL's
operator lookup does, would refuse every filter over a json field holding
both, which `where j > 5` reads now; a comparison by kind, the literal
read as above or not, the first try's wrong answers.

**A key is equal exactly where `cmp_value` is** (`codec::encode_key`), so
a group, a distinct count, `approx_count_distinct`, a facet, a sketch, a
`@hash` bucket, a `@unique` value and an index over several fields part
only the values the order does -- `=` too, but for a time beside the
text naming it, which no key files together and `lookup_key` leaves to
the scan: a whole float short of 2^63 either side,
`-0.0` among them, and a time are keyed as the int they are
(`key_number`, through `value::exact_int`), bytes that are UTF-8 as that
text, every `NaN` as one, in a list or an object too (the key copy of
`codec::encode`). #511 gave `hash_key` the whole float's fold alone, so a
json field's `3` and `3.0` were one bucket but two groups, two distinct
values, two facet values and two sketched, and `[3]` beside `[3.0]` two
`@unique` values -- `=` finds each pair equal. `hash_key` is `encode_key`
now, one copy of the fold. A key is never read back but where an index
names a value off it: a facet over a `@hash` time field reads its keys'
ints back as times (`Database::facet`), and `create index ... @unique`
names the value as the first holder wrote it, not off the key.
`exact_int` is a cast there and back (`f as i64 as f64 == f`, 2^63 held
out) where it asked `fract`, a `trunc` -- libm's call on x86-64 short of
SSE4.1 -- and a range both ways; the whole int's key is out of line, and
a float's tag and bytes go in one write, so a group by a float field pays
nothing for the test. `value::tests` holds two
keys equal exactly where `cmp_value` is over every pair of the edge values
and every int against every float, `cmp_value` antisymmetric and
transitive over every triple of 98 of them, times about a time's
milliseconds and texts reading as times among them, and `cmp_read`
antisymmetric over every pair, `cmp_value`'s answer but where a time
meets a text (`a_comparison_reads_a_text_beside_a_time`); `tests/kinds.rs`
holds the order over `coalesce(t, j)` to one worked out apart in 60 runs
(on main the sort panicked, or put the rows in no order); a text beside a
time to the time through the scan, a `@hash`, a `@sorted`, an index over
several fields and an expression, `in`, `has`, a long list, a parameter
either side and a json field's ISO text; #529's review's answers over a
json path, with no index, a `@sorted` and a `@hash` on it
(`a_json_time_compares_as_the_time_through_every_index`, main's answers);
one answer from a comparison in `where`, a select list, an aggregate's
`filter` and `having`, either way round
(`a_comparison_answers_alike_wherever_it_stands`); a text field's
spellings of a time through the scan, a `@hash`, a `@sorted` and an index
over several fields (`a_text_field_meets_a_time_alike_through_every_index`,
which main's `@hash` failed); and one group, one distinct value, one
sketch, one facet value and one `@unique` value for `3` and `3.0`, `[3]`
and `[3.0]`, a time and its int, bytes and their text. Against the build
before (`make instr-compare PKG=fennec-core EXAMPLE=orders`, the base the
example's new modes on main, 8 identities a build, 7 rounds, every run
started under a 1-minute load of 4): an order by an int, a float, a time
or a text, a float field's scan, an expression's comparison, a group by a
float or an int and 1 000 lookups of `[float]` pairs in a `@hash`, each
inside 2 SE of 0.00%; a group by a `[float]` pair +0.05%, the
whole-float test of its two floats; a time field's scan by a text
parameter -73.7%, a group by a json number of which a third are whole
floats -15.3%, a group fewer for each. The comparison and the order are
two copies of one body (`cmp_kinds::<READ>`): one copy taking a flag, asked
only where a time meets a text, kept the flag in a register through the
recursion and passed it at every call, and every order took 2.2% more,
a float field's scan 0.8%; a `cmp_read` of the reading arms alone, the
rest sent on to `cmp_value` and `query::compare` sending it those kinds
alone, was 682 bytes of the browser module smaller and took the float
field's scan 1.9% more, the kinds asked before every comparison. The time
and text arms stay in the order's copy, returning the kinds' order: left
to its last arm, the dispatch laid out anew took an order by an int 0.5%
more. The browser module grew 1 676 bytes against main, 379 gzip and 382
brotli at `-q 11`, the second copy 1.1 KB of them; the test build without
the indexes 1 676, 428 and 367. *What a file written before meets*: no record
changes; a `@unique` json field or index over several fields holding
`[3]` beside `[3.0]` holds one value twice now, every `set` of either row
refused until one changes; a sketch `hll_accumulate` stored before hashed
a whole float, a time and UTF-8 bytes by their own kinds, so combined with
one made after an approximate count may count such a value twice.

**A value read back through JSON is the value it was.** `json::num_into`
writes a whole float short of 2^63 either side as every digit of the int it
is, which reads back as that int -- equal to the float -- and JavaScript as
the same double; it wrote the shortest digits from 1e15 up, and 2^60 went
out as `1152921504606847000`, read back as that int: `get` printed digits a
statement or a parameter could not find the row by, and the router, which
reads its tenants' rows back from their JSON and checks their order with
`cmp_value` (`fanout::value_of`), named a tenant holding the float 2^60 and
the int 2^60 + 1 out of order. From 2^63 on the shortest digits are past an
`i64`, so a reader takes them back as the float itself
(`a_whole_float_reads_back_as_itself`). The shell prints such a float so
too. The clients follow: JavaScript's `JSON.stringify` and Go's
`encoding/json` write a whole number from 2^53 on as its shortest digits.
What a server is sent goes through `web/builder.js`'s `jsonOf`
(`FennecHttp`'s bodies, the sync layer's batch lines) and Go's `exactly`,
which write every digit of it instead. `JSON.stringify` cannot write a
number's text from a replacer, so `jsonOf` marks such a number
(`\u0000fennec-int:` and its digits, as a text) and replaces the mark, only
where the value holds one: a request's parameters, every `FennecHttp`
body's, are noted as they are normalized (`bodyOf`, through `counted`, its
own copy of `normalize`'s walk), and a sync layer's line is told by the
replica (below); a line no one told is walked, after a first
`JSON.stringify` has refused a cycle the walk would follow. A text or
a key that already starts with the mark -- written by no one but on purpose
-- leaves the text as `JSON.stringify` writes it, the number with its
shortest digits. The module's parameters go by `JSON.stringify` as they always did: a parameter
there is a JavaScript value, and no JavaScript value is an int no double
holds, so `fennec-wasm` reads such an int as the double it rounds to, the
int that double is (`as_double`), and only for a text whose reading met an
int past 2^53 (`json::wide`). Measured in Node over the module, a `run`
putting 1 000 documents took a look at the page's values first 2.3% longer
(a walk of them), a look for 16 digits in a row in its text 30% longer
(every long fraction, `i / 3`, a match, and the text a quarter of
`JSON.stringify`'s time to scan where none was), and `as_double` over
every parameter 1.5% longer. Counted by the reader, against main
(whole-process instructions, `/usr/bin/time -l`, seven alternating runs,
the 1-minute load under 3): that `run` 40.42M -> 40.44M instructions a put
(+0.06%), its cycles +0.3% (paired -0.4% to +1.2%, as much as without the
reader's count) and its time -1.0% (inside its runs' 2%), the builder's
insert of the documents 72.92M -> 72.95M (+0.03%), a small `get` 780 482
-> 780 548. A note kept by `normalize` itself had cost that insert 0.2%, so
the builder's own parameters are not noted; a request's body of 1 000
documents takes 1% longer to make (`normalizeAll` then, a `WeakMap` telling
`jsonOf` what it had found), where a walk of it after had taken 8.7%. The
client ships as source, so a comment is bytes every page loads: these
reasons are here, and `web/builder.js` keeps a line or three a function.
`bodyOf` hands `jsonOf` its note as an argument instead of the `WeakMap`.
Counted so, a call less a run doing nothing (seven alternating runs, the
load under 3, one run over it discarded), against main: that body 13.68M
-> 13.60M instructions (-0.6%, inside its runs' spread), a sync line of the
same documents 2.21M -> 3.32M (+50%: the walk), the module's `run`, insert
and `get` within 0.4%. A note kept by `normalize` itself, measured again,
cost the insert 0.2% and the body 1.9%; `holdsWide` by `some` cost a line
12% more and by `Object.values` 87%. The client grew 2 633 bytes, 903
gzip, 798 brotli (`web/fennec.js` and its modules, as served), and
`@fennecdb/web/client` in an app's bundle (esbuild, minified) 902, 297 and
265; with these reasons in its comments it had grown 5 074, 1 694 and
1 408, and 1 074, 359 and 339. `as_double` is 159 bytes of the module, which with
it grew 649 bytes in all, 336 gzip, 305 brotli; it leaves an int an int --
changing the kind cost 126 bytes of drop glue -- as no text
`JSON.stringify` writes is an int rounding to 2^63. Natively, by `make
instr-compare` (eight identities a build, seven runs each, the load under
4), `query_json`'s `get` of 1 000 rows of four float columns, half the rows
whole and half with a fraction (`examples/json_numbers.rs`): whole floats
in [1e15, 1e16) 3.820M -> 3.488M instructions (-8.7%), in [2^53, 9e18)
4.046M -> 3.592M (-11.2%), under a million -3.3%, none whole -0.5%, int
columns -14.7%. `int_into` writes its digits and sign with one `push_str`,
as `num::f64_into` does; pushed a `char` at a time, as it had been, the
first two were +2.6% and +1.0% against main. A plain `/query` put
(`replays`): +75 and +22 instructions (22 017 -> 22 092, 22 060 -> 22 081)
in two sets, the second inside two standard errors, and one traced call of
`handle` 4 instructions more of its own code (`Collection::index_scalar`
and `Value::coerce_as` 2 each), the rest the allocator's draw. Python,
.NET, Swift, Kotlin and Dart write such a double with a point or an
exponent, which the server reads as the float it is; a Go struct document goes through `encoding/json` whole,
so there such a number belongs in an `int64`, as the languages page says.
`tests/mixed_numbers.rs` (`a_number_read_back_through_json_finds_its_row`)
reads every kind of field's numbers back from the JSON a server answers and
finds each row by them, the router's `a_json_column_of_large_numbers_merges_in_their_true_order`
merges two tenants' engine-written rows, and `web/fennec.test.js`,
`web/fennec.sync.test.js` and Go's `TestAWholeFloat64From2To53FindsItsRows`
find 2^60's rows by the number each language holds.

**A sync line is told by the replica's run of its write.** `FennecSync`
runs a page's write on its replica before it queues the lines, and the
module's reader counts every int past 2^53 it reads (`json::wide`, which
`as_double` already reads): `write` reads the count (`fennec_wide`) before
and after `#apply`, and a write whose path marks it `ran` -- an insert or
an upsert the replica made under its ids, an update, a delete -- hands
`jsonOf` whether it moved, so its lines are never walked. Every value such
a line sends was read by that run: the replica's documents are the line's
with ids put in (temporary ones from 2^52, below the count's 2^53, or ids
the replica holds), an update's or a delete's statement is the line, and
the second line naming rows by their key sends the same patch and the
keys as text. A list of numbers the page's run sent apart as `f32`s
(`vectorsApart`) is not read as JSON, but the server reads it as `f32`s
too unless the statement needs it exact (`exactly`), and there the module
asks for it as JSON first (`res.exact`) and reads it: the count moves
where the server would read the digits. A false move -- an id the replica
holds past 2^53 -- costs the slow path, no more. An insert into a shape
with no key whose documents name no id, and an upsert with neither, are
sent unrun and walked; a module from before the export tells nothing and
every line is walked. When told one is there, `jsonOf` skips its first
`JSON.stringify` and writes the marked text alone. Counted as the table
above (a call less a run doing nothing, whole-process instructions, seven
alternating runs, the 1-minute load under 2.5), a line putting 1 000
documents of two texts, two floats with fractions and a small int: main
1 143 556, walked 1 351 164 (+18.2%), told 1 144 686 (+0.1%, inside the
runs' spread of 0.6% to 1.1%; the two reads of the count included). Every document
holding an int past 2^53: walked 11.01M, told 9.60M (-12.8%; main's
1.44M wrote the wrong digits). `bodyOf` with them 18.42M -> 16.52M
(-10.3%), without them +0.03%; the module's `run`, insert and small `get`
within 0.2%. The narrower look at the text, a whole number of 16 digits
or more neither after a `.` or a digit nor before a `.`, `e` or a digit
(`/(?<![.\d])\d{16,}(?![.\deE])/`), matches no fraction, but scanning the
117 KB line cost 4.17M against `JSON.stringify`'s 1.14M (+265%), its scan
looking at every character. The export is 23 bytes of the
module, 19 gzip, 44 brotli; the client 452 bytes, 139 gzip, 101 brotli.

## Composite indexes

**An index over several fields is its parts' bytes laid end to end**
(`composite.rs`, `Schema::composites`). `@sorted(owner, created desc)` among
a `create collection`'s fields or `create index on posts (owner, created
desc) @sorted`, `@unique(org, slug)` the same refusing a second holder of
every part, `drop index on posts (owner, created)` by its fields; 2 to 8
parts of bool, int, float, timestamp, text or json, a path among them. The
schema writes them one nameless place past its last behind type tag 16
(`TAG_COMPOSITES`), which a binary from before refuses. A part's value is
encoded so its bytes order as `cmp_value` orders the values -- an int's sign
bit flipped, a float's bits made to order with `-0.0` folded, a bool a tag,
text escaped (`00` as `00 FF`) and ended (`00 01`), a json number as its
`f64` below json text -- null a tag below every value, a descending part's
bytes inverted; a collated text is kept as written behind a tag naming the
collation, and an index holding one compares part by part (`cmp_keys`),
the rest as bytes. A float's `NaN` is its part's last key, every `NaN` one
(`float_key`, [above](#sorted)). A json bool, list, object or int no
`f64` holds exactly (2^53 + 1; one a float holds is keyed as that float,
which `cmp_value` finds it equal to) are apart: their codec bytes are their
part, a list's and an object's floats folded as a hash key folds them
(`codec::encode_key`; a unique index finds them by them), the rows are
listed, and while one is no walk answers and a
filter's run takes them as candidates to test. Entries are `(KeyBytes,
DocId)` in the ordered index's `Chunked`, a key of up to 22 bytes held in the
32-byte entry (an int and a time are 18), longer on the heap, two held ones
compared as a `u128` and a `u64` big-endian and then by length: each boxed,
a million keys counted 48 MB and a put cost 2.0 us; `memcmp` a comparison
was 13% of an insert. Natively the build sorts the keys once (1 015 -> 148
ms over a million); the browser module inserts them, a sort of these entries
being code of its own there. The planner (`composed`) fixes the parts the
`and` chain's equalities name, in order -- an `in` whose every element it
can say makes a span each, `MAX_SPANS` (256) at most together -- narrows the
part after them by its comparisons, and calls the plan exact when it took
every term and nothing is apart; any term it did not take is tested row by
row, so a literal it cannot say (`null`, `12.5` against an int) changes
nothing -- but one of another kind than a typed part's is true of every
value or of none (`SortedIndex::by_kind` over `CompositeIndex::part_type`,
natively, as `@sorted` takes it): true, it is taken where the part is fixed
or ranged, whose values are of its kind, and false leaves no span. Left to
be tested, the edge a scoped token's rule holds a claim to its kind by
(`level > true` beside `team = $jwt.team and level <= $jwt.c`) read every
row of the span a count was answered from: 285 us over 5 000 of 200 000
rows against the server token's 0.85, 1.3 with the verdict (three runs in
turns against the build before; `make policy-bench ARGS=ranges` has the
case). `filter_candidates` takes its spans by exact count, two searches
each, under a range's rule; `walk_composite` an `order` on the parts after
the fixed ones in their directions or every one reversed (an `in`'s part is
no fixed one for an order), the spans merged by the order's parts
(`CompositeIndex::walk`), ties gathered and put in id order, a walk testing
the filter only while its run is no longer than `indexed_size`'s smallest
set and giving up past an eighth of the collection; `count_composed` counts
an exact plan off the chunks. A read names it only by its first part
(`names_first`), so one it cannot help neither builds it nor, pinned, meets
it -- and `pin` declines a read naming a composite's first part. Over a
million posts and 10 000 owners `where owner = $1 order created desc limit
20` took 6.3 us p50 against 11.8 through `owner @hash` and 24.4
ms through `created @sorted`, a feed over ten owners 22 us against
100, built in 147 ms in 40 MB; a lone put 715 ns, 1 561
beside it (its first part puts a write anywhere in it), 851 beside
`created @sorted` and `owner @hash`; `@unique(org, slug)` 1 745 ns a put
against 1 285 for `@unique` on the two joined, a read by both 2.5 us
against 1.3 -- a hash index over several fields would answer that by a
microsecond sooner, not worth a third index's code (`make composite-bench`).
`create index` builds it beside the database (`begin_composite` copies the
keys under the read lock); a block puts one made or dropped back
(`Undo::Composed`, `Uncomposed`); a field renamed renames its parts, one
dropped takes it off, either way unbuilt for the first read
(`fit_composites`). A replica builds what the schema declares, a sync
replica a unique one as an ordered one (`schema_ddl`, `schemaDDL`, from
`/collections`' `indexes`). Without `sorted` a file declaring one opens, its
queries the scan's and a unique one asked by reading the rows
(`holds_tuple`). `tests/composite.rs` holds every filter, order and page --
`limit $1` too -- to a twin collection without it through writes, a block
put back, a reopen, an index made later and one beside the database, a
rename and a drop, NaN, nulls, `-0.0`, collations and json paths. The
browser module grew 43.6 KB, 14.1 KB brotli -- the index, its planner, its DDL
and its declared schema, no one function past 4 KB -- and the test build
without the indexes 12.8 KB, 3.9 KB brotli.

## Lazy indexes

**An open builds no hash, text, ordered or sparse index.** Each is a
`Derived` -- a `OnceLock` of the index or of the error its build met -- which
an open leaves empty and the first statement that reads it fills from the
documents, under the read lock too, a second reader waiting for the first
one's build (`Collection::hash`, `text`, `sorted_index`, `sparse_index`). A
write skips an unbuilt one, since its build reads the documents as they stand
then; a block undone after a read built one takes its writes back out as it
does from any other. An ordered index over a collated field is built at the
open still: in the browser a comparison meeting a script whose chunk it lacks
has the load refused and run again with the chunk, and one built by a read
would keep the order it had without it. `stats` and `memory_bytes` count
what is built -- counted by building, every metrics scrape would build them
all. With one `@hash` field, 100 000 x 128 opens in 21.9 ms against 31.0,
and the browser module loads 20 000 x 128 with a hash, an ordered and a text
index in 19.0 ms against 41.8; the first read of each pays its build. It
cost the module 0.7 KB brotli, most of it a builder and a cell a kind. Nor
does an open list a collection's ids where nothing is built from its
documents (`rebuild_indexes_with`): listed for every collection, they were
2.1 ms of a million rows' open.

## warm

**A server warms what an open left for the first read** (`--warm`,
`fennec_http::warm`). The first `match` after a start built its `@text`
index inside the request: a shop's first search page took 2.68 s to its
largest paint against 1.40 warm. `Database::unbuilt_indexes` lists the
derived indexes nothing has built by field and kind (`DerivedKind`), a
text field keeping `@text` beside its `@unique` listed twice; `warm_index`
builds the one named as its first read would, and a thread of the server's
builds them one at a time, each under
the read lock on its own -- reads go on, one needing the index waits for
that build, a write waits out one index at most. So a bench times nothing
until its warm-up has read every index the writes keep up: `make
recon-bench`'s read all but the journal's `@hash` on its account, which no
statement there reads, and `--warm` built it -- 100 to 150 ms over a million
entries -- as the first round began, every transfer and every read behind
them waiting; a read of it waits for that build, or makes it. The thread starts once the
HTTP endpoint is up: `Server::new` takes the write lock for its watcher,
and a build's read lock held the listener back by its 141 ms. Over 100 000
products with a text, two hash and an ordered index the first answer came
16 ms after the start either way, the indexes were built 208 ms after it,
and the first `match` took 1.4 ms against 166; the process held 99 MB
against 88 for a cold one that had searched. On by default for a file (an
index is declared to be read; `--warm off`, or a list of collections and
`collection.field`s), off for a `--dir` node's tenants, which open per
request and close when idle (`Tenants::with_warm` when asked). The native
library has `fennec_warm` (and each binding a `warm`), embedded Rust
`Database::warm`.

## @geo

**A point is two `f64`s on Redis's sphere, and `@geo` is Morton keys in the
ordered index's chunks** (`geo.rs`). A `geo` field (type tag 15) holds
`[lon, lat]` in degrees as written, 16 bytes behind value tag 15, refused
past 180 or 90 degrees rather than wrapped, and once read is a
`Value::List` of two floats: a `Value::Geo` of its own broke `Value`'s
niche, 16 -> 24 bytes on wasm32 and 6 KB more of the browser module.
`distance(a, b)` is the haversine on Redis's radius (6 372 797.56 m), so it
is `GEODIST`: over 2 000 pairs within 0.05 mm of Redis's answer from the
points it keeps (`GEOPOS`, its 52-bit geohash's cells) and 0.54 m from the
points as written; PostGIS's sphere is the mean radius, 0.03% less. Its
`sin`, `cos` and `asin` are series of `geo.rs`'s own, held to the standard
library's within a few units in the last place: the platform's libm rounds
its last bit its own way, so a point on a radius's edge could be in on a
server and out in a page, and in the browser module it brought libm's range
reduction and tables. A list of numbers going to a `geo` field, a
`distance`, a `within` or a `near` over one is read again as written
(`DataType::keeps_numbers`, `geo::is_geo_call` in `exactly_for`'s
walk), as a json
field's is: read the quick way into `f32`s, a point moved by up to 1.7 m at
the antimeridian. `within(p, [w, s, e, n])` holds the box's edges, a west
east of its east crossing the antimeridian; `near loc p` orders by
`(distance, id)`, `_score` the metres as an `f32`, `ef` refused; a build
without `sorted` answers every one of them by the scan. `@geo` (index kind
10) is derived, built at the first read as `@sorted` is: each row's key --
its longitude and latitude cut to 32 bits each, monotonely, and interleaved
-- in a `Chunked<(u64, DocId)>`, the ordered index's own chunks. A
radius's bounding box (padded 1e-7; every longitude over a pole; two boxes
across the antimeridian) or a box is covered by at most 32 cells of the
quadtree over the keys, each a range of them, each key's own cut
coordinates tested against the box with no margin, the ids left tested by
the filter; `filter_candidates` takes it by the rule a range is taken by,
while it names fewer rows than what is in hand. `near` walks the cells
best first by the nearest a cell's points can lie (`cell_floor`, a metre
and 1e-7 under), a cell ahead of a point as near so ties come out by id,
and stops at the page or at a radius the filter puts on the same point; a
hash bucket of 4 096 rows or fewer (`GEO_SET`) is measured whole instead,
and past an eighth of the collection tested the walk gives up for the
filter's set. Not a radius's own box: over 100 000 rows round cities a 50
km box held thousands of rows where the walk met a few dozen, 440 us
against 32. A row whose
latitudes alone put it past a radius (`geo::past`: the meridian arc
between them, less a metre and 1e-9 of it, which the haversine is never
under) is answered with no trigonometry, so a scan by a radius over a
million rows went 136 -> 22 ms, and `near ... exact`'s page of ten, kept
in order as the rows come rather than put in order by `order_rows`, 147 ->
6.1 ms; the test is out of `Filter::test`'s line (`geo_test`), which it
grew by a quarter. The browser module's scans run 3% slower than main's
(`make wasm-speed`'s filter 1.71 -> 1.75 ms) with no point anywhere in
their path -- the read, the test and the binding taken out, the same -- as
main's own did with 26 KB of float formatting that nothing calls: where
the code lands, not what it does. `tests/geo.rs` holds every filter,
order, page and nearest to a twin collection without the index over 2 000
rows round both poles, the antimeridian and a city, radius 0 to past half
the earth, through writes, an index made later and a block put back, and
Redis's own `GEOSEARCH` examples. Over a million points round 48 cities (`make
geo-bench`: fennecdb in process; PostGIS 3.5 and Redis 7 in Docker, each
server's own mean time read from `pg_stat_statements`, planning included,
and `INFO commandstats`): the index builds in 34 ms and holds 20 MB
(PostGIS's GiST 5.4 s and 82 MB, Redis's sorted set 86 MB); within 100 m
7.2 us p50 against Redis's 12.6 and PostGIS's 1 262 (its plan read 132
pages of the GiST); 1 km 12.6 against 34 and 1 310; 10 km, 2 543 rows,
0.56 ms against 1.73 and 4.84; 100 km, 36 341 rows, 5.9 ms against 7.6 and
32.2, the scan 22-28 ms; the ten nearest 33 us anywhere and 38 within 50
km, against Redis's 198 ms and 6.0 ms (its search sorts every member of
the cells it reads) and PostGIS's KNN 1.75 and 1.77 ms. A put with `@geo`
costs 1.19 us against 0.62 without and a `@sorted` float's 0.98. The
browser module grew 23.1 KB, 8.0 KB brotli -- the index's cover and walk
4.4 KB of the 23.1 -- and the one without indexes 11.3 KB, 4.2 brotli.
