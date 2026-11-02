# AGENTS.md

fennec-server's invariant -- every way it stops is the one shutdown -- for code in `crates/fennec-server/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Durability

**[Every way a server stops is the one shutdown.](../../docs/design/durability.md#the-one-shutdown)**
A signal's handler, `--exit-on-stdin-close`'s end of stdin
(`request_shutdown`) and a failure of what runs beside the database
(`stop_with`, its exit code) set one flag, `SHUTDOWN`; the syncer, a file's
or a node's, looks every `LOOK` whatever `--sync` is, syncs, checkpoints
(only once `serving`, and exiting 0, `checkpoints`) and exits holding the
write lock. A new way down sets that flag and never exits on its own. What
fails before anything writes the file or runs beside it -- a flag, the open,
a port, both listeners bound straight after the open -- exits where it
fails, having written only `<file>.lock` and what the open does.
