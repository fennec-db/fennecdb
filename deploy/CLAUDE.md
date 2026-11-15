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
Each fault lands -- drawn again where it did not, three misses in a row
failing the seed, every kind landed once -- and heals, no netem left (read
back) and every replica's rows equal its primary's, before the next; at
the end the YCSB register checker passes, every write and append answered
is found -- a register write's value in the ledger `wl` its block lands --
and none refused, and no term names two histories nor a history two terms,
no change is given two writes and no older term answers after a newer
(`drill/oracle.py`, held by `drill/test_oracle.py`). A write lost fails the
seed but in the window the design names (`documented`: answered while its
span's fault held the promoted copy behind, or in a term a failover of that
span began), where it is counted and named; a violation is reported by its
seed, never a check loosened or a window widened. Every container, the
driver's too, under `--cpus` and `--memory`, at the kit's lease. Its nodes
run the one build naming `test-clock` (`Dockerfile`'s `chaos`): no default
reaches it and nothing else builds with it (`tools/release_features_check.py`,
CI's fmt job), and `release.yml` refuses a tarball's or the image's
`fennec-server` whose `--version` says more than its version.
