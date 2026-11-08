# AGENTS.md

The invariants of what both C ABIs are handed -- a vector marked exact -- and of the sync -- a replica's sync as a state machine and the scenario file both runners are held to -- for code in `crates/fennec-abi/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Vector parameters

**[A vector marked exact is a list of numbers as it
came.](../../docs/design/vector-search.md#vector-parameters)** A vector
handed apart with its place's top bit set (`EXACT`) holds the numbers its
sender was handed, each one an `f32` holds: an `[int]`, `[float]` or
`[timestamp]` takes it without asking for JSON (`exact_noting`), whole
`f32`s short of 2^24 as the vector, any other as the list of its numbers.
An unmarked one, as an older binding hands it, and a marked one bound for
a json field, an `in $n` or a path are asked for as JSON (`Refused::Exact`).
A binding marks only a typed `f32` array, ints of at most 2^24 and doubles
an `f32` holds (Swift's `Fennec.vector`, Kotlin's `Values.exact`, Dart's
`exactVector`, `vectorsApart`), or a list stores a rounding as written.
Whether a collection's fields keep numbers is its flag, set with its
schema wherever that changes (`Collection::numbers`, `set_schema`; a debug
build holds it to the fields at every look): natively a `put` into one
with none is answered at a look (`Database::exactly_needs`).

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
