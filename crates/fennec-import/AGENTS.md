# AGENTS.md

`--follow`'s and the SQLite reader's invariants, for code in `crates/fennec-import/` and the wire client it reads through, `crates/fennec-wire/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

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
`proto::MAX_MESSAGE` before anything is allocated (`proto::zeroed`), a
query's rows to `QUERY_ROWS` and a COPY line to `MAX_MESSAGE`, each byte
scanned once, and
the iteration count a server asks SCRAM for to `SCRAM_MIN_ITERATIONS`
(4 096) through `SCRAM_MAX_ITERATIONS` (2^22) before PBKDF2 runs, refused
naming the range and what brings a server within it, the role's password
set again under another `scram_iterations`: the server, or whoever stands
in for it, chooses it. Nothing waits on PostgreSQL under a lock:
the copy decides under the write lock, then dials and reads with none,
landing a batch a block (`load::run_locked`); the sign-in is held to
`connect_timeout`, a COPY's read wakes for the stop flag within
`client::LOOK`, the copy then left unfinished. A keepalive's end is
confirmed only on a proven stream (`WalStream::proven`), and a server's
words reach a log or a terminal only through `peer_text`.
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
a copy's batch and a follower's pending writes to `BATCH_BYTES` as well
as `--batch` rows (`load::run`, `follow::Mirror`), since a row may read
far larger than it is stored, a row counted once from sizes known before
its values are looked at (`Source::next_counted`: a SQLite row its record's
length and the defaults it lacks) and a number its field makes text as
it is made (`adapt`), never value by value where the source knows them.
A table's columns are read where SQLite puts them -- comments spaces, a
VIRTUAL generated column in no record and named in the summary
(`Source::left_out`), `""` kept as `column<N>`, a lacking column its
default as `valueFromExpr` reads it or refused (`default_of`) -- and a
file beside a non-empty `-wal` or a hot `-journal`, looked for beside the
file a link leads to and opened by that one path (`resolved`), or one
that is no file, is refused (`check_side_files`), the journal opened
never to wait on a FIFO (`side_opened`). The file's text reaches a
terminal escaped by `peer_text`, as a server's words do (`shown`).
