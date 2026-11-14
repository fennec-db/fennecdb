# Releasing fennecdb

A release is a tag. Its binaries, browser bundle and image are built from the
tag and left in a draft; the packages follow once the draft is published, so
nothing reaches a registry before the notes have had a read.

## A release

1. `make version V=X.Y.Z`: the one version goes into the workspace, the
   Python package, the npm packages, the NuGet package, the Kotlin and Dart
   packages, the Flutter plugin, `Package.swift`'s release, the image the
   README pulls and the examples' dependencies (`examples/`). The site needs
   nothing more: `site/build.py` writes the workspace's version into every
   `{{version}}` of the docs (Install's lines and the image's tag among them).
   Then `cargo check` moves `Cargo.lock`, and the change goes through a pull
   request like any other. Once the release is published, run the commands of
   the Quickstart, Quickstart: in the browser, Install and The shell once
   more: the outputs they show come from the release they install (the
   Quickstart's curl steps and its eight programs are held to their answers by
   `make languages-test`; the rest are compared by hand).
2. Build the Swift package's binary: `gh workflow run swift-binary.yml -f
   version=X.Y.Z`. SwiftPM fetches the package by its tag, and the
   `Package.swift` at that tag must already name the XCFramework's zip by
   its URL and checksum, so the zip is built before the tag: the run builds
   it from main, tests it on macOS and an iOS simulator, keeps it as its
   artifact and opens a pull request writing its checksum into
   `Package.swift` (`integrations/swift/set-binary.sh`). It keeps the
   `FennecFFIDynamic.xcframework.zip` beside it -- the frameworks, and
   macOS's binary again as a plain `libfennec_ffi.dylib`, which the Flutter
   plugin's macOS pod vendors -- built from the same commit and named by
   nothing. Merge it.
3. Tag what was merged and push the tag:
   `git tag vX.Y.Z origin/main && git push origin vX.Y.Z`.
   `release.yml` builds the binaries for Linux and macOS, the Windows zips
   (`fennec-vX.Y.Z-<arch>-pc-windows-msvc.zip` for x86_64 and aarch64, each
   on a runner of its architecture, `tools/windows-zip.sh`: the
   C runtime linked in, installed through `install.ps1` from the folder it
   was made in and run before it is kept, `tools/windows-zip-check.ps1`),
   the C library for Linux (`fennec-ffi-vX.Y.Z-<target>.tar.gz` for
   `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, each on a
   runner of its architecture, `tools/linux-ffi.sh`: the shared and static
   library, `fennec.h` and their pkg-config files, a C program linked to
   each and run on the runner and in AlmaLinux 9's glibc 2.34 before it is
   kept, `tools/linux-ffi-check.sh`), the `fennec-web`
   bundle, the Android AAR (`fennecdb-android-X.Y.Z.aar`, the native library
   for each ABI inside) and the multi-arch image on ghcr.io, takes the
   XCFramework zips step 2's run kept -- the static one held to the checksum the tag's
   `Package.swift` names, and stopping the release where none matches --
   and drafts the release with all of them and their checksums. It also
   makes a `.deb` and an `.rpm` for amd64 and arm64 from the musl tarballs
   (`tools/linux-packages.sh`, `packaging/nfpm.yaml`), each architecture on a
   runner of its own, installs them in a Debian and a Fedora container and
   runs what they installed (`tools/linux-packages-check.sh`) before they
   are kept, so a package that does not install stops the release, and
   lists them in `SHA256SUMS` with the rest. Beside `vX.Y.Z` it pushes `integrations/go/vX.Y.Z` at
   the same commit: a Go module in a subdirectory is fetched by a tag that
   names it, so that tag is the Go SDK's release (`go get
   github.com/fennec-db/fennecdb/integrations/go@vX.Y.Z`), and there is no
   registry to publish to. A release built again, by the tag or by a
   dispatch, replaces the assets of a draft; a published release's are never
   replaced, and it only gets the files it has no asset of, with a warning
   that its `SHA256SUMS` does not list them. The Homebrew formula, `install.sh`,
   `install.ps1` and the packages name the release's files by checksum, and a
   rebuild is not byte for byte the first build: a replaced tarball would fail
   every `brew install` and every sum already taken. To change what a
   published release holds, release a new version.

   **A power cut.** Every release needs the last run of
   `linux-power-cut.yml` green, as its Linux binaries are every release's:
   the commit's fennec-server, built as `release.yml` builds it, in an
   Ubuntu guest cut five times on ext4 and five on XFS under `--sync
   always` writes, compacts and spills, twice each with its disk's cache
   lost, the sync log a durable write
   takes on Linux applied at each open and every acknowledged write read
   back (docs/design/durability.md, "A power cut"): `gh run list --workflow
   linux-power-cut.yml --limit 1`, and if it ran before the commit being
   released, `gh workflow run linux-power-cut.yml --ref main` and wait for
   it.

   **Windows.** The zip is unsigned (SmartScreen warns of a program a
   browser downloaded; `install.ps1` takes the mark off what it checked),
   and nothing publishes it but the release itself: there is no winget or
   Scoop manifest yet. A release with a Windows build needs the last run of
   `windows-power-cut.yml` green -- a Windows guest cut five times under
   `--sync always` writes, compacts and spills, twice with its disk's cache
   lost, every acknowledged write read back (docs/design/windows.md, the
   owner's ninth decision): `gh run list --workflow windows-power-cut.yml
   --limit 1`, and if it ran before the commit being released, `gh workflow
   run windows-power-cut.yml --ref main` and wait for it. After the draft is
   published, run `irm https://fennecdb.com/install.ps1 | iex` once on a
   Windows machine and the Quickstart's Windows line, as the other
   installs are run once more.
4. Read the draft, then publish it: `gh release edit vX.Y.Z --draft=false`.
   `packages.yml` builds the three packages again, installs them where a user
   would and uses them (`integrations/packages.sh`), and only then publishes
   `fennecdb` to PyPI, `@fennecdb/web`, `@fennecdb/react`,
   `@fennecdb/cloudflare` and `@fennecdb/langchain` to npm, and `FennecDb` to
   NuGet -- packed, installed into a fresh console app and used against a
   server (`integrations/dotnet/package.sh`) before it is pushed, and
   failing, the other registries going on, where neither trusted
   publishing nor `NUGET_API_KEY` gives it a key. With their
   secrets it also publishes `com.fennecdb:fennecdb` and
   `fennecdb-android` to Maven Central and `fennecdb` and `fennecdb_flutter`
   to pub.dev -- the plugin with the release's dynamic XCFramework and AAR libraries
   in it -- each skipped with a notice where its secrets are not set. A
   package whose version is not the tag's stops it before anything goes out.
   SwiftPM has no registry: the tag and the zip on the release are the
   Swift package. With `HOMEBREW_TAP_TOKEN` it also writes the Homebrew
   formula (`brew install fennec-db/tap/fennecdb`) from the release's
   `SHA256SUMS`, holds it to what the public download URLs serve
   (`tools/homebrew-formula.py --verify`) and pushes it to
   `fennec-db/homebrew-tap`: here, after the publish, because the formula
   names the release's download URLs and a draft's are not public. It
   skips with a notice where the secret is not set, for a draft or a
   pre-release, and for a version older than the tap's formula (the tap is
   never moved back, whichever release is dispatched or published late), and
   one run pushes at a time.
   The `.deb` and `.rpm` need no step of their own: step 3 built them, and
   publishing the draft is what makes them downloadable.
5. Once the release is published and `packages.yml`'s run is done, attach the
   app: `gh workflow run desktop.yml --ref main -f release=vX.Y.Z`. Its
   `app` job builds the Mac app with the release's own `fennec-server` and
   shell (the tarball checked against the release's `SHA256SUMS`; a draft's
   downloads are not public, so not before step 4), makes the DMG and runs
   every check on it, the installer's `--app` path included; its four
   `other-systems` jobs do the same on Windows and Linux, x64 and arm64, each
   with the release's zip or tarball, and make and hold the zip or tarball of
   the app, installed through `install.ps1 -App` or `install.sh --app` and
   started. Where the `desktop-signing` Environment holds its secrets the
   `sign` job signs and notarizes and `attach` attaches that DMG (after its
   reviewer approves). `attach-unsigned` attaches what nothing signs: the
   ad hoc DMG where the Environment holds no secrets, as
   `fennec-studio-vX.Y.Z-macos-arm64.dmg`, and, where all four of their jobs
   passed, `fennec-studio-vX.Y.Z-windows-x64.zip`, `-windows-arm64.zip`,
   `-linux-x64.tar.xz` and `-linux-arm64.tar.xz`. It appends their lines to
   the release's `SHA256SUMS` in one write, every line before them kept byte
   for byte -- which is how `install.sh --app` and `install.ps1 -App` find and
   check them. A download's name is never replaced (a re-run of the job finds
   its own there and goes on, and a failed job's download is attached by
   re-running it), and `SHA256SUMS` is missing for the moment `--clobber`
   replaces it, so not while `packages.yml` is reading it. A DMG built again
   is another file, so a release whose DMG is attached already and whose
   Windows and Linux downloads are not (a release from before they shipped)
   takes them with `-f systems=windows-linux`: the Mac's job does not run and
   the release keeps its DMG. Then, once each:
   `curl -fsSL https://fennecdb.com/install.sh | sh -s -- --app` on a Mac
   with Apple silicon and on a Linux desktop, and `& ([scriptblock]::Create((irm
   https://fennecdb.com/install.ps1))) -App` on Windows; open the DMG from the
   release page, downloaded by a browser: its first open is refused and *Open
   Anyway* in System Settings, Privacy & Security opens it; and start the
   Windows zip's `bin\launcher.exe` downloaded by a browser, which SmartScreen
   holds to *More info*, *Run anyway* -- neither of which a check here can
   click through.

`make packages` runs the same checks locally at any time, NuGet's where
the .NET SDK is installed.

**A release's notes name every break of the REST API, and every route
deprecated since the last.** The API only grows
(`docs/design/http.md#compatibility`): each break made on purpose is an
entry of `tools/openapi_compat.py`'s `BREAKS`, with its reason and the
document version it went out in, and `git diff vA.B.C --
integrations/openapi/` against the last tag shows each operation added
and each newly `deprecated`, with its `x-fennec-sunset`. A deprecated route
is removed no sooner than its sunset, in a release whose notes say so.

**A release whose first use moves the format says so in its notes.** The
first release with named statements is one: installing it changes nothing
on disk, but a `create statement` writes record kind 13, which every binary
before it refuses, alone and in a block. Its notes carry, under their
upgrade heading, the order a cluster of mixed versions takes -- the order
of `deploy/RUNBOOK.md`'s *Upgrading with no downtime*, and one rule more:

1. every archiver (`fennec archive`);
2. every router, which reads a tenant's schema text as it creates the
   tenant;
3. every node -- each replica and standby, and each node a move, a restore
   or a failover may put a tenant on;
4. only then may any primary define a statement, by `create statement`, a
   declared schema's apply or a tenant created with one.

The rule is the last step: every binary that reads a tenant's file -- the
archivers and the nodes -- runs the new one before the first definition.
A router reads no tenant's file, only a schema's text as it creates a
tenant, and one from before refuses a schema declaring a statement, so its
place between them loses nothing: it keeps the place every upgrade gives
it, and the same order is written in the runbook and
`docs/design/named-statements.md`.

What an older binary does where the order is not kept, each case of which
`make upgrade-check` (`tools/upgrade-order.sh`) drives against the
binaries of the last main before the kind, beside this tree's:

- a replica following a primary refuses the record ("record kind 13 is not
  a write") and connects again for ever, taking no write past it;
- an archiver keeps the stream but takes no image of its own ("unknown
  record kind 13"); its `fennec verify` and `fennec restore` refuse the
  archive, which the new ones read and restore;
- a node from before refuses to open the file the new `fennec restore`
  wrote ("could not open ...: corrupt: unknown record kind 13");
- a router from before refuses a tenant's schema declaring a statement as
  one that does not read (400), nothing placed;
- a move onto a node from before fails at its install (400, "the image
  does not load: corrupt: unknown record kind 13"), and the tenant stays on
  its source, answering, with what it held.

Nothing is misread and nothing is lost, but each stops until it is
upgraded. A failover onto a standby from before is not driven: it is the
replica's case, the standby having stopped at the record before it could
be promoted.

A field keeping `@text` beside its `@hash`, `@unique` or `@sorted` takes
the same order: its schema writes type tag 18 (`TAG_BESIDE`), and a replica
from before stops following at it ("unknown type tag 18"), as one does at
`TAG_NOCASE`'s 17, so every reader upgrades before any primary gives a field
its second index. `make upgrade-check` drives record kind 13 alone.

**A statement bound to a collection's writes moves it once more.** From
the release whose engine runs them (`docs/design/named-statements.md`,
F2's slice 3), a primary writes one by `create statement ... after`, a
declared schema's apply or a tenant created with one: kind 13's op 3,
whose collection and events stand outside its text, and which a binary
from before it -- the last main before the op, `b73ce4ca` -- reads as a
statement record it does not read and refuses, never taking it for a
callable statement whose writes would land without its rows. Its notes
name `after` as the clause that moves the format, under the same upgrade
heading and in the same order: every archiver, router and node upgraded
before any primary defines a binding. Where the order is not kept, each
older binary stops in words ("a statement record this version does not
read"): a replica stops following at the record and connects again, taking
no write past it; an archiver keeps the stream and takes no image of its
own, and its `fennec restore` refuses the archive this tree's restores; the
server refuses to open the file that restore wrote; a move onto an older
node fails at its install, the tenant staying on its source, its binding
running. `make upgrade-check` drives each against `OP3_BASE`, beside the
server, a replica and `fennec types` refusing fennec-core's
`tests/statements.fennec`.

**A statement run by the clock moves it once more.** From the release
whose server runs them (`docs/design/named-statements.md`, F3's slice 3), a
file's primary writes one by `create statement ... every` or `... at`, or a
declared schema's apply: kind 13's op 4, its schedule and `since` outside
its text, and each run's mark, op 5, in the block of the run's writes -- or
in the next block that lands, where the run wrote nothing. A binary from
before them -- the last main before the ops, `cd907d6c` -- reads either as
a statement record it does not read and refuses it, never taking a schedule
for a callable statement nobody runs. Its notes name `every` and `at` as
the clauses that move the format, under the same upgrade heading and in the
same order: every archiver, router and node upgraded before any primary
defines a schedule. A tenant node runs its tenants' from F3's slice 4 on,
keeping `schedules.hint` beside their files; a node of the release before
it takes none, so a tenant's schema declaring one placed on it is
refused, nothing placed, and a move onto it is refused at the install, the
tenant left on its source: every node upgraded before a tenant defines one
there. Where the order is not kept, each older binary
stops in words ("a statement record this version does not read"): a
replica stops following at the record and connects again, taking no write
past it; an archiver keeps the stream and takes no image of its own, and
its `fennec restore` refuses the archive this tree's restores; the server
refuses to open the file that restore wrote. `make upgrade-check` drives
each against `OP4_BASE`, a schedule defined over `/query` by this tree's
primary, beside the server, a replica and `fennec types` refusing
fennec-core's `tests/statements.fennec`.

**A webhook consumer moves it once more.** From the release whose server
takes webhook events (`docs/design/named-statements.md`, F4's slice 3), a
file's primary writes one by `create statement ... on hook <h>`: kind 13's
op 6, its hook's name after its text, its events' keys rows of
`_hook_events` in the blocks of their writes. A binary from before it --
the last main before the op, `6fb28002` -- reads it as a statement record
it does not read and refuses it, never taking a hook's statement for a
callable one whose text it cannot read. Its notes name `on hook` as the
clause that moves the format, under the same upgrade heading and in the
same order: every archiver, router and node upgraded before any primary
defines one. From F4's slice 4 a tenant node takes them too, in the same
order: every router before every node -- a router from before sends a read
replica's follow unmarked, so a new primary waits for it as for a standby
before an event's 2xx -- and every node before a tenant defines one, a
node from before refusing the definition, the install of an image holding
one and the router's `hook-secrets` (404, read as no set). Where the order
is not kept, each older binary stops in words ("a statement record this
version does not read"): a replica stops following at the record and connects again, taking no
write past it; an archiver keeps the stream and takes no image of its own,
and its `fennec restore` refuses the archive this tree's restores; the
server refuses to open the file that restore wrote. `make upgrade-check`
drives each against `OP6_BASE`, a hook's statement defined over `/query` by
this tree's primary, beside the server, a replica and `fennec types`
refusing fennec-core's `tests/statements.fennec`.

**Record checksums move a file at its first write; followers keep
following.** From the release whose records carry a claim and a checksum
(`docs/design/storage.md#file-format`), every record a binary writes into a
file -- a write, a block, a graph, the history, each record of an image a
compact or a checkpoint writes -- sets its kind's high bit and ends in its
claim and CRC32C, its length counting them. A binary from before -- a main
before them, `f29a44a7` -- frames such a record by its length and refuses
its kind in words ("unknown record kind 131 at 8"), never reading the sum
as a record, so a file a binary with checksums wrote to opens in no binary
from before: a primary rolled back after its first write is rolled back
onto its last backup from before. Nothing has to be defined first: a
primary's first write after its upgrade is one, and a page's module and an
app's library write them the same. What a follower is sent, though, is
what it reads: one with checksums says so (`sums=1` on `/_replication` and
a node's `POST /_replication/tenants`), and one that does not -- every
binary from before -- is sent each record and image as a binary from before
wrote them. So replicas, standbys, a node's tenants' replicas and archivers
from before keep following an upgraded primary, and what such an archiver
kept verifies, restores and opens with the binaries from before. The order
is free but for what moves a file: a tenant's file an upgraded node wrote,
moved, restored or failed over onto a node from before, is refused there
-- a move fails at its install, the tenant staying on its source -- so a
region's nodes are upgraded within one window, before anything moves a
tenant between them; and its notes say so under the same upgrade heading.
A router reads no tenant's file and keeps its place. Files from before open
as they were, their records loading as they read, and take checked records
after them until a compact writes each anew; a replica with checksums
following a primary from before frames each record it takes as its own
version does, its file checked from its first record. `make upgrade-check`
drives an old replica and an old archiver following this tree's primary --
the old `fennec verify`, `restore` and server reading what the archiver
kept -- the old server and `fennec types` refusing the primary's own file,
and this tree's replica of an old primary, its file whole by `fennec
verify`, and this tree's server writing to the old primary's file; and it
drives the cases above with `CK_BASE`'s binaries in this tree's place,
since a binary from before them refuses a file this tree wrote at its first
record, before the kind or op the case is about.

**A node recorded at `https://` waits for every router and node.** From
the release that dials HTTPS (TLS slice 3), a node serving HTTPS
(`--tls-cert`) is recorded at its `https://host:port` address (`PUT
/_shard/nodes/<n>`), and a region's routers may be `https://` URLs. The
scheme rides in the address rather than in a field beside it, so a
binary from before cannot dial it in the clear; the order is:

1. every router -- each region's, and each standby following a
   directory;
2. every node, among them each one a tenant may follow or be moved to;
3. only then is a node recorded at `https://`, or a region's router
   named by one.

Where it is not kept, a router from before cannot resolve
`https://host:port`: it sends that node nothing -- never its admin token
in plain HTTP -- relays nothing to it and grants it no lease, so the
node's lease lapses and, where it has a standby, its tenants are failed
over; and a node from before refuses to follow an `https://` primary and
takes nothing. `make upgrade-check` drives both against the binaries of
its base, beside this tree's router dialing the same node over TLS.

An older router *reading* such a record -- a standby following the
directory, a binary rolled back over its file -- is not driven: it holds
the address as text and dials it as the one it refused at `PUT
/_shard/nodes` does, through `dial::resolve`, so it skips the node as that
check shows, never misreading it. The address is no socket address, and
the system's lookup is handed `https://host` as a name, the port being
what follows the last colon -- not a host name, refused by the resolver
(`nodename nor servname provided` on macOS) before any byte goes to the
node. A record of the directory carries the address alone, so no
field of it is read another way.

| Package | From | What it holds |
| --- | --- | --- |
| `fennecdb` on PyPI | `integrations/python` | the HTTP client and the LangChain and LlamaIndex stores; the standard library alone |
| `@fennecdb/web` on npm | `web/` | `fennec.js` and its types, `fennec.wasm` and `collate/` |
| `@fennecdb/react` on npm | `integrations/react` | `FennecProvider`, `useFennec`, `useLiveQuery` |
| `@fennecdb/cloudflare` on npm | `integrations/cloudflare` | `persist`, `restore`, `checkpoint`: a database kept in a Durable Object's storage |
| `@fennecdb/langchain` on npm | `integrations/langchain` | `FennecVectorStore` for LangChain.js, over a `Fennec` or a `FennecHttp` |
| `FennecDb` on NuGet | `integrations/dotnet/FennecDb` | `FennecClient`, the .NET SDK over HTTP; `HttpClient` and `System.Text.Json` alone |
| `github.com/fennec-db/fennecdb/integrations/go` | the tag `integrations/go/vX.Y.Z` | package `fennecdb`, the Go SDK over HTTP; the standard library alone |
| `FennecDB` for SwiftPM | `Package.swift`, `integrations/swift` | the tag, and `FennecFFI.xcframework.zip` on the release: the library embedded in a macOS or iOS app |
| `com.fennecdb:fennecdb` on Maven Central | `integrations/kotlin/fennecdb` | the Kotlin library for the JVM; kotlinx-coroutines alone, the native library brought by the app |
| `com.fennecdb:fennecdb-android` on Maven Central | `integrations/kotlin/android` | the AAR: the same library and `libfennec_ffi.so` for arm64-v8a, armeabi-v7a, x86_64 |
| `fennecdb` on pub.dev | `integrations/dart/fennecdb` | the Dart package over `dart:ffi`; `package:ffi` alone |
| `fennecdb_flutter` on pub.dev | `integrations/dart/fennecdb_flutter` | the Flutter plugin: the package with the library for iOS, Android and macOS |
| `fennecdb` in the Homebrew tap `fennec-db/homebrew-tap` | `packaging/homebrew`, `tools/homebrew-formula.py` | the formula: `fennec` and `fennec-server` from the release's tarballs, by platform, each with its SHA-256 |
| `fennecdb_X.Y.Z-1_<arch>.deb`, `fennecdb-X.Y.Z-1.<arch>.rpm` on the release | `packaging/nfpm.yaml`, `tools/linux-packages.sh` | `/usr/bin/fennec` and `/usr/bin/fennec-server` from the musl tarballs, for amd64 and arm64; no dependencies |
| `https://fennecdb.com/install.sh` | `site/install.sh` | the installer: the release's tarball for the machine, checked against `SHA256SUMS`, into `~/.fennecdb/bin`; with `--app`, the app checked the same way: the Mac's DMG, its app into `~/Applications`, or Linux's tarball into `~/.local/share/dev.fennecdb.studio/stable/app` with a menu entry; deployed with the site, not with a release |
| `fennec-studio-vX.Y.Z-macos-arm64.dmg` on the release | `integrations/studio-desktop` (`scripts/dist.sh`), `.github/workflows/desktop.yml` | fennec studio's Mac app (beta, Apple silicon), unpacked and signed ad hoc, attached by step 5 with its line appended to `SHA256SUMS`; unsigned until the signing Environment holds its secrets |
| `fennec-studio-vX.Y.Z-windows-x64.zip`, `-windows-arm64.zip`, `-linux-x64.tar.xz`, `-linux-arm64.tar.xz` on the release | `integrations/studio-desktop` (`scripts/dist.sh`), `.github/workflows/desktop.yml` | fennec studio for Windows and Linux (beta): the folder `fennec-studio`, the app unpacked, which runs where it is unpacked; attached by step 5 with their lines appended to `SHA256SUMS`; unsigned |
| `fennec-vX.Y.Z-x86_64-pc-windows-msvc.zip`, `fennec-vX.Y.Z-aarch64-pc-windows-msvc.zip` on the release | `tools/windows-zip.sh` | `fennec.exe` and `fennec-server.exe`, the C runtime linked in; unsigned |
| `fennec-ffi-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`, `fennec-ffi-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz` on the release | `crates/fennec-ffi`, `tools/linux-ffi.sh` | the C library for Linux with glibc 2.34 or later, in the folder of the tarball's name: `include/fennec.h`, `lib/libfennec_ffi.so` (its SONAME its name, the JNI functions in it for the JVM), `lib/libfennec_ffi.a`, `lib/pkgconfig/fennec-ffi.pc` and `fennec-ffi-static.pc`, `LICENSE`, `README.md`; in `SHA256SUMS` |
| `https://fennecdb.com/install.ps1` | `site/install.ps1` | the Windows installer: the release's zip, checked against `SHA256SUMS`, into `%LOCALAPPDATA%\fennecdb\bin`; with `-App`, the app's zip, checked the same way, into `%LOCALAPPDATA%\dev.fennecdb.studio\stable\app` with a Start-menu shortcut; deployed with the site, not with a release |

## Once: the desktop app's signing Environment

The Mac app (`integrations/studio-desktop`) is signed and notarized by
`desktop.yml`, by hand and from `main` only, in a GitHub Environment that holds
the certificate. **Make the Environment, with its rules, before any secret goes
into it**: a secret in an Environment that has no rules yet is readable by a
job from any branch that names it.

A required reviewer (you: the dispatch waits for a person) and deployment
branches limited to `main` alone (the code that is signed is main's):

    gh api -X PUT repos/fennec-db/fennecdb/environments/desktop-signing --input - <<EOF
    {"reviewers":[{"type":"User","id":$(gh api user -q .id)}],
     "deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}
    EOF
    gh api -X POST repos/fennec-db/fennecdb/environments/desktop-signing/deployment-branch-policies -f name=main -f type=branch

Look at what it holds (one reviewer, `main`, and nothing else) before going on:

    gh api repos/fennec-db/fennecdb/environments/desktop-signing -q '.protection_rules'
    gh api repos/fennec-db/fennecdb/environments/desktop-signing/deployment-branch-policies -q '[.branch_policies[].name]'

Only then the secrets, in the Environment and not in the repository's own,
pasting each when asked (the `.p12` and the `.p8` as base64):

    for s in ELECTROBUN_DEVELOPER_ID MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD \
             ELECTROBUN_APPLEAPIKEY ELECTROBUN_APPLEAPIISSUER ELECTROBUN_APPLEAPIKEY_P8; do
      gh secret set "$s" --env desktop-signing -R fennec-db/fennecdb
    done

A release whose server has `--studio` and `--exit-on-stdin-close` (0.1.12 and
after) is then signed and its DMG attached with `gh workflow run desktop.yml
--ref main -f release=v0.1.12` (step 5 above), instead of the unsigned DMG
the same dispatch attaches while the Environment has no secrets. The job's first step reads the Environment's rules back and
stops unless they are the above, where GitHub lets its token read them; it
may not be able to (the Environments API asks for administration rights, which
a workflow's token is not given), so the order above is what stands behind it.

## Once: the registries' side

`packages.yml` publishes with a token where a registry's secret holds one,
and with its own OIDC identity (trusted publishing) where it does not.
Either is set up once, by whoever owns the names.

**With tokens.** On npm, create the organization `fennecdb` (the `@fennecdb`
scope) and an automation token with publish access to it. npm answers a
publish into a scope nobody has made `404 Not Found - PUT`, as it answers a
token without access -- 0.1.5's dispatch stopped there -- and
`https://registry.npmjs.org/-/org/fennecdb/package` says `Scope not found`
until the organization exists. On PyPI, an API token for the whole account,
since a token scoped to a project cannot make the project on its first
upload. Then, pasting each when asked:

    gh secret set NPM_TOKEN -R fennec-db/fennecdb
    gh secret set PYPI_API_TOKEN -R fennec-db/fennecdb

After the first upload the PyPI token can be replaced by one scoped to
`fennecdb`. A release published before the secrets were set gets its
packages from a dispatch: `gh workflow run packages.yml -f tag=vX.Y.Z`.

**Without tokens,** the registries trust the workflow itself:

**PyPI.** Signed in as the account that will own `fennecdb`, under
*Publishing*, add a pending publisher: project `fennecdb`, owner `fennec-db`,
repository `fennecdb`, workflow `packages.yml`, environment `pypi`. The first
published release creates the project. `PYPI_API_TOKEN` must then be unset:
the job publishes with it wherever it is set, and a token scoped to another
project (`fenecdb`'s, before the rename) fails the upload. The policy does
not look at the ref, so the `pypi` environment is limited to `main` and
`v*` tags, as `nuget` is (the NuGet section below has the commands).

**npm.** Create the organization `fennecdb`, which is the `@fennecdb` scope. npm
trusts a workflow only for a package that already exists, so each package's
first version goes out with a token:

1. Make a granular access token with read and write on `@fennecdb` and store it
   as the repository secret `NPM_TOKEN`.
2. Publish the release: the four `@fennecdb` packages go out with it.
3. On npmjs.com, for each of the four, *Settings*, *Trusted publishing*,
   GitHub Actions: `fennec-db/fennecdb`, workflow `packages.yml`, environment
   `npm`; *Allow npm publish* on, *Allow npm dist-tag* off. With publish off,
   npm stages every version the workflow sends (`0.0.0-stage` on the
   package's page) until the owner approves it with 2FA, as it staged
   0.2.0's token publishes; the job never runs `npm dist-tag`. The repository
   name is fixed once the connection is made: `fennecdb`, two n's.
4. Delete the secret and revoke the token. Later releases publish with the
   workflow's identity alone.
5. The policy does not look at the ref, so the `npm` environment does:
   limit it to `main` and `v*` tags with the NuGet section's three
   commands, naming `npm` where they name `nuget`.

Steps 3 to 5 were done for the four packages after 0.2.0, so the release
after it is the first they take by the workflow's identity alone.

**NuGet.** By trusted publishing: the package is packed and used by a job
that cannot publish (`nuget-pack`), and pushed by one that holds only
`id-token: write` (`nuget`), which trades its OIDC token for a key that
lives an hour (`NuGet/login`). Signed in to nuget.org as the account that
will own `FennecDb` (`furkankinyas`, which the job names), under *Trusted
Publishing*, add a policy: package owner that account, GitHub Actions,
repository owner `fennec-db`, repository `fennecdb`, workflow file
`packages.yml`, environment `nuget`, scope *Push new packages and package
versions*, glob pattern `FennecDb` -- the first push creates the package.

The policy does not look at the ref, so the `nuget` environment does: let
it be used only by runs of `main` and `v*` tags, as `homebrew` is, or a
dispatch from any branch, running that branch's `packages.yml`, could push.
By hand, as the owner:

    gh api -X PUT repos/fennec-db/fennecdb/environments/nuget \
      -F 'deployment_branch_policy[protected_branches]=false' \
      -F 'deployment_branch_policy[custom_branch_policies]=true'
    gh api -X POST repos/fennec-db/fennecdb/environments/nuget/deployment-branch-policies -f name=main -f type=branch
    gh api -X POST repos/fennec-db/fennecdb/environments/nuget/deployment-branch-policies -f name='v*' -f type=tag

Where the exchange fails the job uses `NUGET_API_KEY` if it is set (under
*API Keys*, a key with that scope for the glob pattern `FennecDb`, which
expires within 365 days); with neither it fails, naming the exchange's
failure, and the other registries go on. A key left from before the policy
is better deleted (`gh secret delete NUGET_API_KEY -R fennec-db/fennecdb`):
a key for another package's name only fails the push where the exchange
does. A release published before either was set gets its package from a
dispatch: `gh workflow run packages.yml -f tag=vX.Y.Z`.

**Go.** Nothing to set up: the module is fetched from the repository by its
tag, and `proxy.golang.org` keeps a version once someone has fetched it.
A tag pushed by mistake is not moved; the next version replaces it. The
module path is the repository's name: v0.1.11 and earlier declare
`github.com/fenecdb/fenec/integrations/go`, from before the repository
became `fenecdb`, v0.1.12 declares `github.com/fenecdb/fenecdb/integrations/go`,
and the first release after it declares
`github.com/fennec-db/fennecdb/integrations/go`, the repository's name since
fenecdb became fennecdb. A version's `go.mod` is what `go get` checks the
path against, so an app moves to the new path with that release, and `examples/go/go.mod` (which `make version` writes) names a
version of the new path that resolves from then on.

**Swift.** Nothing to set up either: SwiftPM fetches the package by the
tag and the zip by the URL `Package.swift` names. `swift-binary.yml` opens
its pull request with the workflow's token, which the repository has to
allow (*Settings*, *Actions*, *General*, "Allow GitHub Actions to create
and approve pull requests"); without it the run pushes the branch
`swift-binary-vX.Y.Z` and the pull request is opened by hand.

**Maven Central.** The Central Portal (central.sonatype.com) takes the
bundle `packages.yml` makes, with no Gradle plugin. Once:

1. Sign in to the Portal and add the namespace `com.fennecdb`, the
   domain's: the Portal verifies it by a TXT record on `fennecdb.com`
   holding the key it gives, which can be removed after.
2. Under *View Account*, *Generate User Token*: its two halves are the
   secrets `MAVEN_CENTRAL_USERNAME` and `MAVEN_CENTRAL_PASSWORD`.
3. Every file is signed: make a GPG key for the releases
   (`gpg --quick-gen-key "fennecdb releases" rsa4096 sign never`), send its
   public half to a key server the Portal reads
   (`gpg --keyserver keyserver.ubuntu.com --send-keys <id>`), and store the
   private half, armored, and its passphrase:

       gpg --armor --export-secret-keys <id> | gh secret set SIGNING_KEY -R fennec-db/fennecdb
       gh secret set SIGNING_PASSWORD -R fennec-db/fennecdb
       gh secret set MAVEN_CENTRAL_USERNAME -R fennec-db/fennecdb
       gh secret set MAVEN_CENTRAL_PASSWORD -R fennec-db/fennecdb

The upload is `publishingType=AUTOMATIC`: the Portal releases the
deployment once it validates, and a version is there for good.

**pub.dev.** pub.dev's automated publishing trusts only a workflow started
by a tag's push, and `packages.yml` follows a published release, so it
publishes with an account's credentials. Signed in to pub.dev as the account
that will own `fennecdb` and `fennecdb_flutter` (a verified publisher,
`fennecdb.dev` or the like, can own them after): `dart pub login` on any
machine writes `pub-credentials.json` (`~/Library/Application
Support/dart/` on macOS, `~/.config/dart/` on Linux); then

    gh secret set PUB_CREDENTIALS -R fennec-db/fennecdb < <that file>

The first publish creates both packages. The credentials hold a refresh
token: logging out of that machine's pub (`dart pub logout`) revokes it,
and a new login goes into the same secret.

**Homebrew.** `brew install fennec-db/tap/fennecdb` reads the repository
`fennec-db/homebrew-tap`: Homebrew drops the `homebrew-` from the name, and
the formula is `Formula/fennecdb.rb` on the default branch, `main`. The
repository is public, and `brew install fennec-db/tap/fennecdb` installs the
latest release, whose formula the release's `packages.yml` pushed (0.2.0's
first). Once, and if the tap is ever made again:

1. Make it, empty, and copy in the formula and the README as they are in
   `packaging/homebrew/`:

       mkdir -p Formula
       cp packaging/homebrew/Formula/fennecdb.rb Formula/
       cp packaging/homebrew/README.md README.md

   The copy in the repository is the formula of the release it names
   (0.2.0), as `tools/homebrew-formula.py` writes it; CI holds it to that
   (`--check`, which needs the repository to be `fennec-db/fennecdb`, and
   `--verify`, which downloads the release's four tarballs and holds the
   sums to them), and the release job never edits it. The tap's is
   rewritten at each release.
2. Make a fine-grained personal access token (*Settings*, *Developer
   settings*, *Fine-grained tokens*): resource owner `fennec-db`, repository
   access *Only select repositories*, `fennec-db/homebrew-tap` alone,
   permission *Contents: Read and write* (Metadata, read-only, comes with
   it) and nothing else. It expires within a year at most: note the date, as
   a job with an expired token fails rather than skipping.
3. Keep it as a secret of the `homebrew` environment, not of the repository,
   and let that environment be used only by runs of `main` and of `v*` tags,
   so that a branch's workflow (or a pull request's) cannot ask for it. By
   hand, as the owner (the same is *Settings*, *Environments*, *homebrew*):

       gh api -X PUT repos/fennec-db/fennecdb/environments/homebrew \
         -F 'deployment_branch_policy[protected_branches]=false' \
         -F 'deployment_branch_policy[custom_branch_policies]=true'
       gh api -X POST repos/fennec-db/fennecdb/environments/homebrew/deployment-branch-policies -f name=main -f type=branch
       gh api -X POST repos/fennec-db/fennecdb/environments/homebrew/deployment-branch-policies -f name='v*' -f type=tag
       gh secret set HOMEBREW_TAP_TOKEN --env homebrew -R fennec-db/fennecdb
       gh secret delete HOMEBREW_TAP_TOKEN -R fennec-db/fennecdb # a repository secret of that name, if there is one

   The job runs under that environment and reads the secret from it, in an
   environment variable (an environment's secret wins over a repository's of
   the same name; deleting the repository's is what keeps any other workflow
   from reading the token), and hands it to git through `GIT_CONFIG_*`, never
   in a URL or a command line.
4. A release published before the secret was set gets its formula from a
   dispatch, which runs the whole workflow (the registries pass over what
   they already hold): `gh workflow run packages.yml -f tag=vX.Y.Z`. A tag
   older than the formula in the tap is left out. Or by hand, from a clone of
   this repository and the tap:

       gh release download vX.Y.Z -R fennec-db/fennecdb -p SHA256SUMS
       python3 tools/homebrew-formula.py --version X.Y.Z --sums SHA256SUMS -o ../homebrew-tap/Formula/fennecdb.rb
       python3 tools/homebrew-formula.py --verify ../homebrew-tap/Formula/fennecdb.rb

To try a formula before it is pushed: `brew tap-new --no-git you/test`, copy
it into that tap's `Formula/`, then `brew install you/test/fennecdb`, `brew
test` and `brew audit --strict --online` on it, and `brew untap you/test`
(Homebrew installs no formula from a bare path).

**.deb and .rpm.** Nothing to set up. `nfpm` is the binary
`tools/install-nfpm.sh` pins with its checksum; moving to a newer one is a
change of that file's version and four sums (from the release's
`checksums.txt`), and CI's `packaging` job makes and installs the packages
from the latest release before it merges. There is no apt or yum repository:
the packages are files on the release.

**install.sh.** Nothing to set up: `site/install.sh` is built into the site
(`site/build.py` checks that nothing before `main`, called on its last line,
is more than `set -eu`, comments, assignments and function definitions) and
deployed with it, so a change reaches `https://fennecdb.com/install.sh` when
it merges, not when a release is made. It finds the latest release itself.
`sh site/test_install.sh` runs it against a release of its own on localhost.

**install.ps1.** Nothing to set up either: `site/install.ps1` is built into
the site beside `install.sh` (`site/build.py` checks that nothing before
`Install-Fennec @args`, its last line, is more than comments and function
definitions, and that it is ASCII) and reaches
`https://fennecdb.com/install.ps1` when it merges. Until a release carries
the Windows zip, it says the latest release has no build for Windows and
installs nothing. CI's `windows (release zip)` job runs
`site/test_install.ps1` on Windows against the zip it builds.

**GitHub.** The environments `pypi`, `npm`, `nuget`, `maven`, `pub` and `homebrew` are made the first
time the workflow runs. A required reviewer on them makes a publish wait for a second
yes. `homebrew`, which holds a secret of its own, and `pypi` and `nuget`,
whose policies PyPI and nuget.org trust, are limited to `main` and `v*`
tags (the Homebrew, PyPI and NuGet sections above), and `npm` too since its
packages publish by trusted publishing.

**After the first publish.** The docs install what a registry holds: PyPI's
`fennecdb` since 0.1.5 (`pip install "fennecdb[langchain]"`). Until
`@fennecdb/web` is on npm the React example imports `sync` from
`./fennec.js`; then from `@fennecdb/web`, with `npm install @fennecdb/web
@fennecdb/react` beside it.
