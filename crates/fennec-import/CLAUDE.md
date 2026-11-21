# CLAUDE.md

`--follow`'s and the SQLite reader's invariants, for code in `crates/fennec-import/` and the wire client it reads through, `crates/fennec-wire/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### Replication and archives

**[`--follow` confirms nothing that is not on disk.](../../docs/design/replication.md#follow)**
Every write is a put or delete by id; the mirror is a `fennec-server` thread,
its collections' only writer. A follower that stops cleanly ends its
stream as the protocol does and returns once the server let its slot go
(`WalStream::end`), every read and write of that wait held to what is left
of `follow::END_WAIT`, 3 s -- inside the server's `STOP_WAIT`, 5 s from the
shutdown's start, with a second to spare (asserted at compile time) -- or
cut by the shell's second ctrl-c; past it the connection is closed and the
follower says so (`Event::SlotHeld`, "may" where the connection ended
first): the walsender lets the slot go when it next reads, and a drop
until then meets `55006`. A signal is no failure: a wait it interrupts
goes on for what is left (`Conn::readable`, `Until`), since Linux restarts
no `recv` on a socket with a timeout. No drop waits (`Terminate` only
where the socket takes it). A failure waits for nothing: its stream is
closed and the error returned at once, the server's exit code stored
before its log line and its wait let go, a panic's too (`mirror::Ending`).
A wire message's length is held to
`proto::MAX_MESSAGE` before anything is allocated (`proto::zeroed`), and
the iteration count a server asks SCRAM for to `SCRAM_MAX_ITERATIONS`
(2^22) before PBKDF2 runs, refused naming the range and what brings a
server under it, the role's password set again under a lower
`scram_iterations`: the server, or whoever stands in for it, chooses it.
The tests need a live PostgreSQL and are `#[ignore]`d; CI's `postgres` job runs
them as `make import-test` does and fails unless every test `tests/pg.rs`
and both `tests/follow.rs` mark so passed: a new one goes in one of those
files.

### Imports

**[A SQLite file's lengths are held to the file before anything is
allocated, and its walk reaches a page once.](../../docs/design/crates.md#a-sqlite-files-lengths)**
A cell's payload to the file's length (`Walk::cells_apart`, again where
it is allocated, `Pager::held_to_file`), a page -- a b-tree's or an
overflow chain's -- to the file's pages and reached once by any path
(`Reached::reach`), a chain to the pages its payload needs
(`Pager::chain`), a page's cells apart (`Walk::cells_apart`) and each
read as it is yielded, so a walk holds a page and a row; a page's usable
part to SQLite's 480 bytes (`MIN_USABLE`); a table to SQLite's 32 767
columns (`MAX_COLUMNS`), since a row holds a value a declared column; and
an import's batch to `BATCH_BYTES` as read as well as `--batch` rows
(`load::run`), since a row may read far larger than it is stored. A
table's columns are read where SQLite puts them -- comments spaces, a
VIRTUAL generated column in no record and named in the summary
(`Source::left_out`), `""` kept as `column<N>`, a lacking column its
default as `valueFromExpr` reads it or refused (`default_of`) -- and a
file beside a non-empty `-wal` or a hot `-journal`, looked for beside the
file a link leads to, or one that is no file, is refused
(`check_side_files`). The file's text reaches a terminal escaped
(`shown`).
