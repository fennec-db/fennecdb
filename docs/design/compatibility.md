# The 1.x compatibility contract

What fennecdb 1.x promises, surface by surface, and the check that holds each promise today. The rule across them is one sentence: **a 1.x release only grows a frozen surface, and a break is a major version.** A surface no check holds yet names the item of the 1.0 plan that adds it; until that item lands the promise is held by review alone, and the table says so.

This file holds no invariant of the code on its own: each row's rule lives in the CLAUDE.md of the area that enforces it. The user's page is [site/content/docs/compatibility.html](../../site/content/docs/compatibility.html); what a release must check is RELEASING.md's *1.0* section.

## What grows, what breaks

*Grows* is an addition no existing caller, file or peer can meet as a change: a record kind or type tag a file takes only once a feature uses it, a route, an optional field, a flag, a statement. *Breaks* is anything else: a record or tag removed or reread, a statement that read one way and reads another, a route, field, flag, exit code, metric name or exported symbol removed or changed in type, a release asset renamed. A deprecation is not a removal: a surface is deprecated in a minor, kept through the rest of 1.x, and removed only in 2.0.

A feature whose first use moves the file format -- it writes a new kind or tag -- is named in that release's notes, because an older binary refuses the file (see [the upgrade order](#upgrade-order)).

## The frozen surfaces

| Surface | Rule | Held today by | Not held yet; added by |
| --- | --- | --- | --- |
| The `.fennec` file | Header `FENECDB\x01` (`engine::MAGIC`: the name from before the rename, then version byte 1). Record kinds 1 to 14 (`REC_*` in `engine.rs`; 8 is `history::RECORD`, 13 `stored::REC_STATEMENT`) and type tags 0 to 18 (`codec.rs`) keep their meaning; a new one takes the next number. 1.x opens every file written from v0.2.0 on, and a file a 1.x wrote opens in every later 1.x. | `tests/language.fennec` and `tests/statements.fennec` (opened by every later build; `FENNEC_WRITE_FIXTURE=1` rewrites `language.fennec`, so a change that rewrites a fixture justifies it in its PR until T2's per-release directories exist); the checksum-less file test in `persist.rs`; `linux-power-cut.yml` and `windows-power-cut.yml` | A directory per release, written by that release's own binaries and opened by every later build: T2. The power-cut contract for the default mode and for nodes: T13 |
| FennecQL | No text that parsed reads as another tree. | `make fennecql-corpus` (`tools/fennecql_corpus.py`), comparing trees by their `Debug` text; run by hand at release time, in no workflow | The check in CI, on pull requests; compare by `Definition::canonical` and its digest, on pull requests and against a corpus frozen at v0.2.0: T6 |
| `GRAMMAR_EPOCH` | Stays 4 through 1.x; see [the policy](#the-grammar-epoch). | A stored statement's digest, checked at every open (`named.rs`) | T6, as the row above |
| The HTTP API and its OpenAPI document | The REST API only grows: no operation, parameter, field, status or schema removed or narrowed, no optional made required. | `make openapi-check`: `fennec-server`'s `tests/openapi.rs`, then `tools/openapi_compat.py` against `integrations/openapi/{file,node,router}.json` and against `origin/main`'s own checker; `ci.yml` runs the same steps inline on pull requests, not the make target. A break needs a `BREAKS` entry and a moved `DOCUMENT_VERSION` ([http.md](http.md#compatibility)). | Each SDK against a server of another version: T4 |
| `fennec.h` and the native library | A function's prototype and a `FENNEC_*` code's value never change; one is added beside them. | `crates/fennec-ffi/tests/ffi.rs` holds each code's value | The prototypes' dump, taken at the 1.0 tag: T5 |
| The JS types | `web/fennec.d.ts` and `web/client.d.ts` only gain declarations. | `make types-check` (every export declared, a caller's code under `tsc --strict`, then every TypeScript example in the docs); `make builder-golden` and `make schema-golden` hold the builder's and the schema's output | The exports' dump, taken at the 1.0 tag: T5 |
| Each SDK's public API | Python, JavaScript, Go, .NET, Swift, Kotlin and Dart keep every public name and signature they ship at 1.0. | Each SDK's own tests (`make python-test`, `go-test`, `dotnet-test`, `swift-test`, `kotlin-test`, `dart-test`, `languages-test`) and `make docs-builder` | A dump per language (those needing an outside tool come in 1.0.x): T5; each published SDK against HEAD's server: T4 |
| The sync protocol | A replica of one release syncs with a server of another within 1.x. | `make sync-scenarios-check`: both runners (`web/` and `fennec-abi`) pass every scenario of `integrations/sync-scenarios.json` | A published SDK against HEAD's server: T4 |
| Replication and archives | A newer replica follows an older primary. An older replica or archiver stops by name on a record kind or tag it does not know. | `make upgrade-check` (`tools/upgrade-order.sh`) drives record kind 13 alone against the last main before it, by hand and not yet checked in CI (T3); the in-tree "from before" tests | The check in CI, released binaries as the old side, in both directions, a restore, a router and a rolling pair of nodes; tags 17 and 18: T3 |
| Sealed archives and backups | A sealed file opens with its key in every later 1.x. Magic `FENECSL\x01` (`seal.rs`); an archive's log segments begin `FENECLG\x01` (`archive.rs`). | The seal's and the archive's tests in `fennec-http` (`tests/archive.rs` restores a sealed archive with its key); they use `seal::MAGIC` and the archive's constant, so no test asserts the `\x01` bytes themselves yet | G8 moves the seal to `FENECSL\x02`, binding the file into each frame: 1.x still reads `\x01`, and an older binary refuses `\x02` by name |
| The browser's stored data | A page's IndexedDB image and journal and an OPFS file restore in every later 1.x. The layout (`fennec_journal`, `fennec_drain`, `<name>~`) is not itself frozen. | `web/fennec.test.js` and the persistence tests under `make test` | A real Chrome, Firefox and Safari: T53; an image written by v0.2.0's module: T2 |
| CLI flags and exit codes | A flag of `fennec-server`, `fennec` or `fennec-shard` is not removed or reread; an exit code keeps its meaning. | Each command's own tests | T5's api-golden dumps hold the CLI usage text and the exit codes beside the API's |
| `/_metrics` names | A metric's name, type and labels are not removed or renamed; a metric is added. | The server's tests of its scrape; the dashboards read the names (`make grafana-check`) | T5's api-golden dumps hold the metric names |
| Release asset names | The target archives, Windows zips, `fennec-web`, the AAR, the XCFramework zips, the `.deb` and `.rpm` and `SHA256SUMS`. | In CI, `site/test_install.sh` builds the four target tarballs (`fennec-v<version>-<target>.tar.gz`) and `SHA256SUMS` and installs from them, the Windows job checks the zip's name against its pattern, and `tools/homebrew-formula.py --verify` reads the latest release's tarball names. At a release, `tools/linux-packages-check.sh`, `tools/windows-zip-check.ps1`, `tools/install-ps1-check.ps1` and `release.yml` check each file | The rest by hand: RELEASING.md's table and its *1.0* checklist |

## Not frozen

- The Rust crates' API: none is published, and `fennec-core`'s items change in any release. Their behaviour through the frozen surfaces is what holds.
- `fennec-wasm`'s exports: internal to `@fennecdb/web`, which ships `web/fennec.js` and `web/fennec.wasm` together.
- The IndexedDB and OPFS layout. A page's stored data restores in every later 1.x; how it is laid out may change.
- The studio's pages and the desktop app (beta). The studio's calls are the API's, frozen as it is.
- Measured numbers, benchmarks and their harnesses, `examples/`.
- 0.x: it promises nothing, and a 0.x client has no compatibility with a 1.x server.

## Upgrade order

Replicas before primaries, and files go one way. `deploy/RUNBOOK.md`'s *Upgrading with no downtime* is the long form: archivers, then routers, then every node holding a replica or standby, then primaries.

1. A newer replica follows an older primary: it reads every record the older one writes.
2. An older replica meeting a record kind or tag it does not know stops by name, never reading it as another: it refuses the write ("record kind 13 is not a write") and connects again, an archiver keeps the stream's bytes and takes no image of its own, and an older `verify` or `restore` refuses what it kept. `make upgrade-check` drives kind 13 today, by hand and not yet checked in CI (T3); T3 drives it with released binaries, and tags 17 (`nocase`) and 18 (a text index beside another).
3. A newer file's first use of a feature, not its install, is what an older binary refuses: kind 13 at the first `create statement`, tag 18 at a field's second index. Roll forward; a rollback is promised only before a feature's first use.

## The grammar epoch

`GRAMMAR_EPOCH` is `pub const GRAMMAR_EPOCH: u32 = 4` in `crates/fennec-core/src/named.rs` (`fennec_ql::GRAMMAR_EPOCH` is that one). A named statement is stored as its text, the epoch it was parsed under and its tree's digest; an open parses it again and marks it broken, its calls refused until it is defined again, where the digest differs. It was 4 at v0.2.0 and is 4 now, but 2, 3 and 4 each marked stored definitions broken. Under 1.x:

- The epoch stays 4. A change that would raise it is not made.
- Raising it is a major version, and its notes name the stored statements it breaks.
- A statement an open marks broken under a 1.x release is a bug in that release.

## Support

The latest 1.x minor gets every fix. The previous minor gets security fixes for six months after the next minor's release. `release/1.N` branches are cut when a backport needs one. Release candidates are tagged `1.0.0-rc.N`; the owner pushes tags.
