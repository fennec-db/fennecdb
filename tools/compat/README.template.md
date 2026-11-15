# The fixture set of {server}

Written by `tools/compat_fixtures.py write` with the release's own binaries,
built from its tag by `tools/compat_fixtures.py build` ({rustc}, node
{node}): `{server}`, `{cli}`, the tag's `web/fennec.js` and the module its
`make wasm` built, and `tools/compat/writer.rs` built as an example of the
tag's fennec-core for what the binaries cannot write at a few kilobytes
or on the machine writing the set. The binaries were built `--offline`,
the tag's `Cargo.lock` resolved from cargo's cache, with the toolchain
named above (the tag's `rust-toolchain.toml` says `stable`; `TOOLCHAINS`
in the writer pins the version its release was built with). Writing it
took {total} minutes: {took}.
Every later build opens each file and answers each statement alike:
`compat::` in fennec-core's, fennec-http's, fennec's and fennec-server's
tests, and `web/fennec.compat.test.js`. `SHA256SUMS`, written last, names
every file by its sum: `make version` refuses to move past the release
while a file differs. Every answer below was read at the clock in
`clock` (writer.rs's `CLOCK`, 2026-03-01T00:00:00Z), which a later build
reads its answers at: a scheduled statement's next slot is computed from
it, and no other answer moves with the clock.

| File | Written by | Holds |
| --- | --- | --- |
| `server.fennec` | fennec-server (`--replication-token`, `--no-checkpoint`, `--hooks`, `--idempotency-ttl`) | an image (`compact`) and the log after it: record kinds 1 to 9, 12 and 13 -- the history forked, a block across collections and a block holding every schema change a block takes (a collection made and dropped, an index, a field, a statement), every field change (added, renamed, dropped, its expiry moved: `alter field exp @ttl(2d)`), a collection dropped -- every type tag and every index kind but a quantized graph's; `shapes`, a field's every alternative a schema writes: `required`, `collate tr nocase` with a `@text` beside its `@unique`, a collation or `nocase` alone beside a `@text(chars)` or `@text(accents)`, `@text(chars, accents)`, a list of every type a list takes (`[[int]]` among them), a descending part of `@sorted(a desc, b)`, a path's `@hash`, `@sorted` and `@unique`; a statement record of every op: a named statement replaced, a binding of each of the seven sets of events, a scheduled one's slot that ran and another's refused (409, `tock`), schedules of months, a cron, a named zone and an offset (defined and dropped in one block), a webhook consumer and the event it took; and the server's own collections: a write kept by its `Idempotency-Key` (`_idempotency`), a consumer of the changes (`_consumers`), a migration run by `POST /_schema/apply` (`_migrations`), the event's key (`_hook_events`); `retired`, whose highest row was deleted before the compact, so the image's id counter (kind 7) is past every live id |
| `server.http.json` | fennec-server, started again on `server.fennec` | how it was served (flags, hooks line, secret, token), its answer over HTTP to every statement of `server.fennecql`, and to the requests that reach its own collections by their own paths: the keyed write sent again, the event sent again, the consumers, the schema, the migration's plan |
| `replica.fennec` | fennec-server `--replica-of` the server above | a replica's file: an image whose history follows its primary (kind 8, following), and the primary's records after it |
| `promoted.fennec` | fennec-server `--promote` on a copy of `replica.fennec` | the replica promoted: its history forked twice, a write of its own |
| `shell.fennec` | the `fennec` shell, a `-c` an open and a close | a file an app's own process keeps, a field's expiry set on it (`alter field at @ttl(36500d)`) |
| `graph.fennec` | fennec-server's shutdown | the checkpoint: graphs (version 9) of every metric, precision and quantization -- f32, f16, int8 over an f32 and an f16 field, bit holding its vectors whole and bit codes with their centres over an f32 and an f16 field (2 100 vectors of 4 dimensions, past `BIT_TRAIN`) -- tombstones in each arena carrying their vectors, rows sharing a vector (aliases), a row whose id takes 8 bytes, and the copies table's halves |
| `kept.fennec` | writer.rs `kept` (`set_graph_saves`, `save_graphs`) | graphs kept in the file's tail (version 10), tombstones in them, writes after them, and the close's mark that the file was synced (kind 14) |
| `waiting.fennec` | writer.rs `waiting` (`fs::open_serving`, `checkpoint`) | an image whose graphs hold nodes waiting to be linked (flag 2), as a server's checkpoint writes them before its linker took them |
| `spill.fennec` | writer.rs `spill` (`set_spill`) | a block that spilled (kind 10) and landed (kind 11) -- every schema change a block takes in its first spill and in its land -- one that spilled and was put back -- its spills dead, a write after them -- and kind 14 |
| `synclog.fennec`, `synclog.fennec.sync` | writer.rs `synclog` (`keep_sync_log`) | a file cut back to its last fsync and the sync log holding the rest: the machine went down. The log, its fixed 260 KB, in its first pass: its entries, then zeros |
| `synclog-torn.fennec`, `synclog-torn.fennec.sync` | writer.rs `synclog-torn` (`keep_sync_log`) | the log past its first pass -- this pass's entries, then the rest of the last pass's: the tail of an entry written over, its whole entries, zeros -- and the same crash inside the log's last write: that entry's head whole, the back half of its bytes zeros, the write it held lost |
| `torn-wide.fennec` | writer.rs `torn-wide` | claims of two bytes (records more than 1 MiB past the last fsync), a record length of four bytes (one write of 2.3 MB), collection ids of two, three and four bytes (2 097 152 collections made and dropped before a compact), an image's index of a data record in two shares (16 400 documents); a synced row with a bit a disk flipped, below the point the two-byte claims name, which an open reads unchecked; and the first write after that fsync with a page a power cut lost, past it, where an open cuts the file, the writes after it going with it. The process ended with no close: no mark. `fennec verify` refuses the file |
| `torn-tail.fennec` | writer.rs `torn-tail` | a last record cut short: the process ended inside an append past the last fsync |
| `torn-mark.fennec` | writer.rs `torn-mark` | a close's mark (kind 14) whose claim and sum never reached the disk, read back as zeros |
| `synclog-spent.fennec`, `synclog-spent.fennec.sync` | writer.rs `synclog-spent` (`keep_sync_log`) | `synclog`'s crash, then an open that put the log's entries back, synced the file and wiped the log's header, and ended before it wrote: a header of zeros, the spent generation's entries after it |
| `synclog-unbegun.fennec`, `synclog-unbegun.fennec.sync` | writer.rs `synclog-unbegun` (`keep_sync_log`) | a log made by the first sync and never begun, all zeros -- its header goes out with its first entry, which no later sync wrote -- beside a file whose last write was written behind, unsynced |
| `schedules.fennec` | writer.rs `schedules` (`allow_schedules`, `set_clock`, `run_slot`, `mark_slot`) | a scheduled statement of every timing alive to the file's end, each with its last slot marked as a server's scheduler marks it: every minute (UTC), a day on `+03:00`'s clock, a week (UTC, from its Monday), a month and `30 4 * * *` on Europe/Berlin's, `0 12 * * mon` on `-05:00`'s whose slot was refused (409), and `fresh`, a week on Asia/Tokyo's defined after the runs, whose `since` alone says no slot is due |
| `ids.fennec` | writer.rs `ids` | collection ids of five bytes: 268 435 456 collections made and dropped, the file compacted every 8 388 608, then `far` made past them; its highest row deleted in the tail, where the id counter is its put's frame alone |
| `follow.fennec` | fennec-server `--follow` a PostgreSQL table | the table's copy, then an insert, an update and a delete streamed through its slot, and `_follow`: which collection follows which slot, and that its copy is whole |
| `deep.fennec`, `refused-lists.fennec`, `refused-dims.fennec`, `points-deep.fennec` | the `fennec` shell | what v0.2.0 writes and a later build reads otherwise on purpose (docs/design/crates.md, *How deep a value nests* and *Limits*): a `[[json]]` field -- a list of json inside a list, which v0.2.0 takes where it refuses `[json]` -- whose row nests 66 levels in all, refused where it is read, beside an ordinary collection; a field of 65 nested lists and SQL's array of vectors of 1 000 000 001 dimensions, each refused at the open; a point under 64 lists, its row refused where it is read, and its schema at the open by a change that follows |
| `backup-wide.sealed` | `fennec backup --key-file key` of a server holding 1.1 MB of bytes no compressor shrinks | a file sealed whole in two frames, the last flagged as the last in what its tag covers |
| `archive.verify`, `archive-plain.verify` | `fennec verify` of each archive | what the release said of it, whole: images, segments, the changes it restores and the times they span |
| `node-archive/`, `node-archive.json` | fennec-server `--dir` (a tenant node), `fennec archive --tenants --image-every 1s` of it, then `fennec prune --tenants --keep 1s` | a line a tenant -- `a`, its first image let go of and its `began` written before; `b`, deleted, noted `gone` in its `presence`, made again under its name: its first line moved aside to `b.<history>/` and a new one begun; `c`, moved away (its image taken, deleted, installed again), noted `gone` and `back` -- and in the JSON what the release's `prune` and `verify --tenants` said, and its `restore --tenants` to the end and to moments before the tenants, with all three, with `b` gone, with `c` away and with `b` back: what it said and each restored tenant's answers to `node.fennecql` |
| `backup.sealed` | `fennec backup --key-file key` | a sealed backup of `server.fennec`'s server: a file sealed whole, one frame |
| `archive/` | `fennec archive --key-file key` | a sealed archive: a base image and its history sealed whole, a segment a frame for each batch of writes |
| `archive-plain/` | `fennec archive`, beside the sealed one | the same archive unsealed: `FENECLG\x01` and each write's time and record |
| `key` | `fennec key` | the seal's key, the fixture's own |
| `page-idb.json`, `page-idb-sealed.json` | web client `persist` (`{cryptoKey}`: `PAGE_KEY` in `tools/compat/web.mjs`) | IndexedDB's records, an image and its chunks, bytes in base64, under the database, version and store the page made; a migration its declared schema applied (`_migrations`) and a replica synced with a server of the release, then a write made offline (`_sync_shapes`, `_sync_queue`, `_sync_temps`) |
| `page-opfs.fennec` | web client `openFile` | the OPFS file, the same writes |
| `page-opfs-stopped.fennec`, `page-opfs-stopped.fennec~` | web client `openFile`, its `compact` stopped | a rewrite the page stopped after its copy beside the file was flushed and before the file was touched: the file whole as it stood, the copy (`<name>~`) the compact's image; `page-opfs-stopped.web` the page's answers once it opened the two again and took the copy |

Beside each, `<name>.fennecql` holds the statements -- every reading
statement kind (`lookup`, `fuse`, `rerank`, `highlight`, `snippet`,
`facet`, `fuzzy`, `prefix`, `approx_count_distinct`, groups) and every
index through its own operator -- and `<name>.answers` the release's
answers, a line each as `json::response_to_string` writes them (the file
opened read-only by writer.rs `answers`; a sealed file's or an archive's
restored first by the release's `fennec restore`), and `<name>.web` the
page's (`JSON.stringify` of `run`), read back by a page opened again.
Every file holds at least four statements' answers.

## What the writer lowers, and why each file is still one a release writes

| File | What is lowered or bypassed | Why its layout is one a v0.2.0 process writes |
| --- | --- | --- |
| `spill.fennec` | The spill threshold, 16 MB (`SPILL_AT`), lowered to 2 KB through `set_spill`, the engine's own knob | The record kinds and their bodies are the ones a 16 MB block writes: the block's records as the block would have written them, cut where it outgrew the threshold. Not the widths: at 2 KB every claim is a byte and every length one or two, where a 16 MB block's spills take lengths of four bytes and its land a claim of two -- which `torn-wide.fennec` holds, written with no knob |
| `kept.fennec` | The graph's save thresholds, 10 000 changed nodes and a file grown by three times the graph (`GRAPH_SAVE_CHANGES`), lowered through `set_graph_saves`; `save_graphs` called as the server's keeper calls it | The thresholds say when a graph is due, not what its record holds: the record is `serialize_graph_into(kept)`'s, version 10, as a server's keeper appends it |
| `waiting.fennec` | A server's open (`fs::open_serving`) and its checkpoint, called back to back before any linker thread runs | The race a server can lose -- a checkpoint before its linker took the tail's vectors -- made certain: the record is the checkpoint's own, nodes flagged 2 |
| `synclog.fennec`, `synclog-torn.fennec`, `.sync` | The sync log is Linux's and Android's (`synclog::ENABLED`); `keep_sync_log` turns it on in this build on macOS, as the release's own tests do. The machine's crash is the process ending with no close (`mem::forget`) and the file cut back (`set_len`) to where the log's generation began; in `synclog-torn` the back half of the log's last entry zeroed | The log's format is the same on every Unix that keeps it, and its header names the file by device and inode, which the tests stamp with their copy's; a power cut leaves the file at its last fsync, the log holding the rest, which is what the cut makes, and one inside the log's own write leaves an entry whose bytes did not all reach the disk |
| `torn-wide.fennec` | Nothing lowered: the writes as a server makes them between two fsyncs. The process ends with no close (`mem::forget`); one bit of a synced row is flipped, and one of the first write after the fsync | A server's file after a write of megabytes and a crash: a disk's flipped bit below the durable point, read unchecked, and a page a power cut lost past it, cut -- what an open reads there is what its claims say |
| `torn-tail.fennec`, `torn-mark.fennec` | A crash made by `mem::forget` (no close) and a cut or zeroed tail | A crash leaves exactly these: an append past the last fsync whose last bytes never reached the disk, and a close's unsynced mark whose later bytes read back as zeros |
| `synclog-spent.fennec`, `synclog-unbegun.fennec`, `.sync` | `keep_sync_log` as above; the process ends with no close after the open that spent the log, or after a write behind the log's first sync | An open spends a log before it reads the file, whatever the process does after; a log is made by the sink's first sync and begun by its second |
| `schedules.fennec` | The clock: `set_clock` puts the definitions at 2026-01-01, the scheduler's look at 2026-02-15T10:00Z and `fresh` at 2026-02-26, where a server reads the system's; the slots run and marked by the engine's own calls a server's scheduler makes (`fennec_http::schedule::run_slot`: `begin`, `run_slot`, `mark_slot`, `commit`, or the refusal's mark alone) | A schedule's record holds its timing, zone and `since`, and a mark its slot, version, outcome and the slots passed over: what a server writes on the day those slots come, a day's slot once a day |
| `ids.fennec` | Nothing lowered: 268 435 456 creates and drops, a compact every 8 388 608 keeping the file small (a compact keeps the count, which the process holds) | A server's file that made that many collections, compacted as it grew |
| `follow.fennec` | PostgreSQL in a container of the writer's own, from the image `make pgvector-up` runs | `--follow`'s file as its server writes it beside any PostgreSQL 17 with logical replication |
| `page-opfs-stopped.fennec`, `.fennec~` | The file system's stand-in refuses the file's truncation once the compact's copy was flushed: the page gone there | A page closed in the middle of a rewrite leaves these two, as `web/browser/` reloads a real page at each step of a flush |
| `node-archive/` | `--image-every 1s` and `--keep 1s` where an operator gives hours and days | What an image and a prune write are the same at every period; the prune lets go of what the period says |

The next item of the 1.0 plan (T3) runs v0.2.0's released Linux binaries
against HEAD in CI: that is where a real Linux server's files, its sync
log among them, a tenant node's directory and a router's file meet HEAD.

## Not in any file, and why

`RELEASES` in `crates/fennec-core/tests/compat/walk.rs` names what v0.2.0
writes -- its record kinds, statement ops, graph versions, schema tags,
value tags and index kinds -- and places every alternative its encoders
emit that the walk tells apart (`ALTERNATIVES`): in a file of this set,
held both ways, or here with why. {forms} v0.2.0 can also write, and no
file here holds:

- `claim: 3 bytes`: a record more than 256 MiB past the file's last
  fsync, 256 MiB of working tree.
- `claim: 4 bytes`: a record more than 64 GiB past it.
- `head: length of 5 bytes`: a record of 256 MiB, one collection's data in
  an image that large, 256 MiB of working tree.
- `graph: links of 3 bytes`, `graph: steps of 3 bytes`: a graph past
  65 536 nodes, about 1.5 MB of links git cannot compress, restored and
  written again in every test run.
- `graph: links of 4 bytes`, `graph: steps of 4 bytes`: a graph past
  16 777 216 nodes.

And what v0.2.0 refuses to write, so no file of it holds: an `@hnsw`'s `m`
past 32 767 (its parser: "`m` is 2 to 32767, not 40000"), a field's own
`vector<N>` of 0 or past 10^9 dimensions and a list's `[vector<0>]` from
FennecQL ("a vector's dimension is 1 to 1000000000"); a `[vector<0>]`
made by the Rust API's `Schema::new` alone, which is not frozen, is held
by `crates/fennec-core/tests/damage.rs`.

This list is the set's, under its sums: once the set lands it is never
amended, as no file of it is. A form found later that v0.2.0 writes is a
finding against it, placed by the next release's line in `RELEASES` --
held in that release's set where it writes the form too -- since a
statement made after the release about what its files hold is one no file
of the set can bear out.

Graph versions 3 to 8, records with no sum and the plain signs of a bit
index before its centres are older binaries' files: v0.2.0 reads them and
writes none, and 1.x opens every file written from v0.2.0 on. A plain
backup is an image with a history record after it, as `promoted.fennec`'s
fork and `graph.fennec`'s image are.

## Not the same twice

Written once: the history's id (drawn at random when
`--replication-token` forks it, at a promotion and at each restore), a
seal's and AES-GCM's nonces and a sealed page's generation, the scheduled
slot's time and its mark, the hook's arrival and the event's key's
expiry, a kept Idempotency-Key's time, the sync's queued write's key, the
archive's file names and times, and the sync log's generation and
identity -- its header's generation and the file's device, inode and the
header's hash, and every entry's hash; the node's moments and the
moved-aside line's history; the PostgreSQL slot's positions behind
`follow.fennec`'s writes. `torn-wide.fennec` and the sync logs are text
repeated and zeros, which compress to a few percent; `backup-wide.sealed`
is a mebibyte of ciphertext nothing compresses.

Files: {files}.
