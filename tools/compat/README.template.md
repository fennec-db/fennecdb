# The fixture set of {server}

Written by `tools/compat_fixtures.py write` with the release's own binaries,
built from its tag by `tools/compat_fixtures.py build` ({rustc}, node
{node}): `{server}`, `{cli}`, the tag's `web/fennec.js` and the module its
`make wasm` built, and `tools/compat/writer.rs` built as an example of the
tag's fennec-core for what the binaries cannot write at a few kilobytes.
Every later build opens each file and answers each statement alike:
`compat::` in fennec-core's, fennec-http's, fennec's and fennec-server's
tests, and `web/fennec.compat.test.js`. `SHA256SUMS`, written last, names
every file by its sum: `make version` refuses to move past the release
while a file differs.

| File | Written by | Holds |
| --- | --- | --- |
| `server.fennec` | fennec-server (`--replication-token`, `--no-checkpoint`, `--hooks`, `--idempotency-ttl`) | an image (`compact`) and the log after it: record kinds 1 to 9, 12 and 13 -- the history forked, a block across collections and a block holding every schema change a block takes (a collection made and dropped, an index, a field, a statement), every field change (added, renamed, dropped, its expiry moved: `alter field exp @ttl(2d)`), a collection dropped -- every type tag and every index kind but a quantized graph's, a statement record of every op (a named statement replaced, a write-bound one, a scheduled one's slot and its drop, a webhook consumer and the event it took), and the server's own collections: a write kept by its `Idempotency-Key` (`_idempotency`), a consumer of the changes (`_consumers`), a migration run by `POST /_schema/apply` (`_migrations`), the event's key (`_hook_events`) |
| `server.http.json` | fennec-server, started again on `server.fennec` | how it was served (flags, hooks line, secret, token), its answer over HTTP to every statement of `server.fennecql`, and to the requests that reach its own collections by their own paths: the keyed write sent again, the event sent again, the consumers, the schema, the migration's plan |
| `replica.fennec` | fennec-server `--replica-of` the server above | a replica's file: an image whose history follows its primary (kind 8, following), and the primary's records after it |
| `promoted.fennec` | fennec-server `--promote` on a copy of `replica.fennec` | the replica promoted: its history forked twice, a write of its own |
| `shell.fennec` | the `fennec` shell, a `-c` an open and a close | a file an app's own process keeps, a field's expiry set on it (`alter field at @ttl(36500d)`) |
| `graph.fennec` | fennec-server's shutdown | the checkpoint: graphs (version 9) of every metric, precision and quantization -- f32, f16, int8 over an f32 and an f16 field, bit holding its vectors whole and bit codes with their centres over an f32 and an f16 field (2 100 vectors of 4 dimensions, past `BIT_TRAIN`) -- tombstones in each arena carrying their vectors, rows sharing a vector (aliases), a row whose id takes 8 bytes, and the copies table's halves |
| `kept.fennec` | writer.rs `kept` (`set_graph_saves`, `save_graphs`) | graphs kept in the file's tail (version 10), tombstones in them, writes after them, and the close's mark that the file was synced (kind 14) |
| `waiting.fennec` | writer.rs `waiting` (`fs::open_serving`, `checkpoint`) | an image whose graphs hold nodes waiting to be linked (flag 2), as a server's checkpoint writes them before its linker took them |
| `spill.fennec` | writer.rs `spill` (`set_spill`) | a block that spilled (kind 10) and landed (kind 11), one that spilled and was put back -- its spills dead, a write after them -- and kind 14 |
| `synclog.fennec`, `synclog.fennec.sync` | writer.rs `synclog` (`keep_sync_log`) | a file cut back to its last fsync and the sync log holding the rest: the machine went down. The log is its fixed 260 KB, zeros past its entries, as written |
| `torn-tail.fennec` | writer.rs `torn-tail` | a last record cut short: the process ended inside an append past the last fsync |
| `torn-mark.fennec` | writer.rs `torn-mark` | a close's mark (kind 14) whose claim and sum never reached the disk, read back as zeros |
| `backup.sealed` | `fennec backup --key-file key` | a sealed backup of `server.fennec`'s server |
| `archive/` | `fennec archive --key-file key` | a sealed archive: a base image and the segments after it |
| `key` | `fennec key` | the seal's key, the fixture's own |
| `page-idb.json`, `page-idb-sealed.json` | web client `persist` (`{cryptoKey}`: `PAGE_KEY` in `tools/compat/web.mjs`) | IndexedDB's records, an image and its chunks, bytes in base64, under the database, version and store the page made; a migration its declared schema applied (`_migrations`) and a replica synced with a server of the release, then a write made offline (`_sync_shapes`, `_sync_queue`, `_sync_temps`) |
| `page-opfs.fennec` | web client `openFile` | the OPFS file, the same writes |

Beside each, `<name>.fennecql` holds the statements -- every reading
statement kind (`lookup`, `fuse`, `rerank`, `highlight`, `snippet`,
`facet`, `fuzzy`, `prefix`, `approx_count_distinct`, groups) and every
index through its own operator -- and `<name>.answers` the release's
answers, a line each as `json::response_to_string` writes them (the file
opened read-only by writer.rs `answers`; a sealed file's or an archive's
restored first by the release's `fennec restore`), and `<name>.web` the
page's (`JSON.stringify` of `run`), read back by a page opened again.

## What the writer lowers, and why each file is still one a release writes

| File | What is lowered or bypassed | Why its layout is one a v0.2.0 process writes |
| --- | --- | --- |
| `spill.fennec` | The spill threshold, 16 MB (`SPILL_AT`), lowered to 2 KB through `set_spill`, the engine's own knob | A spill's records are the same whatever the threshold: the block's records as the block would have written them, cut where it outgrew the threshold. A server writes the same kinds 10 and 11 past 16 MB |
| `kept.fennec` | The graph's save thresholds, 10 000 changed nodes and a file grown by three times the graph (`GRAPH_SAVE_CHANGES`), lowered through `set_graph_saves`; `save_graphs` called as the server's keeper calls it | The thresholds say when a graph is due, not what its record holds: the record is `serialize_graph_into(kept)`'s, version 10, as a server's keeper appends it |
| `waiting.fennec` | A server's open (`fs::open_serving`) and its checkpoint, called back to back before any linker thread runs | The race a server can lose -- a checkpoint before its linker took the tail's vectors -- made certain: the record is the checkpoint's own, nodes flagged 2 |
| `synclog.fennec`, `.sync` | The sync log is Linux's and Android's (`synclog::ENABLED`); `keep_sync_log` turns it on in this build on macOS, as the release's own tests do. The machine's crash is the process ending with no close (`mem::forget`) and the file cut back (`set_len`) to where the log's generation began | The log's format is the same on every Unix that keeps it, and its header names the file by device and inode, which the tests stamp with their copy's; a power cut leaves the file at its last fsync, the log holding the rest, which is what the cut makes |
| `torn-tail.fennec`, `torn-mark.fennec` | A crash made by `mem::forget` (no close) and a cut or zeroed tail | A crash leaves exactly these: an append past the last fsync whose last bytes never reached the disk, and a close's unsynced mark whose later bytes read back as zeros |

The next item of the 1.0 plan (T3) runs v0.2.0's released Linux binaries
against HEAD in CI: that is where a real Linux server's files, its sync
log among them, a tenant node's directory and a router's file meet HEAD.

## Not in any file, and why

What `RELEASES` in `crates/fennec-core/tests/compat.rs` names is in a file
of this set, and the walk there fails the set otherwise. v0.2.0 can also
write, and no file here holds:

- A record's claim of 2 to 4 bytes: a claim counts pages, so one byte
  reaches a durable point 1 MiB into the file; a set kept under 1 MB in
  git holds none past it.
- A graph's links of 3 or 4 bytes: a graph of more than 65 536 nodes.
- `_follow`, `--follow`'s record of which collection follows which slot:
  it needs a PostgreSQL server to follow.

Graph versions 3 to 8, records with no sum and the plain signs of a bit
index before its centres are older binaries' files: v0.2.0 reads them and
writes none, and 1.x opens every file written from v0.2.0 on.

## Not the same twice

Written once: the history's id (drawn at random when
`--replication-token` forks it, at a promotion and at each restore), a
seal's and AES-GCM's nonces and a sealed page's generation, the scheduled
slot's time and its mark, the hook's arrival and the event's key's
expiry, a kept Idempotency-Key's time, the sync's queued write's key, the
archive's file names and times, and the sync log's generation and
identity -- its header's generation and the file's device, inode and the
header's hash, and every entry's hash.

Files: {files}.
