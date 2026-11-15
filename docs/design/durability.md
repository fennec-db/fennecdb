# Durability

The long form of the invariants [the root CLAUDE.md](../../CLAUDE.md), [crates/fennec-core/CLAUDE.md](../../crates/fennec-core/CLAUDE.md) and [crates/fennec-server/CLAUDE.md](../../crates/fennec-server/CLAUDE.md) list under *Durability*: a storage error stopping writes, buffered writes written behind the lock, the sync log, the power cut that holds the sync log on Linux, and the one shutdown (both added after the move). Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

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
lost harmlessly. An open hands its durable point to the sink
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

## A power cut

**Answered on CI, on Linux too.** `linux-power-cut.yml` is [Windows' power cut](windows.md#the-power-cut)'s counterpart, for what Linux alone does: a durable write there fsyncs [the sync log](#the-sync-log) beside the file and not the file, and the next open puts back what the file lost from it (`synclog::recover`). `tests/synclog.rs` stands in for a crash inside one process -- the process forgotten, the file cut back to the log's start by hand -- and the Windows rig cuts a system that keeps no log, so until this rig no power cut, a kernel's page cache and a disk's lost with it, had reached that path. Four guests a run, a matrix entry each: one file under `--sync always` on ext4, the same on XFS, one file under `--sync 250` -- the default -- on ext4, held to what that promises (below), and a `--dir` node of two tenants under `--sync always` on ext4, followed by a standby on the runner (below). It runs by hand (`workflow_dispatch`), weekly, and on a push to main touching the durability code -- the paths Windows' runs on, the rig's own files among them, the server's syncer (`fennec-server`'s `durability.rs`), what a primary sends its replicas once an fsync covered it (`replication.rs`) and what `/_changes` hands a consumer as on disk (`cdc.rs`), which the `--sync 250` guest's check reads its syncs from -- never on a pull request, and is not a required check. A release needs its last run green (RELEASING.md).

**The rig** (`.github/linux-power-cut/`; the load, the check and the replay are `.github/power-cut/`'s, which Windows' rig runs too). The runner's KVM, as Windows'. QEMU runs Ubuntu 24.04's cloud image -- a dated release Canonical keeps, its SHA-256 the one that release's `SHA256SUMS` names, not cached -- set up by cloud-init from a seed disk (`user-data`): one user the host reaches with a key made by the run, nothing installed or upgraded, the timers that would write on their own (apt's, `fstrim`, snapd's) off, and GRUB's recordfail wait -- 30 s at a menu no one sees, after every boot that did not finish -- set to none and put on disk by a clean restart before any cut. The server is this commit's, built as `release.yml` builds the release's (`x86_64-unknown-linux-musl`, the release profile) on a runner beside the guest's first boot and handed over as an artifact: `fennec-server --file /d/t.fennec --http 0.0.0.0:8080 --sync always` (`--sync 250 --cdc` in the default's guest, `--dir /d/n --sync always` in the node's), its tokens made by the job, copied in as files and read into `FENNEC_HTTP_TOKEN` -- and a node's `FENNEC_ADMIN_TOKEN` and `FENNEC_REPLICATION_TOKEN` -- never printed. `/d` is a disk of its own -- ext4 as `mkfs.ext4` makes it (`data=ordered`), its inode tables and journal written at the making so that nothing initialises behind the cuts, and XFS as `mkfs.xfs` does, a matrix entry each -- and mounted by the rig after each boot, so the journal's recovery is a line the summary keeps (`MOUNT`). Both disks are virtio disks with a write cache the guest must flush (`write-cache=on`: the guest's `queue/write_cache` says `write back`), each image opened `O_DIRECT` on the host, so a guest's flush is the host's `fdatasync`. A cut is `kill -9` of QEMU: the guest's memory, its page cache and the file system's uncommitted journal go with it, and what the guest handed its disks stays -- but for a write into a cluster QEMU had just allocated in the image and not yet written the table of, a write no flush covered, which a volatile cache may lose too. The rig's own files are synced before the first cut.

**The load and the check** are Windows' (`client.py`, [described there](windows.md#the-power-cut)): five cuts a run, each at a time drawn from 2 to 30 s after the first acknowledgement, seeded by the run, each held to what the check before it found, and each held to landing mid-write: a cut with no write in flight -- none sent before the host's clock read after `kill -9` and left unanswered, the check's `INFLIGHT` lines -- landed between writes and held nothing to one cut short, and fails as the rig's (`RIG`), Windows' cuts too. What may be beside the file after a cut differs: its `.lock`, unlocked; the sync log (`.sync`); a stopped compact's image, `.beside` or `.compacting`, which the next compact writes anew; ext4's `lost+found`; and in a node's directory each tenant's own, its identity (`<t>.instance`) and `schedules.hint`, or the `schedules.hint.writing` it is written again from. Any other file fails the run.

**`--sync 250`, the default.** It does not promise every acknowledged write: the server answers a write before it is on disk, and the syncer (`run_syncer`) sleeps 250 ms after each sync, takes the write lock long enough to take what was written, lets it go and fsyncs. A write landing just after a sync took its writes waits out that sync, the sleep and the next, so a cut may take what was answered within 250 ms and two syncs' time of it, and nothing answered before. The guest's server runs with `--cdc`, which keeps the feed a primary keeps for its replicas and hands a consumer only what an fsync covered (`Tee::flush` marks it durable once the fsync returned): it changes nothing the file holds -- its one cost on disk, an fsync of the file as the open finds it, has nothing to cover after a power cut -- and lets the rig see each sync. A thread of the client reads `/_changes` as a long poll and notes when each page's head came and what it held, before any writer starts. The check holds the database to three things beside a file's (whole documents, a batch all there or none, a keyed write there at most once and replayed where it is). Every write a page held is there, whatever its answer said. An acknowledged write gone was answered no longer before the cut than 250 ms and two syncs' own time, each allowed 200 ms (`SYNC_OWN`): 650 ms, a fixed bound. A sync's own time, a gap between pages less the 250 ms of sleep, was 7 to 9 ms at the median over the ten cuts of runs 43438990710 and 43462058075 and 177 ms at the longest (a gap of 427 ms), and two back to back took 282 ms (an acknowledged write handed over 532 ms after its answer): the runner's disk, early in a cut after a boot or for a moment mid-load. The 118 ms the bound leaves past that is its only slack; the host's clock, read before the kill -- earlier than the guest's end -- and at a write's answer -- later than its landing -- only shortens the span measured. A first bound of 50 ms a sync, 350 ms, passed the second run with a write lost 316 ms before its cut, and a cut at the end of its fourth cut's slow syncs would have failed a server doing what it promises. No page may come more than a second after the one before it or the first acknowledged write, nor the cut a second after the last page (`GAP_CEILING`): a second is four times the interval and more than twice the longest gap seen, so a gap past it names a sync 750 ms late, a syncer stopped or stuck. And at the median the pages must come no more than 350 ms apart, a median sync's own time allowed 100 ms (`SYNC_MEDIAN`): a few slow syncs leave the median where it was, and a syncer late at every sync -- every half second, whose losses stay under the bound -- fails it at every cut. None of these numbers is read from the pages being judged. The first version of this check read its bound from them -- the longest of the last four gaps and the gap open at the cut, less 250 ms each, and 50 ms of slack -- and the bound grew with the stall it was to find: #627's review fed it a syncer stopped 3 s before the cut, its acknowledged writes lost from 3.0 s before it, which passed against 3 058 ms, and one syncing every second, its writes lost from 907 ms before the cut, against 1 708 ms. `test_client.py` holds the check to failing both, a syncer every half second and one stuck for a second and a half mid-load, and to passing two slow syncs as the second run met them; `ci.yml`'s `fmt` job runs it on every pull request. And what the cut kept is a prefix of the writes: every acknowledged write it took landed after every one it kept, by the change each was answered as (`Fennec-Seq`) -- the open cuts a torn tail at its first damaged record, never one in the middle. The load is a file's without its spills and compacts: a compact's image is fsynced before its rename and makes every write before it durable, which a compact every fifth of a second would have done in the syncer's place, and a spill's 20 MiB page held the reader past the next sync; `--auto-compact`'s compacts run beside it all the same. A sync holds a quarter of a second's writes, past the 64 KiB [the sync log](#the-sync-log) takes, so the file is fsynced itself and the log is exercised by the other guests; a run in which no cut took an acknowledged write fails as the rig's, having held nothing to the window. The two cuts that rebuild the disk as of its last flush stay, though both runs' found just what the plain cuts did (247 and 120 writes gone, then 502 and 589): a plain cut keeps what the guest handed the disk, flushed or not, so only the rebuilt disk holds a page's writes to the flush its fsync asked for -- a sync that wrote and did not flush would pass every plain cut.

**A node and its standby.** A `--dir` node (`fennec-server --dir /d/n --sync always`) serves two tenants, made through `/_admin/tenants`: `a` takes a file's whole load, and `b` beside it single puts, batches with keyed writes and rewrites -- no spill nor compact to hold them up, so it makes about half as many writes again as `a`. A standby node on the runner (`--replica-of http://127.0.0.1:8080`, through QEMU's port forward, listening on a port the system picked) follows each, made there as the router makes a tenant on both, and a cut leaves it running, its followers dialing the node again until it is back. After a cut, before the node is started, the client reads the standby as the cut left it: each tenant's change (the `Fennec-Seq` of its `/_changes`), its history, and a digest of each row. Then the node opens the files and each tenant is checked as a file is -- every acknowledged write there, each answered as durable under `--sync always` -- and the standby's copy is held to the tenant as it came back: its change no later than the tenant's, each document the tenant's, each rewrite and spill at the tenant's version or before it, each key the tenant's. And it is held near the node's tip: the acknowledged writes above the change it held -- each answered as durable, what a promotion at the cut would have lost -- are counted (`STANDBY`), and the oldest of them answered more than a second before the cut fails the cut as the rig's (`STANDBY_LAG`). A durable write is answered once every stream keeping up has put it into its socket, and a stream that has not within a second is let lag (`SENT_WAIT`), so a standby a second behind is one the node had stopped waiting for -- and one whose check would have held nothing of the node's last second to it. A primary sends a write once an fsync covered it, so it comes back holding every write a replica was sent ([replication.md](replication.md#replication)); a copy ahead of it would be sent an image once it followed again, which would hide the difference, so it is read before the node is back. Then the client waits for the standby to reach the change the check left the tenant at (`Fennec-After`, `Fennec-Wait` 30 s, four times at most) and finds the two the same, row for row. The node holds a tenant's lock while the tenant is open, so the second server is tried over `a.fennec` after the check opened it. A cut that loses the disk's cache checks the rebuilt disk alone: the disk QEMU left, served first, would send the standby writes the rebuilt one never had. The standby is not promoted: a power cut takes the node's sockets with what they had not sent, so a write answered as durable may not have reached it, which a promotion would lose by design ([replication](../../site/content/docs/replication.html), "a machine that dies takes its sockets with it"); the node reopened loses none.

**Held to a kill first.** Before the rig ran them, both checks were run on a laptop against a process killed, which keeps the page cache (`client.py` as the rig runs it, macOS, a debug build). `--sync 250` killed twice lost 7 and 10 acknowledged writes, still in the server's buffer, kept a prefix and passed, its pages one every 261 ms and an acknowledged write handed over 133 ms after its answer at the median, 288 ms at most; the node and its standby passed too. A file cut short after the kill -- what a lost fsync would leave -- failed each: under `--sync 250` naming every write a page had held that was gone, and on the node the acknowledged writes gone, the standby holding writes the tenant had not, and the rows apart.

**What the sync log held.** Before the server opens the file after a cut, `synclog.py` reads the log in the guest as `recover` reads it -- the header's hash, the file's device and inode, the fingerprint of the 4 KiB before the generation's start, each entry whole by its hash -- and says how many entries the file does not hold as written and how many of their bytes lie past its end: what the open writes back (`SYNCLOG` in the summary). The guest's page cache holds an append until writeback, 5 to 30 s under the kernel's defaults, so a cut finds the file shorter than what was answered and the log holding the rest; a cut whose log puts nothing back tested the file's own fsync instead, which the log's end (a full log, a sync past an entry, a compact's new file) asks for.

**A disk whose cache was lost**, as on Windows: cuts 2 and 4 rebuild the data disk as of the last flush the guest asked for, from the log `blklogwrites` kept of it since the boot before, and both disks are checked; the root disk stays as QEMU left it -- the database's disk alone loses its cache. A write the server answered under `--sync always` was the sync log's `fdatasync` first, which ext4 and XFS end with the device's flush, so it is before that flush in the log. The logged disk merges no adjacent requests (`request-merging=off`): virtio-blk merges them up to IOV_MAX buffers, 1024, and `blklogwrites` writes a request's log entry as its buffers and two of its own, which `pwritev` refuses past IOV_MAX -- QEMU 8.2's `blk_log_writes_refresh_limits` leaves `max_iov` as its child's. Run 43010736756, the first, met it: four adjacent writes of a compact's image, 256 buffers each, merged into one of 1024, its log entry refused, the guest answered EIO -- the compact answered 500, rightly, every acknowledged write there after the cut -- and the log left with a hole where the entry's place was taken, which the replay's walk stopped at, 4 531 entries of the 34 581 its superblock counted. Replayed, that log would have rebuilt a disk older than its last flush, an acknowledged write gone by the rig's doing; so `replay.py` refuses a log that breaks off further back than a queue's depth from its count (only writes still in flight can be missing at a kill), and a cut whose boot logged a disk's I/O error fails as the rig's (`RIG`), before any check.

**Evidence.** A server that does not open the file after a cut, or a check that fails, keeps the files the cut left as they were before the open -- a copy taken after the mount, before the server starts -- and the file's tail and the sync log's header as hex (`dump.sh`), in the run's uploaded logs. Windows' run 42533808035 kept no file, and its cause was read from the error alone.

**Measured**, run 43012743470 on ext4, the first green. The run took 9 min 56 s from its dispatch; the server's build 1 min 50 s beside it, ready a second after the guest asked, and the guest's job 4 min 53 s. The image downloaded in 3 s (6 s in run 1), the first boot was ready in 55 s, the clean restart in 23 s, and after a cut the guest answered SSH in 13 to 16 s, the server `/_health` 1 s after its start, and each check took 0 to 2 s. The load acknowledged 920 to 1 290 writes a second under `--sync always` with compacts back to back -- Windows' guest 570 -- 22 339 in cut 3's 20.25 s; a spill took 0.47 to 0.60 s and a compact 0.04 to 0.56 s. A compact was in flight at two cuts of five and a spill at four, each whole or not there; 108 850 acknowledged documents were there after the fifth. At four cuts the sync log named the file and held what it lacked -- 83, 48, 26 and 1 entries, 218, 127, 78 and 6 KB past the file's end -- and the open put it back; the fifth, cut 2, came 0.1 s into a compact and found no log, the old file's removed and the new file's not yet begun: the compact's image, synced before its rename, held every write answered. A lost cut's log held 7 031 and 12 072 entries, 3 197 and 5 374 of them flushes, 0.5 and 1.3 GB of writes, replayed in 3 s at most; the writes after the last flush were one and none, as on Windows: `--sync always` flushes so often that little waits in the cache. ext4 printed `recovery complete` at every mount after a cut (printed, not asserted). **XFS**, run 43017158423, beside ext4 green again: after a cut the guest answered SSH in 21 to 22 s and XFS printed `Starting recovery` and `Ending recovery` at every mount (printed, not asserted); the load acknowledged 440 to 990 writes a second, slower as the file grew (a compact took up to 1.8 s and a spill 1.5 s); a compact was in flight at two cuts and a spill at four; and at all five the sync log held what the file lacked -- 5 to 99 entries, 11 to 252 KB past its end -- which the open put back. 133 930 acknowledged documents were there after the fifth. Its lost cuts' logs held 2.2 and 3.1 GB of writes, nothing after the last flush.

**The default mode and the node**, run 43438990710 (dispatched from aff77661, which still carried the arm64 probe; the probe is gone from 911b6ca1), green on all four guests: XFS and ext4 under `--sync always`, ext4 under `--sync 250`, and ext4 under a `--dir` node with its standby. Under `--sync 250` each of the five cuts lost 185, 247, 420, 120 and 259 acknowledged writes. The oldest write gone was answered 84, 114, 173, 57 and 116 ms before its cut: within the first check's bounds, read from the pages (309 to 314 ms), and within the fixed 650 ms that replaced them. Every write `/_changes` had handed over was there, and every cut kept a prefix. Syncs came every 257 to 259 ms at the median (261 to 271 ms at most), the reader's turn 24 to 34 ms at most; an acknowledged write was handed over 133 to 134 ms after its answer at the median, 268 to 273 ms at p99 and 275 to 284 ms at most. On the node, ten tenant checks (two tenants, five cuts, cuts 2 and 4 on the disk as of the last flush) passed: no torn block, no write answered as durable lost. The standby, read as each cut left it, never held a change its tenant lacked: it was behind the node as it came back by 0 to 51 changes (`a`: 51, 45, 0, 0 and 45; `b`: 2, 9, 0, 0 and 9, cut by cut), 40 of each of `a`'s 51 and 45s a spill in flight at the cut, which landed on the node and had not reached the standby; and once the node was back it held the same rows as the tenant within 3.0 s of the check asking, 0.0 s at five of ten. That run did not yet count the acknowledged writes the standby lacked, nor hold it near the tip: both came after it (below). The cuts came 2 to 30 s into the load on a runner's disk; these are five cuts a guest, not a distribution.

**Held to fixed numbers**, run 43462058075, dispatched from d17864c9 -- its window's bound still 350 ms and its median not yet held; the numbers above were set from this run's figures, which pass them -- green on all four guests, every cut with 4 to 6 writes in flight. Under `--sync 250` the cuts, 3.7 to 22 s into the writes, lost 652, 502, 54, 589 and 794 acknowledged writes, the oldest answered 248, 200, 32, 234 and 316 ms before its cut; every write `/_changes` had handed over was there, and every cut kept a prefix. The pages came 257 to 259 ms apart at the median and 261 to 427 ms at most -- the longest early in a cut, the first few syncs after the boot (366 ms between the third cut's first two pages), and for a moment mid-load (427 ms, the fourth) -- the gap open at the fifth cut 302 ms, the reader's turn 20 to 27 ms at most; an acknowledged write was handed over 133 to 143 ms after its answer at the median, 269 to 382 ms at p99 and 275 to 532 ms at most. On the node all ten tenant checks passed, and the standby, read as each cut left it, held every write the node had answered as durable: none was above its change at any cut, so a promotion at any of them would have lost none. It was 0 to 10 changes behind the node as it came back (`a`: 0, 0, 1, 0 and 2; `b`: 1, 0, 4, 0 and 10, cut by cut), writes in flight at the cut that landed on the node, and once the node was back it held the same rows within 3.4 s of the check asking (`a`: 0.0, 0.0, 0.0, 2.0 and 3.4 s; `b`: 0.0 s at all five). Under `--sync always` the sync log held what the file lacked at four ext4 cuts of five, the first finding none, and at all five on XFS. A second run's five cuts a guest, not a distribution.

**arm64 is not cut: its runners have no KVM.** A `ubuntu-24.04-arm` runner is a Hyper-V guest on Neoverse-N2 (Azure's kernel, 7.0.0-1017-azure), which Hyper-V starts at EL1: its kernel says `kvm [1]: HYP mode not available`, and there is no `/dev/kvm`, `modprobe kvm` or not (run 43438990710's probe). QEMU there would emulate the guest instruction by instruction (TCG), a power cut of an emulated machine whose timings, and so whose cuts, are not a real one's; no such guest is run in arm64's name. What arm64 has instead: the release's aarch64 build is the same code, its NEON kernels run in `test (macos-latest)`, and `desktop.yml` drives the released server on `ubuntu-24.04-arm` -- none of them a power cut. A cut of an arm64 guest needs a runner with nested virtualisation -- a larger runner that offers it, or one of the project's own -- or a machine at hand, and is the rest of roadmap row 113.

**Not covered.** ext4's other modes (`data=journal`, `data=writeback`), XFS's other options, `nobarrier` on either, and every other file system; `--sync <ms>` but 250, and 250 on XFS or with spills and compacts of the client's own; a node under `--sync 250`, whose tenants a loop of its own syncs (`Tenants::sync_dirty`, called from `fennec-server`'s main loop), not `run_syncer` -- the node here runs `--sync always`; a node's archive (`fennec archive --tenants`), the router in front of it, a promotion after a power cut, a standby's own power cut and a single file's replica; the native library; arm64 (above) and other kernels; the root disk's cache; any subset of the writes after the last flush but none of them; a real disk's firmware -- a flush answered and not done, a sector torn inside a write. As on Windows, the host's own durability, which `kill -9` never tests.

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
