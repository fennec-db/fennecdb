# AGENTS.md

The regions' deploy kit's invariant, for code in `deploy/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

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
