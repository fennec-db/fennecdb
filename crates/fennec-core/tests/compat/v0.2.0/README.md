# The fixture set of fennec-server 0.2.0

Written by `tools/compat_fixtures.py write` with the release's own binaries,
built from its tag by `tools/compat_fixtures.py build` (rustc 1.99.1 (58cf8f9c3 2026-10-13),
node v26.7.0): `fennec-server 0.2.0`, `fennec 0.2.0`, the tag's
`web/fennec.js` and the module its `make wasm` built, and
`tools/compat/writer.rs` built as an example of the tag's fennec-core for
what the binaries cannot write at a few kilobytes. Every later build opens
each file and answers each statement alike: `compat::` in fennec-core's,
fennec-http's and fennec's tests, and `web/fennec.compat.test.js`.

| File | Written by | Holds |
| --- | --- | --- |
| `server.fennec` | fennec-server (`--replication-token`, `--no-checkpoint`, `--hooks`) | an image (`compact`) and the log after it: record kinds 1 to 9, 12 and 13 -- the history, a block, fields added, renamed and dropped, a collection dropped -- every type tag and every index kind but a quantized graph's, and a statement record of every op: a named statement replaced, a write-bound one, a scheduled one (its slot's mark, then its drop) and a webhook consumer with the event it took |
| `shell.fennec` | the `fennec` shell, a `-c` an open and a close | a file an app's own process keeps |
| `graph.fennec` | fennec-server's shutdown | the checkpoint: an image whose graphs (version 9) are every metric and quantization |
| `kept.fennec` | writer.rs `kept` (`save_graphs`) | a graph kept in the file's tail (version 10), writes after it, and the close's mark that the file was synced (kind 14) |
| `spill.fennec` | writer.rs `spill` (`set_spill`) | a block that spilled (kind 10) and landed (kind 11), and kind 14 |
| `synclog.fennec`, `synclog.fennec.sync` | writer.rs `synclog` (`keep_sync_log`) | a file cut back to its last fsync and the sync log holding the rest: the machine went down. The log is its fixed 260 KB, zeros past its entries, as written |
| `backup.sealed` | `fennec backup --key-file key` | a sealed backup of `server.fennec`'s server |
| `archive/` | `fennec archive --key-file key` | a sealed archive: a base image and the segments after it |
| `key` | `fennec key` | the seal's key, the fixture's own |
| `page-idb.json`, `page-idb-sealed.json` | web client `persist` (`{cryptoKey}`: `PAGE_KEY` in `tools/compat/web.mjs`) | IndexedDB's records, an image and its chunks, bytes in base64 |
| `page-opfs.fennec` | web client `openFile` | the OPFS file |

Beside each, `<name>.fennecql` holds the statements and `<name>.answers`
the release's answers, a line each as `json::response_to_string` writes
them (the file opened read-only by writer.rs `answers`; a sealed file's or
an archive's restored first by the release's `fennec restore`), and
`<name>.web` the page's (`JSON.stringify` of `run`), read back by a page
opened again.

Not the same twice, so written once: the history's id (drawn at random
when `--replication-token` forks it, and at each restore), a seal's and
AES-GCM's nonces and a sealed page's generation, the scheduled slot's time
and its mark, the hook's arrival, the archive's file names and times.

Files: archive, archive.answers, archive.fennecql, backup.answers, backup.fennecql, backup.sealed, graph.answers, graph.fennec, graph.fennecql, kept.answers, kept.fennec, kept.fennecql, key, page-idb-sealed.fennecql, page-idb-sealed.json, page-idb-sealed.web, page-idb.fennecql, page-idb.json, page-idb.web, page-opfs.answers, page-opfs.fennec, page-opfs.fennecql, page-opfs.web, server.answers, server.fennec, server.fennecql, shell.answers, shell.fennec, shell.fennecql, spill.answers, spill.fennec, spill.fennecql, synclog.answers, synclog.fennec, synclog.fennec.sync, synclog.fennecql.
