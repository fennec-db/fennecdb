# Contributing to fennecdb

Thanks for taking an interest. This file is the short version of what a change
has to respect; the docs under `site/content/docs/` carry the long-form
reasoning behind each rule.

## Getting set up

```bash
rustup target add wasm32-unknown-unknown   # rust-toolchain.toml asks for it
make test                                  # the Rust suite, then the JS tests
make wasm                                  # builds web/fennec.wasm
make serve                                 # the browser demo on :8787
```

**The wasm32 target comes from rustup.** Homebrew's `cargo` ships no wasm32 std
library, so the Makefile prefers `~/.cargo/bin/cargo`.

`make test` runs Rust first on purpose: `cargo test` is also what builds the
`fennec-server` binary, and `web/fennec.sync.test.js` runs against it. Both JS suites
skip themselves when that binary or `web/fennec.wasm` is missing, so run
`make wasm` first if you want the end-to-end cases to actually execute.

**On macOS, make your terminal a developer tool.** The system checks every
newly linked program the first time it runs, and a change to `fennec-core`
relinks every test binary: a program as small as hello world waited 15.5 s on
its first run. With a binary for each test file, `make test` spent 48 of its
60 minutes waiting so; with one a crate, 20 of its 25. Add the terminal you
run it from under System Settings → Privacy & Security → Developer Tools
(`sudo spctl developer-mode enable-terminal` shows the pane if it is
missing), and start the terminal again.

**Build with one toolchain.** The Makefile takes rustup's `cargo`, which reads
`rust-toolchain.toml`; a Homebrew `cargo` earlier in `PATH` is another
compiler, and each switch between the two rebuilds everything -- and on macOS
has every binary checked again.

Narrower runs:

```bash
cargo test -p fennec-core --test all persist:: # one file of the integration tests
cargo test -p fennec-ql near                   # by name substring
cargo test -p fennec-core codec::tests         # inline unit tests in a module
node --test web/fennec.test.js
```

`cargo test -p fennec-import --test all pg:: -- --ignored` needs a live PostgreSQL;
`make import-test` starts one in Docker and tears it down afterwards.

## The rules a change must not break

**Zero dependencies.** `fennec-core`, `fennec-ql`, `fennec-wasm`, `fennec-http`,
`fennec-server`, `fennec-wire`, `fennec-import` and `fennec-shard` take no external
crates — own codec, own JSON, own HNSW, own SCRAM client. The WASM output has to stay small and auditable. Two
crates are exceptions: `fennec-bench` (`publish = false`), where `rusqlite` and
`postgres` live, and `fennec-tls`, where rustls, ring and webpki-roots live --
TLS, which nobody should write by hand, taken by the others behind a `tls`
feature and never by `fennec-core`, `fennec-ql`, `fennec-abi` or `fennec-wasm`.
`python3 tools/deps_check.py` (CI) holds both. A PR that adds a dependency to
any other crate, or another to `fennec-tls`, needs to argue the case first, in
an issue.

**Dependency direction.** `core -> ql -> http -> wire -> import -> server`,
`import -> cli`, and
nothing points back up. `fennec-core` does dev-depend on `fennec-ql` so tests can
write real queries; Cargo allows that cycle because a dev dependency never
enters the product build.

**Limits error, they do not truncate.** A silently cut result is a wrong answer
believed right. `near` caps at 10 000 rows and expression depth at 512 levels,
and both return a query error. Keep new limits in that shape.

**The HNSW graph is derived data.** It is written by `snapshot`, `compact` and
`checkpoint` only, never on the write path. On open, the version, dimension,
node count and link bounds are validated and anything off triggers a silent full
rebuild — which is why a corrupt graph can never cost you data. Do not add a
path that makes the graph load-bearing.

**The file format is append-only and self-delimiting.** Every record is
`[kind][collection-id][length][body]`. The length is written even for an empty
body and the reader *must* consume it, or the stray byte is read as the next
record kind.

**Threads are `cfg`'d out of WASM.** The parallel HNSW build path must not reach
wasm32, and `now()` errors there — wasm32-unknown-unknown has no clock, so time
is passed in as a parameter.

**Filtered `near` keeps its fallback.** When the ANN path lands under the limit
it must fall back to scanning the filter set in full. Without it, a filter
correlated with the vector eliminates every candidate and the query returns
empty.

## Style

- Comments explain *why*: a measured cost, a trap that was hit, an alternative
  that was rejected. Not what the line already says.
- All prose in the repo — comments, docs, README — is English.
- `Error` in `fennec-core/src/error.rs` is the single error type: allocation-free
  variants, no `Box`. `fennec-http` maps it onto HTTP statuses.
- Unit tests go inline in `#[cfg(test)] mod tests`. Cross-crate and protocol
  tests go in `crates/*/tests/`, one binary a crate: add a new file's `mod`
  line to that crate's `tests/all.rs`, or it is never built. Measurement
  programs are
  `crates/fennec-core/examples/` and are wired to `make`, not to CI.
- Run `cargo fmt --all` before you push; CI checks it. rustfmt settles
  formatting so that review can be about the change itself.
- CI also runs clippy with warnings denied, natively and for the browser
  build: `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo clippy -p fennec-wasm -p fennec-core -p fennec-ql --target
  wasm32-unknown-unknown -- -D warnings`.

## Performance claims

`make bench`, `make sweep`, `make memory` and `make compare` are the harnesses
behind the numbers on the benchmarks pages (`site/content/docs/benchmarks*.html`). If a change moves any
of them, say by how much in the PR description and on what hardware — the docs
quote measurements, not estimates.

## Pull requests

`main` is protected and takes no direct pushes, including from maintainers:
the required checks run on a pull request, so a push straight to the branch
would land code the gate never saw. Branch, open a PR, let CI go green.

Keep the diff to one concern. Say what you measured, and name the invariant
above that the change touches if it touches one. CI runs `make test` on Linux
and macOS plus a release build; it has to be green.

## Conduct and security

Participation is covered by `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1).

Do not open a public issue for a vulnerability — `SECURITY.md` has the private
reporting form and says what counts as one. Notably, the absence of TLS and the
fact that two processes opening the same file corrupts it are documented
design, not bugs.
