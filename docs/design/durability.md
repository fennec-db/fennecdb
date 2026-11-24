# Durability

The long form of the invariants [the root CLAUDE.md](../../CLAUDE.md), [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md), [crates/fennec-http/CLAUDE.md](../../crates/fennec-http/CLAUDE.md) and [crates/fennec-server/CLAUDE.md](../../crates/fennec-server/CLAUDE.md) list under *Durability*: a storage error stopping writes, buffered writes written behind the lock, the sync log, the mode a file is made with and keeps through a rename, a node's tenants each synced on a thread of its own, the power cut that holds the sync log on Linux, and the one shutdown (the last four added after the move). Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

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
had not yet sent. A refused rename is not such a failure: it wrote
nothing. On Windows another process's handle opened without
`FILE_SHARE_DELETE` -- an indexer's, a backup agent's, a synced folder's --
refuses a rename over the file (std's rename answers
`ERROR_ACCESS_DENIED`, the POSIX call `ERROR_SHARING_VIOLATION`; both are
taken), so `swap_in` tries it, and the open after it, again within one
second, a millisecond's wait doubling up to 50, before it fails the sink as
any failed rename does (`fs::windows::swap`,
[windows.md](windows.md#7-what-else-windows-differs-in), held by
`a_rename_another_handle_refuses_is_tried_again` on CI's Windows job). That fsync runs *outside* the exclusive lock: under it a
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
<ms>` flushes the same way, a database's and each of a node's tenants',
[each on a thread of its own](#a-nodes-syncs) (`sync_now`): the tenants'
held their lock through the fsync, and every read and write of one waited
up to 27 ms a pass on macOS.

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
and a `/_changes` consumer's `POST` ahead of one another made (409). A
write refused under its write lock now asks the durability a success would
have waited for (fennec-http's `refusal_durability`: `flush` under the
write lock it holds, `settled` where no error says what it read) and runs
it after the lock (`refused_after`, `refused_under`). An fsync that covered
those writes already runs none, one under way is waited out at the disk,
and a failed one is the answer, a 500, as it is a write's. Under
`--sync <ms>` nothing waits, as no answer does.

Which refusals wait, exactly. An engine error a write meets under its lock
waits unless it is a storage error's (`Io`: its own answer, a durability
after it refused) or a replica's (`ReadOnly`, which says nothing of what a
write left) (`read_from_writes`): on `/query` and `/_call`, keyed or not
(the run's error and a key's failed keep), a REST write as it runs,
`/batch` as one block (a line's stop, and its land's -- a quota's 507), a
`create index` beside the database (its 409), a consumer's write (its
refusal and its error). Besides those: a REST write's refusal as it is
routed under the lock where the schema decided it -- a collection another
dropped (404), a field or a type -- and where the token's rules read
another collection (`scoped_under` with `reaches_out`); an MCP call tool's
403 for a statement made to write since its tool was listed, its 413 past
`--mcp-max-rows` or `--mcp-max-bytes`, and its block's refusal as it lands
(`mcp::open`, `mcp::land`: a quota's 507), on `/query`'s path and the
keyed one alike; a `/_schema/apply` that applies nothing, its plan refused
(409) or finding everything there as declared (`applied: false`, 200),
and its error but a storage error's; a `/batch` run statement by statement,
stopped after a statement was made -- its `completed` statements made
durable before the stop is answered, a failed fsync the 500 in its place --
or before any, waiting as a block's line does. And a held write's answer
at its wait's end, or at a shutdown ending the wait before it began
(`held_answered`, below). What waits for nothing: the memory ceiling's 507
(`over_ceiling`), the server's memory and not what a write left; and a
refusal of the request alone, which no write changes -- a REST write sent
to a path no route takes (404), a body that is no UTF-8 or no JSON (400)
(`api::refusal_read`, which asks the body again on the refusal's path and
counts what it cannot place as read), a token's own rules (a field a rule
withholds, 403), the one refusal of `begin` (a block left open), a `/batch`
holding a compact sent with a key (400). The 422 of a key sent with another
request, found under the lock, waits for none either: it says nothing of
the write (`idempotent::replay`, `http.md`). Refusals asked before the
lock -- a rate's 429, a write `Fennec-Wait` cannot hold -- take none to
begin with. Nor does `fennec mcp --file`'s write tool, whose one caller's
every write was on disk before it was answered. The #497 review found the
second half: a REST write's `{not
json` and an unknown route's 404 had waited out another client's fsync,
and of the first half an MCP call tool's quota at land, its 413 and 403, a
keyed 413, and an apply finding nothing to do -- a second identical apply
answered `applied: false` while the first's fsync was held -- answered at
once. Chosen by error kind for the engine's errors rather than for the few
named, a refusal reading a field another `alter` dropped, a row a scoped
token's rule reads or a quota's bytes waits as well, and none is a path
that succeeds. A rule judged by the answer's status at the edge was the
other way: it could not tell a refusal from a 429 or the 422 of a key sent
with another request, which say nothing of what a write left, and would
have asked the lock again for each.

A held write (`Fennec-Wait`) whose run wrote nothing is answered at its
wait's end with that run's answer, `affected 0` or a `require`'s 412, read
from what other writes left as that run saw it; the wait's end asks its
durability under the read lock (`held_answered`, `Database::durability`), a
sink's error then going to `Database::fail` as a replay's does. A run that
leaves a plan to be held -- `waits::plan` says whether it did -- asks
none: its answer is kept back, and asked at each run a `/query`'s 412
waited at the disk on every look of its group, which held the group's
other requests behind it (the #497 review). One that leaves none, nothing
to look for, is answered at once, as a refusal under the lock is: a keyed
run's and a `/batch`'s went out with no wait before. A shutdown that ends
a wait before it began answers as the wait's end does: returned as it
was, a keyed run's or a `/batch`'s kept-back 412 went out with no
durability. The end's wait is the request's own time, as a write's fsync
is (`waits.md`).

The paths that succeed run nothing new; a refusal asks the sink, the
replay's cost where its bytes are covered (`http.md`, *Idempotency keys*),
and is rare. `tests/server.rs`'s
`a_refusal_is_answered_once_what_it_read_is_on_disk` holds seventeen
refusals, each through its path, to the held fsync -- each answered before
it with the change taken out, the held writes' with the wait's end's
alone -- an apply's refused plan and one applying nothing among them;
`a_quota_refused_at_land_is_answered_once_what_it_read_is_on_disk` a
`/batch`'s and an MCP call tool's quota at land;
`a_refusal_of_the_request_alone_waits_for_no_fsync` a REST write's `{not
json` and an unknown route, answered while the fsync is held;
`a_held_run_asks_no_durability_its_wait_end_does` a held `require`'s two
durabilities, the first write's and its end's, never a third;
`a_batch_stop_after_a_statement_made_is_its_fsync_failure` a stop after a
statement made, on a failing disk, to the 500; and
`a_refusal_read_from_a_write_whose_fsync_failed_is_no_refusal` an
`insert`'s 409 whose reading fsync failed to the 500, with no fsync of its
own. Each fails with its fix taken out.

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

What an fsync covered is also what the next record claims
([storage.md](storage.md#file-format)): each of these -- `Disk::sync`, its
sync log's durability, `sync_inherited`, `sync_existing`, a cut's, a
rewrite's swap -- stores the file's length in `Disk::durable` once the call
returned, never before, and an append reads it into the record's claim:
the database holds the atomic itself, taken from the sink with it
(`Sink::durable`, `Database::set_sink`), so a server's write reads it
with no call through the sinks wrapped around the file's. So a claim never runs ahead of a completed fsync: a
durability still at the disk claims nothing new, the records appended
meanwhile claiming the point before it; a failed one stores nothing. No
fsync is added for it, and a durability that covers nothing appended runs
none still. A close after it appended something and everything was
covered appends the 8-byte mark (`REC_SYNCED`) unsynced: a durable point the next open need not look for,
lost harmlessly. An open that cuts a torn tail leaves the same mark after
what it keeps, where a record it keeps carries a sum and no whole mark
ends them already (`FileSink::cut`), each step fsynced before the next:
the file first -- the earlier process's pages, which only that fsync
makes durable, and a close's mark ending what the cut keeps -- then the
mark, written over the first bytes the cut takes with a head past the end
of any file after it (in three writes where they cross a 512-byte
sector's end, the mark's kind byte last), then the cut. Each comes before
what would claim more than the disk holds without it, so the next open
reads the file as the one that cut it did whatever step a power cut stops
-- the records that claimed how far it was durable may have gone with the
tail -- on a disk that writes a 512-byte sector whole or not at all, which
NVMe promises and SATA does not, but in the window of a mark across a
sector's end, where it may be refused, and where, given a row below the
durable point the disk damaged, three shapes of file have that row cut
with the synced ones after it; a write or an fsync of the cut failing
there leaves that window for an open tried again
([storage.md](storage.md#file-format), *What a cut leaves*, *The cut's
order*, *The sector, an assumption*). An open hands its durable point to the sink
(`Sink::set_durable`), the appends after it claiming it until an fsync of
their own covers more.

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
directory after the rename (`fs::sync_dir`; on Windows, which keeps no
log, the renamed file flushed and then its directory,
[windows.md](windows.md#3-durability-on-ntfs)), which a tenant's import
missed until the #495 review, as a tenant's create missed fsyncing the
file its open made (`tenants.md`); a clean close syncs
the file and removes it, so a node keeps none beside a closed tenant, and
`Tenants::stats` counts it. The close is the sink's drop, which a process
exiting with the database held never runs: a server's shutdown keeps the
write lock to `process::exit` ([the one shutdown](#the-one-shutdown)) and
a node's leaks its tenants' locks. So 0.2.0, stopped cleanly on Linux --
`docker stop`, under `--sync 250`, `always` and a `--dir` node's open
tenants alike -- left the 260 KB log beside each file it had written, and
`fennec -c`, ending in `process::exit` too, beside each file it wrote.
Nothing was lost -- the next open applied a log the file already held --
but a stopped server left more than its file and `.lock`. The shutdown
now asks for the close after its last sync and checkpoint
(`Database::close_log`, which `Holding` and `Tee` pass on): the file
fsynced, the log removed, and none begun again (`Tail::close` clears
`keeps`). A `--sync always` write's durability, handed out before the
shutdown took the lock and run after it, fsyncs the file rather than begin
a log: one with a write still pending at the close began one again, as did
the first sync of a file that had none yet. `-c` drops its database before
the exit. A storage error leaves the log, which may hold what the file
lacks, and `fennec-shard`'s router catches no signal, so its stop is a
crash and leaves its directory's log. A log that cannot be made is not
tried again until the next open, each try being 260 KB of writes. `open_read_only`
applies none: after a power loss, `fennec types` or a raw copy taken
before the next open may lack the last durable writes. On macOS
`F_FULLFSYNC` flushes the drive either way, 3.9 ms appended or not, so
there is no log (`ENABLED`; `fs::keep_sync_log` has a test thread keep
one). In Docker a durable update went 624 -> 325 us p50 (`make
roundtrip-bench`), against PostgreSQL's 378; `tests/synclog.rs` crashes
the machine (the process forgotten, the file cut back to the log's
start) through a full log, a sync past an entry, a torn header, a file
renamed in, a second open and two files of one stem, and holds a database
kept to the exit to `close_log` and a durability run after it
(`a_database_held_to_the_exit_lets_its_log_go`); fennec-server's
`a_clean_stop_leaves_no_sync_log` (`--sync always` and `250`) and
`a_nodes_clean_stop_leaves_no_sync_log`, and fennec-cli's
`the_shell_leaves_no_sync_log`, failed on Linux before the close was
asked, each finding the log after the stop; fennec-core's and
fennec-http's tests pass on Linux, where every file keeps one.

## Files' modes

**Every file is made its owner's alone, and a file there keeps its mode.**
On a Unix the call that makes a file gives it mode `0600` whatever the
umask (`fs::private`, an `OpenOptions` with the mode), and one that makes a
directory `0700` (`fs::create_dir_private`). Up to 0.2.0 each was the
umask's -- `0644` under the usual 022, `0666` under 000 -- and every user
of the machine read a node's tenants (their rows and the server's `_`
collections), a tenant's records, archives, backups, a restore or an
unseal, and the audit log's refused requests (G14's review). The attacker
is another user of the same machine, without the server's user or root.

What makes them, by crate: fennec-core's `FileSink::create` (the database,
which every server, replica, tenant, router directory, shell, import and
app open), `rewrite_with`'s `.compacting` (a checkpoint, a compact under the
lock, a replica's image, a restore's checkpoint), `SideFile::create`'s
`.beside` (a compact or an index built beside the database, the
auto-compact), `SyncLog::open`; fennec-abi's `lock::take` (`<file>.lock`);
fennec-http's `audit::open`, `instance::write` (`<t>.instance`),
`tenants.rs`' `.follows`, `.quota` and an install's `.importing`,
`Tenants::new`'s directory, `tenant_hooks` (`<t>.hooks`, already `0600`),
`schedule::hint::write`, the archive's directory, images, `began`,
segments and a restore's `.restoring` (`fs::copy_private`, `assemble_into`), `backup`'s and
`unseal`'s `.tmp`, a node archive's root, records and restored directory;
fennec-cli's `.save` and `fennec key` (already `0600`, made with
`create_new`). Not data: `fennec types`' TypeScript, written with the
umask's mode as source code is. The browser's OPFS file is no Unix file;
the module has no `std-fs` and makes none.

Never a `chmod` that narrows a file after its make: between the two,
another user's `open` gets a descriptor that reads everything written
after. The one `fchmod` there is (`mode_like`, below) gives a file made
`0600` the mode of the one it is to replace, before the rename, never a
mode wider than that one's. A side file is made
by its open alone (`fs::create_private`, `O_CREAT | O_EXCL`), which follows
no link put at its name: whatever is there -- a link, a file an earlier
process left, wider under 0.2.0 -- is removed and the file made anew. Opened
through a link, the image went into the file the link named and the mode
set after with it, and the link was renamed over the database; truncated in
place, a file kept its mode and any descriptor on it. A file opened again
where it is there -- the sync log, the lock, a tenant's `.instance` -- takes
`O_NOFOLLOW` instead (`fs::private_beside`), each target's number as its
libc's `fcntl.h` has it -- Linux's by processor, the BSDs' and Apple's,
illumos' -- and a Unix whose number is not listed fails to build: `0`
there, as armv7 Android's library had it, left the open following links
unsaid. `fs::copy_private` copies an image into `create_private`'s handle
and returns it (it was the system's `fs::copy`, which opens its target
through a link, then an open by the name); on Apple's systems it clones an
image whose mode gives its owner read and write and no one else anything
(`cloned`, `fclonefileat`), a clone being made with its source's mode --
a wider image's would be another user's to open until an `fchmod` --
and `CLONE_NOFOLLOW` keeping it off a link at its name: without the flag a
clone follows one that names no file and is made where it points (macOS
26.1), as the system's `fs::copy`, passing none, did. The clone hands back
no descriptor, so its name is opened again with `O_NOFOLLOW` and held to
what the clone made -- one regular file of one name, the process's
(`CLONE_NOOWNERCOPY`: under root, root's), on the image's volume and of
its length -- and to the name (`still_named`), then made `0600` on that
handle where it is not; another file system, volume or a system without
the flags copies. And `swap_in` renames a rewrite's
`.compacting` in through the handle
that wrote it, and a compact's `.beside` and a replica's `.taking` through
the handle that wrote them, which `Sink::adopt` is handed and takes a `dup`
of (an open by the name, as both were, followed a link put there while the
image was written, the `fchmod` and every append after landing in the file
it named; opened with `O_NOFOLLOW`, as `.beside` and `.taking` were until
the review of images taken as they arrive, it took a regular file put at
the name instead, which the stores a replica's image was loaded into do
not map, and renamed it in). A replica's image is held to its name first
in `Database::adopt_side` too, so that one put there is refused before the
sink is asked, the copy as it was and taking writes; refused by the sink,
as a compact's is, the database fails. A side file a crash left -- a
replica's `.taking`, a checkpoint's or a compact's `.compacting`, a
compact's or an index's `.beside`, the shell's `.saving` of a `.save` over
the file (`fs::LEFT_BESIDE`), an install's `.importing` -- is removed by
its owner alone: the open that writes the database (`FileSink::create`,
all but `.importing`), and a node as it starts, under each tenant's lock
taken without waiting (`tenants::sweep_left`, all five), so a tenant no
request opens again keeps none; never a tool that only looks
(`open_read_only`: `--read-only`, `fennec types`), nor one whose tenant's
lock another process holds
([replication.md](replication.md#an-image-taken-as-it-arrives)). Nothing
of this process can be writing one as the open runs: `<file>.lock`, which
a `.save` takes too, refuses a second holder of the file in a process as in
two, and a database rewrites nothing before its open returns. The removal
is an `unlink`, which follows no link: one put at the name goes, the file
it names as it was; a directory there stays, and the next rewrite of that
name fails on it as it did before. Until the P6 soak's second run, a
compact's `.beside`, a rewrite's `.compacting` and a `.save`'s `.saving`
stayed until the next writer of the name made it again -- every release,
`.compacting` from 0.1.0, `.beside` from 0.1.5 and `.saving` in 0.2.0 --
and the soak, killed during a compaction, kept a 1.4 GB `.beside` beside
its database through minutes 91 to 102, no compact running: a crash in a
compact doubles the disk a database takes for as long as no compact runs,
and the auto-compact waits for half the file to be dead. fennec-core's
`tests/side_files.rs` kills a compact beside the database once its side
file is made (`compacts_until_killed`, a child process ending with no
destructor run) and holds the file left until the open that writes, past
a read-only open; puts every name at its place before a mapped, an
in-memory and a server's open; and puts a link and a directory at each,
the link removed and its file kept, the directory kept and the database
opened. fennec-http's two `sweep_left` tests put `.compacting`, `.beside`
and `.saving` beside a tenant's file and a linked one's.
The rename itself goes by the name, so `swap_in` first holds the name to the
handle's file (`still_named`, its device and inode against an `lstat`; on
Windows the volume's serial number and the file's index against the name
opened as no link, since the image review's second round):
another file put there since is renamed over nothing, and the sink fails, as
a failed rename fails it. Each of these needs write access to the
database's directory, which takes the database itself anyway: what they
close is a write, a mode or a rename landing on a file the link names.
`backup`, `unseal` and the small records (`renamed`: an image, `history`,
`began`) hold their side file's name to the handle that wrote it before the
rename, as `swap_in` does, and `unseal` syncs through that handle, where it
opened the name again. What still goes by a name, to the same access, and
SECURITY.md says so: a restore opens its `.restoring` again to check and
fork it (`fs::open`, which resolves a link there) and renames it in after
the checkpoint renamed its own image there, and `unseal` reads its `.tmp`
again to check it (`fs::open_read_only`), each held to the handle that
wrote it just before the open, so a link put there in that moment is
followed. Closing the restore's takes an open of a database over a handle,
which fennec-core has not; the unseal's check, read from the plaintext in
memory instead, would copy every document, where the mapped open adopts
the image's offset index.

**One way to make a file.** `tools/file_create_check.py` (CI's fmt job,
with `--self-test`) reads `crates/*/src` but fennec-bench's and
fennec-fuzz's through `tools/secret_eq_check.py`'s reader -- comments,
strings and `#[cfg(test)]` items left out -- and refuses `File::create`,
`fs::write`, `.create(..)` and `.create_new(..)` given `true` or a name,
`create_dir`, `create_dir_all`, `DirBuilder`, `fs::copy`, `fs::rename`,
a call of `clonefile`, `clonefileat` or `fclonefileat`,
`UnixListener::bind` and `UnixDatagram::bind` outside the helpers, a
function of `std::fs` imported by its name or another (`use std::fs::{write,
rename as moved}`) found where it is called so, and `File` or `fs` imported
under another name too: an open whose statement puts the mode on it
(`private(` or `private_beside(`) passes, a rename in a function that keeps
the mode (`create_like(` or `mode_like(`) passes, and each other one is in
its `ALLOWED` by file and kind, with why and how many: the helpers' own
`DirBuilder`s and clone, Windows' opens and swap, `fennec key`'s `create_new` with
mode `0600`, `fennec types`' TypeScript, the unwinding checks' scratch
directory, an archive's `renamed` (whose callers made the record by
`create_like`, or `began` by `create_private` where there is none), a
tenant's install (refused where a file has the name), and a directory or a
sidecar moved aside, which takes its own inode and mode with it. A new
`File::create`, or a rename of a new file over another without its mode,
fails the job rather than waiting for a review to see it.

**A file there keeps its mode, through every replacement.** An operator's
`chmod 640` for a backup job must survive a compact, so a side file renamed
over a file takes that file's permission bits before the rename
(`fs::mode_like`, on the side file's open handle -- `fchmod` -- never by a
name a link could have moved): `swap_in` for a checkpoint, a compact under
the lock or beside it and a replica's image (the handle that wrote the
image, the one the sink appends through after), `Archive::restore` (on
the handle `assemble_into` wrote `.restoring` through, before its
checkpoint carries the mode on), `backup` and `unseal` into a file, `.save`,
a tenant's records and `<t>.hooks`, the hint. A small record takes it as it
is made (`fs::create_like`), before a byte is written; an image as long as a
database just before its rename. The set-id bits are not copied. A file
0.2.0 made `0644` is opened as it is and keeps `0644` -- fennecdb narrows
nothing under an operator -- which the server page and SECURITY.md say,
with the `chmod` that closes it.

**A group's bits stay with its group** (`fs::kept_mode`). The new file is
in the group a file made there gets -- the process's on Linux, the
directory's on macOS -- not the old file's: nothing is chowned, owner and
group being the rename's. Copied whatever the group, the bits of a database
`chgrp backup` and `chmod 640` went, after the first compaction or the
checkpoint at shutdown, to the server's group (the backup group losing its
read on Linux) or, on macOS, to the directory's -- often `staff`, every
local user (G14's review, F5). So the group's bits are copied only where the
new file's group is the old one's, and cleared otherwise; the owner's and
the others' are copied as they are. A `fchown` of the group back, which an
owner may do to a group it is in, is not made: the batch's rule is that
owner and group are whatever the rename gives, and a server keeping a group
it was never told of would hold a file's access to whatever group it last
saw. The way to keep a group across rewrites is the system's: a directory
of the group, setgid and searchable by it (`chgrp backup /var/lib/fennec &&
chmod g+xs /var/lib/fennec`: the `x` lets the group reach the file, which a
`0700` directory does not), every file made in it then of the group, so the
image is, and keeps the bits. A
file another user owns lends nothing: its owner's mode is copied only where
the old file is the process's user's, as the new one is, so a file put at a
name fennecdb writes -- a link in a shared directory, as below -- never
widens what fennecdb writes there.

**`fennec verify` restores into a directory of its own.** It restored the
archive's end into `<temp>/fennec-verify-<pid>-<n>.fennec`, a name another
user of the machine could put first in `/tmp` as a link to a `0644` file of
theirs in a directory of theirs: the restore resolved the link
(`fs::resolved`, a `readlink` that Linux's `protected_symlinks` does not
stop), took the file's mode and renamed the plaintext over it -- under
root's `FENNEC_KEY_FILE` (the runbook's `fennec verify --tenants`), a sealed
archive's rows -- and the cleanup removed the link alone; without a link, a
file put at `.restoring`'s name lent its `0666`. Every release with
`verify` had the name (v0.1.6 on, `0644` under the usual umask before this
batch). It now makes `fennec-verify-<64 random bits>` in the process's temp
directory, one directory, `0700`, by `mkdir` alone (`fs::create_dir_new`,
never `recursive`, which takes one there), a name taken being tried again
under another, restores into it and removes it whole (`Probe`, as it is
dropped). A verify that never drops it -- killed, or a panic under `fennec`'s
`panic = abort` -- leaves the directory in the temp directory with the
restored plaintext in it, `0700` and its file `0600`: its owner's alone,
whom the key's file let read the archive anyway, until the system's own
cleaning of the temp directory or a hand removes it. `fennec verify` sweeps
none: a removal by name and age in a directory every user writes is a
second way to be led somewhere, for a file no one else can read.

**A sidecar takes its database's mode.** The sync log is removed at every
clean close and rewrite and made again at the next sync, so a mode set on it
once would not last; made `0600` by its open, it then takes the mode of the
file whose bytes it holds (`SyncLog::open`, `kept_mode` from the open
files, an `fchmod` only where the modes differ), so whoever an operator let
read the database reads its log, which a `--read-only` open and `fennec
verify` apply after a power loss. `<file>.lock` made beside a database
there takes of its mode only what a class that may write the database has
(`lock::writers_mode`, when the file is new and empty): the owner's bits,
the group's where the group may write it (and the lock is of its group),
the others' where they may. A group that may only read the database got the
lock's read too, and a reader holding `F_RDLCK` on it refuses the server's
next start, where no reader needs the lock (`--read-only` takes none). One
made first -- before its database -- is `0600`, as the database then is. A
lock is never removed, so a mode set on it lasts.

On Windows a file takes its directory's access list and these change
nothing (`private` compiles to the plain open, `mode_like` to nothing);
a secret's file is `owner_only`'s DACL ([windows.md](windows.md)).

Measured: only a file's making changes. `private` is a flag on the open
that makes the file, `O_EXCL` and `O_NOFOLLOW` flags on it too;
`create_private` adds an `unlink` only where something is at the name, and
`mode_like` an `fstat` and a `stat` to a replacement -- a compact's or a
checkpoint's, against an image's writing -- and an `fchmod` only where the
modes differ; `swap_in` takes the writer's handle where it opened
`.compacting` by name (one `open` fewer), a `dup` of `.beside`'s and
`.taking`'s where it opened them again with `O_NOFOLLOW`, and
`still_named` adds an `fstat` and an `lstat` to each, and to a record's, a
backup's and an unseal's rename. The open that writes a file removes four
names beside it where it removed one: three more `unlink`s, each failing
where nothing was left, and their names' allocations -- the perf gate's
`file-open`, an open of 100 000 rows, asked the allocator 200 123 times
for 3 779 720 bytes against 200 114 and 3 779 252 with `.taking` alone
(natively, three runs a side alike), +9 calls and +468 bytes, 0.005% and
0.012%; its time, taken at a load of 37-40 from others' work, was
discarded. The image a
restore or a consolidating round of an archive with no key starts from
(never a request) is copied by `copy_file_range` on Linux, as
`fs::copy` copied it: std's `io::copy` from a `File` into a
`BufWriter<File>` is its `kernel_copy`, which flushes the empty buffer and
hands both descriptors to the kernel. On APFS it is cloned again, as
`fs::copy` cloned it, where the third round of G14's fixes had copied its
bytes a mebibyte a call: with the fsync a restore runs after, on an
8-core Mac with the one-minute load at 2.9-3.1, 256 MiB took
0.004-0.008 s cloned, three runs, against 0.13-0.27 s copied and
0.008-0.012 s by `fs::copy`; 1 GiB 0.003-0.010 s against 0.55-1.20 s and
0.006-0.013 s (at a load of 6.8-7.1, others' work: 0.003-0.004 s,
0.27-0.34 s and 0.010-0.013 s; 0.004-0.008 s, 0.81-1.98 s and
0.006-0.018 s). An image wider than its owner's, which 0.2.0's archives
hold, is still copied. The clone's code is 1 152 bytes of the `fennec`
shell's `__text` on macOS (2 854 400 to 2 855 552; the file 3 751 920 to
3 752 032 bytes), where `restore`, `archive` and `unseal` run;
`fennec-server`'s file and sections are the same to the byte, and no other
target builds it. The lock's `writers_mode` runs
only for a lock file made, and the log's `kept_mode` only for a log made,
its `fchmod` only where the log's mode is not already its database's. No request opens a file, and the perf gate's `file-open` makes
none; its `file-compact` makes a side file an op. Counted natively on
macOS (`proc_pid_rusage`'s instructions, the kernel's with the process's,
which callgrind does not see), the op with the gate's 10 000 rows took
10.34-10.43 million instructions before, four runs, and 10.40-10.42
million after, inside that spread, its allocations the same (38 calls,
1 086 121 bytes). A `chmod` on every compact, as `mode_like` first did,
took it to 10.50-10.61 million: APFS journals the metadata write, some
100 000 instructions, where the image of a database its owner's alone
has its mode already.
fennec-core's `files_are_made_their_owners_alone_and_keep_their_mode`,
fennec-abi's `a_lock_file_is_made_its_owners_alone_or_as_its_database`,
fennec-http's `tests/modes.rs` and fennec-cli's
`what_the_program_makes_is_its_owners_alone_and_keeps_its_mode` run under
a umask of 022 and 000, each set by `sh` for a child that runs the one
test, since the umask is the process's and the tests beside it on cargo's
threads make files too; each first asserts that a plain create takes the
umask, then every path above, and a file made `0640` written, checkpointed,
compacted, compacted beside, restored over, backed up and unsealed over;
the sync log's mode as its open made it is read before it takes its
database's (`synclog::MADE`, a test's), so the log made without the mode on
its open fails it. fennec-core's `a_groups_read_goes_to_no_other_group`
gives a database a group of the test's user it is not made with (`id -G`;
skipped with its reason printed where the user has none) and `0640`, then
holds its sync log, a checkpoint, a compact and a compact beside to that
group with its bits or to none, and a database in a setgid directory to
both; `a_link_at_a_side_files_name_is_never_followed` puts links at the
sync log's, `.compacting`'s and `.beside`'s names, before each is made and,
for the two images, while each is written (a link put there then was
followed until `swap_in` took the writer's handle), and fennec-http's
`a_link_at_the_sidecars_name_is_never_followed` at `.instance`'s;
fennec-core's `a_copy_of_an_image_is_cloned_where_it_is_its_owners_alone`
copies images `0600`, `0700`, `0644` and `0400` onto nothing, a link to a
file, a link to none and a file left there, holds what the links name
unchanged and unmade and the copy `0600`, the file at the name, open at
its end, and on Apple's systems counts the clones (the first two cloned);
fennec-http's `a_record_is_renamed_in_only_from_the_file_it_wrote` puts a
link and a file at a record's side file once written, and holds the rename
refused. fennec-abi's
`a_lock_file_is_given_only_to_those_who_may_write_its_database` takes a
lock beside databases `0600` to `0666`, of another group, and a link at the
lock's name. fennec-http's `modes::verify_writes_nothing_another_user_reaches`
plants links at the names a verify took, in a child whose `TMPDIR` is the
test's own, and holds the files they name unchanged; `.follows`
(`a_read_replicas_stream_is_no_standby`), `<t>.hooks` written again over
`0640` (`a_tenants_secrets_are_set_listed_and_exported_by_its_admin_alone`),
a node archive's `presence`, `history`, `began` and `restore_all`'s
directory (`a_deleted_tenant_is_no_longer_followed_and_its_archive_is_kept`)
are held through the server's own routes and the archiver.

## A node's syncs

**A node syncs each tenant on a thread of its own** (`Tenants::sync_every`, `Tenants::sync_each`; fennec-http's `tenants/syncs.rs`). A file's syncer (`run_syncer`) sleeps the interval, flushes under the write lock and fsyncs after it, so a write landing just after a sync took its writes is on disk within the interval and two syncs' own time -- [the power cut](#a-power-cut) holds `--sync 250` to 650 ms. A node's tenants were synced by one pass on `serve_dir`'s loop: every dirty tenant in turn, each flushed under its write lock and fsynced after it, the interval slept from the pass's end (`Tenants::sync_dirty`). A tenant's window was the interval and the whole pass -- every other tenant's flush and fsync, those before it and those after -- so it grew with the tenants a pass synced, and one tenant's slow fsync held up every tenant behind it and the next pass. So did a writer holding one tenant's lock through a spill or a long block, since the pass looked at each under its read lock first, and an idle tenant's close on the loop's thread between passes, a checkpoint of its graphs among it.

**The stall that showed it.** Runs 43696612848 and 43697476087 cut a node of two tenants under `--sync 250` ([below](#a-power-cut)). Its pages came 272 to 282 ms apart at the median, the file guest's 256 to 259 beside them, and a write was handed over 151 to 162 ms after its answer at the median against the file's 132 to 135: each tick carried two tenants' syncs. Once a run the pass stalled. At the first run's second cut `a`'s pages came 717 ms apart and `b`'s 708, in one pass -- `b`'s reader had begun four pages before `a`'s -- the readers' turns 32 and 44 ms at most; at the second run's fifth, 514 and 707 ms, between the 95th and 96th page of each, the turns 42 and 188 ms. Writes were handed over 772 and 734 ms after their answer, past the 650 ms bound, where the file guest's longest over the same runs was 373 ms. Both tenants' gaps stretched in the same pass: the pass was held up, not one tenant's reading. A green run uploads no server log, so what held it -- one tenant's fsync of its file where its sync log began a generation, the runner's disk under `blklogwrites` (the first run's stall came in a boot it logged), the host itself (its reader 188 ms late at the second) -- is not known; what the pass did with it is: every tenant behind it and the next pass waited it out.

**Now.** A thread of the node's (`fennec-syncer`, started before the listener's thread under `--sync <ms>`) wakes every interval and looks at each open tenant under `try_read`: a dirty one not already syncing is queued for a sync thread (`fennec-sync`), one whose lock a writer holds queued unlooked at, and the thread goes back to sleep without waiting for any. A sync thread runs the pass's own sync (`sync_now`): the flush under the tenant's write lock, the fsync after it, no stream waited for, a failed fsync handed to `Database::fail`, so a storage error stops the tenant's writes as before. A thread is made only where more tenants wait than threads are free, at most 16 (`syncs::THREADS`), and waits for the next once its tenant is synced. A tenant still queued or syncing at a tick is passed over (`Tenant::syncing`, let go however its sync ends), so no tenant waits for another's sync, nor two syncs of its own pile up: its next sync comes with the next tick, an interval after the tick that began the last, where a file's comes an interval after its last ended -- a tenant's window is a file's, or less. A slow tenant holds one thread; a writer's long hold of one tenant's lock holds that tenant's thread alone; an idle close and its checkpoint, on the loop's thread, hold no sync. The pass in turn stays for a caller that must see the writes down before it goes on: Windows' shutdown, which syncs before each of its waits (`before_the_deadline`). The shutdown is [the one shutdown](#the-one-shutdown) still: the loop's thread looks at the flag every `LOOK`, ends the held writes and the connections while the syncs go on, then syncs and checkpoints every tenant under locks it keeps to the exit (`Tenants::shutdown_with`); the syncer queues nothing once that began (`shutting_down`), and the shutdown ends the sync threads before it keeps a tenant's lock (`Syncs::close`): nothing more is handed over, what was queued is let go for the shutdown's own sync, and a sync under way is waited for. Left running, one reached for a lock the exit keeps -- handed over by a tick in flight, its look finding the lock taken -- or for the write lock again to note a failed fsync, and waited for good, holding its tenant. A sync's panic, or the syncer's, is caught and stops the node through that shutdown, as the scheduler's does; on the loop's thread it ended the process with no final sync of any tenant.

**Measured** (`make node-sync-bench`: a node of N tenants, each written 100 times a second -- 50 for 64 -- by a process of its own, its pages and its writes' handover read from `/_changes` as the rig reads them, over 20 s; main's build and this one in turns, two rounds, the ranges below; an M1 under macOS, whose `F_FULLFSYNC` takes some 4 ms, its 1-minute load at 28 to 180 from other work throughout -- it never fell under 10 -- so these are the two builds side by side under the same noise, not an idle machine's):

| a node of | pages apart, main: p50 / p99 / max (ms) | this build | handed over, main: p99 / max | this build |
|---|---|---|---|---|
| 2 tenants | 264-270 / 273-323 / 274-336 | 255-256 / 285-348 / 293-355 | 278-302 / 291-360 | 276-286 / 311-373 |
| 8 | 301-304 / 358-381 / 371-397 | 256 / 283-297 / 300-526 | 334-339 / 404-406 | 281-303 / 312-543 |
| 32 | 415-419 / 526-528 / 537-571 | 255-257 / 312-349 / 347-415 | 473-489 / 554-689 | 300-331 / 375-445 |
| 64, 50 writes a second each | 565-614 / 706-1 203 / 729-1 274 | 257-258 / 446-470 / 617-784 | 658-1 095 / 749-1 310 | 414-433 / 651-806 |
| 8, one putting a 17 MB batch a second, each a spill under its lock | 293-297 / 559-631 / 578-659 | 256 / 278-287 / 286-295 | 449-509 / 585-674 | 281-286 / 301-314 |
| 8, one putting 512 KB batches back to back | 308-318 / 368-421 / 391-429 | 256 / 294-301 / 300-324 | 344-398 / 425-464 | 295-298 / 324-358 |

A pass's tenants added to every tenant's gap -- 2 tenants 8 to 15 ms at the median, 8 tenants 45 to 48, 32 tenants 158 to 164 -- and one slow tenant's spill under its lock to every other's p99 (559 to 631 ms); synced each on its own, every tenant's median gap is the interval and a few milliseconds, as a file's, whatever the count or the slow tenant beside it. The slow tenant's own writes waited for its own syncs either way (handed over 1.8 to 2.9 s after their answer with a spill landing each second). The one 526 ms gap at 8 tenants was one run's alone, at a load of 49.

**Rejected.** Syncing a pass's tenants at once and waiting for them all keeps the next pass waiting for the slowest: one slow tenant still holds every other's next sync. A thread for each open tenant, a file's syncer apiece, is a thread for every tenant a node holds open -- thousands -- to give what a few threads give. Each tenant due its own interval after its last sync ended, `run_syncer`'s own count, starts no sync sooner than the tick does: a sync ending within the interval is followed at the next tick, sooner than an interval after its end, and one running past it at the first tick after, sooner again.

**What it costs.** A thread more under `--sync <ms>`, the syncer, and up to 16 sync threads, made only where more tenants wait than threads are free: a node of a few busy tenants holds a few. Each tenant is fsynced once an interval, as a file is, where the pass fsynced it once an interval and a pass: at 32 tenants a page every 255 to 257 ms against 415 to 419, some 60 % more fsyncs a tenant, each covering fewer writes. No write waits on a sync, so the writes go as fast: back to back (`--rate 0`, three writers a tenant, three rounds), a node of 8 tenants took 8 237 to 8 945 writes a second against main's 6 998 to 8 661, and a file's server, whose syncer this leaves as it was, 4 615 to 5 214 against 4 332 to 6 753 -- the spread of a machine at a load of 19 to 86, no difference beyond it. Written back to back, the tenants' tail stays: pages 458 to 470 ms apart at p99 and 556 to 566 at most, against main's 391 to 524 and 429 to 599, at the median 255 to 257 against 287 to 292. Two changes for that tail were measured and left out, a node of 8 tenants written back to back by four writers each, three rounds a pair: the sync thread's look and flush under one write lock, its look under the read lock first waiting behind every writer queued (the lock lets a waiting writer go first) -- pages 369 to 436 ms apart at p99, against 337 to 372 as built -- and a tick that found a tenant syncing made up by its thread as that sync ends, 351 to 1 083 against 357 to 412 without it: neither moved it, the machine's own contention, its load 32 to 60, setting it. The code: 11.6 KB of `__text` on macOS arm64 (4 886 656 to 4 898 304 bytes), and the Linux release binary 8 264 272 to 8 297 040 bytes, its segments rounded to pages.

**Held by** fennec-http's `a_tenant_slow_to_sync_holds_none_of_the_others` -- one tenant's fsync held by the test between its flush and its fsync, the others' writes waited for on their feeds as `/_changes` reads them, a tick passing the held one over -- and `a_tenant_whose_writer_holds_its_lock_holds_none_of_the_others`, the test holding one tenant's write lock as a spill would: each failed at its 30 s bound against the pass in turn run on one thread, and passes. fennec-server's `a_nodes_interval_sync_flushes_each_tenant_in_background` holds the node's start to the syncer: three tenants' writes leave the node's buffer under `--sync 50` with no shutdown and no further write. On the rig, run 43703173704 cut a node of four tenants, each held to the window: pages 248 to 254 ms apart at the median, a write handed over 577 ms after its answer at most ([below](#a-power-cut)).

**After the shutdown.** A server exits holding the locks its shutdown keeps ([the one shutdown](#the-one-shutdown)); a process going on past `Tenants::shutdown` -- a test's -- holds databases whose write locks nothing will let go. Dropping the registry then hung for good, 5 runs of 5 with the syncer running, on main too: each tenant's drop took the write lock to sync (`Drop for Tenant`), and the compacter and the sweeper, upgrading their `Weak`s while it waited, parked behind it in `read` -- the process's one of each, every database after them neither compacted nor swept again. The shutdown now marks each tenant it keeps (`Tenant::kept_to_exit`), whose drop syncs nothing: its last sync ran under the lock the shutdown keeps, and no write landed after. The registry's drop says once that it let its tenants go so (`Drop for Tenants`), for a drop by code that is not a test's to be seen; a registry dropped with no shutdown before syncs each tenant as it goes, as ever. Each database is marked before its lock is taken (`held::kept_to_exit`), and the threads working beside the queries -- the compacter, the graph keeper, the sweeper, a linking or warming thread -- pass a marked one over as they upgrade it (`held::beside`), so one something else still holds -- a request or a follower parked at the lock -- parks none of them. A pass that already held the database as the shutdown took its lock waits for it still, as until a server's exit: let go, the lock would let it write after the final sync. *Rejected:* forgetting the slots' tenants on the drop, which kept every database and its file's lock for the process's life, and the compacter met each at its next pass; letting the locks go on the drop, which lets a write land after the final sync. *Costs* an atomic's load as a thread beside the queries upgrades a database, nothing on a request's path. *Held by* fennec-http's `a_registry_dropped_after_its_shutdown_lets_its_tenants_go_as_it_left_them` -- the syncer ticking every millisecond, a hand-over after the shutdown refused, the drop on a thread waited for 60 s at most, which without the mark never returned -- and `a_registry_dropped_without_a_shutdown_syncs_each_tenant`.

## A power cut

**Answered on CI, on Linux too.** `linux-power-cut.yml` is [Windows' power cut](windows.md#the-power-cut)'s counterpart, for what Linux alone does: a durable write there fsyncs [the sync log](#the-sync-log) beside the file and not the file, and the next open puts back what the file lost from it (`synclog::recover`). `tests/synclog.rs` stands in for a crash inside one process -- the process forgotten, the file cut back to the log's start by hand -- and the Windows rig cuts a system that keeps no log, so until this rig no power cut, a kernel's page cache and a disk's lost with it, had reached that path. Seven guests a run, a matrix entry each: one file under `--sync always` on ext4 in each of its three data modes -- `ordered`, as `mkfs.ext4` makes it, `writeback` and `journal` (below) -- and on XFS; one file under `--sync 250` -- the default -- on ext4, held to what that promises (below); and a `--dir` node on ext4 followed by a standby on the runner, of two tenants under `--sync always` and of four under `--sync 250` (below). It runs by hand (`workflow_dispatch`), weekly, and on a push to main touching the durability code -- the paths Windows' runs on, the rig's own files among them, the server's syncer (`fennec-server`'s `durability.rs`), what a primary sends its replicas once an fsync covered it (`replication.rs`) and what `/_changes` hands a consumer as on disk (`cdc.rs`), which the `--sync 250` guest's check reads its syncs from -- never on a pull request, and is not a required check. The node's syncs under `--sync <ms>` are among them: the registry (`tenants.rs`), its sync threads (`tenants/syncs.rs`) and `fennec-server`'s `main.rs`, which starts them ([a node's syncs](#a-nodes-syncs)). A release needs its last run green (RELEASING.md).

**The rig** (`.github/linux-power-cut/`; the load, the check and the replay are `.github/power-cut/`'s, which Windows' rig runs too). The runner's KVM, as Windows'. QEMU runs Ubuntu 24.04's cloud image -- a dated release Canonical keeps, its SHA-256 the one that release's `SHA256SUMS` names, not cached -- set up by cloud-init from a seed disk (`user-data`): one user the host reaches with a key made by the run, nothing installed or upgraded, the timers that would write on their own (apt's, `fstrim`, snapd's) off, and GRUB's recordfail wait -- 30 s at a menu no one sees, after every boot that did not finish -- set to none and put on disk by a clean restart before any cut. The server is this commit's, built as `release.yml` builds the release's (`x86_64-unknown-linux-musl`, the release profile) on a runner beside the guest's first boot and handed over as an artifact: `fennec-server --file /d/t.fennec --http 0.0.0.0:8080 --sync always` (`--sync 250 --cdc` in the default's guest, `--dir /d/n` under `--sync always` or `--sync 250` in the nodes'), its tokens made by the job, copied in as files and read into `FENNEC_HTTP_TOKEN` -- and a node's `FENNEC_ADMIN_TOKEN` and `FENNEC_REPLICATION_TOKEN` -- never printed. `/d` is a disk of its own -- ext4 as `mkfs.ext4` makes it (`data=ordered`), its inode tables and journal written at the making so that nothing initialises behind the cuts, the same mounted `data=writeback` and `data=journal` (`MOUNT_OPTIONS`, held at every mount to what `findmnt` shows, or the cut fails as the rig's: a mount that fell back would test `ordered` under another mode's name), and XFS as `mkfs.xfs` does, a matrix entry each -- and mounted by the rig after each boot, so the journal's recovery is a line the summary keeps (`MOUNT`). Both disks are virtio disks with a write cache the guest must flush (`write-cache=on`: the guest's `queue/write_cache` says `write back`), each image opened `O_DIRECT` on the host, so a guest's flush is the host's `fdatasync`. A cut is `kill -9` of QEMU: the guest's memory, its page cache and the file system's uncommitted journal go with it, and what the guest handed its disks stays -- but for a write into a cluster QEMU had just allocated in the image and not yet written the table of, a write no flush covered, which a volatile cache may lose too. The rig's own files are synced before the first cut.

**The load and the check** are Windows' (`client.py`, [described there](windows.md#the-power-cut)): five cuts a run, each at a time drawn from 2 to 30 s after the first acknowledgement, seeded by the run, each held to what the check before it found, and each held to landing mid-write: a cut with no write in flight -- none sent before the host's clock read after `kill -9` and left unanswered, the check's `INFLIGHT` lines -- landed between writes and held nothing to one cut short, and fails as the rig's (`RIG`), Windows' cuts too. Under `--sync 250` a write is in flight for well under a millisecond, so a kill may find every writer between two writes by chance (run 43594792647's first cut did, every other check passing), and the cut still held the server to the acknowledged writes no `/_changes` page had covered, which the window checks: there the rule fails only a cut with no write in flight and none of those left (`Check.unsynced`). A denser load would only lower the odds, and counting a write tried just after the kill as in flight would count one that found QEMU gone. What may be beside the file after a cut differs: its `.lock`, unlocked; the sync log (`.sync`); a stopped compact's image, `.beside` or `.compacting`, which the next compact writes anew; ext4's `lost+found`; and in a node's directory each tenant's own, its identity (`<t>.instance`) and `schedules.hint`, or the `schedules.hint.writing` it is written again from. Any other file fails the run.

**`--sync 250`, the default.** It does not promise every acknowledged write: the server answers a write before it is on disk, and the syncer (`run_syncer`) sleeps 250 ms after each sync, takes the write lock long enough to take what was written, lets it go and fsyncs. A write landing just after a sync took its writes waits out that sync, the sleep and the next, so a cut may take what was answered within 250 ms and two syncs' time of it, and nothing answered before. The guest's server runs with `--cdc`, which keeps the feed a primary keeps for its replicas and hands a consumer only what an fsync covered (`Tee::flush` marks it durable once the fsync returned): it changes nothing the file holds -- its one cost on disk, an fsync of the file as the open finds it, has nothing to cover after a power cut -- and lets the rig see each sync. A thread of the client reads `/_changes` as a long poll and notes when each page's head came and what it held, before any writer starts. The check holds the database to three things beside a file's (whole documents, a batch all there or none, a keyed write there at most once and replayed where it is). Every write a page held is there, whatever its answer said. An acknowledged write gone was answered no longer before the cut than 250 ms and two syncs' own time, each allowed 200 ms (`SYNC_OWN`): 650 ms, a fixed bound. A sync's own time, a gap between pages less the 250 ms of sleep, was 7 to 9 ms at the median over the ten cuts of runs 43438990710 and 43462058075 and 177 ms at the longest (a gap of 427 ms), and two back to back took 282 ms (an acknowledged write handed over 532 ms after its answer): the runner's disk, early in a cut after a boot or for a moment mid-load. The 118 ms the bound leaves past that is its only slack; the host's clock, read before the kill -- earlier than the guest's end -- and at a write's answer -- later than its landing -- only shortens the span measured. A first bound of 50 ms a sync, 350 ms, passed the second run with a write lost 316 ms before its cut, and a cut at the end of its fourth cut's slow syncs would have failed a server doing what it promises. No page may come more than a second after the one before it or the first acknowledged write, nor the cut a second after the last page (`GAP_CEILING`): a second is four times the interval and more than twice the longest gap seen, so a gap past it names a sync 750 ms late, a syncer stopped or stuck. And at the median the pages must come no more than 350 ms apart, a median sync's own time allowed 100 ms (`SYNC_MEDIAN`): a few slow syncs leave the median where it was, and a syncer late at every sync -- every half second, whose losses stay under the bound -- fails it at every cut. None of these numbers is read from the pages being judged. The first version of this check read its bound from them -- the longest of the last four gaps and the gap open at the cut, less 250 ms each, and 50 ms of slack -- and the bound grew with the stall it was to find: #627's review fed it a syncer stopped 3 s before the cut, its acknowledged writes lost from 3.0 s before it, which passed against 3 058 ms, and one syncing every second, its writes lost from 907 ms before the cut, against 1 708 ms. `test_client.py` holds the check to failing both, a syncer every half second and one stuck for a second and a half mid-load, and to passing two slow syncs as the second run met them; `ci.yml`'s `fmt` job runs it on every pull request. And what the cut kept is a prefix of the writes: every acknowledged write it took landed after every one it kept, by the change each was answered as (`Fennec-Seq`) -- the open cuts a torn tail at its first damaged record, never one in the middle. The load is a file's without its spills and compacts: a compact's image is fsynced before its rename and makes every write before it durable, which a compact every fifth of a second would have done in the syncer's place, and a spill's 20 MiB page held the reader past the next sync; `--auto-compact`'s compacts run beside it all the same. A sync holds a quarter of a second's writes, past the 64 KiB [the sync log](#the-sync-log) takes, so the file is fsynced itself and the log is exercised by the other guests; a run in which no cut took an acknowledged write fails as the rig's, having held nothing to the window. The two cuts that rebuild the disk as of its last flush stay, though both runs' found just what the plain cuts did (247 and 120 writes gone, then 502 and 589): a plain cut keeps what the guest handed the disk, flushed or not, so only the rebuilt disk holds a page's writes to the flush its fsync asked for -- a sync that wrote and did not flush would pass every plain cut.

**A node and its standby.** A `--dir` node (`fennec-server --dir /d/n --sync always`) serves two tenants, made through `/_admin/tenants`: `a` takes a file's whole load, and `b` beside it single puts, batches with keyed writes and rewrites -- no spill nor compact to hold them up, so it makes about half as many writes again as `a`. A standby node on the runner (`--replica-of http://127.0.0.1:8080`, through QEMU's port forward, listening on a port the system picked) follows each, made there as the router makes a tenant on both, and a cut leaves it running, its followers dialing the node again until it is back. After a cut, before the node is started, the client reads the standby as the cut left it: each tenant's change (the `Fennec-Seq` of its `/_changes`), its history, and a digest of each row. Then the node opens the files and each tenant is checked as a file is -- every acknowledged write there, each answered as durable under `--sync always` -- and the standby's copy is held to the tenant as it came back: its change no later than the tenant's, each document the tenant's, each rewrite and spill at the tenant's version or before it, each key the tenant's. And it is held near the node's tip: the acknowledged writes above the change it held -- each answered as durable, what a promotion at the cut would have lost -- are counted (`STANDBY`), and the oldest of them answered more than a second before the cut fails the cut as the rig's (`STANDBY_LAG`). A durable write is answered once every stream keeping up has put it into its socket, and a stream that has not within a second is let lag (`SENT_WAIT`), so a standby a second behind is one the node had stopped waiting for -- and one whose check would have held nothing of the node's last second to it. A primary sends a write once an fsync covered it, so it comes back holding every write a replica was sent ([replication.md](replication.md#replication)); a copy ahead of it would be sent an image once it followed again, which would hide the difference, so it is read before the node is back. Then the client waits for the standby to reach the change the check left the tenant at (`Fennec-After`, `Fennec-Wait` 30 s, four times at most) and finds the two the same, row for row. The node holds a tenant's lock while the tenant is open, so the second server is tried over `a.fennec` after the check opened it. A cut that loses the disk's cache checks the rebuilt disk alone: the disk QEMU left, served first, would send the standby writes the rebuilt one never had. The standby is not promoted: a power cut takes the node's sockets with what they had not sent, so a write answered as durable may not have reached it, which a promotion would lose by design ([replication](../../site/content/docs/replication.html), "a machine that dies takes its sockets with it"); the node reopened loses none.

**Held to a kill first.** Before the rig ran them, both checks were run on a laptop against a process killed, which keeps the page cache (`client.py` as the rig runs it, macOS, a debug build). `--sync 250` killed twice lost 7 and 10 acknowledged writes, still in the server's buffer, kept a prefix and passed, its pages one every 261 ms and an acknowledged write handed over 133 ms after its answer at the median, 288 ms at most; the node and its standby passed too. A file cut short after the kill -- what a lost fsync would leave -- failed each: under `--sync 250` naming every write a page had held that was gone, and on the node the acknowledged writes gone, the standby holding writes the tenant had not, and the rows apart.

**What the sync log held.** Before the server opens the file after a cut, `synclog.py` reads the log in the guest as `recover` reads it -- the header's hash, the file's device and inode, the fingerprint of the 4 KiB before the generation's start, each entry whole by its hash -- and says how many entries the file does not hold as written and how many of their bytes lie past its end: what the open writes back (`SYNCLOG` in the summary). The guest's page cache holds an append until writeback, 5 to 30 s under the kernel's defaults, so a cut finds the file shorter than what was answered and the log holding the rest; a cut whose log puts nothing back tested the file's own fsync instead, which the log's end (a full log, a sync past an entry, a compact's new file) asks for.

**A disk whose cache was lost**, as on Windows: cuts 2 and 4 rebuild the data disk as of the last flush the guest asked for, from the log `blklogwrites` kept of it since the boot before, and both disks are checked; the root disk stays as QEMU left it -- the database's disk alone loses its cache. A write the server answered under `--sync always` was the sync log's `fdatasync` first, which ext4 and XFS end with the device's flush, so it is before that flush in the log. The logged disk merges no adjacent requests (`request-merging=off`): virtio-blk merges them up to IOV_MAX buffers, 1024, and `blklogwrites` writes a request's log entry as its buffers and two of its own, which `pwritev` refuses past IOV_MAX -- QEMU 8.2's `blk_log_writes_refresh_limits` leaves `max_iov` as its child's. Run 43010736756, the first, met it: four adjacent writes of a compact's image, 256 buffers each, merged into one of 1024, its log entry refused, the guest answered EIO -- the compact answered 500, rightly, every acknowledged write there after the cut -- and the log left with a hole where the entry's place was taken, which the replay's walk stopped at, 4 531 entries of the 34 581 its superblock counted. Replayed, that log would have rebuilt a disk older than its last flush, an acknowledged write gone by the rig's doing; so `replay.py` refuses a log that breaks off further back than a queue's depth from its count (only writes still in flight can be missing at a kill), and a cut whose boot logged a disk's I/O error fails as the rig's (`RIG`), before any check.

**Evidence.** A server that does not open the file after a cut, or a check that fails, keeps the files the cut left as they were before the open -- a copy taken after the mount, before the server starts -- and the file's tail and the sync log's header as hex (`dump.sh`), in the run's uploaded logs. Windows' run 42533808035 kept no file, and its cause was read from the error alone.

**Measured**, run 43012743470 on ext4, the first green. The run took 9 min 56 s from its dispatch; the server's build 1 min 50 s beside it, ready a second after the guest asked, and the guest's job 4 min 53 s. The image downloaded in 3 s (6 s in run 1), the first boot was ready in 55 s, the clean restart in 23 s, and after a cut the guest answered SSH in 13 to 16 s, the server `/_health` 1 s after its start, and each check took 0 to 2 s. The load acknowledged 920 to 1 290 writes a second under `--sync always` with compacts back to back -- Windows' guest 570 -- 22 339 in cut 3's 20.25 s; a spill took 0.47 to 0.60 s and a compact 0.04 to 0.56 s. A compact was in flight at two cuts of five and a spill at four, each whole or not there; 108 850 acknowledged documents were there after the fifth. At four cuts the sync log named the file and held what it lacked -- 83, 48, 26 and 1 entries, 218, 127, 78 and 6 KB past the file's end -- and the open put it back; the fifth, cut 2, came 0.1 s into a compact and found no log, the old file's removed and the new file's not yet begun: the compact's image, synced before its rename, held every write answered. A lost cut's log held 7 031 and 12 072 entries, 3 197 and 5 374 of them flushes, 0.5 and 1.3 GB of writes, replayed in 3 s at most; the writes after the last flush were one and none, as on Windows: `--sync always` flushes so often that little waits in the cache. ext4 printed `recovery complete` at every mount after a cut (printed, not asserted). **XFS**, run 43017158423, beside ext4 green again: after a cut the guest answered SSH in 21 to 22 s and XFS printed `Starting recovery` and `Ending recovery` at every mount (printed, not asserted); the load acknowledged 440 to 990 writes a second, slower as the file grew (a compact took up to 1.8 s and a spill 1.5 s); a compact was in flight at two cuts and a spill at four; and at all five the sync log held what the file lacked -- 5 to 99 entries, 11 to 252 KB past its end -- which the open put back. 133 930 acknowledged documents were there after the fifth. Its lost cuts' logs held 2.2 and 3.1 GB of writes, nothing after the last flush.

**The default mode and the node**, run 43438990710 (dispatched from aff77661, which still carried the arm64 probe; the probe is gone from 911b6ca1), green on all four guests: XFS and ext4 under `--sync always`, ext4 under `--sync 250`, and ext4 under a `--dir` node with its standby. Under `--sync 250` each of the five cuts lost 185, 247, 420, 120 and 259 acknowledged writes. The oldest write gone was answered 84, 114, 173, 57 and 116 ms before its cut: within the first check's bounds, read from the pages (309 to 314 ms), and within the fixed 650 ms that replaced them. Every write `/_changes` had handed over was there, and every cut kept a prefix. Syncs came every 257 to 259 ms at the median (261 to 271 ms at most), the reader's turn 24 to 34 ms at most; an acknowledged write was handed over 133 to 134 ms after its answer at the median, 268 to 273 ms at p99 and 275 to 284 ms at most. On the node, ten tenant checks (two tenants, five cuts, cuts 2 and 4 on the disk as of the last flush) passed: no torn block, no write answered as durable lost. The standby, read as each cut left it, never held a change its tenant lacked: it was behind the node as it came back by 0 to 51 changes (`a`: 51, 45, 0, 0 and 45; `b`: 2, 9, 0, 0 and 9, cut by cut), 40 of each of `a`'s 51 and 45s a spill in flight at the cut, which landed on the node and had not reached the standby; and once the node was back it held the same rows as the tenant within 3.0 s of the check asking, 0.0 s at five of ten. That run did not yet count the acknowledged writes the standby lacked, nor hold it near the tip: both came after it (below). The cuts came 2 to 30 s into the load on a runner's disk; these are five cuts a guest, not a distribution.

**Held to fixed numbers**, run 43462058075, dispatched from d17864c9 -- its window's bound still 350 ms and its median not yet held; the numbers above were set from this run's figures, which pass them -- green on all four guests, every cut with 4 to 6 writes in flight. Under `--sync 250` the cuts, 3.7 to 22 s into the writes, lost 652, 502, 54, 589 and 794 acknowledged writes, the oldest answered 248, 200, 32, 234 and 316 ms before its cut; every write `/_changes` had handed over was there, and every cut kept a prefix. The pages came 257 to 259 ms apart at the median and 261 to 427 ms at most -- the longest early in a cut, the first few syncs after the boot (366 ms between the third cut's first two pages), and for a moment mid-load (427 ms, the fourth) -- the gap open at the fifth cut 302 ms, the reader's turn 20 to 27 ms at most; an acknowledged write was handed over 133 to 143 ms after its answer at the median, 269 to 382 ms at p99 and 275 to 532 ms at most. On the node all ten tenant checks passed, and the standby, read as each cut left it, held every write the node had answered as durable: none was above its change at any cut, so a promotion at any of them would have lost none. It was 0 to 10 changes behind the node as it came back (`a`: 0, 0, 1, 0 and 2; `b`: 1, 0, 4, 0 and 10, cut by cut), writes in flight at the cut that landed on the node, and once the node was back it held the same rows within 3.4 s of the check asking (`a`: 0.0, 0.0, 0.0, 2.0 and 3.4 s; `b`: 0.0 s at all five). Under `--sync always` the sync log held what the file lacked at four ext4 cuts of five, the first finding none, and at all five on XFS. A second run's five cuts a guest, not a distribution.

**ext4's other data modes**, runs 43696612848 and 43697476087, dispatched from b6da7c00 and f5d2c8a3, beside the four guests before them, each green again in both. What each mode changes for a durable write: under `data=ordered` a file's new blocks reach the disk before the journal commits the length that covers them; under `data=writeback` nothing orders the two, so a length may reach the disk before the bytes -- with the kernel's defaults (6.8 here: delayed allocation and `dioread_nolock`) an append's new blocks are unwritten extents until their write completes, so what a cut can leave past the bytes is zeros, which the open cuts as a torn tail, never an earlier file's bytes; and under `data=journal` a file's bytes go through the journal with its metadata, delayed allocation off (`findmnt` shows `nodioread_nolock,nodelalloc,data=journal`). Both passed every check at all ten cuts, rebuilt disks included: every acknowledged write there, no batch half there. Each guest's job took 5 min 49 s to 6 min 42 s, the ordered one's 5 min 53 s and 6 min 41 s. Under `data=writeback` the load acknowledged 828 to 1 163 writes a second -- the ordered guest beside it 500 to 1 869 over the same runs, one runner's disk to the next more than the mode -- a compact in flight at three cuts of the first run and a spill at three. At all ten cuts the sync log held what the file lacked -- 11 to 81 entries, 37 to 197 KB past the file's end -- and the open put it back. At nine the file the disk kept ended where the log's generation began, at its own last fsync, every entry past it; at the tenth it ran 1 302 bytes past that, to a 4 KiB block's end, those bytes not the log's -- as the ordered guest's own fourth cut did, 4 045 bytes -- and the open wrote them over. 199 171 and 182 076 acknowledged documents were there after the fifth; the lost cuts' logs held 0.9 to 3.8 GB of writes, 19 (23.9 MB) after the last flush once, 1 once and none twice. Under `data=journal` the load acknowledged 85 to 373 writes a second, a compact's image and a spill's 20 MiB journaled whole -- a spill took up to 1.8 s and a compact 3.4 s -- with a compact and a spill in flight at all five cuts of the first run; 35 803 and 60 889 acknowledged documents were there after the fifth, and the lost cuts' logs held 0.9 to 2.8 GB, 4 (4.2 MB) and 16 (20.7 MB) writes after the last flush in the first run, none in the second. No cut there left an entry for the open to put back: the log was live at each, naming the file, 3 to 76 entries whole, and the file held every one. A `write()`'s bytes join the journal's running transaction as it returns, and the log's `fdatasync` commits that transaction, the file's bytes with the log's. The summary, which fails a run in which no cut put an entry back as one that never exercised the log, failed the first run so, every check green; under `data=journal` it holds the guest to a cut having found a live log naming the file with entries in it instead (`JOURNALED` in `vm.sh`), the durable writes having gone through it, which the second run passed.

**A node under `--sync 250`**, the same two runs. The node of two tenants and its standby, the node under `--sync 250`: each tick its own pass synced every dirty tenant in turn, its flush under the tenant's write lock and the fsync after it (`Tenants::sync_dirty`, called from `fennec-server`'s main loop), where a file has `run_syncer` -- the node these runs cut, before [each tenant had a thread of its own](#a-nodes-syncs). Each tenant is held to the `--sync 250` guest's check and the standby to the node's; a node's tenants keep the feed `/_changes` reads with the replication token, no `--cdc`. Green at all twenty tenant checks; its job took 7 min 25 s and 7 min 4 s. A tenant acknowledged 1 436 to 2 045 writes a second, and a cut took 39 to 638 of a tenant's acknowledged writes (`a`: 172, 39, 395, 70 and 330, then 393, 435, 70, 570 and 337; `b`: 146, 64, 345, 89 and 345, then 418, 426, 98, 638 and 325), the oldest answered 49 to 292 ms before it; every write a page held was there, and every cut kept a prefix. A tenant's pages came 272 to 282 ms apart at the median -- the file guest's 256 to 259 beside it, a pass syncing two tenants -- and 291 to 380 ms at most, but for one stall a run: at the first run's second cut 717 and 708 ms between pages, both tenants' at once, the reader's turn 44 ms at most, in the boot whose writes `blklogwrites` logged; at the second run's fifth 514 and 707 ms, the reader's turn 188 ms there -- the host's reader late as well as the node. Writes were handed over up to 772 and 734 ms after their answer, past the 650 ms the window allows a loss, so a cut at either stall's end would have failed the check; the file guest's longest over the same runs was 373 ms. A tenant's sync waited for every dirty tenant synced before it in the pass, so a node's window was 250 ms and its pass's own time, which grew with the tenants a pass synced: the bound held two tenants with no slack, and a node of many was not held to it -- which is why each tenant is now synced on a thread of its own ([a node's syncs](#a-nodes-syncs)), the node of four below. The standby, read as each cut left it, held the node's change as it came back at all twenty: the writes it lacked were the cut's losses on the node, none on the node's disk, so a promotion at the cut would have lost just what the node did. Under `--sync <ms>` its lag is read from when a write was on the node's disk, the first page reaching it (`standby_near`): nothing waits for its streams there, and a write is sent once a sync covered it. It held the same rows as each tenant within 1.2 s of being asked; 278 513 and 282 354, then 315 214 and 321 775, acknowledged documents were there after the fifth.

**A node of four, each tenant on a thread of its own**, run 43703173704, dispatched from 7a9793f9 -- run 43702806628 before it failed as the rig's at its first cut, `kept_names` naming `a`'s and `b`'s files alone -- beside the six guests before, each green again. The `dir250` guest serves four tenants (`NODE_TENANTS`, the matrix's `tenants`), `a` taking the whole load and the others `b`'s, each [synced on a thread of its own](#a-nodes-syncs). Green at all twenty tenant checks; its job took 6 min 27 s. A tenant acknowledged 806 to 853 writes a second, the node some 3 300 -- the runner's four processors run four loads beside the guest -- and the cuts, 11 to 20 s into the writes, took 7 to 141 of a tenant's acknowledged writes (`a`: 136, 69, 13, 96 and 141; `b`: 136, 32, 20, 121 and 117; `c`: 135, 55, 7, 121 and 109; `d`: 138, 48, 10, 100 and 120), the oldest answered 27 to 162 ms before its cut; every write a page held was there, and every cut kept a prefix. A tenant's pages came 248 to 254 ms apart at the median -- the file guest's 257 to 258 beside it in the same run, the two tenants' 272 to 282 before -- and 275 to 298 ms at most, but for five gaps of 501 to 530 ms, each one tenant's alone (`c` at the second cut, `d` at the third, `b` at the fourth, `a` and `b` at the fifth), the readers' turns 46 ms at most: two ticks, a sync of that tenant's that had not ended by the next tick, which passes a tenant still syncing over. What held those syncs is not known, a green run keeping no server log; held so, the pass in turn would have held every tenant behind them. A write was handed over 154 to 165 ms after its answer at the median, 304 to 481 ms at p99 and 577 at most, against the two tenants' 772 and 734 before; the file guest's longest in the same run was 279. The standby held the node's change as it came back at nineteen checks and was one change behind it at the twentieth, the writes it lacked none on the node's disk, and held the same rows within 1.4 s of being asked.

**arm64 is not cut: its runners have no KVM.** A `ubuntu-24.04-arm` runner is a Hyper-V guest on Neoverse-N2 (Azure's kernel, 7.0.0-1017-azure), which Hyper-V starts at EL1: its kernel says `kvm [1]: HYP mode not available`, and there is no `/dev/kvm`, `modprobe kvm` or not (run 43438990710's probe). QEMU there would emulate the guest instruction by instruction (TCG), a power cut of an emulated machine whose timings, and so whose cuts, are not a real one's; no such guest is run in arm64's name. What arm64 has instead: the release's aarch64 build is the same code, its NEON kernels run in `test (macos-latest)`, and `desktop.yml` drives the released server on `ubuntu-24.04-arm` -- none of them a power cut. A cut of an arm64 guest needs a runner with nested virtualisation -- a larger runner that offers it, or one of the project's own -- or a machine at hand, and is the rest of roadmap row 113.

**Not covered.** ext4's options beyond its data modes -- `data=writeback` with delayed allocation off (`nodelalloc`) or `dioread_lock`, where an append's blocks are allocated as written and a length reaching the disk first can show an earlier file's bytes rather than zeros -- XFS's other options, `nobarrier` on either, and every other file system; `--sync <ms>` but 250, and 250 on XFS or with spills and compacts of the client's own; a node of more than four tenants under `--sync 250` (above), and one whose tenants outnumber its sync threads, each tenant's sync waiting its turn there; a node's archive (`fennec archive --tenants`), the router in front of it, a promotion after a power cut, a standby's own power cut and a single file's replica; the native library; arm64 (above) and other kernels; the root disk's cache; any subset of the writes after the last flush but none of them; a real disk's firmware -- a flush answered and not done, a sector torn inside a write. As on Windows, the host's own durability, which `kill -9` never tests.

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
Held so, a database is never dropped, so the shutdown asks for what its
drop did to the file, the sync log let go after the sync and checkpoint
(`Database::close_log`, [the sync log](#the-sync-log)); a new step after
the sync comes before it. The syncers slept a whole `--sync` interval before they looked, so
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
was cut -- the scheduler's thread ending, a file's or a node's, on a panic
outside any run (1, `stop_failed`: fennec-http hands it a failure, the
server the code; `a_scheduler_s_failure_ends_the_server_with_1`) and a
thread the start cannot make (2, as it exited before).
The mirror stores its code (`exit_with`), then tells that wait it has
stopped, so the wait is not its `STOP_WAIT` and the exit cannot come before
the code, and only then asks for the shutdown. Nothing waits before the
code either: the follower returns a failure at once, only a clean stop
waiting for PostgreSQL to let its slot go
([replication.md](replication.md#follow)), and the code is stored before
the line saying why. A first form of that wait ran on a failure too, up to
10 s, and a SIGTERM in it outlasted `STOP_WAIT` and exited 0, with a
checkpoint (`fennec-server`'s
`a_follower_that_fails_stops_the_server_with_its_code_before_it_waits`). A
follower whose thread panics goes the same way -- the release profile
unwinds -- by a guard its thread drops on an unwind too
(`mirror::Ending`), the code stored first: the thread's end let the wait
go and nothing more, and the server served a mirror that no longer moved
and exited 0 at its next shutdown, with a checkpoint
(`a_follower_that_panics_stops_the_server_with_its_code`). The waits before
the final sync run one after another: the held writes' answers
(`HELD_GRACE`, 2 s), the TLS ends (`conn::END_GRACE`, 0.5 s), then what
`before_shutdown` registered, in its order -- the traces' flush (2 s, where
tracing is on), the follower's wait (`STOP_WAIT`, 5 s). Each hook is given
the moment the shutdown began, and the follower's wait counts from there:
the follower began stopping then, at the same flag, beside the waits
before it. Counted from its own start, with `--follow` and tracing the
waits came to 9.7 s with `LOOK`, 0.3 s of docker's 10 left for the final
sync; now it begins 5.2 s after the signal at most
(`the_wait_for_the_follower_counts_from_the_shutdowns_start`). That is
`LOOK` and `STOP_WAIT` alone: the final sync waits besides for the write
lock -- a long statement, or the follower's own batch put where its wait
ran out inside one -- on Windows, past a close, a logoff or the system's
shutdown, for `sync_first`'s two fsyncs where they and the graces outlast
`STOP_WAIT`, and, where the syncer was inside a periodic fsync when the
signal came, for that fsync, the shutdown beginning only once it is
through. A SIGKILL
past the waits cuts the final sync and the checkpoint: what `--sync` left
unsynced, and the graph's image, which the next open links again. A shutdown writes a
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
Windows raises no `SIGTERM`, and the CRT's `signal` caught Ctrl+C alone:
Ctrl+Break, its `SIGBREAK`, ended the process by the default action, as
closing the console did, with no final sync. There
`install_signal_handlers` is kernel32's `SetConsoleCtrlHandler` in the
CRT's place (`durability/windows.rs`, [windows.md](windows.md#4-stopping-console-events-and-services)),
its handler run on a thread the system makes for it, Ctrl+C turned on
first (`SetConsoleCtrlHandler(NULL, FALSE)`: a process started in a group
of its own begins with it off). Ctrl+C and Ctrl+Break store `SHUTDOWN` and
return. The console's close -- its window shut, or *End Task* --, a logoff
in a user's session and the system's shutdown store a deadline and then
the flag, and hold the handler until the syncer's `process::exit` ends its
thread: their handler's return lets the system end the process at once,
and it waits 5 s for one at most. Those five seconds are the whole
shutdown's, and its waits came first -- the held requests' grace (2 s),
the TLS connections' (0.5 s), the hooks (an OTLP flush of up to 2 s, the
mirror's join) -- 4.7 s with tracing on, so under `--sync <ms>` or `off`
the buffered writes were lost where the system ended the process before
the final sync. Past a deadline the shutdown syncs first: before the
graces, again before the hooks (`windows::sync_first`, a node's tenants
through `before_the_deadline`), then its final sync and checkpoint as
before. Every write acknowledged before the event is on disk within `LOOK`
and an fsync; one acknowledged during the graces reaches it with the next
sync if the process is still there; a checkpoint cut short leaves the file
whole as a failed rewrite does. Ctrl+C and Ctrl+Break, with no timeout,
keep a signal's order. A logoff in session 0, the services', which it
reaches at any user's logoff, is passed on: a server run as a service
outlives it, as the JVM's handler passes it on where its window station is
not visible -- whether a service wrapper's child gets the default handler
a service's own process gets is unverified ([windows.md](windows.md#4-stopping-console-events-and-services)).
Ctrl+Break is the one event a process can send another, to a process
group (`GenerateConsoleCtrlEvent`, the group made by
`CREATE_NEW_PROCESS_GROUP`), so on Windows it is a supervisor's `SIGTERM`
and the tests' (`support::Server::terminate`); a service runs under a
wrapper that sends it, or Ctrl+C, and waits before it kills (WinSW, NSSM).
`durability::windows`' unit tests call the handler as the system would:
each event's flag and deadline, the close's, a user's logoff's and the
shutdown's held until the exit is asked, and a file's writes synced first
only past a deadline; `shutdown.rs` and the rest of the server's tests
stop their servers through it on Windows CI.
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
