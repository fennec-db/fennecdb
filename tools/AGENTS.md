# AGENTS.md

The install channels', the toolchain's and the workflows' invariants, for `tools/homebrew-formula.py`, `tools/linux-packages-check.sh`, `tools/windows-zip.sh`, `tools/windows-zip-check.ps1`, `tools/install-ps1-check.ps1`, `tools/linux-ffi.sh`, `tools/linux-ffi-check.sh`, `tools/toolchain_check.py`, `rust-toolchain.toml`, `tools/image-check.sh`, `tools/image-latest.sh`, `tools/release-latest.sh`, `tools/release-verify.sh`, `site/install.sh`, `site/install.ps1`, the release workflows (`release.yml`, `packages.yml`, `swift-binary.yml` and `desktop.yml`'s attach jobs), and every workflow in `.github/workflows/` with `tools/workflow_check.py`, `tools/install-flutter.sh` and `.github/dependabot.yml`; the toolchain's rule binds every Dockerfile and script naming a rust image too. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### The crates

**[The compiler CI tests with is the one that ships.](../docs/design/crates.md#toolchain)**
`rust-toolchain.toml` names an exact release, never `stable`; every rust
image a Dockerfile or script names is that release by digest, the file
kept in the build (`tools/toolchain_check.py`, the fmt job), and
`release.yml` stops where a binary's `rustc -V` differs from it or the
image's. A bump is a pull request of its own carrying `make wasm-sizes`,
the size report, `make instr-compare` and `make float-test`.

### Integrations and deploy

**[A release is installed from its own files, each checked before it is
used.](../docs/design/integrations.md#install-channels)** Homebrew's formula,
`install.sh`, `install.ps1`, the `.deb` and the `.rpm` install
`release.yml`'s binaries -- the tarballs, and on Windows the zip -- by
checksum, building none; `install.sh --app` and `install.ps1 -App` install
the app (`desktop.yml`'s) by its line in `SHA256SUMS`: the Mac's DMG on
Apple silicon alone, into `~/Applications`, its copy's signature checked
before it replaces one; Linux's tarball where glibc 2.38 and WebKitGTK 4.1
are, asked before anything is downloaded, and Windows' zip, each into
`<appdata>/dev.fennecdb.studio/stable/app` with a menu entry or a Start-menu
shortcut, by rename, never while it runs, nothing of it run. A published
release's assets are never replaced (`publish` only adds), but for
`SHA256SUMS`, which only grows by the app's lines. The formula is `tools/homebrew-formula.py`'s alone
(`--check`, `--verify`), pushed by `packages.yml` once published, never a
pre-release or a version older than the tap's, one run at a time, its
`HOMEBREW_TAP_TOKEN` a `homebrew` environment secret given `git` by its
environment alone. `install.sh` holds only functions, assignments and
comments before its last line, `main "$@"`, and `install.ps1` only
fennec-named functions before `Install-Fennec @args`, invoking no expression
and starting no program through the shell's objects
(`tools/install-ps1-check.ps1`, PowerShell's own parser, never a reading of
lines: CI's Windows job and `site/build.py`), in ASCII (`site/build.py`);
`install.sh` takes curl or a wget checking certificates. Each checks the sum
before it unpacks, touches no shell file, profile or `PATH`, and evals
nothing (`site/test_install.sh`, `site/test_install.ps1`). A package is installed and run in a container,
and the Windows zip through `install.ps1` on the runner that built it,
before it is kept (`tools/linux-packages-check.sh`,
`tools/windows-zip-check.ps1`, which CI's `windows (release zip)` runs on
every change), the C library for Linux linked into a C program, shared
and static, run on its runner and on the oldest glibc its docs name, its
exports and glibc symbols held (`tools/linux-ffi-check.sh`, CI's `linux (C
library)` on every change), and the image on the runner that built it -- its
`--version`, its server healthy by its own HEALTHCHECK
(`tools/image-check.sh`, which CI's Grafana job runs on the image it
builds) -- before `image-push` pushes it, from a Docker on its classic
image store (`ubuntu-24.04`), each tag and the `<version>` list then read
back and held to v0.2.0's Docker v2 manifests and gzip layers; `latest`
moves to the newest published release alone, never a draft, a pre-release
or back, as the formula does: `tools/image-latest.sh`, which
`packages.yml`'s `image-latest` runs once the release is published and
`release.yml`'s once `manifest` has made `<version>`, so whichever runs
second moves it; both jobs' tokens read alone and see no draft, so it reads
the release by its tag through the API, which answers a draft with a 404
whatever the token (`gh release view` fails on one), and takes no error but
that 404 and the registry's `not found` for an answer, failing on any
other, as one finding no `<version>` fails saying so, never green
(`tools/test_image_latest.py`, under dash). GitHub's Latest, which the
install scripts read, is held there too: GitHub makes every release it
publishes Latest unless it is a pre-release, so `release.yml` drafts a
candidate as one -- `vX.Y.Z-rc.N`, N from 1, the one form beside `vX.Y.Z`
that every job of `release.yml` and `swift-binary.yml` holds in its first
step and both scripts take (`tools/test_release_verify.py` holds each copy
to one definition) -- and `packages.yml`'s
`github-latest` puts Latest back on the newest published X.Y.Z whatever
was published, a patch to an older minor or a candidate; npm's `latest`
names that release alone, an older minor's patch going out under
`release-X.Y`, and `next` the newest pre-release by version, an older one
going out under `next-X.Y` (`tools/release-latest.sh`, the same `sort -V`
and the same reading of errors, `tools/test_release_latest.py`, under
dash). Neither script takes the API's 404 for a release's tag as a draft in
the run that release's publish started: it reads again a bounded number of
times, then fails. A new channel names what it installs by its checksum.
Every file a release attaches, and the image's list and both its
architectures' manifests by their digests, is attested as the build of the
workflow that made it, by a job of that run that builds nothing
(`actions/attest-build-provenance`, `id-token` and `attestations` beside
that job's write), before it is attached: `release.yml`'s `publish` and
`manifest` in the tag's own run alone (its ref the tag, its commit the
one built), since an attestation names the run's ref and commit as the
source, a branch's dispatch attaching unattested; `swift-binary.yml`'s
`pull-request` the XCFramework it built before the tag on its version's
one branch (`main` for an X.Y.0 and its candidates, `release/X.Y` for a
patch, each job refusing another), which `publish` does not attest again;
`desktop.yml`'s attach jobs the app; and `publish` `SHA256SUMS`, which ties
a file to its name, the app's lines appended after it byte for byte.
`tools/release-verify.sh` (RELEASING.md's step 6) holds a release's assets
to the list the workflows attach (`tools/test_release_verify.py` holds the
list to them, every job uploading a `dist-*` artifact by its name),
verifies `SHA256SUMS`'s lines before the app's as `release.yml`'s at the
tag before it reads a sum, verifies each asset with `--signer-workflow`, `--source-ref` and
`--deny-self-hosted-runners` -- never `-R` alone, which takes any workflow
at any ref -- the image by the digest ghcr.io serves, npm's provenance by
`dist.attestations` (`npm audit signatures` passes a package with none)
and PyPI's attestations, and fails at the first thing missing or wrong,
naming every check it did not make ([provenance](../docs/design/integrations.md#provenance)). A release job that builds runs with `contents: read` and
a checkout keeping no token (`binaries`, `windows`, `linux-ffi`, `wasm`,
`android`, `linux-packages`, `image`): the crates' build scripts run there, and only
the jobs that publish write (`publish`, `image-push`, `manifest`,
`image-latest`, `github-latest`).

**[A workflow runs actions pinned by commit, and a job that writes builds
nothing.](../docs/design/integrations.md#workflows)** Every `uses:` names a
40-hex commit with the tag it was taken from in a comment, the commit's
most specific, X.Y.Z (`@<sha> # v7.0.1`, never `# v7`: the only comment
Dependabot rewrites as it moves the pin; an action that publishes no X.Y.Z
tag is in `MAJOR_TAGS` with the reason), never a tag or a branch, and no
action runs another by a tag inside it (a composite one's `action.yml`,
which `sha_pinning_required` refuses under a pinned commit too: Flutter
comes from its archive, `tools/install-flutter.sh`, held to the sum the
workflow names and to the version Flutter then reports); every workflow
says `permissions:` at its top (where one does not, each job naming none
is a finding by its name), and a job widens only what it uses; a job
holding any write, `id-token: write` among them, runs no build: it runs
only what the check lists, each entry with the reason it builds nothing --
every command its `run:` scripts run, read as their shell reads them
(`$(...)`, `${X:-$(...)}`, `sh -c`, `bash -s <<EOF`, `eval`, `trap`),
being the shell's own, a function the script defines, a program in
`WRITE_COMMANDS` put to a use it lists (`docker push`, never `docker
build`; `npm publish --ignore-scripts`) or a script in `WRITE_SCRIPTS`, and
each action one in `WRITE_ACTIONS` -- so anything else, a build under any
name among it, is a finding (the check fails closed); it takes what a job
that reads alone built as an artifact (`release.yml`'s `image` to
`image-push`, `swift-binary.yml`'s `build` to `pull-request`); and a job
reading a stored secret names its `environment:`, where the secret is kept
under rules saying which runs may read it, but never one that does not
exist yet, which GitHub would create with no rules; and no `run:` script,
nor `actions/github-script`'s `script:`, names in an expression a value a
person outside the workflow chooses --
an input, the event's payload, a branch's or a tag's name, `env` (one
list, `CHOSEN`) -- which GitHub pastes into the script's text, so a tag
named `v1$(...)` would run as the job: such a value reaches a step through
`env:` or `with:`, held to its form in the job's first step before any use,
and a local action (`./...`), whose steps are not read, is refused.
`tools/workflow_check.py` holds every workflow to all five in CI's fmt
job, itself held by `tools/test_workflow_check.py`; a job let off the
third or the fourth is listed in `WRITE_AND_BUILD` or `REPOSITORY_SECRETS`
with the reason (site.yml's `deploy`, until the owner creates its
environment), and an entry nothing needs is a finding: a job let off that
no longer needs it or is gone, or whose workflow is gone or renamed, an
action in `MAJOR_TAGS` no pin names by its major alone, a script in
`WRITE_SCRIPTS` no job that writes runs.
