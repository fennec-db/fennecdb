# CLAUDE.md

The conventions that bind code under `crates/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

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
fuzz` lists the targets; CI's `fuzz-smoke` runs each by count, `fuzz.yml`
weekly): a new parser owes its target and `fuzz.yml`'s matrix line, a
finding a regression test in its own crate.

**[Every `unsafe` block says why it is sound.](../docs/design/conventions.md#unsafe-code)**
A `// SAFETY:` comment above each `unsafe` block and `unsafe impl`, or above
the statement it is, names what guarantees it -- for a C ABI the contract
fennec.h and its `# Safety` sections write, never "the caller guarantees"
alone -- and an unsafe operation in an `unsafe fn` is in a block of its
own. Clippy's `undocumented_unsafe_blocks` and rustc's
`unsafe_op_in_unsafe_fn`, denied in the root Cargo.toml's
`[workspace.lints]`, which every crate takes, hold it where code compiles;
`tools/lints_check.py` (CI's fmt job) reads every `.rs` file whatever its
`cfg`. [fennec-ffi's, fennec-abi's and fennec-core's tests run under
ASan](../docs/design/conventions.md#sanitizers) (`make sanitize`, CI's
`sanitize`, no required check, never a merge group's): every unsafe site
they hold is reached by one, and a new site owes its test.

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
held. `SECURITY.md` is held to the tree the same way, by
`tools/security_md_check.py` (CI's fmt job): a flag, path, link or primitive
it names that is gone fails, and so does any kind of check counting zero.
