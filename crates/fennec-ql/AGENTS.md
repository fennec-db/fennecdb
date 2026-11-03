# AGENTS.md

FennecQL's invariants -- SQL's spellings and joins, parameters, inner `get`s, `alter`, json paths, the shell, declared schemas and named statements -- for code in `crates/fennec-ql/` and the statements fennec-core runs for it. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### FennecQL and schemas

**[SQL's spellings are FennecQL, read by the one
parser](../../docs/design/fennecql.md#sql-spellings)** into the same statements:
no mode, no second front end, no pg wire. What has no FennecQL meaning is
refused naming the FennecQL to write, without consuming its word; `"name"`
where SQL reads a column (`Parser::quoted`) and a regex-like `~`
(`regex_like`) are refused, `<>` stays `!=`.

**[A JOIN on a key is a `lookup` answered flat, or one table's `get` with
the other's rows a filter](../../docs/design/fennecql.md#sql-joins-subqueries-and-schema)**
(`join::relate`). The flat rows are `Select::flat`, set by SQL alone and
rendered by `ResultSet::answered` after a token's masks; a join that would
answer otherwise is refused. Every table it names is held to a token's read
rules (`Scope::select`) and every name to its table's fields (`Flat::names`,
a `*`'s to what the rules show, `Flat::shown`), read or not; a join found by
`id` whose list passes 100 000 values is read as the `lookup`'s probe
(`Flat::probe`, `Select::probed`). `create table` maps types by
`fennec_ql::sqltypes`, as `fennec import` does; pgvector's `<->` is `near`
held to the index's metric (`Near::metric`); SQL's `order by 1` names the
list's first item.

**[`limit`, `offset` and `top` take a parameter, read where the plan reads
them](../../docs/design/fennecql.md#count-parameters)** (`Count::at`); a value is
refused as its literal is (`Select::check_counts`).

**[`in (get ...)` is answered before the query, as the list it
is.](../../docs/design/fennecql.md#inner-gets)** `Database::answered` puts an
`Expr::In` in its place on every path, so `eval` never meets one.
[`in $n`](../../docs/design/fennecql.md#list-parameters) is its list written
out (`Expr::in_list`), held to it before the plan (`Statement::lists_held`)
and read exactly (`Database::exactly`).

**[`alter collection` rewrites no document; positions are not
places.](../../docs/design/fennecql.md#alter-collection)** A drop keeps its place
until `compact`; `read_fields` takes places (`Schema::place`), never
positions, and wherever the schema changes the store is told its dropped
places (`Store::set_dropped`).

**[A `json` field holds any value, and a path is a
name.](../../docs/design/fennecql.md#json-fields-and-paths)** A field name holds
no dot, so a path is an `Expr::Field`; a list of numbers read with no
schema is `f32`s, so `Database::exactly` must walk every place a statement
holds a value.

**[The shell runs a statement once nothing after it can go on with
it](../../docs/design/fennecql.md#the-shell)** (`fennec_ql::reading`). A new
statement form or lexer syntax owes a place in fennec-cli's `kind`, a mark in
`Parser::cut` wherever the text's end decides a reading the parser
backtracks over, `lexer::Unclosed` taught any new string, comment or
bracket, and a line in `a_statement_written_over_lines_reads_as_one`'s
corpus.

**[A schema declared in code is checked at every open, in the
engine.](../../docs/design/fennecql.md#declared-schemas)** `declared::plan`
plans additions alone, never guessing a rename; `make schema-golden`
writes `integrations/schema-golden.json`.

**[Named statements are records of their database, called with their own
text's authority.](../../docs/design/named-statements.md#what-is-built)**
`create statement`, `drop statement` and `call` parse into
`fennec_core::named`'s trees, boxed so a `Statement` stays a `get`'s size.
A definition is record kind 13 (`engine/stored.rs`), one write `writes_in`
counts, alone or in a block (`Undo::Statements`), kept by every image,
`Beside` and build, applied by a replica and passed over by `/_changes`.
With the feature an open reads its text back (`fennec_ql::install`) and
holds its digest. A schema change's commit holds its `depends`: refused
naming it, or marked broken (`Database::internally`, an apply, an open),
never failing; the mark is derived, never stored. A replacement keeps the
old tree's contract; a writing body reads through an index or says `scans`
(`body_checked`). Whether a call writes is asked under the lock it then
runs under, never of its statement or apart (`call_reads`,
`Database::reads`): a writing body runs in its caller's block under the
write lock (`Database::call`), a reading one over `&Database` (`read_call`,
pinned by `pin_call`), refusing a write and taking no write lock, block or
counter, so replicas, read-only servers, marked reads and cross-tenant
queries answer it. A call is judged statement, claims, arguments, `check`,
broken (`callee`), answers its last statement and is put back past
`--call-max-rows`, its rows its thread's (`call::LEFT`). A scoped token
calls what a `call` line grants (else a missing one's 404; `* call`
refused), as itself (`as` 403), alone in its `/batch`, held to
`--call-rate`; `judge_call` holds the body to `append-only` under the lock,
and `run_as`, the one statement so, runs it outside `within()`, unmasked,
its refusals told (`told_call`). `POST /_call/<name>` is `call <name> $1`
run as `/query` runs it: a writing one meets a read-only server's 403 after
its judgment (at `/batch` too), a freeze's or lapsed lease's refusal before
it unless this copy reads it. A call is counted, logged and traced as that
shape (`db.stored_procedure.name`), the router's route `call`, never its
name. A sync replica refuses them (`NAMED`); the router reads a tenant's
with the grammar; readers upgrade before a primary defines one
(RELEASING.md, `make upgrade-check`). `$name` is a definition's alone;
`call` and `statements` reserve no word (`NAMED_COMMANDS`, never
`COMMANDS`). The `statements` feature (fennec-core's, fennec-ql's) is
outside `full`, named by the server, the shell and the native library; the
module refuses in its own words, a definition's own path `#[cold]`.
`Definition::canonical` writes a field only where it holds something, an
operator by a byte of its own; reading a stored text otherwise raises
`GRAMMAR_EPOCH`; `make fennecql-corpus` holds the repository's FennecQL
texts to main's trees.

**[A write-bound statement runs in the block of the write that woke it,
once a row.](../../docs/design/named-statements.md#write-bound-statements-f2)**
`create statement <n> after insert|set|del <c> [where ...] as ...` is kind
13's op 3, refused by a binary from before, its collection and events
outside the text, read into `Collection::bound`, a mask every change keeps
(`Database::rebind`): an unbound collection pays one byte's look a writing
statement, and `run_one` a length. A bound write asks each row the
conditions as it is written (`Database::bind`, `Woken::keep`), keeping a
passing row's pair alone, a write over an expired row an `insert`;
`run_one` alone runs them, after the statement's rows, hooks and `require`
and before the block lands, rows then names, one `now()`; past `MAX_WOKEN`
rows or `--call-max-rows` (fresh a statement, a call's own in a call) a
write is refused, never cut, its block put back and the binding noted
(`Database::woken`). Depth one: its writes wake none, a binding of its own
collection or a chain is refused at definition, one bound to or writing
a `_` collection by its text (`declared::own`), and none is called. A
broken or unread one, or a build without the feature, refuses the writes it
binds. `Database::unbound` (implied by `internally` and a binding's runs)
wakes and refuses nothing: `--follow`'s mirror, `fennec import`, a declared
migration and the sync's apply run under it, and a new internal writer owes
it; an apply, a restore, a compact and the sweep never reach `run_one`. A
scoped token wakes only what a `wake` line grants it (403, never a skipped
binding, `Hook::waking`, `judge_call`), told its status and `reason` alone
(`told_bound`); every other answer -- the server's token's, `/batch`'s, the
native library's, the shell's -- names it (`binding`,
`Database::refused_by`). What they did is noted on the writing thread
(`Database::take_runs`), taken where `runs_noted` says, out of line, for its counts, slow line,
spans and audit, and described (`statements`, `GET /_schema`) to the
server's token alone. `@ttl` and `after del` refuse each other; a replacement
keeps collection, events and kinds of write (a `put` makes and sets).

**[A scheduled statement runs once a slot, by the primary that may write
its file.](../../docs/design/named-statements.md#scheduled-statements-f3)**
`every <interval> | at '<cron>' [in '<zone>']` is kind 13's op 4 and a
slot's mark op 5 (`SlotMark`), each one write, refused by a binary from
before. A run's writes and its mark are one block (`Database::run_slot`,
`mark_slot`), its slot asked again under the write lock (`Database::due`);
a run that wrote nothing leaves its mark pending (`Database::pending`),
the latest by order. Only a scheduler's thread runs one -- a file's
(`fennec_http::schedule::watch`) or a node's (`schedule::Node`) -- ending
through `stop_with`; never hold a schedule nothing runs: a database no
scheduler watches refuses a definition (`SCHEDULED_UNRUN`), a node running
none an install of an image holding one, and a server running none says
how many it holds (`schedule::unrun`). A refusal the statement makes marks
its slot, the state's leaves it due (`schedule::Held`), and only the
server's token calls one, marking nothing. A new path running a body owes
its mark in the block, and a new way a file reaches a node the scheduler's
word (`Node::note`, `Node::installed`, `Node::opened`).
