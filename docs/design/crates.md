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
(`tls = ["dep:fennec-tls"]`), `fennec-import`, `fennec-http` (its
listeners' HTTPS, [http.md](http.md#tls)), and the binaries
`fennec-server`, `fennec-shard` and `fennec`, which have it in their
defaults. The
workspace names `fennec-wire` and `fennec-import` without their default
features, so a crate gets TLS only by asking, and `cargo build -p X
--no-default-features` builds every crate but the two exceptions with
nothing from outside the workspace, as before. The first slice dials
PostgreSQL (`sslmode`, [replication.md](replication.md#follow)); the
second has `fennec-server` and `fennec-shard` serve HTTPS
(`--tls-cert`, [http.md](http.md#tls)); the third has every HTTP dial
take an `https://` URL -- a replica's, the router's to its nodes and
regions, the OTLP exporter's, `fennec backup` and `mcp --url`
([http.md](http.md#dialing-https)) -- by the same crate.

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
Serving HTTPS, the second slice ([http.md](http.md#tls)), measured on
2026-11-02 the same way: `fennec-server` 5777 -> 5922 KB (rustls's server
half and the listener's code), without `tls` 4741 -> 4758; `fennec-shard`,
which took no TLS before, 4256 -> 5113 KB; the image's binary arm64 6.11
MB, amd64 7.18. Dialing HTTPS, the third
([http.md](http.md#dialing-https)): `fennec-shard` 5 251 968 -> 5 550 256
bytes (rustls's client half, the verifier and Mozilla's roots, its first
dial), `fennec-server` and `fennec` one 16 KB page each.

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

**Limits error, they do not truncate.** `near`, `match` and `fuse` rankings
cap at 10 000 rows (`limit + offset`), expression depth at 512 levels, a
`lookup` chain at 8, an `in (get ...)` at 100 000 values and 4 levels and
an `in $n` parameter's list at the inner `get`'s 100 000 values
(`query::param_list`); all of them return a query error, because a
silently cut result is a wrong answer believed right. A `match` beside
`order`, `count` or an aggregate ranks nothing and keeps no ceiling of its
own: a count answers no rows, and a page put in order is held to what an
ordered `get` is ([text.md](text.md#match-as-a-filter)). A value past
its type's range is refused the same way: a whole float going into an
`int` at or past 2^63 either side -- `1e300`, a JSON integer past 64 bits
(read as the float it rounds to, `-9223372036854775809` exactly -2^63), a
`set`'s float result -- is a type error naming the field (`Value::coerce`,
`engine::in_field`), where `as` saturated it to `i64::MAX` and an `@hash`
lookup then found a row the scan does not, while a key looked up by the
float -2^63 is `i64::MIN` and by 2^63 none, as `=` finds them
(`Value::key_as`, [indexes.md](indexes.md#sorted)). A list's elements
too: an `[int]`'s, a `[float]`'s and a `[timestamp]`'s are read as written
and each held to its type as a scalar is, refused naming the field and the
element counted from 0 -- "field `xs`, element 2: an int is from ..." --
where they were the `f32`s of a vector, `[16777217]` stored as 16777216
and 2^63 as `i64::MAX` ([vector-search.md](vector-search.md#numbers-in)).
Full table in `site/content/docs/limits.html`.

A number a statement writes is read as written or refused naming its
range, never fitted to it: `Parser::int_in(what, lo, hi)` reads every
dimension, beam and count of candidates or words, `hi` a `u32` so the
number fits every target's `usize`, refused "`k` is 0 to 1000000, not
99999999999" at the position of the clause it is in. Before it, `as usize`
kept a wasm32 build's low 32 bits -- `fuse candidates 4294967297` was 1 in
the browser module, past the 10 000 a native build refuses,
`snippet(body, 4294967297)` a snippet of one word and `vector<4294967297>`
a `vector<1>` that took `[0.5]` -- and clamps made `fuse k 99999999999`
4294967295, `@hnsw(m=1)` 2, `ef_construction=4` 8 and `ef 0` 1. The
ranges: a `vector<N>` and a `sparse<N>` 1 to `MAX_DIM` (10^9: pgvector's
`sparsevec`, and a round number whose `f32`s still fit a document's 4
GiB); `@hnsw`'s `m` 2 to `MAX_M` (32 767: level 0 keeps `2m` links,
counted in a `u16`, which `m` past it cut), `ef_search` and `near`'s `ef`
from 1 -- the floors the clamps lifted to, refused under them -- and
`ef_construction` from 4, pgvector's floor, each at most `u32::MAX` (a
graph's node ids are `u32`s); `fuse k` 0 to `MAX_FUSE_K`
([text.md](text.md#fuse)); `fuse` and `rerank` `candidates` 1 to 10 000,
refused as they are read; a snippet's words 1 to `u32::MAX`. `fuse k` is
held in `Select::check` too, the one place every `Select` meets -- the
parser's, `/near`'s `fuse_k` and one built in Rust (`Fuse::k` a `usize`,
so `/near` hands it as written and is refused naming it) -- and `/near`
keeps no copy; natively, as in the browser module every `Select` is the
parser's (asked there too, 98 bytes of it). A declared schema is held to
the same ranges (`declared::index`, and `declared::type_named`, whose
`dimension` reads the digits in a `u32`, checked at each: folded in a
32-bit build's `usize` -- the native library's on a 32-bit device --
`vector<4294967297>` wrapped to 1). What no parser reads -- SQL's
`vector(n)`, `fennec import --vector`, a schema built in code -- meets
`IndexKind::check`'s `MAX_DIM`, and natively its `MAX_M`: in the browser
module every spec comes through the parser or SQL's `with (m = ..)`, which
holds `m` to pgvector's 2 to 100 and `ef_construction` to 4 to 1 000. A
dimension or a parameter a file's schema holds is read whole or the file
refused (`codec::get_usize`, through `get_within` at the target's width,
which its test runs at a wasm32 build's); a graph record holds the same
numbers and reads them as it did (checked there too, 88 bytes of the
module). A beam wider than the walk found reserves no more than it found
(`VectorIndex::search`, `nearest_docs`): `ef 4294967295` asked for 64 GiB
for its answer, which macOS hands over unbacked and a wasm32 build cannot
count (`a_beam_past_the_graph_reserves_only_what_it_found`, by the
answer's capacity); `past_tombstones` widens such a beam by its
tombstones saturating (`wider`). The counts a parameter may hold
(`limit`, `offset`, `top`) saturate at `usize::MAX` instead
(`query::as_count`): no more rows than memory holds can be asked for, so
the answer is the one written. These are read once a parse, where a
clause is written, so parsing is as it was.

*One floor for `ef_construction`.* The parser's first floor was 8, the
clamp's, so that no FennecQL text read as another tree; but SQL's `create
index ... using hnsw ... with (m = 2, ef_construction = 4)` takes
pgvector's 4, and the `@hnsw(l2, m=2, ef_construction=4, ..)` its schema
is described as was refused by every reader of the description --
`fennec types --fennecql` read back, `fennec_abi::schema` and `follow`
over `describe`'s JSON and FennecQL, `GET /_schema` handed to
`/_schema/plan`. One floor, 4, for FennecQL, a declaration and SQL
(`pgvectors_floor_reads_back_as_it_was_made`, and the module's test). A
FennecQL `ef_construction=4` to `7`, built as 8 before, now builds what it
says: the one tree that moves (`make fennecql-corpus`), DDL, which no named
statement's definition holds.

*A collection a version before made out of range.* A file holds its
schema as written; an open reads it without `Schema::new`, so a
collection main made with an `@hnsw` `m` past 32 767 -- a graph whose
nodes' counts of links were already cut to 16 bits -- or a `vector<N>`
past 10^9 opens, reads and writes as it did. Every `alter collection`
builds the schema anew, asking each field, so `add field x int` and
`rename field` were refused naming `e`, a field neither touches. Refusing
is right -- the collection holds what a `create` refuses, and letting an
alter through where the field is untouched would keep it -- so the
refusal says so (`held_over`): "`b` holds a field a version before this
one made, which this one no longer takes (`e`: @hnsw's `m` is 2 to
32767): drop it (`alter collection b drop field e`) and add it again
within the range, or copy the rows into a collection made so". A field's
own index goes only with the field (`drop index` takes one over several
fields alone), and a drop asks nothing of the other fields; a field with
an `m` past the bound was answering from a cut graph, and a `vector<N>`
past 10^9 holds no vector past 2^30 dimensions, a document's 4 GiB, so its
vectors are put again from where they came, or the rows copied
(`a_field_made_out_of_range_before_is_named_at_any_alter`). Natively: the
browser module asks no `m`, and a dimension past 10^9, of which no page
holds a vector, is refused there in `Schema::new`'s words, which name the
field and its range -- the message was 0.7 KB of the module.

*Grammar epoch 3.* A named statement is stored as its text and read again
at every open ([named-statements.md](named-statements.md#what-is-built),
*Grammar epoch 3*):
a definition main stored holding a number these ranges refuse -- `fuse k
2000000` or `99999999999`, `near ... ef 0` (main read 1) and natively `ef
5000000000`, `rerank ... candidates 0`, a parameter's `vector<N>` past
10^9 -- no longer reads, and is marked broken, told to define it again; a
bound one refuses every write that would wake it. So `GRAMMAR_EPOCH` is 3
(`a_number_epoch_3_refuses_marks_a_stored_definition_broken`).

One helper and one message: the browser module grew 431 bytes, 373 gzip
and 388 brotli (`make size-report BASE=origin/main`) -- `int_in` 283,
`codec::get_usize` 155, `hnsw_args` 131 and 24 of strings, less
`data_type`'s two refusals of its own (110) and `IndexKind::check`'s one
message for both kinds of vector (29); `altered`, one copy of
`Schema::new`'s call for `add` and `rename`, is 160 bytes against the 155
it took out of `execute_inner`. The review took 103 bytes off the branch
as it was: the file's refusal in the varint's own words, one "a vector's
dimension" in the parser, plain `` `k` `` and `` `candidates` `` for `fuse`
and `rerank`, `fuse k` in `Select::check` and the alter's refusal natively
alone (98 bytes and 0.7 KB of the module). Reading `@hnsw(..)`'s number
once before its key was 33 bytes more, a function of its own for the
range's test, and is not done.

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
