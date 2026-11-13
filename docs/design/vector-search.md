# Vector search

The long form of the invariants [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) lists under *Vector search*: the distance kernels, vectors as numbers, quantized codes, filtered `near` and sparse vectors. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## SIMD

**The wasm32 build has SIMD, and its kernels match the scalar ones bit for
bit.** `.cargo/config.toml` turns on `simd128` for that target, and
`vector::simd` holds the distance kernels written against it: the module is
built at `opt-level = "z"`, where LLVM does not vectorise the scalar strips
(a 20 000 x 384 HNSW build went 28.0 -> 9.95 s). They keep the scalar loop's
eight accumulators and reduction order, so a graph built in the browser is the
graph built natively; `web/fennec.test.js` checks that order against a
`Math.fround` reference. The same config links the module with
`--compress-relocations`: wasm-ld wrote each call's function index and each
address the code takes as a padded five-byte LEB, 36 KB of the module and
2.2 KB brotli, and V8, which tiers a function up by its size in bytes, runs
the smaller one faster -- `near` 12%, a page of vectors as JSON 15% (`make
wasm-speed`). A build that keeps the names strips the debug information
the flag refuses (`strip = "debuginfo"`, as `make size-report` does).

## NEON

**aarch64's strips are written out as well, to the same bits.**
`vector::neon` holds every distance strip on aarch64 -- f32, f16 and int8
codes -- over one `strips`, eight accumulators as two four-lane registers,
each lane doing the scalar loop's multiply and add (no fused multiply-add)
and the lanes summed in `strip8!`'s order. Left to the vectoriser, the f32
and f16 strips read through four-way de-interleaving loads into two-lane
registers, half of NEON's width: a 128-dim dot product took 21 ns in cache
against 9.4, and 91 against 60 out of an arena of 100 000; the int8 ones
chained every strip to the one before. f16 is widened with `half`'s own
masks, shifts and multiply, not NEON's conversion, which would turn
infinities and NaNs into what `half` does not.
`f32_and_f16_kernels_match_the_scalar_strips` holds them to the scalar
strips bit for bit. A 100 000 x 128 build went 5.2 -> 4.3 s (f16 6.0 ->
4.5), and `near` at 768 dimensions over f16 0.54 -> 0.34 ms p50. x86_64's
vectoriser keeps four-lane SSE registers and is left as it is.
A walk measures a node's unvisited neighbours four at a time there
(`distances4`, `Arena::dists_to`) and takes them in the order it did: read
together, four vectors wait on memory once -- out of an arena of 100 000 a
128-dim distance took 60 ns alone and 47 four at a time, a 768-dim one 242
and 149 -- so `near` at 768 dimensions went 0.35 -> 0.29 ms p50 over f16
and 0.35 -> 0.28 over f32, a build 3-10% faster, and
`the_walk_scores_as_the_exact_search_does` holds the walk's scores to the
exact search's bit for bit. The browser's module built a graph 4% slower
four at once and grew 9 KB, and gathering the neighbours before measuring
them one at a time still cost its `near` 7%, so everywhere but aarch64 the
walk is as it was (`cfg`). There the walk also asks for the next
candidate's list and the fresh neighbours' vectors ahead of reading them
(`prefetch`, a `prfm` hint): each read was a cache miss waited on after
the one before, and a 100 000 x 128 build went 3.96-4.25 -> 3.58-3.80 s,
`near` 0.10 -> 0.08 ms p50. The diversity heuristic measures a candidate
against four chosen neighbours at once (`any_nearer`), 5 to 9% off a build
at 768 dimensions. Neither changes a bit of the graph.

## Vectors out

**A vector goes out as its `f32`s.** JSON writes a vector's components
and a row's `_score` as the shortest text that reads back as the `f32`
(`json::num32_into`), as pgvector and a sparse vector's weights
do: `0.1`, where the `f64` each widens to wrote
`0.10000000149011612`. JavaScript reads it as an `f64` and a
`Float32Array` rounds that again, which gives every `f32` back but
`7.038531e-26`: as an `f64` it lands exactly between itself and the `f32`
above, and the tie goes to the even one, so it keeps its `f64`'s text
(`json::TIE`; `every_f32_reads_back_through_an_f64` tries all 2^32). The
digits of an `f32` from 2^-24 to 2^25 are exact 64-bit products
(`num::shortest`), where Ryu's multipliers took 128-bit ones that wasm
emulates (`__multi3`), and they go out in one `push_str` rather than a
`char` at a time: a page of 200 768-dim vectors as JSON, 43% shorter, went
27.7 -> 10.5 ms in the browser module, and a `near` answering ten 128-dim
rows 0.373 -> 0.226 ms (`make wasm-speed`), for 37 bytes brotli.

## Numbers in

**A number is read four ways, each exact, or refused past an `f64`'s
range.** `num::read_f64` takes Clinger's path where the digits fit in 53
bits, divides a whole number of up to 19 digits by `5^n` for `10^-1` to
`10^-25` (`divided`; `div128` divides in two 32-bit digits, since a `u128`
division took the compiler's own, 1.4 KB, into the browser module),
multiplies any other number's first 19 digits by its power of five
(`near`), and compares every digit with a midpoint where that cannot tell
(`exact`), each below. JavaScript writes most floats, every `f32` it widens among
them, with seventeen digits, past Clinger's 53 bits. A page of 200
768-dim vectors took 74.6 ms to read in the browser module; divided, 51.9;
with the JSON reader walking the text's bytes rather than collecting it
into `char`s first, 23.7; read where the page left it rather than copied
(`str_from`), 21.9. 10 000 128-dim vectors went in with their graph in
1 281 ms against 1 706 (`make wasm-speed`), for 368 bytes brotli.
Natively an array of numbers alone is read straight into a vector's `f32`s,
each number in the one pass that finds its end where Clinger's path takes
it (`num::clinger`) and handed to the readers above otherwise: read a
`Value` at a time, each number's text found and then read twice over, a
128-dim vector took 5.26 us, now 2.05, and 100 000 128-dim rows without an
index go in at 220k rows/s over HTTP against 123k (`make load-bench`;
213k as REST arrays, 146k as a `/batch` of single puts now). A page's
vectors come into the browser module as `f32`s already, and the reader
stays out of it: there it was 474 bytes brotli for nothing.

*Multiplied, never shifted.* Every number the first two paths did not
take -- an exponent past 10^±22 or 10^-25, an integer past 2^53, more than
19 digits -- was a decimal string shifted 27 bits a pass until it sat in
`[1/2, 1)`, each pass over every digit and two 800-byte copies: `1e255`
took 137 000 instructions where `1e25` took 211, so a 64 MiB body of
`1e255,` was minutes of a core (found by the fuzzer, #624). Now `near`
multiplies the first 19 digits by the power of five its exponent says, to
125 bits -- Ryu's multipliers, which `f64_into` writes with and the module
holds already (`pow5`, `inv_pow5`, the inverse's table grown from 5^-312 to
5^-364 for the smallest subnormal's 5^-342) -- keeping every bit of the
product, so the number lies in a span of `w` in its last place, `w + t`
where digits were cut: where both ends round to one `f64`, that is the
number's. It is Eisel and Lemire's algorithm on another table: theirs is
the 12 KB of 128-bit powers the module went without, and its proof that a
19-digit number never needs the fallback (Mushtak and Lemire, 2023) is for
that table's precision, not 125 bits; the span needs no proof, it says
itself whether it crosses a midpoint. One within 2^-59 of a midpoint is
left to `exact`, which makes its digits (800 at most, a midpoint having
768) one integer and the midpoint `(2m + 1) × 2^(e - 1)` another, brings
both to one power of ten and of two, and compares them, the digits past
800 settling a tie: some thousands of instructions where the shifting took
up to 183 000, in 32-bit words so the module multiplies them itself.
Crafted inputs reach it -- any midpoint's first 25 digits -- and cost
about what an ordinary number does a byte. Instructions a number, a loop
of 20 000 reads in release (`proc_pid_rusage`), each build's own reader:

| number | main | now |
|---|---:|---:|
| `0.1`, Clinger's | 147 | 142 |
| `123.456` | 231 | 224 |
| `-0.30000000000000004`, JavaScript's 17 digits (the quotient) | 569 | 568 |
| `1.2345678901234567e-7` | 606 | 595 |
| `1.2345678901234567e+21` | 9 142 | 704 |
| `1e25` | 211 | 195 |
| `1e255` | 137 391 | 461 |
| `1e-255` | 76 283 | 465 |
| 17 digits, `e200` | 96 664 | 794 |
| `1.7976931348623157e308` | 182 943 | 810 |
| `4.9406564584124654e-324` | 126 540 | 707 |
| `9007199254740993` | 7 371 | 489 |
| 40 digits, `0.1234...` | 5 667 | 1 068 |
| 40 digits, `e200` | 129 446 | 1 040 |
| 800 random digits | 54 414 | 11 728 |
| the midpoint above 1, 54 digits (`exact`) | 6 918 | 3 170 |
| 25 digits under 1e300's midpoint (`exact`) | 177 401 | 3 949 |
| 25 digits under a subnormal midpoint (`exact`) | 135 684 | 4 398 |
| `1e400` | 522 (an infinity) | 195 (refused) |
| `1e-400` | 166 228 (0) | 197 (refused) |

JSON bodies of 8 000 numbers read by `json::parse`, instructions a number:
`1e255`s 138 340 before and 684 now, documents `{"x":1e255}` 140 482 and
2 067; `1e25`s 419 and 400, and documents of `0.1`, `123.456` and
`-0.30000000000000004` 1 735, 1 850 and 2 280 against 1 736, 1 848 and
2 281 -- medians of 18 runs over six identities of each build
(`tools/instr/setuuid.py`), whose spreads overlap. Every bit is main's:
the reader before, kept beside the new one in a program of its own, read
14 835 394 generated numbers alike -- up to 19 digits over every exponent
and past, every double's shortest texts, `f32`s widened, subnormals,
midpoints written whole with a hair either side and cut to 17 to 40
digits, and 20 to 920 digits -- with the numbers of the repository's JSON
(3 409), of its FennecQL corpus (9 506) and every number-shaped token of
its sources (177 713); where it read an infinity or 0 of a number not 0
the new one refuses (735 124 of them), and `str::parse` read every one
alike too. The tests hold the reader to `str::parse` as they did, each
midpoint's tie and a hair either side (`exact` reached in more than 600),
the digits past 800 breaking a tie, and no number of 19 digits or fewer
reaching `exact` over every exponent. Rejected: `str::parse` natively and
this reader in the module (the module reads what a page fetches too, and
the table is what this file is here to keep out); the 128-bit table
(12 KB); the shifting kept for what `near` leaves (a body of crafted
25-digit numbers was still 177 000 instructions each); 64-bit words in
`Big` (each product a call of the module's 128-bit multiply); `near` and
`exact` inlined into the reader, whose digit loops then spilled their
registers (up to 20 instructions more a number Clinger's path takes); a
closure reading a JSON number either way, a call of its own handing its
`Result<Value>` back through memory (20 instructions a document's number,
over six identities of each build), where the refusals are cold functions
apart (`json::unread`, `lexer::unread`); and `read_f64` itself out of
line, whose `Result<f64, Unread>` came back through memory where main's
`Option<f64>` came in two registers (5 more a document's number): the
reading hands back a pair (`num::read`), its two wrappers inlined.

*Past the range, refused.* A number past an `f64`'s range read as one
anyway: `1e400` an infinity, which JSON writes as `null`, so a document
written with it was read back with `null` where a number was, and `1e-400`
0 -- another number than the one written, which a limit refuses rather
than cuts ([crates.md](crates.md#limits)). Every reader refuses one now,
naming it, where it was and the range (`num::Unread`): JSON's "number
`1e400` at byte 7 is past a float's range: a float is at most
1.7976931348623157e308 either side of 0" (a body's, a parameter's, a
document's, a json field's, a vector's), FennecQL's at its position, the
query string's naming the field, a sparse vector's weight, pgvector's text
of a vector, and an import's naming the row and field or the column -- a
`numeric` holds 1e400, and a
SQLite text may -- the infinities and `NaN` PostgreSQL and SQLite spell as
words kept as before. A number written past 40 characters is named by its
first 40 and its length. Rounding to the nearest decides the ends: past
the largest `f64` but under the midpoint above it (`1.7976931348623158e308`)
reads as the largest, and anything from just past half the smallest
subnormal (`2.4703282292062328e-324`, `3e-324`) as it, while that half
itself is a tie to the even 0 and refused; 0 written any way is 0, its
sign kept. Refusing a nonzero number rather than reading 0 is
PostgreSQL's `float8in` (both ways "out of range for type double
precision"), where JavaScript's and Python's readers give an infinity or
0. An exponent is
read whole to 2^40, where it was cut at 100 000: `0.` and a million zeros
then `1e1000001` -- 1 -- read as an infinity. A float a file holds is its
`f64`: an infinity an earlier build read from `1e400`, or one an import or
a Rust caller put, opens, reads and is found as it did, JSON writing it
`null` (`a_float_past_its_range_is_refused_naming_it`). A named
statement's stored text alone reads one as it did, an infinity or 0, to
the digest main gave it ([named-statements.md](named-statements.md#what-is-built),
*A float past its range*). A vector's component is still the `f32` of the
number read, and a number within an `f64`'s range past an `f32`'s is that
`f32`'s infinity or 0, as a `vector<N, f16>`'s past ±65 504 is its own
(`site/content/docs/limits.html`). The module grew 2 030 bytes, 963 of
them brotli's (`make size-report`): the reader 1 923 where the one that
shifted was 2 982, `exact` 757, `near` 536 and `rounded` 308, the
multipliers out of line 688 where `float_into` gave back 584, the readers'
words some 830 (FennecQL's, pgvector's text, a sparse weight, JSON's) and
360 of data.

**A list of numbers is read as written.** An `[int]`, a `[float]` and a
`[timestamp]` keep each element as written, or refuse the statement naming
the field, the element counted from 0 and the range -- "field `xs`, element
2: an int is from -9223372036854775808 to 9223372036854775807" -- as a
scalar is ([crates.md](crates.md#limits)), never clamped or rounded. Before,
such a list was read the quick way above and made its elements from the
`f32`s: a `[float]` kept 0.10000000149011612 for `0.1`, an `[int]` 16777216
for 16777217, 9007199254740992 for 9007199254740993, 1 for `1.00000001`,
and took 2^63 -- every number from 2^63 - 2^38 to 2^63 + 2^39 as an `f32`
-- as `i64::MAX`. A `[timestamp]` given numbers refused them, and `xs =
[1, 2]` found no row, a vector never equal to the list stored. A list of
numbers is one of the types that keep numbers as written now
(`DataType::keeps_numbers`, `holds_numbers`), as a json field and a point
are: `Database::exactly` names the text, or the parameters by place, where
one is handed a list read the quick way, and the reader reads them again as
written -- the server from the body (`api::exactly`, the named parameters
alone, `json::exact_params` stepping over the rest), the native library and
the browser module from the parameters' JSON (`exact_noting`, a list handed
over apart as `f32`s asked for as JSON, as a json field's is), the text
through `fennec_ql::parse_exact`. A REST body's and a replica's documents
name the field, and read it as written the first time. A row an earlier
build wrote keeps the numbers it stored -- 0.10000000149011612 for `0.1`,
16777216 for 16777217 -- and `where fs = [0.1]` finds none of them, `=`
asking for the numbers written; nothing reads the stored ones back into
what a client sent, so only writing a row's numbers again puts it right.

*Where the rounding is told.* In the value itself, per literal and per
parameter. A vector of whole `f32`s short of 2^24 (`value::whole_f32s`) is
the integers written: the quick readers never make one of a list that hid
a rounding -- a list holding a number written with a fraction or an
exponent whose `f32`s all come out whole is read again
(`num::hides_rounding`, one pass, asked only of such a list, so an
embedding pays nothing) and made the list as written instead
(`[1.00000001, 2]`, `value::quick_vector` a value at a time). So a literal
or parameter of whole numbers is taken as it came (`Exactly::lists`,
`Exactly::settle` against the parameters as read), every other vector bound
for a list of numbers is read again, and a vector built in Rust and handed
to one is its `f32`s widened (`Value::coerce`).

*How a list was read changes no answer.* One value can be read either way
in one statement -- a parameter given an `[float]` and compared with a
vector field (`set t {xs: $1} where v = $1`), a text read as written for
one literal holding another compared with a vector field -- and a vector
was never equal to a list: read as written, `[0.5, 2]` found no row
holding the vector `[0.5, 2]`, which main found, while `[1, 2]`, whose
quick read is taken as it came, did. A filter compares a vector field with
a list of numbers alone by the `f32`s the quick read makes of it -- `=`,
`<`, `in [...]` -- the list made that vector once a query where the filter
is bound (`value::compared_vector`, `Filter::bind`), as `near`, a vector
field's value and the vector functions read one already. Asked in
`Value::cmp_value` instead, in the arm of the kinds that do not compare,
an order by an int over 100 000 rows took 1.15% more instructions and one
by a float 1.54%, every comparison's dispatch laid out anew; moved out of
line there, 3.4% and 3.2% (`make instr-compare PKG=fennec-core
EXAMPLE=orders`). A comparison the filter does not bind -- one inside a
`case`, an item of the select list -- compares a vector and a list as
two kinds still. And a value is
the same whichever way the reader came to it: a list a hint read at once
whose `f32`s are the integers written is given back as the quick read's
vector (`value::whole_vector`, below), and the native library and the
browser module, which read every parameter again where one needs it, take
only the places named from that read (`exact_noting`), so a whole list
beside them is the vector it is with nothing named.

*Read once where it can be, and nothing kept decides an answer.* A server
reads a list as written in its first read wherever it can know to:

- A parameter: a text's parse-cache entry (`api::Cached`) -- for a text
  too long to keep, a table by its hash (`api::HINTS`) -- holds the places
  its last request read again as written (`Cached::hint`), and the next
  request's parameters are read with those as written at once
  (`json::parse_object_listing_at`): every client writes the text before
  the parameters, and the text is looked up as they are reached, the one
  lookup the cache made before. The hint is held to what
  `Database::exactly` names at every request: a place it no longer names
  -- the schema changed, or a long text's hash met another's -- has the
  parameters read the quick way again and the hint dropped; a place it
  names is read as written either way, at once or again, since
  `Exactly::settle` drops a value holding no vector, which is what a value
  read as written is -- but for a list given a list of numbers alone
  (`Exactly::lists`), which `settle` takes as it came where its `f32`s are
  the integers written: read at once, such a list is given back as that
  vector (`value::whole_vector`). So the value is the hint's no more than
  the answer is, whatever text or tenant left it: the parse cache is the
  process's, a `--dir` node's tenants' alike, and a long text is known by
  its 64-bit FNV-1a alone (`a_hint_another_tenant_left_changes_no_answer`).
  First read, the hint read `[1, 2]` as the list of two ints where the
  quick read made a vector, and `v = $1` beside `xs: $1` found the row by
  one and not the other. Kept again only where it changed, under its
  shard's lock: a statement whose hint holds takes no lock for it. The
  first request of a text still reads twice. A hint keeps at most 4 096
  places (`HINT_PLACES`; a text naming more keeps none, and reads twice
  each request) and a shard of long texts' at most 16 384 together
  (`HINT_BUDGET`, forgotten past it as past its 64 texts): a place is 8
  bytes, and a long text's are bounded by its body alone --
  `put t [{xs: $1}, ..., {xs: $3000000}]` would have kept 24 MB a text.
- A kept text: its tree read as written is kept beside its quick one, read
  once (`Cached::exact`) -- what the text says alone, the same tree each
  time.
- A text too long to keep -- a page of rows written out -- is a `put` or
  an `insert` read once (`fennec_ql::parse_one_listing`): the lexer reads
  each list into its vector as the quick read does, and keeps its numbers
  as written beside it from its first number an `f32` does not hold on, in
  one arena set aside by the text's length the first time a list keeps any
  (`lexer::Kept`); a list of small integers keeps none and is made from its
  vector. Where a request needs the text read as written and every list
  read into a vector is a document's field's, the documents' lists are put
  in their places in order (`read_listed`) -- the tree `parse_exact` reads,
  to the bit (`a_put_s_lists_read_again_are_its_text_read_as_written`,
  20 000 random `put`s); otherwise the text is read whole again. Read again
  from the text instead (`values_at` at each list's place), those lists
  cost as much as their first read, 14% of the page; kept in a `Vec` a
  list, with a `Vec` of their places beside, or in an arena grown as it
  went, the allocations sent the allocator down its slow paths, some 600
  instructions a document (`tools/instr/trace.py`): 2.5 to 5.5% of the
  page, 0.7 to 2.6% of a page of `[int]`s, 3 to 8% of a page of vectors.

Rejected: a bit returned beside the statement (`Result<(Statement,
bool)>`) or set on the thread is one per parse, not per literal; the
`f64`s kept beside each vector double every embedding's memory, which the
browser module never gives back; a literal found again by its address or
its `f32`s fails a cached tree cloned, and two texts reading to the same
`f32`s -- the listing reads its lists back in the order the documents hold
them, into a tree the request alone holds. And every literal list read as
written, its `f32`s
made where it binds to a vector, as the browser module makes a page's
(a double, then `Math.fround`): no bit would move -- the lexer reads each
number into an `f64` first and rounds that, natively (`num::clinger`,
`num::parse_f64`) as in the module -- but the tree would, and a named
statement is held to its tree's digest: every stored definition holding a
list of numbers would read to another digest and be marked broken under a
new grammar epoch, and `make fennecql-corpus` would find every such text
reading another tree. A definition alone is read so, its digest still its
quick read's (below). The cost in time is not what rules it out: read as
written, a page of 100 128-dim vectors parsed in 6.78 M instructions, the
quick read 5.77 M (`make instr-compare PKG=fennec-http EXAMPLE=number_lists
MODES=parse,parse-exact`) -- but each list a `Value` a number, eight times
a vector's memory until it binds.

*A named statement runs its text as written.* A definition's tree is the
one an open reads its text into (`fennec_core::named::Reader`), and it was
the quick read's: a body's `[0.5, 2]` handed a list of numbers or a json
field was a vector there, which `Database::exactly` refused at every call
-- naming `fennec_ql::parse_for` to an HTTP caller -- every slot of a
schedule, every event of a hook and every write a binding woke, its
collection then unwritable; on main a list of numbers took the vector's
`f32`s, `[0.1]` as 0.10000000149011612. Now the reader reads the text as
written (`fennec_ql::stored_definition`, `parse_exact`), and holds it to the
digest of the tree the text reads to the quick way, the one every record
holds: no stored definition reads to another digest for it, but one hiding
a rounding (*Grammar epoch 4*, [named-statements.md](named-statements.md)),
and one stored by an earlier build runs its lists as written from its next
open. A vector in a body takes the list's `f32`s where it binds, and `near`
and `=` read it so (above); a text with no `[` is read once.

*The bindings hand a list an `f32` holds apart, marked exact.* Swift,
Kotlin and Dart hand any list of numbers apart as `f32`s, as the browser
module's page does a vector (`vectorsApart`); one bound for a list of
numbers or a json field was asked for again as JSON (`exact`) whatever it
held, since its `f32`s may be the rounding of the client's doubles. Where
each of its numbers is one an `f32` holds -- a typed `f32` array, an int of
at most 2^24, a double the `f32` it rounds to gives back -- the binding
marks it exact and a list of numbers takes it as it came, in one call, as
on main ([Vector parameters](#vector-parameters)). Any other, `[0.1]` or
`[16777217]`, is asked for, and each binding, as the JavaScript client
does, keeps by text the places the library asked for (256 texts) and sends
them as JSON from the second call on.

*What reading again costs, and what was made cheaper for it.* A list read
again is read in one pass, as the quick read reads it: the lexer makes a
list of numbers alone one token as written (`Tok::Values`, `values_at`) for
`parse_exact`, where a token a number was most of what reading the text
again cost; the JSON reader reads one so too (`json::exact_numbers`), 10.7
M instructions for 1 000 parameters each a `[float]` of 16 against 17.2 M
a value at a time; Clinger's path divides a 17-digit fraction itself
(`num::divided`), which went the general path's two more scans before,
some 250 instructions a number read either way, a vector's too; and
`Value::coerce` makes an `[int]` of whole `f32`s and a `[float]` at once
natively, and keeps a list already of its type as it came. A body's text
and parameters are taken out of what the JSON reader made, where they were
copied. The browser module reads a text as written a token a number, as
before, and its parameters' JSON whole: a page's vectors go over apart, so
what the JSON holds is small.

Against main before (`number_lists`, `make instr-compare`, the mean of 8
binary identities' medians of 7 runs each; instructions a request; `parse`
and `parse-exact` as first measured, the rest after the exact mark and
`engine::Places`, against befad9f2):

| mode | main | lists as written | |
|---|---:|---:|---:|
| `docs`: 1 000 rows as one parameter | 53.79 M | 45.35 M | -15.7% |
| `text`: the rows written in the statement | 48.72 M | 47.39 M | -2.7% |
| `params`: a parameter a value | 47.79 M | 45.83 M | -4.1% |
| `ints`: the rows' `[int]`s alone, written | 25.55 M | 24.71 M | -3.3% |
| `embed`: a 128-dim vector and an `[int]`, parameters | 118 850 | 92 363 | -22.3% |
| `vectors`: 100 128-dim vectors written | 12.78 M | 9.62 M | -24.7% |
| `short`: a kept text, a `[float]` of 16 written | 38 370 | 37 871 | -1.3% |
| `parse`: the `vectors` text parsed alone | 9.24 M | 5.77 M | -37.5% |
| `parse-exact`: that text read as written | 20.49 M | 6.78 M | -66.9% |
| replays' `put` | 22 100 | 21 174 | -4.2% |
| replays' `keyed` | 84 817 | 83 618 | -1.4% |

On main those lists were stored as `f32`s; each request here stores the
numbers written. The first reading of the hint (a parameter read again at
every request) was +20% on `params`, and a page's lists read again from its
text +44% on `text`.

Again after the review that made a hinted whole list the quick read's
vector, a filter compare a vector field with a list by its `f32`s and a
named statement run as written, against main a few changes on: `docs`
-15.74%, `text` -1.72%, `params` -3.01%, `embed` -20.22%, `short` -0.65%,
replays' `put` -3.81% and `keyed` -1.02%; and `orders`' `order-int`,
`order-float` and `scan-float` inside 2 SE of main. The hinted places' loop
asks each value before its place: looked up first, `params` was 0.36%
more.

## Vector parameters

**A vector parameter goes into the module as `f32`s.** `web/fennec.js`'s
`run` takes each parameter that is a typed array or an array of finite
numbers -- which the module reads as a vector either way -- out of the
JSON, where it leaves `null`, and hands it over beside it: its place, its
length and its values (`vectorsApart`; `fennec_query`'s two arguments
before its last, the clock; `with_vectors`). A `-0` goes over as `0`, as JSON writes it,
so the file is the same either way (`web/fennec.test.js`). 200 768-dim
vectors went in in 21.9 ms as JSON and take 4.4, and `make wasm-speed`
builds 10 000 128-dim ones with their graph in 1 165 ms against 1 307.

**A vector marked exact is a list of numbers as it came.** Since a list
of numbers keeps its numbers as written ([Numbers in](#numbers-in)), a
vector handed apart and bound for an `[int]`, a `[float]` or a
`[timestamp]` is asked for again as JSON (`Refused::Exact`): its `f32`s may
be the rounding of what the caller held, `0.1` or 16777217. Each binding
and `vectorsApart` sets the top bit of a vector's place (`fennec_abi::EXACT`,
`FENNEC_VECTOR_EXACT` in `fennec.h`) where every number it was handed is
one an `f32` holds -- a typed `f32` array; an int of at most 2^24 in size;
a double `d` with `Double(Float(d)) == d` (`Math.fround(x) === x` in
JavaScript) -- and `exact_noting` takes such a vector as it came for a list
of numbers: one of whole `f32`s short of 2^24 stays the vector the engine
takes as the integers written, any other is made the list of its numbers,
a whole one an int as JSON writes it (`[16777216]`, `[0.5]`). A json field,
an `in $n` list and a path into a json field keep asking for it (an int
and a float differ there). An unmarked vector, from a binding before the
mark, is asked for as before; a library before the mark reads a marked
place as past the parameters and refuses the call ("malformed vector
parameters"), and the bindings ship with their library.

Measured with `ffi_bench lists` (a buffered put of a list of 8 beside a
128-dim vector, 20 000 a round, p50 of the steady rounds of four runs a
build in turn, the 1-minute load under 2): an `[int]` of small ints marked
exact 2.04 us (2.30 to 2.34 mean) against main's 2.00 to 2.04 (2.28 to
2.33), where main took the list as its `f32`s; a `[float]` of tenths, which
no `f32` holds, 2.33 to 2.38 us sent as JSON from a text's second call,
3.58 to 3.62 us its first, refused and sent again, against main's 2.00 to
2.04 storing their `f32`s; `ffi_bench put`, a vector alone, 1.96 us
against 1.92 to 1.96.
Before the mark the `[int]` took 3.38 us asked again each call, 2.12 us
sent as JSON at once. One `fennec_query` call traced (`trace.py`, the
program's own code, a run's 200th call): a 128-dim vector's put (`put t
{n: 0, e: $1}`) into a collection with no list field 13 660 instructions
against main's 13 719, into one with an `[int]` field too 14 285 against
14 006 -- what asking a field holding a list of numbers whether a
parameter is given it costs (`Known::give`) -- and the marked `[int]` of
`ffi_bench lists` 16 219 against 16 058.

What brought it there, the trace read function by function: whether a
field of a collection keeps numbers a flag of the collection, set with
its schema wherever that changes (`Collection::numbers`, `set_schema`; a
debug build holds it to the fields at every look), so a `put` of literals
and parameters into one with none is answered at a look, no `Exactly`
made (`Database::exactly_needs`) -- made, walked and dropped by the
library's ask and again by the engine's own, a vector's put took 13 825
instructions, 106 over main's; the walk inlined into that one body, its
answer made where it is returned -- a second caller put the walk out of
line, and the answer moved out of a local copied its 136 bytes, together
70 to 100 instructions a call where a field keeps numbers;
`Exactly`'s places held in place, up to four (`engine::Places`) -- in `Vec`s a list's place
was two allocations a walk, and a call walks twice, the library's ask and
the engine's own (`refuse_inexact`), with the binding's place one more,
five allocations and some 1 200 instructions with the allocator's; a lone
statement needing nothing answered inline in `exact_noting`, the rest out
of line -- inline whole it grew the library's call past what `fennec_query`
inlined, 80 instructions; the field a pair gives looked up once, out of
the walk's closure (`Known::give`); `holds_vector` inline for a number or a
vector; and the lists `parse_exact` reads as written held beside the
tokens (`Tok::Values` an index into `Noting::listed`), where in the token
every token's drop walked a `Value`'s kinds, some 170 instructions a parse
of a short `put`. The browser module keeps `Places` a `Vec`,
`exact_noting` one body and `holds_vector` a call: as natively they were
1.0 KB of it (`make size-report`). It keeps the flag, which spares the
walk its look at the fields, but not the answer at a look: `exactly` is
the walk's body there, and `exactly_needs` asks it -- as natively, 0.3 KB.

## Quantized indexes

**A quantized index holds codes, and `near` orders by the documents'
vectors.** `@hnsw(..., quant=int8)` keeps a byte a component over a scale a
vector, `quant=bit` the signs of its distance from a centre (cosine only). A
code only estimates a distance,
so `Space` (`engine.rs`) takes the beam's `ef` candidates and puts them in
order by the vectors read out of the store, as `rerank` does, so every score
is exact -- reading one only while it can still make the page. An int8 code
is off by its rounding, a step a component, and `VectorIndex::floor` is the
nearest its vector can plausibly lie (six standard deviations of that
rounding, wrong less than 1.5e-8 of the time); a candidate whose floor is past
the k-th exact distance held is neither tested nor read. At 100 000 x 768 that
is 16.8 of a beam of 100, and of 400, and 20 over a million, with recall
unchanged; a bit code bounds nothing and reads the whole beam. A filtered set under the ANN budget is
ranked by its codes and its beam's worth ordered the same way -- read whole it
was up to 12 800 vectors a query under bit codes -- and `exact` reads every
vector. Bit codes take the wider beam `BIT_EF_SEARCH` -- 200: over a million
clustered 768-dim vectors a beam of 100 held 97.9% of the true ten and 200
held 99.7% in 0.66 ms, int8 codes 97.1% at 100 (`make quant-bench`); over
the vectors' own signs the beam was 400, for 98.4% in 2.16 ms. The core settles it
wherever a spec comes in -- `VectorIndexSpec::default()` leaves `ef_search`
0 for `resolved` to fill by the codes -- since only the parser knew it once,
and the Rust API's bit indexes searched 100. How well bits estimate
depends on the vectors: spread in every dimension, 89.7% at 100 and 97.5% at
200. The code
kernels add in `strip8!`'s order on every target, so a graph over codes is the
browser's graph bit for bit; on aarch64 the int8 strips are NEON intrinsics
(`vector::neon`, as every strip there is), because the vectoriser widened
codes through a register it also accumulated in, which chained every strip to
the one before -- 5x slower, or not, depending on the code around it. A graph
over codes was record version 4, every other graph 3, so no file was rebuilt
for the feature; laid out flat (7), every record carries its quantization.

## Bit codes

**A bit code is a residual's signs, from a centre the index learned.** The
vectors' own signs were the code before: most of a crowded cluster's signs
are its centre's, and over 100 000 x 768 spread in every dimension a beam of
100 held 36.1% of the true ten, 400 held 74.9%. A bit index learns 128
centres by k-means++ and four Lloyd rounds over its first 2 048 vectors
(`BIT_TRAIN`), holding those whole until then -- an index under that
searches exactly -- in one order on every target, so the browser learns a
server's centres; the insert that learns them takes 195 ms at 768
dimensions. A code is the signs `b` of `r = v - C`, the centre's byte, and a
word after the signs holding `σ = |r|²/|r|₁` and `κ = ⟨C, r⟩ - σ⟨b, C⟩`;
`⟨q, v⟩` is estimated as `⟨q, C⟩ + κ + σ⟨b, q⟩` -- RaBitQ's estimator
without its rotation, the query taken from the centre so that only its part
off the centre goes through the signs (`⟨q, r⟩` estimated whole held
38.7% over centres that gave 92.8%). `query_for` appends the query's
product with each centre, once a search, and a distance between two codes
counts both `κ` (`Arena::offset`), which a widened code leaves out. The same
beams hold 89.7% and 99.6%, and over 32 directions a beam of 100 went 96.8%
-> 100% at 100 000 and 82.5% -> 97.9% at a million, for 105 bytes a 768-dim
vector against 96. In an array of their own the factors were a cache miss
more a distance: the build took 69.7 s against 62.8, a query at a beam of
100 0.414 ms against 0.378. `⟨b, q⟩` is counted, not summed: `query_for`
also cuts the query to six bits a component (`QUERY_BITS`), two's
complement bit planes after the centre products, and `dot_planes` takes the
planes' popcounts under the signs -- 72 over 768 dimensions where `dot_bits`
added a float a component, whole numbers exact on every target. RaBitQ cuts
to four bits the query's distance from a centre; cut whole, a query is
mostly its own centre, and four bits held 84% of a crowded cluster's ten
against the 91.5% six hold, as the query whole does. At a million a query
at a beam of 200 went 1.30 -> 0.66 ms at 99.8% -> 99.7%, filtered 1.36 ->
0.97, the build 1 012 -> 874 s; the browser module's 0.557 -> 0.470 ms at
20 000 x 384. The planes ride in the query's `Vec<f32>` as the bits of two
floats a word, since every search hands that vector on. The centres travel in the graph record behind
quantization code 3 (`BIT_CENTRED`): a graph over the plain signs (code 2)
is built again, and a binary from before them builds its own. The browser
module grew 6.9 KB, 2.2 KB brotli, most of it the k-means.

## Filtered near

**Filtered `near` needs its fallback.** The filter's rows are probed first -- in
blocks spread over the collection, and only until more than `ef × m0` match,
which is all the plan needs to know. A set that stays under that is searched
exactly; a larger one goes through the ANN with each candidate tested against the
filter. That second path *must* fall back to searching the whole set (the probe
carries on from where it stopped) when the result lands under the limit —
otherwise a filter correlated with the vector eliminates every candidate and
returns empty. The probe decides when rows are read, never the answer:
`tests/filtered.rs` checks it against the plan with the whole set found first.
That fallback, a set under the budget and `exact` read every vector they
measure, so natively they measure a share of 8 192 at a time on every core,
each share's nearest kept and all of those kept again in the shares' order
-- the rows, and the order of their ties, one walk in turn keeps
(`nearest_of`) -- and keep the nearest `k` as they come rather than sort
every one (`nearest`). At 1 000 000 x 128 a filter matching a quarter of the
rows, none of them near the query, took 20.8 ms and takes 5.1; `exact`
31.6 and 12.2, which is reading 512 MB. A scan asks for the vectors two
groups ahead of those it measures (`dists_to::<true>`), each a wait on
memory otherwise: 10 000 of a million 128-dim vectors in 0.41 ms against
0.82. A set an index names -- a hash bucket, the buckets of an `in` -- is
sized before it is gathered (`indexed_size`): past the budget the walk runs
with the filter as a row test, its beam widened until about twice the page
passes the filter, and where that beam would measure as many vectors as
the set holds the set is searched exactly at once, as the walk would have
ended doing. Gathered, a quarter's 250 000 ids -- copied, sorted, each
looked up -- were most of the 0.71 ms a filter keeping 25% of a million
128-dim rows took at a beam of 100, against 0.15 unfiltered, and at a beam
of 40, which a quarter passes about the page of, 43% of the walks came up
short and searched the 250 000 exactly (p99 8.9 ms); now 0.16 and 0.15 ms,
and a filter keeping 1% 0.69 against 1.22 (`make scale-bench`). The
browser module grew 2.9 KB, 1.0 KB brotli.

## Sparse vectors

**`sparse<N>` is pgvector's `sparsevec`, and `@inverted` answers exactly.** A
sparse vector is held as its non-zero entries, `(index, weight)` ascending
with indices from 0, and travels everywhere in pgvector's text form,
`{1:0.5,3:0.25}/N` with indices from 1 -- a JSON string, a FennecQL
literal, an imported `sparsevec` -- so a pgvector table and `fennec import`
carry the same vector.
Every way in goes through `sparse::normalise` (order, an index given twice
refused, zeros dropped), and the index relies on it. `@inverted` is the text
index's shape with weights where the counts were; `near` by dot product sums
the query's lists a list at a time into a slot a document, a window of 65 536
ids at a time (`sparse::summed`), where they are dense in the ids they span,
and walks them with MaxScore where they are not -- rank-safe, the bounds
compared once rounded to `f32` after a 1e-9 slack, so a tie-break never
depends on the pruning; the browser always sums, and has no walk. A SPLADE
query's dozens of lists reach most of a collection, and the walk, choosing a
document at a time among their heads, took 2.54 ms p50 on FiQA against 0.68
summed, 0.60 against 0.10 on SciFact; bounds per block of a list's postings
(2.69 ms) and per range of ids across the lists (443 of 451 ranges still read,
35 MB more) did not pay. `pruning_never_changes_the_answer` holds both ways to
an exhaustive walk, and `tests/sparse.rs` holds `near` to `near ... exact` row
for row. Only a document sharing a dimension with the query is ranked. Like
the text index it is derived and never persisted. It sorts through the
engine's one `(DocId, f32)` sort and maps dimensions through `DocMap`'s
`HashMap<DocId, u32>` -- its own were 12 KB of the browser module; the
feature cost 16.2 KB, 4.3 KB brotli, as it came, and `make wasm-sizes`
weighs it at 2.7 KB brotli added to the module with none and 0.9 left out
of the full one, the sort and the map being the other indexes' too.
SPLADE++ (`beir/splade.mjs`) scores
nDCG@10 0.693 on SciFact against `match`'s 0.662, and 0.331 on FiQA against
0.232 (the dense vectors 0.366), at 0.68 ms p50 over 57 638 documents: its
queries' 37 to 65 terms reach most of the corpus. Both BEIR scripts cut texts
themselves: transformers.js drops the closing [SEP] when it truncates, SPLADE
without it took SciFact to 0.23, and the dense vectors (`embed.mjs`) moved by
up to 0.003.
