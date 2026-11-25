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

**[A file is made by `fennec_core::fs`'s helpers, its owner's alone](../docs/design/durability.md#files-modes)**
-- `private` on the open of a file its operator names, which goes through
a link there as a database's open does (`fs::resolved`); `private_beside`
(`O_NOFOLLOW`, each target's number, a Unix without one failing to build)
on a file opened again beside a database; `create_private`,
`create_like` (`O_EXCL`) or `copy_private` (a clone with `CLONE_NOFOLLOW`
on Apple's systems, of a file its owner's alone) for a side file written
whole, these following no link at their name, an archive's records, a
backup, an unseal, a compact's swap and a replica's image held to their
writer's handle before their rename (`still_named`, the handle
`Sink::adopt` is handed), one a crash left beside a database removed by its
owner alone (`fs::LEFT_BESIDE`'s `.taking`, `.compacting`, `.beside`,
`.saving` and a compact's scratch `.sorting`, which `fs::Scratch` makes
`O_EXCL` through no link, leaving whatever stands there, and unlinks at
once or on Windows deletes on close, by the open that writes it, a
link there unlinked, never followed; those and `.importing` by a node
under the tenant's lock, `sweep_left`), never one with a `.lock` beside
it, held or not, which no
writer of ours makes there -- a database a user named so -- looked at,
never opened (`fs::remove_left`), each removal and each file so kept said
(`Database::swept_on_open`, `Swept::said`: a server's, a node's and the
router's log, the shell's stderr); `create_dir_private` or
`create_dir_new` for a directory -- `0600`, a directory `0700`, whatever
the umask, from the call that makes it, or for a clone, which takes its
source's owner-only mode, on its handle before it is handed back: never a
`chmod` by name.
A file renamed over another is given that one's mode before the rename, by
an `fchmod` of its open handle (`mode_like`), a group's bits only where it
is of that group, nothing from a file another user owns; nothing is
chowned. `tools/file_create_check.py` (CI's fmt job) refuses
`File::create`, `fs::write`, `.create(..)`, `create_dir_all`, `fs::copy`,
a clone, an `fs::rename` in a function giving no mode and the like, by any
name a `use` gives them, in `crates/*/src` outside tests, fennec-bench and
fennec-fuzz, an exception allowed with why and how many times.
