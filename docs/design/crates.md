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
differing from `TLS_TREE`, its sixteen crates by name and version, or a target's own tree from that and `TLS_EXTRA`'s: ring's `windows-sys` crates are allowed in `aarch64-pc-windows-msvc`'s tree alone. Naming
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
`vector(n)` and its arrays (`vector(n)[]`, `[3]`, `array`), `fennec import
--vector` and `--cast`, a schema built in code -- meets `IndexKind::check`,
which holds a dimension at any depth of lists and an `@hnsw`'s `m` by the
test a file's schema is read with (`schema::out_of_range`, *A file's schema
is held to the same ranges*, below), in the browser module too; and,
natively, a beam built in code to `u32::MAX`, past which a 32-bit reader of
the file -- a page replicating it, the native library on a 32-bit device --
refuses the schema (`codec::get_usize`). SQL's `with (m = ..)` holds `m` to
pgvector's 2 to 100 and `ef_construction` to 4 to 1 000. A
dimension or a parameter a file's schema holds is read whole or the file
refused (`codec::get_usize`, through `get_within` at the target's width,
which its test runs at a wasm32 build's), and a dimension and an `m` within
these ranges, or the file refused as damage (*A file's schema is held to
the same ranges*, below); a graph record holds the same numbers and reads
them as it did (checked there too, 88 bytes of the module), its `m` held to
the field's range before it sizes a node's links. A beam wider than the walk found reserves no more than it found
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

*A file's schema is held to the same ranges.* A file holds its schema as
written, and an open reads it without `Schema::new`; so does a replica
applying its primary's records, and a page's replica in the browser
module its server's. Read as written, the numbers were a crafted file's to
choose: 77 bytes declaring `create collection w (v vector<2> @hnsw)` with
an `m` of 2^40 had the open's index keep `2m` links a node (`m0`, then
`l0`'s growth and `Read::new`'s count of nodes times `m0`) and the process
was killed at `Database::load`; a `vector<N>` past `MAX_DIM` reserved N
floats for each vector an open read (`fill_in_turn`). So `Schema::decode`
holds each field's dimension, wherever its type holds one (`[vector<N>]`
too), to 1 to `MAX_DIM`, and its `@hnsw`'s `m` to 2 to `MAX_M`, the
ranges `Parser::int_in` reads them in, and refuses the record as damage
naming the field and the range (`schema::out_of_range`: "`v`: @hnsw's
`m` is 2 to 32767, not 1099511627776"), in the browser module too. The
test is the one `IndexKind::check` asks of every schema a write makes, so
no open refuses what a write took. With a test each, the check held a
field's own vector alone: SQL's `create table t (e vector(1000000001)[])`
and `Schema::new`'s `[vector<0>]` were taken and written, and at the next
open the file was refused as damage, by every replica and every page
replicating it -- a server's file one DDL statement from not opening at its
restart (the review's finding). `whatever_a_write_takes_an_open_reads_back`
makes a schema at each edge of the ranges, a field's own vector and a
list's at every depth, every way one is made -- `Schema::new`, FennecQL's
`create` and `alter`, SQL's arrays, a declaration -- and reads each one
taken back from the image as it was made; in code the write and the read
take exactly the same fields. One test for both took 79 bytes off the
browser module (855 790 to 855 711; gzip 78, brotli 113): the check's own
refusal of a dimension went, and `m` is asked there for nothing. A graph
record carries its own `m`, which sized its node's links before the open
held its spec to the field's: it is held to the same range, the graph
rebuilt from the documents otherwise. `tests/damage.rs` makes such files
by hand -- `m` of 2^40, 32 768 and 1, a dimension of 2^40, 10^9 + 1 and 0,
a sparse one and one inside a list -- each refused read into memory,
mapped and applied by a replica, and the first killing the test binary
with the bound taken out.

That takes back a promise. A collection main made with an `m` past
32 767 -- a graph whose nodes' counts of links were already cut to 16 bits
-- or a `vector<N>` past 10^9 opened, read and wrote as it did, and every
`alter collection` of it was refused saying so and the way out
(`held_over`: drop the field and add it again within the range). Such a
file is now refused at the open as any other file holding those numbers:
told apart from a crafted one it cannot be, the check being the bytes', and
fennecdb is alpha, with no file of a user's to keep
(`a_field_made_out_of_range_before_is_refused_at_the_open`). So is a list
of vectors past the range that a version before made -- v0.2.0's check
held none below the list, and took SQL's `vector(1000000001)[]`, `fennec
import --cast e=[vector<1000000001>]` and `Schema::new`'s `[vector<0>]`.
An alter refuses only the field it changes: every field it holds was read
within the ranges.

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

### How deep a value nests

**A value read off the bytes nests no deeper than JSON's limit.** A
document's field in a file, a primary's record a replica applies, a page's
image in the browser module: each value of it nests at most
`codec::MAX_DEPTH` lists and objects, `value::MAX_JSON_DEPTH`'s 64, the
most any write makes. A json field's value is held to it as it is written
(`json_value`), a typed list's by its type, which `Schema::new` holds to
the same 64 (`DataType::depth`: a list a level, a point -- `[lon, lat]`
once read -- one, a json value all 64, so a list of json is refused at any
depth, `[[json]]` as `[json]` was) and every reader of a type counts as it
reads, recursing into none: the parser (`Parser::data_type`, "a list type
nests at most 64 levels"), a declared schema's text (`declared::type_named`)
and a file's schema (`codec::decode_type`, refused past it as damage).
Past it the bytes are damage or a file made to hurt: `decode_value`, the
skip and a path's read refuse them, `Error::Corrupt` naming the limit --
"a value nests more than 64 levels deep" -- where the decoder recursed a
level at a time and a record of 100 000 nested lists, 200 KB, overflowed
the stack, which aborts the process: a server on a read of its file, a
replica on its primary's record, the browser module on a page's image
(`tests/damage.rs`: the open, the read and the apply, each refused with
the stack whole, and each aborting the test binary with the bound taken
out). An open reads no document ([indexes.md](indexes.md#lazy-indexes)),
so such a record is refused where something reads it or past it: a read,
an index's build, a vector index linked at the open -- which reads each
document's vector past the fields before it, and refuses the open.

*What it refuses that a version before opened.* Two kinds of file, both
pathological. A field declared `[[json]]`, which a version before took
(`[json]` alone was refused), holds a json value of up to 64 levels inside
its two lists, 66 in all: a row whose value nests past 64 so is refused
where it is read. And a field whose type nests more than 64 lists, which
no version bounded: its schema's type is read past the limit
(`decode_type`), and the file is refused at the open. No write makes
either now (`Schema::new`), and no test file of the repository's holds one.

A length counting items of several bytes -- a vector's `f32`s, a
half-precision vector's halves, a sparse vector's weights -- is multiplied
saturating, and `take` refuses what passes `usize` as longer than the
bytes ("length overflow"): `n * 4` wrapped in a release build, so a vector
declaring 2^62 `f32`s took none of the bytes after it and read as empty,
and a half-precision one declaring 2^63 asked for its whole `Vec` before a
byte was looked for (capacity overflow, a panic). A function of its own
returning the refusal, checked, was 102 bytes of the module and a branch
at each of six sites, for what `take` already refuses. A
bool the bytes end before is refused by the skip as by the decoder:
stepped over unseen, it left the position past the end, and a compact's
copy of the bytes it skipped (`Schema::without_dropped`) sliced past them.

*What the bound costs.* Tested in the arms of the decoder's match, as the
first version of this change tested it, the depth was an argument live
across every value's read, and every value paid about two instructions,
not the list or object alone -- "some 11 to 14 instructions a list or
object", as that version read its trace, was the whole value's cost laid
on its three containers. So the decoder tests it where a list or an
object is read, functions of their own (`decode_list`, `decode_object`,
`#[inline(never)]`), `decode_at` handing it on and no more. Measured as
the retired instructions of a probe built release with LTO
(`/usr/bin/time -l`, 5 million loops, the start-up's taken off, three
runs each within half an instruction) at the base, at the first version
and here: a row of eight scalars decodes in 1 763.3, 1 779.1 and 1 763.3
instructions, and an object holding a list of three floats, a text, an
object of two and a 4-dimensional vector in 5 352.3, 5 394.5 and 5 359.0,
about two a container now, the calls they are. The skip keeps the depth
in its match: its eight skips of the row take 556.0 instructions at the
base and 570.0 so, the object's 906.0 and 934.0 (+2.5% and +3.1%). The
browser module grew by 1 977 bytes, 826 gzip, with every refusal the
fuzzer and its review brought ([conventions.md](conventions.md#fuzzing);
`make size-report BASE=f104d32e`): the codec 501 -- `decode_list` and
`decode_object` a function each, 97 more than the arms they were -- the
schema 632 (the ranges' refusal 274, written out rather than formatted,
where an `i32` literal had brought `i32`'s formatting, 307 bytes), the
store's two refusals 340 and the graph's 86.

### A SQLite file's lengths

**A SQLite file's lengths are held to the file before anything is
allocated, and its walk reaches a page once.** `fennec import`'s reader
(`crates/fennec-import/src/sqlite.rs`) reads a file it did not write: a
cell's payload length is a varint of the file's, and `Vec::with_capacity`
of one declaring 2^62 bytes ended the import -- capacity overflow, or the
allocator's abort -- before the length met the file; it is held to the
file's length now (`Pager::payload`), as a wire message's is held to
`proto::MAX_MESSAGE` (`proto::zeroed`). The b-tree walk keeps a bit a page
of the file (`Walk::visit`): an interior page naming itself or a page
above it as a child walked on for ever, its stack growing by the page's
cells each time round, and is refused at the page reached twice; a child
past the file's pages is refused before it is read. A page's usable part
is held to SQLite's own floor of 480 bytes (`MIN_USABLE`), which the
overflow arithmetic (`local_len`) stands on. And a column's type is read a
character at a time (`take_type`): a space of two bytes, U+00A0, read a
byte at a time began a word that ended at once, and the reading never
moved on. Each is a test of `tests/sqlite.rs` on a file made by hand, and
the fuzzer's `sqlite` target runs the reader over every seed's mutations
([conventions.md](conventions.md#fuzzing)).

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
