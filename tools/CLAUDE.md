# CLAUDE.md

The install channels' invariant, for `tools/homebrew-formula.py`, `tools/linux-packages-check.sh`, `tools/windows-zip.sh`, `tools/windows-zip-check.ps1`, `site/install.sh`, `site/install.ps1` and the release workflows. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

## Invariants

### Integrations and deploy

**[A release is installed from its own files, each checked before it is
used.](../docs/design/integrations.md#install-channels)** Homebrew's formula,
`install.sh`, `install.ps1`, the `.deb` and the `.rpm` install
`release.yml`'s binaries -- the tarballs, and on Windows the zip -- by
checksum, building none; a published release's assets are never replaced
(`publish` only adds). The formula is `tools/homebrew-formula.py`'s alone
(`--check`, `--verify`), pushed by `packages.yml` once published, never a
pre-release or a version older than the tap's, one run at a time, its
`HOMEBREW_TAP_TOKEN` a `homebrew` environment secret given `git` by its
environment alone. `install.sh` holds only functions, assignments and
comments before its last line, `main "$@"`, and `install.ps1` only
functions and comments before `Install-Fennec @args`, in ASCII
(`site/build.py`); `install.sh` takes curl or a wget checking
certificates. Each checks the sum before it unpacks, touches no shell
file, profile or `PATH`, and evals nothing (`site/test_install.sh`,
`site/test_install.ps1`). A package is installed and run in a container,
and the Windows zip through `install.ps1` on the runner that built it,
before it is kept (`tools/linux-packages-check.sh`,
`tools/windows-zip-check.ps1`, which CI's `windows (release zip)` runs on
every change); a new channel names what it installs by its checksum.
