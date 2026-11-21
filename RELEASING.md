# Releasing fennecdb

A release is a tag. Its binaries, browser bundle and image are built from the
tag and left in a draft; the packages follow once the draft is published, so
nothing reaches a registry before the notes have had a read.

## A release

1. `make version V=X.Y.Z` refuses while the release it moves on from has
   no whole fixture set in the working tree: the workspace's version, from
   v0.2.0 on, needs `crates/fennec-core/tests/compat/v<that version>/`
   (step 7 writes it), every file the bytes its `SHA256SUMS` names and the
   `SHA256SUMS` the one its `RELEASES` line pins, every
   later build's tests opening its files -- and so does every release this
   clone has the tag of from v0.2.0 on, a release candidate aside, so a
   set deleted with its `RELEASES` line is refused here though every compat
   test passes; a README and its sums alone are no set. It reads the tree it runs in,
   not main: run it on a branch from main once the set's pull request
   landed. And it refuses in a clone without that version's tag, naming
   `git fetch --tags`: a release is cut from a full clone, and one without
   the tags would let every release through. Then the one version goes into the workspace, the
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
   The sizes the docs quote of the browser module's features and of its code
   by crate are regenerated in the same pull request: `make wasm-sizes
   JSON=1` rewrites `crates/fennec-wasm/sizes.json`, and `site/build.py`
   fails every page figure that is not its rounded value, so the feature
   rows of `benchmarks-size.html`, `docs/design/browser-module.md` and
   `web/README.md` are edited to match. The native library's are CI's: copy
   the table the mobile jobs print (`integrations/ffi-sizes.sh`, in their
   summaries) into `integrations/ffi-sizes.md`; those jobs fail a run more
   than 1% from it.
2. Build the Swift package's binary: `gh workflow run swift-binary.yml -f
   version=X.Y.Z` for an X.Y.0 and its candidates, which are built from
   `main`; a patch (X.Y.Z, Z above 0) is built from `release/X.Y`, the
   branch the owner cuts from the minor's last tag the first time one is
   needed (`git push origin vX.Y.0^{commit}:refs/heads/release/X.Y`), its
   fixes merged there: `gh workflow run swift-binary.yml --ref release/X.Y
   -f version=X.Y.Z`, and step 3 tags that branch's merge. The run stops on
   any other branch, and step 6 takes the XCFramework's attestation from
   that one ref alone: a `release/*` branch is pushed by whoever may write
   the repository, and its run of an edited `swift-binary.yml` would attest
   whatever bytes it was given, so `release/X.Y` vouches for a patch and
   never for an X.Y.0 (and the owner's ruleset on `release/*` decides who
   pushes one). SwiftPM fetches the package by its tag, and the
   `Package.swift` at that tag must already name the XCFramework's zip by
   its URL and checksum, so the zip is built before the tag: the run builds
   it from its branch, tests it on macOS and an iOS simulator, keeps it as its
   artifact and opens a pull request writing its checksum into
   `Package.swift` (`integrations/swift/set-binary.sh`). It keeps the
   `FennecFFIDynamic.xcframework.zip` beside it -- the frameworks, and
   macOS's binary again as a plain `libfennec_ffi.dylib`, which the Flutter
   plugin's macOS pod vendors -- built from the same commit and named by
   nothing. Its `pull-request` job attests the three files (the two zips
   and the `.checksum`) as the run's build, from the branch it ran on and
   its commit, before it opens the pull request: `release.yml` attaches
   them and does not attest them again (step 6 verifies them against
   `swift-binary.yml`). Merge it.
3. Tag what was merged and push the tag:
   `git tag vX.Y.Z origin/main && git push origin vX.Y.Z` (a patch:
   `origin/release/X.Y`). A tag is `vX.Y.Z`, or `vX.Y.Z-rc.N` for a
   candidate with N from 1, and nothing else: every job of `release.yml`
   refuses another form in its first step, saying so, and
   `tools/release-verify.sh` and `release-latest.sh` take the same one
   (`tools/test_release_verify.py` holds each copy to it).
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
   for each ABI inside) and the multi-arch image on ghcr.io as
   `X.Y.Z` -- each architecture's run on the runner that built it before
   it is pushed, its `--version` and its server healthy by its own
   HEALTHCHECK (`tools/image-check.sh`), pushed from Docker's classic image
   store and read back, each tag and the list held to v0.2.0's Docker v2
   manifests and gzip layers; `latest` waits for step 4 -- takes the
   XCFramework zips step 2's run kept -- the static one held to the checksum the tag's
   `Package.swift` names, and stopping the release where none matches --
   and drafts the release with all of them and their checksums, its notes
   opening with the compiler: each `binaries` job holds its `rustc -V` to
   the release `rust-toolchain.toml` names and, on Linux, to the one the
   image's build stage resolves, and a difference stops the release before
   the draft ([A toolchain bump](#a-toolchain-bump)). Each file it built is
   attested first as this run's build (`publish`), then `SHA256SUMS`, which
   ties each file to its name in the release, and the image's
   list and its two architectures' manifests once the list is made
   (`manifest`; step 6 verifies all of them). A candidate's tag (`vX.Y.Z-rc.N`) is
   drafted as a pre-release, which GitHub never makes Latest. It also
   makes a `.deb` and an `.rpm` for amd64 and arm64 from the musl tarballs
   (`tools/linux-packages.sh`, `packaging/nfpm.yaml`), each architecture on a
   runner of its own, installs them in a Debian and a Fedora container and
   runs what they installed (`tools/linux-packages-check.sh`) before they
   are kept, so a package that does not install stops the release, and
   lists them in `SHA256SUMS` with the rest. Beside `vX.Y.Z` it pushes `integrations/go/vX.Y.Z` at
   the same commit: a Go module in a subdirectory is fetched by a tag that
   names it, so that tag is the Go SDK's release (`go get
   github.com/fennec-db/fennecdb/integrations/go@vX.Y.Z`), and there is no
   registry to publish to. A release built again -- a job of its run
   retried, or a dispatch for its tag; the tag itself is never moved
   (*GitHub*, below) -- replaces the assets of a draft; a published
   release's are never
   replaced, and it only gets the files it has no asset of, with a warning
   that its `SHA256SUMS` does not list them. Only the tag's own run attests
   what it built, as an attestation names the run's ref and commit as the
   source: its push, or a dispatch at the tag, `gh workflow run release.yml
   --ref vX.Y.Z -f tag=vX.Y.Z`, which runs the tag's own `release.yml`. A
   dispatch from a branch (`--ref main`, for a fix to the workflow made
   after the tag) builds the tag's tree under the branch's name and head
   commit, and would sign that as the source: its files and image are
   attached unattested, with a warning, and step 6 fails on them, so such
   a fix ships in the next version, a candidate's or a patch's. **A
   published release whose file went out without its attestation stays
   unverifiable for good**: a rebuild at the tag is not the same bytes,
   and `publish` gives a published release only the files it has no asset
   of, so nothing can put an attested file under that name, and step 6
   fails on that release as long as it stands. Its fix ships in the next
   version. A draft is still mended: a dispatch at the tag replaces its
   files and `SHA256SUMS` with attested ones. The Homebrew formula, `install.sh`,
   `install.ps1` and the packages name the release's files by checksum, and a
   rebuild is not byte for byte the first build: a replaced tarball would fail
   every `brew install` and every sum already taken. To change what a
   published release holds, release a new version.

   **A power cut.** Every release needs the last run of
   `linux-power-cut.yml` green, as its Linux binaries are every release's:
   the commit's fennec-server, built as `release.yml` builds it, in seven
   Ubuntu guests cut five times each, twice with the disk's cache lost,
   the sync log a durable write takes on Linux applied at each open -- on
   ext4 in each of its three data modes (`ordered`, `writeback`,
   `journal`) and on XFS under `--sync always` writes, compacts and
   spills, every acknowledged write read back; on ext4 under `--sync 250`,
   the default, every write `/_changes` handed over read back, every write
   lost answered within 250 ms and two syncs' own time of the cut, no sync
   a second late nor the syncer slowed; and a `--dir` node of two tenants
   with a standby following it, under `--sync always` every acknowledged
   write read back, under `--sync 250` each tenant held as the file is,
   and the standby near the node's tip and holding nothing the node lost;
   each cut mid-write
   (docs/design/durability.md, "A power cut"): `gh run list --workflow
   linux-power-cut.yml --limit 1`, and if it ran before the commit being
   released, `gh workflow run linux-power-cut.yml --ref main` and wait for
   it.

   **Two releases beside each other.** Every release needs `compat.yml`
   green on the release commit: the commit's `fennec-server`, `fennec` and
   `fennec-shard` beside the last published release's -- its own Linux
   tarball, checked against its `SHA256SUMS` by `site/install.sh`, and its
   router built from its tag's sources with the tag's `Cargo.lock`
   (`--locked`) by this tree's toolchain, a debug build, since no release
   ships one -- driven through replication both ways, archives and backups
   plain and sealed, the values 0.2.0 wrote that 1.x does not read whole, a
   router over nodes of the other release and a node at `https://`, a
   `github` hook kept with `key delivery`, and a rolling upgrade of a router
   and a node pair under durable writes, every acknowledged write read back
   from both nodes and none the router said it did not send landed (`make
   compat-check`, `docs/design/compatibility.md#the-compatibility-matrix`).
   The tag's push runs it on the tag; read it before the draft is published
   (step 4): `gh run list --workflow compat.yml --commit <sha>`, or run it
   beforehand, `gh workflow run compat.yml --ref main`. A case that fails
   is a break or a bug until this file or the compatibility contract
   promises what it found, and the release waits for that decision.

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
4. Read the draft, then publish it, once step 3's run has passed
   `image (manifest)`, the job that makes the image's `X.Y.Z`:
   `gh release edit vX.Y.Z --draft=false` (with `--latest=false` where
   `X.Y.Z` is not the newest release: a patch to an older minor, [A
   security release](#a-security-release)). A candidate is published as a
   pre-release and never Latest: `gh release edit vX.Y.Z-rc.N --draft=false
   --prerelease --latest=false` (its draft is a pre-release already; the
   flags say so again). Published without them a candidate would be
   GitHub's Latest, and `install.sh` and `install.ps1`, which read
   `releases/latest/download/SHA256SUMS`, would install it for everyone;
   `packages.yml`'s `github-latest` puts Latest back on the newest X.Y.Z
   whatever was published, a candidate included, within the minute its run
   takes. A candidate's packages go out as pre-releases where a registry
   has them: npm under `next` (`next-X.Y` for one older than the newest
   pre-release), PyPI as `X.Y.ZrcN`, which `pip install` passes over
   unless asked, NuGet and pub.dev as pre-release versions; the image gets
   `X.Y.Z-rc.N` and no `latest`, Homebrew nothing. Maven Central is the
   one that does not hold it apart: its `maven-metadata.xml` `<release>` is
   the highest version uploaded, pre-releases among them, so while a
   candidate is the highest, Gradle's `latest.release` and Maven's
   `RELEASE` resolve to it. The upload stays (a candidate is there to be
   tried); a user pins a version. npm's `next` is left on the last
   candidate once X.Y.Z is out, so `npm i @fennecdb/web@next` would install
   an older build than `latest`, and the workflow cannot move it: each
   package's trusted publisher allows `npm publish` alone (*Allow npm
   dist-tag* off, *Once: the registries' side*), and a publish names one
   dist-tag. So after a release that went out under `latest`, the owner
   moves `next` to it with npm's own login (2FA):
   `for p in web react cloudflare langchain; do npm dist-tag add
   @fennecdb/$p@X.Y.Z next; done`, where `npm view @fennecdb/web
   dist-tags.next` names an older version.
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
   one run pushes at a time. The image's `latest` follows the same way
   (`image-latest`, `tools/image-latest.sh`): moved to `X.Y.Z`'s list once
   the release is published, only where it is the newest published release,
   never for a draft, a pre-release or an older version. Where a release
   was published before `image (manifest)` made `X.Y.Z`,
   `packages.yml`'s `image (latest)` fails, saying `X.Y.Z` is not on
   ghcr.io yet. If the tag's `release.yml` run was still going, nothing
   needs re-running: its own `image (latest)` moves `latest` once
   `manifest` has made the list. If its image jobs failed, re-run that
   run's failed jobs (`gh run rerun <run-id> --failed`), not a new
   `gh workflow run`: a new run builds the images again and pushes the arch
   tags under new digests. A re-run within a day pushes the images `image`
   saved; after that they are gone, and `image` must run again too. Of the
   two `image (latest)` jobs, the one that runs second moves it.
   The `.deb` and `.rpm` need no step of their own: step 3 built them, and
   publishing the draft is what makes them downloadable.
5. Once the release is published and `packages.yml`'s run is done, attach the
   app: `gh workflow run desktop.yml --ref main -f release=vX.Y.Z`. A
   candidate has no app: `desktop.yml` takes `X.Y.Z` alone (its `app`,
   `other-systems` and attach jobs refuse `vX.Y.Z-rc.N`), so this step is
   skipped for one. Its
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
   click through. `attach` and `attach-unsigned` attest each download before
   they attach it, as `publish` does.
6. Verify the release, once step 5 has attached the app's downloads (a
   candidate's at once): `tools/release-verify.sh vX.Y.Z`, with `gh` logged
   in, `npm`, `curl` and `jq`. It holds the release to what the workflows
   publish and attest ([docs/design/integrations.md](docs/design/integrations.md#provenance)),
   one line an asset, and stops non-zero at the first thing missing or
   wrong, its last lines (`not checked: ...`) naming every check it did
   not make:
   - the assets are exactly `release.yml`'s 17 -- its own 14 and
     `swift-binary.yml`'s three XCFramework files -- `SHA256SUMS`, and for
     an X.Y.Z the app's 5: 23, none missing and none beside them; a
     candidate's are 18, no app (`tools/release-verify.sh --assets
     vX.Y.Z` lists them, held to the workflows by
     `tools/test_release_verify.py`);
   - a candidate is a pre-release, an X.Y.Z is not, and GitHub's Latest is
     an X.Y.Z;
   - `SHA256SUMS`'s lines before the app's verify as the file `release.yml`
     attested at `refs/tags/vX.Y.Z` (the app's lines follow them, appended
     by `desktop.yml` in step 5), before a sum is read from it; then it
     lists every asset but itself and the `.checksum`, each by its sum.
     It is what ties a file to its name in this release: an attestation
     names bytes, so two of the tag's files swapped under each other's
     names would each verify alone;
   - each other asset verifies as built by its workflow at its ref, on
     GitHub's runners: `gh attestation verify <file> -R
     fennec-db/fennecdb --signer-workflow
     fennec-db/fennecdb/.github/workflows/release.yml --source-ref
     refs/tags/vX.Y.Z --deny-self-hosted-runners`; the XCFramework's with
     `swift-binary.yml` and the one ref of its version (step 2):
     `refs/heads/main` for an X.Y.0 and its candidates,
     `refs/heads/release/X.Y` for a patch; the app's with `desktop.yml` and
     `refs/heads/main`, the one ref `desktop.yml` attaches from. `-R`
     alone takes an attestation by any workflow of the repository at any
     ref -- a branch's, or `desktop.yml`'s `attach` vouching for a server
     tarball;
   - the image, `oci://ghcr.io/fennec-db/fennec-server:X.Y.Z`, `X.Y.Z-amd64`
     and `X.Y.Z-arm64`, with `release.yml`'s flags, each by the digest
     ghcr.io serves for the tag, which the verified statement must name;
   - each of npm's four packages carries SLSA provenance
     (`dist.attestations`) naming `packages.yml` of this repository, and
     `npm audit signatures` passes over them. That command alone is not
     enough: it fails on a bad registry signature or a bad attestation and
     passes a package that has no provenance at all, exit 0;
   - each of PyPI's files has PEP 740 attestations whose publisher is
     `packages.yml` of this repository (`pypa/gh-action-pypi-publish` makes
     them under trusted publishing alone: with `PYPI_API_TOKEN` set it
     makes none).

   Run on a draft, it verifies `release.yml`'s files and the image and
   stops there. A file that does not verify is a release job that attached
   without attesting, or a run that was not the tag's own (step 3): fix
   the workflow, and the next release carries it (a published release's
   files are never replaced, so that release stays unverifiable: step 3).
   What each check proves, and what none does, is in
   [the design file](docs/design/integrations.md#provenance).

   **The app's attest step before 1.0.0.** A candidate has no app files,
   so `attach` and `attach-unsigned` first attest at 1.0.0. Once this is
   merged the owner runs the step for real on a release that has its app
   already: `gh workflow run desktop.yml --ref main -f release=v0.2.0`.
   Its build jobs rebuild the app with 0.2.0's server, `attach-unsigned`
   attests what they made (Sigstore's public log and GitHub's attestations
   API take a record of each, naming the run), and `attach.sh` then holds
   each rebuilt file to the asset of its name, finds other bytes and fails
   before it writes anything: v0.2.0's assets and `SHA256SUMS` are left as
   they are. Nothing else is written: the build jobs read alone, `sign`
   signs nothing without its secrets (with a reviewer on
   `desktop-signing` it waits for one), and `attach`, the signed DMG's,
   runs only after a signing. **Approve nothing there: reject the
   `desktop-signing` review the run asks for.** Were its signing secrets
   set and the review approved, `sign` would sign the rebuilt DMG and
   `attach` would run `gh release upload v0.2.0 ... --clobber`: a published
   release's DMG replaced (or one added beside it, as the signed DMG's
   name falls), its bytes in no line of v0.2.0's `SHA256SUMS`, so
   `install.sh --app` would refuse v0.2.0's app from then on, and nothing
   puts the first DMG back. The run's red `attach-unsigned` is the
   expected end; what it proves is the attest step's log, and an artifact
   of the run verifying: `gh run download <run-id> -R fennec-db/fennecdb
   -n fennec-studio-app-Linux-X64`, then `gh attestation verify
   fennec-studio-linux-x64.tar.xz -R fennec-db/fennecdb --signer-workflow
   fennec-db/fennecdb/.github/workflows/desktop.yml --source-ref
   refs/heads/main --deny-self-hosted-runners`.
7. Write the release's fixture set, on macOS or Linux, from the tag alone:
   `make compat-fixtures V=X.Y.Z`. It needs rustup with the tag's
   toolchain and its wasm32 target -- the version the tag's
   `rust-toolchain.toml` names, or, where it names a channel (v0.2.0's
   says `stable`), the one `TOOLCHAINS` in `tools/compat_fixtures.py` pins
   (v0.2.0: 1.99.1) -- node, and Docker with the Makefile's `PG_IMAGE`
   already pulled (nothing is pulled: `--follow`'s file is written against
   a PostgreSQL container of the writer's own, on a port the system picks,
   removed after); each is asked for before anything is built or written.
   `tools/compat_fixtures.py build` puts the
   tag's tree in a scratch directory (`git archive`), builds its
   `fennec-server` (`--release`) and `fennec` (`--profile cli`) as the
   tag's `release.yml` does, `--locked --offline` for the host's target
   (nothing downloaded, the tag's rustls, ring and webpki-roots from
   cargo's cache, a cache lacking them named with the `cargo fetch` that
   brings them), its module (the tag's `make wasm`) and
   `tools/compat/writer.rs` as an example of its fennec-core, by the pinned
   compiler and no other, keeps the binaries, the tag's `web/` and its
   @fennecdb/cloudflare module with `MANIFEST.json` naming each by its
   SHA-256 beside the tag, its commit and the compiler, and deletes the
   rest (about ten minutes on an M1); `write` refuses a directory whose
   files are not the manifest's, whose manifest names another tag, commit
   or compiler, or whose copies of the tag's files are not `git show
   <tag>:<path>`'s bytes, before any program of it runs, then
   runs them into `crates/fennec-core/tests/compat/vX.Y.Z/`: a server's
   file and its answers over HTTP, a replica's file and that file
   promoted, a shell's, a checkpoint's graphs, the files a crash leaves, a
   file of the widths no small file holds and one of collection ids of
   five bytes, scheduled statements of every timing alive with their
   marks, a mirrored PostgreSQL table, the forms a later build reads
   otherwise on purpose, two sealed backups (one of several frames), a
   sealed archive and a plain one and what `fennec verify` said of each, a
   tenant node's archive of every tenant and what the release's restore of
   it said at each moment, the release's restore of each archive to every
   moment a later build restores it to, a graph past 65 536 vectors, the
   page's IndexedDB records, its OPFS file and the copy a stopped rewrite
   left beside one, a Durable Object's SQL rows and key-value values as
   the release's @fennecdb/cloudflare kept them, the answers the release
   gave over each, the manifest, and `SHA256SUMS` last (the set's README
   says how long each part took), whose SHA-256 `write` prints for the
   set's `RELEASES` line (`sums`). Written to
   another directory instead, `FENNEC_COMPAT_SETS=<its parent>` runs every
   `compat::` test and `web/fennec.compat.test.js` over it before it
   lands. A release that writes something new -- a record kind, a form
   inside one, a type tag, an index, a flag -- adds the statements writing
   it to `tools/compat_fixtures.py` or `tools/compat/` before its set is
   written, the forms it adds to `ALTERNATIVES` and its line naming what
   it writes to `RELEASES`, both in `crates/fennec-core/tests/compat/walk.rs`,
   which fails a set without its line, a line without its set, a form no
   file holds that its line does not name absent with why, and one a file
   holds that its line does not name. The set goes through a pull request
   like any other with its line pinning the SHA-256 of its `SHA256SUMS`,
   never written again -- nor its README, whose list of the forms no file
   holds is never amended: review it before it lands. A set written again
   whole, its sums with it, fails its pin in every `compat::` test and in
   `make version`. A form the release writes that its set is found to lack
   after it landed goes into a further set of the same tag,
   `crates/fennec-core/tests/compat/vX.Y.Z+2/` (then `+3`, ...): written
   by `make compat-fixtures`'s two steps with binaries built from the same
   tag commit, `write <bin-dir> .../vX.Y.Z+2`, the statements writing the
   form added first, a README, sums, manifest and `RELEASES` line pinning
   it of its own; the first set stays as it landed. `make version`
   refuses while any release tagged from v0.2.0 on (a release candidate
   aside) has no whole set in the tree it runs in, or a set the tree names
   is not the one its line pins, and every later build's `compat::` tests,
   in fennec-core, fennec-http, fennec and fennec-server, and
   `web/fennec.compat.test.js` open every set. A release candidate gets
   no set: its files carry no promise -- a candidate is tried on copies --
   and the release's set is written by the release's own binaries; a
   change to a file format between two candidates is named in the later
   candidate's notes.

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

**Every reader upgrades before any primary.** Across two releases, every
binary that reads a file or a stream -- a replica, a standby, an archiver,
a router, a node, the `fennec` that verifies and restores -- runs the new
release before any primary does. A binary one release back beside a newer
one either works or stops in words, and never reads a record as another;
where it names the record kind, the type tag or the version it does not
know, it names it, and where it knew of no other it cannot: 0.2.0's
`fennec` refuses a version 2 sealed file as "not a sealed file", naming
nothing, so the notes of the release that made version 2 name it. A newer
reader reads what an older writer wrote, but for four values and types
0.2.0 took that no write makes any more
(`docs/design/compatibility.md#what-1x-does-not-read-of-020s-files`,
and *The first 1.x release's notes* below). The order below and
`deploy/RUNBOOK.md`'s *Upgrading with no downtime* keep it, and the notes
of a release that moves a format say which reader stops where it is not
kept. Three things that order cannot say alone:

- **A node pair: the standby's node first.** Between two nodes' restarts,
  wait until the standby holds what the primary does: on the primary's
  node `/t/<tenant>/_replication/status` (the replication token) lists the
  standby's stream with `sent` equal to `durable`, or the standby's own
  says `"connected": true` and `"behind": 0` -- for a file's replica, the
  metrics `fennec_replica_connected` 1 and `fennec_replica_behind` 0. Not
  the alert `FennecTenantsWithoutReplica`, which fires only for a tenant
  with no copy at all, after 15 minutes. Restarting the primary's node in
  place costs that node's tenants their writes for the restart -- the
  router answers each "the request was not sent" (503, `Fennec-Unsent`),
  safe to send again -- since automatic failover is off unless the router
  runs with `--auto-failover`.
- **A node holding both primaries and replicas** -- a `--dir` node holding
  its own tenants and another node's standbys -- cannot be a reader
  upgraded before every primary. In such a fleet nothing uses a feature
  that moves the format until every node runs the release.
- **"Upgrade every router" means building it.** No release publishes
  `fennec-shard` yet (`release.yml` packs `fennec` and `fennec-server`):
  each router is built from the release's tag (`cargo build --release
  --locked -p fennec-shard`), and `compat.yml`'s old router is that tag's
  code, built by this tree's toolchain.

`compat.yml` (`make compat-check`) drives the order with the last
published release's server and `fennec` and its tag's router, weekly and
on every release tag; `make upgrade-check` drives the cases below against
the commits before a kind, an op or the checksums. More than one release
back is not driven.

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

**A sealed frame bound to its file moves a sealed archive at once.** From
the release that binds it (`docs/design/replication.md#sealed-archives`),
an archiver restarted on it seals its next image and segment, and `fennec
backup --key-file` a backup, as version 2 (`FENECSL\x02`, a file id under
every frame's tag), which a `fennec restore` or `fennec verify` from 0.2.0
or earlier refuses (`not a sealed file`, `not sealed, where the archive
is`): its notes say, under their upgrade heading, to upgrade `fennec`
wherever an archive is restored or verified before any archiver, and that
files sealed before stay open under their key -- splices of them with it
-- until a new key (`fennec key`) begins a new archive. The same notes say
that a tenant's `github` hook 0.2.0 saved with `key delivery` is kept, its
events answered 409 until it is set again with `key body`, its other hooks
armed, and that a single server started with `--hooks` whose file has a
`github` line saying `key delivery` refuses to start on the release (exit
2, ``<hooks file>: hooks file line N: `key delivery` is refused: ...``): that
line's `key delivery` is taken out, or made `key body`, before the server
is restarted on it. `make upgrade-check` drives the archive's case: CK_BASE's `fennec
restore` and `fennec verify` refuse a backup and an archive this tree
sealed, which this tree's read. `compat.yml` drives each against 0.2.0's
own binaries: its `fennec` refusing a backup and an archive sealed as
version 2 in those words, writing no file, and opening what it sealed
itself; a hooks file's `key delivery` refused at the start, exit 2; and a
tenant's set kept with it by a 0.2.0 node, its verified event answered 409
once the router and the node are upgraded, nothing written, until it is
set again with `key body`.

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

## 1.0

From 1.0.0 on a release only grows what [the compatibility
contract](docs/design/compatibility.md) freezes; a break is a major
version. Release candidates are tagged `1.0.0-rc.N`, N from 1 -- the one
form a candidate's tag takes, `vX.Y.Z-rc.N`, which every job of
`release.yml` holds in its first step (step 3) -- and the owner pushes
every tag. A candidate goes through [A release](#a-release) as its steps
say for one: drafted and published as a pre-release, never GitHub's
Latest (step 4, where Maven Central's `<release>`, which a candidate
becomes while it is the highest version, is said too), no app (step 5),
and verified as 18 assets (step 6), its XCFramework built from `main`
(step 2), and no fixture set (step 7): a candidate's files carry no
promise, a candidate being tried on copies, so a change to a file format
between two candidates is named in the later candidate's notes, and the
release's set, written by the release's own binaries, is the one later
builds are held to. `make version` (`tools/version.py`)
takes X.Y.Z alone today, so a candidate's version needs it to take a
pre-release part first. Before a 1.x tag, check:

1. `make openapi-check` is green against the previous 1.x tag's documents,
   and `make types-check`, `make builder-golden`, `make schema-golden` and
   `make sync-scenarios-check` pass. `openapi-check` asserts the API only
   grew; the golden targets rewrite their files, so `git diff` must show no
   changed or removed line in them, only added ones.
2. `make fennecql-corpus BASE=<the previous 1.x tag>` finds no text
   reading as another statement or refused: against a 1.x base an
   `ACCEPTED` entry lets a refusal through only with the grammar epoch
   raised, as a moved digest, and a statement no definition holds read
   otherwise only with the epoch or its `same_meaning` -- read every claim
   the summary prints, each a statement said to mean what it meant (CI
   runs it against main, and `BASE=frozen` against every frozen corpus, on
   every pull request). A major version's first tag is frozen with
   `FREEZE=<tag>`, its file added beside the others in
   `integrations/fennecql-corpus/` -- a frozen corpus is added, never
   replaced or removed, which the step against main holds, and a freeze
   reads the earlier corpora's texts too, so a text deleted since is still
   held to its release's reading and to the tag's -- and `GRAMMAR_EPOCH`
   (`crates/fennec-core/src/named.rs`) is still 4.
3. `make api-check` is green and `make api-golden` leaves no diff:
   `fennec.h`'s prototypes and macros, `web/fennec.d.ts` and
   `web/client.d.ts`, the flags and exit codes of `fennec`, `fennec-server`
   and `fennec-shard`, and the `/_metrics` names, types and labels of the
   server and the router only gained entries against
   `integrations/api-golden/` (the CLI's and the metrics' dumps are held to
   the binaries by `make test`). The SDKs' public names have no dump yet
   (they come in 1.0.x, needing outside tools): diff them by hand against
   the previous tag. **Before the 1.0 tag, once:** `python3 tools/api_compat.py --freeze 1.0`
   on the commit to be tagged, committed (it writes
   `integrations/api-golden/frozen.json`, and needs the dumps current and no
   `BREAKS` entry for a release not made yet). `make version V=1.0.0` or
   later refuses until it has been run; from then a break needs a major
   version above 1.0 in its `BREAKS` entry.
4. HEAD opens every file written from v0.2.0 on but for the four
   exceptions *The first 1.x release's notes* name: the `compat::` tests
   pass over every release's set (`crates/fennec-core/tests/compat/`, step
   7 of *A release*), beside `tests/language.fennec` and
   `tests/statements.fennec`; and
   `compat.yml` is green on the release commit: `make compat-check`'s
   matrix against the previous release's server and `fennec` and its
   tag's router (*Two releases beside each other*, under A release) -- a
   replica, an archiver, `fennec`, a router and a node of either release
   beside the other's, each working or stopping in words, never reading
   wrong, and a rolling upgrade under writes losing no acknowledged one. `make upgrade-check` (by hand) still
   passes against the commits before kind 13, its ops and the checksums:
   an older replica stops by name on a record kind or tag it does not
   know. The notes name any feature whose first use moves the file format,
   and say that every reader upgrades before any primary.
5. No asset of the table above is renamed or dropped.
6. The size ceilings hold against the 1.0 baseline (the browser module
   +3.5%, native libraries and binaries +5%), and the notes state the
   measured cost where one was raised.

**The first 1.x release's notes.** "1.x opens every file written from
v0.2.0 on" has four exceptions, all values or types 0.2.0 took that no
write makes any more
(`docs/design/compatibility.md#what-1x-does-not-read-of-020s-files`),
and the first 1.x release's notes name them under their upgrade heading:

1. A `[[json]]` field's row nested past 64 levels: the file opens and
   every other collection reads, but any statement that decodes that row
   fails whole (`a value nests more than 64 levels deep`) -- `count`, a read
   by another row's id and the fields stored before the deep one still
   answer; a `del` or a `set` of the row is refused; `alter ... drop` of
   the field is taken and helps only where no field is declared after it
   (every skip of a dropped place is depth-bound as a read is, so a field
   after it, and a `del` of the row, are refused in the same words), and
   an index built over a later field stays refused. Backups and images
   carry the row unchanged; a replica takes it in an image, holds
   it and refuses its reads alike, and connects again, 100 ms to 5 s
   apart, while a write touching it cannot be applied. On 1.x alone the
   way out is, where the field is its collection's last, to drop it and
   `del` the row; otherwise to read out the readable rows, drop the
   collection and make it again.
2. A type nested past 64 lists,
3. a point's type under 64 lists, and
4. a schema's number past its range (`m` under 2 or past 32 767, a
   `vector<N>` or a `sparse<N>` of 0 or past 10^9, a list of such
   vectors): the whole file is refused at the open, and only v0.2.0 can
   rewrite it.

The advice they give: rewrite such values with v0.2.0 before upgrading.
`compat.yml` drives the first two with v0.2.0's own binaries (a3, a3b; the
engine's tests that hold all four are named in
`docs/design/compatibility.md#what-1x-does-not-read-of-020s-files`).

The previous minor gets security fixes for six months after the next
minor's release; a `release/1.N` branch is cut when a backport needs one
([A security release](#a-security-release)).

## A security release

The policy a reporter reads is [SECURITY.md](SECURITY.md#supported-versions).
Until 1.0.0 there is no such release: a 0.x bug is fixed on `main` and ships
in the next release, with no advisory, no CVE and no patch release, and the
pull request names the releases that had it; a report is still acknowledged
within 3 working days, and its reporter told where the fix stands. From 1.0.0, a report of a bug in
a supported release (the latest 1.x minor, and the previous one for 6 months
after the next ships) goes this way. The owner does each step that needs a
tag, a setting or a secret.

1. **Acknowledge within 3 working days.** Reports arrive through private
   vulnerability reporting
   (<https://github.com/fennec-db/fennecdb/security/advisories/new>). Answer
   in the report, say who looks at it, and reproduce it before anything
   else: from the reporter's input, on the latest 1.x and on the previous
   minor.
2. **Open a draft advisory** from the report (`Security`, `Advisories`),
   naming the affected versions and the first fixed one, and, if the fix
   needs a branch to work on, **a temporary private fork** from it. Until
   step 5 pushes it, the fix is in the fork alone: prepare it and have it
   reviewed there, with no pull request, issue or commit on the public
   repository. Do not assume the fork runs the repository's workflows; run
   the tests the fix touches by hand. The fix becomes public at step 5, at
   the push to `release/1.N`: the swift-binary run is on a public branch and
   opens a public pull request, the tag publishes the commit, and
   `release.yml`'s logs are public. So keep what follows the push short:
   the push, swift-binary, the tag, the publish and the advisory (step 6)
   back to back, with everything else ready beforehand.
3. **Request a CVE** from the advisory (GitHub is the CNA) once the bug is
   confirmed and the affected range known.
4. **Fix on the oldest supported branch and carry it forward.** Cut
   `release/1.N` from the latest `v1.N.*` tag if the branch does not exist
   (`git branch release/1.N v1.N.M`), put the fix there, then in the latest
   minor and `main`. The fix keeps [the contract](docs/design/compatibility.md):
   it only grows what it freezes. Where it cannot -- a protocol or a file
   format -- the advisory names the SDKs and replicas that must move (the
   table above says which package carries which part), and replicas
   upgrade before primaries, as for any release.
5. **Build and publish the patch from its branch.** `make version V=1.N.P`
   and the change reviewed in the private fork, then pushed to
   `release/1.N`, and from here the fix is public. Then, as in
   [A release](#a-release): step 2, always, since `release.yml` stops a tag
   whose `Package.swift` `release` is not the tag's version and wants a
   swift-binary run for it, and `make version` moves `release` but not the
   checksum: `gh workflow run swift-binary.yml --ref release/1.N -f
   version=1.N.P`. Its pull request is opened against the branch the run
   was started on, `release/1.N`: merge it there. Then the owner
   tags the branch's commit (`git tag v1.N.P <commit>`, then `git push
   origin v1.N.P`) and `release.yml` drafts the release. A tag already
   pushed is built again by `gh workflow run release.yml --ref v1.N.P -f
   tag=v1.N.P`: at the tag, so the run is the tag's own and its
   attestations name `refs/tags/v1.N.P`, as [A release](#a-release)'s step
   6 verifies (one dispatched from `release/1.N` builds the same tree but
   attaches it unattested, since its attestations would name the branch:
   step 3 there). The swift-binary run's files are attested as built on
   `release/1.N`, the one ref that step 6 takes for a patch's (and never
   for an X.Y.0's, step 2 there). The power-cut checks of step 3 are read the same way,
   with `--ref release/1.N` where they say `--ref main`. Read the draft,
   publish it, and `packages.yml` follows, as in steps 4 to 6 there: every
   channel of the table above -- PyPI, npm, NuGet, Maven Central, pub.dev,
   Homebrew, the `.deb` and `.rpm`, the image on ghcr.io, the Go module's
   tag, the Swift package's tag and zip, the install scripts (which read
   the latest release, so a patch to an older minor reaches them only as
   the newest minor's own) and the desktop app -- gets the patch; check
   each by installing it.

   **A patch to a minor that is not the newest** moves none of the
   pointers that mean "the newest"; each is kept there by the pipeline,
   by version order (`sort -V`), never by which release was published
   last:
   - GitHub's Latest, which `site/install.sh` and `site/install.ps1` read:
     GitHub makes a release Latest as it is published, whatever its
     version, so publish it with `gh release edit v1.N.P --draft=false
     --latest=false` and Latest does not move. Published without it,
     `packages.yml`'s `github-latest` (`tools/release-latest.sh github`)
     puts Latest back on the newest release within the minute its run
     takes, the installs in between taking the patch.
   - The image's `latest`: `image-latest` (`tools/image-latest.sh`) moves it
     only to the newest published release; `1.N.P` gets its own tag alone.
   - npm's `latest`: `packages.yml`'s `npm` publishes `1.N.P` under the
     dist-tag `release-1.N` (`npm install @fennecdb/web@release-1.N`), never
     `latest`; a pre-release goes out under `next` where it is the newest
     pre-release by version, and `next-1.N` where it is not, so `next`
     never moves back either. Not `v1.N`: npm refuses a dist-tag that reads
     as a semver range. A package's first publish ever is `latest`
     whatever the tag: the registry gives a package with none that
     version. The tag is decided from the
     release list as the job runs, and the job's credential (a trusted
     publisher allowing `npm publish` alone) cannot move it after, so
     publish one release at a time, each once the previous one's
     `packages.yml` run is done; `next` left on a candidate older than the
     newest release is moved by hand (step 4 there).
   - The Homebrew tap is never moved back (`homebrew` skips a release older
     than the formula).
   - The rest have no pointer the pipeline sets: PyPI, NuGet, pub.dev and
     Maven Central each name their newest by version order (a `pip
     install`, a `dotnet add`, a `dart pub add` takes the highest; Central
     writes `maven-metadata.xml`'s `<release>` as the highest version, a
     candidate among them (step 4), and `packages.yml` uploads none of its
     own), Go's proxy answers `@latest`
     with the highest `integrations/go/v*` tag, SwiftPM resolves the
     highest tag a requirement allows, and the `.deb` and `.rpm` are files
     of the release, named by its version.
6. **Disclose.** Publish the advisory (with the CVE) the same hour the
   release is published, as step 2 says, and put it in the release's notes with the first
   fixed version of each supported minor. Merge the private fork's changes
   into the public branches where the fix was made there. Credit the
   reporter if they agree, and close the report.

This procedure is written and not yet run. Its rehearsal -- a draft
advisory, a private fork, and a patch built from a release branch through
`release.yml`'s dispatch to the draft release -- is owed before 1.0.0, and
its elapsed time goes in the roadmap's log.
## A toolchain bump

The compiler is part of what a release ships, and it moves only by a pull
request of its own ([docs/design/crates.md](docs/design/crates.md#toolchain)).
`rust-toolchain.toml` names an exact release, never `stable`, which moved
under main's CI on 2026-11-12 with no change of ours: 1.100.0's Unicode
tables failed fennec-core's case tests on every pull request. A bump is
deliberate, not each stable release -- one taken for what it brings, a fix,
a lint, a measured speed or size -- and its pull request changes nothing
else but what the new compiler needs:

1. `rust-toolchain.toml`'s `channel`, the `FROM rust:` of `Dockerfile` and
   `deploy/Dockerfile` and the Kotlin harnesses' image
   (`integrations/kotlin/run-tests.sh`, `examples/kotlin-android/run-smoke.sh`),
   each tag with the index digest `docker buildx imagetools inspect
   rust:X.Y.Z-alpine` (`-slim-bookworm`) names; `python3
   tools/toolchain_check.py` passes.
2. What the compiler breaks, fixed in the same pull request -- a lint, a
   table held to the standard library's (fennec-core's case tables) --
   never a test held back.
3. In its body, against main:
   - `make wasm-sizes`, before and after: what each feature adds to the
     module and what leaving it out saves;
   - the size report: CI's size job measures the module against the base,
     each built by its own `rust-toolchain.toml` and named in the summary,
     and `make size-report BIN=fennec-server` and `BIN=fennec`, run on main
     and on the branch, the binaries' code by crate; a size the docs quote
     that moved is written again, which `site/build.py` asks for under CI;
   - `make instr-compare`: the instructions a request costs, read over
     re-signed copies ([docs/design/benchmarks.md](docs/design/benchmarks.md#instruction-counts));
   - `make float-test`, which `float-tests.yml` also runs on the pull
     request: fennec-core's own float formatting and reading held to the
     new standard library's over every `f32` and 10^8 random `f64`s.

A tag's `release.yml` holds its binaries' `rustc -V` to the file's release
and the image's to theirs, and the draft's notes name it.

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
upload. Then, pasting each when asked, each into the environment of the
job that reads it and not the repository's (GitHub, below):

    gh secret set NPM_TOKEN --env npm -R fennec-db/fennecdb
    gh secret set PYPI_API_TOKEN --env pypi -R fennec-db/fennecdb

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
   as `NPM_TOKEN`, a secret of the `npm` environment.
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
failure, and the other registries go on. A key kept for that is a secret
of the `nuget` environment (`gh secret set NUGET_API_KEY --env nuget -R
fennec-db/fennecdb`), not the repository's; once the policy has pushed a
release it is better deleted, the repository's too (`gh secret delete
NUGET_API_KEY -R fennec-db/fennecdb`): a key for another package's name only
fails the push where the exchange does. A release published before either was set gets its package from a
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
   private half, armored, and its passphrase.

The Portal has no trusted publishing, so these four stay secrets, kept in
the `maven` environment and not the repository's: limit it to `main` and
`v*` tags first (the NuGet section's three commands, naming `maven`), then

       gpg --armor --export-secret-keys <id> | gh secret set SIGNING_KEY --env maven -R fennec-db/fennecdb
       gh secret set SIGNING_PASSWORD --env maven -R fennec-db/fennecdb
       gh secret set MAVEN_CENTRAL_USERNAME --env maven -R fennec-db/fennecdb
       gh secret set MAVEN_CENTRAL_PASSWORD --env maven -R fennec-db/fennecdb

and delete any of the same names from the repository's secrets (`gh secret
delete SIGNING_KEY -R fennec-db/fennecdb`, and so on). The upload is
`publishingType=AUTOMATIC`: the Portal releases the deployment once it
validates, and a version is there for good.

**pub.dev.** pub.dev's automated publishing trusts only a workflow started
by a tag's push, and `packages.yml` follows a published release, so it
publishes with an account's credentials. Signed in to pub.dev as the account
that will own `fennecdb` and `fennecdb_flutter` (a verified publisher,
`fennecdb.dev` or the like, can own them after): `dart pub login` on any
machine writes `pub-credentials.json` (`~/Library/Application
Support/dart/` on macOS, `~/.config/dart/` on Linux); then, the `pub`
environment limited to `main` and `v*` tags first (the NuGet section's
three commands, naming `pub`),

    gh secret set PUB_CREDENTIALS --env pub -R fennec-db/fennecdb < <that file>
    gh secret delete PUB_CREDENTIALS -R fennec-db/fennecdb # the repository's, if there is one

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
packages publish by trusted publishing; `maven` and `pub` hold their
secrets and are limited the same way. A job reads only the secrets of the
environment it names, and a repository secret of the same name too: each
is moved by setting it in its environment (`gh secret set <NAME> --env
<environment> -R fennec-db/fennecdb`) and then deleting the repository's
(`gh secret delete <NAME> -R fennec-db/fennecdb`), its environment limited
first, since a secret in an environment with no rules is read by a job of
any branch that names it. `tools/workflow_check.py` refuses a job that
reads a secret and names no environment, but for one it lets off by name
with the reason.

`site.yml`'s deploy is that one, until its environment exists: the
Cloudflare secrets (`CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, and
`CLOUDFLARE_AI_TOKEN` where there is one: site/README.md) are the
repository's, which any branch's workflow reads. A workflow naming an
environment that does not exist makes GitHub create it at its next run,
with no rules, so the owner creates `site` first, limited to `main` alone:

    gh api -X PUT repos/fennec-db/fennecdb/environments/site \
      -F 'deployment_branch_policy[protected_branches]=false' \
      -F 'deployment_branch_policy[custom_branch_policies]=true'
    gh api -X POST repos/fennec-db/fennecdb/environments/site/deployment-branch-policies -f name=main -f type=branch

then a pull request puts the line back in `site.yml`'s `deploy`
(`environment: { name: site, url: https://fennecdb.com }`) and drops the job
from `REPOSITORY_SECRETS` in `tools/workflow_check.py`, which fails while
the entry outlives its reason; once it has merged, the three secrets move
into `site` as above.

The repository's own settings hold what the workflows cannot. Every action
a workflow runs is pinned to a commit (`docs/design/integrations.md#workflows`);
GitHub refuses one that is not, a pull request's workflow among them, once
the Actions setting says so:

    gh api -X PUT repos/fennec-db/fennecdb/actions/permissions \
      -F enabled=true -f allowed_actions=all -F sha_pinning_required=true

A `v*` tag is what `release.yml` builds and SwiftPM, Go's checksum database
and the packages name, so it is created by the owner alone, deleted by the
owner alone, and moved by nobody: under *Settings*, *Rules*, *Rulesets*,
a tag ruleset on `v*` restricting creations and deletions with the
repository's admins as its bypass, and a second restricting updates with
none. A tag pushed by mistake is never moved: the owner deletes it, or the
next version replaces it.

**After the first publish.** The docs install what a registry holds: PyPI's
`fennecdb` since 0.1.5 (`pip install "fennecdb[langchain]"`). Until
`@fennecdb/web` is on npm the React example imports `sync` from
`./fennec.js`; then from `@fennecdb/web`, with `npm install @fennecdb/web
@fennecdb/react` beside it.
