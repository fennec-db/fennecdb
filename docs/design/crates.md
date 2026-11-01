# The crates

The long form of the invariants [CLAUDE.md](../../CLAUDE.md) lists under *The crates*: zero dependencies, limits that error rather than truncate, and the build profiles. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Zero dependencies

**Zero dependencies is a hard rule** for `fennec-core`, `fennec-ql`, `fennec-abi`, `fennec-wasm`, `fennec-ffi`,
`fennec-http`, `fennec-wire`, `fennec-server`, `fennec-import`, `fennec-shard`. The WASM output has to stay small and
auditable; own codec, own JSON, own HNSW, own SCRAM/crypto, own decimal-to-`f64`
and back (`str::parse` drags in a 12 KB table, `{}` on a float 20.8 KB of
Grisu and Dragon -- see `num.rs`). `fennec-core` does
dev-depend on `fennec-ql` (Cargo allows the cycle through a dev dependency) so tests
can write real queries. `fennec-bench` is one exception: a measurement
harness (`publish = false`), where `rusqlite` and `postgres` live.
`fennec-tls` is the other, and the only one a shipped binary takes.

### TLS, the one exception

**What is allowed, and where.** rustls 0.23 with no default features and
`ring`, `std` and `tls12`, and webpki-roots for the default trust store, in
`crates/fennec-tls` alone (rcgen beside them as its dev-dependency, to make
a private CA as a test runs). Every other crate that speaks TLS takes
`fennec-tls`, optional, behind a feature named `tls`: `fennec-wire`
(`tls = ["dep:fennec-tls"]`), `fennec-import`, and the binaries
`fennec-server` and `fennec`, which have it in their defaults. The
workspace names `fennec-wire` and `fennec-import` without their default
features, so a crate gets TLS only by asking, and `cargo build -p X
--no-default-features` builds every crate but the two exceptions with
nothing from outside the workspace, as before. The first slice dials
PostgreSQL (`sslmode`, [replication.md](replication.md#follow)); the
replication client, the OTLP exporter, the router dialing its nodes, and
`fennec-server` and `fennec-shard` serving HTTPS come in by the same crate.

**Why.** The user's decision, on 2026-11-01. Almost no Rust project writes
TLS itself, and a client of our own -- the handshake and its state machine,
X.509 path building and name checks, the AEADs and the key schedule, each
with a history of CVEs in the libraries that did write them -- was judged
too risky for what it guards: SCRAM's exchange, a password's proof and a
table's rows on a network nobody owns. A terminator in front covers a
listener, never a dial, and `--follow` to a managed PostgreSQL (RDS, Cloud
SQL, Azure, Supabase, Neon) has to dial over TLS. rustls rather than
OpenSSL or BoringSSL for a static musl binary with no C library to match;
ring rather than rustls's default provider, aws-lc-rs, whose build wants
cmake and a C toolchain of its own in every job, where ring's C and
assembly build with the `cc` the release jobs already have (musl-tools,
Apple's clang, MSVC, the alpine image's gcc). `tls12` because PostgreSQL
servers and the proxies in front of them still speak 1.2 alone.
webpki-roots, Mozilla's roots compiled in, rather than the system's store:
a static binary in a `scratch` image has no store to read. BoringSSL, as
Bun takes it, is being weighed against rustls; so no type of rustls's
crosses `fennec-tls`'s API -- a `Client` and a `Server` built from DER or
PEM bytes or files, `dial(stream, name)` handing back a `TlsStream` that is
`Read` and `Write`, and the crate's own `Error` -- and the backend can be
changed there without touching a caller.

**What it costs.** Twelve crates (rustls, rustls-webpki, rustls-pki-types,
ring, untrusted, getrandom, libc, cfg-if, once_cell, subtle, zeroize,
webpki-roots), as vulpes counted them, where a release server binary went
from 0.84 to 2.10 MB. Here, measured on 2026-11-01 on macOS arm64 against
main's tree (a KB 1024 bytes): `fennec-server`, release, 4725 -> 5776 KB
(+1051 KB, 22%), its code by crate (`make size-report BIN=fennec-server`)
rustls 299 KB, ring's Rust 63 beside its C and assembly, rustls-webpki 35,
fennec-tls 20, rustls-pki-types 7, the rest ring's tables, Mozilla's roots
and unwinding; `fennec` as a release ships it (`--profile cli`) 2627 ->
3357 KB, with rustls, rustls-webpki and ring built for size in that
profile (at release speed they made it 3533: Cargo.toml's
`[profile.cli.package.*]` says what the 176 KB cost). The image's binary,
static musl: arm64 4.87 -> 5.98 MB, amd64 5.61 -> 7.00 MB. Built without `tls`
the server is 4741 KB -- 16 KB, one Mach-O page, for `sslmode`'s parsing
and what stays of the `SSLRequest` -- and the shell 2627 KB, main's bytes'
size; `make small` (2091 KB), `--no-default-features` (2479 KB) and the
browser module (826 184 bytes, sha256 `cc21e9de...`) come out the same
bytes as main's. A clean release build of `fennec-server` took 46 and
68 s on main and 64 and 59 s here, `--profile cli` 36 and 38 s against 41
and 49, built in turns with other builds loading the machine to 2-5: ring's
C compiles beside the workspace's crates, and the cost is the link's
larger LTO. A million-row `fennec import` (`make pgvector-tls`'s server)
took 2245 ms at the median on main in plain text, 2173 here with
`sslmode=disable` and 2357 under `verify-full`, at the same loads.

**What must never happen.** A dependency in `fennec-core`, `fennec-ql`,
`fennec-abi`, `fennec-wasm`, the browser module or `fennec-ffi`, the
library apps embed -- not `fennec-tls`, not anything -- and another crate
coming in by this door: `fennec-tls` takes rustls and webpki-roots, rcgen
to test, and nothing else, and no other crate takes a crate from outside
the workspace because TLS now does. `tools/deps_check.py`, in CI's fmt
job, fails a change that breaks either: a crate but `fennec-tls` and
`fennec-bench` naming a crate from outside the workspace, of any kind;
`fennec-tls` naming one beyond its three; `fennec-tls` taken other than
optional behind `tls`; the trees of `fennec-core`, `fennec-ql`,
`fennec-abi`, `fennec-wasm` and `fennec-ffi`, every feature on, reaching
outside the workspace or `fennec-tls`; or `fennec-tls`'s own tree, normal
and build edges as Cargo.lock resolves them for the shipped targets,
differing from `TLS_TREE`, its sixteen crates by name and version. Naming
only the two direct dependencies held nothing: a rustls feature turned on
(`aws-lc-rs`, `brotli`) or a `cargo update` brings crates in, or moves
them, without a line of any Cargo.toml changing, so the check fails and
says to review the crate and write the list again. Not `cargo deny`: a
dependency of its own, for the one question asked. A change that would
take another dependency anywhere argues its case first, as this one did.

**Build scripts.** Three crates of that tree run one: ring's compiles its
C and assembly, libc's and rustls's probe the compiler.
`tools/image-cargo.sh`, which builds the images' binaries and holds each
to the bytes of the build context, refuses a workspace crate's build
script (what it reads cargo tracks by mtime alone) and takes a crates.io
one only by name and version -- `ring@0.17.14`, `rustls@0.23.46`,
`libc@0.2.189` -- each read first and found to read only its own files,
which Cargo.lock's checksum fixes, and the environment, which cargo tracks
by value. Taking every registry crate's, as the first cut did, would run
at the build whatever a new dependency or version brought, read by no one.
Bumping one means reading its `build.rs` at the new version and writing
the version there; `deploy/test/image_cargo.py` (`make deploy-check`)
holds libc's taken at its version and refused at another.

## Limits

**Limits error, they do not truncate.** `near`, `match` and `fuse` results
cap at 10 000 rows (`limit + offset`), expression depth at 512 levels, a
`lookup` chain at 8, an `in (get ...)` at 100 000 values and 4 levels and
an `in $n` parameter's list at the inner `get`'s 100 000 values
(`query::param_list`); all of them return a query error, because a
silently cut result is a wrong answer believed right. Full table in `site/content/docs/limits.html`.

## Profiles

**Profiles differ on purpose.** `fennec-cli` uses the `cli` profile (`panic =
abort`, single process, nothing to recover). `fennec-server` stays on `release`: a
panicking connection thread unwinds and drops only its own request. A cold
crate is built for size in the profile it is cold in: in `cli`, `fennec-http`, which the shell reaches only for
`backup`, `archive` and `restore`, waiting on the network and the disk,
and for `fennec mcp`, waiting on a model (16.3 KB of the released binary,
measured before `mcp`). Not the importer, whose SQLite import of
200 000 rows took 285 ms against 251 for 32.6 KB, nor the root crate, which
built for size made the binary 129 KB larger (`make size-report BIN=fennec`).
Both binaries hold 72 KB of the standard library's backtrace symbolizer
(`gimli`, `addr2line`, `rustc_demangle`, `object`), which only a nightly
`build-std` leaves out.
