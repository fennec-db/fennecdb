# CLAUDE.md

The sync's invariants -- a replica's sync as a state machine and the scenario file both runners are held to -- for code in `crates/fennec-abi/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### The native library and sync

**[A replica's sync is a state machine; the network is the
binding's.](../../docs/design/native-and-sync.md#sync-core)** `fennec_abi::sync`
does no I/O: a binding feeds it events in turn and performs its actions.
Only `fennec-ffi` turns on `fennec-abi`'s `sync` feature; the browser module
has none of it.

**[Both sync runners are held to one scenario
file.](../../docs/design/native-and-sync.md#sync-scenarios)** A behaviour of
either changes in `integrations/sync-scenarios.json` first;
`make sync-scenarios-check` fails unless both ran every scenario.
