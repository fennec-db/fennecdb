# AGENTS.md

The conventions that bind code under `crates/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Conventions

**[Unit tests live inline](../docs/design/conventions.md#test-binaries)**
(`#[cfg(test)] mod tests`); cross-crate and protocol tests in
`crates/*/tests/`, a file each, one binary a crate through `tests/all.rs`
(`autotests = false`: a new file needs its `mod` line). A test reading the
process's own counts (`/_metrics`, the statements'), one whose servers hold
many threads and one opening the audit log (`tests/audit.rs`,
`tests/refusals.rs`) are each a `[[test]]` of its own, the other tests'
servers asking with no refusal wait (`audit::set_delay(0)`). Measurement
programs (`examples/`, `fennec-bench`) are wired to `make`, not CI, but the
perf gate's workloads (`fennec-http/examples/gate.rs`); an example that is a
check -- fennec-ql's `corpus` -- runs in CI's test job.

**[Every parser of untrusted bytes is fuzzed](../docs/design/conventions.md#fuzzing)**
by `crates/fennec-fuzz`, the repository's own mutator under ASan (`make
fuzz` lists the targets; CI's `fuzz-smoke` runs each 30 s, `fuzz.yml`
weekly): a new parser owes its target and `fuzz.yml`'s matrix line, a
finding a regression test in its own crate.

**[A secret is compared by `crypto::ct_eq` alone](../docs/design/conventions.md#secrets-compared)**
-- a token, a mark, a MAC, a signature, a proof, a password -- never by
`==` or `!=`, which leave at the first byte that differs:
`tools/secret_eq_check.py` (CI's fmt job) refuses one on an operand so
named in `crates/*/src` outside tests, each operand read whole, a name
that is no secret allowed with why and how many times. A primitive we
write is held to its RFC's vectors, every one, to every case of Project
Wycheproof's file for it where there is one (committed unchanged, an
`acceptable` case to the answer chosen for its kind), and where a key
meets its block to another implementation's, each test's count of cases
held.
