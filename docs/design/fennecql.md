# FennecQL and schemas

The long form of the invariants [CLAUDE.md](../../CLAUDE.md) lists under *FennecQL and schemas*: SQL's spellings, parameters for counts, inner `get`s, `alter`, json paths, the shell's reading and declared schemas. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## SQL spellings

**SQL's spellings are FennecQL, read by the one parser into the same
statements** (`fennec-ql`'s `values`, `on_conflict`, `assignments`,
`worded`, `in_list`; `query::sql_like`; `site/content/docs/sql.html`).
The pg wire, its SQL subset and its `pg_catalog` went on 2026-10-01 and
stay gone; what came back is spelling, with no mode and no second front
end, so HTTP, MCP, the shell, the browser module and the native library
read it alike. `INSERT INTO t (a, b) VALUES (..), (..)` is `insert t
[{a: .., b: ..}]`, `UPDATE t SET a = e` `set t {a: e}`, `x IN (1, 2)` `x
in [1, 2]` (a `(` after `in` took a `get` alone, so no text that parsed
reads otherwise), `x [NOT] BETWEEN a AND b` `[not] (x >= a and x <= b)` --
its `and` read by `worded` before `and_expr` sees one, and its operand a
field or a parameter, copied as one: any expression's copy was `Expr`'s
whole clone, 2 KB of the browser module -- `NOT IN`, `NOT LIKE` `not`
over the test, `IS [NOT] DISTINCT FROM` `!=` and `=`, `IS [NOT] TRUE`
`[not] (x = true)`, `count(x)` the rows where `x` is not null
(`Fold::add` skips a null for `Count` too, `count(*)` being handed `true`),
`'it''s'` a quote (`Tok::Single`), `x = ANY($1)` `x in $1` and `x <>
ALL($1)` `not (x in $1)` (grammar epoch 2, [below](#list-parameters)),
`||` the `+` it lexes to, `NULLS
FIRST`/`LAST` where it says what the order does, `values {..}, {..}` and
`put t {..}, {..}` a list without its brackets, `ON CONFLICT DO NOTHING`
`if absent` and `DO UPDATE SET` `if absent else set`, `excluded.f` made
`new.f` and `<collection>.f` `f`. A conflict target, `ON CONFLICT
(email)`, rides in `Pick::conflict` and the engine takes it only where it
names the one unique key the collection's rows are found by
(`Schema::arbiter`): `if absent` is held by every unique key, so with a
second, or an `id` named beside it, the two would part. The fields of an
insert are named (the parser reads no schema, `id` is no field, an alter
adds last); a row of another arity, `INSERT … SELECT` and `DO UPDATE …
WHERE` are refused naming the FennecQL to write, as are `CREATE TABLE`,
`CREATE INDEX` (a name, no kind), `ALTER`/`DROP TABLE`, `TRUNCATE`,
`BEGIN`/`COMMIT`, `JOIN` and a comma between collections (both ways a
`lookup` runs, its `where` before it), an alias, `SELECT DISTINCT` (outside
`in (select distinct …)`, where it is passed over), `EXISTS`, a scalar
subquery, `MATCH(…)`, `@@` and `->>` -- refused without consuming the word
they start at, which the shell's statement cutting reads. A `get` whose
clauses stop at a join or an alias says so before its `check`, whose
refusal of a grouping over the join's columns came first and hid the
hint. An agent eval (row 40) found agents recovered whenever an error named
the FennecQL form and never from ``unknown command `x` ``; on this branch
SQL's first tries went 30–35% -> 61–71%. Past the parser: `rowid` and
`_id` are told `id` (`schema::no_field`), a field a `lookup` level lacks
that the clauses after `lookup` are its collection's, `users.email` that a
field is named bare (`path_of`), `json_extract()` a path,
`contains()`/`starts_with()`/`ends_with()` `like`, and `/_schema?as=fennecql`
and the MCP `schema` tool name the `id` in a comment (`with_id_note`), which
`/_schema/plan` reads past. `like`/`ilike` are calls, as `case` is, so no
walk over an expression grew a variant: `eval` special-cases them
(`sql_like`: a `nocase` field by its fold, as its `=`; `ilike` by
Unicode's full fold, refused without `nocase`; no escape unless named; a
pattern ending in its escape refused whatever the text), and
`Filter::bind` binds one over a text field to `Test::Pattern` through the
out-of-line `apart` arm, so `test` gained no arm. Through `eval` a scan of
100 000 rows by `%99 of%` took 42.8 ms against `~`'s 8.3; bound, 29.0;
with a literal after a `%` found by a byte scan, 10.4. **Two traps
refused.** SQL's `"name"` is a column, FennecQL's text: `where "name" =
'a'` answered every row or none. A double-quoted word is a name where only
a name stands (`ident`, `path`) and text where a value does, and is
refused (`Parser::quoted`) where SQL reads a column and text makes the
answer the same in every row: alone on the left of a test against
literals or parameters, on the right against a literal, a select item, an
`order` or `group` key, an aggregate's argument -- never `$1 = "admin"`
(a policy's claim) nor `name = "x"`. PostgreSQL's `~` is a regular
expression: a `~` literal anchored (`^a`, `a$`) or holding `.*`/`.+` is
refused pointing at `like` (`regex_like`; 85 `~` literals in the
repository, none refused). `<>` stays `!=`, null and all: it was read so
before any other SQL, and SQL's unknown runs through every `not` above it,
which no rewrite of one comparison carries. The corpus of every FennecQL
text the repository holds -- the docs' blocks, both golden files, the
Rust crates' literals, the web tests' -- parsed on main and on the branch,
4 085 texts: none parsed to another statement, the trap's own tests aside.
`cmp_expr` keeps its frame: the worded tests are out of line (`worded`),
or the depth test's 512 levels outgrew 8 MiB in a debug build; out of
line, 511 parentheses parse in 1 091 KiB of stack in release against
main's 1 187. The browser module grew 20.2 KB, 7.9 KB gzip, 6.7 KB brotli
(733.7, 301.4, 250.5), the one with no feature 19.9 and 6.5: the parser's
SQL forms and their refusals 9.9 KB raw, `like` 2.6 (`sql_like` 1.0,
`like_pattern` 0.8, `bind_pattern` 0.8), the conflict target's check and
the hints past the parser 2.0 (`Schema::arbiter` and `no_field` 1.3, the
engine's calls of them 0.7). The trap's error first found by
`str::contains` brought the standard library's two-way searcher and a
slice's panic, 4.1 KB, where `ends_with` brings none. Part 2 below.

## SQL joins, subqueries and schema

**A JOIN on a key is a `lookup` answered flat, or one table's `get` with
the other's rows a filter** (`fennec-ql`'s `join.rs`: `relate`,
`correlate`; `Select::flat`, `ResultSet::answered`). SQL's `from orders o
join customers c on c.id = o.customer_id`, `left [outer] join`, `using
(k)` and a comma whose key is in the `where` are read by `tables_after` into
the tables and their `on`s, the statement's clauses parsed as any `get`'s
with qualified names (`o.total`, a path to the lexer), and `relate` makes
of them the statement they mean. Each `on` pairs a column of the joined
table with one of the table joined just before it -- several by `and` are a
key over several fields -- so the tables are a `lookup` chain, the joined
table read by its column as a child key must be (`id`, `@hash`, the lead of
an index over several, which the engine holds it to); its other terms, and
the `where`'s, go to the level of the one table they read. A name no table
qualifies is the first table's: the parser reads no schema, and SQL would
have refused one two tables hold. Where the answer reads one table's
columns and each of its rows is answered once -- the other table found by
its `id`, or `distinct` -- it is that table's `get`, the other's rows `key
in (get ...)` (`semi`), the first table's key read as the joined table's
where the joined table's rows are answered and the first is found by its
`id` (an inner join's rows hold one value in both); an aggregate or
`distinct` otherwise reads both tables' rows and is refused, since over
pairs a row is counted once a match. Otherwise it is the `lookup`, each
level `required` for an inner join, and the answer is SQL's rows: the
select carries a `Flat` -- each column's level, column and SQL's name, the
page -- which no FennecQL text sets, so no text that parsed answers
otherwise; the engine refuses a name two columns would take (`flat_page`:
`select *`'s every `id`), and every JSON writer and the shell render
`answered()` -- the paths of `ResultSet::flatten`, picked and paged
(`picked`) -- after a token's masks have read the levels, which hold each
level's fields to its own collection's rules as the `lookup`'s are. The
order is the first table's, whose rows the chain hangs off; an order by a
joined table's column would order each row's children, and is refused. The
page is the first table's where every level is found by its `id`, at most
one row a parent; otherwise it is cut from the flat rows, the first table
read as far as the page could need (`limit + offset` of them where both
are numbers written, all where a parameter). A `left join`'s table in the
`where` reads the rows it kept with nulls: `o.id is null` (or its key) is
the rows with none, `not (key in (get ...))`; anything else is refused --
one that drops the rows kept with nulls, a comparison with a value, makes
it an inner join, written so, since telling which conditions do was code
for a spelling SQL's own style guides warn of -- as are `right`, `full`
and `cross` joins, an inner join
after a left one, and a condition over two tables other than the key.
`tests/join.rs` holds 720 generated joins -- inner and left, to one row
and to many, a chain of three, filtered, ordered, paged, `distinct`,
`count(*)` -- to rows worked out by nested loops over each table, each
reading reached over 40 times, and fails with the page rule or the
semi-join's once taken out; and three joins to their `lookup` written by
hand, flattened. **`exists`** (`Parser::exists`, `correlate`) takes the one
equality between a column of its query's table and one qualified by a
table around it as `around in (get inner select col where <the rest>)`,
`not exists` `not` over it, which keeps a row whose key is null as SQL
does; any other correlation is refused. **`= (select ...)`**
(`Parser::scalar`) is the same list where the query answers one row -- an
aggregate with no group, `limit 1` -- and refused where it may answer
several, which SQL refuses at the second. **`select distinct`** is a group
of the list's items, and a list of keys alone with a `group` -- FennecQL's
`select city group city` and SQL's `group by city` with no aggregate, both
refused before -- is that group (`keys_alone`); `order by 2`/`group by 1`
name the list's items in SQL's order (`ordinals`), where a number was a
key the same in every row. **`create table`** (`create_table`,
`sql_column`) maps each type by `fennec_ql::sqltypes`, PostgreSQL's names
and then SQLite's affinity rules, the table `fennec import` maps its columns
by (`pg::map_oid` and `sqlite::map_decl` call it; each crate's test holds
`create table`'s reading to the import's type by type). A column `id` of
an integer type that is the primary key is the row's id; a primary key on
other columns is `required` and `@unique`, over several an index over
them; `not null` is `required`, which refuses a `null` now -- it took one,
where `fennec types` and the declared schema's `.notNull()` read it never
null -- `unique` `@unique`; a length is not held, as SQLite holds none;
`default`, `references`, `check`, a computed column, a sequence on another
column and a decimal are refused rather than dropped, which would take
writes SQL refuses. **`create [unique] index [name] on t [using m]
(...)`**: btree `@sorted` or an index over several fields, `@unique`, hash
`@hash`, hnsw `@hnsw` by its operator class (pgvector's default
`vector_l2_ops`) and `with (m, ef_construction)`, gin over
`to_tsvector(...)` `@text`; a partial index and ivfflat, gist or brin are
refused. **pgvector's `order by emb <-> $1 limit k`** -- `<->`, `<=>`,
`<#>`, lexed as `Tok::Distance` (`<#>` was a `<` and a comment; the shell's
`Unclosed` reads it so) -- is `near` with `Near::metric`, which the engine
holds to the index's distance (`metric_held`), refusing another with the
operator to write; `_score` is the L2 distance, 1 minus the cosine
distance, and the inner product. The corpus of every FennecQL text main's
tree holds -- the docs' `fennecql` and `sql` blocks and inline code, both
golden files, the brief, every JS, Python, Go, C#, Swift, Kotlin and Dart
string and the Rust crates' literals that begin a statement, with the
review's own texts: 7 620 texts, 15 240 parses with `parse` and
`parse_exact` -- parsed on main and the branch: of the repository's texts
no tree changed and none broke; 34 parse that were refused, 42 are refused
in other words. **The exception, deliberate:** SQL's `order by <n>` and
`group by <n>` in a `select ... from` -- written with `by`; FennecQL's
`order 1` and `group 1` keep their trees -- name the list's items, where
main read the number as a key the same in every row, which ordered nothing
and grouped every row as one: never SQL's meaning, and no text of the
repository's writes it. So `select name from users order by 1`, `select
name, n from t order by 2 desc`, `select a, b from t order by 1, 2`,
`select name, age, age * 2 as twice from users order by 2 desc, 3, 1` and
`select status, count(*) from t group by status, 1` parse to other trees --
`order by (1)` is the ordinal as well, as PostgreSQL reads it, its brackets
no node of their own -- and `select * from t order by 2` (a number over
`*`, whose places a parser reading no schema cannot count), `select
count(*) from t group by 1` (an aggregate as a group's key, which SQL
refuses) and a number that names no item -- `order by 0`, `order by -1`,
`order by 3` over two items, each refused by SQL -- are refused where main
parsed them; `select distinct from t`, refused on main for its
`distinct`, is refused in other words.

**What the eval refused after the first pass.** An item's name without
`as` -- `count(*) c` -- is read where the list goes on or ends at `from`
after it (`item_alias`; at the text's end the next line says which, `cut`),
a word no clause or statement begins with (`TAKEN`, the aliases' list). An
aggregate's `filter (where c)` is what `case` leaves of each row --
`count(*) filter (where c)` is `count(case when c then 1 end)`, `sum(x)
filter (...)` `sum(case when c then x end)`, `count(distinct x)` the case
inside the `distinct` -- since every aggregate passes a null over and
`count(*)` alone counts rows; `c` reads nulls as a `where` does. And SQL's
list refused for its own fault, a `from` ahead outside brackets
(`sees_from`), is refused with its own words: read as FennecQL's `select
<collection>`, an unnamed `sum(case ...)` or `count(*) c` was answered that
FennecQL has no `join`, and `meta->>'source'` in the list "expected a
command". Not taken: `?` placeholders (one run of 160, and of #358's 240),
`= (select ...)` over a query that may answer several rows -- exact only
with a check at run time that it answers one at most, PostgreSQL's "more
than one row returned by a subquery used as an expression", which the
engine has no place for -- and `->>`, whose text is not the json value a
path reads (`'5'` against `5`).

**What the review found.** An ungrouped aggregate over a join counted
each row once a match: `items` takes the list out of the select before
`folds` was asked of it, so `select distinct count(*) from users u join
orders o on o.user_email = u.email` took the semi-join and answered 2
where SQLite answers 4, and `count(*)` or `sum` over pairs was refused
with "unknown function" where FOLDED's words belong; `folds` reads the
items now (`items_fold`). A token's `x.*` beside `s.place as code` told it
a field it may not read was there (`Flat::shown`, access.md). An inner
join found by the joined table's `id` read every one of its ids as the
list of an `in (get ...)`, refused past 100 000. The list stays -- a small
joined table answers through the first table's index, `select u.n from
users u join vip v on v.id = u.n` three values of `vip` through `users.n`'s
`@hash` -- and the parser marks where the same join may be read through
the `lookup`'s probe instead (`Flat::probe`), which the engine makes of
the list's reading where it reads it (`Select::probed`): the list's term
taken out and `lookup <c> on id = key required where <its filter>` hung
off the rows, which answer flat as they stand -- a row's match looked up by
its id, whichever table the statement names first and with the joined
table filtered (a `required` level's filter is exact). Made by the parser
beside the list, the probe took a join's parse 2.25 -> 3.0 us. The engine
reads the probe (`answered_top`) at once where the list asks every id of a
table holding
more rows than the bound, which is past it before a row is read, or an
index names more of its rows than the bound and than the probe would read
(`probe_wins`: the bucket's count and the tables' sizes, exact, as
`required`'s side is chosen by); otherwise it tries the list, its ids
listed to one past the bound at most (`distinct_column`), and reads the
probe once that passes -- where no index names a set, a range of a few
rows would have had the probe read every row of the first table. Read as
the probe alone, as the first round of fixes had it, that join to three
rows looked up each of 100 001 users; and the probe drives the table whose
rows are answered, so `select o.total from users u join orders o on
o.user_id = u.id order by u.id` reads `orders` by `user_id` with `users` a
`required` level found by its id. The probe is offered only over a key
its `lookup` takes beside an `id` -- `id`, or an int, float or timestamp
field (`probe_keyed`): a json, text or bool key keeps the list, which `in`
compares as values, and its refusal past the bound, where the probe was
refused as key types that do not match, naming a `lookup` the statement
never wrote. The list tried first lists its ids to one past the bound and
stops a scan there, but hands an index the bound it would have with none
(`matching_ids_to`'s `want`): handed to `filter_candidates`, it lowered
the ordered and composite indexes' range bound to 6.4 million rows, and a
wide range over a collection past 12.8 million was passed over for the
scan. Over HTTP and pinned, `query_json` writes the probe's rows itself:
declined, as a statement holding a `lookup`, the server had `query` answer
the statement again and walk the list a second time
(`fennec-http/tests/join_lists.rs` counts the lists refused past their
bound, `lists_past_bound`). The names are shared (`Arc`) by the copy every
statement holding an `in (get ...)` is answered in, which would otherwise
copy them. A scoped token's join keeps the list alone (access.md). In turns against main and the branch before these fixes, four
runs a side each begun at a load under 3: the join to three rows of `vip`
1.50 us against main's `in (get ...)` reading at 1.44 (2 322 as the probe
alone); 1 000 orders to 100 010 users 123 us in either order (122 as the
probe alone, which refused the other order; `lookup ... required` 147 on
main); `where u.n != 4`, which no index names, 168 against `lookup ...
required where`'s 189 -- 3 061 while the list was tried first; a join's
parse 2.25-2.29 -> 2.42 us. An aggregate, `distinct`, `exists`
and `left join ... is null` -- no row to look a missing match up by --
over a larger joined table still read the list and are refused past it,
never cut. `exists (... limit 0)` was true, and a `limit` there is held to
a number from 1. `int array` mapped to an `int` (`array` read into the
type's words). `--mcp-max-rows` counted a join's parents: it bounds the
flat rows, its `limit` put on the flat page, and the parents to as many as
that page past its offset could need, one flat row each at least -- the
probe of a `required` level stops at the page where there is no `order`
and no facet (`retain_with_children_to`), as every `lookup ... required
limit n` now does, where an 11-row page probed every parent; a join read
without flat rows is bounded as a `get` is. `-0.0` and `0.0` were two
groups, two distinct values, two facet values and two values of
`approx_count_distinct` (`encode_folded`, the group key, which a sketch
hashes): a sketch `hll_accumulate` stored before, merged with one made
after, may count them as two. Left as main has it: `3` and `3.0` under a
json path are two groups and two values of a facet read by the scan, but
one value of a facet a `@hash` index counts, which files a whole float as
the int it equals (`hash_key`) where `encode_key` keeps the two apart. A
LEFT JOIN's `on` condition on a table before it was told to go in the
`where`, which drops the rows; `hnsw with (m = 1)` was clamped, where
pgvector refuses it; and a name only a joined table has, read as the first
table's, read null in every row or failed at the first, and over no rows
not at all. The engine looks every table of a join up, read or not --
`left join nothere o on o.id = s.order` was answered, as SQL refuses an
unknown table -- and holds every column a join names to its table's
fields (`Flat::names`, `join_names`) and refuses one no table has, as SQL
refuses an unknown column -- a key with no table (`on cu.id = cid`), a
LEFT JOIN's `on` term the reading leaves out (`... and o.nosuch = 'x'`
was answered), an item -- and one with no table that another table has
naming it as the statement does (`` `cu.country` names `c`'s ``).
`tests/join.rs` evaluates each table's conditions through FennecQL, so
`fennec-bench`'s `tests/joins_sqlite.rs` holds 1 248 joins, `exists`, `not
exists`, `left join ... is null`, names with no table and aggregates and
`distinct` over pairs to SQLite's answer over rows with nulls in keys and
conditions: 1 008 answered as SQLite does, 240 refused, and the 240 probes
beside a list, answered at once, to the same rows -- the split pinned, on
CI's clippy job (`cargo test -p fennec-bench --test joins_sqlite`).

**What it costs.** The agent eval (`tools/agent-eval`, condition C, two
runs a task and model, B on main the same day): SQL's first try 60% ->
74-75% for haiku and 75% -> 91-94% for sonnet, within three 98% -> 98-100%
and 100%, refused statements 36 -> 7 over 160 runs. The browser module grew
70.6 KB, 29.2 KB gzip, 22.9 KB brotli (802.6, 329.9, 272.9), the test
builds 24.0 and 24.4 KB brotli -- the reviews' fixes 10.8 KB and 3.5 KB
brotli of it, the second's 6.8 and 2.5 (the probe made of the list and
when to read it 3.2 KB, every name held to its table, `required`'s stop
at the page), the third's 0.5 and nothing brotli: `join.rs` 18.0 KB
(`exists`'s `correlate`
2.5 of it), the parser's readings 24.1 (`create table` and `create index`
with their columns and keys about 11, `exists` 1.3, the select's
`distinct`, ordinals, distance and alias 3.7), `sqltypes` 1.9, the flat
rows and the metric in `query` and the engine 4.8, the refusals' text 6.9,
the standard library's instances 4.9. Slimmed from 79 KB and 25.3 KB
brotli: a copy of `Expr::clone` compiled into fennec-ql (2.2 KB, out of the
core now, `Expr::duplicate`), `str::contains` and `rsplit` in the type map
and the operator class bringing the two-way searcher, a slice's panic and
its `char` formatting (about 5.5 KB), the SQL readings out of line (0.4 KB
brotli), the
refusals' text shortened and their `String`s made `&'static str`.
Natively `fennec-server` grew 113 KB, its code 95.2 (fennec-ql 103.6 ->
179.0 at `opt-level = 3`), `fennec-shard` 97 KB, the shell 113 (97 under
`--profile cli`). The parse, ABBA against main, ns, medians of eight runs
a side each run at a load under 3: a row by id 790.6 -> 830.3 (+5.0%), a
filtered `get` 2 369 -> 2 488 (+5.0%), a `set` 1 179 -> 1 189 (+0.9%), a
128-dim `near` 3 606 -> 3 634 (+0.8%), a `put` of 1 000 128-dim rows 2.528
-> 2.525 ms (-0.2%) -- where the code lands: with every function aligned
to 64 bytes on both sides (`-C llvm-args=-align-all-functions=6`) the same
source measured 795.2 -> 791.9 by id (-0.4%), 2 370 -> 2 349 (-0.9%), 1 173
-> 1 177 (+0.3%), 3 623 -> 3 568 (-1.5%) and 2.526 -> 2.519 ms (-0.3%);
the build before the review's fixes measured +1.2% and +1.0% by id and
filtered in its own layout. A first pass was 8% slower by id: every `get`, `set` and `del`
copied its collection's name into the tables an `exists` reads its row
around in (`Around::At` keeps the token's place now), and `scalar_ahead`,
asked of every comparison, was a call. 511 parentheses parse in 995 KiB
of stack against main's 979 (`or_expr`'s and `and_expr`'s frames 16 bytes
larger each): a `format!` in `cmp_expr`'s refusal of a distance outside
`order by` was 16 KiB more, one text now (`DISTANCE`).

## Count parameters

**`limit`, `offset` and `top` take a parameter, read where the plan reads
them** (`query::Count`). A page's size and place are a request's data:
written into the text, each page of a list was a text of its own, which
`/query`'s parse cache (16 shards of 64, a shard emptied when full) kept
apart and parsed again once it was gone, and every client spliced a number
into its statement -- `/_stats/statements` already counted them as one
shape, a literal being a place there. `limit $n`, `offset $n` and a
facet's `top $n` parse to `Count::Param` wherever a number goes: a `get`, a
`lookup` level, an inner `get`, a `set`'s or a `del`'s pick, and a REST
`facet=` (which has no parameters, and says which one it lacks).
`answered_select` holds each to what the number written is held to
(`Select::check_counts`, the inner `get`s' where each is answered,
`page_of` a pick's): a value `as_count` refuses -- a whole number from 0,
a float with no fraction its number, past `usize` every row, where `as`
wrapped a literal `limit 5000000000` to 705 million in the browser module
-- is refused by `count_refused` in the words the literal is (the parser
reads one as an operand, so `limit -1` and `limit "x"` say what a
parameter of -1 or "x" says), and a `top` outside 1 to 10 000 by
`top_in_range`, `check()`'s for a number written. The plan reads it where
it reads the number (`Count::at`): the `@sorted` walk's page, the scan's
stop, `near`'s and `match`'s 10 000 rows, a lookup's page a parent, a
facet's `top`, `explain`'s steps, `query_json`'s page. Bound into a copy of
the select instead, a read by id took 472 -> 710 ns in process; read in
place, 454-468 ns either way (in turns against main), an ordered page 2.63
-> 2.68 us and a hashed filter's 12.8 -> 13.0 us, where a neutral
`#[inline(never)]` on main moved the page 0.8% and taking the check out
left the gap: the code's layout. What judges a statement by its shape
reads the parameter as there -- `count` and an aggregate refuse `offset $1`
as they refuse `offset 5`, a sync replica refuses a synced `limit $1` --
and the pin reads the number it is handed (`Count::of`). Every builder
sends `limit`, `offset` and `top` as parameters (the golden file's texts
changed with them), so do the stores and the studio's grid, and Grafana's
`limit $__max_data_points` is now a statement the server takes. Over 5 000
rows paged at random in sizes of 10 to 50, 1 050 texts written in, the
parse cache held 84.2-84.7% of them, its phase 1.36-1.88 us a request,
against 100% and 0.73-0.84 us as parameters (`make pages-bench`, a
`--features timing` server); pages a second (105-109k at eight clients on
a quiet machine) and `make wasm-speed` stayed within the runs' noise
either way, a parse being a microsecond of a 33 us round trip. The
browser module grew 2 331 bytes, 947 gzip, 791 brotli: the parser's
`count`, the check and its refusal, `top_in_range`.

## Inner gets

**`in (get ...)` is answered before the query, as the list it is.**
`Expr::InSelect` holds an inner `Select`; `Database::answered` (from
`query`, `query_json` and `execute_inner`, and `explain` inside its
plan) runs each inner `get` once, with the statement's parameters, and puts an
`Expr::In` of its one column's values in its place -- so every place a
filter goes, and the planner's "every element resolves or the list goes
to the scan" rule, take it unchanged, and `eval` never meets one. Only
a statement holding one, reading a collection whose rows expire, or
calling `now()` over a collection with an ordered index (`fold_now`,
above) is cloned: the rest costs a look at its filters. A null is left out of the
list; past `MAX_SUBQUERY_VALUES` (100 000) distinct values it is a query
error: an inner `get` with no `limit`, `group` or ranking lists its
matches' ids, as `count` does, and reads its column, the values told
apart by their encodings only once there are more than the bound
(`distinct_column`) -- cut at that many rows, 125 000 `buy` events of
937 users were refused, and told apart as they came, a bucket of 1 967
went 0.15 -> 0.36 ms; one with them is read one row past the bound; past
`MAX_SUBQUERY_DEPTH` (4) refused in the parser and the engine. The inner
`get` selects one column -- a field or one aggregate -- and no `lookup`,
`count` or `require` (`Select::check_subquery`). A scoped token's `scoped()` holds
each inner `get` to the read rules as any `get` (`Scope::inner`), and a
rule's filter may hold one too, answered before each statement as the
list it is (`Scope::bound`, above). A long `in` list is looked up, not walked (`Test::InSet`,
`IN_SET_AT` = 8, over an int, a text, a timestamp or a boolean typed
field and `id`, the values' encodings in a `HashIndex` made at the first
row tested): 2 000 codes over a scan of 200 000 rows took 2 302 ms
compared a value at a time and take 16. One country's 2 000 customers'
orders by `@hash` take 0.83 ms against 0.72 written out; customers with
a rare order 0.20 against `lookup ... required`'s 0.44 (`make
subquery-bench`). A subscription's REST shape cannot hold one, and the
JS builder's `{ f: { in: query } }` renders one with the outer query's
parameters.

## List parameters

**`in $n` is the list written out with a parameter a value.** A team
building over HTTP named its main friction: `get products where id in $1`
handed `[[1, 2]]` was refused (`[` expected), so a cart's ids, known at
run time, were `in [$1, $2, ...]`, one parameter an element, which every
client generated -- a text of its own for every length, which the parse
cache (`POST /query`, 16 shards of 64) kept one of a length and, past
`PARSED_LONGEST` (1 024 bytes, some 150 ids), parsed again at every
request. `x in $1`, `$1` holding a JSON array, parses to
`Expr::InParam(x, 0)` (`Parser::in_list`, `list_param`), the last of
`Expr`'s variants so none before it was renumbered; `not in $1` is `not`
over it. A definition's `$ids` reads as its place, and the canonical tree
writes it as node 15. SQL's list parameter is PostgreSQL's: `x = ANY($1)`
reads as `x in $1`, `x <> ALL($1)` (and `!=`) as `not (x in $1)`
(`Parser::any_param`, out of line and cold, asked only where a word of
three letters follows `=` or `<>`, and taking only `any`/`all`, `(`, one
parameter and `)`), while `x IN ($1)` stays SQL's list of one value, as
SQL reads it. Any other `any(` or `all(` after a comparison -- a list
written out, an inner `get`, a field, `> any`, `= all` -- is refused
naming the `in` to write (`ANY`), its word not read, the shell told where
the text ended inside the parentheses (`cut`): it was a call of a
function no build has, refused only as it ran. The builders still write `in [$1, $2, ...]`, the golden
file unchanged.

It is planned as the list written out. Every planner that read an
`Expr::In`'s items reads them through `Expr::in_list`, which yields an `in
[$1, $2]`'s literals and parameters or an `in $n`'s list alike
(`InItems`): `conjunct_in_sets` (the id, hash and `@unique` indexes'
buckets, and the pin), `Filter::bind` (`Test::In`, `Test::InSet` past
`IN_SET_AT`, a `nocase` field's fold), the composite index's runs, a bare
`in`'s exactness and `names_first`; `explain` names what they took.
`tests/in_param.rs` holds rows and plans equal for both forms over the id,
a `@unique`, a `@hash`, a `nocase` and an `@sorted(g, n)` field, a list of
1 000, an empty one, one holding a null (kept, as `in [$1]` handed null
keeps it, where an inner `get`'s list leaves nulls out), `not in`, a `set`
and a `del` that pick their rows, a select item and a `group`. Anywhere
else -- a select list's item, an `order` key, a stream's test of a written
row, a held write's look among the rows written -- `eval` reads the list
(`param_list`), comparing as `Expr::In` does (`in_same`, the fold
included). **Every `in $n` is held to its list before the plan**
(`Statement::lists_held`, asked in `Database::answered`, which every
statement handed parameters passes -- a `/batch`'s line, a call's body
with its arguments, the module's -- walking its filter, select list,
`order`, `lookup` levels, facets, a picked write's keys and an inner
`get`'s at any depth). Left to the filter's `eval` at the first row it
tested, as what `Filter` cannot bind is, a list of none or past the
bound was answered over an empty collection, under `id = 7 and n in $1`
finding no 7, or by a held `del` finding nothing, and refused once a row
was tested: an answer that hung on the data. A statement handed no
parameters walks nothing; one handed some walks its expressions once,
SIZE_AND_TIME. `has_subquery` is the inner `get`'s alone,
so a poll's tag (`etag`) and the pin, which ask whether another collection
is read, see an `in $n` as reading none. **Put in its place first, as an
inner `get`'s list is, it cost a copy of the statement:** 10 ids by `id`
took 1 353 ns in process against 1 052 written out, and `query_json`, a
server's plain `get`, 397 against 149 -- the `Select`'s clone alone 157;
read where it stands, 1 045 and 100 against 1 070 and 151 (a load of 3
to 10, the figures indicative, the bench's below). `ExactSizeIterator`
for the items was 1.1 KB of the browser module (its `len`'s `assert_eq!`
brought `Debug` for `usize` and `Option<usize>`). The tree holds the place, never
the values: the parse cache's statement serves every list
(`the_parsed_tree_holds_no_value`).

Its numbers are read as written. `expr_needs` names an `in $n`'s
parameter whatever its left side, a json field's or not (`Exactly`):
read the quick way, `[16777217]` is a vector's `f32`s, 16777216, another
row's id, and `[0.1]` 0.100000001. The server reads the body again as
written (`api::exactly`, `/batch`'s lines and `/_call` alike); the
browser module, handed the array apart as a vector (`vectorsApart`),
answers `exact` and the page sends that parameter as JSON (`#run`'s
second call) -- once a text: the page keeps the places the module named
for each (`#asked`, 256 texts, cleared when full) and sends them as JSON
at once after, looked up only where a list of numbers is among the
parameters, so a run with none pays nothing, and a place it names beside
them is added. COST_NUMBERS; a `Value::Vector` handed to `Database::query` is refused
(`refuse_inexact`, its words naming `in $n`). A call's argument is named
where its parameter is a list (`$ids [int]`) as where it keeps numbers, and
one handed as a parameter (`call f($1)`) is the parameter's -- `exact_args`
asked only the literals written in before, so `call f($1)` handed a json
field's list read it as `f32`s. A writing body's `in $ids` over `id` or
an indexed field narrows its rows (`narrowed`), as `in [$a, $b]` does:
the holes a body is judged with hand it no list.

The list is held to the inner `get`'s bound: past `MAX_SUBQUERY_VALUES`
(100 000) `param_list` refuses it, never cut, and a parameter holding no
list is refused naming the form (`` `in $1` takes a list, and $1 is of
type int: `= $1` compares one value ``). A rule's `in $jwt.rooms` is still
written `in [$k]` before it is parsed and spread by `bind` (`usable` and
`kind_held` unchanged), and SQL's `= any($jwt.rooms)`, which the parser
reads as `in $k`, is made that same `in [$k]` as the rule is read
(`any_as_in`, `<> all` its `not`): one rule however it is spelled, held
by every check, where the parser's own node had none of them, and
`claims_held` refuses one should it come. A rule writing `$1` is refused
at startup as any is; `bind_held` and `Reach::of` take an `in $n` as the
list it holds, and the held writes' `bind` refuses one holding none
(`holdable`), whose every look's test was an error, a `del` finding
nothing held its whole wait.

**Grammar epoch 2.** `x = any($1)` parsed before, as a call of `any`,
which no build has and every run refused, and a definition could store
one (a body's functions are named as it runs): read as `x in $1` now, its
digest is another, so `GRAMMAR_EPOCH` moved and such a definition is
marked broken, to be defined again; one holding any other `any(...)`
after a comparison, refused as it ran, is refused as it is read now, and
marked broken alike. The corpus (`make fennecql-corpus`),
9 726 texts: none of main's reads otherwise; the six that do are this
change's own `= any`/`<> all` texts, and the `in $n` texts parse where
they were refused (Grafana's tests bind `in $syms` themselves, into `[$1,
$2]`). Rejected: a list parameter spread inside `in [$1]`, as a rule's
claim is -- `tags in [$1]` handed a list compares the list whole, and
would have answered otherwise; and a call standing for the list (`in
[spread($1)]`), which every walk would have special-cased by its name and
the canonical tree written as a call.

The browser module grew 2 634 bytes, 1 255 gzip, 814 brotli (811.4,
333.4, 275.6 KB; the test builds without the indexes and with no feature
2.6 KB each): the parser's 0.9 KB (`any_param` 349, `list_param` 181,
`cmp_expr` inlined into `not_expr` 284), `param_list` and its refusals
281, `eval`'s arm 244, `in_list` and its items' `next` 413, `Filter::bind`
150. Handing the operand into `any_param` and back out was 85 bytes more.
`make lists-bench` (100 000 rows, a load of 1.7 at the start, the two
forms in turns, twice): in process, 10 ids by `id` take 0.92-0.96 us
against 0.96 written out, by a `@hash` field 1.96 against 1.83-1.88; 1 000
ids 40.5 against 46.8 us by `id` and 105.6-105.8 against 112.2-112.5 by
the field, the list no `Param` a value read. Over HTTP, one client's p50
and eight clients' requests a second: 10 ids by `id` 26 us and 129.1-129.5k
against 25 us and 129.3-129.8k, by the field 28 us and 121.6-122.6k
against 27-28 us and 120.7-123.7k -- the body read twice for a list of
numbers (the quick way, then as written: 0.5 us) paid for by the shorter
text; 1 000 ids by `id` 179 us and 22.3-24.2k against 287-289 us and
14.5-15.9k, by the field 262-266 us and 14.6-16.0k against 377-383 us and
10.7-11.7k, the text written out past the parse cache's 1 024 bytes and
parsed at every request. The written-out form itself, base against branch
in process, in turns: 10 ids by `id` 1 167 ns either way, by the field
2 208-2 291 against 2 083-2 167, 1 000 by `id` 59.8-59.9 against
59.1-59.3 us -- its plans now read their items through `in_list`, at no
cost.

## alter collection

**`alter collection` rewrites no document; positions are not places.** A
document is its values in field order, so a field added goes last and a
document written before it ends before its place -- `skip_field` skips
nothing at the end, and the read is `null` (`Schema::read_doc`,
`Store::read_field`, `read_fields`); a rename is the schema alone; a drop
leaves the place in `Schema::dropped`, written as a nameless field of type
tag 12, skipped on read, written as `null`, refused by name, its index
taken off (`Collection::take_indexes`), until `compact` writes each
document without it (`strip_dropped`, the kept values' bytes copied: 95
ms over a million rows in memory against 56 for a compact with none). A
field's position -- what `field_pos` gives and every reader passes -- is
its index in `fields`; its place in a payload is past the dropped ones
before it (`Schema::place`). The store keeps the dropped places
(`Store::set_dropped`, set wherever the schema changes) and works a
place out per read for `read_field`; `read_fields` takes places, worked
out once a query by its callers (`Filter::new`, the aggregate, the
rebuild): worked out a row, the compare took a million-row scan 21.2 ->
21.8 ms. Natively the end is told inside the skip's own tag lookup
(`skip::<true>`); asked in the loop, a scan reading two fields three apart
took 39.1 -> 40.4 ms; the browser module asks the compare, a second copy of
the skip being 1.1 KB of it. After both, five scans of a million rows went
from 1.7% slower to 1.7% faster (21.2 -> 21.6 ms, 36.8 -> 36.2, 39.1 ->
39.3), as the same build with `read_fields` written as before did (+1.3%
to +3.1%) -- where the code lands, not what it does -- and the browser's
1.70 -> 1.71 ms. An alter is record kind 12 -- `[change][field]
[new name][schema]` (`FieldChange`) -- not a kind-5 record with a change
byte, which a binary from before would read as an index added; it counts
one write, rides in a block, and is undone as an index built is
(`Undo::Altered`: the schema before, a dropped field's indexes to put
back). The load keeps a graph restored before it under its field's new
name; a replica applies it through `Collection::alter_fields`, as the
write path does, checking nothing; `changes_in` reads the documents after
it by the schema it made. A field added under a dropped one's name is a new
field: the old values stay in the dropped place. A type change is refused:
it would rewrite every document under the write lock. A compact over a
collection holding a dropped place holds the lock (`compact_online`): the
copies made beside the database are the records as they stand. An add, a rename and a drop take 0.025,
0.021 and 0.004 ms over a million rows; the browser module grew 12.2 KB,
4.0 KB brotli -- the reading side, which a file from a server needs, and
the writing side, which a page's own database does.

## json fields and paths

**A `json` field holds any value, and a path is a name.**
`DataType::Json` (type tag 14) holds an object (`Value::Object`, value tag
13 -- 11 and 12 being a collation's and a dropped place's, in a number space
the two share), a list or a scalar; a typed field still refuses an object.
An object's members are sorted and unique (`Value::object`: a key twice is
refused, never one of them kept), so a member is found by binary search, two
equal objects encode alike and a hash index files them as one. A field's
name holds no dot (`Schema::new`), so a path -- `meta.source.rank` -- is an
`Expr::Field` whose name `Schema::path_of` splits, and every place a name
went takes it unchanged: `select`'s columns, `order`'s keys, `Fields`, the
planner's equalities and ranges; an `Expr::Path` would have been a variant
for every match over an expression in six crates. A path that leads nowhere
is `null`. `Filter` binds one as a field (`Slot::Path`) and reads it off the
row's bytes (`Store::read_paths`, `codec::decode_path_into`: the members
before the key passed over undecoded, a text into the text its slot held);
the reading is out of line and `matches` inlined natively, since left to the
compiler a scan by a text field went 3.05 -> 3.17 ms with no path in the
filter at all. A path's index is a field of its own beside the fields
(`Schema::paths`), written inside its json field's schema entry, and kept up
from `Document::at`; `Collection::fit_paths` makes the structures match the
schema after an alter or an undo, unbuilt, rather than move them by name.
`@hash` files a whole float as the int it equals (`hash_key`), and a lookup
over json takes only what a bucket answers exactly (`lookup_key`: not an int
past 2^53, not a timestamp); `@sorted` keys numbers as `f64`s and text by its
bytes, numbers first, and holds apart what it cannot order -- a boolean, a
list, an object, an int past 2^53 -- answering nothing while it holds one
(`SortedIndex::answers`); `tests/json.rs` holds both to a twin collection
row for row. Nesting is 64 levels and a path 64 keys, refused past either
(`MAX_JSON_DEPTH`, `MAX_PATH_KEYS`), the JSON reader's recursion bounded
with them. A json field keeps a number as written, and a list of numbers
alone is read the quick way, into a vector's `f32`s, by every reader that
has no schema -- the lexer (`Tok::Vector`), a query's parameters, the
browser module's vectors handed apart -- so a vector given to one, or
compared with a path into one, is refused (`json_value`,
`Database::refuse_inexact`) and the caller reads that part again as
written: `Database::exactly` names it (the text, or parameters by place),
walking every place a statement holds a value, each collection by its
own schema (`statement_needs`, `Known`) -- an inner `get` at any depth, a
`lookup` level, a disjunctive facet's filter, `having`, `group`, each
item of the list, an `order` key, a `set`'s values, a comparison under
any call, `case`'s among them. It walked the statement's own filter, list
and `lookup` levels alone, so a point in an inner `get` stayed `f32`s
that `distance` refused as no point (the dispatch example's claim of the
nearest); `exactly_for`, which knows one collection, reads another's
lists as written, slower for a vector and never wrong. Then
`fennec_ql::parse_for` / `parse_exact`, `json::parse_params_exact`,
`/query`, `/batch`, a REST `where=`, and the module, which answers
`"exact": [places]` for `run` to send those as JSON. A statement with no
list of numbers costs the walk of its literals, one into a collection with
no json field a look at its fields: 0.03 us for a `put` of 1 000 128-dim
rows, 14 us with a json field beside the vector, against its 9.0 ms, which
parsed and ran as before natively and in the browser module (`make
wasm-speed`'s `put`). A typed array handed to the module goes as the
numbers it holds, each an `f32` exactly. An object, an HTTP body
(`parse_documents_json`) and an imported `jsonb` cell (`json::parse_json`)
are read exact from the start. Over 100 000 documents a path
filter scans in 5.7 ms against a text field's 4.2, 0.025 ms through `@hash`;
a scan with no json field moved by noise alone. The browser module grew
29.8 KB, 9.6 KB brotli -- 3.7 KB of it reading a list as written, a second
lexer 2.9 KB of that until it took its flag at run time there
(`lex_with`); the standard library's sort for the members (4.1 KB) and a
walk generic over its callback (3.8 KB) were taken out on the way.

## The shell

**The shell runs a statement once nothing after it can go on with it**
(`fennec_ql::reading`, fennec-cli's `Reader`). A statement has no end of its
own -- `del notes` is whole, and so is it with `where id = 3` on the line
under it -- and the shell ran every line whose brackets closed, so that
`del` deleted every row before the `where` was refused. `reading` gives
the statements a `;` or the next statement ended (`whole`) and what is
left: a statement whole so far (`Rest::Last`), or one refused so far
(`Refusal`), at the text's end (`end`) or at what begins no statement
(`begins`: an unknown command, `wher`, a clause past a `;`), which refuses
the statement before it too, `;` or no `;`, since it may have meant to go
on with it -- `del notes; where id = 3` deletes nothing, as `-c` refuses
it. Piped, the next line says whether a refusal stands
(`Refusal::stands`: the same words, not at the end, and where the earlier
one was not at the end, the same `place` -- which a reader that lets go
of the text before shifts, where the words' `position` it cannot), so a
script reads as `-c` and `POST /query` read it, and the text refused ends
where it did (`Held::Refused`'s length: from the latest line instead, the
lines read inside a string were dropped and the last run alone).
`a_script_piped_writes_what_minus_c_writes` holds that over generated
scripts (`FENNEC_SHELL_CASES` for more). Typed, a read or a schema change
on one line runs at Enter, and a write or a read over lines waits for the
next line: an empty one runs it, a `.` command leaves it waiting, as
`psql`'s meta-commands leave its buffer, and `.quit` or Ctrl-D leaves it
unrun; with no next line to ask, a refusal stands where `limit 1` after
it leaves it standing (`stands_alone`). A paste is read as a pipe, its
next line waiting already (`pending`, a `poll` of 40 ms: iTerm2 paces a
paste about 15 ms apart), an empty line waiting as the line before was
judged is white space (`waiting`), and in a script or a paste a `.`
command -- `.quit` too -- waits behind the statement before it
(`deferred`), as does a statement a `;` ends at a line's end (`take`'s
`hold`): run there, `del notes;` then `where id = 3` deleted every row,
and a `.quit` between `del notes` and its `where` ran the `del` alone.
Bracketed paste is not read: it needs the terminal out of its line mode. A new
statement form or lexer syntax owes this: a place in fennec-cli's `kind`
(every statement named, so a new write cannot run at Enter by default); a
mark in `Parser::cut` where the end of the text decides a reading the
parser then backtracks over (the SQL order's list before its `from`,
`alter field f` before its `@`) -- a check at the end of a list that a
next line could satisfy belongs there too; `lexer::Unclosed` taught any
new string, comment or bracket; and a line in the corpus of
`a_statement_written_over_lines_reads_as_one`, which fails where a
statement written over lines would be refused piped. A `;` is a token
(`Tok::Semi`) that ends a statement in the parser, where it was white
space and `del notes; where id = 3` one statement; a query string's
`select=`, `group=` and `facet=` take one after their list as before.
`lexer::Unclosed` keeps a document of many lines from being parsed again
at each, and `spans` and `reading` place statements in bytes: in
characters, a synced text's second write was sliced a byte early per
letter outside ASCII before it, and the server refused its first. The
shell's code grew 8.6 KB (`__text`, aarch64-apple-darwin; 9.3 in
`make small`): the reader, `reading`, and the lexer out of line once it
had a second caller there (+0.4 KB net) -- `meta` called from two places
went out of line, 2.2 KB, and is called from one. The browser module grew
242 bytes, 52 gzip (brotli within noise); parsing moved by noise alone.

## Declared schemas

**A schema declared in code is checked at every open, in the engine.**
Two front ends compile to one description (`fennec_core::declared`,
versioned JSON: `{"format": 1, "collections": [...] | "fennecql": "...",
"migrations": [...]}`): tables written Drizzle 1.0's way in TypeScript
(`web/schema.js`, its own entry point `@fennecdb/web/schema`, so a page that
declares nothing loads none of it), and FennecQL `create collection` text --
a `schema.fennecql` -- which every SDK hands over with no builder of its own
(`fennec_ql::schema_text`). `declared::plan` compares a database's schemas
with it: what only adds (a collection, a field, an index where there is
none, a path index) comes back as statements; a field the code lacks
(dropped, or renamed? never guessed), a type, a collation or `required`
changed, an index changed or taken off, a required field new to a
collection that exists, are refusals with their resolution; a collection
the code does not declare is left alone. `Mode::Follow` is for a database
another owns -- a replica's server, a client without the right -- where
the code's must be there and the rest is the owner's. `fennec_abi::schema`
runs the migrations not yet recorded (known by their place in the list,
held to their text: one edited after it ran, or a database holding more
than the code lists, is refused), records each in `_migrations (n, text,
at)`, plans and applies the additions, all one block, put back on any
refusal or error; a database holding none of its own collections is made
from the description and records its migrations without running them. A
plan runs the pending migrations in a block put back. `rebuild` is the
migration that changes an index or a collation: the field added again as
declared, `set` from the old, the old dropped, the new renamed. The module
exports `fennec_schema` (feature `schema`, on by default, `make wasm
SCHEMA=0` without; reading the collections' JSON was 8 KB of it, so the
module reads FennecQL alone), the native library `fennec_schema` (plan,
apply, follow, describe), the server `GET /_schema`, `POST /_schema/plan`
(`?mode=follow` for a scoped token) and `/_schema/apply` (the server's
token, under the write lock). `Fennec.open`, `restore` and `openFile`
apply; `sync` follows (the server owns a replica's schema); `connect`
follows, or applies with `migrate: true`; Python's, Go's and .NET's
`schema(...)`, Swift's `Fennec.schema(...)`, Kotlin's and Dart's
`schema(...)` (JNI's `schema`, Dart's worker op) take the text. A `load`
adds an image's collections to what a database holds, so one that holds
only what its schema made, and nothing since, is emptied before a restore
or an openFile loads into it (`untouched`). `integrations/schema-golden.json`
holds declarations, texts and plans (`make schema-golden` writes it
through the module; `fennec-abi`'s tests run every case natively, the JSON
and the FennecQL form of each). The check costs an open of 10 collections
0.09 ms in Node and the module 7.5 KB brotli (8.1 until the plan's two
modes were one const generic and its messages one helper each); `fennec.js` grew 1.8 KB
brotli for the hooks and the check's errors, and `schema.js` is 5.9. `fennec types` reads a
`.fennecql` as it reads a database, and writes Python, Go, C#, Swift,
Kotlin and Dart rows (`--lang`; Kotlin's and Dart's tests compile the
golden file and read a row into it), the tables as code (`--schema`) and the schema as FennecQL
(`--fennecql`); `integrations/types-golden/` holds each for one schema, with the
named statements of [named-statements.md](named-statements.md#what-is-built) typed
beside its collections (slice 6).
