# CLAUDE.md

The install channels' invariant, for `tools/homebrew-formula.py`, `tools/linux-packages-check.sh`, `site/install.sh` and the release workflows. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

## Invariants

### Integrations and deploy

**[A release is installed from its own files, each checked before it is
used.](../docs/design/integrations.md#install-channels)** Homebrew's formula,
`install.sh`, the `.deb` and the `.rpm` install `release.yml`'s binaries by
checksum, building none; a published release's assets are never replaced
(`publish` only adds). The formula is `tools/homebrew-formula.py`'s alone
(`--check`, `--verify`), pushed by `packages.yml` once published, never a
pre-release or a version older than the tap's, one run at a time, its
`HOMEBREW_TAP_TOKEN` a `homebrew` environment secret given `git` by its
environment alone. `install.sh` holds only functions, assignments and
comments before its last line, `main "$@"` (`site/build.py`), takes curl or
a wget checking certificates, checks the sum before it unpacks, touches no
shell file and evals nothing (`site/test_install.sh`). A package is
installed and run in a container before it is kept
(`tools/linux-packages-check.sh`); a new channel names what it installs by
its checksum.
