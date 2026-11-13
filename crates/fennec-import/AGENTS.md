# AGENTS.md

`--follow`'s invariant, for code in `crates/fennec-import/` and the wire client it reads through, `crates/fennec-wire/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Replication and archives

**[`--follow` confirms nothing that is not on disk.](../../docs/design/replication.md#follow)**
Every write is a put or delete by id; the mirror is a `fennec-server` thread,
its collections' only writer. A wire message's length is held to
`proto::MAX_MESSAGE` before anything is allocated (`proto::zeroed`), and
the iteration count a server asks SCRAM for to `SCRAM_MAX_ITERATIONS`
(2^22) before PBKDF2 runs, refused naming the range and what brings a
server under it, the role's password set again under a lower
`scram_iterations`: the server, or whoever stands in for it, chooses it.
