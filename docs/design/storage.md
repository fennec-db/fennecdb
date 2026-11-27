# Storage

The long form of the invariants [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) lists under *Storage*: the mapped file, the handover of writes to it, a block's spills, compaction and the file format; the rules of the handover, of maintenance beside the database and of compaction are in [crates/fennec-core/src/engine/CLAUDE.md](../../crates/fennec-core/src/engine/CLAUDE.md), beside `handover.rs`, `maintenance.rs` and `garbage.rs`. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## The mapped file

**No buffer pool of its own, and the file is mapped.** The byte sequence on
disk and in memory are the same format, so a read decodes straight over the
bytes -- there is no eviction policy, no dirty pages and no cache of fennecdb's
own. "Page cache" is the operating system's, which keeps the mapped file's
pages and starts cold after a restart, as for any file; it was called "no page
cache" here before, which read as nothing being cached.
`fs::open` maps the file where the target maps files -- a 64-bit Unix or
Windows -- so the documents stay in it and the process holds what it derived from them: the
offset index, the hash, ordered and text indexes, the graph, and the
documents written since the open until it hands them over to the file
(below). A 1 GB file of 2.3 million rows with a hash and an ordered
index holds 188 MB that way against 1 095 read into memory, and its
`compact` peaks at 236 MB against 2 012; a 10 GB file opens on an 8 GB
machine, which read it cannot. On Windows a file is mapped where its
volume renames over a file other handles hold open (NTFS, ReFS) and is no
share (`fs::mappable`); on FAT32, exFAT or a share it is read into memory,
a view there refusing the rename a rewrite makes, or a read error through
one ending the process
([windows.md](windows.md#1-the-mapped-file-on-windows)).
`fs::open_in_memory` (`fennec-server --no-mmap`,
which reaches a replicated file and a `--dir` node's tenants as well) is the
other way, for a network file system or to have `--max-memory` cover the
data. In the browser `store::Base` is the image a load was handed
(`fennec_load_owned`, `Database::load_mapped`): the module keeps it and
reads the documents out of it, as a server reads its file, where it copied
each into a segment -- 10 000 rows of 768 dimensions restored held 97 MB,
and hold 66, for 0.7 KB brotli; the load is up to 12% quicker and the
first build of each derived index after it 2 to 6% slower, which grows the
memory past the image the copy let go of: a load and its first three reads
took 28.7 ms against 28.8. A
new file is mapped from the start. A rewrite -- `checkpoint`,
`compact`, an image adopted -- writes the new file and points the stores at
it (`Database::repoint`), so the old one is let go of; a compact over a
mapped file never copies a record into memory, on a server either, and
rebuilds no index but a graph holding tombstones, since the documents are
the same ones. An image this database writes records where each
collection's data went (`image_into`'s `placed`), and each store works its
new places out from its own (`Store::relocate_image`, `relocate_live`):
walking the new file's record heads read the whole of it back, 1.8 to 2.3 s
of a 1 GB checkpoint under the write lock, against 19 ms. Only an adopted
image, written elsewhere, is walked.

## Handover

**A mapped database hands what it writes over to its file.** A block's
record goes into the file as it lands, and the stores held its documents in
their segments besides, until a restart or a compact: 250 000 768-dim rows
loaded into a server, the engine counted 1 592 MB against 818 for the same
file started again. Once the documents since the last handover amount to
`HANDOVER_AT` (16 MB), 65 536 records or 16 384 writes
(`HANDOVER_DOCS`: its pause is a document's work each, about 17 ns, and 16
MB of rows of a text and an int were 930 000 of them, the write lock held
16 to 17.5 ms; at 65 536 writes 0.82-0.89 ms, and at 16 384 0.26-0.31, a
block's p99 under the lock 1.04 -> 0.45 ms and its mean as it was), `Database::hand_over` has the sink
write what is pending (`Sink::written_through`) and each store point the
documents it holds at their places in the file and let its segments go
(`Store::hand_over`): 820 MB after that load. One due while a durability
fsyncs the file is put off to the first write landing after it, up to
twice the bounds (`Sink::syncing`): it writes into the file, and waited
out the fsync with the write lock held (below). Where each record went is
noted as it lands (`handover::Landed`: a data record's body, each of a
block record's), so nothing of the file is read, and a store takes its runs
in only when they account for every frame its segments hold, in order --
frames a compact built in memory stay, and with them the ones after. Nothing
is handed over while a block is open, whose writes are in no record yet.
The file is the same byte for byte, and so is the image a checkpoint writes
from it (`tests/handover.rs`). The write lock is held 1 ms for 16 MB of
768-dim documents and 2 ms of 128-dim ones; 3.9 and 8.5 at 64 MB. The file
is mapped with room past its end (`Mapping::with_room`: its length again, a
gigabyte at the least, address space alone) and the mapping grows over the
appends, which Linux and macOS both show through a mapping made before them,
so readers keep the pages they touched; only a file outgrowing it is mapped
anew. Windows maps no room past a file -- a section keeps the size it was
made at, and a read-only one is no larger than its file -- so the room is a
placeholder (`VirtualAlloc2`) the file's views are laid in end to end: the
open's, then at each handover past them the file's pages from the page the
last one ends at (`Mapping::cover`: the placeholder split, a section and a
view, `MapViewOfFile3`), the stores' addresses kept and a view's last page
showing what later writes put there, CI found; where the system has no
placeholders, before Windows 10 1803, the file is mapped whole and anew at
each handover, as one outgrowing its room is; and a mapping of more than 16
views dropped lets them go on a thread of its own, an `UnmapViewOfFile`
each, rather than under the write lock (`map::Teardown`,
[windows.md](windows.md#built-in-slice-2)). Whether the heap goes back to the system is the allocator's call:
musl's and glibc's unmap a large block when it is freed, so the process
holds what the engine counts (250 000 x 768 with no graph: 6 MB of
anonymous memory against 777), while macOS's keeps hundreds of megabytes of
freed large blocks in a cache of its own, which its footprint counts (998
to 1 288 MB after the 250 000 x 768 load; 823 with `MallocSpaceEfficient=1`).
So a large block's buffers are let go of as it lands (`Database::spare`),
where a COPY of 50 000 768-dim rows kept its 154 MB of frames until the next
write, and a record the sink's buffer cannot hold is written where it lies
rather than copied into it.

## Spills

**A block that outgrows 16 MB spills into the file before it lands.** Held
back until it landed, a block's frames were in memory twice, in its record
and in the stores: 250 000 768-dim rows in one block peaked at 2 389 MB of
anonymous memory on Linux against the 820 they left, blocks of 50 000 at
1 144. Once the frames a block holds amount to `SPILL_AT` (16 MB),
`run_one` has it spill after the statement (`Database::spill`): the frames
are appended as a spill record (kind 10, a block record's body), and each
store takes its frames in from there with the records that landed before
the block (`Store::hand_over_keeping`) -- the block's mark for it becomes
where it stood then, those taken in (`Mark::spilled`), and each document
the block overwrote is pointed at where it is in the file now
(`store::Moved`), so a rollback puts it back. The block lands as a land
record (kind 11) naming its spills by where their bodies are, then its
writes since the last; a load applies a spill only where a land names it,
and cuts off the spills a file ends with. A rollback, a lapsed lease or a
crash leaves them dead in the file until a compact. The land never travels:
the sink is handed the spills' bodies (`Sink::land`), and a primary's feed
sends the block as the one block record it would have been
(`landed_block`), which a replica applies whole. A spill that cannot be
made (`Store::would_hand_over` refuses its frames) stops the block's
spills, which lands holding them as before. With them the peaks were 904 and
888 MB (in process, in a `rust:alpine` container), and the one block
without a graph loaded in 3.8 s against 8.6. A binary from before refuses
kinds 10 and 11. The browser module writes no spill, and reading them cost
it 1.5 KB, 0.5 KB brotli.

## Maintenance beside the database

**A server's `create index` and `compact` run beside the database**
(`Database::maintain`, `engine/maintenance.rs`): under the read lock it takes
only what the build reads from -- a collection's store as a pin takes it, a
compact's for one graph at a time -- reads the documents from that with no
lock held, builds with none, and takes the write lock only to apply the last of the writes made
meanwhile and put the result in place ([the graphs off the
lock](#the-graphs-off-the-lock), below). Those writes
are known exactly, not from the change ring a long build would overflow: every
write passes through `Database::note`, which hands the id to the `Tail` of each
maintenance on that collection -- so a write path that skipped `note` would
leave a built index missing it. A schema change there (another index, a drop)
fails the maintenance rather than installing what no longer fits. At 100 000 x
128 reads waited at most 21 ms through an HNSW build and 30 ms through a compact
(file rewrite included), against the full ~20 s under the write lock. Over a
mapped file a compact copies no record: the graphs holding tombstones are
rebuilt beside the database, their vectors read with no lock held, then each
store is cloned under the read lock --
the records in the old file and the sealed segments shared (`SegmentBytes` is
an `Arc`, in the browser module too now), the index copied, each graph's
record written, held once: streamed into the side file after its field's
name and let go of there (`Beside::write`, [hnsw.md](hnsw.md#graphs-in-the-tail)) -- the live
records written into `<file>.beside` with no lock held and fsynced, and
the clones relocated onto it without reading it back (`relocate_live`). Under
the write lock only the documents written meanwhile go in, as a data record
inside the image, whose header then takes the counter as it stands, and the
side file is renamed into place (`Sink::adopt`, through a primary's `Tee`
too). What they superseded stays in the new file, dead, for the next compact.
At 1.8 million x 128, a 1.1 GB file, writes waited at most 24 ms against 1.22
s, for a peak of +67 MB; cloning the segments rather than sharing them held
the read lock 508 ms and took +1.16 GB. One side file at a time; a collection
created, dropped or altered meanwhile fails it, and so does a named statement
defined or dropped; a scheduled statement's mark (kind 13, op 5) does not,
kept beside the statements' table rather than in it: the swap writes each
schedule's last mark as it stands then (`last_marks`;
`Statements::defines_as` asks the definitions alone,
[named-statements.md](named-statements.md#what-f3-has-built)).
Only a lone statement takes this path; a batch, the shell and `execute`
hold the lock.

### The graphs off the lock

**A maintenance reads no document from the file under the lock.** Until
this change a compact rebuilt each graph holding tombstones from its
documents' vectors read under the read lock (`begin_graphs`,
`Store::read_field` a document at a time), and `create index` read its
field so (`begin_index`) -- every release that maintained beside the
database, 0.1.5 to 0.2.0; #684 made the rewrite past the page cache fast
and left this phase as it stood. Past the page cache each read is a fault.
In the P6 soak's dry run 5 -- a server and its replica in containers of
512 MB, `--sync 250`, a 2 GB file of 1 GB live, 20 000 `docs` of a
`vector<16> @hnsw` among a million records -- each compact held its
server's read lock 5-6 s: its writers and its syncer waited behind it,
and `RwLock`, which holds a new reader behind a waiting writer, held
every reader too. On the primary `durable` stood still, so the replica,
which is sent only what is on disk, received nothing, and the reads
waiting on it for a write (`Fennec-After`, 5 s) were answered 504, 14 and
25 in the two compactions' minutes; on the replica the follower waited at
its write lock, and every read behind it. The log said 3.9-17.1 ms under
the write lock: the swap's alone. The graph phase runs before the
rewrite, so its faults were the docs' records pushed out by the million
records' reads and writes, not by the compact's own read.

Now the read lock takes only what is read from: the collection's store
as a pinned read takes it (`Store::pinned`, `maintenance::Taken`) -- the
records in the file and the segments shared, the id index copied, 12
bytes a row, 240 KB for the soak's docs, and the stretches of the file an
image writes from left out, which a read by id never asks for and a
mapped tail of single writes holds 16 bytes of a write (`Store::clone`,
which the first cut took, copied them) -- and the tail's token. Its id
index is charged to the pins' budget while it is held, as a pin's is
(`PIN_BUDGET`): a long read that would take the budget past it runs under
the lock meanwhile, and the maintenance itself never declines, since
reading under the lock is what held the soak's server. So it is held
while it is read, and no longer: a compact takes each graph's store and
token just before that graph's read (`begin_graph`, the graphs named
first by `graphs_rebuilt`) and lets the store go, its charge with it,
once the vectors are read, before the graph is built. The second cut
took every collection's at once, and a collection's charge stood through
the builds of every graph before its own -- minutes of them. The charge
is the id index's capacity, 12 to 24 bytes a row, against a budget of
256 MiB no flag changes: past 5.6 to 11 million rows, fewer with other
pins open, every long read of a collection whose graph was still to come
ran under the lock for that while -- the stall pins are there to
prevent. A probe with the budget at one and a half collections' id
indexes saw a pin admitted before the compact, declined during its read
and admitted after. Counted without declining pins, the charge would
have let the pins past the memory the budget bounds; held through the
read alone, it bounds it, and a pin is declined only while a graph's
vectors are read, never through a build. The
rewrite's stores are another matter: each is cloned whole, uncharged, as
the store the swap puts in place, held through the whole rewrite --
charged, it would decline long reads for as long. The values are
read from that store with no lock held (`IndexCopy::read`). A record
there never moves: the file is only appended to, a compact's swap renames
a new file over it while the store keeps its mapping (on Windows its
views), and a write to a segment it shares copies it first
(`Arc::make_mut`) -- read into memory alike. The graph is built, then
caught up with the writes made meanwhile in rounds (`catch_up_graphs`),
as the rewrite's are copied: a round's ids drained from every tail at
once, their documents looked up 4 096 at a time under the read lock
(`Store::snapshot`) and read after it (`store::field_at`), each linked
with no lock held -- taken out and put back for a compact, as
`finish_graphs` took them, and for `create index` a node holding the
vector already kept, as `finish_index` keeps it. A round whose links cost
less than 64 at 768 dimensions (`GRAPH_DONE`) ends them, 16 at most. A
chunk that may take the graph past the nodes a graph holds
(`vector::MAX_NODES`, each write counted as a node: `past_nodes`) links
none of it, and the graph is refused (`Built::Refused`) -- a compact
keeps it as it stands, `create index` is refused naming the field -- as
the write lock refuses the last round's ([crates.md,
limits](crates.md#limits)); linked, its last node met `alloc_links`'
assert.

**The write lock links what 128 links at 768 dimensions cost, at most**
(`GRAPH_HELD`), the graphs' together: what was written during the last
round. A link costs about its dimensions and 64 more, times its
`ef_construction` over 200 and its `m` over 16 (`link_cost`), so that is
128 links at 768, 66 at 1 536, 33 at 3 072 and 1 331 at 16 at the
defaults, and at 768 dimensions 64 at `ef_construction` 400, 32 at 800,
32 at `m` 32 with 400 and 10 at `m` 48 with 800 (`held_links`). A single link took, at the median into a graph of 20 000
vectors (`VectorIndex::insert` timed alone, a document written over and a
new one a thousand times each, one core of an M-series Mac, three runs a
dimension, each begun under a load below 4), 0.08 ms at 16 dimensions,
0.15 at 128, 0.38 at 384, 0.82-0.89 at 768, 1.80 at 1 536 and 3.7 at
3 072, and 1.14 ms into one of 100 000 x 768 (the half millisecond a
vector a 100 000 x 768 build takes is its eight cores' wall time, not a
link's). The model is over by a fifth at 128 and 384 and under by as much
at 1 536 and 3 072, where each dimension costs a little more, so the
bound holds every writer 85-125 ms into 20 000 vectors at any of those
dimensions, about a third more into 100 000 -- under the 0.19-0.31 s the
rewrite's copy holds the soak's file. The second cut counted links alone,
128 whatever the dimensions: 230 ms at 1 536, 470 at 3 072 and 10 at 16.
The third counted dimensions alone, whatever the graph's settings, and an
insert searches every layer with a beam of `ef_construction`, measuring
each node's neighbours, up to `m` of them: into 20 000 x 768, one core,
release build, the median link of 600 took 0.91 ms at `m` 16 and
`ef_construction` 200, 1.47 at 16 and 400, 2.34 at 16 and 800, 3.65 at
32 and 400 and 7.87 at 48 and 800 -- 1.6, 2.6, 4.0 and 8.6 times the
default's, so 128 links held every writer 1.0 s at 48 and 800. The
product of the two ratios puts them at 2, 4, 4 and 12: a hair under at
32 and 400 (4.0 against 4.01), over the rest by 24-56%, so the bound
holds every writer 75-117 ms at each, against 116 at the defaults. Below
the defaults a link is counted at theirs, never less, as nothing was
timed there; past those timed the product is the guess, and `m`'s pruning
may cost it more than linearly where it is wide and the beam narrow. The
cost and its products are `u64`s, saturating: a 32-bit `usize` -- armv7
Android compacts beside its database too -- wrapped past 5.2 million ids
at 768 dimensions, and a round past it read as cheap. A graph whose one
link costs more than the whole bound -- at 768 dimensions `m` times
`ef_construction` past 409 600, `m` 16 with an `ef_construction` past
25 600 -- links none under the write lock: any vector written during its
last round keeps it, or refuses its `create index`.
Linking the last ones as a batch (`insert_batch`) instead was not taken:
its nodes do not see one another, so it is not the graph the rounds and
the read lock link in turn, and it is faster only by the cores a server
has. Where the links keep up with the vectors written, each round costs
less than the one before and the last leaves less than 64's worth; twice
that is room for a burst, and past it the writes outran the rounds. With
no bound the write lock linked whatever the 16th round left, without end
where they did. Past it a compact keeps the graph as it stands,
tombstones and all, for the next compact, and says so (`KeptGraph`, below);
`create index` is refused, its words saying why (`fell_behind`). A kept
graph misses no write: it is the live graph, which every write since the
copy reached as it landed. During a sustained ingest of vectors -- the
rounds link on one core, about 1 000 a second at 768 dimensions -- every
compact may keep it, and its tombstones grow until a quieter one.

**A rewrite's last copy has no such bound.** Its rounds shrink where a
write cost its writer more under the write lock than a round's copy of it
costs with none, and the write lock copies what was written during the
last round and during what follows it before the swap. This section said
the margin was the write's parse and encoding; but a server builds and
encodes a block's `put`s of `PREPARE_AT` (16) documents or more before the
write lock, a `POST /<name>`'s, `/query`'s or `/batch`'s
(`Database::prepare`, fennec-http's `early_put`), and asked whether into a
collection with no index such a write costs the lock no more than its
copy, so that the rounds need not shrink at all, it was measured
(`bulkhold`, a scratch program over `Database::maintain_held`: the soak's
2 GB file in `docker run --cpus 3`, its page cache dropped, each run begun
under a 1-minute load below 4, the rounds and the handover timed by a
patch that changes nothing else; beside the compact a writer putting
1 000 new 1 KB rows a block into the million records' collection, which
has no index but the id, built and encoded under the read lock and
landed under the write lock as a server lands them):

| writer | memory | rounds' rows | handover | the swap's rows | the swap |
|---|---|---|---|---|---|
| 10 000 rows/s | 512 MB | 119 000, 4 000, 0 | 20 ms | 0 | 6.6 ms |
| 20 000 rows/s | 512 MB | 207 000, 21 000, 0 | 184 ms | 4 000 | 25.6 ms |
| 30 000 rows/s | 2 GB | 257-362 000, 28-75 000, 3-16 000, ... | 48 ms | 2 000-4 000 | 10-29 ms |
| as fast as it went, 43 000-50 000 rows/s | 2 GB | 0.91-1.06 M, 259-299 000, 66-82 000, 17-24 000, 3-5 000, ... | 9.2-10.6 s | 549 000-621 000 | 3.8-6.4 s |
| as fast as it went | 512 MB | 666 000, then killed for memory | | | |

One run each at 10 000 and 20 000 rows a second and in 512 MB as fast as
it went, two at 30 000 and three as fast as it went in 2 GB, a run begun
under a load of 13 left out. The 1 000-row rounds after the shrinking
ones are a block a round, which `CATCH_UP_DONE` (1 000) does not end, so
the 16 rounds run out, at 10 to 45 ms each.

The rounds shrink: each copied a third of the one before or less, a
write still costing its writer under the lock several times its copy --
appended, put in the id index and noted -- with its documents built
before it. What does not shrink is the handover after them
(`Beside::hand_over`): no lock held, it points each store at the rows the
rounds copied into the side file, which the stores hold in memory until
then -- 1.46-1.70 GB of anonymous memory at the peak in the fastest runs,
and in 512 MB the first round's 666 000 rows were the end of the process.
Every write made during it the swap copies under the write lock: 9-11 s
of a writer's pace became 3.8-6.4 s with every writer and reader held
(the writer's own longest hold, under the same memory, was 3.3-6.3 s).
So it was on main too. A bound needs a design of its own: a compact
cannot keep the old file as it keeps a graph, and refused past a bound it
would be refused at every try beside a steady ingest, the file growing.
Handing the copies over round by round, which would bound both the
memory they hold and what the swap is left, is the likelier way, and is
left for later. A link
costs as much as the one the write that made it paid, and a block's puts
link on every core (`link_batch`), so a graph's rounds may not shrink at
all. A document a round cannot look up or read leaves a compact's graph as it
stood, as one the copy cannot read does, and refuses `create index`
(`BuiltIndex::broken`); one the rewrite cannot look up fails the compact,
whose new file would be without it. An index over several fields reads
its keys from a pinned store too; the other indexes' builds take
milliseconds and catch up under the write lock as before, each write a
read of a record just written.

**A compact of several collections asks every one before it places any
graph.** Asked as each was placed, a compact whose second collection was
dropped, altered or given an index by a second `create index` meanwhile
put the first's rebuilt graph in place and then refused -- and a round
had already drained the first's tail before it met the second's change,
so the graph lacked a write: a probe's held document 200's old vector,
which the graph keeper would then have saved. Every tail is drained
(`drain` hands back each, `None` for a changed one), a round meeting a
change ends the rounds, and `finish_graphs` refuses before it places
anything.

The graph kept is the one the read lock built, link for link, where the
writes made meanwhile land in one round; one written in two rounds leaves
a tombstone more, for the next compact. `maintenance::tests` hold it: a
writer gets through mid-read and between a round's lookups and its links
(`try_write` in a test seam, which fails while the thread holds the read
lock -- the read moved back under the lock fails it), a hundred documents
written over, one deleted and one made reach the graph, byte for byte the
graph the old way builds on the same seed (the copy as it stood linked as
a batch, the writes in id order after it), every live document's vector
and no other, as a rebuild at the end holds them -- in memory and over a
mapped file, its documents in the file and its writes since in memory;
with no write, a compact beside the database rebuilds the graph a compact
under the lock does, byte for byte. A document written in two rounds
holds its last vector; one written again unchanged keeps one node, and
one written with another's vector joins that node, as the read lock
built them; a block put back meanwhile reaches no graph; a second `create
index` beside the first refuses it, placing nothing; a compact of two
collections, the second dropped, altered or indexed once its graph's
store was taken, places no graph and leaves the first's as the writes
left it (asked one at a time, it fails), dropped before is refused as its
store is taken, and altered before both are rebuilt; a document a round
cannot read, or cannot look up (`Store::snapshot` refusing it), keeps the
graph and names it, the other collection's rebuilt; at 1 613 left at 2
dimensions a compact places its graph and `create index` its index, at
1 614 the compact keeps the graph as the writes left it, naming it, and
`create index` is refused -- at 201 and 202 so with `ef_construction`
1 600, and at 100 and 101 with `m` 32 too, `held_links` asked at the
settings timed, below the defaults, past the whole bound and at the widest
a schema takes, and a cost of `usize::MAX` links saturating; of two
collections left 900 each, the first's
graph is placed and the second's kept, the bound spent across them; and a
compact of three collections holds one collection's charge on the pins'
budget at each graph's read and none in its rounds, a long read pinned
beside each under a budget the three together would pass -- at the
second cut's head the first read held all three and the pin was
declined.

Every hold is timed (`Holds`, one `Instant` a hold) and handed back by the
maintenance that held them (`Database::maintain_held`,
`compact_when_due_in`): read off the database after it, as the first cut's
`last_holds` was, a `create index` finishing between gave its holds for
the compact's. The build's copy -- a compact's longest, one a graph --
its rounds' longest and its put in place, and a compact's file's copy,
rounds' longest and swap. With them the graphs a compact kept as they
stood (`Holds::kept`, each a `KeptGraph`: its collection, field and why,
`KeptFor` -- the writes its last round left, a document unread, or its
writes past the nodes a graph holds), since
nothing else says one was kept: the answer to `compact` carries no holds,
and a kept graph answers as it did. `--auto-compact`'s line and the one a
`compact` or `create index` sent as a statement writes are one shape
(`link::said_beside`): `<what>: <done> in <took>, the lock held <longest>
at most (<each hold>)`, then ``; graph `<collection>.<field>` kept, <n>
behind`` (or `a document unread`, or `its writes would pass 4294967295
nodes`) for each graph kept, `--auto-compact`
naming the file or tenant, a statement itself and its request id after.

*Measured* (`graphhold`, a scratch program over `Database::maintain`:
the soak's shape -- a million 1 KB records each written over once at a
random key and the 20 000 docs 50 000 times, 2 088 MB of 1 028 live, the
graph 50 000 tombstones -- opened in `docker run --memory 512m --cpus 3`
after the VM's page cache was dropped, beside it a writer (95% records,
5% docs) and a reader by id, each at a steady rate and its latency taken
from when its op was due, and a syncer every 250 ms, as `--sync 250`; a
compact begun 20 s in; main, its holds timed by a patch that changes
nothing else, against this change, in ABBA order, each run started under
a 1-minute load of 4). At 300 writes and 600 reads a second, which the
container keeps up with, six runs a side:

| | main | this change |
|---|---|---|
| the build's copy (read lock) | 933-2 474 ms | 12.5-27.8 ms |
| its rounds' longest (read lock) | -- | 0.9-3.1 ms |
| its put in place (write lock) | 9.8-21.8 ms | 6.0-12.5 ms |
| the file's copy (read lock) | 208-307 ms | 191-244 ms |
| the file's rounds' longest (read lock) | 55-124 ms | 60-163 ms |
| the swap (write lock) | 1.9-3.6 ms | 2.5-6.5 ms |
| a writer's longest wait for the lock | 0.93-2.47 s (median 2.33) | 0.19-1.14 s (0.50) |
| a reader's longest wait | 0.93-2.78 s (2.33) | 0.24-0.43 s (0.28) |
| the syncer's longest wait | 0.69-2.24 s (2.08) | 0.09-0.84 s (0.45) |
| a write's latency at most | 1.16-4.49 s (3.12) | 0.23-2.47 s (1.30) |
| the compact | 11.9-17.0 s (14.3) | 12.1-20.3 s (16.7) |
| peak anonymous memory | 125-143 MB | 133-138 MB |

At 1 500 and 1 500 a second, past what the container keeps up with (four
runs a side): the build's copy 1 020-2 881 ms against 12.8-16.7, a
writer's longest wait 1.02-2.88 s against 0.36-0.73, a reader's 1.16-3.03
against 0.36-0.72, the compact 14.4-20.2 s against 16.4-22.6, with 6 100-11
050 writes during it against 9 800-13 370.

What waits now is the workload's own: a read or a write that faults
under the lock holds it as long. Outside any compact the same runs'
writer waited up to 0.07-0.75 s on either side, and held the lock itself
up to 15-91 ms (74-193 during a compact, which reads the file beside it);
the syncer held it 0.1 ms at most. The compact takes a little longer by
what it no longer stops: the writes and reads go on beside its reads, and
it reads beside their faults. The file's copy -- `begin_beside` cloning
the million records' id index, 12 MB, under the read lock in a container
short of memory -- is now the longest hold, 0.19-0.31 s, on main as here;
it is the next to take off the lock.

Taken as a pinned read takes it rather than cloned whole, the build's
store holds the read lock 0.8-2.6 ms where the clone held it 13.0-18.2:
the same runs, four a side against the clone, in ABBA and then BAAB
order, each begun under a 1-minute load of 2.5 to 4 -- the clone copied
the docs' stretches of the file as well as their id index. The rest moved
with the runs alone: the rounds' longest 1.2-1.8 ms against 0.7-1.2, the
put in place 6.0-10.5 against 6.3-8.4, the file's copy 186-244 ms against
191-229, the swap 3.7-6.0 against 3.0-3.6, the compact 12.1-20.0 s
against 13.1-16.4, a writer's longest wait 0.18-1.25 s against 0.59-1.71
and a reader's 0.24-0.91 against 0.25-0.79 -- the workload's own faults
and the file's copy -- and peak anonymous memory 133-139 MB against
128-143. Nothing on a write's path moved:
copies of the gate's `core-put` and `file-open`, counted over 16
identities a build against main (`make instr-compare`'s means), read -40
instructions an op (standard error 32) and +0.26% (standard error 0.74%),
each inside two standard errors; the first cut read +44 (23) and -0.56%,
a compact beside the database of 5 000 vectors of 8 -0.07%, and a traced
`put` ran 12 instructions more of its own code, in `HashIndex::add`, which
this change does not touch.

## Compaction

**A file compacts itself once half of it is dead** (`engine/garbage.rs`).
An update appends the whole record and leaves the version it replaced dead
until a `compact`, which nothing ran on its own: under YCSB's updates a
million 1 KB records grew a buffered file to 4.7-6.5 GB on an 8 GB machine,
a live record a few to a page among dead ones, and one read in five waited
for the disk (C at one thread 30.1k ops/s, 443k over the file freshly
loaded). `Database::garbage` is the file's bytes as written (`appended`)
against the live records' (`Store::live_bytes`, `total_bytes - dead_bytes`,
which the stores counted as writes landed already and an image's index
carries: [whole frames](#dead-bytes-are-whole-frames)) and
what the last compact kept besides the documents (`kept`: schemas,
counters, graphs, the file less every record the stores hold -- the
versions a compact beside the writes copied and saw written over are dead,
and counted as kept they left a 307 KB file of 53 KB of rows not due
again; and a graph's record leaves it once a rebuild drops the graph it
holds, [worn graphs](hnsw.md#worn-graphs)) -- a sum over the collections, nothing added to the write
path: a lone put, put over and del measured 570/700/532 ns in memory either
way, and 666/797/595 against 660/785/593 over a mapped file, inside the base
build's own spread. `CompactPolicy` is the one rule (half the file dead and
64 MB, `compact_due`), `compact_when_due` a look under the read lock and a
compact beside the database, `Compactor` a thread that looks every 5 s;
what lets the database go -- the `Compactor`'s drop, a tenant's release, a
shutdown -- stops a host's compact in its graph phase, writing nothing
(`compact_when_due_until`, [worn graphs](hnsw.md#worn-graphs)). The
look waits for a write under way: under `try_read` 10 looks in 12 runs of
`make compact-bench` found the lock taken and put a due compact off 5 s,
the file growing 100-160 MB a second meanwhile.
`fennec_http::link` runs one thread over every database the process serves
(`--auto-compact <ratio>|off`, default 0.5), apart from the graph keeper so
its graphs do not wait out a compact of a gigabyte; the native library
starts a `Compactor` a file (`FENNEC_OPEN_NO_AUTO_COMPACT`); a replica and a
following tenant compact too, since `may_write` lets a compact run there and
their files grow with every update sent, and an image adopted during a
maintenance marks every tail changed (`Tails::all_changed`), or a replica's
compact put back what it copied before the image. The browser has no thread:
`snapshot` writes the live records alone, so a page's persisted image drops
them, and its memory keeps them until it reloads. The swap was the cost:
the writes made during a 4-6 s compact of a 1 GB file, 200 000 records, were
copied under the write lock (409-811 ms); they are copied in rounds before
it, the ids drained from the tails at once and their documents looked up
4 096 at a time under the read lock and copied after it (`Store::snapshot`:
a payload in the file by its place, which an append never moves, one in
memory copied out; 95-245 us held at the median, 3.1 ms at most) -- copied
under it, one round of 408 708 held the readers 592 ms behind the waiting
writer, and with its pages touched first a writer at 170-180k updates a
second ran at 1.5-37k through the worst 100 ms of a 1.2 GB file's compact,
83-144k now -- until a round finds fewer than 1 000. The rounds' frames are
handed over to the side file before the swap (`Beside::hand_over`; left to
the first write after it, 400 MB under the write lock, 158 ms), every page
of the new file is asked for 16 MB at a time ahead of its reads and read in
before it (`fs::touch`, `MADV_WILLNEED`, Windows' `PrefetchVirtualMemory`: a fault at a time a 1.2 GB file
took 3.0-3.3 s on this 8 GB Mac, the writer beside it at 11-15k, and its
pages were out of memory again by the swap, the 2 s after it at 21-39k;
asked for, 0.6-0.8 s and 90-150k; left to the readers, the read p50 went 2
-> 100 us for two seconds), and the stores it replaces are dropped after the
lock (their unmapping was 10-15 ms of it). Under the lock: the last few ids,
the side file's fsync and the rename, 6.1-12.6 ms over a 1 GB file and
13.7-15.7 durable (`make compact-bench`: 5M updates of a field over a
million 1 KB records from one writer, buffered, the file 1.03 -> 2.1-3.0 ->
1.1-1.6 GB three or four times where it reached 6.2 GB without, and after
the updates 435k reads/s from the first second where the 6.2 GB file
started at 19k and took 25 s to 400k). Over nine runs each in turns, the
median 100 ms of a compact went 27.1k -> 30.6k, its worst 1.1k -> 3.6k, a
compact 10.9 -> 7.8 s, the reads during it p99 2.3-6.5 -> 1.3-5.3 ms; at
250 000 records, whose file fits, 143k -> 153k, the worst 16.9k -> 129k
against 180k with none, a compact 1.0 -> 0.3 s, the reads p99 0.5-1.3 ->
0.14-0.35 ms and 52 -> 18 ms at most. Paced to half and to a quarter of its
speed a compact left the writer where it was (30-35k) and took 56 and 95 s
against 10; at utility QoS its reads were throttled behind the writer's and
it took 138 s; with its side file uncached (`F_NOCACHE`) in a compact that
began once the file had outgrown the cache (below), or at utility QoS for
the CPU alone, nothing moved. A database
read into memory compacts beside itself too, the live records copied into
fresh segments with no lock held; it used to copy them under the read lock
and build every index again, though the indexes are keyed by id.

A compact walks a collection's ids four times -- `Store::live_len` for the
data record's length, `image_index`, which asks `live_len` again, and
`write_live` -- and each takes every id's location from the walk itself
(`IdIndex::walk`), as `Store::compacted` and the copy in turn's buckets
do; `relocate_live` and the copy's `places` walk the dense slots
themselves. Each walk took the ids alone and looked each one up again: 64
instructions a row of the perf gate's `file-compact` as CI counts it
(x86_64 musl), 36 of them `live_len`'s, 12 `image_index`'s and 16
`write_live`'s. `file-compact` went 7.74 -> 7.10 million (-8.3%; aarch64
-7.4%), `core-lookup` 0.9% under main (aarch64 0.1%), and no other
workload above main's by more than 2 instructions an op. The walk's
`next` walks the dense slots and nothing else while they last, the
sparse ids asked for out of line (`Apart::next`), and the walks that give
the ids alone (`Store::iter_ids`, `ids`) are the same walk.
`store::tests::the_walk_gives_each_id_its_location_in_the_order_ids_came`
holds it to the walk before, a lookup an id, and a program writing one
file through checkpoints and each kind of compact -- under the lock,
beside the database, in turn -- wrote main's files, byte for byte. How
the sparse part hands its pair back moved the walks by as much as the
lookups: a `Loc` by value came back through memory, and every caller's
loop stored each dense slot's location there too (-7.1%, aarch64 -6.2%);
handed the walk's parts rather than its place, the slots stayed in
registers that x86_64's loops then spilled (-6.6%, aarch64 -7.1%); by
reference the pair comes back in registers. The code shrank: built as
CI's build job builds them, `.text` lost 5 568 bytes of `fennec`, 2 880
of `fennec-server` and 5 536 of `make small`'s, each file 4 096 bytes
smaller; the browser module grew 161 bytes raw, its gzip and brotli
inside the noise (+3, +28).

### Dead bytes are whole frames

A version written over or deleted adds its whole frame to `dead_bytes`,
head and payload, the head's length worked out from the id and the
payload's length as `put_uvarint` writes them, each the shortest
(`store::dead_frame`). So `Store::live_bytes`, `total_bytes - dead_bytes`,
is the live frames to the byte -- what a compact writes of the documents,
`Garbage::live`, and `fennec_data_bytes` (`CollectionStats::live_bytes`),
read at a scrape with no walk of the index: through appends, a block put
back, the walks a load makes in memory and in the file and an image's
index (`store::tests::the_live_bytes_are_the_live_frames_to_the_byte`),
and over HTTP against what a compact keeps (fennec-server's
`the_data_bytes_are_the_live_records`). Through 0.2.0 a version written
over or deleted left its payload's length alone, its head of 3 bytes or
more counted live -- that test's 250 rows of 3 409 bytes, 200 of them
written over three times and 50 deleted, read as 5 628 -- and
`fennec_data_bytes` (`fenec_data_bytes` from 0.1.5 to 0.1.12) said
`CollectionStats::bytes`, every record dead or alive, where its help and
the monitoring page said the live ones: a P6 soak's server said 1 907 MiB
of a 1 950 MiB file just before it compacted at half dead, and the page's
alert, `dead / (data + dead) > 0.5`, could not fire.
`CollectionStats::bytes` keeps its meaning, every record's bytes, which
the browser module's `stats()` answers as `bytes` and @fennecdb/cloudflare
reads as what the module holds (`held`); the shell's `.stats` says the
live and the dead apart, as the studio's sidebar does.

An image an older build wrote carries the payload count in its index,
which an open takes (`adopt_index`): such a file's live bytes are high by
its dead versions' heads until its next compact, where a walk of the frames
counts them whole. `Store::live_len` still walks the index for the length
`write_live` writes: a file's frame may hold a varint longer than the
shortest, which no writer here makes and `dead_frame` would count short,
and a length that is off makes the record after the data one unreadable.
What it costs: a put over a row runs 14 instructions more of fennec's own
code, all in `Store::append_onto` (the 530th put of `replays` traced by
function against main, `make instr-compare TRACE=fennec_http::handle`:
9 104 -> 9 118 of the request's 17 983). Over eight identities a side,
`put` read +57 instructions a request (+0.27%) and `batch`, 100 rows
written over a request, +2 611 (+0.26%), each inside two standard errors.
A varint's length is protobuf's `(log2 * 9 + 73) / 64`
(`store::varint_len`), an add and a shift: with `div_ceil(7)`, which
divides, the put's own code was 29 more, and `after`, which an image's
index asks once a document as it is written and as an open takes it, is
cheaper by as much. The browser module grew 91 bytes, `dead_frame` held
once there (`inline(never)`), where a copy in each of its three callers
grew it 141; on aarch64 macOS fennec-server's code grew 128 bytes and
`fennec`'s shrank 128, both files the same size to the byte. CI's perf
job holds the cost.

### Dropped places

**A compact keeps every place a collection's documents hold.** `alter
collection x drop field b` leaves `b`'s place in the documents
([fennecql.md](fennecql.md#alter-collection)), and every write after it
puts a one-byte `null` there, or nothing where no field follows it. A
compact makes the value at each dropped place that `null`, and leaves out
the dropped places a document ends with, as a write does; every other
value is copied as its bytes stand, at the place it stood
(`schema::reclaim`). It renumbers nothing: the schema's `dropped` list
stays for the collection's life, in memory, in the file and on every copy.
The reclaim rides on the copy every compact makes of the live records: in
memory into the fresh segments (`Store::compacted`), and over a mapped
file by writing again, into the store's segments, only the documents
holding a value at a dropped place, before the rewrite streams the live
records into the new file and points the store at it (`Store::reclaim`,
`relocate_live`). It does so under the lock and beside the database alike,
so a compact over a collection holding a dropped place runs beside the
database as every other compact does; one used to hold the write lock
throughout. The store notes the places it reclaimed (`Store::cleared`, in
memory alone): the compacts after the first walk no document for them, as
none walked once the places were taken out, until a drop adds a place or
the process starts again.

*The two bugs.* A compact took the places out (`strip_dropped`) from 0.1.7,
the release that brought `drop field`, to 0.2.0, and emptied the list.
That went wrong two ways. *Refused part way:* it stripped one collection
at a time, in memory as it went, so a document no decoder reads in a later
collection -- a damaged value the open let through (read unchecked below
a claim), or, once the depth was bounded after 0.2.0, a value nesting past
`codec::MAX_DEPTH` that 0.2.0 wrote through a `[[json]]` field
([crates.md](crates.md#how-deep-a-value-nests)) -- refused it with the
earlier collections renumbered in memory and the file not rewritten. The
writes after it were encoded by the renumbered places: a later `put x {a:
10, c: 30, d: 40}` read `[10, 30, 40]` in the process and `[10, 40, null]`
after a reopen and on a replica. The server's own compaction (`--auto-compact`,
from 0.1.10) met the same refusal every five seconds, logging it each
time. *Succeeded, telling no copy:* a compact
appends no record. A copy caught up before it kept the places, and read
every later write of the collection one field early. A copy that
compacted itself first (`may_write` lets a following database compact, and
a server's compaction runs on one) read its primary's later writes one
field late, `[10, null, 30]`. An archive restored from an image taken
before the compact, with the writes after it, misread them, and so did
every image the archive took after. A `--dir` node's standby misread them,
and after a failover the misread values were the tenant's. Nothing was
loud until the primary's next `create index`, whose record carries the
whole schema and failed the copy's layout check (`Database::apply`). A
copy now reads each write as it was made, and refuses one made by other
places than it holds ([replication.md](replication.md#a-copy-across-a-compact)).

*What a compact meets that it cannot read.* A compact is refused for
nothing a document holds, and fails only as storage fails:

- The reclaim reads a document as far as its last dropped place, a value
  at a time (`skip_value`). One it cannot read that far is copied as it
  is, and is refused where it is read, in the same words, after the compact
  as before.
- A graph holding tombstones is built again from every document's vector
  (`build_graph` under the lock, `begin_graph` and `finish_graphs` beside
  the database). A document it cannot read leaves the graph as it stands,
  tombstones and all, answering as it did; a graph built without the
  document would not. No file brings such a document to it today: an open
  reads every vector a graph holds, past every value before it, and
  refuses the file, as `create index`, a write and a replica's apply do
  (`compact_places::a_vector_past_a_value_none_reads_is_refused_at_the_open`).
  So the engine's own test puts one in a store
  (`maintenance::tests::a_graph_a_compact_cannot_rebuild_stays_as_it_stood`),
  where each path used to refuse the compact: under the lock part way, the
  collections before it compacted.
- Nothing else in a compact decodes a document. The rewrite copies payloads
  by their locations, whose one refusal is a location outside its segment
  or the mapped file: a damaged store, which every read of that row meets
  too, and which fails the database (below).

The browser module's compact is `execute`'s, under the lock. The copy it
built beside the page, and every index over it built again
(`begin_compact`, `finish_compact`), refused a compact for a document any
index could not read. Nothing reached it, since fennec-wasm calls no
`maintain`, and it is gone.

*Why places are never renumbered.* A copy applies its primary's records by
the places it holds, and a compact is no record. The ways rejected:

- A record that carries the strip to every copy. It costs nothing a write,
  but every copy then runs a full rewrite as it applies it, an open must
  replay it idempotently, and a copy must never strip on its own. It is a
  new record kind besides, which an older copy refuses.
- A layout number in every write: bytes on every write, and a new kind of
  data record.
- Seeding every copy again after a stripping compact. That sends an image
  to each copy, and leaves a window while standbys are seeding, which a
  failover inside it would lose.
- The first fix (branch `compact-refused-midway`): every stripped store,
  and every graph, built before any is put in place. It mended the refusal
  part way and left the copies misreading. It also held the data twice at
  the peak, every rebuilt graph a second time, and in the browser that
  peak is never given back.

Kept places mend both bugs with no format change: 0.2.0 reads every file
this writes.

*What it costs.* A byte a document for each dropped place a field
follows, for the collection's life, and a value every read of such a field
passes. Counted (`crates/fennec-core/examples/places.rs`, macOS, the
instructions an op over 8 identities a build, 7 runs each,
[benchmarks.md](benchmarks.md#instruction-counts)) against main's build
over the same rows stripped as its compact left them -- `x (a int, b text,
c int, d text)`, 100 000 rows read into memory, `b` dropped and the
collection compacted:

| | stripped (main) | kept (this) | |
|---|---|---|---|
| bytes a document, 1 / 4 / 16 text fields dropped | 43.04 | 44.04 / 47.04 / 59.04 | +1 a place |
| a get by id | 13 419 | 13 446 | inside the draw |
| a scan by `c` with a limit, every row read | 43.92 M | 45.75 M | +4.17%, 18 a row |
| a plain `get` of 100 rows as JSON | 234 082 | 238 783 | +2.01%, 47 a row |
| a put over a row | 11 959 | 12 038 | +79, +0.66% |

A read passing a dropped place as any value calls the skip for it, 34
instructions of its frame and its match and 10 of the loop's by a trace:
the scan paid 44 a row (+8.7%). A `null` passed by its byte
(`codec::skip_passing`) is a compare and an add. A filter over a
collection holding a dropped place before a field it reads reads its
fields out of line with its paths (`Store::read_fields_passing`, a second
copy of the reader), through the branch every filter takes for its paths
already: 18 a row. `Schema::read_doc` passes only dropped places in its
skip, so it passes them so at no cost to another collection: a put's read
of the row it writes over took the put from +119 to +53 and +79 in two
counts, the rest the
`null` it writes, each field's place worked out and a byte more framed and
copied. Asked anywhere every collection's rows go
through -- in the reader's loop, or in the JSON writer's, where a page of
another collection paid 150 instructions held as a bool across its rows
and 518 asked of the collection -- the test cost every other collection's
reads, so the JSON writer passes a dropped place as any value. With no
dropped place, the same reads and the put counted inside the draw of
main's (-81, -403, +33, -12). A compact:

| | stripped (main) | kept (this) | |
|---|---|---|---|
| `x` beside the database, a tenth of its rows written over, after a compact in the same process | 81.12 M | 81.49 M | +0.46%: a byte more a row to copy |
| the same, the first after an open | 82.87 M | 103.82 M | +25.3%: the walk, 209 a row |
| the perf gate's `file-compact`, no dropped place | 10.50 M | 10.51 M | inside the draw; `p`'s too (+77 of 81.09 M) |
| two collections a field dropped each, 2 x 500 000 rows mapped, under the lock, peak footprint above the open | 2 480 M, 44.8 MB | 2 053 M, 37.7 MB | the first after the drop |
| the same, 2 x 200 000 rows read into memory | 729 M, 7.3 MB | 570 M, 7.2 MB | |

The first compact after a drop writes the documents holding a value at the
place again, where the strip wrote every document again. The first after
an open walks every document of a collection holding a dropped place, a
value at a time as far as its last one, copying nothing where it finds
only `null`s (`schema::reclaims`); building the copy to compare took that
compact to +47%; walked, +25%. Beside the database the walk holds no
lock: the server's compaction and the native library's `Compactor`, and a
`compact` a server or the native library is sent alone
(`Database::maintain`). The shell's `compact`, and `Database::compact`
wherever a program calls it, walks under the write lock. The browser
module has no walk: its compact, under the lock, builds every document of
such a collection again as the reclaim would leave it (`schema::reclaim`
in `Store::compacted`), kept where it changed, once after a drop or an
open.
The store notes the places it reclaimed, so the compacts
after it walk nothing; an open forgets them, the note being in no file.

*Aggregates, orders, and what an open reads.* An aggregate's fields and
its group keys, and an order's keys, passed a dropped place as any value
too, which this change's review found; counted as above, over
`x (a int, b text, k int, v int)`, 100 000 rows in 100 groups by `k`, `b`
dropped and compacted, and over `x`'s rows ordered by `c`:

| | stripped (main) | kept, read as any value | kept, read passing | |
|---|---|---|---|---|
| `get x select k, count(*), sum(v) group k` | 90.25 M | 94.66 M | 91.46 M | +1.33%, 12 a row (passed as any value: +4.88%, 44) |
| the same, no field ever dropped (`p`) | 90.25 M | 90.26 M | 90.26 M | +134, inside 2 SE (±6 055) |
| `get x order c limit 10` | 60.68 M | 66.28 M | 60.28 M | -0.66%, -4 a row (passed as any value: +9.23%, 56) |
| the same, `p` | 60.68 M | 60.68 M | 60.68 M | +3, inside 2 SE (±2 399) |

Each now picks its reader once a query, as a filter does. The aggregate's
(`aggregate::Reader::passing`) calls `Store::read_fields_passing` where a
dropped place comes before a field it reads: a flag asked a row, which a
flag in place of the lengths of its paths and computed values pays for
(`Reader::more`). Out of line in a wrapper of its own, the second reader
cost 34 instructions a row by a trace, all a passed `null` saves. The rest
of the aggregate's 12 a row is the compare a passed value of any other
kind pays, and the payload a byte longer. An order picks at its call
(`order_ids_of`): `order_ids_passing`, a copy of its own out of line, or
`order_ids` as it was. Any test or copy beside the loop inside `order_ids`
-- a second loop, a generic copy, the original inlined beside a call out
of line -- moved its registers and took 1 to 11 instructions a row from
every other collection's order. The passing copy inlines its reader
(`Store::read_field_in`), a call a row fewer than `order_ids` makes of
`read_field`, and counts below main.

Every other reader of a field past a dropped place passes it as any value,
about 34 instructions a row a place: a page's columns (as many rows as its
limit), a facet's field, a `lookup`'s key, and what is read once a process
-- each vector an open copies into a graph's arena (`Store::field_at`),
and the field an index is built from at the first read after an open or a
server's warm-up. Counted once a process, plainly, three runs a side
(millions against the allocator's hundreds):

| | stripped (main) | kept | |
|---|---|---|---|
| a hash index of `c int @hash` built from 100 000 documents (`Database::warm`) | 131.39 M | 134.35 M | +2.26%, 30 a row |
| the same, `p` | 127.70 M | 127.66 M | -0.03% |
| the open, read into memory, of 100 000 rows `x (a int, b text, v vector<8> @hnsw(cosine))` checkpointed with its graph | 123.93 M | 130.22 M | +5.07%, 63 a row |
| the same, `p` | 125.11 M | 125.07 M | -0.03% |

Each is paid once a process, a few per cent of what that build or open
costs whole, so neither keeps a second copy of its reader: a server warms
an index once, and opens a file once. A `--dir` node pays it again at each
open of a tenant it had closed, a collection's index or graph at a time.

*How many places.* A collection holds `schema::MAX_PLACES`, 4 096, over
its life: its fields and its dropped ones. An `alter ... add field` past
them is refused, and so is a `create` declaring more fields, a declared
schema too. The refusal says to make the collection again -- create
another and copy the rows into it -- which is rid of the places, or,
where the fields alone fill them, that making it again keeps them.
PostgreSQL's dropped columns count toward
its 1 600 the same way; 4 096 is MySQL's column count, past SQLite's
default 2 000, so a table `fennec import` copies fits. A file holding more
places -- 0.2.0 had no bound -- opens as it is (`Schema::decode` counts
none): no read refuses what a write took. Its collection takes every alter
that adds no place, a rename and a drop: the count is held where
`Schema::new` asks for it -- a `create`, a declared schema, an import --
and on an add (`places_full`); an alter's check of the fields it leaves
counts none (`Schema::checked`). A rename first went through
`Schema::new`, which refused it on a collection of 4 097 fields ("declares
4097 fields") though it adds no place
(`compact_places::a_collection_past_the_bound_takes_every_alter_but_an_add`,
a schema of 4 097 fields made whole, as 0.2.0's parser made one, and one of
4 094 and four dropped places). A copy takes its primary's schema as it
comes.

*The rest of a compact.*

- From the first store a compact changes until every store points at the
  new file, a panic fails the database (`compact_unwound`), as one landing
  a block does through `unwound`. A server takes a lock a panic poisoned as
  it stands, and stores half pointed at the new file would read a
  handover's records at the old one's places.
- Beside the database nothing changes before the side file takes the
  file's place. From there to the last store, a panic fails the database
  too (`adopted_beside`).
- A rewritten file the sink cannot map (`Sink::remapped`: the file's length
  unread, the mapping refused) fails the database as well. `repoint`
  answered `Ok` there, and the stores read on from the old file's mapping,
  which they hold. The next handover then read the records appended to the
  new file at their places in the old one. A checkpoint and an adopted
  image reach `repoint` too, and fail alike.
- Under the lock, the stores are compacted one collection at a time before
  the rewrite (in memory into fresh segments, over a mapped file the
  reclaimed documents written again), and the only refusal there is a
  location outside its segment or the file. Met at a later collection, it
  left the stores before it compacted, their counters and their records'
  places ahead of the file -- over a mapped file, frames the file never
  held, which no handover then took in. It fails the database now
  (`Database::storage`), as the rewrite's own copy of that record already
  did: writes stop until the file is opened again, which reads back what
  the file holds, and a server's compaction asks no failed database again
  (`compact_due`). No write makes such a location, so a test puts one
  where the store is read (`unwinding`'s `refused`). Beside the database
  the copies are the maintenance's own: the same refusal leaves the
  database as it was, and the compact refused.
- A refusal the server's compaction meets at every look is counted in
  `fennec_auto_compactions_refused_total` each time, and logged once in its
  words, until the words change or a compact runs (`link::refusal_is_news`).
  It was logged every five seconds for as long as it stood. Left after
  this change: a damaged location met beside the database, which reads of
  that row meet too; a side file the disk refuses (`Error::Io`, the disk
  full); and a schema changed or an image adopted during the compact, which
  the next look gets past. None is what a document holds. The native
  library's `Compactor` keeps no log, and tries again at its next look.

## Compaction and the page cache

**A file is compacted before it outgrows the page cache** (`cache.rs`,
`CompactPolicy::memory`, `due_in`). There is no cache of fennecdb's own, so
what the reads need in memory is the file's pages holding a live record --
and under updates at uniform keys that is every page until a compact: a
million 1 KB records updated until the file stood at 2.1 GB left one on
every 16 KB page and 94% of 4 KB ones (a simulation of the bench's keys).
Past the cache every read and update waits for the disk in turn, whatever
is dead: in a container held to 2 GB (`docker run --memory 2g`), at half
the file dead, the file stood over 2 GB 69-74% of the time and 5M updates
took 358-511 s, 3.5-4.1k a second at the median 100 ms against 79k while
it fitted, 0.81-0.90M major faults, compacts of 38-99 s; on this Mac beside
1 GB wired by a ballast (`mlock`), 82-535 s. So a file is compacted too
once a third of what the compact writes is dead -- four bytes written a
byte updated at most, against two -- where its live records fit the room
the page cache has for it (`cache::room`) and either the file has reached
three quarters of that room (`ROOM_SHARE`) or the process takes 500 major
faults a second (`cache::faulting`, `getrusage`, over a second or more).
The room is exact on Linux -- `MemAvailable`, and a cgroup's limit less
what the group holds that is not file pages, from its own group up -- and
not on macOS, whose figure (physical memory less the apps', wired and
compressed pages) grows under the pressure as it compresses the apps:
2.4-2.9 GB while a 1.5 GB file was read from the disk at 5-7k faults a
second. On Windows it is `GlobalMemoryStatusEx`'s available memory, the
standby list among it as `MemAvailable` counts the cache, and a job's
memory limit at most, a container's; Windows counts no fault that waited
for the disk apart from the rest, so `cache::faulting` says `false` there
and the room alone decides
([windows.md](windows.md#7-what-else-windows-differs-in), 7j). The faults are the macOS half: beside the ballast, the rule
without them took 226-361 s, with them 191-260. A compact's own
faults -- the old file's records read, the new file read in -- are
forgotten once it is done (`cache::forget_faults`): counted, the next
look found the process faulting, and a 1.6 GB file that took none was
compacted every 6 s. A compact whose old and new files would not fit
together, or that starts while the process faults, writes the new one past
the cache (`SideFile::past_cache`: `F_NOCACHE`, or put on disk and
`POSIX_FADV_DONTNEED` a sixteenth of the room at a time, since written
pages stay until they are on disk -- [a compact's pages on their way to
disk](#a-compacts-pages-on-their-way-to-disk); none on Windows, whose `FILE_FLAG_NO_BUFFERING` asks for writes
the image's writer does not align) and reads it in after the swap rather than before: through the
cache its pages pushed out the old file's, which the reads need until the
swap, and the writer ran at 5-6k updates a second through a compact, 8k
with the pages dropped once the image was written, 18-25k dropped as it
went; read in before the swap, the writer ran at 6.2k a second through
a compact and the swap held the lock 24-60 ms, after it 18.6k and 6-14 ms.
With all of it the container's 5M updates took 80-100 s against 358-511,
the file at most 1.9 GB, 34-39k major faults against 0.81-0.90M, compacts
of 3-6 s, the reads during them p99 0.44-0.49 ms against 4.6-6.5 (max
113-517 ms against 220-534: a read meeting the new file as it comes in
after the swap), for 3.0-3.2 bytes written a byte updated against 1.9-2.0;
on the Mac, 191-260 s against 82-535 and 3.3-3.6 bytes written a byte --
macOS gives a growing file more of the machine as it compresses the apps,
which main's 82 s run had and its 535 s one did not, and a file kept small
is not given it: steadier, never the best case. A file that fits moves
nothing: 250 000 records (29-31 s, the median 100 ms 174-181k, through a
compact 153-156k), YCSB A, B and F in process (with the memory free A at
16 threads 82-92k -> 133k, the rest within the runs' spread; beside a 1
GB ballast on a quiet machine, against the same build with `memory` off,
two ABBA quartets, A at 1 thread 50-99k -> 101-148k and at 16 47-60k ->
72-127k, B at 16 197-460k -> 475-576k, F at 1 78-90k -> 111-202k and at
16 71-83k -> 102-115k, B at 1 313-476k against 412-477k -- one pair
beside other agents' builds had A at 1 at 29.7k -> 24.1k),
`make concurrency-bench` and `make requests-bench` in ABBA turns as on
main, the browser module byte for byte (`cache.rs` is `std-fs`'s). Rejected: `MADV_RANDOM` (Linux reads 128 KB
around a fault, the neighbours the next reads want: 806-811 s, 561-608
with the rule), a look every second rather than 5 s (13 compacts for 10,
3.8 bytes written a byte, no faster), writing an update over its old
record (its torn write is no torn tail to cut but a corrupt record mid-file,
and a pin, a handover's `Landed`, the sync log, a replica's apply and the
archive all take the file as only growing), dropping the pages only dead
records hold (none, by the count above), a compact ordering the records by
heat (a uniform workload has none; YCSB's hottest quarter of 16 KB pages
takes 77% of reads in id order against 96% by heat, which matters only
once the live records outgrow memory, and E's scans want id order).

### A compact's pages on their way to disk

**What a compact writes past the page cache goes to disk a sixteenth of the
room at a time** (`fs::past_step`, `fs::Steps`). A page a process writes
stays in memory until it is on disk, and a cgroup, whose limit counts it,
cannot reclaim it before: a written page holds the container as an
anonymous one does until its writeback ends. The side file past the cache
(`SideFile::past_cache`) and the scratch file a compact reads the file in
turn through ([below](#reading-the-file-in-turn)) were synced and their
pages dropped every 64 MiB whatever the room, and the scratch file's last
stretch, less than that, never: the read back let each bucket go
(`POSIX_FADV_DONTNEED`), which drops no dirty page, while the side file
filled its first 64 MiB. The P6 soak's primary, in a container of 512 MB
holding 417 MiB of its own, was OOM-killed in a requested compact with 84
MB of `file_dirty` in its cgroup and 15 MiB of clean file pages left to
reclaim -- its two files' steps at once, a sampler a second having seen
64 MiB on an earlier compact.

Now a step is a sixteenth of what `cache::room` gives -- the memory the
page cache has, a cgroup's limit less what its processes hold that is not
file pages -- between 4 and 64 MiB; a write longer than a step, a bucket
copied in turn among them (16 MiB), goes in pieces; and the scratch file is
settled -- every page on disk and dropped -- as its deal ends
(`Scratch::settle`), before the side file takes a byte of the bucket read
back, and the side file before each deal (`ImageOut::settle`), before the
scratch file takes a byte: one file's steps at a time. Left, the side
file's last step stood dirty beside the scratch file's two where two
collections past the in-turn floor were copied, three sixteenths of the
room. On Linux and Android, below the ceiling, each step's fdatasync runs
on a thread of its own while the compact writes the next, and is waited for
as that one ends, the pages then on disk dropped (`POSIX_FADV_DONTNEED`): a
step dirty and one on its way at most, an eighth of the room, the disk
writing one while the compact fills the next. At the ceiling, from a GiB of
room or with none known, each step is synced where it ends, as every 64 MiB
step was before the steps followed the room: a step dirty at most, a
sixteenth of the room or less, and the compact as fast as it was (below) --
on a thread it took 4% longer there, for a bound that room does not need.
The thread is the system's (`pthread_create`), running the fdatasync alone,
rather than std's (`std::thread::Builder`), whose packet, spawn hooks, name
and output capture no sync uses: the steps on std's grew the shell's code 3
264 bytes and took the binary a page past its budget ([binary
sizes](benchmarks.md#binary-sizes)); on the system's, each step function
kept once and their io errors made the crate's in one place (`fs::failed`),
1 280, and no binary a release ships grew a byte (x86_64 musl, built as CI
builds them). The thread makes its fdatasync again where a signal cuts it
short (EINTR), as std's `sync_data` does for a step synced where it ends
(`cvt_r`): a sync over NFS or FUSE is long enough to meet one, and the
compact failed with it where an inline step went on. One failing with no
error number fails with EIO, never as a success (`fs::sync_failure`). That
took the shell's code 64 bytes more, 1 344 in all, and its binary and
every other a release ships none (x86_64 musl, as above). A
file dropped with a step under way -- a failed compact's -- joins the
step's thread first (`Steps`' drop), so none outlives the compact. The
writes a rewrite beside the database makes under the
write lock, the last round's (`Database::finish_beside`), end no step
(`SideFile::unstepped`): the sync after them puts them on disk at once,
where each step past the first waited for the one before it under the lock,
sixteen fdatasyncs at the 4 MiB floor where 64 MiB took one. The in-turn
copy's buffers take a quarter of the same room at most (`in_turn::budget`),
so from 64 MiB of room up its buffers and its pages on their way together
stay under half of it, the rest left to the old file's pages, which the
reads need until the swap. The floor keeps a step a run of large requests
the disk takes whole and the steps few, 256 a GiB, a thread and an
fdatasync each; the ceiling is the step as it was, reached from a GiB of
room. Where no thread or descriptor can be had, a step is synced where it
ends, and a file dropped mid-step -- a compact that failed -- waits its
fdatasync out. macOS writes both files past the cache (`F_NOCACHE`) and
Windows through it, as before: neither steps.

*Not a write's durability.* None of these files is the sink's -- the side
file is synced whole before its swap, under no lock (`SideFile::sync`), and
the scratch file is unlinked -- so a step takes no lock of the database's
and never the sink's disk: `Sink::syncing`, a `try_lock` of it, says no
durability for one, and a write's buffer or a handover is put off for none,
as before. What a step can hold up is a write's own fsync on the same
volume, which flushes the disk's cache of whatever reached it unflushed and
shares the journal's commits: each step's fdatasync flushes what the step
wrote itself, so a write's fsync meets a step's at most where main's met
64 MiB.

*Measured* (a scratch program over `Database::maintain`, the compact beside
the database, its cgroup's `memory.stat` read every 5 ms; `docker run
--cpus=3 --memory 512m --memory-swap 512m`, Linux 6.12 aarch64 in macOS's
Docker, an ext4 volume; the 1-minute load under 4 at each run's start,
main and this change in ABBA turns, a run the load rose past 5 in left
out; a million 1 KB documents each updated once at a random key, 2.04 GB,
1.02 live). With 300 MiB held by the process, as the soak's server held its
indexes -- `cache::room` 186-190 MB, a step of 11.9 MiB -- main left 33-41
MiB dirty or on their way at the peak, this change 15-23; with 11 MiB held
-- 491 MB of room, a step of 30.7 MiB -- main left the soak's 80, this
change 34-37. The kernel throttles a cgroup's writer by a share of what it
could hold of the file, clean pages counted, which is all that kept main's
peak under its 64 MiB steps of two files; at 190 MB of room it holds this
change's under its two steps too, 64 MiB steps on the same thread peaking
at 15-22, and the sixteenth is the bound where it allows more.

The compact alone took 4.75 s at the median on main and 4.95 with this
change, 43 and 41 runs in six sessions: this change's median 1.7-4.7%
above main's in four quiet sessions and 4.7% below in a fifth; with writes
beside it 2.5-2.9% above. Steps of 64 MiB on the same thread took as long
as a sixteenth's (4.80 s at the median against 4.77 and main's 4.59, ten
runs each), and main's own 64 MiB steps synced where they end, with the
scratch file settled and the writes cut at the steps, as long as main
(4.64 s against 4.60, six runs each): what costs is a step's fdatasync
running beside the compact's writes, the price of the bound, which an
fdatasync where each step ends pays several times over (below). On macOS,
whose files are written past the cache as before and step nowhere, the
code is main's: 4.2-11.2 s on main and 8.2-12.1 with this change, six runs
each, the spread of a laptop's disk.

Every step above ran on a thread, as this change first had them at every
room; from a GiB of room up a step is synced where it ends. Measured again
so, in the same container and data, main and this change in ABBA and BAAB
turns, a run whose 1-minute load was 5 or more at its start or its end
left out: with 1536 MiB, 11 of them held -- `cache::room` 1495-1504 MB, a
step of 64 MiB -- the compact took 6.77 s at the median with this change
and 6.89 on main, twelve runs each (5.93-9.38 and 5.72-8.86), the pages
waiting for the disk peaking at 64 MiB against main's 88-90, main's
scratch file's last stretch dirty beside the side file's first step. With
512 MiB, 300 of them held -- 190 MB of room, a step of 11.9 MiB, on the
system's thread -- 5.57 s against main's 5.27, thirteen and twelve runs
(4.93-7.75 and 4.77-7.13; in the quietest of three rounds, 5.27 against
5.19), the peak 13-22 MiB against 35-48: the price of the bound where
memory is tight, as above.

The writes beside it. With a write made durable every 2 ms (`--sync
always`, a kilobyte a write into [the sync log](durability.md#the-sync-log)),
its fsync took 19.9-24.2 ms at p99 during main's compacts and 9.2-19.4
during this change's, 27-64 ms and 29-60 at the most, the compacts
4.03-4.28 s and 4.15-4.31 (one of 5.39). With writes buffered and synced
every 250 ms (`--sync 250`, the default, the file's own fdatasync), the
syncs took 1.1-17.8 ms at the median beside main's compacts and 1.5-3.3
beside this change's, twelve runs each in three sessions, the compacts
4.59 and 4.71 s at the median. Of the eight runs a side whose every sync
was timed, a sync took 41-90 ms during six of main's compacts, as one met
a 64 MiB fdatasync, and during one of this change's, 73 ms in its last
fifth of a second; none took 40 during the other seven. The first sync
after the swap took 79-154 ms in each of this change's eight runs so timed
and 99-120 in six of main's eight -- as long with steps of 64 MiB, and
with the scratch file's last step left to the kernel as main left it: the
swap's, every step's fdatasync waited for before it, which this change
does not touch.

*Rejected.* `sync_file_range` -- `WRITE` for a step, `WAIT_BEFORE` for the
one before, no journal commit, as first built: the same peaks, but nothing
flushed the disk's cache of the compact's writes but the next fsync on the
volume, the database's own. With writes synced every 250 ms its syncs took
53-64 ms at the median beside the compact against main's 3-5, and the
compact 6.42-9.10 s against 6.09-7.38; beside a durable write every 2 ms,
4.70-4.93 s on main against 5.15-5.41. The same with each step waited for
whole (`WAIT_BEFORE | WRITE | WAIT_AFTER`, a step at most): 15 MiB at the
peak, 4.59-5.35 s alone against 4.23-4.55 pipelined, the disk idle while
the compact filled a step. An fdatasync each step where it ends, as main's
64 MiB steps were synced: 5.79-6.90 s alone against 4.79-5.20 with
`sync_file_range`, the compact idle while the disk wrote. A step of a
thirty-second of the room: 9.4 MiB dirty at the peak, the syncs and the
compact as a sixteenth's -- twice the threads and fdatasyncs for a bound
the room already allows. Letting go of only the stretches a step synced,
never a page the next one dirtied -- `POSIX_FADV_DONTNEED` starts a dirty
page's writeback and, failing to drop it, drains every CPU's page lists to
try again -- for the time above: no faster, 5.13 s at the median against
4.97 letting go of the whole file and 4.89 on main, eight runs each.
Leaving the pages to the kernel's dirty
throttling, as main did: the 80 MiB above. The kernel counts the clean
pages a cgroup could drop as room for dirty ones, and a compact reading
the old file fills the cache with just those.

### Reading the file in turn

**A compact of a file past the page cache reads it in turn, and writes
what a compact by id writes** (`store::in_turn`). The new file lays each
collection's live records in id order, so `Store::write_live` walked the id
index and copied each record from wherever its last write had put it:
after updates at keys drawn at random, a page of the mapped file at a time,
at random. Past the page cache each was a read from the disk -- the P6
soak's dry run 3, a server in a container of 512 MB over a 2 GB file of
1.0-1.1 GB live, took 110-344 s a compact at 139-180k major faults a
minute, the writer at 1 500-2 000 ops/s meanwhile, against 3-6 s where the
file fitted. Every release that mapped the file compacted so, 0.1.5 to
0.2.0. Now, where the file is past the room `cache::room` gives it or the
process waits for the disk (`cache::faulting`) -- the rule the side file
is written past the cache by -- and the collection holds 4 MiB live or
more (`in_turn::FROM`), the records are read from the old file's start to
its end and put in id order on the way, in the same bytes.

Each record's place in the new data record is known before a byte of it
is read: the frames are laid in id order, each where the one before it
ends, so a walk of the id index sums them (`Store::buckets`) and cuts them
into buckets of ids, each at most the budget -- a quarter of the room,
between 4 and 16 MiB (`in_turn::budget`) -- or one record longer than it.
Then one pass over the store's frames as they stand, the mapped stretches
by their place in the file and then the segments in turn, keeps each frame
the id index points at (`each_standing`) and deals it into its bucket's
stretch of a scratch file beside the database, through a buffer a bucket:
the budget shared out between them, 64 KiB to 1 MiB each, 256 buckets at
the most (`BUCKETS_MOST`, below). The scratch
file is laid out as the data record is, a bucket's stretch where its frames
go in the record and as long, its frames in the order they were read.
Then each bucket in turn is read back into one buffer, each frame put at
its place by its id (`Places`: a slot of the id index's dense span each,
and the bucket's run of the sparse ids, sorted once a store and found by
halving -- a scan of the sparse map a bucket was buckets times sparse ids),
and the buffer written into the new file. Where
one bucket holds the collection, no scratch file is made: the frames are
put in their places in the buffer as they are read (`in_memory`). The new
file is the one a compact by id writes, byte for byte, so nothing that
reads it changes: the image's index, `relocate_live`, an open, a scan, a
replica. The choice is `Store::compact_live`'s, which a compact's data
record is written through, and it is made by the out (`ImageOut::in_turn`,
which a rewrite's image under the lock and a compact's side file beside the
database answer, and nothing else): a snapshot's out asks nothing, and an
image a pin sends (`pin_image`) is written out by `Store::write_live`, the
walk by id, which asks nothing. The two are methods apart on purpose:
with the check inside `write_live`, inlined or not, the walk's loop kept
one register fewer and reloaded the out from the stack every record --
19 000 to 25 000 instructions more of 5.67 million for a pinned image of
10 000 rows (macOS's counts) -- and with it apart the loop is main's,
instruction for instruction.

*What it holds.* In memory, the buckets' buffers while the records are
dealt -- the budget shared out between them, each at least 64 KiB, so 16
MiB together at the most whatever the file: at most 256 buckets
(`BUCKETS_MOST`, 16 MiB over 64 KiB), past which -- 4 GiB of live records
in a collection at the most budget, 1 GiB at the least -- the records are
walked by id, nothing made. The bound is the deal's own: this change's
second round gave each bucket 64 KiB past 256 of them, which held 1/256 of
the live bytes -- 400 MiB for 100 GB of documents of 100 KB, a memory the
room the budget is cut from never weighed -- and a smaller buffer a bucket
writes the scratch file a smaller piece at a time, at random between the
buckets; a pass over the file for each 256 buckets is the window rejected
below. Then one bucket's buffer, 4 bytes a slot of its span of the id
index and a read buffer of 1 MiB while they are read back. The budget's ceiling is measured (below): buckets
of 64 MiB took the process's anonymous memory to 76-78 MB, of 32 MiB to
44-46, of 16 MiB to 28-30, where the walk by id peaks (27-29), and the
time no further than the runs' spread -- 5.0-8.1, 5.2-7.9 and 4.6-6.6 s.
The first round's list of the live records by place held 16 bytes a
document more, 16 MB a million. On the disk, the
collection's live bytes once, for as long as its record is written, one
collection at a time: the scratch file is made only where the volume gives
twice the live bytes and 64 MiB beside what is written so far -- the
scratch, the data record still to come, the records after it and the
writes landing meanwhile (`fstatvfs`, `fstatfs`, Windows'
`FILE_FS_FULL_SIZE_INFORMATION`) -- and the records are walked by id
otherwise, as they are where it cannot be made or the volume cannot be
asked. The room is a snapshot, asked as the scratch file is made: a node
compacting several tenants at once, each on its own thread, can pass it
for each, as can a volume another program fills meanwhile. Then the
scratch file's write the disk refuses ends the deal, and the records are
walked by id (*Its failures*, below), the scratch file's room given back
as it closes before a record of the walk is written. A scratch file is written and read past the page cache as a side
file past it is (`F_NOCACHE`; on Linux on disk and its pages dropped a step
at a time and as its deal ends, [below](#a-compacts-pages-on-their-way-to-disk),
each bucket's dropped once read back): its pages would push out the old
file's, which the reads need until the swap.

*Its name.* The scratch file is `<file>.sorting` -- `data.fennec.sorting`
beside `data.fennec` -- made by its open alone (`O_CREAT | O_EXCL`,
`create_new`) and through no link (`O_NOFOLLOW` on a Unix), then unlinked
at once on a Unix, and on Windows opened to be deleted as its last handle
closes (`FILE_FLAG_DELETE_ON_CLOSE`): a crash leaves nothing beside the
file, the process's end closing the handle. What can leave it is a power
cut on Windows, or a kill in the instant between a Unix's open and its
unlink, which leaves it empty: `.sorting` is among `fs::LEFT_BESIDE`, which
the open that writes the file and a node starting remove
([durability.md](durability.md#files-modes)). Whatever stands at the name
as a compact starts is left as it is and the records are walked by id: a
database a user named so, whose lock `fs::remove_left` keeps it by, held or
not, a link, on Windows another compact's held open. This change's second
round made the file by `fs::create_private`, which on a Unix removes what
stands at its name first, as a side file written whole may -- a user's
database so named too, its lock held beside it.

*Its failures.* A walk that does not meet every live record as the id index
has it -- a frame that does not read, a record outside the stretches and
the segments, as a pinned store's, which keeps no stretch -- leaves the
records to the walk by id, nothing of the new file written yet. So does a
scratch file that fails before a byte of the new file is written -- a
write or a read refused while the records are dealt or the first bucket is
read back, or that bucket read back otherwise than it was written: nothing
of the database's own file was touched, so the compact goes on by id,
said on stderr once a database and failure (`walked_by_id`, a line as a
server's log writes one), where this change's second round failed the
compact, and under the lock the database with it. Once the first bucket is
written into the new file, a write or a read of the scratch file that fails
fails the compact as a write of its new file does: beside the database,
the compact alone, the side file removed, the database writing on; under
the lock (a batch's, the shell's), the database too, as every rewrite
failing there does (`Database::storage`). A bucket the scratch file gives
back otherwise than it was written -- each bucket's frames summed as they
are dealt and again as they are read back (CRC32C), each frame held to its
id's length and place, the length before it is added to, as a length read
off a failing disk may be any number and its sum overflowed into a panic
under the lock (`compact_unwound`) -- is such a failure, `Error::Io`, never
a record copied wrong. Beside the database the reading and the scratch file
are the side file's, with no lock held; under the lock, the lock's as the
walk by id was.

`store::in_turn`'s unit tests hold the copy to the walk by id byte for byte
over a store reading from two stretches of a mapped file and two segments
-- ids rewritten within a stretch, from one to the next and into memory,
deleted, written again, four past the dense array out of their order, one
longer than a bucket -- in buckets of one record each up to one in memory,
and over 188 sparse ids, each more than 4 096 past the one before, in three
buckets of them and more; a store past `BUCKETS_MOST` buckets and a pinned
store, whose walk misses its mapped records, to the walk by id with nothing
written; a file and a link at the scratch file's name to the walk by id,
each left as it was; and a scratch file refusing a write, or handing back
the first bucket with a flipped byte or a length of `u64::MAX` less one,
to the walk by id, and the second bucket so to a failed copy, none leaving
its name. `tests/in_turn.rs` holds a compact in turn to one by id byte for
byte under the lock and beside the database, over an image, a spilled
block, a tail taken in from the file and writes in memory, a broken scratch
file before the new file is written to the file a compact by id writes,
and one after to the failures above, each test in a directory of its own
that goes as it ends. Twenty-six mutations each fail at least one: the
segments left out of the walk, a place one off, a record dealt into the
next bucket, the sums not compared, a bucket's last buffer never written, a
head's length one off, the sparse ids' places unsorted, the buckets written
out last first, a head put one byte on, a payload never put in memory, the
rule's `or` an `and`, the 4 MiB floor dropped, `compact_live` never asking,
a compact under the lock or beside the database calling `write_live`
instead, the side file never asked or never asking, the image under the
lock never asking, the scratch file left at its name, a walk that missed a
record copied all the same, the bound on the buckets dropped, a scratch
file failing in the deal or in the first bucket failing the compact, a
bucket's run of sparse ids taken from the first sparse id, the scratch
file made by `create_private`, and a length read back summed before it is
held to the index's.

*Measured* (a scratch program over `Database::maintain`, the compact
beside the database; `docker run --cpus=3`, Linux aarch64 under macOS's
Docker, the 1-minute load under 3 at each run's start, main and this
change in ABBA order). A file of a million documents of 1 KB, each
updated once at a random key: 2.04 GB, 1.02 live. In a container of 512
MB, past the room: by id, 44.9-51.1 s over 10 runs, 526 000-531 000 major
faults, the process's anonymous memory peaking at 27-29 MB and the
volume's use at 977 MB over the old file (the new one); in turn, with
buckets of 16 MiB, 4.6-6.6 s over 10 runs, 1 550-2 630 major faults, 28-30
MB, 1 921-1 945 MB (the scratch file beside the new one). Of a compact in
turn, dealing the records took 2.9-3.2 s and reading the buckets back and
writing them 1.7-1.9 (16 MiB). A scratch file left to the kernel's
writeback, against one synced and its pages dropped every 64 MB: 5.0-8.5
s against 5.0-6.5, the writeback stalling the deal (2.6-5.0 s against
2.8-3.2). Half a million such documents in a container of 2.5 GB, which
fits: 1.1-2.1 s on main, 1.0-2.8 on this change, by id both, no fault, 16
MB. Both compacts' files are the same bytes (`cmp`, every pair), so are
their images' indexes (4 001 502 and 2 000 758 bytes), their opens (a
warm open 2.7-6.3 ms, the small one 1.5-2.0, by either binary) and their
scans: a scan of the large one in id order took 74-76 ms where the read
before had left it in the page cache and 283-736 ms where it had left less
of it, by either binary on either file; the small one 35-38 ms. `fennec-server`
(aarch64 macOS, release) grew 16 592 bytes, the browser module none (863
088 bytes: `store::in_turn` and `fs::Scratch` are a 64-bit Unix's and
Windows' with `std-fs`). The perf gate's `file-compact` shape (10 000
rows, under `FROM`), counted by a copy of it on macOS, gave 10.21 million
instructions on both, heap bytes equal, and a pinned image of the same
rows 5.65 million against 5.68.

*Rejected.* Laying the new file in the order the records stood in the old
one (this change's first round): the copy read the file in turn and needed
no scratch file, but the file kept the order of the last writes -- a scan
in id order of a million records past the page cache took 2.0-31.7 s
against 0.21-0.59, the image's index 7.75 MB against 4.00 (a place out of
id order 4-5 varint bytes, not 1), a warm open 4.7-6.3 ms against 3.5-3.7,
and the list of the live records by place 16 bytes a document beside 8
more to index them, 46-53 MB of anonymous memory against 27-30. A list of
the live records sorted by place, to read only the pages holding one: 16
bytes a document where the walk of the frames holds nothing, and it reads
dead pages alike once updates have left a live record on most. A pass over
the file a window of ids at a time: sixteen passes of 2 GB for 64 MB
windows. The scratch file's bytes put at their places as they are dealt,
the file then copied out whole: written at random, a page past the cache is
read back to be written again. A scratch file a bucket: a descriptor each,
past a few hundred buckets more than a process may hold, and a name each
to leave.

## File format

**File format** (see README *File format*): every record is
`[kind][collection-id][length][body]`. The length is written even for an empty
body and the reader **must** consume it, or the stray byte is read as the next
record kind. A last record a crash cut short is cut off the file on open
(`Database::load` says where, `fs::open` and `replication::open` cut; on
Windows, which cuts no file a view maps, the open lets the database and its
views go, cuts and opens again, `fs::windows::cut_and_open`): only
skipped, it swallowed the next append, an acknowledged write lost on the open
after. Only past the checkpoint image, though: an image is renamed into place
whole, so a record cut short inside it -- or an image longer than the file --
is refused as corrupt; cut there as a torn tail is, one flipped bit deleted
every record after it. Nor is a tear ever inside a record: a data record's
frames (`[op][id][length][payload]` each) reach the replay whole by the
record's length, and past the durable point by its sum, so a frame running
past its record's end is damage, refused as a replica's apply refuses it
("a write record cut short", `Store::replay_noting` and `replay_mapped`),
and where a feed counts a record's writes (`writes_in`), whose add of a
length near 2^64 wrapped back into the record in a release build.
The replay stopped there as at a torn tail, as it did before records had
lengths, and the frames after it vanished while the open went on with the
records after them (found in the review of the fuzzer's pull request,
`a_frame_running_past_its_record_is_refused_where_a_file_is_opened_and_applied`).
A frame's op is a put (1) or a del (2), and the replay refuses any other
by its byte and the frame's place in the file ("unknown write op 3 at
4096") before the frame changes anything: a later version's op, or damage
below the durable point, which the replay reads unchecked, is never read
as another. It matches the op once (`Store::replay_noting`,
`replay_mapped`), each arm counting the version it leaves dead as well as
the frame itself, and counts no more than before (instructions from
`proc_pid_rusage` under a bump allocator, medians of 6 runs): `file-open`'s
100 000 rows mapped -0.02%, read into memory -1.5% (`copy_frame` tests
`active`'s seal inline, where `active` whole in both arms was a call a
frame, +1.3%), and a del through `Store::append` -3.4%, its dead count
taken from `remove`'s answer instead of a second lookup. The browser
module grew 400 bytes, fennec-server's code some 690. A replica's apply
refuses one before the frame lands (`Database::apply_frames`, "a write op
3 is neither a put nor a del"), so `Store::land_frame` is only ever handed
a put or a del, its other arm a `debug_assert`. An image's index,
which a mapped open takes in the place of the walk (`Store::adopt_index`),
reads no op: it names each live document's payload, whatever else the
record holds. A checkpoint's record holds its frames as they stand,
deletes and versions written over among them, counted in the index's
`dead` (`Store::write_image`); only a compact's holds puts alone
(`write_live`). So a new op comes behind a new mark on the index, which a
build that does not know it leaves to the walk (the id counter's index,
below).
Every release from 0.1.0 through 0.2.0 replayed such a frame as one that
changed nothing: it counted the version before it dead while the index
still pointed at it, counted itself live, and the next compact dropped it
with no word (`store::tests::an_unknown_op_is_refused_by_its_byte`,
`an_unknown_op_in_the_file_is_refused_by_its_byte`; through a record of
its own, a block's, a spill's and a land's, each refusal naming the
frame's place in the file, in
`a_frame_of_an_op_neither_a_put_nor_a_del_is_refused_where_a_file_is_opened_and_applied`).
A replica before 0.2.0 landed one in its store as an open replayed it
(v0.1.12's `apply_frames` handed it to `Store::append`), and took the row
out of its indexes besides (`unindex_doc(old, None)`) while the store
still held it. 0.2.0's apply refuses it
(`a_frame_that_does_not_read_changes_nothing`).
A tool that only looks (`fennec types`) opens with
`fs::open_read_only`, which cuts, creates and writes nothing: a server's
append in flight looks torn from outside. Zeros to the end of the
file are a torn tail too when they start at a record's head (`zero_tail`),
cut the same way: Windows power-cut run 42533808035, its disk's cache lost
at the cut, came back with a file its server refused, "unknown record kind
0". NTFS journals a file's length, not its bytes, so an append's new length
can reach the disk before the bytes do -- a flush of the volume's log
commits it, and the `.beside` image of the compact in flight, flushed
beside the file, may have been one -- and after the cut the append reads as
zeros, bytes the file claims and the disk never took (ext4's
`data=writeback` can do the same). The run kept no file, so this is the
cause read from the error, not seen. Such an append was never answered as
durable: `--sync always` answers after the file's own `FlushFileBuffers`,
which writes its bytes. No kind is 0, so a zero head is no record; the
zeros are cut only where they run to the end and start at that head
itself: at the image's end (`body_end`; an image is renamed into place
whole, whatever its last byte), or after a byte that is not zero. A zero
byte before the head may be the previous record's own last byte -- a
document ending in a 0 or a `false` -- or the disk's zeros starting inside
that record: its length fits the file, so it reads as whole, and the
rule's first version (#559 as first pushed), which cut any zero head with
zeros to the end, opened a file whose 8 000-byte text, zeroed from the
4 KiB page after its head, read back as 4 001 NULs. Neither can be told
from the other, so both are refused; so are zeros then any other byte, and
a zero head inside the image. An unknown kind's refusal names the byte it starts
at, the image's too, so the next such failure says where.

**Checksums.** Every record this version writes into a file ends in a
claim and a CRC32C: `[kind | 0x80 | (w-1)<<5][collection][length][body]
[claim][sum]` -- the kind in the low five bits (`KIND`), the claim's width
`w`, 1 to 4 bytes (`CLAIM_MAX`), in the two above (`WIDTH`), the high bit
saying the record carries both (`CHECKED`), the length counting them, and
the sum covering the head, the body and the claim (`framed_claiming`,
`Summed` and `put_record` write one, `crc.rs` sums). Every top-level record
carries them -- an image's, a block's, a spill's, a land's, a graph's, the
history's, a close's mark -- but the counter's fixed-width head, which is
patched in place and only ever written by a rewrite renamed into place
whole; the records inside a block, a spill or a land carry none, the one
around them covering them. An image's data record is written a stretch of
its store at a time through `Summed`, its sum carried over each stretch, so
the image is still never a second copy of the data.

*The claim, and the durable point.* A record's claim says how far its file
was durable when it was appended: 0 for nothing known, or one more than how
many pages (`CLAIM_UNIT`, 4 096 bytes, rounded up) before the record's first
byte every byte was covered by an fsync that had completed -- the file's or
the sync log's (`Disk::sync`, `sync_inherited`, `sync_existing` store the
file's length in `Disk::durable` once the call returned) -- or by the point
the open found (`Sink::set_durable`). The append reads that point -- an
atomic the durability stores from behind the lock, which the database
takes from the sink as it takes the sink (`Sink::durable`,
`Database::set_sink`) and reads with no call through it, `claim_of` --
and writes it; nothing more is written and nothing synced. A claim
rounded down from its own record may point up to a page before the file's
start, where an fsync covered less than a page: it claims nothing. A
record within a mebibyte of the last fsync claims in one byte (two reach
256 MiB back): under `--sync always` every record claims every byte before
it, and under the default `--sync 250` the syncer moves the point four
times a second. A claim counted in bytes took two or three bytes for a
record more than 254 bytes past the last fsync -- most, between periodic
syncs; counted in pages the open checks a page more at most. The claim is
in the trailer, its width in the kind byte: a walk reads a record's head
alone, never its last page, and an image's data record, its body streamed
before its sum is known, writes its claim there too (0). The browser
module claims nothing (its page writes the file and journal whole, with no
fsync of the module's), and checks every record past the image.

The file's durable point D is the claim of the newest record whose sum
holds, the image's end where none says more (an image is synced before it
is renamed into place). The open finds it on the walk over the heads that
already ran before the load for each index's last graph (`last_graphs`,
now `survey`): it keeps the last 16 checked records past the image
(`RECENT`) and one every 16 MiB of them (`SAMPLE`), and sums them from the
newest back until one holds -- the last record, or the last few, where a
power cut took more. A claim is a page behind an earlier one's at most (it
is rounded down from its own record's start), so the newest whole one is
the point to a page. The walk passes over a record of no sum after one
with, whatever its kind: the load judges it (below). Taken as a graph's,
stale bytes past the point starting with a 4 refused the open where their
field did not read as a name ("varint ended early", "invalid utf8": 2 or
3 of 300 random stale tails in the third review), and passed over the
index's last graph where it did. A file's sink closing after it appended something and
an fsync covered everything appends a mark saying so (`REC_SYNCED`, kind 14, 8
bytes, its body empty and its claim 1; `FileSink`'s drop, with no fsync of
its own: lost, the claims before it stand), so a clean stop's next open
checks nothing; a file opened and closed with nothing written gains no
byte. The mark is no write, never sent to a follower.

*What an open checks, and why that is enough.* Records ending at or below D
are read as the image is, unchecked: an fsync that completed covered them
before the record claiming D was appended, and a power cut takes only what
no completed fsync covered. Past D the open holds each record to its sum,
but a graph a later one of its index replaces, which the load never reads
(in the browser, which restores each graph it meets, each is). What it
reads that it did not: the records past D, the window since the last
completed fsync, and the survey's last record or few. Damage at rest below
D -- a bit the disk flipped in a record it held -- is `fennec verify`'s to
find, never the open's: it holds every record to its sum (`check_records`;
`check_reader` streamed, a record summed as it is read rather than the image
held whole), of an archive's images and, given a file (`fennec verify
data.fennec`), of the file. An open that meets damage below D anyway -- a head no record
reads from stops its walk there -- refuses it as damage, by the record that
claims past it (below).

Measured on this 8 GB M1 (`make open-bench`'s writer, `examples/open.rs`:
1 GB written as a server that never checkpointed leaves it, every record
past the image; main's build over the file written without sums, this one
over the same documents claimed as `--sync always` claims them, over main's
file -- checking nothing, the noise -- and over a file claiming nothing, the
worst case, every record checked; medians of seven, the file read into the
cache before each open or copied past it with `F_NOCACHE` for a read from
the disk, three for a read into memory from the disk and for 400-byte
documents from the disk): 1 GB of 64 KB documents opened mapped in 14.4 ms
against main's 15.7 cached (this build over main's file 13.1), and in 1 529
against 1 490 from the disk (1 562); read into memory in 329 against 326
cached, 595 against 716 from the disk. 1 GB of 400-byte documents, 1.2%
larger with five bytes more a record: mapped 466 ms against 454 cached
(449), 2 769 against 2 759 from the disk (2 327); read 773 against 746
cached (699), 933 against 917 from the disk (928). The rows from the disk
move by about 15% between runs of one build -- this build over main's file
read 2 327 and main 2 759 -- so 595 against 716 is no speedup, and none of
them a cost: within the noise. Claiming nothing, the 64 KB file opened in
124 ms cached and 3 761 from the disk -- every page of the tail read, as
the first version of this change read every file -- and the 400-byte one
in 491 and 2 801 mapped, 866 read.

*Cut, or refused.* Past D, damage -- a record failing its sum, or a head no
record reads from: zeros, stale bytes, a record of no sum after checked
ones, a counter's head anywhere but the front, a head naming a collection
no create gives (`u32::MAX` on, `whole_record`, whose length fits), a
plain one (`0a ff ff ff ff 0f 05`, a newline read as a kind) as well as a
checked one whose record fails its sum -- is judged by what follows it
(`judge_after`). A whole record after it, by its sum, whose claim is
past the damage says an fsync covered the damaged bytes before that
record was appended: they were on the disk and changed since, and the
open is refused, naming both ("the record at 1234 fails its checksum, and
the whole record at 5678 says the file was durable past it"; "no whole
record starts at 1234, and ..."), the file left as it is. None found, the
damage is in bytes written since the last completed fsync, which a power
cut loses in any order -- a record's later page lost and a later record's
kept, a hole -- and the file is cut there, with everything after it, said
by the open (`Database::cut_on_open`, the server logging the bytes, the
offset and the records cut; on Windows, where the open lets the file's
views go, cuts and opens again, the cut is carried over that second open,
which finds nothing to cut -- dropped there, every test of the cut said
`None` on Windows alone). Under `--sync always` a write is answered once
the fsync covering it returned, and the next record claims past it, so a
write answered as durable is cut only where nothing after it says so: the
disk's damage to the last record it synced reads as a torn tail, and so
does a head whose length was damaged past the end (below). The log says
what the open knows -- "a torn tail: no whole record after it says a sync
covered it" -- where it said "none of it a write answered as durable",
which those two make false.

*What a cut leaves.* The open that cuts leaves a close's mark after what
it keeps (`REC_SYNCED`, its claim its own start; `FileSink::cut`,
`Database::cut_to`): every byte before the mark was read by that open --
below D unchecked, past it held to its sums -- and is durable before the
mark is, and the mark says so, so the next open finds D at the cut and
reads the file as the open that cut it did. The
records claiming D are most often the newest whole ones, past the damage,
and they go with the tail: unmarked, as every version from v0.2.0, which
brought the claims, cut until the fix (2026-11-22), the next open found
the file durable only as far as the records left claimed, held the ones
between that point and the cut to their sums, and judged what failed: a
row the disk had flipped a bit of below D, read unchecked by the open
that cut, was cut then as a torn tail with every record after it, synced
ones. v0.2.0's `torn-wide.fennec` is that file -- its first open cuts
2 307 463 bytes (23 records) back to 194 119, and unmarked the next cut
5 358 more, two synced rows -- silently on a Unix, while on Windows, whose
open cuts and opens again at once (`cut_and_open`), the second open
refused it ("cut short again"; CI's four Windows jobs on the sixth
train). Only where a record the open keeps carries a sum
(`Database::marks_cut`, found from the front at the cut, the first record
past the counter in a file this version wrote): in a file of no sums --
a version before checksums wrote it, its torn tail cut -- no claim decides
what an open reads, and the mark would be the first record that version
refuses. Nor where a mark whole by its sum already ends the records kept
-- a close's, or the one a cut a crash stopped left (below): it claims
them as the cut's would, and the cut is the length's alone, after an
fsync that makes a close's mark, written unsynced, durable first. The mark is
the record v0.2.0's closes write, and its survey takes the mark's claim
for D as this one's does, so a v0.2.0 binary reads a cut file as this
build does. An open that cuts nothing runs none of it.
The browser module reads no claim, checking every record past the image,
so its own cut (`openFile`'s truncate) needs no mark and makes none: it
reads a file a native open cut and marked as it read the file before the
cut (refused, both, where a whole record claims past a row failing its
sum; unmarked, it took the cut file by cutting the two synced rows too).

*The cut's order.* Each step is fsynced before the next: the pages one
fsync covers reach the disk in no set order, the file's length among them.
1. The file is fsynced: the pages the earlier process never synced, which
the open read from the page cache after a crash of the process alone, the
torn tail with them, and a close's mark that ends what the cut keeps,
which a close writes unsynced. 2. Marked, the mark is written over the
torn record's first eight bytes, in place, and right after it a head
whose length runs past any file (`engine::CUT_SHORT`, 11 bytes: the head
of a block 2^63 - 1 bytes long), and fsynced: one write inside a 512-byte
sector, which a power cut is taken to leave whole or as it was (*The
sector, an assumption*, below), and across a sector's
end -- the cut in the 18 bytes before one -- three, each fsynced: a zero
over the torn record's kind byte, the other 18 bytes, then the mark's
kind byte, so the head at the cut is the torn record's, a zero or the
whole mark, never a mix. 3. The file is cut after the mark (`set_len(whole
+ 8)`, which grows it back where the tail was shorter than what step 2
wrote) and fsynced. Unmarked -- a file of no sums, or a whole mark
already ending what is kept -- the cut is step 1 and step 3.

A power cut leaves, before step 1 returns, the file the earlier crash
left, nothing of the cut on the disk: the next open is a cutting open
again. After step 1, the torn file the open read, durable, which the next
open cuts as this one did. Inside step 2, inside a sector, that, or the 19
bytes whole, or the length alone where the write grew the file (the torn
bytes and zeros after them, cut as before). After step 2, the mark and the head past
the end: the next open finds D at the cut, the mark's claim, reads every
record before it as this open read them, and stops its walk at the head
as at any torn tail -- nothing of the torn record after the mark read or
judged, whatever it holds -- cutting right after the mark and marking
nothing (a whole mark ends what it keeps). Inside step 3 that, or the file
cut and marked; after it, the file cut and marked. Unmarked, the torn file
with what it keeps durable, or the file cut. Each opens, every way a file
opens, as the open that cut it read it, cutting nothing it kept, and
every open that writes leaves it as the uninterrupted cut does: the bytes
kept and one mark. A write or an fsync of the cut that fails fails the
open, nothing after it written; an open tried again reads the file as the
page cache holds it -- what the failed step wrote among it, the mark
perhaps, which may never reach the disk -- and goes on from there, as
every reopen after a failed fsync takes the page cache's word for bytes
the disk may not hold ([durability.md](durability.md#storage-errors): the
kernel may have dropped the pages, and a failed fsync is never retried):
that general case, which the cut neither widens nor closes but in one
state. Across a sector's end, a failure once the zero is written -- its
fsync, the write of the other 18 bytes or of the mark's kind byte (step
2's second and third writes), or the fsync after either -- leaves the
zero where an open tried again reads it, durable once its own fsync ran,
and the kind byte perhaps not: that open reads the window's state below
with no power cut, refused or, in the window's cases, cutting the flipped
row and the synced rows after it. A failed cut (step 3) leaves the mark
whole and durable, read as after step 2.

What came before. The mark alone in one write (57f77b58): torn across a
sector's end, its first bytes and the torn record's next framed a head
anew, and with `83 01 ff ff 7f` there (a data head whose length runs past
the end) the next open cut the flipped row and the synced row after it
in `torn-wide`'s shape; whole, the walk went on after it into the torn
record's bytes, and a record there whole by its sum -- a client's text
can be one -- was read as a write. A single zero after the mark stopped
the walk but judged what followed it, refusing the open where a record
there claimed past the zero: a client's text so made, or a later spill of
a dead block an fsync covered. And a close's mark, unsynced, could lose to
the shorter length of the cut after it, which the next open then judged
from the kept records' claims alone: the flipped row and those after it
cut.

One window is left, inside step 2 across a sector's end: between the zero
and the mark's kind byte. The zero stops the walk at the cut, so D is the
claim of the records kept alone, and what follows the zero is judged,
never read (`judge_after`). Where that is the cutting open's D -- the torn
record ran past the end or did not frame, or nothing past it claimed more
-- the open reads the file as the open that cut it did and cuts at the
zero, or is refused where a whole record after the zero claims past it (a
later spill of a dead block an fsync covered; a client's text so made).
Where the records past the cut raised D and a row between the two points
fails its sum (`torn-wide`'s shape), that row is judged across what
follows the zero: refused where a record there claims past it -- the
records past the torn record, still on the disk until step 3 ("the record
at 289 fails its checksum, and the whole record at 5692 says the file was
durable past it", the test's) -- and cut, with every record after it,
synced ones, where the judgment reaches none. It reaches none in two
ways, three files the test holds to this window (`window_case`). Every
record claiming past the row -- one, in each file built -- has its head
under the 19 bytes the cut writes, which take it: the dead spill whose head the zero is (a block
that spilled just after an fsync and never landed: the row and the three
synced after it cut, "a dead spill, 3 bytes before a sector's end"), or
the one record after a torn record shorter than those bytes, whose head
the other 18 write over (a row of no text, 13 bytes, torn, then one row,
the only record claiming the fsync past the flipped row: the row and the
synced one after it cut, "a torn record shorter than the cut writes, 3
bytes before a sector's end, then one record", and 12 bytes before). Or
the torn record's own bytes, a client's text, are made a record whole by
its sum that claims nothing and then a head whose length runs past the
end: found before the cut's other 18 bytes reach it, that record sends
the judgment on to the head, where it stops short of the records that
claim past the row (the row and the synced one after it cut). So a cut of
synced rows needs all of a row the disk damaged below D, a crash that
tore the tail, a power cut in the window -- three fsyncs, about 10 ms
here -- or a write or an fsync failing there (above), and a cut in the 18
bytes before a sector's end (18 in 512 cuts). Closing it needs a judgment that
stops at the cut's zero rather than looking past it: a byte no head of
this version is, which a judgment reaching it refuses -- a change to how
an open judges any damaged file, not made here.

*The sector, an assumption.* That a power cut leaves a 512-byte sector's
bytes whole or as they were is what the cut assumes, not what every disk
promises. NVMe promises it: a write no longer than its atomic write unit
for a power failure (AWUPF, a logical block at least: 512 bytes or 4 KiB)
that crosses no atomic boundary lands whole or not at all, and so any
512-byte span inside one block. SATA's standard promises nothing of a
sector a power cut stops. SQLite assumes less than the cut: that a sector
is written linearly, from one end, so a power cut keeps a part of it at its
front or its back. On a disk that tears a sector so, step 2's one write
inside a sector can be kept in part as a write across a sector's end can,
with no zero before it -- a front of the mark and the torn record's next
bytes framing a head anew, or the mark whole and nothing of the head past
the end -- the states 57f77b58 left (*What came before*): the next open
may cut the flipped row and the synced rows after it in `torn-wide`'s
shape, or read a record of the torn record's bytes whole by its sum as a
write. The three writes hold under such a tear -- a byte alone kept or
not, the 18 bytes kept at either end from any byte, as the test builds
them across a sector's end -- and inside a sector too would cost every
marked cut two writes and two fsyncs more (+257 k instructions and about
7 ms an open here, as across a sector's end): not done here, as NVMe's
promise makes them needless.

On Windows a view another process maps of the file refuses the cut itself
(step 3; `cut_refused`, `SetEndOfFile`'s `ERROR_USER_MAPPED_FILE`, the
message naming the views): marked, the mark and the head past the end
are on the disk by then over the torn record's first bytes, the length
kept, and the open once the views go reads that file as above, cutting
right after the mark; unmarked, nothing was written.

Measured (rusage's instruction counter and the wall clock, arm64 macOS,
a release build of each tree, 3 rounds of 15 opens of an APFS clone of
each file, the three binaries alternating, medians of the rounds' medians,
the 1-minute load under 2.5 at the start and 2.1 at the end, 2026-11-22),
the cut's first form (1a4f8e5b) -> the last round's (57f77b58) -> this
one: an open cutting a torn last row off a file of 24 rows, the cut inside
a sector, takes 1.276 M -> 1.427 M -> 1.427 M instructions mapped (1.234 M
-> 1.389 M -> 1.407 M read into memory), of 10 024 rows 4.437 M -> 4.680 M
-> 4.683 M (4.904 M -> 5.183 M -> 5.187 M), and 4.9 ms -> 12.6 ms ->
12.9 ms of wall time on the 24 rows mapped: the 19 bytes written cost what
the 8 did. The cut 3 bytes before a sector's end, two writes and two
fsyncs more: 1.256 M -> 1.415 M -> 1.672 M (1.228 M -> 1.392 M -> 1.655 M),
4.434 M -> 4.694 M -> 4.965 M (4.917 M -> 5.192 M -> 5.468 M), and 5.0 ms
-> 12.7 ms -> 19.9 ms (6.8 ms -> 14.4 ms -> 22.4 ms on the 10 024), each
`F_FULLFSYNC` about 4 ms here, in 18 cuts of 512. One cutting a torn
record right after a close's mark, the length's alone, with the fsync
before it now: 1.262 M -> 1.154 M -> 1.288 M (1.241 M -> 1.128 M ->
1.265 M), 4.284 M -> 4.175 M -> 4.418 M (4.771 M -> 4.654 M -> 4.887 M),
and 4.6 ms -> 4.5 ms -> 8.5 ms. Each on an open only a crash leaves a
tail for; the files without a tail open in the same count (560 k -> 560 k
-> 561 k, 3.234 M -> 3.232 M -> 3.230 M mapped). The first form had cost
133 k-145 k over no mark (602 k -> 736 k on its own files of 24 rows).
`fennec-server`'s `__text` 4 974 592 -> 4 975 232 bytes (+640, +1 728
over no mark; the release build, arm64 macOS; the file the same
7 123 776): what it costs that no file a power cut leaves of the cut is
read otherwise but in the window; the browser module the same 862 018
bytes, none of it built there.
Held by `a_cut_a_power_cut_stops_at_any_step_opens_as_the_cutting_open_read_it`
(fs.rs: from the steps each way an open writes took, every file a power
cut after each may leave -- each write after the last fsync lost, whole,
its length alone, and where it crosses a sector's end split at every byte
either way, and before the first fsync bytes the earlier process never
synced lost too -- of 30 files: one torn as `torn-wide` is and one whose
kept rows no fsync covered, torn long and short, each cut inside a sector
and 3 and 12 bytes before a sector's end, the torn record's bytes from its
ninth as written, `83 01 ff ff 7f`, a record whole by its sum claiming its
own start from its ninth byte and from its tenth, and one claiming
nothing, that head after it; a dead spill; a torn record of 13 bytes, a
row of no text, then the one record claiming past a flipped row, at the
same three places; a close's mark, then a torn record. 639 files a power
cut may leave, each opened every way, read-only too: each as the cutting
open read it, but in the window above, where 1 080 opens were refused,
and its three cases cut, 40 opens of the crafted record, 160 of the dead
spill and 220 of the record after the short torn one (44 files, each
after the zero's fsync, the other 18 bytes over that record's head kept
whole or in part), each case held to be reached. On 57f77b58's cut 679
opens of 52 of the 27 files then built failed: the record whole at the torn record's ninth byte read as a write
after the mark, inside a sector too; the mark torn across a sector's end
cutting the flipped and the synced rows or refusing the open; and the
length cut after a close's mark kept with the mark lost, the flipped row
and the four after it cut. On the cut's first form an earlier form of the
test read lost bytes as zeros and cut the synced rows),
`a_file_an_open_cut_opens_again_as_that_open_read_it` (a file torn as
`torn-wide` is, cut by each way an open writes and opened again by every
way, read-only too) and
`torn_wide_cut_by_an_open_opens_again_as_that_open_read_it` (the fixture
itself, answering as v0.2.0 did after each).

*Framed first, a byte at a time last.* What follows the damage is read
record by record where it can be. A record whose head frames -- a kind this
version knows, a length that fits the file -- and whose sum fails holds the
damage inside it, and the judgment goes on at its end, a head at a time,
as long as that end reads as one more record (the file's end, zeros to it,
or a head that frames or runs past it): a hole in 4 MiB of vectors costs
the records after it, not their bytes. A record whose head or length runs
past the end of the file is a torn tail, whatever came before it, cut as a
version before cut it with nothing after it looked at. Only a head that
does not frame, short of the end, is looked past a byte at a time -- every
version before refused those -- for the next record whole by its sum: one
summed only where its head frames, its claim is none or one this version
could have written after D (no more than 16 pages under it,
`CLAIM_SLACK`: a claim is rounded down a page, and a process after a power
cut claims from the point its open found), and its end reads as one more;
from the first whole one the judgment frames again. A long one is checked
without reading its bytes again: the look keeps the sum of the bytes from
where it began to every 4 KiB past it (`STRIDE`), and a record's sum is the
one at its end less the one at its start carried over its length, a
multiplication in the polynomial's field for each bit of it
(`crc::crc32c_after`, zlib's `crc32_combine` turned round). Each byte
summed counts against sixteen times the bytes after the damage and 64 MiB;
past that the open is refused ("what follows it could not be judged"), as a
version before refused any head it could not frame, and never for a tear
at the end, which is never looked past. A head of a kind this version does
not know whose record is whole by its sum is a later version's, refused as
before ("unknown record kind 143 at 1234": the message prints the byte,
here a checked record's of kind 15 with a one-byte claim -- a wider claim
prints another byte, 175 for two -- where a plain head of kind 15 reads
"unknown record kind 15"), and so is one naming a collection no create
gives whole by its sum ("a record names collection
4294967295, an id no create gives"): a crash leaves bytes no sum holds,
so a whole record that does not read is damage refused by name, never
cut (`whole_head`, which sums a head of any kind naming any collection).
The fuzz fix's walk, as its second review found, judged such a head as
stale bytes, cutting it and the file there, where a version before
refused it; one of a kind this version knows whose sum fails is judged
as a record failing its sum, from its end, as a version before framed
it, and what the cut takes counts it, as a version before counted it
(`records_from`). An archive's reader sums one the same way
(`checked_claim`): read as a record of no sum, it was cut from its
segment as a power cut's hole, where a version before took it and a
restore refused it as it applied it. In a file whose records so far
carry no sum -- the magic alone, the magic and the counter, or one a
version before checksums wrote -- a checked head of a kind this version
knows naming such a collection is judged as it is past checked records,
by its sum, where it is the file's tail: its frame ends the file, or
zeros run from its end to the file's (`zero_tail` from where its length
says it ends). A version before read it as the file's first checked
record and cut it torn where its sum failed. Until the fuzz fix's third
review the load judged a stopped walk only once a record had carried a
sum, and refused such a file by the collection the head names, where a
crash's stale bytes start at a record's head on a block's boundary
(`a_torn_tail_naming_an_unmade_collection_is_cut_in_a_file_of_no_sums`).
Whole by its sum it is refused by name, and so is one anywhere else,
plain records after it, whatever its sum, the file left as it is:
nothing in such a file says where a crash's tail begins, and judged,
it would be cut with every plain record after it. A version before cut
it so, and so did the third review's first fix, which judged such a
head wherever it framed; its check found it, and the decision taken is
to refuse it there, as the fuzz fix's walk did before that review. A
checked head whose varint does not read is refused, as every version
refused it there, and so is one of a kind this version does not know,
whatever its sum ("unknown record kind 143 at 1234" at the end of such
a file, its sum failing): a later version's kind is refused by its
number, a decision taken too, for the same reason. Otherwise a file
with no record carrying a sum keeps the old rule: a record cut short at
the end or zeros from a head (`zero_tail`, above) cut, any other stop
refused, a counter's head past the front among them, which read as one
moved the image's end.

The second version of this change looked past every damage a byte at a
time, summing every head that framed, within four times the bytes it looked
over and 64 MiB. The review found the bytes of vectors and of binary or
compressed bodies read as heads at about one byte in twenty, some with
lengths of megabytes: a `put` of 8 000 `vector<64>` rows cut 1.2 MB into
its 2 MB record was refused, where main cut it, and so was a page of zeros
inside a 1 MB unsynced record of them. Over a head lost before 4 MiB of
`vector<64>` rows (`a_head_lost_before_vectors_is_judged_by_the_records_
after_them`), 2 196 heads framed and passed the claim and the end's test,
225 of them past 16 KiB: summed one by one, 133 MB before the work allowed
ran out, 120 of them holding all but 2 MB of it; checked from the kept sums,
9 MB in all, 4 MB of it the sums kept, and the record after the vectors
found in 17 ms on this M1 (24 ms with the kind tested by two branches, a
coin each over a vector's bytes, before the bits of `HEADS`). A tear
(`a_tear_inside_a_large_vector_record_is_cut`, 3 MiB into 4 MiB) and a hole
in an unsynced record of vectors (`a_page_lost_inside_an_unsynced_vector_
record_is_cut`) are framed past, not looked through; the same hole where
the record after it claims past it (`..._synced_vector_record_refuses_the_
open`) is refused. The open checks such a record only where it ends past
D, which a claim makes a page short of its own record: a record wholly
below D is read as the image is, its damage `fennec verify`'s to find.

The first version of this change refused any record failing its sum with a
whole record after it, and stopped at the first head no record read from,
cutting there without looking past it. Its review found both wrong: a power
cut whose cache wrote the unflushed pages out of order -- a hole in one
record, a later one kept -- left a server that would not start, though it
lost nothing it answered; and zeros across a record's head in the middle of
a 202-row file cut it to one row, where main refused. The claim tells the
two apart; nothing else in a file does (`a_hole_in_an_unsynced_record_
before_a_whole_one_is_cut`, `a_page_lost_across_heads_before_a_claim_past_
it_refuses_the_open`, `..._past_the_durable_point_is_cut`). Rejected: a mark
appended after each fsync naming how far the file was durable (~20 bytes a
write under `--sync always`, where the next record's claim says it in one);
a length written beside the file (`<file>.synced`), another file a backup,
a restore and a rename must keep in step; the claim in the head, written
before an image's streamed body ends; the tail checked whole, which read
every page of 1 GB of 64 KB documents from the disk in 4.1 s against main's
1.5.

*What the judgment leaves.* A head damaged into one whose length runs past
the end reads as a torn tail and is cut, records after it unseen, as every
version before cut it; one damaged into a wrong length that fits, ending
by chance where a head frames, is framed past, the bytes it spans not
looked through; a whole record claiming past the damage directly
before a second head that does not frame fails the end's test and is not
counted, a later one counting in its place where there is one. A record
of no sum after checked ones, framed by its length, is below the point a
whole record after it claims, and refused as a flipped bit is. Stale blocks
of an older file -- FAT32, exFAT and ext4's `data=writeback` give a file's
new length its old blocks -- may hold that file's whole records past the
damage, their sums their own and their claims passing by chance: the open
then refuses where it could have cut, never the other way. That is not
rare. The third review put stale pages taken from a file written with
frequent syncs over the lost ones, past a power cut's point: 219 to 228 of
300 cuts were refused -- the stale file's records whole by their sums,
their claims past the damage -- and none lost a byte a sync had covered;
main refused about as often, any head it could not frame. Such a refusal
is the file left as it is, for a restore from the archive or a replica. A
seed for the sum written in each file would tell them apart, but every
file would need it from its first byte (one never compacted has no image
to carry it), a replica and an archiver would sum each record anew rather
than carry their primary's sum on, and a restore would sum every record of
the archive anew under the file's: a field in every header and a sum more
on every replica's write, for refusals that lose nothing. Not done.

*Which records carry one: the kind's bit, record by record.* A file a
version before wrote has none, and its records load as they read; once
this version appends to such a file, it holds both until its next compact
writes it anew. After a record carrying a sum, every record carries one: a
record with none after one with is a checked record whose high bit flipped,
which read so would take its claim and sum as its body -- refused ("the
record at 1234 carries no checksum, after records that do"). A replica of a
primary from before frames each record it takes as this version does
(below), and an archiver does the same (*Every reader of records*), so
this version never writes a file mixed the other way -- a restore
assembles one from an archive's image and segments. Before the archiver
did, the third review found a rolling upgrade's archiver -- upgraded first,
following a primary from before -- consolidating its first image as this
version writes one, checked, then keeping the primary's plain records
after it: an image and two plain `put`s opened at the image's end, two
writes short, so the restore ("the restored file is at change 2, not
4"), `verify` and every consolidation after it failed, and went on failing
after the primary was upgraded; a replica from before promoted did the
same. A test that met it (fennec-import's mirror, a plain record written
by hand after a snapshot) had been changed to write a checked one; it now
holds the open to the cut as well. The open's rule stays: read as a write,
a stale `2` past the point -- in a file whose tail no fsync covered --
would drop a collection. What a
one-bit flag leaves: the first checked record after a version before's (or
a file's first record) with its bit flipped reads as unchecked, nothing
before it to say otherwise; its claim and sum are then read as the end of
its body, which a data record's frames or a schema's decode may refuse and
may not. `fennec verify` does not find it either. Rejected: a flag in the
counter's head or a version byte for the whole file -- the four magics stay
as they are, a file's appends after an open of an old one would have gone
unchecked until a compact, and a file with no image (a new one, never
compacted) has no head to carry it; new kinds numbered 14 to 26, a `match`
arm each where the bit is one mask.

*What a binary from before does, and what it is sent.* It frames a checked
record by its length, which counts the claim and the sum, and refuses its
kind in words ("unknown record kind 131 at 8"; v0.1.12: "unknown record
kind 131"), never reading the sum as a record. A file this version wrote to
opens in no binary from before, so a rollback after the first write is a
rollback onto an older file (RELEASING.md). Its followers keep following,
though: a follower of this version says it reads sums (`sums=1` on
`/_replication`, and on a node's `POST /_replication/tenants`); one that
does not -- every one from before -- is sent each record with its claim and
sum taken off and its kind's bits cleared (`plain_records`), and an image as
a version before wrote it (`plain_image`, its counter's length patched;
`replication::plain_unless`). Its bytes are otherwise the primary's, so a
replica from before lands them, an archiver from before counts a block's
writes as it did and takes its images, its `fennec verify` and `fennec
restore` read what it kept, and the server from before opens the file its
restore wrote (`make upgrade-check`). What stays one way: a tenant moved
onto a node from before is refused at its install, its file being this
version's (the tenant stays on its source); and a replica from before
promoted is a primary writing records with none, which this version's
replicas and archivers of it take, each framing them anew.

*A replica's records.* A replica's `apply` holds each record its primary
sends to its sum before landing it ("a record of the primary's at 1234
fails its checksum"). The claim it came with is the primary's, of the
primary's file: the replica writes its own into the record before landing
it (`land_applied`) -- in place where it fits the record's width, else 0,
and the sum carried on from the head and body's rather than summed again
(`crc32c(head_body, claim)`: a CRC's state is its value) -- so a replica
that synced nothing never claims its file durable past what it synced
(`a_replica_claims_its_own_file_durable_never_its_primarys`). A record from
a primary from before, carrying none, is framed afresh with the replica's
claim, so a replica's file is checked from its first record and stays so
when it is promoted.

A record whose claim changed goes to the file as its parts -- the head and
body as they came, then the claim and the sum -- and they are appended
under one lock (`FileSink::record_parts`): the buffer's, or the disk's for
a record the buffer cannot hold, so that no durability writes the file
between them. A replica's syncer takes its durability under the database's
lock and runs it after, beside the next write, and the point it stores is
where the bytes it wrote end. Appended a part at a time, as the third
review's round had it, a sync could write out a record's head and body
alone and store that point, inside the record; every record after it then
claimed it, and a power cut that took the trailer alone -- whose bytes no
fsync covered -- refused the open ("fails its checksum, and the whole
record at ... says the file was durable past it") where it should cut. The
fourth review reproduced it by hand: the durable point at the end of a
20 KB record's body and its trailer zeroed, refused; at the record's start,
cut. `a_record_in_parts_is_never_split_by_a_sync` appends 20 000 records in
parts beside a thread that syncs as fast as it can, and holds every point a
sync stored to a record's end: the loop over the parts failed it in each of
five runs, the lock in none of fifteen. Every other record is appended
whole (`Sink::append`), and the durable point is a record's end by that.

*Every reader of records.* `archive::entries` judges a segment's records,
and an archiver writes them so that it can: `Archive::take` writes each
record it is sent as the segment's own (`claimed_into`), as a replica's
land does -- held to its sum, refused where it fails ("the primary's write
12: a record that fails its checksum"), and refused where it carries none
after one that did on the same stream ("... carries no checksum, after
records that do"): a primary writes all of its records with a sum or none,
so that is a checked record whose high bit flipped, its claim and sum read
as its body's end where it was framed anew. Each is damage
(`InvalidData`): the single archiver makes its stream again, and a node's
passes over that tenant alone until it follows the node again
([replication.md](replication.md#a-nodes-archive)). Its
claim is the segment's -- how far the archiver's fsyncs of the segment had
reached as it was appended (`Segment::durable`, set as each `sync_data`
returns; nothing on a segment the archiver went on with after a restart
until its first sync, which covers what the process before left) -- in
place where it fits the record's width, its sum carried on, and framed
anew otherwise, a primary from before's plain records among them. A
segment then holds no record without a sum after one with, so neither
does an image its consolidation writes nor a file a restore assembles;
`an_archiver_following_a_primary_from_before_checksums_restores_it_whole`
and `an_image_from_a_promoted_replica_from_before_checksums_restores`
hold a restore, a `verify` and two consolidations to every write, and
`make upgrade-check` this tree's archiver of a primary from before.

So a reader -- `fennec verify`, `restore`, a moment's read -- judges
damage in a segment as an open judges a file's (`judge_after`, `Laid {
prefix: 8, durable: D }`, D the newest claim of a whole record before
the damage): refused where a whole record after it says the segment was
synced past it ("the record at 1234 is damaged, and the whole record at
5678 says the segment was synced past it"), and otherwise a power cut's
hole in what the archiver had not synced, cut, the segment's end there
(`a_reader_cuts_a_hole_in_what_the_archiver_had_not_synced`). A record of
no sum after one with -- stale bytes, or a flipped bit, read as a write
that would have moved every number after it -- is damage too. Until the third
review an archiver kept the primary's records as sent, their claims the
primary's file's, and a reader judged by any whole record after the damage:
until the archiver started again and cut it, a power cut's hole in its
last unsynced writes refused every restore and `verify`, and so did a
copy of the archive taken then (the third review's LOW). The archiver
itself, resuming after a crash (`Archive::position`), still cuts the
damage of the segment it goes on with and everything after it, logged,
and fetches those writes again from its primary. A judgment there goes by
heads past a record that frames, a byte at a time only past a head that
does not, a candidate held to an entry's time before it (under 2^48 ms)
as well; and a record whose head or length runs past the segment's end is
that segment's end, nothing of it looked at -- looked through, a segment
ending in 256 KiB of vectors' `f32`s cut short was refused, so an archiver
killed mid-append left `fennec verify`, `restore` and `last_write`
refusing what main took as the segment's end
(`a_large_record_cut_short_ends_a_segment`, 4 MiB of them). `fennec
verify` holds every record of each image to its sum as well
(`check_records`), the one reader that reads an image whole. A sealed
segment's frame is covered by its tag already, and its plaintext's records
are judged as a plain segment's, their claims counted in its plaintext. A
restore copies a segment's records as they are: their claims the
segment's, they claim less than the restored file holds, which is synced
whole before it is renamed into place.

*What only looks.* `fennec verify data.fennec` and `fs::open_read_only`
(`fennec types`) write nothing, the sync log beside the file among it: on
Linux, after the machine went down, a durable write's bytes may be in the
log alone until an open to write puts them back (`synclog::recover`), and
read as the file stood they were a torn tail, or damage their sums
refused. Both read the file with the log's entries put back in memory
(`fs::read_with_sync_log`, `synclog::applied`: the same entries `recover`
writes, read where they differ from the file), the log and the file left as
they were (`a_reader_that_writes_nothing_reads_the_log`); with no log
naming the file, `verify` streams it and the open maps it, as before. Where
a log puts something back, the file is read whole into memory, `verify`'s
included: as much memory as the file for that one read, after the machine
went down and before a server or the library opened the file again -- an
open into memory (`fs::open_in_memory`) holds as much. A reader putting
the log's entries over the file as it streams would spare `verify` that;
not built, the case being that window alone. A
close's mark torn by a power cut after the close is cut by the next open
and not said as a cut (`torn_mark`): it was no write, and every byte
before it was durable (`a_torn_close_mark_is_cut_unsaid`).

*What a reader maps.* `fs::open_read_only` takes no lock (root CLAUDE.md,
*Single writer*), so a server may open the file while the reader walks it,
and a server opening a file after a crash cuts its torn tail
(`FileSink::cut`): a page of a mapping wholly past a file's end is the
process's end to read (`SIGBUS`), the reader's alone, no byte of the file
harmed. An open cuts where its walk stops, at a record failing its sum
that ends past the durable point the survey found, or where the spills the
walk ends with start (`load_records`' `whole`) -- never before the record
holding that point, nor before a run of spills, the close's marks among
them, reaching it. So on a Unix the reader first surveys the file by
reading it, a mebibyte at a time, never through a mapping
(`engine::surveyed`): the survey's own walk over the heads, stopping where
it stops, finding the same graphs, newest records and samples and so the
same durable point, which its load takes in the place of a walk of the
mapping (`Database::load_surveyed`) -- the heads are walked once, as
before -- and the runs of spills, with where a record starts kept every 64
KiB. It maps the file up to the last such start at or before the point,
rounded down to 64 KiB, the largest page of a system it maps on, and reads
the rest into memory of its own laid over the mapping's pages
(`Mapping::reading`: anonymous memory `MAP_FIXED` over them before
anything reads them, made read-only once read). What its walk reads past
the point is that copy, which no cut reaches, and a file cut by then is
covered as far as it was read; the load then covers the mapping only to
the records it took (`Mapping::cover`). What it read holds whatever the
file does after: cut, appended to, or replaced by a rename, which leaves
the reader the old file, whose pages its mapping keeps. What it reads
into memory has no bound but the file's: every byte past the point, and
the record holding it whole however large -- a graph a server kept in the
tail, a block's record of up to 16 MB, the spills of a block still open.
A server's file under `--sync always` is durable to within a page of its
end, so the copy is the last 64 KiB to 128 KiB and that record; under a
periodic sync (a server's default, 250 ms) the writes since the last one
too; under `--sync off`, running or after a crash, every write since the
server opened the file or last wrote it anew, a compact or a checkpoint,
until its shutdown syncs it; after a crash, the torn tail. A file no
sync ever claimed past its image -- a library's, written and let go of
with no `sync` -- is read whole past the image, and whole where it has
none, as one with a sync log beside it always was (`read_with_sync_log`). A file with nothing past its image -- what a
compact, a checkpoint, `.save` and a backup write -- is mapped whole, none
of it read: no cut lands inside an image (a walk stopping there refuses
the open), so the point no open cuts before is never short of the image's
end, and the file's length with nothing past it, its last page mapped
rather than read. Until 2026-11-21 that point was the last 64 KiB start at
or before it, which with no record after the image's last is that
record's start, of a collection with no graph its data: about the whole
file read into memory (`a_reader_maps_a_file_with_nothing_past_its_image_whole`
holds a `.save`'s and a checkpoint's to their length and to no byte of
their rows read, which before was every byte of them). The open is
cheaper for its survey, which walks a buffer read a mebibyte at a time
where it faulted mapped pages in (rusage's instruction counter, arm64
macOS, five runs a side, medians, 2026-11-21): the perf gate's `file-open`
file -- 100 000 rows, 6.56 MB, written with no `sync`, so read whole --
150.13 M -> 147.13 M instructions (-2.0%); the same rows synced and closed,
68 KB read, 139.98 M -> 134.90 M (-3.6%); with 4 KiB written past the
close, 72 KB read, 140.01 M -> 134.95 M; with 16 MiB past it, 16.4 MB
read, 297.29 M -> 294.39 M (-1.0%). What the copy costs is the gap
between those savings: nothing a run tells from its spread for 4 KiB,
about 2.2 M instructions for 16 MiB and 2.1 M for the 6.5 MB read whole --
and the bytes it holds, in memory of its own rather than the page cache.
A file with nothing past its image, mapped whole since its point is its
length, reads none: a `.save` of 100 000 rows of 1 KB in one collection
with no index, 102 475 488 bytes, read its data record into memory and
now reads nothing -- 70.70 M -> 16.18 M instructions (-77%), the
footprint the open added 103.4 MB -> 1.3 MB, the process's peak
footprint 104.5 MB -> 2.3 MB and its maxrss 105.8 MB -> 3.7 MB (the same
counter and runs, a release build of each tree, load 2.37 at the start
and the end, 2026-11-21).
`tests/reader.rs` holds the survey to the walk's and the point to the
open's cut over a file torn at every 4 093rd byte of its tail, cut short
or zeroed to its end -- an image with a graph, graphs in the tail, spills
landed and put back, records of 200 KiB -- and
`a_reader_that_looked_before_a_cut_walks_what_it_read` stops a reader
between its look and its walk, cuts the file with a server's open and
walks: mapped at its length, as the reader was until 2026-11-21, its walk
summed the torn record's pages past the cut and the process ended. A
byte-range lock on the file was the other way, read-held by the reader
across its load and write-taken by the cut: a server's start would wait
for a reader's load, or fail beside it, and a POSIX record lock is let go
of by any close of the file in its process. On Windows the reader maps the
file at its length still: its view refuses the cut
(`ERROR_USER_MAPPED_FILE`), so a server opening a file a crash left torn
fails by name until the reader is gone ([windows.md](windows.md)) -- not
changed here, no Windows build being at hand to try a view ending where a
copy begins.

*The sum.* CRC32C: the instructions compute it -- aarch64's `crc32c`, in
every Apple chip's target and found at run time elsewhere, and x86_64's
SSE4.2 `crc32`, found at run time -- three lanes at once over a stretch of
48 KiB, joined by two multiplications in the polynomial's field (one
instruction waits three cycles for the one before), and a record's last 7
bytes in three instructions rather than seven. On this M1 one 1 GiB buffer sums in 49 ms (22 GB/s, three lanes),
the same bytes as 400-byte records in 75 ms (14 GB/s) and as 60-byte ones
in 62 ms. Elsewhere,
wasm32 among them, eight tables a step (slicing-by-8), made at the first
use rather than carried as 8 KiB of the module: a table a byte, a byte a
step, made a 100-document `put` in the module 6 us slower than main's
(134 against 128), each lookup waiting on the one before, about 7 cycles a
byte. Each way is held to a bitwise CRC over every length around a word
and a stretch (`crc::tests`), the x86_64 one under Rosetta. Rejected: the
sync log's 64-bit multiply hash, which promises no burst it finds; xxHash
or a 64-bit CRC, code of our own on every target, never an instruction.

*What it costs.* A write sums its record once, as it lands, and its claim
is a load of an atomic and one byte: five bytes a record past main's
(1.2% of a file of 400-byte documents). Counted by `make instr-compare`'s way (tools/instr/compare.py: 8 re-signed
identities a build, 7 runs each, `replays.rs` through `handle`, main's
binary built with this branch's example), a lone `put` took 21 391
instructions against main's 21 236, +155 (+0.73%, outside two standard
errors): the first version measured +96 for the sum alone, the rest the
claim's -- the durable point loaded through the sink, its pages counted,
its byte written and summed. Measured again after the second review,
+217 (21 407 against 21 190, +1.02%, SE 41) -- the same code in another
draw of the allocator, each build moving about 70 between runs -- and
+50 against the build before it (inside two standard errors); a trace
of one `put`'s `handle` (`TRACE=fennec_http::handle`) counts that
review's change at -3 instructions of fennecdb's own code: the claim
reads the durable point from the database's own atomic, where it called
through the sink -- `replays.rs`'s one, a server's three
(`Holding`, `Tee`, the file's). A keyed `put` (a block of two writes) +242
(+0.29%, just outside two), and a `put` of 100 documents in one record
(`replays.rs`'s `batch`) -1 576 of 1 010 811 (inside two). The third
review put 130 to 150 of a lone `put`'s instructions in this change's own
code, 30 to 40 of them removable with the format as it is; taken out --
the claim written as a store of four bytes and the length cut back, where
`extend_from_slice(&claim[..w])` called `memcpy`; a lone record appended
as main appended it, where every append went through a loop over its
parts (`record_parts`, a replica's, alone loops now, under one lock); `crc32c` inlined --
its own code is 38 instructions fewer a lone `put` than the build before
by a trace of one `put`'s `handle` (the append's loop over one part -21,
`crc32c`'s call -20 less 15 inlined into `commit`, the sink's `record` -9,
a store for the `memcpy` -3), -41 by the plain counts (inside two standard errors). A lone
`put` measured +192 against main (21 370 against 21 178, +0.91%, SE 29),
and the trace counts fennecdb's own code at +118 against main's (`commit`
+101, `record_at` +13, `note_record` +3; the allocator's draw -254 there). A keyed `put` +337 (+0.40%, SE 82), its own code +171:
the block's record framed around its claim and summed (`framed_claiming`
+150, `frame_into` -57). The fourth review's round frames a block record
in place: its length counted from its heads first, the frames copied once
after its head, where main and the rounds before copied them into a body
and the body into the record, one allocation more. A keyed `put` then
measured -292 against main (83 568 against 83 860, -0.35%, SE 77) and a
lone `put`, whose path it leaves as it was, +196 (21 365 against 21 169,
+0.93%, SE 40); the trace counts the keyed `put`'s own code at +170
(`commit` +107 with the length counted, the claim and sum's `closed` +74,
`frame_into` -57) and a call each of `malloc`, `free` and `memcpy` fewer,
the allocator's paths for that allocation and the body's copy gone. The
claim is
what bounds the open: without it the 64 KB file read from the disk took
3.8 s against 1.5. In the browser module, in
Node (medians of 15 interleaved rounds), a lone `put` took 3.84 us against
main's 3.81 and a `put` of 100 documents 131.9 against 131.4 -- inside
their quartiles; the first version's 1.5% did not show again beside it
(131.2), each record summed once (`Block::record`; the journal's sink
appends it as it came). The module grew 6 607 bytes, 846 118 to 852 725
(344.5 KB gzip, 284.8 brotli): the first version's 2 168 -- `crc.rs` 758,
the sum's writers and the tables' builder -- 2 287 for what the first
review found missing, the look for a whole record past damage (`whole_at`,
`whole_after`, `judge`: 707), the walk that stops rather than refuses
(`Walk::next`, +434), the claim's writing and reading (`framed_claiming`,
`Rec`), the cut said (`Cut`) and 480 of data, the refusals' words; and
2 152 (+1 370 gzip, +840 brotli) for the second's: the judgment framed
first (`judge_after` 758, `head_at` 449, `Judging`'s claim and end tests
277), the sums a look keeps and the field's multiplication, now in every
build (`Judging::sum` 347, `sum_to` 331, `crc::mul` 86), the load's new
branches (`load_from` +318), less the look it replaced (`whole_at`,
`whole_after`: -707). A table of the field's squares made once, where
they are now made as each length needs them, was 220 bytes more for its
lock, a 256-byte table of the kind bytes 224 more than its 32 bytes of
bits, and a `take_while` over the trailing zeros 118 more than a loop.
The third review's round added 36 (852 761), `crc32c` inlined where it is
called; what it added for the archive and the survey is native alone. The
fourth's added 63 (852 824; 344.6 KB gzip, 285.1 brotli): the block
record's length counted from its heads, less `Laid`'s `None` branches; the
claim and sum's writer (`closed`) is left to the compiler, which, told to
inline it into both callers, made the module 51 bytes larger still. The browser opens files a server wrote (`openFile`), so it
judges damage as a server does; the survey, the downgrade to a follower and
`fennec verify`'s stream are native alone. A compact of that 1 GB file to
0.9 GB, its image summed in three lanes as it is written, took 561 to 944 ms
against main's 574 to 1 072: no difference out of the noise.

The archive judges its segments' heads with the same `zero_tail`
(`archive::entries`): a zero tail is cut where 11 zeros were read as a
write each, and any other zero head is refused once, where a run with a
byte after it was scanned again at each 11 zeros (a MiB of them, 9.8 s). A
sealed segment's frames (`seal::open_frames`) cut a frame head of zeros to
the end as a frame cut short; zeros inside a frame fail its tag. Judged
only at a head that reads as no kind, so a walk's known records pay
nothing; the browser module grew 369 bytes (845 749 to 846 118): 259 for
the cut and the refusals' offsets, 31 of them the image's, and 110 for the
byte before a zero head and `Walk`'s `edge` (a check that indexes the byte
before took 140, one that sets aside `Walk` and judges the head in the load
131).

The change counter record (kind 6) is at the front and fixed width;
the id counter (kind 7) exists so `compact` cannot hand out a deleted id again,
and in an image carries behind the counter the index of the data record right
after it (`Store::image_index`) -- each live document's id, payload length and
place, 4 bytes a document -- which a mapped open takes (`adopt_index`) rather
than walking the frames' heads, a chain of cache misses: 100 000 x 128 opened
in 8.4 ms against 14.4, and every reader of the record reads the counter
alone. Behind its documents the index says where each 16 384 of them end --
the byte, the id and the place the next share counts from, 24 bytes each,
and their number (`FRAMES_SPLIT`) -- and an open reads the shares on every
core, each into its own stretch of the dense id array (`adopt_shares`). Read
in turn through `get_uvarint` and `insert`, the index was 9.5 ms of a
million rows' open; a byte read inline and the next id pushed onto the dense
array (`push_above`), 4.8; in shares, a million rows of no vector open in
1.3 ms against 10.3 (`make opens-bench`). Ids that would leave the array a
quarter empty or reach the sparse map are read in turn, as `insert` places
them, and so is an index whose shares do not read as it says. A binary that
knows only `FRAMES_MARK` walks the record of a `FRAMES_SPLIT` image, as it
walks one with no index.

The mark is also what keeps a data op a later version adds refused rather
than misread. `adopt_index` reads no frame's op: a frame's head is the chain
of cache misses the index is there to skip (5.5 of a 14.2 ms open at
100 000 x 128, `Store::image_index`), and reading each op would give that
back. So under a mark this build knows, a frame of an op it does not is
read as the put an entry says it is, or not at all, where the walk refuses
it by its byte and its place ("unknown write op 3 at 4096", above). A version that adds an op
writes its images' index under a mark of its own, the next number:
`adopt_index` passes over a first byte it does not know (`false`, the store
untouched), the open walks the record (`replay_mapped`, `load_from`'s other
arm), mapped or read-only, and the walk refuses the frame; read into
memory, a load walks every record whatever the mark. A put's op turned into
another by damage inside an image is not seen by the index either, as a
payload moved within the record is not: an image is read unchecked, and
`fennec verify` holds it to its sums
(`an_image_index_under_a_mark_this_build_does_not_know_leaves_its_record_to_the_walk`).

Read into memory, a load copies each frame as it stands into the
segment `append` would put it in (`replay_noting`): framed afresh into a `Vec`
of its own first and copied again, it was 17 of a 25 ms load, now 9.6;
the history (kind 8) and a graph a server keeps in the tail (kind 4) are the
appended records that are not writes; a block (kind 9) holds a record for
each of its writes: a data record, or a create's, a drop's, an index's,
an alter's (kind 12) or a named statement's (kind 13, defined or dropped,
[named-statements.md](named-statements.md#what-is-built), or a scheduled
one's slot marked, [F3](named-statements.md#what-f3-has-built); a webhook
consumer's definition is op 6, its hook's name after its text,
[F4](named-statements.md#what-f4-has-built)), which an image writes again
after the collections, each schedule's last mark alone.
A head is written in one place (`head`, the counter's `image_head` aside)
and read in one (`record_at`), and a walk over a file's records is a
`Walk` -- a load, a repoint and the search for the last graphs each wrote
their own once -- which steps over the counter's fixed-width head and
stops at a record cut short for its caller to judge.
