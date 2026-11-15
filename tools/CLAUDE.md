# CLAUDE.md

The install channels' invariant, for `tools/homebrew-formula.py`, `tools/linux-packages-check.sh`, `tools/windows-zip.sh`, `tools/windows-zip-check.ps1`, `tools/install-ps1-check.ps1`, `tools/linux-ffi.sh`, `tools/linux-ffi-check.sh`, `site/install.sh`, `site/install.ps1` and the release workflows. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

## Invariants

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
every change), and the C library for Linux linked into a C program, shared
and static, run on its runner and on the oldest glibc its docs name, its
exports and glibc symbols held (`tools/linux-ffi-check.sh`, CI's `linux (C
library)` on every change); a new channel names what it installs by its
checksum. A
release job that builds runs with `contents: read` and a checkout keeping no
token (`binaries`, `windows`, `linux-ffi`, `linux-packages`): the crates' build scripts
run there, and only the jobs that publish write.
