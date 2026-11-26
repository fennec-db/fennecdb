# The HNSW graph

The long form of the invariants [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) lists under *The HNSW graph*: the graph as derived data, its records, its restore and open, its linking, and vectors written again; and of the one [src/engine/CLAUDE.md](../../crates/fennec-core/src/engine/CLAUDE.md) lists under *Storage*, beside the maintenance it shares: a worn graph rebuilt. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## The graph is derived

**The HNSW graph is derived data, not a cache.** It is written by
`snapshot`, `compact` and `checkpoint`, and by a server into its file's tail
(below) — never on the write path. On open the
version, dimension, precision and link bounds are validated, and the live nodes
against the documents holding a vector -- one each, no two of one document,
and counted by reading every document's field only when the nodes are fewer
than the documents: the scan was 5.4 ms of a 36 ms open at 100 000 x 128,
where every document held one. Anything off means a silent full
rebuild. A corrupt graph can therefore never lose data. It is restored where
its last record is -- a checkpoint's image holds one after each collection's
data; `survey` walks the record heads for it first, since restoring each
record read every vector again -- against the documents as they stood there,
and the writes after it are applied as the write path would (a touched
document keeps its node while it holds the same vector, and has it retired for
the new one otherwise) -- restored after the whole file, one write in the
tail threw it away, and a crash cost 48 s at 100 000 x 768 instead of 0.99.
An index added in the tail (`create index`) leaves the graphs restored
before it, as a replica leaves them; reset with the rest, it had the next
open build them all again. A
tombstone carries its own vector in the record, since its document may be gone:
without that, one `del` rebuilt the graph on every open until `compact`.
`compact` rebuilds a graph holding tombstones and leaves the rest: nothing
else takes one out, every write that changes or deletes a vector leaves one,
and they crowd the beam `near` walks. A rewrite touches only the indexes
whose field it changes (`unindex_doc` and `index_scalar` take both versions,
and `VectorIndex::insert` keeps a node that already holds the vector): an
update of a title took the vector out of the graph and back in, 1.89 ms at
20 000 x 768 and a tombstone, against 0.006 ms now. "Unchanged" is to the
bit, since `==` says -0.0 is 0.0 and a hash key is the value's encoding. An unfiltered `near` they cut short walks
again with the beam wider by their number -- no more than all of them can be
in it -- or searches exactly where that walk costs more than reading every
vector (`past_tombstones`); without it a `limit 10` answered 4 rows.

## Restored arena

**A restore gives back the arena it had, to the bit.** Restoring a graph
reads each live node's vector out of its document and normalises it as the
write path did, the flat sum's order and all (`flat_sq`: the 8-strip `norm`
rounds otherwise, and the graph would be another). One add waiting on the
one before made that a third of an open, so natively eight vectors are
summed side by side, each in its own order (`flat_sqs8`), and the links --
two or three bytes each -- are decoded inline (`graph_varint`), their list
buffer kept: a checkpointed 100 000 x 128 file opened in 29.8 ms against
41.0, 100 000 x 768 in 70 against 134, and
`a_restored_arena_holds_the_vectors_it_had_bit_for_bit` holds the arena to
the one it restored. The browser loads a 20 000 x 128 image 13% faster from
the rest; summing side by side gained it nothing there, and would have cost
0.5 KB brotli. Natively the arena is filled a share of `FILL_SHARE` nodes at
a time by whichever thread is free, each writing its nodes' slots where they
stand (`fill_restored`) rather than reading a vector, copying it into a
batch and pushing it: 100 000 x 128 opens in 16.5 ms against 21.8, x 768 in
42 against 61, any share from 256 to 4 096 nodes the same. The mapped
file's pages it reads are most of what is left of that; `MADV_WILLNEED` took
off 3%. The browser fills in turn: the shares were 1.3 KB brotli of its
module for the same load. Each document's node is a `DocMap` (`store.rs`),
a dense array where the ids are, as the store's id index, which the text
index's lengths share: filled as a `HashMap` it was 2.1 of a 16.7 ms open.

## Flat graph records

**A graph record is laid out flat.** Versions 7, an image's, and 8, a
server's tail -- 9 and 10, the aliases added, are what is written now
(below) -- hold the nodes' documents, flags, levels and level-0 lengths
as arrays, then every list sorted, its first link in as many bytes as the
node count needs and its steps in as many as its widest (`put_links`).
Decoding a varint a link was a quarter of an open; written 4 bytes a link
and 8 a document, a 100 000 x 128 graph took 12.4 MB against the varints'
6.2, and a file 5% more. Laid out so, that file opens in 35.9 ms against
43.5 at 58.97 MB against 58.49, and 100 000 x 768 in 63.6 against 67.5.
Both layouts are read into the same arrays (`Read`) and the index is built
from them in one place (`build_restored`);
`a_flat_graph_record_restores_as_the_varint_one_did` holds 3 to 6 and 7 to
8 to the same index, which a binary before 7 builds again. The browser
module carries the reader -- 1.8 KB brotli -- to open a server's file
without building its graphs again, and loads its own no faster for it.
Natively a graph of `LISTS_SHARED` (8 192) nodes or more has its level-0
lists decoded `LIST_SHARE` (2 048) nodes at a time on every core, once a
walk over the lists' widths has found where each share starts, and its
levels above beside them on a thread of their own (`lists_in_shares`), each
share held to end where the next starts: in turn they were 2.6 ms of an
8.6 ms open at 100 000 x 128, a stretch no other core worked in, and are 1.3
-- the walk 0.7 of it. Below 8 192 nodes the threads cost more than they share: a
graph of 1 000 opened in 0.197 ms that way against 0.166 in turn. Each width
of a document id is its own loop, and the flags are checked before they are
copied: a `memcpy` a document, the arrays were 0.7 ms, and are 0.3. The
browser decodes in turn as it did, its module 325 bytes smaller raw.

## Parallel linking

**A batch links in parallel, into the graph it would link in turn.**
`insert_batch` finds a batch's neighbours on every core against the graph
before it (`compute_candidates`); `link_batch` then sets each node's own
lists and groups what they add to their neighbours' lists by list, each
group applied in node order on whichever thread is free (`spread`). A
pruning reads only the list it prunes and the vectors, so every list ends
as linking the nodes one after another left it, and
`a_batch_links_as_its_nodes_would_one_after_another` holds the graph
records to that byte for byte over every arena. Linked on one core, the
pruning was 70% of a 100 000 x 128 build: 10.6 s then, 5.3 after (3.6-3.8
s since, with NEON's strips and the prefetch, below); 48.9 -> 23.4 s at
768 dimensions, and 50.6 -> 22.0 s for a crashed file's vectors linked at
the open. A share of the work a thread left the M1's efficiency
cores finishing last while the rest waited, so the work goes out an item
at a time. The browser has one thread and links in turn (`link_node`),
through the same pruning (`GraphView::pruned`).

## Upper layer beam

**The upper layers are walked with a beam.** A search and a node joining
the graph walk each layer above level 0 with a beam of four (`UPPER_BEAM`,
`descend_beam`) and start level 0 from all four, where the greedy descent
took one node at a time. Over a million 128-dim vectors in 64 clusters it
left 20 of 1 000 queries in another cluster, and level 0 has too few links
across clusters for any beam to cross back: 12 found none of their ten,
and recall stopped at 97.4% from a beam of 200 up to 800. With the beam,
92.5, 99.1 and 99.8% at 40, 100 and 200 against 89.9, 96.4 and 97.4 --
pgvector's at the same m and ef_construction 93.0, 97.4 and 97.8 -- for a build
3 to 7% longer and a search as fast (`make scale-bench`). Both ways are
needed: in the search alone it gave 98.2% at 100, in the build alone a
graph the greedy search lost 32 queries in. The browser module grew 150
bytes brotli.

## Block linking

**A block's `put`s link their vectors together.** A block
`Database::begin` opened -- a `/batch`, a keyed write -- is natively its
statements' batch: a `put` in it leaves its vectors waiting
(`defer_batch`, `Block::waiting`), and they are linked on every core 512
at a time a field (`LINK_AT`, `link_waiting`), the rest as the block lands,
before its record is written, so that a block put back after all is put
back as any other. A search in the block measures the waiting ones
exactly, as it does a server's backlog, and a rollback drops those its
undo made tombstones (`forget_waiting`) -- the newest of the waiting, since
nothing else leaves one while a block is open. Natively a rollback gives
each row back the node its write took from it, a tombstone the graph still
holds (`VectorIndex::put_back`), rather than place its vector again: a
graph at its bound has no node left for one more ([crates.md,
limits](crates.md#limits)). The node comes back with its own links, and
the links into it that lists chosen again while it was a tombstone let go
of are made again from its own once the put-back ends (`link_revived`):
each node it links to takes it back, pruned as a link is, no search made
-- made with none, a rollback of a `put` rewriting 4 000 rows left 3 899 or
more of them unfound by their own vector -- unless the pruning would let
go of a node no other list holds, kept then in the place of the last it
chose that another list holds (`link_back_into`): pruned as a link is,
at m = 4 the links back took nodes' other ways in, and later writes'
pruning left more live nodes no walk reached than main's rollback did
([crates.md, limits](crates.md#limits)). Asking whether another list
holds a node reads about `m0` squared list entries, and a link back asks
it of each node its list chose: about `m0` cubed a link back, 556 at
m = 4 and 23 995 at m = 16 at the most over the review's stress, 157 494
at m = 32, so it is asked up to an `m` of 16 alone (`KEEP_M`), past which
a link back is a link -- at m = 32 the stress left no node unreached
either way, and the misses 0.3 to 0.8% more without it. Each of its lists holding a
node the put-back made a tombstone, the block's (`taken`), is then chosen
again from its live nodes and those its tombstones' lists hold, pruned
the same way (`mend`): a `put` linked as a
batch while the rows' nodes were tombstones linked its first nodes
through them, and their full lists, chosen again taking no tombstone,
kept the block's nodes alone. An entry left a tombstone by a put-back
that made a node live or a tombstone there -- the block's node placed
above the top level -- moves to the live node, none waiting to be
linked, of the highest level a walk from it down its levels meets in 256
lists (`enter_live`, `live_near`), the top level falling with it: the
block's start's entry, in a graph linked in node order, where the walk
meets it -- the highest live node the walk meets, not always the highest
the graph holds (at m = 2, 1 of 86 moves over the stress's 16 seeds left a
live node a level above the top, reached through its lower lists as any
node is; [crates.md, limits](crates.md#limits)). Every node is
walked only where that meets none and the entry is the block's
(`live_anywhere`); a block that wrote no vector there puts nothing back.
A delete leaves the entry where it is, its links still walked, and
a rollback placing every vector again reached the rows by a search from
it; over 2 000 x 16 after a `put` of every row, the revived graph missed
372 to 444 rows by their own vector at ef 8, 118 to 159 with the entry
moved alone, and none with both (placing them again: 0 to 1). The nodes
the block placed stay, tombstones, until the graph is made again; the tombstones a
put-back makes are never the ones it makes live (`take_back`). A lone statement links as
it did, and so does the browser module's `run` of several: its one thread
links a vector as it is written (`DEFERS`), the waiting being 843 bytes
brotli of it for nothing. Linked a row at a time, as a `/batch` of single puts would have
them, 100 000 128-dim rows went in at 5.3k rows/s with the graph kept;
linked together, at 16.0k, and 18.3k as a `/batch` of single puts over HTTP now (`make load-bench`).

## Deferred linking

**A server answers before its graph is linked.** A server checkpoints only
on its way down, so a crash after a long run leaves every vector written
since in the tail, and linking them at the open kept the port closed for as
long as they took: at 100 000 x 768 never checkpointed, `fennec-server` answered
its first `near` 67.7 s after it started, and linking at the open still
took 18.4 s with the lists pruned in parallel (14.9-16.3 s now, below). `fennec-server`, a tenant and a replica
open with `fs::open_serving` instead, and it answers after 1.23 s: those
vectors go into the arena unlinked (`VectorIndex::defer_batch`, every vector
of a graph the open cannot restore too), a search measures each of them
beside what its walk finds -- so an answer is never missing one -- and
`fennec_http::link::beside` links them on a thread of its own, slices of
about 10 ms under the write lock, each at most twice the last (a pace taken
over a small graph had a slice hold the lock for 112 ms). `near` takes the
exact scan's 10.5 ms until the 19.2 s of linking are done and 0.31 ms after,
recall 0.976 against 0.978 (`make reopen-bench`). A checkpoint meanwhile
writes the waiting nodes flagged -- a node's flag in the flat layout, where
the varint one, which only the tests write now, took graph record version 5
and only then -- and an open that does not
defer links them there. The linking needs the
lock to let a waiting writer in: Linux's std lock does, while on macOS
readers slip past it, and four clients asking back to back kept it from
finishing in eleven minutes -- as they would keep any write waiting. The
browser has none of it (`vector::UNLINKED`: 1.1 KB brotli).

## Graphs in the tail

**A server keeps its graphs in its file.** It checkpoints only on its way
down, so a crash after a long run left every vector written since the start
to link again. `fennec_http::link::keep` looks at every database the process
serves every 5 s, and appends to the tail each graph that changed in 10 000
nodes since it last reached the file, has none waiting to be linked, and
whose record the file has grown three times over since
(`Database::save_graphs`, under the read lock, which keeps the writes out
while the graph is written). A database whose lock is taken is passed over
until the next look (`try_read`): a block of 100 000 128-dim vectors loaded
with the graph kept held the keeper 3.0 to 4.3 s, and every other database
it keeps with it. The record holds the whole graph, so a bound
on the linking alone would have a big graph written over and over for a
little of it; waiting for the linking kept a crash from leaving the nodes
waiting again. It is a graph record (kind 4) of version 6, 8 laid out flat
and 10 with the aliases (below), which an older binary does not know and rebuilds from -- it restored a record in the tail
against the documents the whole file left, and a vector rewritten after it
kept the links of the one before -- and like the history it moves no
counter and no replica is sent it (`Tee::append`). At 100 000 x 768 a crash
leaves at most 10 000 vectors to link, 2.6 s with `near` at 1.56 ms p50
meanwhile, where every vector waited 19.1 s in the same run with `near` at
the exact scan's 11.96 ms; the ten records were 33.8 MB of a 372.7 MB file
until the next checkpoint. Every write waits out a record's read lock, so
its lists go out a part of `LIST_PART` nodes at a time on every core
(`put_all_lists`), each link as its eight bytes cut to its width rather
than a `memcpy` of the width, and the file's sink leaves the record to the
durability run once the lock is let go (`Sink::append_deferred`) rather
than write its megabytes under it: a record held the lock 5.0 ms p50 and
7.9 at most, against 18.3 and 33.3, the same bytes (`make reopen-bench`,
in turns).

## Aliases

**A vector written again is one node** (`VectorIndex::place`,
`aliases`, `Same`). Written hundreds of times, a node each filled its
neighbours' lists with copies -- the diversity rule takes a candidate at
distance 0 from the owner, `0 < 0` being false -- and a walk could not
leave them: 1 812 of 6 019 copies over 20 000 x 128 found by no search,
other queries' recall 0.82 -> 0.67. Skipping copies in the heuristic took
the recall back to 0.72 and orphaned the copies, since no list took them
in. Now every insert, batch and deferral goes through `place`, which
finds a live node storing the same vector to the bit -- `Same`, open
addressing over `node + 1` beside the hash's top half, made on the first
insert rather than at an open, 16 to 32 bytes a node -- and makes the
document one of its `aliases` (`(node, doc)` sorted, not a map: another
hashbrown in the browser) rather than a node. A search hands a node out as
its documents in id order, the exact paths by `(node, doc)` pairs put in
node order through the `u64` sort; a document leaving a node hands it to
another holding its vector, and only the last makes it a tombstone
(`detach`). Over codes the stored codes are compared, which `near` puts in
order by the documents' own vectors anyway. The level is drawn only for a
new node, so a graph with no vector twice is byte for byte the graph it
was. Graph records 9 and 10 carry the aliases after the tombstones (and
the copies table's hash halves after those, below); a restore
checks each alias's document holds the node's vector. Tests whose
data repeated vectors (`i % 13`) to count nodes and tombstones now write a
vector a row. The browser module 2.2 KB brotli; builds, searches and
opens as fast.

## The copies table

**Natively the table that finds a copy grows from its own slots**
(`Same`, `SameTable`). In the browser `Same` is one table, made again from
the arena once it is half full; natively that was the write that found it
full hashing every live vector again under the write lock -- 118-132 and
242-281 ms at 2^19 and 2^20 nodes of 128 dimensions, 166-169 and 376-377
ms at 2^17 and 2^18 of 768 and 1.45 s at 2^19 (`make growth-bench`'s
`vector`). A slot's place is the low bits of the half of the hash it
holds, so a table made twice the size reads its slots and no vector,
leaving the tombstones' behind, and past `SAME_SPLIT` (2^16 slots, an
index of 32 768 nodes) the table is 256, by that half's top byte, each
growing on its own: the split moves its slots in 0.72-0.80 ms and a shard
at 2 million nodes grows in 0.15, and over 1.1 million 128-dim puts one at
a time and 300 000 768-dim ones no put at a doubling took 10 ms. A place
taken from the half leaves fewer of its bits to tell two vectors apart --
a slot of another vector at the same place passes the half's test once in
2^(24 - k) for a shard of 2^k slots, a vector compared for nothing about
once in 4 000 puts at a million nodes. Where its graph record carries no
hash halves (below), the first put after an open makes the table from the
arena, every vector hashed -- made at the open it would cost a database
that only reads, and under `--warm` a read-only server 16 to 32 bytes a
node -- on every core, `SAME_SHARE` nodes at a
time, through four chains of multiplies and MurmurHash3's finish
(`hash_lanes`) where one chain waited on its multiply a word (a 768-dim
vector in cache 1.18 us against 0.17): 275-441 ->
33-169 ms at 1.1 million x 128, 444-741 -> 24-175 ms at 300 000 x 768, and
1.18-1.37 -> 0.31-0.59 s at 600 000 x 768, where on this 8 GB machine the
arena and the file outgrew memory. An open whose tail holds vectors makes
it there: `make reopen-bench`'s deferred open of 100 000 x 768 never
linked 0.59-0.75 -> 0.35 s, the linked one 16.2-17.3 -> 14.9-16.3 s. A
put's p50, a batch's build (100 000 x 128 in 3.63-3.74 s against
3.72-3.85), `near` and an open of a checkpointed file are as they were.
The browser module kept its table and its hash (`cfg`): 618 010 bytes as
on main, every function the size it was, until it took the native hash
with the halves (below).

## Halves

**A graph record carries the halves the copies table is made from**
(`HALVES`, `Same::halves`). The first put after an open read and hashed
every vector to make the table: 33-169 ms over 1.1 million of 128
dimensions. Hashed as the restore fills the arena, each vector in cache,
the table would cost no put, but the open of a checkpointed 100 000 x 128
took 0.45 ms longer (3.8%) and of 100 000 x 768 2 ms (6.7%), and four
vectors' chains side by side hashed only 1.65 times as fast. So a version
9 or 10 record ends with a `HALVES` byte and the top half of each node's
hash, 4 bytes a node, a tombstone's 0: 0.68% of a 128-dim file, 0.13% of
a 768-dim one. The restore keeps them -- once the nodes at 64 places
spread over the arena hash to theirs (`keep_halves`) -- and the first
vector placed makes the table from them with no vector read, gathered by
shard and each shard's table filled on whichever core is free
(`Same::spread`: 1.1 million halves in 4.0-4.4 ms, where put in where
they fell they took 10.5-10.9). The first put after an open went 4.7-5.5
-> 3.4-4.2 ms at 100 000 x 128, 12.8-15.8 -> 6.4-9.7 at 100 000 x 768,
30-244 -> 7.6-8.5 at 300 000 x 768, and in `make growth-bench` 47-160
-> 16-31 at 1.1 million x 128 and 68-312 -> 4.4-27 at 300 000 x 768;
the opens as they were, 11.65-11.98 against 11.68-11.83 ms and 28.2-29.1
against 28.7-30.1 in turns, and the puts after (growth-bench's p50 264
and 263 us, 690 and 676). What is left is the first growth of the arrays
a restore sized exactly and the caches an open leaves cold. On this 8 GB
machine an open that paged the arena out had main's first put, reading
every vector, bring it all back; now the puts after it fault it in as
they walk -- 3.4 -> 8.4-9.7 ms p50 at 300 000 x 768 there, the first
twenty puts 339 -> 224 ms in all. A
database that is only read keeps the halves, 4 bytes a node -- 0.6% of a
128-dim index. A writer has them from the restore, or reads them off the
table's slots (`halves_by_node`, on every core past `SAME_SPLIT` nodes:
8.3 -> 2.3 ms at 1.1 million) -- natively, a split table's on the threads
that write the record's lists, a share of the shards to each besides its
part (`put_all_lists`): read after the lists, they held a server's read
lock as it keeps a graph of 110 000 nodes 6.07-6.34 ms p50 against
5.85-5.88 without them, and beside them 5.83-5.98 (100 saves a run, five
runs each in turns) -- or hashes the arena where neither holds them, so a
record is the same whatever way its index came to be. The record keeps
its version: the reader before stops at the aliases, so a binary from
before reads it without them -- this build refuses anything else there --
and a record without them, a binary's from before, has the table made
from the arena.

## Halves in the browser

**The browser module hashes as a native build does, and takes the
halves.** Its `Same` hashed with one chain of multiplies (`hash_words`),
so it could use no server's halves, and its first put after a load hashed
every vector: 13.1-14.6 ms at 20 000 x 384, 21-34 at 100 000 x 128, the
puts after it 0.3-0.5. It hashes with `hash_lanes` now, a slot placed by
its half as natively, and reads and writes the halves as a native build
does; `web/fennec.test.js` finds the bytes
`the_halves_a_record_carries_are_the_browsers` holds a native record to in
the module's image of the same vectors. Its first put after a load went
13.1-14.6 -> 1.3-2.1 ms at 20 000 x 384 from its own image and 12.7-13.1
-> 1.2-1.9 from a native file, 21.6-24.1 -> 2.8-4.2 and 21.3-30.7 ->
3.0-5.9 at 100 000 x 128; its loads (65.4 against 65.6 ms median, 25.8
against 26.0), builds, puts, `near`, filters and JSON (`make wasm-speed`)
as they were, and the graph and its aliases the same bytes. 1 221 bytes,
543 gzip, 197 brotli (`make size-report`): the module holds one copy of
the hash, over the bytes the arena would hold -- an f32 vector written
out as a code is -- where another over the floats was 0.4 KB more;
checking the halves in a plain loop rather than through `filter`,
`step_by` and `all` took 0.5 KB off, and appending them a half at a time
rather than into room made first 0.4 KB.

## Opening a graph

**An open of a file with a graph copies its vectors, and that is most of
it.** `make compare`'s reopen once read 9 ms against SQLite's 0.9: one open
and an ANN query of fennecdb's against SQLite's open and a count through its
index, two different first questions. It now times each side's open alone,
then with a read by id, then with a vector top-10 -- SQLite's a full scan --
the median of 31 each: 6.0, 6.0 and 6.1 ms against 0.07, 0.46 and 30.5.
SQLite reads nothing until it is asked. Of fennecdb's 6.2 ms at 100 000 x 128
(`examples/opens.rs`, timers in each part), 3.6 are the arena -- 51 MB of
vectors read out of the mapped file, made unit ones and written into pages
the open allocates, on every core -- which the pages bound: each page of the
62 MB file touched once took 1.5 ms on eight threads, each of 62 MB newly
allocated 4.8 (2.7 on one), and with the arena filled on one core the open
took 17.7 ms against 10.5. Then 1.8 the graph record (`Read`), 0.3 `by_doc`
and 0.3 the id index. Before, 8.6: the graph record 3.5, the id index 1.0,
every collection's ids listed for nothing 0.2 (all above). A million rows of
128 dimensions open in 60 ms against 86, a million of no vector in 1.3
against 10.3, a thousand of 128 in 0.156 against 0.165, and the browser
module loads as fast as it did. Rejected: the arena's pages written once each
on a thread beside the record's read, as `MAP_POPULATE` would -- 7.63 ms
against 7.50, the two's faults contending; a restore left to the first
statement that needs the graph, as the derived indexes are -- an open would
no longer validate the graph it holds, and the first `near` or write would
pay the 6 ms; the arena kept in the file for an open to map rather than copy
-- every 100 000 x 128 file 51 MB larger, its vectors twice.

## Worn graphs

**A worn graph is rebuilt.** A write that gives a row another vector, or
deletes it, leaves its node a tombstone: its vector, its lists and its
flags stay, routing walks, until the graph is built again -- about 230
bytes of heap at 16 dimensions (64 of vector, 128 of level-0 list at the
full stride, the document, the upper levels' header, a flag), and about
145 in each record of it (the vector, the links, a flag, a level, a hash
half). Only a compact rebuilt one, and only of the collections it named:
the P6 soak's primary, 27 vector updates a second into 20 000 rows of
`vector<16>` beside a `compact usertable` every 20 minutes, carried about
490 000 tombstones by its fifth hour -- 113 MB of heap, and a record of
about 75 MB the keeper appended every five minutes and the requested
compact held whole and copied again -- and its 512 MB container killed it.
The file never reached half dead for the server's own compact to rebuild
it either: the record each requested compact wrote counted as kept.

So a graph whose tombstones outnumber its live nodes, and number
`GRAPH_WORN_FLOOR` (10 000) at least, is worn (`VectorIndex::worn`: two
counts the writes keep, compared -- a look reads no node), and
`Database::rebuild_worn` builds it again beside the database: a compact's
graph phase (`graphs_online`) without the rewrite -- its store taken as a
pin takes it, its vectors read and linked with no lock held, the writes
made meanwhile caught up with in rounds, the write lock taken for the last
round's links, `GRAPH_HELD` at most, and to put it in place. The ratio
bounds what the rebuilds cost: each links every live node once, and each
tombstone it drops was left by a write that linked a node of its own, so
the rebuilds together link no more than the writes did, and the graph
holds about twice the nodes it reads at most -- its heap and its record
with it. A lower ratio -- tombstones a quarter of the live nodes -- would
rebuild four times as often, four links a write on top of its own, for a
graph at most a quarter past its live nodes rather than twice them; a
budget in bytes would rebuild a big graph over and over for a little of
it, as a record on a bound of the linking alone would have been written
over and over (above). The floor is
`GRAPH_SAVE_CHANGES`: what a rebuild costs whatever its graph -- its holds
of the lock, its log line, and the graph's record written again -- is paid
once in as many tombstones as a record waits for, and a rebuild drops at
least that many, which it counts as changes (`changed_by`), so the keeper
writes the rebuilt graph at the look that rebuilt it, where after a crash
an open would restore the record before, tombstones and all. Below it a
small graph keeps up to 10 000: 2.3 MB at 16 dimensions, 32 MB at 768.

`make churn-bench` (`examples/churn.rs`) writes 20 000 x 16 over 500 000
times, a row at a time -- 7.3 MB of heap and a 1.3 MB record freshly
loaded. Never rebuilt, as before, the graph ended at 520 000 nodes, 500 000
of them tombstones, 121.5 MB of heap (`graph_bytes` and `arena_bytes`,
capacity included) and a 64.5 MB record. Rebuilt by a keeper looking every
5 s, the writer at 2 000 updates a second: 23 rebuilds, each dropping about
20 900 tombstones in 418-475 ms (440 at the median) on an M-series Mac's
eight cores beside the writer, the write lock held 1.61 ms at the median and
1.90 at most -- the last round's links and the swap; the most a look found
was 41 006 nodes, 16.4 MB and a 4.1 MB record, and the run ended at 39 535,
9.8 MB and 3.9 MB.

A server rebuilds on the thread its compacts run on
(`fennec_http::link::rebuild`), right after its look for a compact, one
database at a time, a replica's as its primary's: a replica applies every
write its primary takes, each a tombstone in its graph, which is its own,
sent to none, and the rebuild writes nothing and moves no change -- as a
replica's compact. The graph keeper's next look appends the rebuilt
graph's record (`changed_by`, above). A rebuild takes as long as building
the graph -- 0.44 s at 20 000 x 16, about 50 s at 100 000 x 768 -- and
the first build ran it on the keeper's thread, before each save: one
rebuild held every other database's graph saves back as long, a crash
meanwhile linking all they wrote since again; the database was held as
long, so a tenant's close was put off (`Tenant::db_in_use`) and a delete
then a create of its name waited `RELEASE_WAIT` for the file's lock and
answered 500; and the database was maintained meanwhile (`maintaining`),
so a compact its quota asked for waited for a look after it. On the
compacter's thread a compact that is due runs first -- it rebuilds every
worn graph itself -- and the rebuild after it, so neither holds the other
off; a long rebuild holds the other databases' compacts back as a long
compact does. The native library's `Compactor` rebuilds where no compact
is due at its look, for the same reason (`FENNEC_OPEN_NO_AUTO_COMPACT`
stops both); an app embedding the engine starts one, or calls
`rebuild_worn` from a thread of its own. Every `compact` rebuilds each
worn graph, whatever collection it names (`Rebuild::Compact`), since its
rewrite writes every graph's record and a worn one's is mostly
tombstones -- under the lock too (`rebuild_graphs_here`), the browser
module's way: it has no thread to rebuild beside a page, so a worn graph
is rebuilt when the page compacts, and its `rebuild_worn`, there for a
host that calls it, builds under the lock. It cost `fennec-server`
4.5 KB of `__TEXT` on aarch64 macOS (4 224 bytes of code), the native
library 2.5 KB, and the browser module 197 bytes, 20 brotli: its compact's
look at every collection for a worn graph. Its stop and each graph's own
wait (below) cost `fennec-server` 1 728 bytes more of code, its `__TEXT`
the pages it was, and the native library 5 696, a page more of `__TEXT`:
the graph phase's placing made a function of its own there
(`finish_graphs`, 5.0 KB, inlined into `graphs_online` before), and the
`Compactor`'s thread asking its stop. The browser module came out a byte
smaller, 92 bytes more brotli: the batches' stop, which it asks of a
closure that never says so.

Whatever lets the database go stops a rebuild under way
(`rebuild_worn_until`). It asks its stop before each graph, every
`STOP_EVERY` (1 024) documents its copy reads and a round links, between
the new graph's batches -- before each node while the graph is under
1 024 nodes, each batch of 512 at most after, about a quarter of a second
at 100 000 x 768 (50 s over some 200 batches) -- before each round, and
under the write
lock before it places a graph; once told, it places none, puts off no
graph's next try and answers `None`. What it was told holds for the rest
of the rebuild, latched in `graphs_online`: a release refused takes its
word back (`held::taken_back`), and a round that left its links undone
once told must never be placed. The first build latched it in
`rebuild_worn_until` and no test held it there: unlatched, a yes at one
look alone -- the look before the last round's chunk, 106 of 108 at 100
rows -- left that chunk unlinked, the look after said no, and the rebuild
placed a graph missing a write made during it, a row holding its old
vector. The `Compactor`'s drop tells it, which `fennec_close`
is: a close on an app's UI thread as it goes to the background waited a
rebuild of 20 000 x 768 out, seconds on a phone, an ANR or a watchdog's
kill. So do a tenant's release -- a delete, a follow, a take-back -- and a
node's or a file server's shutdown (`held::let_go`), and a shutdown's lock
kept to the exit, which the rebuild would wait for for good
(`held::stops`). The node's shutdown looks at its open tenants without
waiting on a slot busy opening, closing or releasing, and a release
refused meanwhile takes its let-go back; neither escapes, since the
shutdown then takes each slot's lock and marks its tenant kept to the
exit before it takes the tenant's write lock, and a rebuild so missed runs
on only through the syncs of the tenants before it. Waiting on a busy
slot there held every other tenant's let-go back a `RELEASE_WAIT`. Asked
under the write lock last, a holder that tells it under its own read lock
finds no graph placed after it, which is how
`maintenance::tests::a_compactor_dropped_during_its_rebuild_leaves_the_graph`
and `tenants::tests`' delete, follow, take-back and shutdown catch it
under way.

A host's compact asks the same stop in its graph phase
(`compact_when_due_until`, `Database::compact_beside_until`): the
`Compactor` and a server's compacter run a compact due before a rebuild,
and its graph phase rebuilds every worn graph, as long as a rebuild takes.
The first build passed it a stop that never said so, so a close during a
compact's graph phase waited the whole build out, as long as the rebuild
it was there to stop. Told there, the compact ends rather than go on to
its rewrite, and answers `None`: nothing is written before the rewrite --
the side file is the rewrite's own -- and no graph is placed, so the file
and the graphs stand as the writes left them, as a stopped rebuild leaves
them, and the compact is due again at the next look. Going on to the
rewrite with the graphs kept as they stood was the other way: a close
would then wait the rewrite out, 2.1 s a gigabyte, and the rewrite would
write each worn graph's record, tombstones and all, which the graph phase
is there to drop. A rewrite begun runs to its end, its file in place or
none, as before; a `compact` statement is never told. The compact's stop,
and the latch moved, cost `fennec-server` 896 bytes of code and the
native library 704, each `__TEXT` the pages it was, on aarch64 macOS; the
browser module, which compacts under the lock and asks no stop, is the
same length, brotli 207 bytes more, inside the layout's noise.

While a rebuild runs the heap holds the old graph, the live vectors copied
out of the documents (a `Vec<f32>` each, `IndexCopy::vectors`) and the new
graph at once, until the new one is placed and the old one dropped. A worn
graph's live nodes are fewer than half its nodes, so the new graph is
under half the old one's heap, and the copies are the live vectors once
more, as large as the new graph at 768 dimensions; the writes made during
the build go into the new graph too, and a round's documents are looked up
4 096 at a time (`CATCH_UP_CHUNK`). `make churn-bench` counts the heap
through its allocator, every thread's but the writer's: at 20 000 x 16 a
rebuild of a graph of 42 975 nodes and 16.5 MB grew it by 12.9 MB at the
most, and at 20 000 x 768, 400 updates a second, one of 41 836 nodes and
142 MB by 189 MB -- the copies 61 MB of it, and a rebuild of 13 to 19 s,
the machine busy, linking thousands of writes meanwhile. So a rebuild
needs room for three quarters of its graph again at 16 dimensions, and
for the graph and a third more at 768 under writes: up to two and a third
times the graph at its peak, which a container's memory limit has to
hold. A compact's graph phase holds as much.

A graph the rebuild keeps as it stood -- behind its writes, holding a
document it cannot read, or past its nodes -- is named in the holds, as a
compact's is, and it alone is not tried again until as many writes as its
tombstones have landed (`VectorIndex::worn_due`, put off to that change by
`put_off`): a graph behind a sustained ingest would be built again at
every look, every core spent on a build the rounds could not catch up
with, where that many writes linked as many nodes, so a try kept costs no
more than one placed. Put off for the whole database, as the first build
had it, every other worn graph waited the kept one out: a `docs` holding a
document that would not read was kept at each try, its tombstones about
doubling between them, and an `items` worn meanwhile was tried only at
those points, its own tombstones growing with them. A rebuild refused -- a
collection changed meanwhile, in its own words (`changed_while_rebuilt`:
no "run `compact` again", which nobody runs for it), a storage error --
places no graph. A panic in it with no lock held leaves the graph the
writes kept up to date, and one under the write lock, as a graph is put in
place, the same: it opens no block and writes nothing, so nothing needs
putting back or failing, and each graph is the old one or the rebuilt one,
whole (`maintenance::tests::a_rebuild_stopped_leaves_the_graph_as_the_writes_left_it`).

The record a compact beside the database wrote of a graph counts as kept
-- what the next compact writes again (`Garbage::kept`) -- and its length is
noted on the graph (`Persisted::kept`), measured as the side file takes it
(`Beside::write`); a rebuild drops the graph it describes, so its bytes
leave `kept` and count as dead, as a record the keeper appended does, for
the file's own compaction to find. Counted whole, it overstates the dead
bytes by the record of the rebuilt graph, which the next compact writes --
as the keeper's records in the tail are counted. An open counts no record
kept, so a graph restored there notes none. A compact under the lock -- an
embedding program's `execute`, the browser module's -- counts its graphs'
records in `kept` too but notes none on a graph, so a rebuild after it
leaves them counted kept until the next compact: noting them meant
measuring each record as `image_into` writes it, which a checkpoint
shares, its `kept` the last compact's.

`maintenance::tests` hold it: a graph past the ratio rebuilt to no
tombstone, each row's own vector finding the ten it found before and the
recall against the exact answer as it was; the writes made during it
linked as the read lock would have linked them; a rebuild behind its
writes kept and waiting out its tombstones' writes, and a kept graph
putting off no other's rebuild; a block put back during it and after it;
a compact naming another collection; a replica's follower applying its
primary's records on a thread of its own as the rebuild reads, links its
rounds and places its graph, a message refused and put back among them;
a refusal for a change meanwhile, in its own words; a stop at each of its
looks, a stop said at one look alone at every look, and a `Compactor`
dropped during one and rebuilding on its own; a compact told to stop in
its graph phase, and a `Compactor` dropped there; a panic and a storage
error; and the record's bytes moved from kept to dead, exactly. `vector::tests`
hold the build's stop between its batches. `fennec-http`'s
`tests/worn_graphs.rs` holds the server to it at its own floor and record
pace -- 200 live nodes, fewer than a record waits for, so only the
tombstones dropped make the rebuilt graph's record due -- the file holding
a record of the worn graph first, and opened again restoring the rebuilt
one to the byte; `tenants::tests` hold a delete, a follow, a take-back
and the shutdown -- told before its syncs, by its let-go -- to stopping
one.
