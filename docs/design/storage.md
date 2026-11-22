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
(`Database::maintain`, `engine/maintenance.rs`): what the build reads is copied
under the read lock, the build holds no lock, and the write lock is taken only
to apply the writes made meanwhile and put the result in place. Those writes
are known exactly, not from the change ring a long build would overflow: every
write passes through `Database::note`, which hands the id to the `Tail` of each
maintenance on that collection -- so a write path that skipped `note` would
leave a built index missing it. A schema change there (another index, a drop)
fails the maintenance rather than installing what no longer fits. At 100 000 x
128 reads waited at most 21 ms through an HNSW build and 30 ms through a compact
(file rewrite included), against the full ~20 s under the write lock. Over a
mapped file a compact copies no record: the graphs holding tombstones are
rebuilt beside the database, then each store is cloned under the read lock --
the records in the old file and the sealed segments shared (`SegmentBytes` is
an `Arc`, in the browser module too now), the index copied, each graph's record written -- the live
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

## Compaction

**A file compacts itself once half of it is dead** (`engine/garbage.rs`).
An update appends the whole record and leaves the version it replaced dead
until a `compact`, which nothing ran on its own: under YCSB's updates a
million 1 KB records grew a buffered file to 4.7-6.5 GB on an 8 GB machine,
a live record a few to a page among dead ones, and one read in five waited
for the disk (C at one thread 30.1k ops/s, 443k over the file freshly
loaded). `Database::garbage` is the file's bytes as written (`appended`)
against the live records' (`Store::total_bytes - dead_bytes`, which the
stores counted as writes landed already and an image's index carries) and
what the last compact kept besides the documents (`kept`: schemas,
counters, graphs, the file less every record the stores hold -- the
versions a compact beside the writes copied and saw written over are dead,
and counted as kept they left a 307 KB file of 53 KB of rows not due
again) -- a sum over the collections, nothing added to the write
path: a lone put, put over and del measured 570/700/532 ns in memory either
way, and 666/797/595 against 660/785/593 over a mapped file, inside the base
build's own spread. `CompactPolicy` is the one rule (half the file dead and
64 MB, `compact_due`), `compact_when_due` a look under the read lock and a
compact beside the database, `Compactor` a thread that looks every 5 s. The
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
  (`build_graph` under the lock, `begin_graphs` and `finish_graphs` beside
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
the cache (`SideFile::past_cache`: `F_NOCACHE`, or synced and
`POSIX_FADV_DONTNEED` every 64 MB, since written pages stay until they are
on disk; none on Windows, whose `FILE_FLAG_NO_BUFFERING` asks for writes
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

*What a cut leaves.* The open that cuts appends a close's mark after the
cut (`REC_SYNCED`, its claim its own start) and fsyncs the two together
(`FileSink::cut`, `Database::cut_to`): every byte before the mark was
read by that open -- below D unchecked, past it held to its sums -- and is
durable once the fsync returns, and the mark says so, so the next open
finds D at the cut and reads the file as the open that cut it did. The
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
refuses. The mark is the record v0.2.0's closes write, and its survey
takes the mark's claim for D as this one's does, so a v0.2.0 binary reads
a cut file as this build does. It is written after the cut and before
the fsync: a crash before that fsync returns leaves the file as it was,
cut and marked, or cut with the mark lost or torn -- read as an unmarked
cut was, the one window left. An open that cuts nothing runs none of it.
The browser module reads no claim, checking every record past the image,
so its own cut (`openFile`'s truncate) needs no mark and makes none: it
reads a file a native open cut and marked as it read the file before the
cut (refused, both, where a whole record claims past a row failing its
sum; unmarked, it took the cut file by cutting the two synced rows too).
Measured (rusage's instruction counter, arm64 macOS, a release build of
each tree, 3 rounds of 15 opens, medians, 2026-11-22): an open cutting
a torn tail off a file of 24 rows takes 602 k -> 736 k instructions
mapped (576 k -> 709 k read into memory), of 10 024 rows 2.53 M ->
2.68 M (3.78 M -> 3.92 M); the same files without a tail open in the
same count either side (392 k -> 390 k, 2.258 M -> 2.269 M mapped, the
least of each run overlapping). The 133 k-145 k is the kernel's: a cut
and fsync alone, with and without the 8 bytes written between, differ
by 63 k on a file just written and by 128 k-147 k on an APFS clone,
as the measured copies were. `fennec-server`'s `__text` +512 bytes (the
release build, arm64 macOS; the file the same size, page-padded); the
browser module the same bytes, none of it built there.
Held by `a_file_an_open_cut_opens_again_as_that_open_read_it` (a
file torn as `torn-wide` is, cut by each way an open writes and opened
again by every way, read-only too) and
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
walks one with no index. Read into memory, a load copies each frame as it stands into the
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
