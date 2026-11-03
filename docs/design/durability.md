# Durability

The long form of the invariants [the root CLAUDE.md](../../CLAUDE.md), [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) and [crates/fennec-server/CLAUDE.md](../../crates/fennec-server/CLAUDE.md) list under *Durability*: a storage error stopping writes, buffered writes written behind the lock, the sync log, and the one shutdown (added after the move). Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Storage errors

**A storage error stops writes.** Once the sink refuses an append, a sync or a
rewrite, every later write and sync returns `Error::Io` until the file is
reopened (`Database::failure`); reads go on, from memory and from the
mapped file's pages. A page that cannot be read in is the process's end
(`SIGBUS`), not an error -- which is what `--no-mmap` is for on a disk that
fails reads or a network file system -- and a mapped file is only ever
replaced by rename: a copy over it in place took a server down. A failed `fsync` is
never retried -- the kernel may already have dropped the pages -- and
`fennec-server` under `--sync always` reports it (500) instead of the success it
had not yet sent. That fsync runs *outside* the exclusive lock: under it a
write only calls `Database::flush`, which hands back a `Durability` to run once
the lock is released, and `FileSink` writes the bytes there as well (a `write`
under the lock waited out concurrent fsyncs on macOS). The writes the lock
still makes -- the 1 MB buffer's once it is full, a handover's -- take the
sink's disk, which a durability holds through its fsync, so both are put
off while one does (`Sink::syncing`, a `try_lock`): the buffer up to
`WRITE_HELD` (8 MB), a handover to twice its bounds. A ledger's transfer
landing a handover waited out the syncer's `F_FULLFSYNC` of a quarter
second's writes, 52 ms, every request behind it. A durability whose bytes
an earlier fsync already covered runs none, which is the group commit: 268 ->
1 156 durable writes/s over eight clients. A failed one is reported back with
`Database::fail` so the engine stops taking writes. The syncer of `--sync
<ms>` flushes the same way, a database's and each of a node's tenants'
(`Tenants::sync_dirty`): the tenants' held their lock through the fsync, and
every read and write of one waited up to 27 ms a pass on macOS.

A durable write's block is read from the moment it lands, before its
fsync after the lock has run -- by design: on a primary a read, an SSE
stream, a live query and `Fennec-After` see it from then on, and only a
replica, sent what an fsync covered, and `/_changes`, which reads what an
fsync covered, wait for the disk. So is an answer kept with it: a
replayed `Idempotency-Key` was handed back the kept success at once,
before the first try's fsync or its wait for the streams -- a write a
power loss or a failover could take back from a client that held it as
made, and never sent it again (`http.md`, *Idempotency keys*). A request
answering a write another landed now waits for it as that write's answer
does: it asks `Database::durability` -- `flush` on `&self`, under the read
lock alone if that is what it holds, clearing nothing -- and runs it after
its lock. The sink's group commit makes it free where an fsync covered the
bytes and holds it at the disk where one runs; a failed fsync is its
answer. On `&self` the engine notes no error: one the sink gives as the
durability is asked, as one the durability gives, is the caller's to hand
to `Database::fail` once its lock is let go (fennec-http's
`storage_failed`), so the writes stop and the fsync is never tried again.
A sink with the trait's default `flush` runs its sync there, under the
caller's lock, and can fail there; a file's sink cannot, its `flush`
touching nothing. After `Database::fail` a durability is refused as every
write is, and a replay with it, under either `--sync`: the write it
answers may be among what the disk lost.

So is a write's refusal: it is read from what other writes left, from the
moment they land. Under `--sync always` an `insert` of an id another
`insert` had just landed was answered 409 while that one's fsync ran --
the #495 review held the first at `SlowDisk`'s gate and the second was told
"holds a document 7 already" -- and a power loss then took back the row
the client acted on: an insert it never sent again, an id it took for
another's. As were a `@unique` clash, a `create collection`, `create
statement` or `create index` that exists (409), a `require` another's
`del` left unmet (412), a REST write to a collection another dropped (404)
and a `/_changes` consumer's `POST` ahead of one another made (409). Every
write path that refuses under its write lock now asks the durability a
success would have waited for (fennec-http's `refusal_durability`:
`flush` under the write lock it holds, `settled` where no error says what
it read) and runs it after the lock (`refused_after`) -- REST writes,
routed or run, `/query` keyed or not, `/batch` as a block and statement by
statement, `create index` beside the database, `/_schema/apply`'s plan
refused, a consumer's write. An fsync that covered those writes already
runs none, one under way is waited out at the disk, and a failed one is
the answer, a 500, as it is a write's. A storage error's refusal and a
replica's (`ReadOnly`) wait for nothing: the one is its own answer, a
durability after it refused, and the other says nothing of what a write
left. A held write (`Fennec-Wait`) whose run wrote nothing is answered at
its wait's end with that run's answer, `affected 0` or a `require`'s 412,
which a keyed run and a `/batch` kept back with no durability; the wait's
end asks it under the read lock (`held_answered`, `Database::durability`),
a sink's error then going to `Database::fail` as a replay's does. Under
`--sync <ms>` nothing waits, as no answer does. Every refusal under the
lock is asked so but the memory ceiling's 507, which is the server's
memory and not what a write left: chosen by error kind rather than for the
few named, a refusal reading a field another `alter` dropped, a row a
scoped token's rule reads or a quota's bytes waits as well, and none is a
path that succeeds. A rule judged by the answer's status at the edge was the
other way: it could not tell a refusal from a 429 or the 422 of a key sent
with another request, which say nothing of what a write left, and would
have asked the lock again for each. The paths that succeed run nothing
new; a refusal asks the sink, the replay's cost where its bytes are
covered (`http.md`, *Idempotency keys*), and is rare. `tests/server.rs`'s
`a_refusal_is_answered_once_what_it_read_is_on_disk` holds fourteen
refusals, each through its path, to the held fsync -- each answered before
it with the change taken out, the two held writes' with the wait's end's
alone -- and `a_refusal_read_from_a_write_whose_fsync_failed_is_no_refusal`
an `insert`'s 409 whose reading fsync failed to the 500, with no fsync of
its own.

A file's sink fsyncs, too, what it inherited once (`Disk::inherited`): the
bytes an earlier process left in the file, maybe in the page cache alone
after its crash, are on disk once no fsync of this sink has run, and a
durability that covers nothing appended -- a replay after a restart, of a
key that process landed -- fsynced nothing. Only a durability
(`Sink::flush`) asks for them: a plain `sync`, which the shell runs as it
exits, a read's one-shot run too, would have fsynced for nothing. One that
covers appended bytes fsyncs anyway and clears the flag with it; one that
covers none fsyncs the file itself, beginning no sync log
(`Disk::sync_inherited`) -- through the log's path it made the log's
260 KB on Linux and Android for an app that wrote nothing. So the one
fsync more is paid once an open, by a durability with nothing appended
since: a replay after a restart, `fennec_sync` of an app that has written
nothing since it opened, a `--sync always` statement that appended nothing
(a `set` or `del` matching no row) as the first after the open. The syncer
and a node's tenants flush only what is dirty, and pay nothing; a write
under `--sync <ms>` runs no `Disk::sync` on its path. Every platform
needs it: on Linux an open's `synclog::recover` fsyncs the file only where
a log whose header names this file decodes -- a log missing, torn before
its header or another file's leaves the bytes as they were -- and a macOS
file keeps no log. A primary's open (`Sink::sync_existing`) and a cut of a
torn tail fsync the whole file, and clear the flag.

## Writes behind the lock

**Buffered writes are written behind the lock** (`Database::write_behind`,
`Sink::write_behind`). On macOS a `write` that takes a file's dirty pages
past each 8 MB waits for the disk: 1 MB writes appended one after another
took 1.5-2.9 ms every eighth, 2.0-11 ms with the file mapped, and
preallocating 128 MB (`F_PREALLOCATE`) changed nothing. The buffer written
as it outgrew 1 MB, under the write lock, held four readers beside blocks of
1 000 writes 4-6 ms every 8 MB. `write_behind` hands back what writes the
buffer, with no fsync, for the caller to run once it has let the lock go,
as it runs a durability: a server asks after every write that `--sync
always` does not fsync (`flush_for`), the buffer once it holds 64 KB
(`BEHIND_AT`). It passes over a disk a durability holds (`try_lock`), whose
fsync writes the bytes anyway: waiting, a buffered write waited out the
syncer's `F_FULLFSYNC`, a ledger's transfer once 126 ms, where it had
touched no disk before. A sink
asked once leaves its buffer for it from then on, an append writing it
only past `WRITE_HELD`, should the writes behind fall back. A handover writes what is pending, so for a database written behind
it is put off to the next block's start (`hand_over_before`), where the
write behind -- every byte, once a handover is due -- left nothing pending,
and runs at the end of a block only past twice its bounds: written there,
its bytes met the same stall under the lock. A replica and the shell write
as before; the native library writes behind as a server does (below).

## The sync log

**A durable write syncs a log beside the file, not the file**
(`synclog.rs`, Linux and Android). The file only grows, and on ext4 an
fsync of a file whose length changed commits the journal too: in Docker's
VM a 300-byte append and its `fdatasync` took 356 us, the same bytes
written into room the file already had and synced 65 -- why PostgreSQL
fills its WAL segments first. So `FileSink`'s sync writes what the file
took since the last one into `<file>.sync` (the whole name and `.sync`:
with the extension replaced, `x.db` and `x.fennec` shared one, as they
shared `.compacting` and `.beside` -- `fs::beside` names all three), 4 KB
of header and 256 KB of entries written once at full size and never
grown, and syncs that alone (`Tail`). The file is fsynced when an entry
would not fit, a sync holds more than 64 KB, or the sink is new to the
file -- whose bytes an earlier process may have left unsynced -- and the
log then begins a generation from where the file stands. An entry is the
file's bytes at a place, with its generation and a hash, so a cut one
ends the log; the header names the generation, where the file stood
synced, the file's device and inode, and a hash of the 4 KB before that
point, so a log is applied to its own file alone -- not to one renamed
into the place, even one holding the same bytes. `create` applies it
before the file is read or mapped (`synclog::recover`), fsyncs the file
and wipes the header, so a file cut below the log's end after that open
is not written over at the next. Everything that renames a file into a
database's place removes the log first and makes the removal durable --
`swap_in` after syncing the file it replaces, a restore, a backup
unsealed, a tenant's import (`fs::forget_sync_log`) -- and syncs the
directory after the rename (`fs::sync_dir`), which a tenant's import
missed until the #495 review, as a tenant's create missed fsyncing the
file its open made (`tenants.md`); a clean close syncs
the file and removes it, so a node keeps none beside a closed tenant, and
`Tenants::stats` counts it; a log that cannot be made is not tried again
until the next open, each try being 260 KB of writes. `open_read_only`
applies none: after a power loss, `fennec types` or a raw copy taken
before the next open may lack the last durable writes. On macOS
`F_FULLFSYNC` flushes the drive either way, 3.9 ms appended or not, so
there is no log (`ENABLED`; `fs::keep_sync_log` has a test thread keep
one). In Docker a durable update went 624 -> 325 us p50 (`make
roundtrip-bench`), against PostgreSQL's 378; `tests/synclog.rs` crashes
the machine (the process forgotten, the file cut back to the log's
start) through a full log, a sync past an entry, a torn header, a file
renamed in, a second open and two files of one stem, and fennec-core's
and fennec-http's tests pass on Linux, where every file keeps one.

## The one shutdown

**Every way a server stops is the one shutdown** (`fennec-server`'s
`durability`). `SIGINT`, `SIGTERM` and `SIGHUP` store `SHUTDOWN` in their
handler, which is all a handler may safely do, and the thread that syncs
-- `run_syncer` over a file, `serve_dir`'s loop over a node's tenants --
sees it within `LOOK`, 200 ms (`wait_tick`): the held writes answered
(`waits::end_all`), what was registered to run first (`before_shutdown`:
`--follow`'s follower stopped with every change it applied on disk, the
traces sent), then the final sync and the checkpoint, the write lock held
to `process::exit`, so no write is answered between the sync and the exit.
The syncers slept a whole `--sync` interval before they looked, so
`--sync 5000` exited 5.04 s after the end of its stdin, its lock held
against the app's next open of the file, and `--sync 60000` would have let
docker's SIGKILL, 10 s after its SIGTERM, come before the final sync; the
interval is still counted from the end of the last pass, so a flush comes
as often as it did -- under writes, `--sync 250`'s flushes 260 ms apart
before and after, and an idle `--sync 60000` server 10 ms of CPU in 30 s,
the resolution `ps` reads it at. A failure of what runs beside the
database goes the same way with an exit code (`stop_with`): the HTTP
listener's `serve_on` ending (1), `--follow`'s mirror stopping (1) -- which
exited the process from its own thread and so cut short a shutdown already
under way, whose `before_shutdown` was waiting for it, the sync among what
was cut -- and a thread the start cannot make (2, as it exited before).
The mirror stores its code (`exit_with`), then tells that wait it has
stopped, so the wait is not its `STOP_WAIT` and the exit cannot come before
the code, and only then asks for the shutdown. A shutdown writes a
checkpoint only once the HTTP thread started (`serving`, unless a stop was
asked for before that thread was: read after it, the flag was a
supervisor's answer to the thread's own `listening on` line 1 time in 200
beside 16 busy loops, and a server that had served kept no graph) and only
when it exits 0 (`checkpoints`), a server's or
a node's (`Tenants::shutdown_with`): a checkpoint rewrites the whole file,
and the review of #383 measured a file server of 100 000 64-dimension
vectors whose port was held log `checkpoint written` at every failed start,
its inode new and its file 26.4, 28.4, 28.9, 29.3 MB, each graph a part
(the next open still linked 91 691 vectors), a `--dir --replica-of` standby
rewrite its vector tenant at each, which `fennec-node.service`'s
`Restart=on-failure` repeats, and `--follow` make its slot and publication
and copy 200 000 rows within `STOP_WAIT` before it exited 1. A `--follow`
server whose PostgreSQL refused it served before its mirror failed, and
rewrote a file of 6 000 vectors at each of three tries, a new inode each
time: a failure is not a stop asked for, and the graphs `link::keep` appends
as they change keep the next open's linking short without a checkpoint.
So both listeners, `--metrics`' and the HTTP one, are bound straight after
the open -- before the guard `--follow` puts on it, the history settled
(a failed `--promote` had forked and synced its file, 24 bytes, and a first
start as a primary gave its file a history), and anything running beside
the database: the graph keeper, the sweeper, a replica's follower, the
mirror, the stdin thread, a node's tenants. Not before the open, whose
reading of a large file a connection would wait out in the backlog. An open
address without a token is refused with the flags, exit 2, before the open
(`fennec_http::exposed`, which counts an empty token as none). What fails
before anything writes the file or runs beside it -- a flag, the lock, the
open, a port -- exits where it fails, with the exit codes it had: 1 for a
file server's HTTP port, 2 for a node's and for `--metrics`'. It has
written `<file>.lock` (`fennec_abi::lock::hold`, taken before the open and
left on disk, as every server leaves it) and what the open writes, no
more: a file, or a node's directory, made where none was (an empty file is
8 bytes), a torn tail cut, and on Linux and Android a sync log's entries
written back into the file (`synclog::recover`), the writes an earlier
process had made durable. Taken
before the tenants open, a node's port queues the connections that come
while they do: a standby of 400 vector tenants took 1.36 to 1.67 s to
answer, macOS kept 128 connections in its backlog and the rest of a burst
had their SYNs sent again, for up to some 4 s, and through a router a
request waits up to `--upstream-timeout` -- where before they were refused
at once. No request was answered by a half-started node. On a node the
signal handlers are installed once the tenants a standby follows are
open, as before: a SIGTERM while they open stops it at once by the default
action, rather than wait for each to open past a supervisor's grace. What
its followers had applied by then is not lost to the primary: a replica
asks from its file's own position, so the primary sends it again.
`--exit-on-stdin-close` sets the flag (`request_shutdown`) from a thread
that reads stdin to its end. An app
that starts a server for a file it opened holds a pipe on the server's
stdin, and when the app is killed (`kill -9`, a crash) the system closes
the pipe, where no signal would have come: the desktop studio's spike
(#381) found the orphaned server, its parent now pid 1, holding
`<file>.lock`, so the file was refused to the app until its next launch
reaped the server from a registry of pids. Watching the parent's pid was
the alternative: a poll, late by its interval and fooled by a pid used
again, and macOS has no `PR_SET_PDEATHSIG`. What arrives on stdin is read
into a 4 KB buffer and dropped, so a parent's writes never block on a
full pipe and nothing grows; a stdin handed over non-blocking is asked
again every `LOOK` while it answers `WouldBlock`. `/dev/null`, or a stdin
closed before the start, ends at once and so does the server, exit 0,
which a parent that forgot the pipe learns at its first start. A terminal
is refused at the start (exit 2): a server reading one from the background
is stopped whole by `SIGTTIN`, its lock held -- the stuck server the flag
is for -- and in a terminal Ctrl-C makes the same shutdown. So is a
`--jwt-secret-file`, `--policy` or `--jwt-keys` that is stdin
(`is_stdin`: by name or link, since macOS answers a stat of `/dev/fd/0`
over a pipe with the device node's, or by device and inode), before it is
read: read to its end, it would have been the server's end, and a parent
holding the pipe open would have held the read for ever. A way down of
its own, an exit from the stdin thread, would have been a second place to
hold the lock to the exit, and one the `--dir` loop and the follower were
never taught. `tests/stdin.rs` holds a file's and a node's shutdown to
it, a long `--sync` to an exit well inside its interval, a server without
the flag to never reading stdin, `/dev/null`, a terminal with the flag and
without it, and a secret on stdin; `wait_tick`'s tests hold a tick to no
sooner than its interval and a stop to long before it; `tests/start.rs`
holds a file server, a first start as a primary and a standby whose port
is held to an exit that leaves a file of vectors as it was, to the inode
and the byte, a `--follow` server whose source refuses it to an exit 1
with no checkpoint and the same inode, twice, and an open address with no
token to an exit 2 before the file is made; and
`tests/follow.rs` (against PostgreSQL) a held port to no slot or
publication made, and a mirror that cannot go on to an exit 1 through the
shutdown.
