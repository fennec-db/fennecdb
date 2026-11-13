# CLAUDE.md

The regions' deploy kit's invariants -- the kit and its chaos drill -- for code in `deploy/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

## Invariants

### Integrations and deploy

**[`deploy/` is the regions' kit, held to the binaries by two
checks.](../docs/design/integrations.md#deploy)** Tokens go in env files or
`LoadCredential=`, never on a command line, a role a user each; no
`ProcSubset=pid`. The tunnel passes only tenant paths, never
`_replication` (`deploy/test/ingress.py`); the bucket copy is `rclone
copy`, never `sync`; a standby router's unit conflicts with its primary's,
promoted by `POST /_replication/promote` and the units switched at once;
`tests/datadog.rs` holds the alerts to scrapes.

**[`make chaos` breaks a region by seed, one fault at a time, and holds its
history to an oracle.](../docs/design/integrations.md#the-chaos-drill)**
Each fault heals, and every replica's rows equal its primary's, before the
next; at the end the YCSB register checker passes, every append answered
is found and none refused, and no history gives a change two writes or
answers an older term after a newer (`drill/oracle.py`, held by
`drill/test_oracle.py`). A violation is a bug or a window the design
names, reported by its seed, never a check loosened; every container,
the driver's too, under `--cpus` and `--memory`. Its nodes run the one
build naming `test-clock` (`Dockerfile`'s `chaos`), which says so in
`--version`, a release's package check refusing it.
